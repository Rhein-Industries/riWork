import XCTest
import RiWorkCore
@testable import RiWorkRemote

/// Chats on the phone against a scripted desktop: listing, creating (never twice), following one while it is on screen, and commands.
@MainActor final class ChatModelTests: XCTestCase {
    private let project = ChatTransport.project
    private let chatID = "cccccccc-1111-4111-8111-111111111111"
    private let otherID = "cccccccc-2222-4222-8222-222222222222"
    private var defaultsNames: [String] = []
    private var tasks: [Task<Void, Never>] = []

    override func setUp() async throws {
        // A loop that is never cancelled would hang the run; fail the test instead.
        executionTimeAllowance = 30
    }
    override func tearDown() async throws {
        for task in tasks { task.cancel() }
        tasks = []
        for name in defaultsNames { UserDefaults().removePersistentDomain(forName: name) }
        defaultsNames = []
    }

    private func chat(_ id: String? = nil, provider: ChatProvider = .codex, state: ChatState = .idle, at: UInt64 = 10, title: String = "") -> ChatInfo {
        ChatInfo(id: id ?? chatID, provider: provider, projectID: project, cwd: "/fixture", title: title, createdAtUnix: at, state: state)
    }
    private func defaults() -> UserDefaults {
        let name = "com.riwork.tests.chat.\(UUID().uuidString)"
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
    private struct Rig { let model: RemoteModel; let transport: ChatTransport }
    /// A model connected to a desktop with chats. A wait of 300 ms and a quick idle make the loop observable in a test.
    private func connected(chats: [ChatInfo]? = nil, feature: Bool = true, wait: Int = 300, options: Bool = true) async throws -> Rig {
        let transport = ChatTransport(chats: chats ?? [chat()])
        await transport.setFeature(feature)
        if !options { await transport.setOptions(nil) }
        let model = RemoteModel(client: transport, keychain: try store(), defaults: defaults(), chatWaitMilliseconds: wait, chatIdleInterval: .milliseconds(20))
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
    private func follow(_ rig: Rig, _ id: String? = nil) -> Task<Void, Never> {
        let id = id ?? chatID
        let task = Task { await rig.model.followChat(id) }
        tasks.append(task)
        return task
    }
    private func started(_ id: String, text: String) -> ChatEvent { .itemStarted(ChatItem(id: id, turnID: "t1", status: .completed, body: .agentMessage(text))) }
    private func texts(_ model: RemoteModel, _ id: String? = nil) -> [String] {
        model.conversation(id ?? chatID).transcript.items.compactMap { if case .agentMessage(let text) = $0.body { text } else { nil } }
    }

    // MARK: Listing and features

    func testAProjectLoadReadsItsChatsWhenTheDesktopHasThem() async throws {
        let rig = try await connected(chats: [chat(chatID, at: 10, title: "Old"), chat(otherID, provider: .claude, at: 20, title: "New")])
        XCTAssertEqual(rig.model.chatSupport, .supported)
        XCTAssertTrue(rig.model.chatsOffered)
        XCTAssertEqual(rig.model.projectChats.map(\.id), [otherID, chatID], "the newest first")
        let asked = await rig.transport.params(of: "chats.list")
        XCTAssertEqual(asked.first, ["project_id": .string(project)])
        await rig.model.disconnect()
    }
    func testADesktopWithoutTheFeatureIsNeverAskedForChats() async throws {
        let rig = try await connected(feature: false)
        XCTAssertEqual(rig.model.chatSupport, .unsupported)
        XCTAssertFalse(rig.model.chatsOffered)
        XCTAssertTrue(rig.model.chats.isEmpty)
        let asked = await rig.transport.count("chats.list")
        XCTAssertEqual(asked, 0, "older desktops answer “unsupported RPC method”, so it is not even asked")
        let form = try XCTUnwrap(rig.model.newTerminalForm())
        XCTAssertEqual(form.kinds, NewTerminalKind.terminalKinds)
        await rig.model.disconnect()
    }
    func testTheTabListIsReadAgainWithTheOthersAndASelectedChatThatWentAwayLeavesTheScreen() async throws {
        let rig = try await connected()
        rig.model.selectChat(chatID)
        XCTAssertEqual(rig.model.selectedChat?.id, chatID)
        XCTAssertTrue(rig.model.chatIsOnScreen)
        await rig.transport.setChats([chat(otherID, provider: .claude)])
        await rig.model.refreshSessionsQuietly()
        XCTAssertEqual(rig.model.chats.map(\.id), [otherID])
        XCTAssertNil(rig.model.selectedChatID, "the terminals come back")
        XCTAssertFalse(rig.model.chatIsOnScreen)
        await rig.model.disconnect()
    }
    func testAChatTabTakesTheScreenAndATerminalTabGivesItBack() async throws {
        let rig = try await connected()
        rig.model.selectChat(chatID)
        XCTAssertEqual(rig.model.selectedChatID, chatID)
        rig.model.selectChat("not-a-chat")
        XCTAssertEqual(rig.model.selectedChatID, chatID, "only a listed chat can be selected")
        let shell = try XCTUnwrap(rig.model.openSessions.first)
        await rig.model.chooseSession(shell)
        XCTAssertNil(rig.model.selectedChatID)
        await rig.model.disconnect()
    }
    // MARK: Following

    func testFollowingAsksAfterTheLastEventItHasAndWaitsForNewOnes() async throws {
        let rig = try await connected()
        await rig.transport.append(chatID, [.state(.idle), started("a", text: "one")])
        let follower = follow(rig)
        await eventually("the first page is in") { rig.model.conversation(self.chatID).feed.loaded && self.texts(rig.model) == ["one"] }
        XCTAssertEqual(rig.model.conversation(chatID).feed.next, 2)
        // It then asks from 2 and the desktop holds the request: a long poll.
        await eventually("it asks again from where it is") { await rig.transport.sinces().contains(2) }
        let all = await rig.transport.calls(of: "chat.events")
        let first = try XCTUnwrap(all.first)
        XCTAssertEqual(first.params["since"], .number(0))
        XCTAssertEqual(first.params["wait_ms"], .number(300), "the wait is asked for on the first request too: the desktop answers at once when there is something")
        XCTAssertNil(first.params["max_events"])
        await rig.transport.append(chatID, [started("b", text: "two")])
        await eventually("the new event shows up") { self.texts(rig.model) == ["one", "two"] }
        await eventually("and it asks from 3") { await rig.transport.sinces().contains(3) }
        let sinces = await rig.transport.sinces()
        XCTAssertEqual(sinces.filter { $0 == 0 }.count, 1, "events already held are never asked for again")
        follower.cancel()
        await rig.model.disconnect()
    }
    func testItStopsAskingWhenTheScreenGoesAndTheDesktopsWaitKeepsItsSlot() async throws {
        let rig = try await connected(wait: 2000)
        let follower = follow(rig)
        await eventually("a long poll is out") { await rig.transport.count("chat.events") >= 1 && rig.model.conversation(self.chatID).following }
        try await Task.sleep(for: .milliseconds(50))
        follower.cancel()
        await follower.value
        XCTAssertFalse(rig.model.conversation(chatID).following)
        let askedAtCancel = await rig.transport.count("chat.events")
        XCTAssertEqual(rig.model.waitSlots.stillWaiting(at: ProcessInfo.processInfo.systemUptime), 1, "the desktop still holds that wait, and the phone knows")
        await rig.transport.append(chatID, [started("a", text: "late")])
        try await Task.sleep(for: .milliseconds(300))
        let askedLater = await rig.transport.count("chat.events")
        XCTAssertEqual(askedLater, askedAtCancel, "nothing is asked of the desktop for a chat nobody is looking at")
        XCTAssertTrue(texts(rig.model).isEmpty, "and the late event waits on the desktop until the screen is back")
        // Back on screen: it carries on from where it was.
        let again = follow(rig)
        await eventually("the late event arrives") { self.texts(rig.model) == ["late"] }
        again.cancel()
        await rig.model.disconnect()
    }
    func testALongPollThatWouldBeAThirdIsAShortRead() async throws {
        let rig = try await connected(wait: 2000)
        let now = ProcessInfo.processInfo.systemUptime
        rig.model.waitSlots.abandon(startedAt: now, wait: 30)
        rig.model.waitSlots.abandon(startedAt: now, wait: 30)
        let follower = follow(rig)
        await eventually("a request went out") { await rig.transport.count("chat.events") >= 1 }
        let all = await rig.transport.calls(of: "chat.events")
        let first = try XCTUnwrap(all.first)
        XCTAssertEqual(first.params["wait_ms"], .number(0), "two waits are already held on the desktop")
        // And the loop does not spin: a short read that found nothing is followed by a pause.
        try await Task.sleep(for: .milliseconds(400))
        let asked = await rig.transport.count("chat.events")
        XCTAssertLessThan(asked, 8)
        follower.cancel()
        await rig.model.disconnect()
    }
    func testAPageThatWasCutIsFollowedAtOnceAndTheRestArrivesInOrder() async throws {
        let rig = try await connected(wait: 2000)
        await rig.transport.append(chatID, (1...1200).map { started("m\($0)", text: "line \($0)") })
        let follower = follow(rig)
        await eventually("all of it is in") { rig.model.conversation(self.chatID).transcript.items.count == 1200 }
        let sinces = await rig.transport.sinces()
        XCTAssertEqual(Array(sinces.prefix(3)), [0, 500, 1000], "each page continues from the one before without waiting for anything")
        XCTAssertEqual(texts(rig.model).last, "line 1200")
        follower.cancel()
        await rig.model.disconnect()
    }
    func testWhatThePhoneCannotReadIsSkippedAndTheRestOfThePageIsKept() async throws {
        let rig = try await connected()
        await rig.transport.append(chatID, [started("a", text: "before")])
        await rig.transport.appendRaw(chatID, #"{"event":"hologram","payload":{"x":1}}"#)
        await rig.transport.appendRaw(chatID, #"{"event":"item_started","item":{"id":"z","status":"completed","body":{"type":"hologram"}}}"#)
        await rig.transport.append(chatID, [started("b", text: "after")])
        let follower = follow(rig)
        await eventually("both readable messages are in") { self.texts(rig.model) == ["before", "after"] }
        XCTAssertEqual(rig.model.conversation(chatID).feed.skipped, 2)
        XCTAssertEqual(rig.model.conversation(chatID).feed.next, 4, "the unknown ones count, so they are not asked for again")
        XCTAssertNil(rig.model.conversation(chatID).readError)
        follower.cancel()
        await rig.model.disconnect()
    }
    func testAfterAReconnectItCarriesOnFromNextAndNothingIsRepeated() async throws {
        let rig = try await connected()
        await rig.transport.append(chatID, [started("a", text: "one"), started("b", text: "two")])
        let follower = follow(rig)
        await eventually("two events") { self.texts(rig.model).count == 2 }
        await rig.transport.drop()
        await eventually("the loss is noticed") { rig.model.state == .failed }
        XCTAssertEqual(texts(rig.model), ["one", "two"], "the transcript is kept while the link is down")
        await rig.transport.append(chatID, [started("c", text: "three")])
        let askedWhileDown = await rig.transport.count("chat.events")
        try await Task.sleep(for: .milliseconds(150))
        let askedStill = await rig.transport.count("chat.events")
        XCTAssertEqual(askedStill, askedWhileDown, "nothing is asked while the link is down")
        await rig.model.connect()
        XCTAssertEqual(rig.model.state, .connected)
        await eventually("the event that came while it was away arrives") { self.texts(rig.model) == ["one", "two", "three"] }
        let sinces = await rig.transport.sinces()
        XCTAssertEqual(sinces.last.map { $0 >= 2 }, true)
        XCTAssertEqual(sinces.filter { $0 == 0 }.count, 1, "it never started over")
        XCTAssertEqual(rig.model.conversation(chatID).feed.next, 3)
        follower.cancel()
        await rig.model.disconnect()
    }
    func testErrorsBackOffAndAnAnswerClearsThem() async throws {
        let rig = try await connected(wait: 50)
        await rig.transport.failEvents(3)
        let follower = follow(rig)
        await eventually("it has asked four times") { await rig.transport.count("chat.events") >= 4 }
        let times = await rig.transport.calls(of: "chat.events").map(\.at)
        let gaps = zip(times.dropFirst(), times).map { ($0 - $1).timeInterval }
        XCTAssertGreaterThanOrEqual(gaps[0], 0.2, "250 ms after the first failure")
        XCTAssertGreaterThanOrEqual(gaps[1], 0.45, "500 ms after the second")
        XCTAssertGreaterThanOrEqual(gaps[2], 0.9, "1 s after the third")
        await eventually("the fourth answered, and the error is gone") { rig.model.conversation(self.chatID).readError == nil }
        XCTAssertEqual(rig.model.state, .connected, "a failing request is not a lost link")
        follower.cancel()
        await rig.model.disconnect()
    }
    func testAnErrorIsShownWhileItLastsAndTheLoopKeepsTrying() async throws {
        let rig = try await connected(wait: 50)
        await rig.transport.failEvents(1_000)
        let follower = follow(rig)
        await eventually("the error is shown") { rig.model.conversation(self.chatID).readError != nil }
        XCTAssertEqual(rig.model.conversation(chatID).readError, .failed("the chat host is not answering"))
        await rig.transport.failEvents(0)
        await rig.transport.append(chatID, [started("a", text: "back")])
        await eventually("it recovers by itself") { self.texts(rig.model) == ["back"] && rig.model.conversation(self.chatID).readError == nil }
        follower.cancel()
        await rig.model.disconnect()
    }
    func testNothingIsAskedWhileTheAppIsNotActive() async throws {
        let rig = try await connected(wait: 50)
        rig.model.setAppActive(false)
        let follower = follow(rig)
        try await Task.sleep(for: .milliseconds(200))
        var asked = await rig.transport.count("chat.events")
        XCTAssertEqual(asked, 0)
        rig.model.setAppActive(true)
        await eventually("it starts when the app is back") { await rig.transport.count("chat.events") >= 1 }
        asked = await rig.transport.count("chat.events")
        XCTAssertGreaterThanOrEqual(asked, 1)
        follower.cancel()
        await rig.model.disconnect()
    }
    func testAChatThatIsGoneEndsTheFollowingAndLeavesTheTabList() async throws {
        let rig = try await connected()
        rig.model.selectChat(chatID)
        await rig.transport.setGone(true)
        await rig.transport.setChats([])
        let follower = follow(rig)
        await follower.value
        let conversation = rig.model.conversation(chatID)
        XCTAssertTrue(conversation.gone)
        XCTAssertEqual(conversation.readError, .notFound(.events))
        XCTAssertTrue(rig.model.chats.isEmpty, "the list was read again")
        XCTAssertNil(rig.model.selectedChatID)
        let asked = await rig.transport.count("chat.events")
        XCTAssertEqual(asked, 1, "and it does not ask again")
        await rig.model.disconnect()
    }
    func testADesktopThatRefusesChatEventsIsNotAskedAgain() async throws {
        let rig = try await connected()
        await rig.transport.setFeature(false)
        let follower = follow(rig)
        await eventually("it gave up") { rig.model.chatSupport == .unsupported }
        _ = follower
        await follower.value
        await rig.model.disconnect()
    }
    func testTheTranscriptOfAStreamingMessageGrowsWithoutRebuildingTheRest() async throws {
        let rig = try await connected()
        await rig.transport.append(chatID, [.turnStarted(turnID: "t1"), .itemStarted(ChatItem(id: "a", turnID: "t1", body: .agentMessage("")))])
        let follower = follow(rig)
        await eventually("started") { rig.model.conversation(self.chatID).transcript.items.count == 1 }
        for part in ["Hel", "lo ", "world"] {
            await rig.transport.append(chatID, [.itemDelta(itemID: "a", delta: .text(part))])
        }
        await eventually("deltas are folded") { self.texts(rig.model) == ["Hello world"] }
        await rig.transport.append(chatID, [.itemCompleted(ChatItem(id: "a", turnID: "t1", status: .completed, body: .agentMessage("Hello world!"))), .turnCompleted(turnID: "t1", outcome: .completed)])
        await eventually("completion replaces it") { self.texts(rig.model) == ["Hello world!"] }
        follower.cancel()
        await rig.model.disconnect()
    }
    func testTheConversationsOfOtherChatsAreKeptUpToALimitAndTheFollowedOneIsNeverDropped() async throws {
        let rig = try await connected()
        let follower = follow(rig)
        await eventually("following") { rig.model.conversation(self.chatID).following }
        for index in 0..<20 { _ = rig.model.conversation(String(format: "dddddddd-0000-4000-8000-%012d", index)) }
        XCTAssertLessThanOrEqual(rig.model.chatConversations.count, RemoteModel.keptConversations)
        XCTAssertNotNil(rig.model.chatConversations[chatID], "the one on screen stays")
        follower.cancel()
        await rig.model.disconnect()
    }

    // MARK: Creating

    private func createRequest(_ provider: ChatProvider = .codex, mode: ChatApprovalMode? = nil) throws -> ChatCreateRequest {
        try ChatCreateRequest(provider: provider, target: .project(project), approvalMode: mode)
    }
    func testCreatingAChatSendsOneRequestPutsTheChatOnScreenAndRemembersTheKind() async throws {
        let rig = try await connected(chats: [])
        let model = rig.model
        XCTAssertEqual(model.lastTerminalKind, .shell)
        var created: [ChatInfo] = []
        let failure = await model.createChat(try createRequest(.claude)) { created.append($0) }
        XCTAssertNil(failure)
        let sent = await rig.transport.params(of: "chat.create")
        XCTAssertEqual(sent, [["provider": .string("claude"), "project_id": .string(project)]])
        let made = try XCTUnwrap(created.first)
        XCTAssertEqual(model.selectedChatID, made.id)
        XCTAssertEqual(model.chats.map(\.id), [made.id])
        XCTAssertEqual(model.lastTerminalKind, .claudeChat)
        XCTAssertFalse(model.creatingChat)
        XCTAssertEqual(model.chatSupport, .supported)
        let listed = await rig.transport.count("chats.list")
        XCTAssertGreaterThanOrEqual(listed, 2, "the list was read again after creating")
        await model.disconnect()
    }
    func testAnUnrestrictedChatStartsInFullModeAndNothingElseSaysAMode() async throws {
        let rig = try await connected(chats: [])
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        XCTAssertEqual(sheet.form.kinds, NewTerminalKind.allCases, "this desktop has chats")
        sheet.select(kind: .codexChat)
        sheet.create()
        await sheet.pending?.value
        sheet.select(kind: .claudeChat)
        sheet.setUnrestricted(true)
        sheet.create()
        await sheet.pending?.value
        let sent = await rig.transport.params(of: "chat.create")
        XCTAssertEqual(sent.map { $0["provider"]?.string }, ["codex", "claude"])
        XCTAssertEqual(sent.map { $0["approval_mode"]?.string }, [nil, "full"])
        XCTAssertEqual(rig.model.chats.count, 2)
        await rig.model.disconnect()
    }
    func testASecondCreateWhileOneIsOnTheWireIsRefusedNotSent() async throws {
        let rig = try await connected(chats: [])
        await rig.transport.setCreateMode(.gated)
        let request = try createRequest()
        let one = Task { await rig.model.createChat(request) }
        await eventually("the first is out") { rig.model.creatingChat }
        let refused = await rig.model.createChat(try createRequest(.claude))
        XCTAssertEqual(refused, .busy)
        await rig.transport.setCreateMode(.ok)
        let result = await one.value
        XCTAssertNil(result)
        let count = await rig.transport.count("chat.create")
        XCTAssertEqual(count, 1)
        await rig.model.disconnect()
    }
    func testTheSheetSendsOneChatRequestForAHeldReturnOrADoubleTap() async throws {
        let rig = try await connected(chats: [])
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.select(kind: .claudeChat)
        var dismissed = 0
        sheet.dismiss = { dismissed += 1 }
        await rig.transport.setCreateMode(.gated)
        sheet.create(); sheet.create(); sheet.create()
        XCTAssertTrue(sheet.busy)
        XCTAssertFalse(sheet.canCreate)
        await rig.transport.setCreateMode(.ok)
        await sheet.pending?.value
        let count = await rig.transport.count("chat.create")
        XCTAssertEqual(count, 1)
        XCTAssertGreaterThanOrEqual(dismissed, 1)
        XCTAssertNil(sheet.chatError)
        await rig.model.disconnect()
    }
    func testALostAnswerLeavesTheOutcomeUnknownAndTheCreateIsNeverRetried() async throws {
        let rig = try await connected(chats: [])
        await rig.transport.setCreateMode(.timeout)
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.select(kind: .codexChat)
        sheet.create()
        await sheet.pending?.value
        XCTAssertEqual(sheet.chatError, .outcomeUnknown(.create(.codex)))
        XCTAssertEqual(sheet.problem?.outcomeIsUncertain, true)
        XCTAssertTrue(sheet.problem?.message.contains("may or may not have been created") == true)
        XCTAssertNil(rig.model.selectedChatID)
        XCTAssertNotEqual(rig.model.lastTerminalKind, .codexChat, "only a success is remembered")
        // Nothing sends it again by itself: not time, not a reconnect.
        try? await Task.sleep(for: .milliseconds(300))
        await rig.model.disconnect()
        await rig.model.connect()
        try? await Task.sleep(for: .milliseconds(100))
        let count = await rig.transport.count("chat.create")
        XCTAssertEqual(count, 1)
        await rig.model.disconnect()
    }
    func testFailuresKeepTheSheetOpenWithTheReason() async throws {
        let rig = try await connected(chats: [])
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        var dismissed = 0
        sheet.dismiss = { dismissed += 1 }
        sheet.select(kind: .claudeChat)
        func attempt(_ mode: ChatTransport.CreateMode) async -> String? {
            await rig.transport.setCreateMode(mode)
            sheet.create()
            await sheet.pending?.value
            return sheet.problem?.message
        }
        let harness = await attempt(.harness)
        XCTAssertEqual(harness, "Claude isn’t installed on the Mac (or isn’t on its PATH).")
        let missing = await attempt(.notFound)
        XCTAssertEqual(missing, "That project or worktree no longer exists on the Mac. Refresh and try again.")
        XCTAssertEqual(dismissed, 0)
        XCTAssertNil(rig.model.selectedChatID)
        sheet.select(kind: .shell)
        XCTAssertNil(sheet.problem, "choosing something else takes the message down")
        await rig.model.disconnect()
    }
    func testADesktopThatRefusesChatsDisablesCreateAndAReconnectTriesAgain() async throws {
        let rig = try await connected(chats: [])
        await rig.transport.setCreateMode(.unsupported)
        let failure = await rig.model.createChat(try createRequest())
        XCTAssertEqual(failure, .unsupported)
        XCTAssertEqual(rig.model.chatSupport, .unsupported)
        XCTAssertFalse(rig.model.chatsOffered)
        let again = await rig.model.createChat(try createRequest())
        XCTAssertEqual(again, .unsupported)
        let sent = await rig.transport.count("chat.create")
        XCTAssertEqual(sent, 1, "no more requests go out")
        await rig.model.disconnect()
        await rig.transport.setCreateMode(.ok)
        await rig.model.connect()
        XCTAssertEqual(rig.model.chatSupport, .supported, "a new connection reads the features again")
        await rig.model.disconnect()
    }
    func testNothingIsSentWhileDisconnected() async throws {
        let rig = try await connected(chats: [])
        await rig.model.disconnect()
        let failure = await rig.model.createChat(try createRequest())
        XCTAssertEqual(failure, .notConnected)
        let sent = await rig.model.sendChatCommand(chatID, .interrupt)
        XCTAssertEqual(sent, .notConnected)
        let count = await rig.transport.count("chat.create") + rig.transport.count("chat.command")
        XCTAssertEqual(count, 0)
    }

    // MARK: Commands

    func testSendingClearsTheDraftSendsOnceAndJumpsToTheEnd() async throws {
        let rig = try await connected()
        let conversation = rig.model.conversation(chatID)
        conversation.draft = "  fix the build\n"
        let jumps = conversation.jumps
        let failure = await rig.model.sendChatDraft(chatID)
        XCTAssertNil(failure)
        XCTAssertEqual(conversation.draft, "")
        XCTAssertFalse(conversation.sending)
        XCTAssertEqual(conversation.jumps, jumps &+ 1)
        let commands = await rig.transport.commands()
        XCTAssertEqual(commands, [.object(["command": .string("send"), "text": .string("  fix the build\n")])], "the message is sent as written")
        await rig.model.disconnect()
    }
    func testABlankDraftSendsNothing() async throws {
        let rig = try await connected()
        rig.model.conversation(chatID).draft = " \n "
        let failure = await rig.model.sendChatDraft(chatID)
        XCTAssertNil(failure)
        let count = await rig.transport.count("chat.command")
        XCTAssertEqual(count, 0)
        await rig.model.disconnect()
    }
    func testAMessageThatDidNotGoThroughComesBackToTheComposerAndIsNotSentAgain() async throws {
        let rig = try await connected()
        let conversation = rig.model.conversation(chatID)
        conversation.draft = "important"
        await rig.transport.failCommand(.timeout)
        let failure = await rig.model.sendChatDraft(chatID)
        XCTAssertEqual(failure, .outcomeUnknown(.command))
        XCTAssertEqual(conversation.draft, "important", "nothing the person wrote is lost")
        XCTAssertTrue(conversation.notice?.contains("may or may not have gone through") == true)
        try? await Task.sleep(for: .milliseconds(200))
        let count = await rig.transport.count("chat.command")
        XCTAssertEqual(count, 1, "never sent again by itself")
        await rig.model.disconnect()
    }
    func testWhatIsTypedWhileAMessageIsOnItsWayIsKeptWhenItComesBack() async throws {
        let rig = try await connected()
        let conversation = rig.model.conversation(chatID)
        conversation.draft = "first"
        await rig.transport.gateCommands(true)
        await rig.transport.failCommand(.rpc(code: "cli_error", message: "host down"))
        let sending = Task { await rig.model.sendChatDraft(chatID) }
        await eventually("on its way") { conversation.sending }
        let again = await rig.model.sendChatDraft(chatID)
        XCTAssertNil(again, "an empty draft")
        conversation.draft = "second"
        let refusedWhileSending = await rig.model.sendChatMessage(chatID, "second")
        XCTAssertEqual(refusedWhileSending, .busy, "a second message waits for the first")
        await rig.transport.gateCommands(false)
        let failure = await sending.value
        XCTAssertEqual(failure, .failed("host down"))
        XCTAssertEqual(conversation.draft, "first\nsecond")
        await rig.model.disconnect()
    }
    func testAnApprovalIsAnsweredOnceAndLeavesTheBarAtOnce() async throws {
        let rig = try await connected()
        let approval = ChatApproval(requestID: "r1", kind: .command, title: "rm -rf build", choices: [.accept, .acceptForSession, .decline, .cancel])
        await rig.transport.append(chatID, [.turnStarted(turnID: "t1"), .state(.waiting), .approvalRequested(approval)])
        let follower = follow(rig)
        let conversation = rig.model.conversation(chatID)
        await eventually("the request is there") { conversation.openApprovals.map(\.requestID) == ["r1"] }
        await rig.transport.gateCommands(true)
        let first = Task { await rig.model.decideChatApproval(self.chatID, approval, .acceptForSession) }
        await eventually("the first is out") { conversation.answered.contains("r1") }
        let second = await rig.model.decideChatApproval(chatID, approval, .accept)
        XCTAssertEqual(second, .busy, "a second tap while the first is out sends nothing")
        await rig.transport.gateCommands(false)
        let result = await first.value
        let third = await rig.model.decideChatApproval(chatID, approval, .decline)
        XCTAssertEqual(third, .busy, "nor one after it")
        XCTAssertNil(result)
        XCTAssertTrue(conversation.openApprovals.isEmpty, "gone from the bar before the desktop has said so")
        XCTAssertEqual(conversation.transcript.approvals.map(\.requestID), ["r1"], "the transcript is the desktop's word")
        let commands = await rig.transport.commands()
        XCTAssertEqual(commands, [.object(["command": .string("approve"), "request_id": .string("r1"), "decision": .string("accept_for_session")])])
        // The desktop resolves it; the bookkeeping is released.
        await rig.transport.append(chatID, [.approvalResolved(requestID: "r1", decision: .acceptForSession)])
        await eventually("resolved") { conversation.transcript.approvals.isEmpty && conversation.answered.isEmpty }
        follower.cancel()
        await rig.model.disconnect()
    }
    func testARefusedApprovalIsSaidQuietlyAndStaysOut() async throws {
        let rig = try await connected()
        let approval = ChatApproval(requestID: "r1", kind: .tool, title: "Bash", choices: [.accept, .decline])
        await rig.transport.append(chatID, [.approvalRequested(approval)])
        let follower = follow(rig)
        let conversation = rig.model.conversation(chatID)
        await eventually("the request is there") { conversation.openApprovals.count == 1 }
        await rig.transport.failCommand(.rpc(code: "invalid_request", message: "request already answered"))
        let failure = await rig.model.decideChatApproval(chatID, approval, .accept)
        XCTAssertEqual(failure, .invalid("request already answered"))
        XCTAssertEqual(conversation.notice, "The Mac refused the request: request already answered")
        XCTAssertTrue(conversation.openApprovals.isEmpty, "an answered request is not asked again")
        follower.cancel()
        await rig.model.disconnect()
    }
    func testAnApprovalWhoseAnswerWasLostComesBackToTheBar() async throws {
        let rig = try await connected()
        let approval = ChatApproval(requestID: "r1", kind: .command, title: "ls", choices: [.accept, .decline])
        await rig.transport.append(chatID, [.approvalRequested(approval)])
        let follower = follow(rig)
        let conversation = rig.model.conversation(chatID)
        await eventually("the request is there") { conversation.openApprovals.count == 1 }
        await rig.transport.failCommand(.timeout)
        let failure = await rig.model.decideChatApproval(chatID, approval, .accept)
        XCTAssertEqual(failure?.outcomeIsUncertain, true)
        XCTAssertEqual(conversation.openApprovals.map(\.requestID), ["r1"], "it may not have been answered: the bar offers it again, and the events will say if it was")
        follower.cancel()
        await rig.model.disconnect()
    }
    func testAnswersToQuestionsAreSentInOrder() async throws {
        let rig = try await connected()
        let question = ChatQuestion(requestID: "q1", questions: [
            ChatQuestionPrompt(question: "Which?", options: [ChatQuestionOption(label: "A"), ChatQuestionOption(label: "B")]),
            ChatQuestionPrompt(question: "Name?")
        ])
        var form = ChatAnswerForm(question: question)
        let early = await rig.model.answerChatQuestion(chatID, form)
        XCTAssertEqual(early, .busy, "an incomplete form is not sent")
        form.toggle(prompt: 0, option: 1)
        form.setText(prompt: 1, "Ada")
        let failure = await rig.model.answerChatQuestion(chatID, form)
        XCTAssertNil(failure)
        let commands = await rig.transport.commands()
        XCTAssertEqual(commands, [.object(["command": .string("answer"), "request_id": .string("q1"), "answers": .array([.array([.string("B")]), .array([.string("Ada")])])])])
        await rig.model.disconnect()
    }
    func testModeInterruptCompactAndStopAreOneRequestEach() async throws {
        let rig = try await connected()
        let conversation = rig.model.conversation(chatID)
        let mode = await rig.model.setChatMode(chatID, .plan)
        XCTAssertNil(mode)
        XCTAssertEqual(conversation.pendingMode, .plan, "the picker shows the choice until the desktop's own word replaces it")
        _ = await rig.model.interruptChat(chatID)
        _ = await rig.model.compactChat(chatID)
        let stopped = await rig.model.stopChat(chatID)
        XCTAssertNil(stopped)
        let commands = await rig.transport.commands()
        XCTAssertEqual(commands, [.object(["command": .string("configure"), "approval_mode": .string("plan")]), .object(["command": .string("interrupt")]), .object(["command": .string("compact")])])
        let stops = await rig.transport.params(of: "chat.stop")
        XCTAssertEqual(stops, [["chat_id": .string(chatID)]])
        await rig.model.disconnect()
    }
    func testAModeTheDesktopRefusesIsTakenBackAndOneItConfirmsIsNoLongerPending() async throws {
        let rig = try await connected()
        let conversation = rig.model.conversation(chatID)
        await rig.transport.failCommand(.rpc(code: "cli_error", message: "no"))
        let failure = await rig.model.setChatMode(chatID, .full)
        XCTAssertEqual(failure, .failed("no"))
        XCTAssertNil(conversation.pendingMode)
        XCTAssertEqual(conversation.notice, "no")
        // The desktop confirms with an `info` event.
        _ = await rig.model.setChatMode(chatID, .autoEdit)
        XCTAssertEqual(conversation.pendingMode, .autoEdit)
        await rig.transport.append(chatID, [.info(chat(state: .idle).withMode(.autoEdit))])
        let follower = follow(rig)
        await eventually("confirmed") { conversation.pendingMode == nil && conversation.transcript.info?.approvalMode == .autoEdit }
        follower.cancel()
        await rig.model.disconnect()
    }
    // MARK: Model and effort

    func testTheOptionsAreAskedOncePerConnectionAndAReconnectAsksAgain() async throws {
        let rig = try await connected()
        // Asked as the connection learns the desktop has chats, before a chat is on screen; asking again asks nothing.
        await eventually("read at connect") { rig.model.chatOptionsSupport == .supported }
        await rig.model.loadChatOptions()
        await rig.model.loadChatOptions()
        XCTAssertEqual(rig.model.chatOptions?[.claude]?.models, ["opus", "sonnet", "haiku"])
        XCTAssertEqual(rig.model.chatOptions?[.codex]?.efforts, ["low", "medium", "high", "xhigh"])
        let asked = await rig.transport.count("chat.options")
        XCTAssertEqual(asked, 1)
        await rig.model.disconnect()
        // A new connection may reach another build of the Mac: it is asked again.
        await rig.transport.setOptions(nil)
        await rig.model.connect()
        await eventually("asked again") { await rig.transport.count("chat.options") == 2 }
        await eventually("and believed") { rig.model.chatOptionsSupport == .unsupported && rig.model.chatOptions == nil }
        await rig.model.disconnect()
    }
    func testAMacFromBeforeTheOptionsOffersNoModelAndIsNotAskedAgainButChatsGoOn() async throws {
        let rig = try await connected(options: false)
        await eventually("asked at connect") { rig.model.chatOptionsSupport == .unsupported }
        await rig.model.loadChatOptions()
        XCTAssertNil(rig.model.chatOptions)
        XCTAssertEqual(rig.model.chatOptionsSupport, .unsupported)
        XCTAssertEqual(rig.model.chatSupport, .supported, "the chats themselves are not given up")
        await rig.model.loadChatOptions()
        let asked = await rig.transport.count("chat.options")
        XCTAssertEqual(asked, 1)
        // A desktop without chats is not asked at all.
        let none = try await connected(feature: false)
        await none.model.loadChatOptions()
        let noneAsked = await none.transport.count("chat.options")
        XCTAssertEqual(noneAsked, 0)
        await rig.model.disconnect(); await none.model.disconnect()
    }
    func testAModelOrAnEffortIsOneConfigureShownAtOnceAndConfirmedByTheDesktop() async throws {
        let rig = try await connected(chats: [chat(provider: .claude)])
        let conversation = rig.model.conversation(chatID)
        // The desktop's host publishes the chat's new settings, as `configure` does on the Mac.
        await rig.transport.handleCommands { [project] id, command in
            guard case .configure(let model, let effort, _) = command else { return [] }
            return [.info(ChatInfo(id: id, provider: .claude, projectID: project, cwd: "/fixture", title: "", createdAtUnix: 10, model: model ?? "sonnet", effort: effort ?? "high", state: .idle))]
        }
        let failure = await rig.model.setChatModel(chatID, model: "sonnet")
        XCTAssertNil(failure)
        XCTAssertEqual(conversation.pendingModel, "sonnet", "the menu shows the choice until the desktop's own word replaces it")
        XCTAssertNil(conversation.pendingEffort)
        _ = await rig.model.setChatModel(chatID, effort: "max")
        let nothing = await rig.model.setChatModel(chatID)
        XCTAssertNil(nothing, "nothing to change sends nothing")
        let commands = await rig.transport.commands()
        XCTAssertEqual(commands, [.object(["command": .string("configure"), "model": .string("sonnet")]), .object(["command": .string("configure"), "effort": .string("max")])])
        let follower = follow(rig)
        await eventually("confirmed") { conversation.pendingModel == nil && conversation.pendingEffort == nil && conversation.transcript.info?.effort == "max" }
        XCTAssertEqual(conversation.transcript.info?.model, "sonnet")
        follower.cancel()
        await rig.model.disconnect()
    }
    func testAModelTheDesktopRefusesIsTakenBackAndSaidAboveTheComposer() async throws {
        let rig = try await connected()
        let conversation = rig.model.conversation(chatID)
        await rig.transport.failCommand(.rpc(code: "cli_error", message: "the chat is stopped; send a message to resume it"))
        let failure = await rig.model.setChatModel(chatID, model: "gpt-5.5", effort: "high")
        XCTAssertEqual(failure, .failed("the chat is stopped; send a message to resume it"))
        XCTAssertNil(conversation.pendingModel); XCTAssertNil(conversation.pendingEffort)
        XCTAssertEqual(conversation.notice, "the chat is stopped; send a message to resume it")
        // One that cannot be sent never leaves the phone.
        let blank = await rig.model.setChatModel(chatID, model: " ")
        XCTAssertNotNil(blank)
        XCTAssertNil(conversation.pendingModel)
        let sent = await rig.transport.commands()
        XCTAssertEqual(sent, [.object(["command": .string("configure"), "model": .string("gpt-5.5"), "effort": .string("high")])], "the refused one only")
        await rig.model.disconnect()
    }
    func testAModelChosenOnTheMacReachesThePhoneWhileItFollows() async throws {
        let rig = try await connected()
        let conversation = rig.model.conversation(chatID)
        let follower = follow(rig)
        await eventually("loaded") { conversation.feed.loaded }
        await rig.transport.append(chatID, [.info(ChatInfo(id: chatID, provider: .codex, projectID: project, cwd: "/fixture", title: "", createdAtUnix: 10, model: "gpt-5.5-codex", effort: "xhigh", state: .running))])
        await eventually("the Mac's choice is in") { conversation.transcript.info?.model == "gpt-5.5-codex" }
        let menu = ChatModelMenu(choices: ChatOptions.Choices(efforts: ["low", "high"]), model: conversation.transcript.info?.model, effort: conversation.transcript.info?.effort)
        XCTAssertEqual(menu.models, ["gpt-5.5-codex"]); XCTAssertEqual(menu.efforts, ["low", "high", "xhigh"])
        follower.cancel()
        await rig.model.disconnect()
    }

    func testRetryOnAFailedChatSendsTheLastMessageAgainOnlyWhenAskedTo() async throws {
        let rig = try await connected()
        await rig.transport.append(chatID, [.itemStarted(ChatItem(id: "u", status: .completed, body: .userMessage("run the tests"))), .state(.failed("process exited"))])
        let follower = follow(rig)
        let conversation = rig.model.conversation(chatID)
        await eventually("failed") { conversation.transcript.state == .failed("process exited") }
        let banner = try XCTUnwrap(ChatBanner(state: conversation.transcript.state, provider: .codex, lastMessage: conversation.transcript.lastUserMessage))
        XCTAssertEqual(banner, .failed(message: "process exited", retry: "run the tests"))
        var count = await rig.transport.count("chat.command")
        XCTAssertEqual(count, 0, "nothing is retried by itself")
        _ = await rig.model.sendChatMessage(chatID, "run the tests")
        count = await rig.transport.count("chat.command")
        XCTAssertEqual(count, 1)
        follower.cancel()
        await rig.model.disconnect()
    }
    func testTheTabSaysWhatTheChatIsDoing() async throws {
        let rig = try await connected(chats: [chat(state: .idle)])
        let listed = try XCTUnwrap(rig.model.chats.first)
        XCTAssertEqual(rig.model.chatActivity(listed), .unknown)
        await rig.transport.append(chatID, [.state(.running)])
        let follower = follow(rig)
        await eventually("followed and running") { rig.model.chatActivity(listed) == .working }
        await rig.transport.append(chatID, [.approvalRequested(ChatApproval(requestID: "r", kind: .command, title: "x", choices: [])), .state(.waiting)])
        await eventually("waiting") { rig.model.chatActivity(listed) == .waiting }
        follower.cancel()
        await follower.value
        // Not followed any more: the list's word again.
        XCTAssertEqual(rig.model.chatActivity(listed), .unknown)
        await rig.model.disconnect()
    }
}

private extension ChatInfo {
    func withMode(_ mode: ChatApprovalMode) -> ChatInfo { var copy = self; copy.approvalMode = mode; return copy }
}
