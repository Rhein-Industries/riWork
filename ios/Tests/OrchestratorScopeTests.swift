import XCTest
@testable import RiWorkCore

/// The global orchestrator belongs to no project: a global terminal and a global chat are in no project's strip, open terminals, chats,
/// hidden-tab count or picker, under the earlier strip and under the desktop's shared tabs alike, even when a list names them; a project's
/// orchestrator is a tab of its own project only; the Global screen has the global ones alone.
final class OrchestratorScopeTests: XCTestCase {
    private let alpha = "11111111-1111-4111-8111-111111111111"
    private let beta = "22222222-2222-4222-8222-222222222222"
    private let globalTerminal = "aaaaaaaa-0001-4000-8000-000000000001"
    private let globalChatEntry = "aaaaaaaa-0002-4000-8000-000000000002"
    private let globalChat = "cccccccc-0002-4000-8000-000000000002"
    private let alphaOrchestrator = "aaaaaaaa-0003-4000-8000-000000000003"
    private let betaOrchestrator = "aaaaaaaa-0004-4000-8000-000000000004"
    private let alphaShell = "44444444-0005-4000-8000-000000000005"
    private let alphaChat = "cccccccc-0006-4000-8000-000000000006"

    private func session(_ id: String, project: String?, kind: String = "orchestrator", alive: Bool = true, at: Int = 5, chat: String? = nil, provider: String = "claude") throws -> RemoteSession {
        let mode = chat.map { ",\"mode\":\"chat\",\"chat_id\":\"\($0)\",\"provider\":\"\(provider)\"" } ?? ""
        let json = "{\"id\":\"\(id)\",\"project_id\":\(project.map { "\"\($0)\"" } ?? "null"),\"worktree_id\":null,\"kind\":\"\(kind)\",\"cwd\":\"/a\",\"harness\":null,\"alive\":\(alive),\"created_at_unix\":\(at)\(mode)}"
        return try JSONDecoder().decode(RemoteSession.self, from: Data(json.utf8))
    }
    /// Every orchestrator the desktop lists: a global terminal, a global chat, and each project's own.
    private func orchestrators() throws -> [RemoteSession] {
        [try session(globalTerminal, project: nil, at: 9), try session(globalChatEntry, project: nil, at: 8, chat: globalChat),
         try session(alphaOrchestrator, project: alpha, at: 3), try session(betaOrchestrator, project: beta, at: 4)]
    }
    private func alphaShells() throws -> [RemoteSession] { [try session(alphaShell, project: alpha, kind: "project", at: 7)] }
    private func chats() -> [ChatInfo] {
        [ChatInfo(id: alphaChat, provider: .codex, projectID: alpha, cwd: "/a", title: "Ordinary chat", createdAtUnix: 10, state: .idle),
         // A chat list that names the global orchestrator's chat (a desktop should not): it is still no project's.
         ChatInfo(id: globalChat, provider: .claude, projectID: nil, cwd: "/a", title: "Global", createdAtUnix: 11, state: .running)]
    }
    private var globalIDs: Set<String> { [globalTerminal, globalChatEntry, globalChat] }
    private func ids(_ tabs: [ProjectTab]) -> [String] { tabs.map(\.id) }
    private func sessionIDs(_ tabs: [ProjectTab]) -> [String] { tabs.compactMap { $0.session?.id } }
    private func shared(_ entries: [(String, String, Bool)]) throws -> SharedTabsReply {
        let list = entries.enumerated().map { index, entry in
            "{\"key\":\"\(entry.0)\",\"kind\":\"\(entry.1)\",\"title\":\"T\(index)\",\"status\":\"working\",\"hidden\":\(entry.2),\"worker\":false,\"order\":\(index),\"parent\":null,\"children\":[],\"child_count\":0}"
        }.joined(separator: ",")
        return try JSONDecoder().decode(SharedTabsReply.self, from: Data("{\"epoch\":\"e\",\"revision\":1,\"entries\":[\(list)]}".utf8))
    }

    // MARK: The earlier strip

    func testTheGlobalOrchestratorsAreInNoProjectsStripAndAProjectsOwnIsOnlyInItsProject() throws {
        let all = try orchestrators()
        let alphaTabs = OrchestratorScope.projectTabs(orchestrators: all, shells: try alphaShells(), chats: chats(), shared: nil, project: alpha, chatsAvailable: true)
        XCTAssertEqual(ids(alphaTabs), [alphaOrchestrator, alphaShell, alphaChat], "its own orchestrator first, its shell, its own chat")
        XCTAssertTrue(Set(ids(alphaTabs) + sessionIDs(alphaTabs)).isDisjoint(with: globalIDs), "neither global terminal nor global chat")
        XCTAssertFalse(ids(alphaTabs).contains(betaOrchestrator), "another project's orchestrator is not one of its tabs")
        let betaTabs = OrchestratorScope.projectTabs(orchestrators: all, shells: [], chats: [], shared: nil, project: beta, chatsAvailable: true)
        XCTAssertEqual(ids(betaTabs), [betaOrchestrator])
        let none = OrchestratorScope.projectTabs(orchestrators: all, shells: [], chats: [], shared: nil, project: nil, chatsAvailable: true)
        XCTAssertTrue(none.isEmpty, "with no project chosen, nothing (the global orchestrator is not a stand-in project)")
        // Open terminals and chats of the project, the counts drawn from them.
        let open = OrchestratorScope.projectOpenSessions(orchestrators: all, shells: try alphaShells(), shared: nil, project: alpha)
        XCTAssertEqual(open.map(\.id), [alphaOrchestrator, alphaShell])
        XCTAssertEqual(OrchestratorScope.projectChats(chats(), orchestrators: all).map(\.id), [alphaChat])
        XCTAssertEqual(OrchestratorScope.projectEntries(orchestrators: all, shells: try alphaShells(), project: alpha).map(\.id), [alphaOrchestrator, alphaShell])
    }

    func testAGlobalOrchestratorOnADesktopWithoutChatsIsInNoStripEither() throws {
        let all = try orchestrators()
        let tabs = OrchestratorScope.projectTabs(orchestrators: all, shells: try alphaShells(), chats: [], shared: nil, project: alpha, chatsAvailable: false)
        XCTAssertTrue(Set(ids(tabs)).isDisjoint(with: globalIDs))
        XCTAssertFalse(tabs.contains { if case .unavailable(let session, _) = $0 { session.project_id == nil } else { false } }, "not even as a notice")
    }

    // MARK: The shared strip

    func testTheSharedStripNeverHasTheGlobalOrchestratorEvenWhenItsListNamesIt() throws {
        let all = try orchestrators()
        let list = try shared([("shell:\(alphaOrchestrator)", "shell", false), ("shell:\(alphaShell)", "shell", false), ("chat:\(alphaChat)", "chat", false),
                               ("shell:\(globalTerminal)", "shell", false), ("chat:\(globalChat)", "chat", false),
                               ("shell:\(globalChatEntry)", "shell", true), ("chat:\(globalChat)", "chat", true)])
        let tabs = OrchestratorScope.projectTabs(orchestrators: all, shells: try alphaShells(), chats: chats(), shared: list, project: alpha, chatsAvailable: true)
        XCTAssertEqual(ids(tabs), [alphaOrchestrator, alphaShell, alphaChat], "the shared list's order, without the global entries")
        XCTAssertEqual(tabs.map { $0.session?.sharedTitle ?? $0.chatInfo?.title ?? "" }, ["T0", "T1", "T2"], "with the shared titles")
        let open = OrchestratorScope.projectOpenSessions(orchestrators: all, shells: try alphaShells(), shared: list, project: alpha)
        XCTAssertEqual(open.map(\.id), [alphaOrchestrator, alphaShell], "no global terminal among the open terminals")
        let openable = OrchestratorScope.projectOpenable(list, orchestrators: all)
        XCTAssertTrue(openable.isEmpty, "hidden global entries are not offered or counted: \(openable.map(\.id))")
        // The plain shared helper would have counted them: the project's count goes through the scope.
        XCTAssertEqual(SharedTabStrip.openable(list).count, 2)
        // Nor are they among the tabs the Edit tabs sheet reorders.
        let groups = OrchestratorScope.projectGroups(list, orchestrators: all)
        XCTAssertEqual(groups.flatMap(\.tabs).map(\.key), ["shell:\(alphaOrchestrator)", "shell:\(alphaShell)", "chat:\(alphaChat)"])
        XCTAssertEqual(SharedTabStrip.groups(list).flatMap(\.tabs).count, 5, "the plain helper still has the two visible global ones")
    }

    // MARK: The Global screen

    func testTheGlobalScreenHasTheGlobalOrchestratorsAloneEachOpenedTheWayItRuns() throws {
        let all = try orchestrators()
        let tabs = OrchestratorScope.globalTabs(orchestrators: all, chatsAvailable: true)
        XCTAssertEqual(ids(tabs), [globalTerminal, globalChat], "newest first; the chat by its chat id")
        guard case .terminal(let terminal) = tabs[0], case .orchestratorChat(let entry, let info) = tabs[1] else { return XCTFail("\(tabs)") }
        XCTAssertEqual(terminal.id, globalTerminal)
        XCTAssertEqual(entry.id, globalChatEntry); XCTAssertEqual(info.id, globalChat)
        XCTAssertEqual(info.title, "Global orchestrator")
        XCTAssertEqual(OrchestratorScope.globalOpenSessions(orchestrators: all).map(\.id), [globalTerminal])
        XCTAssertTrue(tabs.allSatisfy { $0.session?.project_id == nil })
        // A desktop without chats: the chat one is a notice, in words of its own.
        let older = OrchestratorScope.globalTabs(orchestrators: all, chatsAvailable: false)
        guard case .unavailable(_, let opening) = older[1] else { return XCTFail("\(older)") }
        XCTAssertEqual(opening, .needsUpdate)
        for notice in [SessionOpening.needsUpdate.globalNotice, SessionOpening.notReady.globalNotice] {
            let words = "\(notice?.headline ?? "") \(notice?.detail ?? "")".lowercased()
            XCTAssertTrue(words.contains("global orchestrator"), words)
            XCTAssertFalse(words.contains("project") || words.contains("tabs"), "project-free: \(words)")
        }
        XCTAssertNil(SessionOpening.terminal.globalNotice)
    }

    func testTheGlobalSelectionIsRememberedByItsOwnKey() throws {
        let tabs = OrchestratorScope.globalTabs(orchestrators: try orchestrators(), chatsAvailable: true)
        XCTAssertEqual(tabs.map(OrchestratorScope.key), ["shell:\(globalTerminal)", "chat:\(globalChat)"])
        XCTAssertEqual(OrchestratorScope.restoredGlobalTab(remembered: "chat:\(globalChat)", in: tabs)?.id, globalChat, "the remembered one")
        XCTAssertEqual(OrchestratorScope.restoredGlobalTab(remembered: "chat:\(alphaChat)", in: tabs)?.id, globalTerminal, "gone: the first")
        XCTAssertEqual(OrchestratorScope.restoredGlobalTab(remembered: nil, in: tabs)?.id, globalTerminal)
        XCTAssertNil(OrchestratorScope.restoredGlobalTab(remembered: "shell:\(globalTerminal)", in: []))
        let blocked = OrchestratorScope.globalTabs(orchestrators: try orchestrators(), chatsAvailable: false)
        XCTAssertEqual(OrchestratorScope.key(blocked[1]), "orchestrator:\(globalChatEntry)", "a notice is never read as a terminal")
        XCTAssertEqual(OrchestratorScope.id(in: "shell:\(globalTerminal)", kind: "shell"), globalTerminal)
        XCTAssertNil(OrchestratorScope.id(in: "chat:\(globalChat)", kind: "shell"))
        XCTAssertNil(OrchestratorScope.id(in: "shell:", kind: "shell"))
        XCTAssertNil(OrchestratorScope.id(in: nil, kind: "chat"))
    }

    func testASavedLibraryFromBeforeTheGlobalSelectionStillReads() throws {
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = alpha
        var json = try JSONSerialization.jsonObject(with: JSONEncoder().encode(desktop)) as! [String: Any]
        json.removeValue(forKey: "globalTabKey")
        let old = try JSONDecoder().decode(SavedDesktop.self, from: JSONSerialization.data(withJSONObject: json))
        XCTAssertNil(old.globalTabKey)
        XCTAssertEqual(old.selectedProjectID, alpha)
        desktop.globalTabKey = "chat:\(globalChat)"
        let again = try JSONDecoder().decode(SavedDesktop.self, from: JSONEncoder().encode(desktop))
        XCTAssertEqual(again.globalTabKey, "chat:\(globalChat)")
        XCTAssertEqual(again.selectedProjectID, alpha, "kept apart from the project's selection")
    }
}
