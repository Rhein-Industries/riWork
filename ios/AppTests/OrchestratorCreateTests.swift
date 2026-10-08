import XCTest
import RiWorkCore
@testable import RiWorkRemote

/// Opening the project's orchestrator, or the global one, from the phone (`orchestrator.create`), against a scripted desktop: a chat when
/// the Mac runs it as one (opened by its `chat_id`), otherwise a terminal; one that is there already is only opened, with a note; and
/// nothing is ever sent twice.
@MainActor final class OrchestratorCreateTests: XCTestCase {
    private let project = ChatTransport.project
    private let shell = ChatTransport.shell
    private let entryID = "aaaaaaaa-1111-4111-8111-111111111111"
    private let chatID = "cccccccc-1111-4111-8111-111111111111"
    private var defaultsNames: [String] = []
    private var tasks: [Task<Void, Never>] = []

    override func setUp() async throws { executionTimeAllowance = 30 }
    override func tearDown() async throws {
        for task in tasks { task.cancel() }
        tasks = []
        for name in defaultsNames { UserDefaults().removePersistentDomain(forName: name) }
        defaultsNames = []
    }

    private func entry(project scope: String?, chat: Bool, id: String? = nil) -> String {
        let mode = chat ? ",\"mode\":\"chat\",\"chat_id\":\"\(chatID)\",\"provider\":\"claude\"" : ""
        return "{\"id\":\"\(id ?? entryID)\",\"project_id\":\(scope.map { "\"\($0)\"" } ?? "null"),\"worktree_id\":null,\"kind\":\"orchestrator\",\"cwd\":\"/fixture\",\"harness\":null,\"alive\":true,\"created_at_unix\":3\(mode)}"
    }
    private func store() throws -> KeychainStore {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = shell
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        return keychain
    }
    private struct Rig { let model: RemoteModel; let transport: ChatTransport }
    private func connected(orchestrators: [String] = [], feature: Bool = true, chats: Bool = true, asChats: Bool = true) async throws -> Rig {
        let transport = ChatTransport(chats: [])
        await transport.setFeature(chats)
        await transport.setOrchestratorFeature(feature)
        await transport.setNewOrchestratorsAreChats(asChats)
        await transport.setOrchestrators(orchestrators)
        let suite = "com.riwork.tests.orchestratorcreate.\(UUID().uuidString)"
        defaultsNames.append(suite)
        let model = RemoteModel(client: transport, keychain: try store(), defaults: UserDefaults(suiteName: suite)!, chatWaitMilliseconds: 300, chatIdleInterval: .milliseconds(20))
        await model.connect()
        XCTAssertEqual(model.state, .connected)
        return Rig(model: model, transport: transport)
    }
    private func eventually(_ what: String, timeout: Double = 5, file: StaticString = #filePath, line: UInt = #line, _ condition: () async -> Bool) async {
        let end = Date().addingTimeInterval(timeout)
        while await !condition(), Date() < end { try? await Task.sleep(for: .milliseconds(5)) }
        let met = await condition()
        XCTAssertTrue(met, what, file: file, line: line)
    }
    private func readShells(_ rig: Rig) async -> Set<String> {
        Set(await rig.transport.params(of: "shell.output").compactMap { $0["shell_id"]?.string })
    }
    private func sheet(_ rig: Rig, _ kind: NewTerminalKind) throws -> NewTerminalSheetModel {
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.select(kind: kind)
        return sheet
    }

    // MARK: What is offered

    func testTheSheetOffersTheOrchestratorsOnlyWhenTheDesktopCanOpenThem() async throws {
        let rig = try await connected()
        XCTAssertEqual(rig.model.orchestratorCreateSupport, .supported)
        XCTAssertTrue(rig.model.orchestratorsOffered)
        XCTAssertEqual(try XCTUnwrap(rig.model.newTerminalForm()).kinds.map(\.title),
                       ["Shell", "Codex", "Claude", "Grok", "Chat", "Project orchestrator", "Global orchestrator"])
        await rig.model.disconnect()

        let older = try await connected(feature: false)
        XCTAssertEqual(older.model.orchestratorCreateSupport, .unsupported)
        XCTAssertFalse(older.model.orchestratorsOffered)
        XCTAssertEqual(try XCTUnwrap(older.model.newTerminalForm()).kinds, NewTerminalKind.allCases, "as before")
        // And with no chats either: the four terminals, as for the oldest Mac.
        let oldest = try await connected(feature: false, chats: false)
        XCTAssertEqual(try XCTUnwrap(oldest.model.newTerminalForm()).kinds, NewTerminalKind.terminalKinds)
        let none = await older.transport.count("orchestrator.create") + oldest.transport.count("orchestrator.create")
        XCTAssertEqual(none, 0)
        await older.model.disconnect(); await oldest.model.disconnect()
    }
    func testAnOrchestratorRowWantsNoWorktreeAndNoSwitch() async throws {
        let rig = try await connected()
        let sheet = try sheet(rig, .projectOrchestrator)
        XCTAssertFalse(sheet.form.kind.isAgent)
        XCTAssertFalse(sheet.form.fields.contains(.unrestricted))
        XCTAssertFalse(sheet.form.fields.contains(.target))
        XCTAssertTrue(sheet.canCreate)
        sheet.select(kind: .globalOrchestrator)
        XCTAssertTrue(sheet.canCreate, "the global one needs no project")
        await rig.model.disconnect()
    }

    // MARK: Opening

    func testTheProjectsOrchestratorOpensAsAChatByItsChatId() async throws {
        let rig = try await connected()
        let model = rig.model
        let sheet = try sheet(rig, .projectOrchestrator)
        var dismissed = 0
        sheet.dismiss = { dismissed += 1 }
        sheet.create()
        await sheet.pending?.value
        let sent = await rig.transport.params(of: "orchestrator.create")
        XCTAssertEqual(sent, [["project_id": .string(project)]], "one request, for the project on screen")
        XCTAssertNil(sheet.orchestratorError)
        XCTAssertGreaterThanOrEqual(dismissed, 1)
        let made = try XCTUnwrap(model.orchestrators.first)
        XCTAssertEqual(made.mode, .chat)
        let opened = try XCTUnwrap(made.chat_id)
        XCTAssertNotEqual(opened, made.id)
        XCTAssertEqual(model.selectedChatID, opened, "the chat's id, not the orchestrator's")
        XCTAssertEqual(model.selectedChat?.title, "Project orchestrator")
        XCTAssertTrue(model.terminalCovered)
        XCTAssertNil(model.orchestratorNotice, "it was made now: nothing to say")
        XCTAssertEqual(model.lastTerminalKind, .standard, "an orchestrator is not a kind to be remembered")
        // Nothing but the orchestrator was created, and no terminal was read for the chat.
        let others = await rig.transport.count("shell.create") + rig.transport.count("chat.create")
        XCTAssertEqual(others, 0)
        let asked = await readShells(rig)
        XCTAssertFalse(asked.contains(made.id))
        // Following it asks for the chat.
        let follower = Task { await model.followChat(opened) }
        tasks.append(follower)
        await eventually("the chat is read by its id") { await rig.transport.params(of: "chat.events").contains { $0["chat_id"]?.string == opened } }
        follower.cancel()
        await model.disconnect()
    }
    func testTheProjectsOrchestratorOpensAsATerminalWhenTheMacRunsItThatWay() async throws {
        let rig = try await connected(asChats: false)
        let model = rig.model
        let sheet = try sheet(rig, .projectOrchestrator)
        sheet.create()
        await sheet.pending?.value
        let made = try XCTUnwrap(model.orchestrators.first)
        XCTAssertEqual(made.mode, .terminal)
        XCTAssertEqual(model.sessionID, made.id, "selected like any terminal tab")
        XCTAssertNil(model.selectedChatID)
        XCTAssertFalse(model.terminalCovered)
        let asked = await readShells(rig)
        XCTAssertTrue(asked.contains(made.id), "and its screen is read")
        let chats = await rig.transport.count("chat.events")
        XCTAssertEqual(chats, 0)
        await model.disconnect()
    }
    func testAnOrchestratorThatIsThereAlreadyIsOnlyOpenedWithANote() async throws {
        let rig = try await connected(orchestrators: [entry(project: project, chat: true)])
        let model = rig.model
        XCTAssertNil(model.orchestratorNotice)
        let sheet = try sheet(rig, .projectOrchestrator)
        sheet.create()
        await sheet.pending?.value
        XCTAssertNil(sheet.orchestratorError)
        XCTAssertEqual(model.selectedChatID, chatID, "opened, by its chat's id")
        XCTAssertEqual(model.orchestratorNotice, "Project orchestrator is already running.")
        XCTAssertEqual(model.orchestrators.count, 1, "no second one")
        let sent = await rig.transport.count("orchestrator.create")
        XCTAssertEqual(sent, 1)
        model.clearOrchestratorNotice()
        XCTAssertNil(model.orchestratorNotice)
        await model.disconnect()
    }
    func testAnExistingTerminalOrchestratorIsSelectedWithTheSameNote() async throws {
        let rig = try await connected(orchestrators: [entry(project: project, chat: false)])
        let model = rig.model
        let sheet = try sheet(rig, .projectOrchestrator)
        sheet.create()
        await sheet.pending?.value
        XCTAssertEqual(model.sessionID, entryID)
        XCTAssertEqual(model.orchestratorNotice, "Project orchestrator is already running.")
        await model.disconnect()
    }
    func testTheGlobalOrchestratorIsRequestedWithNoProjectAndAppearsInTheStrip() async throws {
        let rig = try await connected()
        let model = rig.model
        let first = await model.createOrchestrator(try NewOrchestratorRequest(projectID: nil))
        XCTAssertNil(first)
        let sent = await rig.transport.params(of: "orchestrator.create")
        XCTAssertEqual(sent, [[:]], "an absent project_id")
        let made = try XCTUnwrap(model.orchestrators.first)
        XCTAssertNil(made.project_id)
        XCTAssertEqual(made.title, "Global orchestrator")
        XCTAssertTrue(model.tabs.contains { $0.session?.id == made.id }, "the global orchestrator is a tab of the project on screen")
        XCTAssertEqual(model.selectedChatID, made.chat_id)
        XCTAssertEqual(model.selectedChat?.title, "Global orchestrator")
        XCTAssertNil(model.orchestratorNotice)
        // Asked again, it is only opened.
        let again = await model.createOrchestrator(try NewOrchestratorRequest(projectID: nil))
        XCTAssertNil(again)
        XCTAssertEqual(model.orchestratorNotice, "Global orchestrator is already running.")
        XCTAssertEqual(model.orchestrators.count, 1)
        await model.disconnect()
    }
    func testTheGlobalOrchestratorFromTheSheetNeedsNoWorktree() async throws {
        let rig = try await connected(asChats: false)
        let sheet = try sheet(rig, .globalOrchestrator)
        sheet.create()
        await sheet.pending?.value
        let sent = await rig.transport.params(of: "orchestrator.create")
        XCTAssertEqual(sent, [[:]])
        XCTAssertEqual(rig.model.sessionID, rig.model.orchestrators.first?.id, "a terminal one is selected")
        await rig.model.disconnect()
    }
    func testTheGlobalOrchestratorIsInEveryProjectsStripAfterTheProjectsOwn() async throws {
        let global = entry(project: nil, chat: false, id: "aaaaaaaa-2222-4222-8222-222222222222")
        let mine = entry(project: project, chat: false)
        let other = entry(project: "22222222-2222-4222-8222-222222222222", chat: false, id: "aaaaaaaa-3333-4333-8333-333333333333")
        let rig = try await connected(orchestrators: [global, other, mine])
        XCTAssertEqual(rig.model.openSessions.map(\.id), [entryID, "aaaaaaaa-2222-4222-8222-222222222222", shell], "another project's orchestrator is not one of these tabs")
        await rig.model.disconnect()
    }

    // MARK: Failing

    func testAnOlderMacIsNeverAsked() async throws {
        let rig = try await connected(feature: false)
        let failure = await rig.model.createOrchestrator(try NewOrchestratorRequest(projectID: project))
        XCTAssertEqual(failure, .unsupported)
        let sent = await rig.transport.count("orchestrator.create")
        XCTAssertEqual(sent, 0)
        await rig.model.disconnect()
    }
    func testAMacThatRefusesTheMethodStopsBeingOffered() async throws {
        let rig = try await connected()
        await rig.transport.setOrchestratorMode(.unsupported)
        let sheet = try sheet(rig, .projectOrchestrator)
        sheet.create()
        await sheet.pending?.value
        XCTAssertEqual(sheet.orchestratorError, .unsupported)
        XCTAssertFalse(rig.model.orchestratorsOffered)
        XCTAssertTrue(sheet.unsupported)
        XCTAssertFalse(sheet.canCreate)
        XCTAssertEqual(sheet.unsupportedMessage, OrchestratorControlError.unsupportedMessage)
        XCTAssertEqual(sheet.problem?.message, OrchestratorControlError.unsupportedMessage)
        await rig.model.disconnect()
    }
    func testALostAnswerLeavesTheOutcomeUnknownAndNothingSendsItAgain() async throws {
        let rig = try await connected()
        await rig.transport.setOrchestratorMode(.timeout)
        let sheet = try sheet(rig, .projectOrchestrator)
        var dismissed = 0
        sheet.dismiss = { dismissed += 1 }
        sheet.create()
        await sheet.pending?.value
        XCTAssertEqual(sheet.orchestratorError, .outcomeUnknown)
        XCTAssertEqual(sheet.problem?.outcomeIsUncertain, true)
        XCTAssertEqual(dismissed, 0, "the sheet stays")
        XCTAssertNil(rig.model.selectedChatID)
        XCTAssertFalse(rig.model.terminalCovered)
        try await Task.sleep(for: .milliseconds(300))
        let sent = await rig.transport.count("orchestrator.create")
        XCTAssertEqual(sent, 1, "not retried by itself")
        // Only the person sends it again.
        await rig.transport.setOrchestratorMode(.ok)
        sheet.create()
        await sheet.pending?.value
        let again = await rig.transport.count("orchestrator.create")
        XCTAssertEqual(again, 2)
        XCTAssertNil(sheet.orchestratorError)
        XCTAssertNotNil(rig.model.selectedChatID)
        await rig.model.disconnect()
    }
    func testAProjectThatIsGoneAndAnAnswerThatIsNotTheOrchestratorAreSaid() async throws {
        let rig = try await connected()
        await rig.transport.setOrchestratorMode(.notFound)
        let gone = await rig.model.createOrchestrator(try NewOrchestratorRequest(projectID: project))
        XCTAssertEqual(gone, .notFound)
        await rig.transport.setOrchestratorMode(.garbled)
        let garbled = await rig.model.createOrchestrator(try NewOrchestratorRequest(projectID: project))
        XCTAssertEqual(garbled, .unreadableReply)
        XCTAssertEqual(garbled?.outcomeIsUncertain, true)
        XCTAssertNil(rig.model.selectedChatID)
        XCTAssertTrue(rig.model.orchestrators.isEmpty, "nothing was taken from an answer that was not understood")
        await rig.model.disconnect()
    }
    func testASecondRequestWhileOneIsOnTheWireIsRefusedNotSent() async throws {
        let rig = try await connected()
        await rig.transport.setOrchestratorMode(.gated)
        let sheet = try sheet(rig, .projectOrchestrator)
        sheet.create(); sheet.create(); sheet.create()
        XCTAssertTrue(sheet.busy)
        XCTAssertFalse(sheet.canCreate)
        await eventually("the first is out") { rig.model.creatingOrchestrator }
        let refused = await rig.model.createOrchestrator(try NewOrchestratorRequest(projectID: nil))
        XCTAssertEqual(refused, .busy)
        await rig.transport.setOrchestratorMode(.ok)
        await sheet.pending?.value
        let sent = await rig.transport.count("orchestrator.create")
        XCTAssertEqual(sent, 1, "a held Return or a double tap sends one")
        await rig.model.disconnect()
    }
    func testNothingIsSentWhileNotConnected() async throws {
        let rig = try await connected()
        await rig.model.disconnect()
        let failure = await rig.model.createOrchestrator(try NewOrchestratorRequest(projectID: project))
        XCTAssertEqual(failure, .notConnected)
        let sent = await rig.transport.count("orchestrator.create")
        XCTAssertEqual(sent, 0)
    }
}
