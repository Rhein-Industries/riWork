import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// Choosing a chat's model, effort and Fast on the phone, against a scripted desktop: what one choice sends (one `Configure`, with
/// exactly the fields that changed), how it shows before the desktop has answered, what is remembered for the next chat and what a new
/// chat is created with; and the picker sheet and the New terminal sheet as a keyboard drives them.
@MainActor final class ChatModelPickerTests: XCTestCase {
    private let project = ChatTransport.project
    private let chatID = "cccccccc-1111-4111-8111-111111111111"
    private var defaultsNames: [String] = []
    private var tasks: [Task<Void, Never>] = []
    private var windows: [UIWindow] = []

    override func setUp() async throws { executionTimeAllowance = 60 }
    override func tearDown() async throws {
        for task in tasks { task.cancel() }
        tasks = []
        for window in windows { window.isHidden = true }
        windows = []
        for name in defaultsNames { UserDefaults().removePersistentDomain(forName: name) }
        defaultsNames = []
    }

    // MARK: A desktop with models

    private let gpt = ChatModelOption(id: "gpt-5.5", name: "GPT-5.5", description: "Frontier model", efforts: ["low", "medium", "high", "xhigh"], defaultEffort: "medium", supportsFast: true, isDefault: true)
    private let mini = ChatModelOption(id: "gpt-5.4-mini", name: "GPT-5.4 mini", description: "Faster, cheaper", efforts: ["low", "medium"], defaultEffort: "low")
    private let bare = ChatModelOption(id: "bare", name: "Bare")
    private var list: [ChatModelOption] { [gpt, mini, bare] }

    private func chat(provider: ChatProvider = .codex, model: String? = nil, effort: String? = nil, fast: Bool = false, state: ChatState = .idle) -> ChatInfo {
        ChatInfo(id: chatID, provider: provider, projectID: project, cwd: "/fixture", title: "", createdAtUnix: 10, model: model, effort: effort, fast: fast, state: state)
    }
    private func defaults() -> UserDefaults {
        let name = "com.riwork.tests.models.\(UUID().uuidString)"
        defaultsNames.append(name)
        return UserDefaults(suiteName: name)!
    }
    private func store() throws -> KeychainStore {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = ChatTransport.shell
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        return keychain
    }
    private struct Rig { let model: RemoteModel; let transport: ChatTransport; let keychain: KeychainStore }
    private func connected(chats: [ChatInfo]? = nil, defaults suite: UserDefaults? = nil, hardwareKeyboard: Bool = true) async throws -> Rig {
        let transport = ChatTransport(chats: chats ?? [chat()])
        let keychain = try store()
        let model = RemoteModel(client: transport, keychain: keychain, defaults: suite ?? defaults(), chatWaitMilliseconds: 300, chatIdleInterval: .milliseconds(20),
                                hardwareKeyboard: HardwareKeyboardMonitor(probe: { hardwareKeyboard }))
        await model.connect()
        XCTAssertEqual(model.state, .connected)
        return Rig(model: model, transport: transport, keychain: keychain)
    }
    private func finish(_ rig: Rig) async {
        await rig.model.disconnect()
        try? rig.keychain.delete()
    }
    private func eventually(_ what: String, timeout: Double = 5, file: StaticString = #filePath, line: UInt = #line, _ condition: () async -> Bool) async {
        let end = Date().addingTimeInterval(timeout)
        while await !condition(), Date() < end { try? await Task.sleep(for: .milliseconds(10)) }
        let met = await condition()
        XCTAssertTrue(met, what, file: file, line: line)
    }
    private func follow(_ rig: Rig) {
        let id = chatID
        tasks.append(Task { await rig.model.followChat(id) })
    }
    /// A chat the desktop says runs `model`, with the list of models.
    private func withModels(_ rig: Rig, info: ChatInfo? = nil, models: [ChatModelOption]? = nil) async {
        await rig.transport.append(chatID, [.info(info ?? chat()), .models(models ?? list)])
        follow(rig)
        await eventually("the models are in") { rig.model.conversation(self.chatID).transcript.models.count == (models ?? self.list).count }
    }
    private final class Mirror: @unchecked Sendable {
        private let lock = NSLock()
        private var info: ChatInfo
        init(_ info: ChatInfo) { self.info = info }
        func apply(_ command: ChatCommand) -> [ChatEvent] {
            guard case .configure(let model, let effort, _, let fast) = command else { return [] }
            lock.lock(); defer { lock.unlock() }
            if let model { info.model = model }
            if let effort { info.effort = effort }
            if let fast { info.fast = fast }
            return [.info(info)]
        }
    }
    /// A desktop that does what a Configure says and reports the chat's new info, as the real one does.
    private func echoing(_ rig: Rig, from info: ChatInfo? = nil) async {
        let mirror = Mirror(info ?? chat())
        await rig.transport.handleCommands { _, command in mirror.apply(command) }
    }
    private func choices(_ rig: Rig) -> ChatModelChoices { rig.model.conversation(chatID).modelChoices(fallback: chat()) }
    private func configure(_ fields: [String: JSONValue]) -> JSONValue { .object(["command": .string("configure")].merging(fields) { $1 }) }

    // MARK: A desktop with no list of models

    func testWithoutAListThereIsNoPickerAndNoShortcut() async throws {
        let rig = try await connected()
        await rig.transport.append(chatID, [.info(chat(model: "gpt-5", effort: "high"))])
        follow(rig)
        await eventually("the chat is read") { rig.model.conversation(self.chatID).feed.loaded }
        XCTAssertTrue(choices(rig).isAvailable, "an older host gets an immediately usable bundled fallback")
        XCTAssertEqual(rig.model.conversation(chatID).modelCatalogueSource, .bundled)
        XCTAssertNil(rig.model.conversation(chatID).pendingModel)
        let sent = await rig.transport.commands()
        XCTAssertTrue(sent.isEmpty)
        await finish(rig)
    }
    func testAModelsEventCanComeAgainAndReplacesTheList() async throws {
        let rig = try await connected()
        await withModels(rig)
        XCTAssertEqual(choices(rig).models.map(\.id), ["gpt-5.5", "gpt-5.4-mini", "bare"])
        await rig.transport.append(chatID, [.models([mini])])
        await eventually("replaced") { self.choices(rig).models.map(\.id) == ["gpt-5.4-mini"] }
        await rig.transport.appendRaw(chatID, #"{"event":"models","models":"nope"}"#)
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(choices(rig).models.map(\.id), ["gpt-5.4-mini"], "a garbled event takes nothing away")
        await finish(rig)
    }

    // MARK: One choice, one Configure

    func testEachChoiceIsOneConfigureWithExactlyTheFieldsThatChanged() async throws {
        let rig = try await connected()
        await echoing(rig)
        await withModels(rig)
        let info = chat()
        // Fast on: that and nothing else.
        let first = await rig.model.chooseChatModel(info, .fast(true))
        XCTAssertNil(first)
        await eventually("Fast is on") { self.choices(rig).fast }
        // An effort.
        let second = await rig.model.chooseChatModel(info, .effort("xhigh"))
        XCTAssertNil(second)
        await eventually("the effort is in") { self.choices(rig).selectedEffort == "xhigh" }
        // A model that cannot keep the effort or Fast: one command, which carries both fix-ups.
        let third = await rig.model.chooseChatModel(info, .model("gpt-5.4-mini"))
        XCTAssertNil(third)
        await eventually("the model is in") { self.choices(rig).current?.id == "gpt-5.4-mini" }
        let sent = await rig.transport.commands()
        XCTAssertEqual(sent, [
            configure(["fast": .bool(true)]),
            configure(["effort": .string("xhigh")]),
            configure(["model": .string("gpt-5.4-mini"), "effort": .string("low"), "fast": .bool(false)])
        ])
        XCTAssertEqual(choices(rig).selectedEffort, "low")
        XCTAssertFalse(choices(rig).showsFast)
        await finish(rig)
    }
    func testAChoiceThatChangesNothingSendsNothing() async throws {
        let rig = try await connected()
        await withModels(rig, info: chat(model: "gpt-5.4-mini", effort: "medium"))
        let info = chat()
        let results = [await rig.model.chooseChatModel(info, .model("gpt-5.4-mini")), await rig.model.chooseChatModel(info, .effort("medium")),
                       await rig.model.chooseChatModel(info, .fast(false)), await rig.model.chooseChatModel(info, .fast(true)), await rig.model.chooseChatModel(info, .effort("xhigh"))]
        XCTAssertTrue(results.allSatisfy { $0 == nil })
        let sent = await rig.transport.commands()
        XCTAssertTrue(sent.isEmpty, "already so, or not on offer for this model")
        await finish(rig)
    }
    func testAChoiceShowsAtOnceAndTheDesktopsWordTakesItOver() async throws {
        let rig = try await connected()
        await rig.transport.gateCommands(true)
        await withModels(rig)
        let conversation = rig.model.conversation(chatID)
        let task = Task { await rig.model.chooseChatModel(self.chat(), .model("gpt-5.4-mini")) }
        await eventually("shown before the Mac has answered") { conversation.pendingModel == .init(model: "gpt-5.4-mini") }
        XCTAssertEqual(choices(rig).current?.id, "gpt-5.4-mini", "the chip and the sheet say so already")
        XCTAssertEqual(choices(rig).chipText, "GPT-5.4 mini")
        // The desktop answers and reports the chat's info: the pending choice is settled by it.
        await rig.transport.gateCommands(false)
        _ = await task.value
        XCTAssertNotNil(conversation.pendingModel, "until the chat says so itself")
        await rig.transport.append(chatID, [.info(chat(model: "gpt-5.4-mini"))])
        await eventually("settled") { conversation.pendingModel == nil }
        XCTAssertEqual(choices(rig).current?.id, "gpt-5.4-mini")
        await finish(rig)
    }
    func testARefusedChoiceIsTakenBackAndSaid() async throws {
        let rig = try await connected()
        await withModels(rig)
        await rig.transport.failCommand(.rpc(code: "invalid_request", message: "the model is not available"))
        let failure = await rig.model.chooseChatModel(chat(), .model("gpt-5.4-mini"))
        XCTAssertNotNil(failure)
        let conversation = rig.model.conversation(chatID)
        XCTAssertNil(conversation.pendingModel, "the picker does not keep a choice the Mac refused")
        XCTAssertEqual(choices(rig).current?.id, "gpt-5.5")
        XCTAssertNotNil(conversation.notice)
        XCTAssertEqual(rig.model.chatChoice(for: .codex), NewChatChoice(), "and it is not remembered")
        // Asking again works.
        let again = await rig.model.chooseChatModel(chat(), .model("gpt-5.4-mini"))
        XCTAssertNil(again)
        XCTAssertNil(conversation.notice)
        await finish(rig)
    }
    func testWithoutALinkNothingIsSent() async throws {
        let rig = try await connected()
        await withModels(rig)
        await rig.model.disconnect()
        let failure = await rig.model.chooseChatModel(chat(), .fast(true))
        XCTAssertEqual(failure, .notConnected)
        XCTAssertNil(rig.model.conversation(chatID).pendingModel, "nothing is shown that was not sent")
        let sent = await rig.transport.commands()
        XCTAssertTrue(sent.isEmpty)
        try? rig.keychain.delete()
    }

    // MARK: Remembered for the next chat

    func testWhatIsChosenInsideAChatIsWhatTheNextChatOfThatProviderStartsWith() async throws {
        let suite = defaults()
        let rig = try await connected(defaults: suite)
        await echoing(rig)
        await withModels(rig)
        XCTAssertEqual(rig.model.chatChoice(for: .codex), NewChatChoice(), "nothing yet")
        _ = await rig.model.chooseChatModel(chat(), .effort("high"))
        _ = await rig.model.chooseChatModel(chat(), .fast(true))
        await eventually("both are in") { self.choices(rig).selectedEffort == "high" && self.choices(rig).fast }
        let remembered = rig.model.chatChoice(for: .codex)
        XCTAssertEqual(remembered, NewChatChoice(model: gpt, usesModel: true, effort: "high", fast: true), "the model that runs, with what it is set to")
        XCTAssertEqual(rig.model.chatChoice(for: .claude), NewChatChoice(), "the other provider keeps its own")
        await finish(rig)
        // Another run of the app finds it.
        let next = try await connected(chats: [], defaults: suite)
        XCTAssertEqual(next.model.chatChoice(for: .codex), remembered)
        XCTAssertEqual(next.model.rememberedChatChoices()[.codex], remembered)
        await finish(next)
    }
    func testAStoredChoiceThatCannotBeReadIsTheDefault() async throws {
        let suite = defaults()
        suite.set(Data("not json".utf8), forKey: RemoteModel.newChatChoiceKey(.claude))
        suite.set("a string", forKey: RemoteModel.newChatChoiceKey(.codex))
        let rig = try await connected(defaults: suite)
        XCTAssertEqual(rig.model.chatChoice(for: .claude), NewChatChoice())
        XCTAssertEqual(rig.model.chatChoice(for: .codex), NewChatChoice())
        await finish(rig)
    }

    func testCreationReadsProviderCatalogueWithoutStartingOrChangingAChat() async throws {
        let rig = try await connected()
        await rig.transport.append(chatID, [.info(chat()), .models(list)])
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.selectChat(.codex)
        await sheet.loadChatModels()
        XCTAssertEqual(sheet.form.chatModels[.codex], list)
        XCTAssertNil(sheet.chatModelsError)
        XCTAssertFalse(sheet.loadingChatModels)
        XCTAssertFalse(rig.model.conversation(chatID).following)
        sheet.chooseChatModel(gpt)
        sheet.chooseChatEffort("xhigh")
        sheet.setChatFast(true)
        sheet.chooseChatModel(mini)
        XCTAssertEqual(sheet.form.chatChoice?.chosen, mini)
        XCTAssertEqual(sheet.form.chatChoice?.selectedEffort, "low")
        XCTAssertFalse(sheet.form.chatChoice?.fastIsOn ?? true)
        let created = await rig.transport.count("chat.create")
        let commands = await rig.transport.commands()
        XCTAssertEqual(created, 0)
        XCTAssertTrue(commands.isEmpty)
        sheet.create()
        let params = await createdParams(rig)
        XCTAssertEqual(params?["model"], .string(mini.id))
        XCTAssertEqual(params?["effort"], .string("low"))
        XCTAssertNil(params?["fast"])
        await finish(rig)
    }

    func testCatalogueFailureCanBeRetriedAndNeverUsesAnotherProvider() async throws {
        let rig = try await connected()
        await rig.transport.append(chatID, [.models(list)])
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.selectChat(.claude)
        await sheet.loadChatModels()
        XCTAssertNotNil(sheet.chatModelsError)
        XCTAssertEqual(sheet.chatModelsSources[.claude], .bundled)
        XCTAssertFalse(sheet.form.chatModels[.claude]?.isEmpty ?? true)
        sheet.selectChat(.codex)
        await rig.transport.failEvents(1)
        await sheet.loadChatModels()
        XCTAssertNotNil(sheet.chatModelsError)
        XCTAssertFalse(sheet.loadingChatModels)
        await sheet.loadChatModels()
        XCTAssertNil(sheet.chatModelsError)
        XCTAssertEqual(sheet.form.chatModels[.codex], list)
        let commands = await rig.transport.commands()
        XCTAssertTrue(commands.isEmpty)
        await finish(rig)
    }

    func testPickerLoadsCatalogueForAnUnfollowedChatAndPreservesCompatibleSettings() async throws {
        let info = chat(model: gpt.id, effort: "medium", fast: true)
        let rig = try await connected(chats: [info])
        await rig.transport.append(chatID, [.info(info), .models(list)])
        let catalogue = try await rig.model.availableChatModels(provider: info.provider, chat: info)
        rig.model.conversation(chatID).modelCatalogue = catalogue
        XCTAssertEqual(rig.model.conversation(chatID).modelChoices(fallback: info).current?.name, gpt.name)
        let compatible = ChatModelOption(id: "compatible", name: "Compatible", efforts: ["medium"], supportsFast: true)
        rig.model.conversation(chatID).modelCatalogue.append(compatible)
        let failure = await rig.model.chooseChatModel(info, .model(compatible.id))
        XCTAssertNil(failure)
        let commands = await rig.transport.commands()
        XCTAssertEqual(commands, [configure(["model": .string(compatible.id)])])
        XCTAssertEqual(rig.model.conversation(chatID).modelChoices(fallback: info).selectedEffort, "medium")
        XCTAssertTrue(rig.model.conversation(chatID).modelChoices(fallback: info).fastIsOn)
        await finish(rig)
    }

    // MARK: Fallbacks and legacy-host recovery

    func testLegacyChatUsesNewestCompleteCatalogueFromCompatibleSibling() async throws {
        let legacy = chat(model: gpt.id)
        var sibling = legacy
        sibling.id = "dddddddd-1111-4111-8111-111111111111"
        let rig = try await connected(chats: [legacy, sibling])
        await rig.transport.append(chatID, [.info(legacy)]) // A pre-Models chat, as on the user's running host.
        await rig.transport.append(sibling.id, [.models([gpt])])
        await rig.transport.append(sibling.id, Array(repeating: .state(.idle), count: 501))
        await rig.transport.append(sibling.id, [.models([mini])])
        let models = try await rig.model.availableChatModels(provider: .codex, chat: legacy)
        XCTAssertEqual(models, [mini], "the last complete page wins, not the first Models event")
        let commands = await rig.transport.commands()
        XCTAssertTrue(commands.isEmpty)
        await finish(rig)
    }

    func testEmptyCatalogueDoesNotResurrectAnOlderLiveList() async throws {
        let rig = try await connected()
        await rig.transport.append(chatID, [.models(list), .models([])])
        do {
            _ = try await rig.model.availableChatModels(provider: .codex, chat: chat())
            XCTFail("an explicitly empty latest list must not return the earlier one as live")
        } catch { }
        rig.model.prepareChatCatalogue(chat())
        XCTAssertEqual(rig.model.conversation(chatID).modelCatalogueSource, .bundled)
        XCTAssertFalse(choices(rig).models.isEmpty)
        await finish(rig)
    }

    func testCachedCatalogueSurvivesRelaunchAndIsIsolatedByDesktopProviderAndAccount() async throws {
        let suite = defaults()
        let first = try await connected(defaults: suite)
        var accountA = chat(); accountA.codexAccountID = "account-a"
        first.model.rememberModelCatalogue(list, provider: .codex, chat: accountA)
        await finish(first)
        let next = try await connected(defaults: suite)
        next.model.prepareChatCatalogue(accountA)
        XCTAssertEqual(next.model.conversation(chatID).modelCatalogue, list)
        XCTAssertEqual(next.model.conversation(chatID).modelCatalogueSource, .cached)
        var accountB = accountA; accountB.codexAccountID = "account-b"
        XCTAssertEqual(next.model.cachedOrBundledModels(provider: .codex, chat: accountB).source, .bundled)
        XCTAssertEqual(next.model.cachedOrBundledModels(provider: .claude, chat: accountA).source, .bundled)
        let original = next.model.selectedDesktopID
        next.model.selectedDesktopID = nil
        XCTAssertEqual(next.model.cachedOrBundledModels(provider: .codex, chat: accountA).source, .bundled)
        next.model.selectedDesktopID = original
        await finish(next)
    }

    func testFirstRunNetworkFailureKeepsBundledChoicesAndRecoveryReplacesThem() async throws {
        let rig = try await connected()
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.selectChat(.codex)
        await rig.transport.failEvents(1)
        await sheet.loadChatModels()
        XCTAssertEqual(sheet.chatModelsSources[.codex], .bundled)
        XCTAssertFalse(sheet.form.chatModels[.codex]?.isEmpty ?? true)
        XCTAssertNotNil(sheet.chatModelsError)
        await rig.transport.append(chatID, [.models(list)])
        await sheet.loadChatModels()
        XCTAssertEqual(sheet.chatModelsSources[.codex], .live)
        XCTAssertEqual(sheet.form.chatModels[.codex], list)
        XCTAssertNil(sheet.chatModelsError)
        XCTAssertEqual(rig.model.cachedOrBundledModels(provider: .codex).source, .cached)
        await finish(rig)
    }

    func testBundledModelRejectionRestoresSelectionAndShowsReason() async throws {
        let rig = try await connected()
        let info = chat(model: "existing-model")
        rig.model.prepareChatCatalogue(info)
        let target = try XCTUnwrap(choices(rig).models.first)
        await rig.transport.failCommand(.rpc(code: "invalid_request", message: "model is not available for this account"))
        let failure = await rig.model.chooseChatModel(info, .model(target.id))
        XCTAssertNotNil(failure)
        XCTAssertNil(rig.model.conversation(chatID).pendingModel)
        XCTAssertEqual(rig.model.conversation(chatID).modelChoices(fallback: info).modelID, "existing-model")
        XCTAssertTrue(rig.model.conversation(chatID).notice?.contains("not available") == true)
        await finish(rig)
    }

    func testLoadingIsBoundedAndLateCompletionCannotReplaceNewerList() async throws {
        let rig = try await connected()
        await rig.transport.gateEvents(true)
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.selectChat(.codex)
        let request = Task { await sheet.loadChatModels() }
        await eventually("fallback is available during loading") { sheet.loadingChatModels && sheet.form.chatModels[.codex]?.isEmpty == false }
        await eventually("spinner stops within the bounded period", timeout: 5) { !sheet.loadingChatModels }
        XCTAssertNotNil(sheet.chatModelsError)
        sheet.form.chatModels[.codex] = [mini]
        sheet.chatModelsSources[.codex] = .live
        await rig.transport.append(chatID, [.models([gpt])])
        await rig.transport.gateEvents(false)
        await request.value
        XCTAssertEqual(sheet.form.chatModels[.codex], [mini], "the timed-out callback is discarded")
        await finish(rig)
    }

    func testCompatibleSourcesNeverCrossAccountsAndGenerationChangesDiscardCallbacks() async throws {
        var old = chat(); old.codexAccountID = "account-a"
        var other = old; other.id = "eeeeeeee-1111-4111-8111-111111111111"; other.codexAccountID = "account-b"
        let rig = try await connected(chats: [old, other])
        await rig.transport.append(other.id, [.models(list)])
        do { _ = try await rig.model.availableChatModels(provider: .codex, chat: old); XCTFail("another account is not compatible") } catch { }
        rig.model.chats = [old]
        await rig.transport.setChats([old])
        await rig.transport.gateEvents(true)
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.selectChat(.codex)
        let request = Task { await sheet.loadChatModels() }
        await eventually("loading") { sheet.loadingChatModels }
        rig.model.generation = UUID()
        await rig.transport.append(chatID, [.models(list)])
        await rig.transport.gateEvents(false)
        await request.value
        XCTAssertEqual(sheet.chatModelsSources[.codex], .bundled)
        await finish(rig)
    }

    // MARK: A new chat

    private func createdParams(_ rig: Rig) async -> [String: JSONValue]? {
        await eventually("chat.create was sent") { await rig.transport.count("chat.create") == 1 }
        return await rig.transport.params(of: "chat.create").first
    }
    func testANewChatWithNothingChosenAsksForNoModelEffortOrFast() async throws {
        let rig = try await connected(chats: [])
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.selectChat(.claude)
        XCTAssertNil(sheet.form.chatChoice?.chosen)
        sheet.create()
        let params = await createdParams(rig)
        XCTAssertEqual(params, ["provider": .string("claude"), "project_id": .string(project)], "the Mac’s defaults stay the Mac’s, and an older Mac is not sent a field it does not know")
        await finish(rig)
    }
    func testANewChatStartsWithTheModelEffortAndFastRememberedForItsProvider() async throws {
        let rig = try await connected(chats: [])
        let opus = ChatModelOption(id: "opus", name: "Claude Opus 4.1", efforts: ["low", "medium", "high"], defaultEffort: "medium", supportsFast: true)
        rig.model.rememberChatChoice(NewChatChoice(model: opus, usesModel: true, effort: "high", fast: true), for: .claude)
        rig.model.rememberChatChoice(NewChatChoice(model: gpt, usesModel: true), for: .codex)
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.selectChat(.claude)
        XCTAssertEqual(sheet.form.chatChoice?.chosen, opus, "the last model is the one chosen")
        XCTAssertEqual(sheet.form.chatChoice?.summary, "Opus 4.1 · High · Fast")
        sheet.create()
        let params = await createdParams(rig)
        XCTAssertEqual(params, ["provider": .string("claude"), "project_id": .string(project), "model": .string("opus"), "effort": .string("high"), "fast": .bool(true)])
        await finish(rig)
    }
    func testTheChoiceMadeInTheSheetIsSentAndRemembered() async throws {
        let suite = defaults()
        let rig = try await connected(chats: [], defaults: suite)
        rig.model.rememberChatChoice(NewChatChoice(model: gpt), for: .codex)
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.selectChat(.codex)
        XCTAssertNil(sheet.form.chatChoice?.chosen, "offered, not chosen")
        sheet.chooseChatModel(last: true)
        sheet.chooseChatEffort("xhigh")
        sheet.setChatFast(true)
        XCTAssertEqual(sheet.form.focus, .chatFast)
        sheet.create()
        let params = await createdParams(rig)
        XCTAssertEqual(params, ["provider": .string("codex"), "project_id": .string(project), "model": .string("gpt-5.5"), "effort": .string("xhigh"), "fast": .bool(true)])
        XCTAssertEqual(rig.model.chatChoice(for: .codex), NewChatChoice(model: gpt, usesModel: true, effort: "xhigh", fast: true))
        XCTAssertEqual(rig.model.chatChoice(for: .claude), NewChatChoice())
        await finish(rig)
        // The next sheet starts as the last chat did.
        let next = try await connected(chats: [], defaults: suite)
        let again = try XCTUnwrap(NewTerminalSheetModel(model: next.model))
        again.selectChat(.codex)
        XCTAssertEqual(again.form.chatChoice?.summary, "GPT-5.5 · X-High · Fast")
        // Going back to the default is remembered too, and the model stays on offer.
        again.chooseChatModel(last: false)
        again.create()
        let second = await createdParams(next)
        XCTAssertEqual(second, ["provider": .string("codex"), "project_id": .string(project)])
        XCTAssertEqual(next.model.chatChoice(for: .codex).usesModel, false)
        XCTAssertEqual(next.model.chatChoice(for: .codex).model, gpt)
        await finish(next)
    }
    func testTheSheetsKeyboardChoosesTheModelEffortAndFast() async throws {
        let rig = try await connected(chats: [])
        rig.model.rememberChatChoice(NewChatChoice(model: gpt), for: .codex)
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.selectChat(.codex)
        sheet.press(.tab)
        XCTAssertEqual(sheet.form.focus, .chatModel)
        sheet.press(.down)
        XCTAssertEqual(sheet.form.chatChoice?.usesModel, true, "↓ chooses the last model")
        XCTAssertEqual(sheet.form.kind, .chat)
        sheet.press(.tab); XCTAssertEqual(sheet.form.focus, .chatEffort)
        sheet.press(.down); sheet.press(.down)
        XCTAssertEqual(sheet.form.chatChoice?.selectedEffort, "xhigh")
        sheet.press(.tab); XCTAssertEqual(sheet.form.focus, .chatFast)
        sheet.press(.space)
        XCTAssertEqual(sheet.form.chatChoice?.fastIsOn, true)
        sheet.create()
        let params = await createdParams(rig)
        XCTAssertEqual(params?["model"], .string("gpt-5.5")); XCTAssertEqual(params?["effort"], .string("xhigh")); XCTAssertEqual(params?["fast"], .bool(true))
        await finish(rig)
    }

    // MARK: The picker sheet, driven by keys

    private func host(_ view: some View, width: CGFloat = 402, height: CGFloat = 640) throws -> (UIWindow, UIHostingController<AnyView>) {
        let controller = UIHostingController(rootView: AnyView(view))
        let window = UIWindow(windowScene: try XCTUnwrap(UIApplication.shared.connectedScenes.first as? UIWindowScene))
        window.frame = CGRect(x: 0, y: 0, width: width, height: height)
        window.rootViewController = controller
        window.makeKeyAndVisible()
        windows.append(window)
        return (window, controller)
    }
    private func descendants<T: UIView>(_ type: T.Type, in view: UIView) -> [T] {
        view.subviews.compactMap { $0 as? T } + view.subviews.flatMap { descendants(type, in: $0) }
    }
    private final class Closed: @unchecked Sendable { var count = 0 }
    private struct Sheet { let window: UIWindow; let keys: KeyCommandHost.HostView; let closed: Closed }
    private func openSheet(_ rig: Rig, info: ChatInfo? = nil) async throws -> Sheet {
        let closed = Closed()
        let chat = info ?? chat()
        let (window, controller) = try host(ChatModelSheet(model: rig.model, chat: chat) { closed.count += 1 }.desktopThemed(rig.model.theme.style))
        var keys: KeyCommandHost.HostView?
        await eventually("the sheet is up") { keys = self.descendants(KeyCommandHost.HostView.self, in: controller.view).first; return keys != nil }
        return Sheet(window: window, keys: try XCTUnwrap(keys), closed: closed)
    }
    private func press(_ sheet: Sheet, _ input: String, _ flags: UIKeyModifierFlags = [], file: StaticString = #filePath, line: UInt = #line) {
        guard let found = sheet.keys.keyCommands?.first(where: { $0.input == input && $0.modifierFlags == flags }) else {
            return XCTFail("the sheet does not claim \(input.debugDescription) \(flags.rawValue)", file: file, line: line)
        }
        sheet.keys.fire(found)
    }
    private func picture(_ window: UIWindow, _ name: String) {
        window.layoutIfNeeded()
        let image = UIGraphicsImageRenderer(bounds: window.bounds).image { _ in window.drawHierarchy(in: window.bounds, afterScreenUpdates: true) }
        XCTAssertGreaterThan(image.pngData()?.count ?? 0, 10_000, "\(name) drew something")
        if let directory = ProcessInfo.processInfo.environment["RIWORK_CHAT_SNAPSHOTS"], let data = image.pngData() {
            try? FileManager.default.createDirectory(atPath: directory, withIntermediateDirectories: true)
            try? data.write(to: URL(fileURLWithPath: directory).appendingPathComponent("\(name).png"))
        }
    }

    func testTheSheetClaimsTheArrowsReturnAndTheWaysOutAndNothingFromTheTerminal() async throws {
        let rig = try await connected()
        await withModels(rig)
        let sheet = try await openSheet(rig)
        let claimed = Set((sheet.keys.keyCommands ?? []).map { ($0.input ?? "") + "|\($0.modifierFlags.rawValue)" })
        let command = UIKeyModifierFlags.command.rawValue
        for expected in [UIKeyCommand.inputUpArrow + "|0", UIKeyCommand.inputDownArrow + "|0", UIKeyCommand.inputLeftArrow + "|0", UIKeyCommand.inputRightArrow + "|0",
                         "\r|0", " |0", "\t|0", "\t|\(UIKeyModifierFlags.shift.rawValue)", UIKeyCommand.inputEscape + "|0", ".|\(command)", "m|\(command)"] {
            XCTAssertTrue(claimed.contains(expected), expected.debugDescription)
        }
        for taken in ["k", ",", "/", "n", "o", "e", "t", "w", "a", "s", "d", "b", "f"] { XCTAssertFalse(claimed.contains("\(taken)|\(command)"), "⌘\(taken) belongs to the terminal or the app") }
        picture(sheet.window, "chat-model-sheet")
        await finish(rig)
    }
    func testReturnChoosesTheModelUnderTheRingAndSendsOneConfigure() async throws {
        let rig = try await connected()
        await echoing(rig)
        await withModels(rig)
        let sheet = try await openSheet(rig)
        press(sheet, UIKeyCommand.inputDownArrow)
        let none = await rig.transport.commands()
        XCTAssertTrue(none.isEmpty, "moving the ring sends nothing")
        press(sheet, "\r")
        await eventually("the Mac was told") { await rig.transport.commands().count == 1 }
        let sent = await rig.transport.commands()
        XCTAssertEqual(sent, [configure(["model": .string("gpt-5.4-mini")])])
        XCTAssertEqual(sheet.closed.count, 0, "the sheet stays, so the effort can be chosen next")
        await eventually("the chat runs it") { self.choices(rig).current?.id == "gpt-5.4-mini" }
        // The efforts are now this model's: ↓↓ reaches them, → picks the next, ⏎ chooses.
        press(sheet, UIKeyCommand.inputDownArrow); press(sheet, UIKeyCommand.inputDownArrow)
        press(sheet, UIKeyCommand.inputRightArrow)
        press(sheet, "\r")
        await eventually("the effort was sent") { await rig.transport.commands().count == 2 }
        let both = await rig.transport.commands()
        XCTAssertEqual(both.last, configure(["effort": .string("medium")]))
        XCTAssertEqual(both.count, 2)
        // Return on what is chosen already closes the sheet; space does not.
        await eventually("the effort is in") { self.choices(rig).selectedEffort == "medium" }
        press(sheet, " ")
        XCTAssertEqual(sheet.closed.count, 0)
        press(sheet, "\r")
        XCTAssertEqual(sheet.closed.count, 1)
        let after = await rig.transport.commands()
        XCTAssertEqual(after.count, 2, "nothing was sent for a choice that changes nothing")
        await finish(rig)
    }
    func testFastIsFlippedByReturnOrSpaceOnItsRow() async throws {
        let rig = try await connected()
        await echoing(rig)
        await withModels(rig)
        let sheet = try await openSheet(rig)
        press(sheet, UIKeyCommand.inputUpArrow)    // wraps to the last stop: Fast
        press(sheet, " ")
        await eventually("Fast on") { self.choices(rig).fastIsOn }
        press(sheet, "\r")
        await eventually("and off again") { !self.choices(rig).fastIsOn }
        let sent = await rig.transport.commands()
        XCTAssertEqual(sent, [configure(["fast": .bool(true)]), configure(["fast": .bool(false)])])
        XCTAssertEqual(sheet.closed.count, 0)
        await finish(rig)
    }
    func testEscapeCommandPeriodAndCommandMCloseTheSheet() async throws {
        let rig = try await connected()
        await withModels(rig)
        let sheet = try await openSheet(rig)
        press(sheet, UIKeyCommand.inputEscape)
        press(sheet, ".", .command)
        press(sheet, "m", .command)
        XCTAssertEqual(sheet.closed.count, 3)
        let sent = await rig.transport.commands()
        XCTAssertTrue(sent.isEmpty)
        await finish(rig)
    }
    func testTheSheetFollowsWhatTheChatSaysAndAKeyOnAModelThatIsGoneIsHarmless() async throws {
        let rig = try await connected()
        await withModels(rig, info: chat(model: "gpt-5.4-mini", fast: true))
        let sheet = try await openSheet(rig)
        // The list shrinks under the open sheet: the ring is put back on a row that is there.
        await rig.transport.append(chatID, [.models([bare])])
        await eventually("the list is replaced") { self.choices(rig).models.map(\.id) == ["bare"] }
        press(sheet, UIKeyCommand.inputDownArrow)
        press(sheet, UIKeyCommand.inputDownArrow)
        press(sheet, "\r")
        // The chat runs a model that is not in the list now, so the one there is gets chosen; it has no Fast, which the chat has on.
        await eventually("the one model there is is chosen") { await rig.transport.commands() == [self.configure(["model": .string("bare"), "fast": .bool(false)])] }
        await rig.transport.append(chatID, [.models([])])
        await eventually("empty catalogue falls back") { rig.model.conversation(self.chatID).modelCatalogueSource == .cached }
        press(sheet, UIKeyCommand.inputDownArrow); press(sheet, "\r")
        XCTAssertTrue(self.choices(rig).isAvailable)
        await finish(rig)
    }

    // MARK: The chat screen

    private struct Screen { let rig: Rig; let window: UIWindow; let host: UIHostingController<AnyView> }
    private func screen(chats: [ChatInfo]? = nil, hardwareKeyboard: Bool = true) async throws -> Screen {
        let rig = try await connected(chats: chats, hardwareKeyboard: hardwareKeyboard)
        let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))
        let (window, controller) = try host(TerminalTabsView(model: rig.model, project: projectValue, onBack: {}).desktopThemed(rig.model.theme.style), height: 874)
        await eventually("the terminal is on screen") { !self.descendants(KeyCaptureView.self, in: controller.view).isEmpty && rig.model.terminalArea != nil }
        return Screen(rig: rig, window: window, host: controller)
    }
    private func finish(_ screen: Screen) async {
        screen.window.endEditing(true)
        screen.window.isHidden = true
        await finish(screen.rig)
    }
    private func commandKeys(_ screen: Screen) -> [String] { (screen.host.keyCommands ?? []).filter { $0.modifierFlags == .command }.compactMap { $0.input?.lowercased() } }

    func testCommandMOffersThePickerWhileLoadingOrMissingModelsAndNothingElseUsesIt() async throws {
        let screen = try await screen()
        let rig = screen.rig
        rig.model.selectChat(chatID)
        await eventually("the chat screen is up") { !self.descendants(ChatComposerTextView.self, in: screen.host.view).isEmpty && rig.model.conversation(self.chatID).following }
        XCTAssertTrue(commandKeys(screen).contains("m"), "the picker remains accessible to show loading and retry before a list arrives")
        // The terminal's own shortcuts are not on this screen; ⌘M is not one of the hotkey menu's, the help's or the Clicks template's.
        await rig.transport.append(chatID, [.info(chat(model: "gpt-5.5")), .models(list)])
        await eventually("⌘M is there with a list") { self.commandKeys(screen).contains("m") }
        for taken in ["k", ",", "/"] { XCTAssertFalse(commandKeys(screen).contains(taken), taken) }
        XCTAssertTrue(commandKeys(screen).contains("n"), "⌘N, a new terminal or chat, still works")
        await rig.transport.append(chatID, [.models([])])
        await eventually("still available to retry an empty list") { self.commandKeys(screen).contains("m") }
        await finish(screen)
    }
    func testCommandMOpensTheSheetAndClosingItGivesTheComposerTheKeyboardBack() async throws {
        let screen = try await screen()
        let rig = screen.rig
        await echoing(rig)
        await rig.transport.append(chatID, [.info(chat(model: "gpt-5.5")), .models(list)])
        rig.model.selectChat(chatID)
        await eventually("the chat screen and its list") { !self.descendants(ChatComposerTextView.self, in: screen.host.view).isEmpty && self.commandKeys(screen).contains("m") && rig.model.conversation(self.chatID).transcript.models == self.list && rig.model.conversation(self.chatID).transcript.info?.model == "gpt-5.5" }
        let composer = try XCTUnwrap(descendants(ChatComposerTextView.self, in: screen.host.view).first)
        for _ in 0..<150 where !composer.isFirstResponder { try await Task.sleep(for: .milliseconds(10)) }
        if !composer.isFirstResponder { _ = composer.becomeFirstResponder() }
        let shortcut = try XCTUnwrap((screen.host.keyCommands ?? []).first { $0.input?.lowercased() == "m" && $0.modifierFlags == .command })
        let action = try XCTUnwrap(shortcut.action)
        // From the first responder up the chain, as a key press goes.
        XCTAssertTrue(UIApplication.shared.sendAction(action, to: nil, from: shortcut, for: nil), "⌘M reaches the screen")
        var sheetKeys: KeyCommandHost.HostView?
        await eventually("the picker is up") { sheetKeys = screen.host.presentedViewController.flatMap { self.descendants(KeyCommandHost.HostView.self, in: $0.view).first }; return sheetKeys != nil }
        let keys = try XCTUnwrap(sheetKeys)
        func press(_ input: String, _ flags: UIKeyModifierFlags = []) {
            if let found = keys.keyCommands?.first(where: { $0.input == input && $0.modifierFlags == flags }) { keys.fire(found) } else { XCTFail("no \(input)") }
        }
        // ⌘M, ↓, ⏎ is a model, as the shortcut says; ⎋ puts the sheet away.
        press(UIKeyCommand.inputDownArrow); press("\r")
        await eventually("the Mac was told") { await rig.transport.commands() == [self.configure(["model": .string("gpt-5.4-mini")])] }
        try await Task.sleep(for: .milliseconds(400))
        picture(screen.window, "chat-model-picker-over-chat")
        press(UIKeyCommand.inputEscape)
        await eventually("the picker is gone") { screen.host.presentedViewController == nil }
        await eventually("and the composer has the keyboard again") { composer.isFirstResponder }
        await finish(screen)
    }
    func testTheChipShowsTheModelAndABoltAndTheToolbarDraws() async throws {
        let screen = try await screen()
        let rig = screen.rig
        await rig.transport.append(chatID, [.info(chat(model: "gpt-5.5", effort: "high", fast: true)), .models(list), .usage(ChatUsage(contextWindow: 200_000, contextUsed: 42_000))])
        rig.model.selectChat(chatID)
        await eventually("the list is in") { rig.model.conversation(self.chatID).transcript.models.count == 3 }
        let choices = choices(rig)
        XCTAssertEqual(choices.chipText, "GPT-5.5 ⚡")
        XCTAssertEqual(choices.spoken, "GPT-5.5, High effort, Fast on")
        try await Task.sleep(for: .milliseconds(500))
        picture(screen.window, "chat-model-chip-fast")
        await rig.transport.append(chatID, [.info(chat(model: "gpt-5.4-mini", effort: "low", fast: false))])
        await eventually("the chip follows the chat") { self.choices(rig).chipText == "GPT-5.4 mini" }
        try await Task.sleep(for: .milliseconds(400))
        picture(screen.window, "chat-model-chip")
        // A model the list does not know, with a long name, must not push the other controls off the toolbar.
        await rig.transport.append(chatID, [.info(chat(model: "claude-opus-4-1-20250805-extended-thinking-preview", fast: true))])
        await eventually("the chip says what the chat says") { self.choices(rig).chipTitle == "claude-opus-4-1-20250805-extended-thinking-preview" }
        try await Task.sleep(for: .milliseconds(400))
        picture(screen.window, "chat-model-chip-long")
        await finish(screen)
    }

    // MARK: The New terminal sheet

    func testTheNewChatSheetDrawsTheModelRows() async throws {
        let rig = try await connected(chats: [])
        let opus = ChatModelOption(id: "opus", name: "Claude Opus 4.1", efforts: ["low", "medium", "high"], defaultEffort: "medium", supportsFast: true)
        rig.model.rememberChatChoice(NewChatChoice(model: opus, usesModel: true, effort: "high", fast: true), for: .claude)
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        let (window, _) = try host(NewTerminalSheet(sheet: sheet).desktopThemed(rig.model.theme.style).frame(width: 402, height: 720), height: 720)
        sheet.selectChat(.claude)
        try await Task.sleep(for: .milliseconds(300))
        picture(window, "new-chat-model")
        sheet.chooseChatModel(last: false)
        sheet.selectChat(.codex)
        try await Task.sleep(for: .milliseconds(300))
        picture(window, "new-chat-model-first-time")
        await finish(rig)
    }

    // MARK: One chat for both providers

    private var opus: ChatModelOption { ChatModelOption(id: "opus", name: "Opus", description: "Opus 5.5", efforts: ["low", "high"], supportsFast: true) }
    /// A desktop that lets a chat go on with the other provider and lists a provider's models (`chat.models`).
    private func switchable(chats: [ChatInfo]? = nil) async throws -> Rig {
        let rig = try await connected(chats: chats)
        await rig.transport.setSwitchFeatures(true)
        await rig.model.disconnect(); await rig.model.connect()
        XCTAssertTrue(rig.model.desktopFeatures.chatProviderSwitch)
        return rig
    }

    func testANewChatListsBothProvidersFromTheMacAndTheModelDecidesTheProvider() async throws {
        let rig = try await switchable(chats: [])
        await rig.transport.setProviderModels(.codex, list)
        await rig.transport.setProviderModels(.claude, [opus])
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.select(kind: .chat)
        XCTAssertEqual(sheet.form.chatProvider, .codex, "nothing remembered: Codex")
        await sheet.loadChatModels()
        XCTAssertEqual(sheet.form.chatModels[.codex], list); XCTAssertEqual(sheet.form.chatModels[.claude], [opus])
        XCTAssertEqual(sheet.chatModelsSources[.codex], .live); XCTAssertEqual(sheet.chatModelsSources[.claude], .live)
        let listed = await rig.transport.params(of: "chat.models")
        XCTAssertEqual(Set(listed.compactMap { $0["provider"]?.string }), ["codex", "claude"], "read from the Mac, not by replaying chats")
        let replays = await rig.transport.count("chat.events")
        XCTAssertEqual(replays, 0)
        XCTAssertEqual(sheet.form.chatRows.count, 6, "each provider's default and its models")
        sheet.chooseChatRow(.model(.claude, opus))
        XCTAssertEqual(sheet.form.chatProvider, .claude)
        sheet.create()
        let params = await createdParams(rig)
        XCTAssertEqual(params?["provider"], .string("claude")); XCTAssertEqual(params?["model"], .string("opus"))
        XCTAssertEqual(rig.model.lastChatProvider, .claude)
        XCTAssertEqual(rig.model.newTerminalForm()?.chatProvider, .claude, "the next sheet starts on Claude")
        await finish(rig)
    }
    func testAProviderTheMacCannotOfferSaysWhyUnderItsOwnRows() async throws {
        let rig = try await switchable(chats: [])
        await rig.transport.setProviderModels(.claude, [opus])
        await rig.transport.setProviderModels(.codex, [], error: "The Codex account Work cannot be used.")
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.selectChat(.claude)
        await sheet.loadChatModels()
        XCTAssertEqual(sheet.chatModelsErrors[.codex], "The Codex account Work cannot be used.")
        XCTAssertNil(sheet.chatModelsErrors[.claude]); XCTAssertNil(sheet.chatModelsError, "the chosen provider's is what the sheet says")
        XCTAssertEqual(sheet.chatModelsSources[.codex], .bundled, "its fallback stays on offer")
        await finish(rig)
    }
    func testTheChatGoesOnWithTheOtherProviderInPlace() async throws {
        let rig = try await switchable()
        await rig.transport.setProviderModels(.claude, [opus])
        // The desktop does what a switch says: the chat's info names Claude, and a notice reads as the divider.
        let moved = ChatInfo(id: chatID, provider: .claude, projectID: project, cwd: "/fixture", title: "Claude chat", createdAtUnix: 10, model: "opus", state: .idle,
                             carriedOver: ChatCarriedOver(document: "/chats/c/context.md", from: "Codex chat"))
        await rig.transport.handleCommands { _, command in
            guard case .switchProvider = command else { return [] }
            return [.info(moved), .itemCompleted(ChatItem(id: "switch-3", status: .completed, body: .notice(level: .info, text: "Continued with Claude (opus), which has the conversation so far.")))]
        }
        await withModels(rig)
        await rig.transport.append(chatID, [.rateLimits([ChatRateWindow(id: "old-weekly", label: "Old weekly", usedPercent: 100, resetsAt: UInt64.max)])])
        await eventually("old windows arrived") { rig.model.conversation(self.chatID).transcript.rateLimits.count == 1 }
        await rig.model.loadSwitchCatalogue(chat())
        let conversation = rig.model.conversation(chatID)
        XCTAssertEqual(conversation.switchCatalogue, [opus]); XCTAssertEqual(conversation.switchCatalogueSource, .live)
        let offered = conversation.modelChoices(fallback: chat(), switchable: true)
        XCTAssertEqual(offered.switching?.provider, .claude)
        XCTAssertEqual(offered.switching?.rows.map(\.id), ["default", "opus"])
        XCTAssertNil(offered.switching?.blocked)
        XCTAssertNil(conversation.modelChoices(fallback: chat()).switching, "nothing to switch to without the desktop's word")
        let failure = await rig.model.switchChatProvider(chat(), model: "opus")
        XCTAssertNil(failure)
        let sent = await rig.transport.commands()
        XCTAssertEqual(sent.last, .object(["command": .string("switch"), "provider": .string("claude"), "model": .string("opus")]))
        await eventually("the chat says it runs Claude") { conversation.transcript.info?.provider == .claude }
        XCTAssertEqual(conversation.transcript.item("switch-3")?.body, .notice(level: .info, text: "Continued with Claude (opus), which has the conversation so far."))
        XCTAssertTrue(conversation.transcript.models.isEmpty, "Codex's list is not Claude's")
        XCTAssertFalse(conversation.modelCatalogue.contains(gpt), "the picker shows Claude's models, not Codex's")
        XCTAssertTrue(conversation.transcript.rateLimits.isEmpty, "switch command clears the old provider’s windows")
        XCTAssertNil(conversation.pendingModel)
        XCTAssertEqual(rig.model.chats.first { $0.id == chatID }?.provider, .claude, "the tab shows Claude at once")
        XCTAssertEqual(conversation.modelChoices(fallback: chat(), switchable: true).switching?.provider, .codex, "and the way back is Codex")
        await finish(rig)
    }
    func testASwitchIsRefusedWhileATurnRunsAndNothingIsSent() async throws {
        let rig = try await switchable()
        await withModels(rig, info: chat(state: .running))
        await eventually("running") { rig.model.conversation(self.chatID).transcript.state == .running }
        let offered = rig.model.conversation(chatID).modelChoices(fallback: chat(), switchable: true)
        XCTAssertEqual(offered.switching?.blocked, ChatProviderSwitch.busyReason)
        let failure = await rig.model.switchChatProvider(chat(), model: nil)
        XCTAssertEqual(failure, .failed(ChatProviderSwitch.busyReason))
        XCTAssertEqual(rig.model.conversation(chatID).notice, ChatProviderSwitch.busyReason)
        let sent = await rig.transport.commands()
        XCTAssertTrue(sent.isEmpty)
        await finish(rig)
    }
}
