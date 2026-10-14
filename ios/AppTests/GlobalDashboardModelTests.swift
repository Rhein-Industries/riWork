import XCTest
import RiWorkCore
@testable import RiWorkRemote

/// The dashboard and the Global screen against a scripted desktop: the global orchestrator (a terminal or a chat) is in no project's strip,
/// counts or selection under the earlier strip and the shared one; opening or starting it from a cold dashboard changes no project and
/// its selection is restored apart from every project's, across reconnects and project switches; reading the dashboard creates nothing and
/// happens only while asked; an older desktop is never asked and still lists and opens its projects.
@MainActor final class GlobalDashboardModelTests: XCTestCase {
    private let project = ChatTransport.project
    private let shell = ChatTransport.shell
    private let other = "22222222-2222-4222-8222-222222222222"
    private let globalTerminal = "aaaaaaaa-0001-4000-8000-000000000001"
    private let globalEntry = "aaaaaaaa-0002-4000-8000-000000000002"
    private let globalChat = "cccccccc-0002-4000-8000-000000000002"
    private let projectOrchestrator = "aaaaaaaa-0003-4000-8000-000000000003"
    private let projectChat = "cccccccc-0004-4000-8000-000000000004"
    private var defaultsNames: [String] = []
    private var tasks: [Task<Void, Never>] = []
    /// Methods that make or change something on the desktop: none of them may come from looking.
    private static let creating: Set<String> = ["orchestrator.create", "chat.create", "shell.create", "shell.close", "project.create", "tabs.open", "tabs.update",
                                               "chat.command", "chat.stop", "shell.input", "shell.keys", "shell.paste"]

    override func setUp() async throws { executionTimeAllowance = 60 }
    override func tearDown() async throws {
        for task in tasks { task.cancel() }
        tasks = []
        for name in defaultsNames { UserDefaults().removePersistentDomain(forName: name) }
        defaultsNames = []
    }

    private func orchestrator(_ id: String, project scope: String?, chat: String? = nil, alive: Bool = true, at: Int = 3, activity: String = "working") -> String {
        let mode = chat.map { ",\"mode\":\"chat\",\"chat_id\":\"\($0)\",\"provider\":\"claude\"" } ?? ""
        return "{\"id\":\"\(id)\",\"project_id\":\(scope.map { "\"\($0)\"" } ?? "null"),\"worktree_id\":null,\"kind\":\"orchestrator\",\"cwd\":\"/fixture\",\"harness\":null,\"alive\":\(alive),\"created_at_unix\":\(at),\"activity\":\"\(activity)\"\(mode)}"
    }
    private var globalTerminalEntry: String { orchestrator(globalTerminal, project: nil, at: 9) }
    private var globalChatEntry: String { orchestrator(globalEntry, project: nil, chat: globalChat, at: 8) }
    private var projectEntry: String { orchestrator(projectOrchestrator, project: project, at: 2) }
    private func store(tabKey: String? = nil) throws -> KeychainStore {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = shell
        desktop.projectSessionIDs = [other: "55555555-5555-4555-8555-555555555555"]
        if let tabKey { desktop.projectTabKeys = [project: tabKey] }
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        return keychain
    }
    private struct Rig { let model: RemoteModel; let transport: ChatTransport; let keychain: KeychainStore; let suite: String }
    private func connected(orchestrators: [String], chats: [ChatInfo] = [], chatFeature: Bool = true, create: Bool = true, dashboard: Bool = false,
                           shared: [ChatTransport.Tab]? = nil, tabKey: String? = nil, extraProjects: [String] = []) async throws -> Rig {
        let transport = ChatTransport(chats: chats)
        await transport.setFeature(chatFeature)
        await transport.setOrchestratorFeature(create)
        await transport.setOrchestrators(orchestrators)
        await transport.setDashboard(feature: dashboard, reply: dashboard ? try dashboardReply() : nil)
        await transport.setExtraProjects(extraProjects)
        if let shared { await transport.setSharedTabs(shared) }
        let suite = "com.riwork.tests.global.\(UUID().uuidString)"
        defaultsNames.append(suite)
        let keychain = try store(tabKey: tabKey)
        let model = RemoteModel(client: transport, keychain: keychain, defaults: UserDefaults(suiteName: suite)!, activityRefreshInterval: .milliseconds(40),
                                dashboardRefreshInterval: .milliseconds(60), chatWaitMilliseconds: 300, chatIdleInterval: .milliseconds(20))
        await model.connect()
        XCTAssertEqual(model.state, .connected)
        return Rig(model: model, transport: transport, keychain: keychain, suite: suite)
    }
    private func dashboardReply() throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: Data("""
        {"v":1,"generated_at":1800000000,"chats_known":true,
         "totals":{"projects":1,"active_projects":1,"live_shells":2,"live_chats":1,"agents":{"working":1,"waiting":0,"done":0,"unknown":0,"exited":0,"subagents":0},"tasks":{"todo":1,"in_progress":0,"done":0},"attention":0},
         "global":{"kind":"chat","chat_id":"\(globalChat)","provider":"claude","chat_state":"running"},
         "attention":[],"attention_total":0,"working":[],"working_total":0,"finished":[],"finished_total":0,
         "projects":[{"id":"\(project)","name":"Fixture","agents":{"working":1,"waiting":0,"done":0,"unknown":0,"exited":0,"subagents":0},"live_shells":1,"live_chats":1,"tasks":{"todo":1,"in_progress":0,"done":0},"worktrees":1,"attention":0}],
         "projects_total":1,"tasks_in_progress":[],"tasks_in_progress_total":0,"automations":{"state":"not_read"}}
        """.utf8))
    }
    private func eventually(_ what: String, timeout: Double = 5, file: StaticString = #filePath, line: UInt = #line, _ condition: () async -> Bool) async {
        let end = Date().addingTimeInterval(timeout)
        while await !condition(), Date() < end { try? await Task.sleep(for: .milliseconds(5)) }
        let met = await condition()
        XCTAssertTrue(met, what, file: file, line: line)
    }
    private func shellsRead(_ rig: Rig) async -> [String] { await rig.transport.params(of: "shell.output").compactMap { $0["shell_id"]?.string } }
    /// Everything a project's selection and restore are made of.
    private struct ProjectState: Equatable { let project: String?, session: String?, sessions: [String: String]?, tabs: [String: String]? }
    private func projectState(_ model: RemoteModel) -> ProjectState {
        ProjectState(project: model.desktop?.selectedProjectID, session: model.desktop?.selectedSessionID, sessions: model.desktop?.projectSessionIDs, tabs: model.desktop?.projectTabKeys)
    }
    private func noCreation(_ rig: Rig, file: StaticString = #filePath, line: UInt = #line) async {
        let made = await rig.transport.calls.filter { Self.creating.contains($0.method) }.map(\.method)
        XCTAssertEqual(made, [], "nothing was made or changed", file: file, line: line)
    }

    // MARK: No project's strip, counts or chats

    func testAGlobalTerminalAndAGlobalChatAreInNoProjectStripUnderTheEarlierStrip() async throws {
        let projectChatInfo = ChatInfo(id: projectChat, provider: .codex, projectID: project, cwd: "/fixture", title: "Mine", createdAtUnix: 10, state: .idle)
        // The chat list names the global orchestrator's chat too (it has no project): still no project's.
        let globalChatInfo = ChatInfo(id: globalChat, provider: .claude, projectID: nil, cwd: "/", title: "Global orchestrator", createdAtUnix: 11, state: .running)
        let rig = try await connected(orchestrators: [globalTerminalEntry, globalChatEntry, projectEntry], chats: [projectChatInfo, globalChatInfo])
        let model = rig.model
        XCTAssertEqual(model.tabs.map(\.id), [projectOrchestrator, shell, projectChat])
        XCTAssertEqual(model.openSessions.map(\.id), [projectOrchestrator, shell])
        XCTAssertEqual(model.projectChats.map(\.id), [projectChat])
        XCTAssertEqual(model.sessions.map(\.id), [projectOrchestrator, shell])
        let global: Set<String> = [globalTerminal, globalEntry, globalChat]
        XCTAssertTrue(Set(model.tabs.map(\.id) + model.tabs.compactMap { $0.session?.id }).isDisjoint(with: global))
        // Choosing it as a project's tab is refused: it is no tab of the project.
        let terminal = try XCTUnwrap(model.globalOrchestrators.first { $0.id == globalTerminal })
        await model.chooseSession(terminal)
        XCTAssertEqual(model.sessionID, shell)
        model.selectChat(globalChat)
        XCTAssertNil(model.selectedChatID)
        await model.disconnect()
    }

    func testAGlobalTerminalAndAGlobalChatAreInNoProjectStripOrCountUnderSharedTabs() async throws {
        let tabs = [ChatTransport.Tab(key: "shell:\(projectOrchestrator)", kind: "shell", title: "Project orchestrator"), ChatTransport.Tab(key: "shell:\(shell)", kind: "shell", title: "zsh"),
                    // A shared list that names the global ones, shown and hidden (a desktop should not): still none of the project's.
                    ChatTransport.Tab(key: "shell:\(globalTerminal)", kind: "shell", title: "G"), ChatTransport.Tab(key: "chat:\(globalChat)", kind: "chat", title: "G", hidden: true)]
        let rig = try await connected(orchestrators: [globalTerminalEntry, globalChatEntry, projectEntry], shared: tabs)
        let model = rig.model
        XCTAssertNotNil(model.sharedTabs)
        XCTAssertEqual(model.tabs.map(\.id), [projectOrchestrator, shell])
        XCTAssertEqual(model.openSessions.map(\.id), [projectOrchestrator, shell])
        XCTAssertEqual(OrchestratorScope.projectOpenable(model.sharedTabs, orchestrators: model.orchestrators).count, 0, "the hidden-tab count has no global entry")
        await model.disconnect()
    }

    // MARK: Opening it from a cold dashboard

    func testOpeningTheGlobalChatChangesNoProjectAndComesBackToTheProjectAsItWas() async throws {
        let projectChatInfo = ChatInfo(id: projectChat, provider: .codex, projectID: project, cwd: "/fixture", title: "Mine", createdAtUnix: 10, state: .idle)
        let rig = try await connected(orchestrators: [globalChatEntry, projectEntry], chats: [projectChatInfo])
        let model = rig.model
        model.selectChat(projectChat)
        let before = projectState(model)
        await rig.transport.append(globalChat, [.state(.running)])
        // The dashboard's Open: the Global screen comes up.
        model.enterGlobal()
        XCTAssertEqual(model.scope, .global)
        XCTAssertEqual(model.selectedChatID, globalChat, "opened by its chat id")
        XCTAssertEqual(model.tabs.map(\.id), [globalChat])
        XCTAssertEqual(model.desktop?.globalTabKey, "chat:\(globalChat)")
        XCTAssertEqual(projectState(model), before, "no project selected, switched or adopted, and no project restore changed")
        let follower = Task { await model.followChat(globalChat) }
        tasks.append(follower)
        await eventually("its events are read by the chat id") { await rig.transport.params(of: "chat.events").contains { $0["chat_id"] == .string(self.globalChat) } }
        follower.cancel()
        // Back to the dashboard, then into the project: its chat on screen, as it was.
        model.leaveGlobal()
        XCTAssertEqual(model.scope, .project)
        XCTAssertEqual(model.selectedChatID, projectChat)
        XCTAssertEqual(model.sessionID, shell)
        XCTAssertEqual(projectState(model), before)
        await noCreation(rig)
        await model.disconnect()
    }

    func testOpeningTheGlobalTerminalReadsItAloneAndLeavesTheProjectTerminalSelected() async throws {
        let rig = try await connected(orchestrators: [globalTerminalEntry, projectEntry], tabKey: "shell:\(shell)")
        let model = rig.model
        let before = projectState(model)
        model.enterGlobal()
        XCTAssertEqual(model.sessionID, globalTerminal, "the terminal pipeline follows the global terminal")
        XCTAssertEqual(model.openSessions.map(\.id), [globalTerminal])
        XCTAssertNil(model.selectedChatID)
        model.setTerminalVisible(true)
        await model.readOutput()
        let read = await shellsRead(rig)
        XCTAssertEqual(read.last, globalTerminal)
        XCTAssertEqual(projectState(model), before)
        model.setTerminalVisible(false)
        model.leaveGlobal()
        XCTAssertEqual(model.sessionID, shell)
        await model.readOutput()
        let after = await shellsRead(rig)
        XCTAssertEqual(after.last, shell)
        XCTAssertEqual(projectState(model), before)
        await noCreation(rig)
        await model.disconnect()
    }

    func testTheGlobalSelectionIsRestoredApartAcrossReconnectsAndProjectSwitches() async throws {
        let secondGlobal = "aaaaaaaa-0009-4000-8000-000000000009"
        let rig = try await connected(orchestrators: [globalTerminalEntry, orchestrator(secondGlobal, project: nil, at: 1), projectEntry], extraProjects: [other])
        let model = rig.model
        model.enterGlobal()
        XCTAssertEqual(model.sessionID, globalTerminal, "the newest first")
        let older = try XCTUnwrap(model.openSessions.first { $0.id == secondGlobal })
        await model.chooseSession(older)
        XCTAssertEqual(model.desktop?.globalTabKey, "shell:\(secondGlobal)")
        XCTAssertEqual(model.desktop?.selectedSessionID, shell, "a global choice is never the project's")
        XCTAssertNil(model.desktop?.projectSessionIDs?[project])
        model.leaveGlobal()
        // Another project, then back: the global choice is untouched.
        await model.chooseProject(other)
        XCTAssertEqual(model.projectID, other)
        XCTAssertEqual(model.desktop?.globalTabKey, "shell:\(secondGlobal)")
        await model.chooseProject(project)
        XCTAssertEqual(model.sessionID, shell, "the project's own selection came back")
        // A reconnect (and a relaunch: a new model on the same library) restores it.
        await model.disconnect()
        await model.connect()
        model.enterGlobal()
        XCTAssertEqual(model.sessionID, secondGlobal)
        model.leaveGlobal()
        await model.disconnect()
        let relaunched = RemoteModel(client: rig.transport, keychain: rig.keychain, defaults: UserDefaults(suiteName: rig.suite)!, chatWaitMilliseconds: 300, chatIdleInterval: .milliseconds(20))
        await relaunched.connect()
        XCTAssertEqual(relaunched.sessionID, shell)
        relaunched.enterGlobal()
        XCTAssertEqual(relaunched.sessionID, secondGlobal)
        // Gone from the desktop: the Global screen falls back to the one that is there, and the project is still not touched.
        await rig.transport.setOrchestrators([globalTerminalEntry, projectEntry])
        await relaunched.refreshGlobalQuietly()
        XCTAssertEqual(relaunched.sessionID, globalTerminal)
        relaunched.leaveGlobal()
        XCTAssertEqual(relaunched.sessionID, shell)
        await relaunched.disconnect()
    }

    func testStartingTheGlobalOrchestratorFromAColdDashboardIsProjectlessAndOpensOnTheGlobalScreen() async throws {
        let rig = try await connected(orchestrators: [projectEntry], dashboard: true)
        let model = rig.model
        let before = projectState(model)
        let status = GlobalOrchestratorStatus.decide(entry: model.globalOrchestrators.first, dashboard: .none(exitedTerminal: false), canStart: model.orchestratorsOffered)
        XCTAssertEqual(status.action, .start)
        let requests = model.globalScreenRequests
        let failure = await model.startGlobalOrchestrator()
        XCTAssertNil(failure)
        let sent = await rig.transport.params(of: "orchestrator.create")
        XCTAssertEqual(sent, [[:]], "no project in the request")
        XCTAssertEqual(model.globalScreenRequests, requests + 1, "the Global screen is put up; Back from it is the dashboard")
        XCTAssertEqual(projectState(model), before, "no project selected, switched or adopted")
        XCTAssertFalse(model.tabs.contains { $0.session?.project_id == nil })
        let made = try XCTUnwrap(model.globalOrchestrators.first)
        model.enterGlobal()
        XCTAssertEqual(model.selectedChatID, made.chat_id)
        model.leaveGlobal()
        XCTAssertEqual(projectState(model), before)
        let unhides = await rig.transport.count("tabs.open") + rig.transport.count("tabs.update")
        XCTAssertEqual(unhides, 0)
        await model.disconnect()
    }

    // MARK: Notices that name no project

    func testANeedsUpdateGlobalChatSaysSoOnTheGlobalScreenOnly() async throws {
        let rig = try await connected(orchestrators: [globalChatEntry, projectEntry], chatFeature: false)
        let model = rig.model
        XCTAssertFalse(model.tabs.contains { if case .unavailable = $0 { true } else { false } }, "no notice tab in the project")
        model.enterGlobal()
        let blocked = try XCTUnwrap(model.selectedBlocked)
        XCTAssertEqual(blocked.session.id, globalEntry)
        XCTAssertEqual(blocked.opening, .needsUpdate)
        XCTAssertNil(model.sessionID, "a chat is never tried as a terminal")
        let words = (blocked.opening.globalNotice?.headline ?? "") + (blocked.opening.globalNotice?.detail ?? "")
        XCTAssertFalse(words.lowercased().contains("project"))
        model.leaveGlobal()
        XCTAssertNil(model.selectedBlocked)
        XCTAssertEqual(model.sessionID, shell)
        await model.disconnect()
    }

    // MARK: Reading the dashboard

    func testReadingTheDashboardCreatesNothingAndOnlyWhileItIsOnScreen() async throws {
        let rig = try await connected(orchestrators: [globalChatEntry, projectEntry], dashboard: true)
        let model = rig.model
        XCTAssertEqual(model.dashboardSupport, .supported)
        let watcher = Task { await model.keepFresh(.dashboard) }
        await eventually("read a few times") { await rig.transport.count("dashboard.get") >= 3 }
        watcher.cancel()
        _ = await watcher.value
        let snapshot = try XCTUnwrap(model.dashboard)
        XCTAssertEqual(snapshot.global, .chat(chatID: globalChat, provider: .claude, state: .running))
        XCTAssertEqual(snapshot.projects.rows.map(\.id), [project])
        XCTAssertEqual(model.dashboardFreshness().text, "Live")
        let params = await rig.transport.params(of: "dashboard.get")
        XCTAssertTrue(params.allSatisfy(\.isEmpty), "asked with no parameters")
        await noCreation(rig)
        // Off screen: no more reads.
        let count = await rig.transport.count("dashboard.get")
        try await Task.sleep(for: .milliseconds(300))
        let later = await rig.transport.count("dashboard.get")
        XCTAssertEqual(later, count, "nothing is read for a dashboard nobody is looking at")
        XCTAssertEqual(projectState(model).project, project)
        await model.disconnect()
    }

    func testAFailedReadKeepsTheLastDashboardAndSaysHowOldItIs() async throws {
        let rig = try await connected(orchestrators: [], dashboard: true)
        let model = rig.model
        await model.readDashboard()
        XCTAssertNotNil(model.dashboard)
        await rig.transport.setDashboard(feature: true, error: .rpc(code: "cli_error", message: "riwork dashboard failed"))
        await model.readDashboard()
        XCTAssertNotNil(model.dashboard, "the last read stays")
        XCTAssertEqual(model.dashboardFailure, "riwork dashboard failed")
        let freshness = model.dashboardFreshness()
        XCTAssertTrue(freshness.stale)
        XCTAssertTrue(freshness.text.hasPrefix("Showing the read from"), freshness.text)
        await rig.transport.setDashboard(feature: true, error: .rpc(code: "response_too_large", message: "x"))
        await model.readDashboard()
        XCTAssertEqual(model.dashboardFailure, "The dashboard is too large to send over this link.")
        await model.disconnect()
        XCTAssertEqual(model.dashboardFreshness().stale, true, "disconnected: stale")
    }

    func testAnOlderDesktopIsNeverAskedSaysWhyAndStillListsAndOpensProjects() async throws {
        let rig = try await connected(orchestrators: [globalTerminalEntry, projectEntry], dashboard: false)
        let model = rig.model
        XCTAssertEqual(model.dashboardSupport, .unsupported)
        await model.refreshDashboardQuietly()
        let asked = await rig.transport.count("dashboard.get")
        XCTAssertEqual(asked, 0, "a desktop that does not announce it is never asked")
        XCTAssertNotNil(model.dashboardUnavailableReason)
        XCTAssertTrue(model.dashboardUnavailableReason?.contains("Update RiWork on the Mac") == true)
        XCTAssertEqual(model.projects.map(\.id), [project], "the projects are listed")
        // The global orchestrator's row still works from the orchestrator list.
        XCTAssertEqual(GlobalOrchestratorStatus.decide(entry: model.globalOrchestrators.first, dashboard: nil, canStart: model.orchestratorsOffered).action, .open)
        // And a project still opens.
        await model.chooseProject(project)
        XCTAssertEqual(model.loadedProjectID, project)
        XCTAssertEqual(model.tabs.map(\.id), [projectOrchestrator, shell])
        await model.disconnect()
    }

    func testADesktopThatRefusesTheMethodStopsBeingAskedAndSaysWhy() async throws {
        let rig = try await connected(orchestrators: [], dashboard: true)
        await rig.transport.setDashboard(feature: true, error: .rpc(code: "invalid_request", message: "unsupported RPC method"))
        await rig.model.readDashboard()
        await rig.model.readDashboard()
        let asked = await rig.transport.count("dashboard.get")
        XCTAssertEqual(asked, 1)
        XCTAssertEqual(rig.model.dashboardSupport, .unsupported)
        XCTAssertNotNil(rig.model.dashboardUnavailableReason)
        XCTAssertNil(rig.model.dashboardFailure)
        await rig.model.disconnect()
    }

    // MARK: Rows

    func testADashboardRowOpensItsProjectWithItsTab() async throws {
        let projectChatInfo = ChatInfo(id: projectChat, provider: .codex, projectID: project, cwd: "/fixture", title: "Mine", createdAtUnix: 10, state: .idle)
        let rig = try await connected(orchestrators: [globalChatEntry, projectEntry], chats: [projectChatInfo], dashboard: true, extraProjects: [other])
        let model = rig.model
        await model.readDashboard()
        XCTAssertEqual(model.dashboardDestination(.chat(id: globalChat, projectID: nil, orchestrator: nil)), .global, "the global chat by the orchestrator list")
        XCTAssertEqual(model.dashboardDestination(.shell(id: globalEntry)), .global)
        let destination = model.dashboardDestination(.chat(id: projectChat, projectID: project, orchestrator: nil))
        XCTAssertEqual(destination, .project(id: project, tab: "chat:\(projectChat)"))
        // From another project, the row's project and its chat come up.
        await model.chooseProject(other)
        model.requestProjectTab(project, key: "chat:\(projectChat)")
        await model.chooseProject(project)
        XCTAssertEqual(model.selectedChatID, projectChat)
        // Already the project on screen: only its tab changes.
        model.requestProjectTab(project, key: "shell:\(shell)")
        await model.chooseProject(project)
        await eventually("the shell is on screen") { model.selectedChatID == nil && model.sessionID == self.shell }
        await noCreation(rig)
        await model.disconnect()
    }
}
