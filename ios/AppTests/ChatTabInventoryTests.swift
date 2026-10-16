import XCTest
import RiWorkCore
@testable import RiWorkRemote

/// The shared strip remains authoritative while chat metadata arrives separately or changes during a read.
@MainActor final class ChatTabInventoryTests: XCTestCase {
    private let project = ChatTransport.project
    private let chatID = "cccccccc-1111-4111-8111-111111111111"
    private let otherID = "cccccccc-2222-4222-8222-222222222222"
    private var defaultsNames: [String] = []
    private var keychains: [KeychainStore] = []
    private var models: [RemoteModel] = []
    private var tasks: [Task<Void, Never>] = []

    override func setUp() async throws { executionTimeAllowance = 30 }
    override func tearDown() async throws {
        for task in tasks { task.cancel() }
        for model in models { await model.disconnect() }
        for keychain in keychains { try? keychain.delete() }
        for name in defaultsNames { UserDefaults().removePersistentDomain(forName: name) }
        tasks = []; models = []; keychains = []; defaultsNames = []
    }

    private func chat(_ id: String? = nil, provider: ChatProvider = .codex, title: String = "Codex chat") -> ChatInfo {
        ChatInfo(id: id ?? chatID, provider: provider, projectID: project, cwd: "/fixture", title: title, createdAtUnix: 10, state: .idle)
    }
    private func titledInfo(_ title: String, provider: ChatProvider = .codex) throws -> ChatInfo {
        let wire: JSONValue = .object(["id": .string(chatID), "provider": .string(provider.rawValue), "project_id": .string(project),
                                      "cwd": .string("/fixture"), "title": .string(provider.chatTitle), "provider_title": .string(title),
                                      "created_at_unix": .number(10), "state": .object(["state": .string("idle")])])
        return try wire.decode(ChatInfo.self)
    }
    private struct Rig { let model: RemoteModel; let transport: ChatTransport }
    private func makeRig(chats: [ChatInfo], shared: [ChatTransport.Tab]? = nil, failedChatLists: Int = 0, remembered: String? = nil,
                         chatFeature: Bool = true, orchestrators: [String] = []) async throws -> Rig {
        let keychain = KeychainStore(service: "com.riwork.tests.chat-inventory.\(UUID().uuidString)")
        keychains.append(keychain)
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = ChatTransport.shell
        if let remembered { desktop.projectTabKeys = [project: remembered] }
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        let suite = "com.riwork.tests.chat-inventory.\(UUID().uuidString)"
        defaultsNames.append(suite)
        let transport = ChatTransport(chats: chats)
        await transport.setFeature(chatFeature)
        await transport.setOrchestrators(orchestrators)
        if let shared { await transport.setSharedTabs(shared) }
        await transport.failChatList(failedChatLists)
        let model = RemoteModel(client: transport, keychain: keychain, defaults: UserDefaults(suiteName: suite)!,
                                chatWaitMilliseconds: 100, chatIdleInterval: .milliseconds(10))
        models.append(model)
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
    private func follow(_ rig: Rig) {
        let id = chatID
        let task = Task { await rig.model.followChat(id) }
        tasks.append(task)
    }

    func testAVisibleSharedChatSurvivesAnInitialMetadataReadFailure() async throws {
        let rig = try await makeRig(chats: [chat(provider: .claude)], shared: [
            .init(key: "chat:\(chatID)", kind: "chat", title: "Repair the build", status: "waiting"),
            .init(key: "chat:\(otherID)", kind: "chat", title: "Hidden worker", hidden: true, worker: true, parent: "chat:\(chatID)"),
            .init(key: "shell:\(ChatTransport.shell)", kind: "shell", title: "zsh")
        ], failedChatLists: 1, remembered: "chat:\(chatID)")
        XCTAssertTrue(rig.model.chats.isEmpty, "the failed metadata reply has no chats to install")
        XCTAssertEqual(rig.model.tabs.map(\.id), [chatID, ChatTransport.shell], "visible shared membership supplies the chat tab")
        XCTAssertEqual(rig.model.tabs.first?.chatInfo.map(ChatTabs.title), "Repair the build")
        XCTAssertEqual(rig.model.selectedChatID, chatID, "the remembered chat is still openable")
        XCTAssertFalse(rig.model.tabs.contains { $0.id == otherID }, "a placeholder must not reveal a hidden worker")

        await rig.model.refreshChatsQuietly()
        XCTAssertEqual(rig.model.selectedChat?.provider, .claude, "the later metadata supplies the provider")
        XCTAssertEqual(rig.model.selectedChat.map(ChatTabs.title), "Repair the build", "metadata keeps the shared tab title")
    }

    func testConversationInfoSuppliesMissingMetadataWithoutReplacingTheSharedTitle() async throws {
        let rig = try await makeRig(chats: [chat(provider: .claude)], shared: [
            .init(key: "chat:\(chatID)", kind: "chat", title: "My shared name", status: "idle")
        ], failedChatLists: 1)
        rig.model.selectChat(chatID)
        XCTAssertEqual(rig.model.selectedChatID, chatID)
        await rig.transport.append(chatID, [.info(try titledInfo("Provider conversation name", provider: .claude))])
        follow(rig)
        await eventually("chat info reaches the transcript") { rig.model.conversation(self.chatID).transcript.info?.provider == .claude }
        XCTAssertEqual(rig.model.selectedChat?.provider, .claude, "the strip uses metadata from the opened chat")
        XCTAssertEqual(rig.model.selectedChat.map(ChatTabs.title), "My shared name", "live provider metadata cannot rename a shared tab")
    }

    func testAProviderOnlyTitleChangeUpdatesTheLegacyTabImmediately() async throws {
        let rig = try await makeRig(chats: [chat()])
        rig.model.selectChat(chatID)
        await rig.transport.append(chatID, [.info(try titledInfo("Debug the parser"))])
        follow(rig)
        await eventually("provider info has arrived") { rig.model.conversation(self.chatID).feed.next >= 1 }
        XCTAssertEqual(rig.model.selectedChat.map(ChatTabs.title), "Debug the parser", "the provider and generic legacy title need not change")
    }

    func testAnOlderChatListDoesNotUndoALiveProviderTitleUpdate() async throws {
        let rig = try await makeRig(chats: [chat()])
        rig.model.selectChat(chatID)
        let reads = await rig.transport.count("chats.list")
        await rig.transport.hold(["chats.list"])
        let refresh = Task { await rig.model.refreshChatsQuietly() }
        tasks.append(refresh)
        await eventually("the old chat metadata is captured") { await rig.transport.count("chats.list") > reads }

        await rig.transport.append(chatID, [.info(try titledInfo("Live provider name"))])
        follow(rig)
        await eventually("the live title reaches the tab") { rig.model.selectedChat.map(ChatTabs.title) == "Live provider name" }
        await rig.transport.releaseHeld()
        await refresh.value

        XCTAssertNil(rig.model.sharedTabs, "the legacy strip needs the live metadata to retain its name")
        XCTAssertEqual(rig.model.selectedChat.map(ChatTabs.title), "Live provider name", "an older list cannot restore the generic title")
    }

    func testReconnectFinishesLoadingWhenLiveMetadataChangesDuringTheFirstRead() async throws {
        let rig = try await makeRig(chats: [chat()])
        rig.model.selectChat(chatID)
        await rig.transport.append(chatID, [.info(chat())])
        follow(rig)
        await eventually("the existing chat is being followed") { rig.model.conversation(self.chatID).feed.next >= 1 }
        let reads = await rig.transport.count("chats.list")
        await rig.transport.hold(["chats.list"])
        let reconnect = Task { await rig.model.connect() }
        tasks.append(reconnect)
        await eventually("the reconnect's first metadata read is captured") {
            await rig.transport.count("chats.list") > reads && rig.model.loadedProjectID == nil
        }

        let liveInfo = try titledInfo("Title received during reconnect")
        await rig.transport.append(chatID, [.info(liveInfo)])
        await eventually("the follower installs fresh title metadata") {
            rig.model.selectedChat.map(ChatTabs.title) == "Title received during reconnect"
        }
        await rig.transport.setChats([liveInfo])
        await rig.transport.releaseHeld()
        await reconnect.value

        XCTAssertEqual(rig.model.state, .connected)
        XCTAssertEqual(rig.model.loadedProjectID, project, "fresh metadata cannot leave the first inventory permanently unloaded")
        XCTAssertTrue(rig.model.capabilitiesKnown)
        XCTAssertTrue(rig.model.openSessions.contains { $0.id == ChatTransport.shell })
        XCTAssertEqual(rig.model.selectedChat.map(ChatTabs.title), "Title received during reconnect")
    }

    func testASharedRenameKeepsItsTitleAfterProviderMetadataChanges() async throws {
        let rig = try await makeRig(chats: [try titledInfo("Original provider name")], shared: [
            .init(key: "chat:\(chatID)", kind: "chat", title: "Original shared name", status: "idle")
        ])
        rig.model.selectChat(chatID)
        try await rig.model.renameTab("chat:\(chatID)", title: "My renamed tab")
        await rig.transport.append(chatID, [.info(try titledInfo("New provider name"))])
        follow(rig)
        await eventually("provider info has arrived") { rig.model.conversation(self.chatID).feed.next >= 1 }
        XCTAssertEqual(rig.model.selectedChat.map(ChatTabs.title), "My renamed tab")
    }

    func testChatAndShellKeysDoNotBorrowEachOthersMetadata() async throws {
        let sameID = ChatTransport.shell
        let rig = try await makeRig(chats: [chat(sameID, provider: .claude)], shared: [
            .init(key: "chat:\(sameID)", kind: "chat", title: "Chat name", status: "idle"),
            .init(key: "shell:\(sameID)", kind: "shell", title: "Shell name", status: "idle")
        ])
        XCTAssertEqual(rig.model.tabs.count, 2)
        guard rig.model.tabs.count == 2 else { return }
        XCTAssertEqual(rig.model.tabs[0].chatInfo?.id, sameID, "the chat key resolves chat metadata")
        XCTAssertEqual(rig.model.tabs[0].chatInfo?.provider, .claude)
        XCTAssertEqual(rig.model.tabs[0].chatInfo.map(ChatTabs.title), "Chat name")
        if case .terminal(let session) = rig.model.tabs[1] { XCTAssertEqual(session.title, "Shell name") }
        else { XCTFail("the shell key resolves a terminal") }
        XCTAssertNotEqual(rig.model.tabs[0].key, rig.model.tabs[1].key, "SwiftUI needs distinct identities for distinct tab keys")
    }

    func testASharedChatOrchestratorKeepsItsNoticeWhenChatsAreUnsupported() async throws {
        let entryID = "99999999-9999-4999-8999-999999999999"
        let rig = try await makeRig(chats: [chat(otherID)], shared: [
            .init(key: "chat:\(chatID)", kind: "chat", title: "Renamed orchestrator", status: "idle"),
            .init(key: "chat:\(otherID)", kind: "chat", title: "Ordinary unsupported chat", status: "idle"),
            .init(key: "shell:\(ChatTransport.shell)", kind: "shell", title: "zsh", status: "idle")
        ], chatFeature: false, orchestrators: ["""
        {"id":"\(entryID)","project_id":"\(project)","kind":"orchestrator","cwd":"/fixture","alive":true,"created_at_unix":5,"mode":"chat","chat_id":"\(chatID)","provider":"claude"}
        """])
        XCTAssertEqual(rig.model.chatSupport, .unsupported)
        XCTAssertEqual(rig.model.tabs.map(\.id), [entryID, ChatTransport.shell])
        let notice = try XCTUnwrap(rig.model.tabs.first)
        guard case .unavailable(let session, let opening) = notice else { return XCTFail("the orchestrator needs an update notice") }
        XCTAssertEqual(session.id, entryID, "blocked selection uses the orchestrator entry's identity")
        XCTAssertEqual(opening, .needsUpdate)
        XCTAssertEqual(notice.key, "shell:\(entryID)")
        XCTAssertEqual(notice.title, "Renamed orchestrator", "the shared chat key supplies the notice's displayed name")
        rig.model.chooseOrchestrator(notice)
        XCTAssertEqual(rig.model.selectedBlockedID, entryID)
        XCTAssertNil(rig.model.selectedChatID)
        XCTAssertFalse(rig.model.tabs.contains { $0.id == otherID }, "ordinary unsupported chats do not become placeholders")
        let chatReads = await rig.transport.count("chats.list")
        XCTAssertEqual(chatReads, 0)
    }

    func testAnOlderFullRefreshDoesNotRemoveANewSharedChat() async throws {
        let rig = try await makeRig(chats: [chat()], shared: [
            .init(key: "chat:\(chatID)", kind: "chat", title: "Original", status: "idle"),
            .init(key: "shell:\(ChatTransport.shell)", kind: "shell", title: "zsh", status: "idle")
        ])
        let chatReads = await rig.transport.count("chats.list"), tabReads = await rig.transport.count("tabs.list")
        await rig.transport.hold(["chats.list", "tabs.list", "shells.list"])
        let refresh = Task { await rig.model.refresh() }
        tasks.append(refresh)
        await eventually("the older project reads are captured") {
            let currentChatReads = await rig.transport.count("chats.list")
            let currentTabReads = await rig.transport.count("tabs.list")
            return currentChatReads > chatReads && currentTabReads > tabReads
        }
        await rig.transport.setChats([chat(), chat(otherID, provider: .claude, title: "Just created")])
        let madeID = otherID
        await rig.transport.peerChange { $0.append(.init(key: "chat:\(madeID)", kind: "chat", title: "Just created", status: "idle")) }
        await rig.model.refreshChatsQuietly()
        try await rig.model.listTabs()
        rig.model.selectChat(otherID)
        await rig.transport.releaseHeld()
        await refresh.value
        XCTAssertEqual(rig.model.selectedChatID, otherID)
        XCTAssertTrue(rig.model.chats.contains { $0.id == otherID }, "an old full refresh cannot erase newer chat metadata")
        XCTAssertEqual(rig.model.selectedChat.map(ChatTabs.title), "Just created")
    }

    func testARefreshKeepsTheSelectedChatWhenItsOrchestratorMetadataArrives() async throws {
        let rig = try await makeRig(chats: [chat(provider: .claude)])
        rig.model.selectChat(chatID)
        // Either listing can provide a chat. Their installation must not briefly remove it between replies.
        await rig.transport.setChats([])
        await rig.transport.setOrchestrators(["""
        {"id":"99999999-9999-4999-8999-999999999999","project_id":"\(project)","kind":"orchestrator","cwd":"/fixture","alive":true,"created_at_unix":5,"mode":"chat","chat_id":"\(chatID)","provider":"claude"}
        """])
        await rig.model.refreshSessionsQuietly()
        XCTAssertEqual(rig.model.selectedChatID, chatID, "the final inventory still contains the selected chat")
        XCTAssertEqual(rig.model.selectedChat.map(ChatTabs.title), "Project orchestrator")
        XCTAssertEqual(rig.model.orchestrator(ofChat: chatID)?.id, "99999999-9999-4999-8999-999999999999")
    }
}
