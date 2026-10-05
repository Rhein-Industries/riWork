import XCTest
import RiWorkCore
@testable import RiWorkRemote

/// An orchestrator the Mac runs as a chat (`"mode": "chat"`, with `chat_id` and `provider`), against a scripted desktop: it opens the chat
/// screen for its `chat_id` and not a terminal, a terminal orchestrator is what it always was, and on a desktop without chats it opens
/// neither.
@MainActor final class OrchestratorChatTests: XCTestCase {
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

    /// What `orchestrators.list` holds for the project's orchestrator.
    private func entry(_ extra: String = "", id: String? = nil, alive: Bool = true) -> String {
        "{\"id\":\"\(id ?? entryID)\",\"project_id\":\"\(project)\",\"worktree_id\":null,\"kind\":\"orchestrator\",\"cwd\":\"/fixture\",\"harness\":null,\"alive\":\(alive),\"created_at_unix\":3\(extra.isEmpty ? "" : "," + extra)}"
    }
    private func chatEntry(_ extra: String = "", id: String? = nil, alive: Bool = true) -> String {
        entry("\"mode\":\"chat\",\"chat_id\":\"\(chatID)\",\"provider\":\"claude\"\(extra.isEmpty ? "" : "," + extra)", id: id, alive: alive)
    }
    private func listedChat(state: ChatState = .idle) -> ChatInfo {
        ChatInfo(id: chatID, provider: .claude, projectID: project, cwd: "/fixture", title: "Whatever the chat is called", createdAtUnix: 10, state: state)
    }
    private func store(selected session: String = ChatTransport.shell) throws -> KeychainStore {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = session
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        return keychain
    }
    private struct Rig { let model: RemoteModel; let transport: ChatTransport }
    private func connected(orchestrators: [String], chats: [ChatInfo] = [], feature: Bool = true, selected session: String = ChatTransport.shell) async throws -> Rig {
        let transport = ChatTransport(chats: chats)
        await transport.setFeature(feature)
        await transport.setOrchestrators(orchestrators)
        let suite = "com.riwork.tests.orchestrator.\(UUID().uuidString)"
        defaultsNames.append(suite)
        let model = RemoteModel(client: transport, keychain: try store(selected: session), defaults: UserDefaults(suiteName: suite)!, chatWaitMilliseconds: 300, chatIdleInterval: .milliseconds(20))
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
    /// The shells the phone asked the screen of.
    private func readShells(_ rig: Rig) async -> Set<String> {
        Set(await rig.transport.params(of: "shell.output").compactMap { $0["shell_id"]?.string })
    }

    // MARK: A chat orchestrator

    func testOrchestratorCatalogueUsesItsChatIdAndIsAvailableForNewChats() async throws {
        let rig = try await connected(orchestrators: [chatEntry()])
        let option = ChatModelOption(id: "provider-option", name: "Provider display name", efforts: ["medium"], supportsFast: true)
        await rig.transport.append(chatID, [.info(listedChat()), .models([option])])
        let catalogue = try await rig.model.availableChatModels(provider: .claude)
        XCTAssertEqual(catalogue, [option])
        let reads = await rig.transport.params(of: "chat.events")
        XCTAssertEqual(reads.map { $0["chat_id"] }, [.string(chatID)])
        XCTAssertFalse(reads.contains { $0["chat_id"] == .string(entryID) })
        let info = try XCTUnwrap(rig.model.tabs.first?.chatInfo)
        rig.model.conversation(chatID).modelCatalogue = catalogue
        let failure = await rig.model.chooseChatModel(info, .model(option.id))
        XCTAssertNil(failure)
        let commands = await rig.transport.params(of: "chat.command")
        XCTAssertEqual(commands.first?["chat_id"], .string(chatID))
        XCTAssertEqual(commands.first?["command"], .object(["command": .string("configure"), "model": .string(option.id)]))
        await rig.model.disconnect()
    }

    func testAChatOrchestratorIsAChatTabAndNotATerminalTab() async throws {
        let rig = try await connected(orchestrators: [chatEntry()])
        let model = rig.model
        XCTAssertEqual(model.openSessions.map(\.id), [shell], "the chat is not a terminal")
        XCTAssertEqual(model.tabs.map(\.id), [chatID, shell], "the orchestrator first, as ever, then the shell")
        guard case .orchestratorChat(let session, let info) = model.tabs[0] else { return XCTFail("a chat tab: \(model.tabs[0])") }
        XCTAssertEqual(session.id, entryID)
        XCTAssertEqual(info.id, chatID)
        XCTAssertEqual(ChatTabs.title(info), "Project orchestrator")
        XCTAssertEqual(info.provider, .claude)
        XCTAssertTrue(model.projectChats.isEmpty, "it is not one of the project's own chats")
        XCTAssertEqual(model.sessionID, shell, "the terminal that was selected stays selected")
        XCTAssertFalse(model.chatIsOnScreen)
        let asked = await readShells(rig)
        XCTAssertFalse(asked.contains(entryID), "no terminal is read for a chat")
        await model.disconnect()
    }
    func testOpeningItUsesTheChatIdForEveryChatRequest() async throws {
        // The desktop's chat list does not mention it at all: the entry is enough.
        let rig = try await connected(orchestrators: [chatEntry()])
        let model = rig.model
        await rig.transport.append(chatID, [.state(.idle), .itemStarted(ChatItem(id: "a", turnID: "t1", status: .completed, body: .agentMessage("hello from the orchestrator")))])
        model.chooseOrchestrator(model.tabs[0])
        XCTAssertEqual(model.selectedChatID, chatID, "the chat's id, not the entry's")
        XCTAssertEqual(model.selectedChat?.id, chatID)
        XCTAssertEqual(model.selectedChat?.title, "Project orchestrator")
        XCTAssertTrue(model.chatIsOnScreen)
        XCTAssertTrue(model.terminalCovered)
        XCTAssertEqual(model.orchestrator(ofChat: chatID)?.id, entryID)
        let follower = Task { await model.followChat(chatID) }
        tasks.append(follower)
        await eventually("the transcript is in") { model.conversation(self.chatID).feed.loaded && model.conversation(self.chatID).transcript.items.count == 1 }
        model.conversation(chatID).draft = "status?"
        let failure = await model.sendChatDraft(chatID)
        XCTAssertNil(failure)
        let events = await rig.transport.params(of: "chat.events")
        XCTAssertFalse(events.isEmpty)
        XCTAssertEqual(Set(events.compactMap { $0["chat_id"]?.string }), [chatID], "chat.events asks for the chat")
        let commands = await rig.transport.params(of: "chat.command")
        XCTAssertEqual(commands.compactMap { $0["chat_id"]?.string }, [chatID], "chat.command too")
        let asked = await readShells(rig)
        XCTAssertFalse(asked.contains(entryID) || asked.contains(chatID), "and no terminal is asked for")
        // A terminal tab takes the screen back.
        let terminal = try XCTUnwrap(model.openSessions.first)
        await model.chooseSession(terminal)
        XCTAssertNil(model.selectedChatID)
        XCTAssertFalse(model.terminalCovered)
        follower.cancel()
        await model.disconnect()
    }
    func testWhenTheChatListNamesTheChatToo() async throws {
        let rig = try await connected(orchestrators: [chatEntry()], chats: [listedChat(state: .waiting)])
        let model = rig.model
        XCTAssertEqual(model.tabs.map(\.id), [chatID, shell], "one tab for it, not two")
        XCTAssertTrue(model.projectChats.isEmpty)
        let info = try XCTUnwrap(model.tabs[0].chatInfo)
        XCTAssertEqual(ChatTabs.title(info), "Project orchestrator", "it is called by its label")
        XCTAssertEqual(model.chatActivity(info), .waiting, "the desktop's word about the chat")
        model.selectChat(chatID)
        XCTAssertEqual(model.selectedChat?.title, "Project orchestrator")
        await model.disconnect()
    }
    func testTheTabShowsWhatTheEntryIsDoingUntilTheChatSaysSo() async throws {
        let rig = try await connected(orchestrators: [chatEntry("\"activity\":\"working\"")])
        let model = rig.model
        let info = try XCTUnwrap(model.tabs[0].chatInfo)
        XCTAssertEqual(model.chatActivity(info), .working, "the activity indicator of a terminal's tab")
        // Followed, the chat's own word is the fresher one.
        await rig.transport.append(chatID, [.state(.waiting)])
        model.selectChat(chatID)
        let follower = Task { await model.followChat(chatID) }
        tasks.append(follower)
        await eventually("the chat's own word wins") { model.chatActivity(info) == .waiting }
        follower.cancel()
        await model.disconnect()
    }
    func testAChatOrchestratorAtRestIsStillATab() async throws {
        let rig = try await connected(orchestrators: [chatEntry(alive: false)])
        XCTAssertEqual(rig.model.tabs.map(\.id), [chatID, shell], "a stopped chat is started again by the next message")
        await rig.model.disconnect()
    }
    func testTheChatLeavesTheScreenWhenTheDesktopNoLongerRunsTheOrchestratorAsOne() async throws {
        let rig = try await connected(orchestrators: [chatEntry()])
        let model = rig.model
        model.selectChat(chatID)
        XCTAssertTrue(model.chatIsOnScreen)
        // Switched back to a terminal on the Mac.
        await rig.transport.setOrchestrators([entry()])
        await model.refreshSessionsQuietly()
        XCTAssertNil(model.selectedChatID, "the terminals come back")
        XCTAssertEqual(model.openSessions.map(\.id), [entryID, shell], "and it is a terminal tab again")
        // Chosen as a chat again, then removed altogether.
        await rig.transport.setOrchestrators([chatEntry()])
        await model.refreshSessionsQuietly()
        model.selectChat(chatID)
        XCTAssertTrue(model.chatIsOnScreen)
        await rig.transport.setOrchestrators([])
        await model.refreshSessionsQuietly()
        XCTAssertNil(model.selectedChatID)
        XCTAssertEqual(model.tabs.map(\.id), [shell])
        await model.disconnect()
    }
    func testATerminalSavedAsTheSelectionThatIsNowAChatIsNotRead() async throws {
        // The person last had this orchestrator open as a terminal; the Mac has since made it a chat.
        let rig = try await connected(orchestrators: [chatEntry()], selected: entryID)
        XCTAssertEqual(rig.model.sessionID, shell, "the selection falls back to a terminal that is one")
        let asked = await readShells(rig)
        XCTAssertFalse(asked.contains(entryID), "no terminal is read for a chat")
        await rig.model.disconnect()
    }

    // MARK: A terminal orchestrator

    func testATerminalOrchestratorBehavesAsItAlwaysDid() async throws {
        for extra in ["", "\"mode\":\"terminal\"", "\"mode\":\"somethingnew\",\"chat_id\":\"\(chatID)\""] {
            let rig = try await connected(orchestrators: [entry(extra)])
            let model = rig.model
            XCTAssertEqual(model.openSessions.map(\.id), [entryID, shell], extra)
            XCTAssertEqual(model.tabs.map(\.id), [entryID, shell], extra)
            let tab = try XCTUnwrap(model.openSessions.first)
            await model.chooseSession(tab)
            XCTAssertEqual(model.sessionID, entryID, extra)
            XCTAssertNil(model.selectedChatID)
            XCTAssertFalse(model.terminalCovered)
            let asked = await readShells(rig)
            XCTAssertTrue(asked.contains(entryID), "its screen is read: \(extra)")
            let chatCalls = await rig.transport.count("chat.events") + rig.transport.count("chat.command")
            XCTAssertEqual(chatCalls, 0, extra)
            await model.disconnect()
        }
    }

    // MARK: A desktop without chats

    func testAChatOrchestratorOnADesktopWithoutChatsSaysToUpdateTheMacAndTriesNothing() async throws {
        let rig = try await connected(orchestrators: [chatEntry()], feature: false)
        let model = rig.model
        XCTAssertEqual(model.chatSupport, .unsupported)
        XCTAssertEqual(model.openSessions.map(\.id), [shell], "not a terminal")
        guard case .unavailable(let session, let opening) = model.tabs[0] else { return XCTFail("a tab that says why: \(model.tabs[0])") }
        XCTAssertEqual(session.id, entryID)
        XCTAssertEqual(opening, .needsUpdate)
        XCTAssertNil(model.selectedBlocked)
        model.chooseOrchestrator(model.tabs[0])
        XCTAssertEqual(model.selectedBlockedID, entryID)
        let blocked = try XCTUnwrap(model.selectedBlocked)
        XCTAssertEqual(blocked.opening.notice?.headline, "Update the Mac to open this orchestrator")
        XCTAssertEqual(blocked.session.title, "Project orchestrator")
        XCTAssertTrue(model.terminalCovered, "the terminal is released while the notice is up")
        XCTAssertNil(model.selectedChat)
        XCTAssertFalse(model.chatIsOnScreen)
        // Nothing was asked of the desktop for it: not a chat, not a terminal.
        let asked = await readShells(rig)
        XCTAssertFalse(asked.contains(entryID) || asked.contains(chatID))
        let chatCalls = await rig.transport.count("chats.list") + rig.transport.count("chat.events") + rig.transport.count("chat.command")
        XCTAssertEqual(chatCalls, 0)
        // A terminal tab brings the terminal back.
        await model.chooseSession(try XCTUnwrap(model.openSessions.first))
        XCTAssertNil(model.selectedBlockedID)
        XCTAssertFalse(model.terminalCovered)
        await model.disconnect()
    }
    func testThePhoneCannotSelectAChatOnADesktopWithoutChats() async throws {
        let rig = try await connected(orchestrators: [chatEntry()], feature: false)
        rig.model.selectChat(chatID)
        XCTAssertNil(rig.model.selectedChatID)
        XCTAssertFalse(rig.model.terminalCovered)
        await rig.model.disconnect()
    }
    func testAChatOrchestratorWithoutAChatIdIsNotATerminalEither() async throws {
        let rig = try await connected(orchestrators: [entry("\"mode\":\"chat\",\"provider\":\"claude\"")])
        let model = rig.model
        XCTAssertEqual(model.openSessions.map(\.id), [shell])
        guard case .unavailable(_, let opening) = model.tabs[0] else { return XCTFail("a tab that says why") }
        XCTAssertEqual(opening, .notReady)
        model.chooseOrchestrator(model.tabs[0])
        XCTAssertNotNil(model.selectedBlocked)
        let asked = await readShells(rig)
        XCTAssertFalse(asked.contains(entryID))
        await model.disconnect()
    }
    func testTheNoticeGoesWhenTheMacCanOpenItAfterAReconnect() async throws {
        let rig = try await connected(orchestrators: [chatEntry()], feature: false)
        let model = rig.model
        model.chooseOrchestrator(model.tabs[0])
        XCTAssertNotNil(model.selectedBlocked)
        // The Mac was updated; the next connection reads its features again.
        await rig.transport.setFeature(true)
        await model.disconnect()
        await model.connect()
        XCTAssertEqual(model.chatSupport, .supported)
        guard case .orchestratorChat = model.tabs[0] else { return XCTFail("it is a chat now: \(model.tabs[0])") }
        XCTAssertNil(model.selectedBlocked)
        XCTAssertFalse(model.terminalCovered)
        await model.disconnect()
    }
}
