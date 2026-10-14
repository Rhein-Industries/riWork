import XCTest
@testable import RiWorkCore

/// The desktop's dashboard on the phone (`dashboard.get`): read leniently (a figure that is missing or wrong is unknown, never 0; a row
/// that cannot be read is left out), kept in the desktop's order and attention precedence (checked on the fixture the engine itself
/// writes, ios/Tests/Fixtures/dashboard.json), how current it says it is, where each row leads, and what the global orchestrator's
/// button does.
final class GlobalDashboardTests: XCTestCase {
    private let alpha = "11111111-1111-4111-8111-111111111111"
    private let beta = "22222222-2222-4222-8222-222222222222"
    private let shellA = "44444444-0001-4000-8000-000000000001"
    private let chatA = "cccccccc-0001-4000-8000-000000000001"
    private let globalShell = "aaaaaaaa-0001-4000-8000-000000000001"
    private let globalChat = "cccccccc-0009-4000-8000-000000000009"

    private func json(_ text: String) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: Data(text.utf8)) }
    /// Written by the engine's own test from `global_overview`'s fixtures (`RIWORK_UPDATE_FIXTURES=1 cargo test -p riwork-engine global_overview`).
    private func fixture() throws -> JSONValue {
        #if SWIFT_PACKAGE
        let bundle = Bundle.module
        #else
        let bundle = Bundle(for: Self.self)
        #endif
        let url = try XCTUnwrap(bundle.url(forResource: "dashboard", withExtension: "json", subdirectory: "Fixtures") ?? bundle.url(forResource: "dashboard", withExtension: "json"))
        return try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: url))
    }

    // MARK: Lenient, and unknown is not zero

    func testAFigureThatIsMissingOrWrongIsUnknownNotZero() throws {
        let snapshot = try DashboardSnapshot(wire: try json("""
        {"v":1,"generated_at":1800000000,"chats_known":false,
         "totals":{"projects":2,"active_projects":"two","live_shells":-1,"live_chats":4,"agents":{"working":1.5,"waiting":2},"tasks":{"todo":0},"attention":0},
         "global":{"kind":"unknown"},
         "projects":[{"id":"\(alpha)","name":"Alpha","live_shells":0,"live_chats":3,"tasks":{"todo":0,"in_progress":0,"done":0},"agents":{"working":0}}],
         "projects_total":1}
        """))
        XCTAssertEqual(snapshot.totals.projects, 2)
        XCTAssertNil(snapshot.totals.activeProjects, "a word where a count belongs")
        XCTAssertNil(snapshot.totals.liveShells, "a negative count")
        XCTAssertNil(snapshot.totals.liveChats, "the chats were not read: unknown, whatever came")
        XCTAssertNil(snapshot.totals.liveSessions)
        XCTAssertNil(snapshot.totals.agents?.working, "a fraction")
        XCTAssertEqual(snapshot.totals.agents?.waiting, 2)
        XCTAssertNil(snapshot.totals.agents?.done, "absent")
        XCTAssertEqual(snapshot.totals.tasks?.todo, 0, "a real zero stays zero")
        XCTAssertNil(snapshot.totals.tasks?.inProgress)
        XCTAssertNil(snapshot.totals.tasks?.open, "to do + in progress is unknown when one of them is")
        XCTAssertEqual(snapshot.totals.attention, 0)
        XCTAssertEqual(snapshot.global, .unknown)
        XCTAssertEqual(snapshot.chatsKnown, false)
        let project = try XCTUnwrap(snapshot.projects.rows.first)
        XCTAssertEqual(project.liveShells, 0)
        XCTAssertNil(project.liveChats, "unknown, not the 3 a desktop should not have sent")
        XCTAssertNil(project.lastActivity, "no time recorded: unknown, not the epoch")
        XCTAssertNil(project.worktrees)
        XCTAssertNil(project.attention)
        XCTAssertEqual(DashboardText.count(project.attention), "—")
        // Lists that did not come are empty, not invented, and say no total.
        XCTAssertTrue(snapshot.attention.rows.isEmpty)
        XCTAssertNil(snapshot.attention.total)
        XCTAssertEqual(snapshot.automations, .notRead)
    }

    func testARowThatCannotBeReadIsLeftOutAndTheRestStands() throws {
        let snapshot = try DashboardSnapshot(wire: try json("""
        {"v":1,"chats_known":true,"totals":{},"global":{"kind":"chat","chat_id":"\(globalChat)","provider":"claude","chat_state":"running"},
         "attention":[
           {"kind":"waiting","target":{"type":"shell","id":"not-a-uuid"}},
           "a string",
           {"kind":"waiting","target":{"type":"shell","id":"\(shellA)"},"session":{"target":{"type":"shell","id":"\(shellA)"},"agent":"Claude","project_id":"\(alpha)","project_name":"Alpha","is_chat":false},"at":1799999000},
           {"kind":"later_kind","target":{"type":"project","id":"\(alpha)"}},
           {"kind":"waiting","target":{"type":"teleport","id":"\(alpha)"}}
         ],"attention_total":1,
         "working":[{"session":{"target":{"type":"chat","id":"\(chatA)","project_id":"\(alpha)"},"agent":"Codex","title":"  Fix it\\u0007  ","is_chat":true},"activity":"working","subagents":"x"}],
         "projects":[{"id":"\(alpha)"},{"name":"No id"},{"id":"\(beta)","name":"Beta","tasks":[1,2]}]}
        """))
        XCTAssertEqual(snapshot.attention.rows.count, 2, "the readable waiting row and the kind a newer desktop added")
        XCTAssertEqual(snapshot.attention.rows.first?.kind, .waiting)
        XCTAssertEqual(snapshot.attention.rows.first?.session?.place, "Alpha")
        XCTAssertEqual(snapshot.attention.rows.last?.kind, .other("later_kind"))
        XCTAssertEqual(snapshot.attention.total, 2, "a total under what arrived never hides a row")
        let work = try XCTUnwrap(snapshot.working.rows.first)
        XCTAssertEqual(work.session.name, "Fix it", "trimmed, control characters out")
        XCTAssertNil(work.subagents, "not a count: unknown")
        XCTAssertNil(work.sinceUnix)
        XCTAssertEqual(snapshot.projects.rows.map(\.id), [beta], "a project needs an id and a name")
        XCTAssertNil(snapshot.projects.rows.first?.tasks, "tasks that are not an object are unknown")
        XCTAssertEqual(snapshot.global, .chat(chatID: globalChat, provider: .claude, state: .running))
    }

    func testOnlyAWholeReplyThatIsNotVersionOneIsUnreadable() throws {
        XCTAssertThrowsError(try DashboardSnapshot(wire: .array([]))) { XCTAssertEqual($0 as? DashboardSnapshot.ReadError, .unreadable) }
        XCTAssertThrowsError(try DashboardSnapshot(wire: try json("{\"totals\":{}}"))) { XCTAssertEqual($0 as? DashboardSnapshot.ReadError, .unreadable) }
        XCTAssertThrowsError(try DashboardSnapshot(wire: try json("{\"v\":2}"))) { XCTAssertEqual($0 as? DashboardSnapshot.ReadError, .unsupportedVersion) }
        let bare = try DashboardSnapshot(wire: try json("{\"v\":1}"))
        XCTAssertNil(bare.generatedAt); XCTAssertNil(bare.chatsKnown); XCTAssertNil(bare.totals.projects)
        XCTAssertEqual(bare.global, .unknown); XCTAssertEqual(bare.automations, .notRead)
    }

    func testTheGlobalOrchestratorAndAutomationsAreReadAsTheDesktopSaysThem() throws {
        let terminal = DashboardGlobal(wire: try json("{\"kind\":\"terminal\",\"shell_id\":\"\(globalShell)\"}"))
        XCTAssertEqual(terminal, .terminal(shellID: globalShell, activity: nil), "activity not sampled: unknown")
        XCTAssertEqual(DashboardGlobal(wire: try json("{\"kind\":\"none\",\"exited_terminal\":true}")), .none(exitedTerminal: true))
        XCTAssertEqual(DashboardGlobal(wire: try json("{\"kind\":\"chat\",\"chat_id\":\"\(globalChat)\",\"chat_state\":\"failed\",\"message\":\"gone\"}")),
                       .chat(chatID: globalChat, provider: nil, state: .failed("gone")))
        XCTAssertEqual(DashboardGlobal(wire: try json("{\"kind\":\"terminal\",\"shell_id\":\"x\"}")), .unknown, "an id that is no UUID")
        XCTAssertEqual(DashboardAutomations(wire: try json("{\"state\":\"unreadable\",\"message\":\"schedules.json is damaged\"}")), .unreadable("schedules.json is damaged"))
        guard case .read(let total, let paused, let next, let items) = DashboardAutomations(wire: try json("""
        {"state":"read","total":3,"paused":1,"next":{"title":"Nightly","at":1800003600},
         "items":[{"id":"b","title":"Weekly","paused":true,"next_run":1800000001},{"id":"a","title":"Nightly","paused":false,"next_run":1800003600,"last_run":{"outcome":"missed","at":1799990000}}],"items_total":3}
        """)) else { return XCTFail("read") }
        XCTAssertEqual(total, 3); XCTAssertEqual(paused, 1); XCTAssertEqual(next?.title, "Nightly")
        XCTAssertEqual(items.rows.map(\.id), ["a", "b"], "the next to run first, paused last")
        XCTAssertNil(items.rows.last?.nextRun, "a paused automation has no next run")
        XCTAssertTrue(items.rows.first?.lastRunFailed == true)
        XCTAssertEqual(items.more, 1)
    }

    // MARK: The desktop's order and attention precedence, on the engine's own fixture

    func testTheEnginesFixtureIsKeptInItsOrderAndArrangedAsTheDesktopArrangesIt() throws {
        let cases = try fixture()
        guard case .object(let named) = cases else { return XCTFail("the fixture is an object of cases") }
        XCTAssertNotNil(named["busy"]); XCTAssertNotNil(named["chats_unknown"]); XCTAssertNotNil(named["empty"])
        for (name, wire) in named {
            let snapshot = try DashboardSnapshot(wire: wire)
            // The phone keeps exactly the order the desktop sent: every row, in place.
            XCTAssertEqual(snapshot.attention.rows.count, wire["attention"].array.count, name)
            XCTAssertEqual(snapshot.attention.rows.map(Self.targetID), wire["attention"].array.map { Self.wireTargetID($0["target"]) }, "\(name): attention order")
            XCTAssertEqual(snapshot.working.rows.map { Self.targetID($0.session.target) }, wire["working"].array.map { Self.wireTargetID($0["session"]["target"]) }, "\(name): working order")
            XCTAssertEqual(snapshot.finished.rows.map { Self.targetID($0.session.target) }, wire["finished"].array.map { Self.wireTargetID($0["session"]["target"]) }, "\(name): finished order")
            XCTAssertEqual(snapshot.projects.rows.map(\.id), wire["projects"].array.compactMap { $0["id"].string }, "\(name): project order")
            XCTAssertEqual(snapshot.tasksInProgress.rows.map(\.id), wire["tasks_in_progress"].array.compactMap { $0["id"].string }, "\(name): task order")
            // The phone's arrangement is the desktop's: applying it again changes nothing (ties keep the desktop's order), and lists whose
            // keys are all distinct come back in the desktop's order even after a shuffle.
            XCTAssertEqual(DashboardArrangement.attention(snapshot.attention.rows).map(Self.targetID), snapshot.attention.rows.map(Self.targetID), name)
            XCTAssertEqual(DashboardArrangement.projects(snapshot.projects.rows.shuffled()).map(\.id), snapshot.projects.rows.map(\.id), "\(name): projects")
            XCTAssertEqual(DashboardArrangement.tasks(snapshot.tasksInProgress.rows.shuffled()).map(\.id), snapshot.tasksInProgress.rows.map(\.id), "\(name): tasks")
            XCTAssertEqual(DashboardArrangement.finished(snapshot.finished.rows).map { Self.targetID($0.session.target) }, snapshot.finished.rows.map { Self.targetID($0.session.target) }, name)
            XCTAssertEqual(DashboardArrangement.working(snapshot.working.rows).map { Self.targetID($0.session.target) }, snapshot.working.rows.map { Self.targetID($0.session.target) }, name)
            // Attention precedence: every waiting row before every failure, newest first within each, unknown times last.
            let rows = snapshot.attention.rows
            if let firstFailure = rows.firstIndex(where: { !$0.isWaiting }) { XCTAssertFalse(rows[firstFailure...].contains { $0.isWaiting }, "\(name): waiting first") }
            for group in [rows.filter(\.isWaiting), rows.filter { !$0.isWaiting }] {
                let times = group.map { $0.at ?? 0 }
                XCTAssertEqual(times, times.sorted(by: >), "\(name): newest first, unknown last")
            }
            // Working: the longest-running first, unknown starts last. Finished: the most recent first.
            let started = snapshot.working.rows.map(\.sinceUnix)
            XCTAssertEqual(started, started.sorted { ($0 == nil ? 1 : 0, $0 ?? 0) < ($1 == nil ? 1 : 0, $1 ?? 0) }, name)
            XCTAssertEqual(snapshot.finished.rows.compactMap(\.sinceUnix), snapshot.finished.rows.compactMap(\.sinceUnix).sorted(by: >), name)
            // Every count the desktop sent is read; an unknown one stays unknown.
            XCTAssertEqual(snapshot.totals.projects, wire["totals"]["projects"] == .null ? nil : Int(Self.number(wire["totals"]["projects"])), name)
            if snapshot.chatsKnown != true { XCTAssertNil(snapshot.totals.liveChats, "\(name): chats not read"); XCTAssertTrue(snapshot.projects.rows.allSatisfy { $0.liveChats == nil }, name) }
        }
        // The busy case exercises the precedence for real.
        let busy = try DashboardSnapshot(wire: named["busy"]!)
        XCTAssertTrue(busy.attention.rows.contains(where: \.isWaiting))
        XCTAssertTrue(busy.attention.rows.contains { if case .chatFailed = $0.kind { true } else { false } }, "a failed chat")
        XCTAssertTrue(busy.attention.rows.contains { if case .automation = $0.kind { true } else { false } }, "an automation")
        XCTAssertTrue(busy.attention.rows.contains { $0.isWaiting && $0.at == nil }, "a waiting row with no time")
        XCTAssertGreaterThan(busy.attention.rows.filter(\.isWaiting).compactMap(\.at).count, 1)
        XCTAssertTrue(busy.working.rows.contains { $0.sinceUnix == nil } && busy.working.rows.contains { $0.sinceUnix != nil })
        XCTAssertFalse(busy.finished.rows.isEmpty)
        XCTAssertGreaterThan(busy.projects.rows.count, 1)
        XCTAssertFalse(busy.tasksInProgress.rows.isEmpty)
        XCTAssertNotEqual(busy.global, .unknown)
        guard case .read(_, _, _, let automations) = busy.automations else { return XCTFail("automations read") }
        XCTAssertTrue(automations.rows.contains(where: \.paused))
        XCTAssertEqual(DashboardArrangement.automations(automations.rows.shuffled()).map(\.id), automations.rows.map(\.id))
        let unknown = try DashboardSnapshot(wire: named["chats_unknown"]!)
        XCTAssertEqual(unknown.chatsKnown, false)
        let empty = try DashboardSnapshot(wire: named["empty"]!)
        XCTAssertTrue(empty.projects.rows.isEmpty && empty.attention.rows.isEmpty && empty.working.rows.isEmpty)
    }

    func testTheArrangementPutsADesktopsRowsWhereTheDesktopWould() throws {
        func project(_ id: String, _ name: String, attention: Int = 0, working: Int = 0, shells: Int = 0, progress: Int = 0, todo: Int = 0) throws -> DashboardProject {
            try XCTUnwrap(DashboardProject(wire: try json("""
            {"id":"\(id)","name":"\(name)","attention":\(attention),"agents":{"working":\(working)},"live_shells":\(shells),"live_chats":0,"tasks":{"todo":\(todo),"in_progress":\(progress),"done":0}}
            """), chatsKnown: true))
        }
        let ids = (1...7).map { String(format: "00000000-0000-4000-8000-%012d", $0) }
        let rows = [try project(ids[0], "quiet b"), try project(ids[1], "Quiet A"), try project(ids[2], "todo", todo: 1), try project(ids[3], "progress", progress: 1),
                    try project(ids[4], "live", shells: 1), try project(ids[5], "working", working: 1), try project(ids[6], "needs", attention: 1)]
        XCTAssertEqual(DashboardArrangement.projects(rows.shuffled()).map(\.name), ["needs", "working", "live", "progress", "todo", "Quiet A", "quiet b"])
        func attention(_ kind: String, at: Int?) throws -> DashboardAttention {
            try XCTUnwrap(DashboardAttention(wire: try json("{\"kind\":\"\(kind)\",\"target\":{\"type\":\"project\",\"id\":\"\(alpha)\"}\(at.map { ",\"at\":\($0)" } ?? "")}")))
        }
        let mixed = [try attention("chat_failed", at: 300), try attention("waiting", at: nil), try attention("automation", at: 500), try attention("waiting", at: 200), try attention("waiting", at: 100)]
        let arranged = DashboardArrangement.attention(mixed)
        XCTAssertEqual(arranged.map(\.isWaiting), [true, true, true, false, false], "waiting first, whatever the time")
        XCTAssertEqual(arranged.map(\.at), [200, 100, nil, 500, 300])
    }

    // MARK: How current it is

    func testFreshnessSaysLiveTheAgeOrWhyAndMarksStale() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        XCTAssertEqual(DashboardFreshness.decide(connected: true, connectionLabel: "Connected", readAt: now.addingTimeInterval(-3), failure: nil, now: now),
                       DashboardFreshness(text: "Live", stale: false))
        XCTAssertEqual(DashboardFreshness.decide(connected: true, connectionLabel: "Connected", readAt: now.addingTimeInterval(-120), failure: nil, now: now),
                       DashboardFreshness(text: "Last read 2m ago", stale: true))
        XCTAssertEqual(DashboardFreshness.decide(connected: true, connectionLabel: "Connected", readAt: now.addingTimeInterval(-40), failure: "The Mac did not answer in time.", now: now),
                       DashboardFreshness(text: "Showing the read from 40s ago · The Mac did not answer in time.", stale: true))
        XCTAssertEqual(DashboardFreshness.decide(connected: false, connectionLabel: "Connection unavailable", readAt: now.addingTimeInterval(-7200), failure: nil, now: now),
                       DashboardFreshness(text: "Connection unavailable · showing the read from 2h ago", stale: true))
        XCTAssertEqual(DashboardFreshness.decide(connected: true, connectionLabel: "Connected", readAt: nil, failure: nil, now: now).text, "Reading the dashboard…")
        XCTAssertTrue(DashboardFreshness.decide(connected: false, connectionLabel: "Disconnected", readAt: nil, failure: nil, now: now).stale)
        XCTAssertEqual(DashboardText.duration(4), "now"); XCTAssertEqual(DashboardText.duration(45), "45s"); XCTAssertEqual(DashboardText.duration(3 * 86400), "3d")
    }

    // MARK: Where a row leads

    func testRowsLeadToTheirProjectSessionOrChatAndTheGlobalOrchestratorToItsOwnScreen() throws {
        let session = { (target: String, extra: String) in try XCTUnwrap(DashboardSession(wire: try self.json("{\"target\":\(target),\"agent\":\"Claude\",\"is_chat\":false\(extra)}"))) }
        let projectShell = try session("{\"type\":\"shell\",\"id\":\"\(shellA)\"}", ",\"project_id\":\"\(alpha)\"")
        XCTAssertEqual(DashboardNavigation.destination(projectShell.target, session: projectShell), .project(id: alpha, tab: "shell:\(shellA)"))
        let chat = DashboardTarget.chat(id: chatA, projectID: alpha, orchestrator: nil)
        XCTAssertEqual(DashboardNavigation.destination(chat), .project(id: alpha, tab: "chat:\(chatA)"))
        XCTAssertEqual(DashboardNavigation.destination(.chat(id: globalChat, projectID: nil, orchestrator: .global)), .global, "the global chat")
        XCTAssertEqual(DashboardNavigation.destination(.chat(id: globalChat, projectID: nil, orchestrator: nil), globalIDs: [globalChat]), .global)
        let globalTerminal = try session("{\"type\":\"shell\",\"id\":\"\(globalShell)\"}", ",\"orchestrator\":\"global\"")
        XCTAssertEqual(DashboardNavigation.destination(globalTerminal.target, session: globalTerminal), .global, "the global terminal, never a project")
        XCTAssertEqual(DashboardNavigation.destination(.shell(id: globalShell), globalIDs: [globalShell]), .global)
        XCTAssertEqual(DashboardNavigation.destination(.project(id: beta)), .project(id: beta, tab: nil))
        XCTAssertEqual(DashboardNavigation.destination(.task(id: shellA), projectID: beta), .project(id: beta, tab: nil))
        XCTAssertEqual(DashboardNavigation.destination(.automations(projectID: nil)), .none, "an app automation opens nothing on the phone")
        XCTAssertEqual(DashboardNavigation.destination(.automations(projectID: alpha)), .project(id: alpha, tab: nil))
        XCTAssertEqual(DashboardNavigation.destination(.shell(id: shellA)), .none, "a shell of no known project leads nowhere rather than guessing")
    }

    // MARK: The global orchestrator's row

    func testTheGlobalRowOpensWhatThereIsAndStartsOnlyWhatIsNot() throws {
        let chatEntry = try JSONDecoder().decode(RemoteSession.self, from: Data("""
        {"id":"\(globalChat)","project_id":null,"worktree_id":null,"kind":"orchestrator","cwd":"/","harness":"claude","alive":true,"created_at_unix":5,"mode":"chat","chat_id":"\(globalChat)","provider":"claude","activity":"done"}
        """.utf8))
        let open = GlobalOrchestratorStatus.decide(entry: chatEntry, dashboard: .chat(chatID: globalChat, provider: .claude, state: .running), canStart: true)
        XCTAssertEqual(open.action, .open); XCTAssertEqual(open.actionTitle, "Open global chat")
        XCTAssertEqual(open.summary, "Claude chat · Working"); XCTAssertEqual(open.activity, .working); XCTAssertEqual(open.mode, .chat)
        let terminalEntry = try JSONDecoder().decode(RemoteSession.self, from: Data("""
        {"id":"\(globalShell)","project_id":null,"worktree_id":null,"kind":"orchestrator","cwd":"/","harness":null,"alive":true,"created_at_unix":5,"activity":"waiting"}
        """.utf8))
        let terminal = GlobalOrchestratorStatus.decide(entry: terminalEntry, dashboard: nil, canStart: false)
        XCTAssertEqual(terminal.summary, "Runs in a terminal · Waiting for input"); XCTAssertEqual(terminal.action, .open)
        XCTAssertEqual(terminal.actionTitle, "Open global orchestrator")
        XCTAssertNil(terminal.provider, "no provider is invented for a terminal")
        XCTAssertEqual(GlobalOrchestratorStatus.decide(entry: nil, dashboard: .none(exitedTerminal: false), canStart: true).action, .start)
        XCTAssertEqual(GlobalOrchestratorStatus.decide(entry: nil, dashboard: .none(exitedTerminal: true), canStart: true).summary, "Its last terminal has exited")
        XCTAssertEqual(GlobalOrchestratorStatus.decide(entry: nil, dashboard: nil, canStart: false).action, .unavailable)
        // The dashboard knows one the orchestrator list does not show yet: opened, never started a second time.
        XCTAssertEqual(GlobalOrchestratorStatus.decide(entry: nil, dashboard: .chat(chatID: globalChat, provider: .codex, state: .idle), canStart: true).action, .open)
        // A global terminal that has ended is nothing to open (the caller passes only what can be opened): Start is offered, and says why.
        let exited = GlobalOrchestratorStatus.decide(entry: nil, dashboard: nil, canStart: true, exitedTerminal: true)
        XCTAssertEqual(exited.action, .start)
        XCTAssertEqual(exited.summary, "Its last terminal has exited")
        XCTAssertEqual(GlobalOrchestratorStatus.decide(entry: nil, dashboard: .unknown, canStart: true, exitedTerminal: false).summary, "No global orchestrator listed")
    }

    // MARK: Helpers

    private static func targetID(_ row: DashboardAttention) -> String { targetID(row.target) }
    private static func targetID(_ target: DashboardTarget) -> String {
        switch target {
        case .shell(let id), .chat(let id, _, _), .project(let id), .task(let id): id
        case .automations(let project): "automations:\(project ?? "app")"
        }
    }
    private static func wireTargetID(_ target: JSONValue) -> String {
        target["type"].string == "automations" ? "automations:\(target["project_id"].string ?? "app")" : (target["id"].string ?? "")
    }
    private static func number(_ value: JSONValue) -> Double { if case .number(let n) = value { n } else { -1 } }
}
