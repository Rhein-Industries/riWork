import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// A desktop that reports what its agents are doing, and that the test can change under the phone: the lists are raw wire JSON, so
/// they can carry odd values too. It records what it was asked.
actor ActivityTransport: RemoteTransport {
    var connected = false
    var calls: [String] = []
    var projects: JSONValue = .array([])
    var shells: JSONValue = .array([])
    var orchestrators: JSONValue = .array([])
    /// Lists fail (like a dropped request) without the connection going away.
    var failing = false
    private var created = 0

    func setProjects(_ json: String) { projects = (try? JSONDecoder().decode(JSONValue.self, from: Data(json.utf8))) ?? .null }
    func setShells(_ json: String) { shells = (try? JSONDecoder().decode(JSONValue.self, from: Data(json.utf8))) ?? .null }
    func setOrchestrators(_ json: String) { orchestrators = (try? JSONDecoder().decode(JSONValue.self, from: Data(json.utf8))) ?? .null }
    func setFailing(_ on: Bool) { failing = on }
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { connected = true; return pairing }
    func disconnect() async { connected = false }
    func isConnected() async -> Bool { connected }
    func count(_ method: String) -> Int { calls.filter { $0 == method }.count }

    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        guard connected else { throw RemoteError.disconnected }
        try RequestValidation.validate(method: method, params: params, id: id)
        if method != "appearance.get" { calls.append(method) }
        if failing, method.hasSuffix(".list") { throw RemoteError.timeout }
        switch method {
        case "projects.list": return .object(["projects": projects])
        case "worktrees.list": return .object(["worktrees": .array([])])
        case "orchestrators.list": return .object(["orchestrators": orchestrators])
        case "shells.list": return .object(["shells": shells])
        case "shell.output": return .object(["shell_id": params["shell_id"]!, "output": .string("screen")])
        case "shell.resize": return .object(["shell_id": params["shell_id"]!, "columns": params["columns"]!, "rows": params["rows"]!])
        case "shell.resize.clear": return .object(["shell_id": params["shell_id"]!, "status": .string("cleared")])
        case "appearance.get": throw RemoteError.rpc(code: "not_found", message: "appearance not published")
        case "project.create":
            // The way a desktop answers before it has worked out the new project's edit time: no `last_edited_unix`.
            created += 1
            let newID = String(format: "ccccccc%d-cccc-4ccc-8ccc-cccccccccccc", created)
            let name = params["name"]?.string ?? ""
            let entry = try JSONDecoder().decode(JSONValue.self, from: Data("{\"id\":\"\(newID)\",\"name\":\"\(name)\",\"root\":\"/Users/me/\(name)\",\"created_at\":\(5_000 + created)}".utf8))
            projects = .array(projects.array + [entry])
            return .object(["project_id": .string(newID), "project": entry])
        default: throw RemoteError.protocolViolation("Unknown method \(method)")
        }
    }
}

/// Agent activity on the phone: the project order and its memory, the lists read again while they are looked at, and what the rows and
/// tabs draw for each state.
@MainActor final class ActivityTests: XCTestCase {
    private let fixture = "11111111-1111-4111-8111-111111111111"
    private let first = "44444444-4444-4444-8444-444444444444"
    private let second = "55555555-5555-4555-8555-555555555555"
    private let manager = "66666666-6666-4666-8666-666666666666"
    private var defaultsNames: [String] = []
    private var retained: [UIWindow] = []

    override func tearDown() async throws {
        for name in defaultsNames { UserDefaults().removePersistentDomain(forName: name) }
        defaultsNames = []
        for window in retained { window.isHidden = true }
        retained = []
    }

    // MARK: Fixtures

    /// A project id the desktop would send: a full UUID, written here as one letter ("a" is aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa).
    private func uid(_ letter: String) -> String { letter.count == 1 ? "\(String(repeating: letter, count: 8))-\(String(repeating: letter, count: 4))-4\(String(repeating: letter, count: 3))-8\(String(repeating: letter, count: 3))-\(String(repeating: letter, count: 12))" : letter }
    private func project(_ id: String, _ name: String, created: Int, edited: Int? = nil, active: Int? = nil, agents: String? = nil) -> String {
        "{\"id\":\"\(uid(id))\",\"name\":\"\(name)\",\"root\":\"/\(name)\",\"created_at\":\(created)\(edited.map { ",\"last_edited_unix\":\($0)" } ?? "")\(active.map { ",\"last_activity_unix\":\($0)" } ?? "")\(agents.map { ",\"agents\":\($0)" } ?? "")}"
    }
    private func shell(_ id: String, harness: String = "claude", at: Int = 1, extra: String = "") -> String {
        "{\"id\":\"\(id)\",\"project_id\":\"\(fixture)\",\"worktree_id\":null,\"kind\":\"project\",\"cwd\":\"/fixture\",\"harness\":\"\(harness)\",\"alive\":true,\"created_at_unix\":\(at)\(extra.isEmpty ? "" : ","+extra)}"
    }
    private func makeStore(project: String?) throws -> KeychainStore {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        return keychain
    }
    private func defaults() -> UserDefaults {
        let name = "com.riwork.tests.activity.\(UUID().uuidString)"
        defaultsNames.append(name)
        return UserDefaults(suiteName: name)!
    }
    private struct Rig { let model: RemoteModel; let transport: ActivityTransport; let defaults: UserDefaults; let keychain: KeychainStore }
    /// A connected model. `chosen` is the project whose tabs are loaded.
    private func connected(projects: String = "[]", shells: String = "[]", orchestrators: String = "[]", chosen: String? = nil, interval: Duration = .milliseconds(25),
                           defaults suite: UserDefaults? = nil) async throws -> Rig {
        let transport = ActivityTransport()
        await transport.setProjects(projects); await transport.setShells(shells); await transport.setOrchestrators(orchestrators)
        let keychain = try makeStore(project: chosen)
        let defaults = suite ?? self.defaults()
        let model = RemoteModel(client: transport, keychain: keychain, pollInterval: .seconds(30), defaults: defaults, activityRefreshInterval: interval)
        await model.connect()
        XCTAssertEqual(model.state, .connected)
        return Rig(model: model, transport: transport, defaults: defaults, keychain: keychain)
    }
    private func eventually(_ what: String, timeout: Double = 4, file: StaticString = #filePath, line: UInt = #line, _ condition: () async -> Bool) async {
        let end = Date().addingTimeInterval(timeout)
        while await !condition(), Date() < end { try? await Task.sleep(for: .milliseconds(5)) }
        let met = await condition()
        XCTAssertTrue(met, what, file: file, line: line)
    }
    /// The project ids in the order shown, as the letters they were written with.
    private func ids(_ model: RemoteModel) -> [String] { model.sortedProjects.map { String($0.id.prefix(1)) } }

    // MARK: The project order

    func testTheOrderIsRecentFirstAndFollowsTheChoice() async throws {
        let list = "[\(project("a", "Alpha", created: 10, edited: 100)),\(project("b", "bravo", created: 30, edited: 300)),\(project("c", "Charlie", created: 20)),\(project("d", "Delta", created: 40, edited: 200, active: 50)),\(project("e", "Echo", created: 5, active: 70))]"
        let rig = try await connected(projects: list)
        let model = rig.model
        XCTAssertEqual(model.projectSort, .recent)
        XCTAssertEqual(ids(model), ["e", "d", "b", "a", "c"], "terminal activity newest first, then edited newest first, then the one with neither")
        model.setProjectSort(.name)
        XCTAssertEqual(ids(model), ["a", "b", "c", "d", "e"])
        model.setProjectSort(.dateAdded)
        XCTAssertEqual(ids(model), ["d", "b", "c", "a", "e"])
        model.cycleProjectSort()
        XCTAssertEqual(model.projectSort, .recent)
        model.cycleProjectSort()
        XCTAssertEqual(model.projectSort, .name)
        // The list the desktop sent is untouched; only the order shown differs.
        XCTAssertEqual(model.projects.map { String($0.id.prefix(1)) }, ["a", "b", "c", "d", "e"])
        await model.disconnect()
    }
    func testTheOrderFollowsTerminalActivityAsTheListIsReadAgain() async throws {
        let rig = try await connected(projects: "[\(project("a", "Alpha", created: 1, edited: 900, active: 100)),\(project("b", "Beta", created: 2, edited: 800, active: 50)),\(project("c", "Gamma", created: 3, edited: 950))]")
        let model = rig.model
        XCTAssertEqual(ids(model), ["a", "b", "c"], "Gamma was edited last, but nothing of it is running")
        let watching = Task { await model.keepFresh(.projects) }
        // Beta's terminal prints: it moves to the top without any refresh. A project whose last shell closed loses its figure and
        // drops behind the ones that have one.
        await rig.transport.setProjects("[\(project("a", "Alpha", created: 1, edited: 900)),\(project("b", "Beta", created: 2, edited: 800, active: 200)),\(project("c", "Gamma", created: 3, edited: 950))]")
        await eventually("the new order reaches the list") { ids(model) == ["b", "c", "a"] }
        watching.cancel()
        await watching.value
        await model.disconnect()
    }
    func testTheChoiceIsRememberedAcrossLaunches() async throws {
        let suite = defaults()
        let rig = try await connected(projects: "[\(project("a", "Alpha", created: 1))]", defaults: suite)
        XCTAssertEqual(rig.model.projectSort, .recent, "Recent until the person chooses")
        rig.model.setProjectSort(.dateAdded)
        XCTAssertEqual(suite.string(forKey: ProjectSort.defaultsKey), "dateAdded")
        await rig.model.disconnect()
        // A new model on the same defaults (a relaunch) starts with it.
        let relaunched = RemoteModel(client: ActivityTransport(), keychain: try makeStore(project: nil), defaults: suite)
        XCTAssertEqual(relaunched.projectSort, .dateAdded)
        // Choosing the same order again, or one that is unknown, changes nothing.
        suite.set("nonsense", forKey: ProjectSort.defaultsKey)
        XCTAssertEqual(RemoteModel(client: ActivityTransport(), keychain: try makeStore(project: nil), defaults: suite).projectSort, .recent)
    }
    func testOddActivityFieldsNeverStopTheListsLoading() async throws {
        let odd = "[\(project("a", "Alpha", created: 1, edited: 7, agents: "{\"working\":1,\"waiting\":0}")),{\"id\":\"\(uid("b"))\",\"name\":\"Bravo\",\"root\":\"/b\",\"created_at\":2,\"last_edited_unix\":\"soon\",\"agents\":[1,2]}]"
        let shells = "[\(shell(first, extra: "\"activity\":\"thinking\",\"subagents_working\":-4,\"activity_since_unix\":\"x\""))]"
        let rig = try await connected(projects: odd, shells: shells, chosen: uid("a"))
        XCTAssertEqual(rig.model.projects.count, 2)
        XCTAssertNil(rig.model.error)
        XCTAssertEqual(rig.model.shells.first?.activity, .unknown)
        XCTAssertEqual(rig.model.shells.first?.subagents_working, 0)
        XCTAssertEqual(ids(rig.model), ["a", "b"])
        await rig.model.disconnect()
    }
    func testAProjectMadeOnThePhoneLandsWhereTheOrderPutsIt() async throws {
        let list = "[\(project("a", "Alpha", created: 10, edited: 4_000)),\(project("e", "Mike", created: 20, edited: 3_000)),\(project("g", "Golf", created: 30, active: 3_500))]"
        let rig = try await connected(projects: list)
        let model = rig.model
        let failure = await model.createProject(try NewProjectRequest(name: "Beta", git: true))
        XCTAssertNil(failure)
        let made = try XCTUnwrap(model.projects.first { $0.name == "Beta" })
        XCTAssertNil(made.last_edited_unix, "the desktop has no edit time for it yet")
        XCTAssertNil(made.last_activity_unix, "and nothing of it has run yet")
        // Recent: it was just made, so it is on top and not behind every project with activity.
        XCTAssertEqual(model.sortedProjects.first?.id, made.id)
        XCTAssertEqual(ids(model).dropFirst().map { $0 }, ["g", "a", "e"])
        // Name: alphabetically, between Alpha and Golf.
        model.setProjectSort(.name)
        XCTAssertEqual(model.sortedProjects.map(\.name), ["Alpha", "Beta", "Golf", "Mike"])
        // Date added: newest.
        model.setProjectSort(.dateAdded)
        XCTAssertEqual(model.sortedProjects.first?.id, made.id)
        await model.disconnect()
    }
    func testAProjectMadeBeforeIsNotMistakenForOneMadeHere() async throws {
        let list = "[\(project("a", "Alpha", created: 10, edited: 9_000)),\(project("f", "Old", created: 5_999))]"
        let rig = try await connected(projects: list)
        XCTAssertEqual(ids(rig.model), ["a", "f"], "undated projects follow the dated ones when this phone did not make them")
        await rig.model.disconnect()
    }

    // MARK: Reading the lists again

    func testTheProjectListIsReadAgainWhileItIsOnScreenAndNotAfterwards() async throws {
        let rig = try await connected(projects: "[\(project("a", "Alpha", created: 1, edited: 5, agents: "{\"working\":0,\"waiting\":0}"))]")
        let model = rig.model
        XCTAssertEqual(model.projects.first?.agents?.working, 0)
        let watching = Task { await model.keepFresh(.projects) }
        await rig.transport.setProjects("[\(project("a", "Alpha", created: 1, edited: 9, agents: "{\"working\":2,\"waiting\":1}"))]")
        await eventually("the counts reach the list without a refresh") { model.projects.first?.agents == ProjectAgents(working: 2, waiting: 1) }
        XCTAssertEqual(model.projects.first?.last_edited_unix, 9)
        // And again when they change again.
        await rig.transport.setProjects("[\(project("a", "Alpha", created: 1, edited: 9, agents: "{\"working\":0,\"waiting\":1}"))]")
        await eventually("a later change too") { model.projects.first?.agents == ProjectAgents(working: 0, waiting: 1) }
        // The view went away: nothing more is asked.
        watching.cancel()
        await watching.value
        let settled = await rig.transport.count("projects.list")
        try await Task.sleep(for: .milliseconds(150))
        let after = await rig.transport.count("projects.list")
        XCTAssertEqual(after, settled, "no reads once nobody looks at the list")
        await model.disconnect()
    }
    func testTheTabsAreReadAgainWhileTheyAreOnScreen() async throws {
        let rig = try await connected(projects: "[\(project(fixture, "Fixture", created: 1))]",
                                      shells: "[\(shell(first, extra: "\"activity\":\"working\",\"subagents_working\":1")),\(shell(second, harness: "codex", at: 2))]",
                                      orchestrators: "[{\"id\":\"\(manager)\",\"project_id\":\"\(fixture)\",\"worktree_id\":null,\"kind\":\"orchestrator\",\"cwd\":\"/fixture\",\"harness\":null,\"alive\":true,\"created_at_unix\":0,\"activity\":\"done\"}]",
                                      chosen: fixture)
        let model = rig.model
        await model.chooseProject(fixture)
        func state(_ id: String) -> (AgentActivity, Int)? { model.sessions.first { $0.id == id }.map { ($0.activity, $0.subagents_working) } }
        XCTAssertEqual(state(first)?.0, .working)
        XCTAssertEqual(state(first)?.1, 1)
        XCTAssertEqual(state(manager)?.0, .done)
        let selected = model.sessionID
        let watching = Task { await model.keepFresh(.sessions) }
        await rig.transport.setShells("[\(shell(first, extra: "\"activity\":\"waiting\"")),\(shell(second, harness: "codex", at: 2, extra: "\"activity\":\"working\",\"subagents_working\":3"))]")
        await rig.transport.setOrchestrators("[{\"id\":\"\(manager)\",\"project_id\":\"\(fixture)\",\"worktree_id\":null,\"kind\":\"orchestrator\",\"cwd\":\"/fixture\",\"harness\":null,\"alive\":true,\"created_at_unix\":0,\"activity\":\"working\"}]")
        await eventually("each tab shows its new state") {
            state(first)?.0 == .waiting && state(second)?.0 == .working && state(second)?.1 == 3 && state(manager)?.0 == .working
        }
        XCTAssertEqual(model.sessionID, selected, "reading the tabs again does not move the selection")
        watching.cancel()
        await watching.value
        let settled = await rig.transport.count("shells.list")
        try await Task.sleep(for: .milliseconds(150))
        let after = await rig.transport.count("shells.list")
        XCTAssertEqual(after, settled)
        await model.disconnect()
    }
    func testNothingIsReadWhileDisconnectedOrInTheBackground() async throws {
        let rig = try await connected(projects: "[\(project("a", "Alpha", created: 1))]")
        let model = rig.model
        // In the background: no reads, and none start by themselves.
        model.setAppActive(false)
        let watching = Task { await model.keepFresh(.projects) }
        try await Task.sleep(for: .milliseconds(120))
        let quiet = await rig.transport.count("projects.list")
        try await Task.sleep(for: .milliseconds(120))
        let stillQuiet = await rig.transport.count("projects.list")
        XCTAssertEqual(stillQuiet, quiet)
        // Back in the foreground: they resume.
        model.setAppActive(true)
        await eventually("reads resume in the foreground") { await rig.transport.count("projects.list") > stillQuiet }
        // Link down: nothing is asked of a connection that is not there.
        await model.disconnect()
        let down = await rig.transport.count("projects.list")
        try await Task.sleep(for: .milliseconds(150))
        let downAfter = await rig.transport.count("projects.list")
        XCTAssertEqual(downAfter, down)
        XCTAssertNotEqual(model.state, .connected)
        watching.cancel()
        await watching.value
    }
    func testAListReadAMomentAgoIsNotReadAgainAtOnce() async throws {
        let rig = try await connected(projects: "[\(project("a", "Alpha", created: 1))]", interval: .seconds(30))
        let model = rig.model
        let before = await rig.transport.count("projects.list")
        let watching = Task { await model.keepFresh(.projects) }
        try await Task.sleep(for: .milliseconds(150))
        let after = await rig.transport.count("projects.list")
        XCTAssertEqual(after, before, "the connect just read it")
        watching.cancel()
        await watching.value
        await model.disconnect()
    }
    func testAFailedReadIsSkippedQuietlyAndTheNextOneWorks() async throws {
        let rig = try await connected(projects: "[\(project("a", "Alpha", created: 1, agents: "{\"working\":1,\"waiting\":0}"))]")
        let model = rig.model
        await rig.transport.setFailing(true)
        let watching = Task { await model.keepFresh(.projects) }
        await eventually("reads are being attempted") { await rig.transport.count("projects.list") >= 3 }
        XCTAssertNil(model.error, "a list that could not be read again is not an error banner")
        XCTAssertEqual(model.projects.first?.agents?.working, 1, "what was shown stays")
        await rig.transport.setFailing(false)
        await rig.transport.setProjects("[\(project("a", "Alpha", created: 1, agents: "{\"working\":4,\"waiting\":0}"))]")
        await eventually("it recovers") { model.projects.first?.agents?.working == 4 }
        watching.cancel()
        await watching.value
        await model.disconnect()
    }

    // MARK: What is drawn

    private func show<V: View>(_ view: V, width: CGFloat = 360, height: CGFloat = 640) async -> UIHostingController<V> {
        let host = UIHostingController(rootView: view)
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: width, height: height))
        window.rootViewController = host
        window.makeKeyAndVisible()
        retained.append(window)
        host.view.layoutIfNeeded()
        try? await Task.sleep(for: .milliseconds(120))
        return host
    }
    private func size<V: View>(_ view: V) -> CGSize {
        UIHostingController(rootView: view).sizeThatFits(in: CGSize(width: 300, height: 100))
    }
    /// A desktop palette with colors that cannot be mistaken for one another or for the built-in ones (and readable enough that the theme
    /// keeps them: a text color below 3:1 on its background falls back to the built-in one).
    private func syncedStyle() throws -> DesktopStyle {
        let json = try JSONDecoder().decode(JSONValue.self, from: Data("""
        {"v":1,"updated_at":1790000000,"dark":false,"palette":{"bg":"#ffffff","panel":"#f0f0f0","panel_active":"#e0e0e0","divider":"#cccccc","cyan":"#0000ff",
         "magenta":"#00aa00","gold":"#ff0000","text":"#000000","muted":"#555555"}}
        """.utf8))
        return DesktopStyle(DesktopTheme.resolve(try DesktopAppearance(json: json)))
    }
    /// What `view` looks like drawn at 3x on a clear background, light appearance.
    private func pixels<V: View>(_ view: V) -> [(r: Int, g: Int, b: Int, a: Int)] {
        let renderer = ImageRenderer(content: view.environment(\.colorScheme, .light))
        renderer.scale = 3
        guard let image = renderer.uiImage?.cgImage, let data = image.dataProvider?.data, let bytes = CFDataGetBytePtr(data) else { return [] }
        let step = image.bitsPerPixel / 8
        var found: [(Int, Int, Int, Int)] = []
        for y in 0..<image.height {
            for x in 0..<image.width {
                let p = bytes + y * image.bytesPerRow + x * step
                // BGRA or RGBA, premultiplied: pixels that are mostly covered are compared, as the color they would have fully covered.
                let (r, g, b, a) = image.bitmapInfo.contains(.byteOrder32Little) ? (Int(p[2]), Int(p[1]), Int(p[0]), Int(p[3])) : (Int(p[0]), Int(p[1]), Int(p[2]), Int(p[3]))
                if a >= 160 { found.append((min(255, r * 255 / a), min(255, g * 255 / a), min(255, b * 255 / a), a)) }
            }
        }
        return found
    }
    private func count(_ pixels: [(r: Int, g: Int, b: Int, a: Int)], near hex: (Int, Int, Int)) -> Int {
        pixels.filter { abs($0.r - hex.0) < 16 && abs($0.g - hex.1) < 16 && abs($0.b - hex.2) < 16 }.count
    }

    func testOnlyWorkingWaitingAndDoneDrawAnything() {
        for activity in [AgentActivity.unknown, .exited] {
            XCTAssertEqual(size(ActivityIndicator(activity: activity, subagents: 3)), .zero, "\(activity) looks as it always did")
        }
        let working = size(ActivityIndicator(activity: .working))
        let waiting = size(ActivityIndicator(activity: .waiting))
        let done = size(ActivityIndicator(activity: .done))
        for (name, shown) in [("working", working), ("waiting", waiting), ("done", done)] {
            XCTAssertGreaterThan(shown.width, 0, name)
            XCTAssertGreaterThan(shown.height, 0, name)
        }
        // The subagent count sits beside the dot and makes it wider; it is not shown without a state to hang it on.
        XCTAssertGreaterThan(size(ActivityIndicator(activity: .working, subagents: 2)).width, working.width)
        XCTAssertEqual(size(ActivityIndicator(activity: .waiting, subagents: 2)), waiting)
        XCTAssertEqual(size(ActivityIndicator(activity: .done, subagents: 2)), done)
    }
    func testTheIndicatorIsDrawnInTheSyncedDesktopColors() throws {
        let style = try syncedStyle()
        let accent = (0, 0, 255), gold = (255, 0, 0), muted = (85, 85, 85)
        func drawn(_ activity: AgentActivity, subagents: Int = 0) -> [(r: Int, g: Int, b: Int, a: Int)] {
            pixels(ActivityIndicator(activity: activity, subagents: subagents).environment(\.desktopStyle, style).padding(6))
        }
        // Working is the accent color, and only that.
        let working = drawn(.working, subagents: 2)
        XCTAssertGreaterThan(count(working, near: accent), 8, "the dot and the +2")
        XCTAssertEqual(count(working, near: gold), 0)
        // Waiting is gold: a filled disc.
        let waiting = drawn(.waiting)
        XCTAssertGreaterThan(count(waiting, near: gold), 20)
        XCTAssertEqual(count(waiting, near: accent), 0)
        // Done is a quiet check in the muted color.
        let done = drawn(.done)
        XCTAssertGreaterThan(count(done, near: muted), 3)
        XCTAssertEqual(count(done, near: gold) + count(done, near: accent), 0)
        // Unknown and exited draw nothing at all.
        XCTAssertTrue(drawn(.unknown).isEmpty)
        XCTAssertTrue(drawn(.exited).isEmpty)
    }
    func testProjectBadgesCountWorkingAndWaitingAgents() throws {
        let style = try syncedStyle()
        let accent = (0, 0, 255), gold = (255, 0, 0)
        func width(_ agents: ProjectAgents) -> CGFloat { size(ProjectAgentBadges(agents: agents)).width }
        XCTAssertGreaterThan(width(ProjectAgents(working: 2, waiting: 0)), 0)
        XCTAssertGreaterThan(width(ProjectAgents(working: 0, waiting: 1)), 0)
        XCTAssertGreaterThan(width(ProjectAgents(working: 2, waiting: 1)), width(ProjectAgents(working: 2, waiting: 0)))
        XCTAssertEqual(size(ProjectAgentBadges(agents: ProjectAgents(working: 0, waiting: 0))), .zero)
        func drawn(_ agents: ProjectAgents) -> [(r: Int, g: Int, b: Int, a: Int)] { pixels(ProjectAgentBadges(agents: agents).environment(\.desktopStyle, style).padding(6)) }
        let both = drawn(ProjectAgents(working: 2, waiting: 1))
        XCTAssertGreaterThan(count(both, near: accent), 8)
        XCTAssertGreaterThan(count(both, near: gold), 20)
        XCTAssertEqual(count(drawn(ProjectAgents(working: 3, waiting: 0)), near: gold), 0, "no waiting agents, no gold")
        XCTAssertEqual(count(drawn(ProjectAgents(working: 0, waiting: 3)), near: accent), 0, "no working agents, no accent")
    }
    func testTheProjectListHasASortControlOnlyWhenThereIsSomethingToSort() async throws {
        let many = try await connected(projects: "[\(project("a", "Alpha", created: 1)),\(project("b", "Bravo", created: 2))]")
        let one = try await connected(projects: "[\(project("a", "Alpha", created: 1))]")
        func menus(_ rig: Rig) async -> Int {
            let host = await show(ProjectSelectionView(model: rig.model, onSelect: { _ in }, onNewTerminal: nil, onNewProject: nil, shortcutsActive: true))
            return descendants(UIControl.self, in: host.view).filter { $0.isContextMenuInteractionEnabled }.count
        }
        let withMany = await menus(many), withOne = await menus(one)
        XCTAssertGreaterThan(withMany, withOne, "the sort menu is one more control than a list of one project has")
        await many.model.disconnect(); await one.model.disconnect()
    }
    private func descendants<T: UIView>(_ type: T.Type, in view: UIView) -> [T] {
        view.subviews.compactMap { $0 as? T } + view.subviews.flatMap { descendants(type, in: $0) }
    }
    func testCommandOStepsTheSortOrderOnlyWhileTheListIsOnTop() async throws {
        let rig = try await connected(projects: "[\(project("a", "Alpha", created: 1)),\(project("b", "Bravo", created: 2))]")
        func commandO(_ host: UIViewController) -> UIKeyCommand? { (host.keyCommands ?? []).first { $0.input?.lowercased() == "o" && $0.modifierFlags == .command } }
        let top = await show(ProjectSelectionView(model: rig.model, onSelect: { _ in }, onNewTerminal: nil, onNewProject: nil, shortcutsActive: true))
        XCTAssertNotNil(commandO(top), "keyCommands: \((top.keyCommands ?? []).map { "\($0.input ?? "")|\($0.modifierFlags.rawValue)" })")
        let covered = await show(ProjectSelectionView(model: rig.model, onSelect: { _ in }, onNewTerminal: nil, onNewProject: nil, shortcutsActive: false))
        XCTAssertNil(commandO(covered), "a terminal or a sheet on top of the list keeps its keys")
        // It is the only one on ⌘O, and the list claims none of the terminal's (⌘N, ⌘K, ⌘,).
        XCTAssertEqual((top.keyCommands ?? []).filter { $0.input?.lowercased() == "o" }.count, 1)
        for input in ["n", "k", ","] { XCTAssertFalse((top.keyCommands ?? []).contains { $0.input?.lowercased() == input && $0.modifierFlags == .command }, input) }
        await rig.model.disconnect()
    }
    func testWithOneProjectThereIsNoShortcutEither() async throws {
        let rig = try await connected(projects: "[\(project("a", "Alpha", created: 1))]")
        let host = await show(ProjectSelectionView(model: rig.model, onSelect: { _ in }, onNewTerminal: nil, onNewProject: nil, shortcutsActive: true))
        XCTAssertTrue((host.keyCommands ?? []).filter { $0.input?.lowercased() == "o" }.isEmpty)
        await rig.model.disconnect()
    }
    func testTheSearchTakesRowsOutOfTheSameOrder() async throws {
        let list = "[\(project("a", "web-alpha", created: 10, edited: 400)),\(project("b", "api", created: 30, edited: 300)),\(project("c", "web-charlie", created: 20)),\(project("d", "docs", created: 40, edited: 200))]"
        let rig = try await connected(projects: list)
        let model = rig.model
        XCTAssertEqual(model.visibleProjects(matching: "").map(\.name), ["web-alpha", "api", "docs", "web-charlie"])
        XCTAssertEqual(model.visibleProjects(matching: "WEB").map(\.name), ["web-alpha", "web-charlie"])
        model.setProjectSort(.name)
        XCTAssertEqual(model.visibleProjects(matching: "").map(\.name), ["api", "docs", "web-alpha", "web-charlie"])
        XCTAssertEqual(model.visibleProjects(matching: "o").map(\.name), ["docs"])
        model.setProjectSort(.dateAdded)
        XCTAssertEqual(model.visibleProjects(matching: "").map(\.name), ["docs", "api", "web-charlie", "web-alpha"])
        XCTAssertEqual(model.visibleProjects(matching: "web").map(\.name), ["web-charlie", "web-alpha"])
        XCTAssertTrue(model.visibleProjects(matching: "zzz").isEmpty)
        await model.disconnect()
    }
}
