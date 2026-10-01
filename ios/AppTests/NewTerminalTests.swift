import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// A desktop that opens and closes terminals the way the connector does, and records what it was asked.
actor SpawnTransport: RemoteTransport {
    enum CreateMode { case ok, unsupported, notFound, harness, cliError(String), timeout, exited, unreadable }
    enum CloseMode { case ok, unsupported, notFound, cliError(String), timeout }
    struct Call { let method: String; let params: [String: JSONValue] }
    var connected = false
    var connections = 0
    var calls: [Call] = []
    var createMode = CreateMode.ok
    var closeMode = CloseMode.ok
    var gated = false
    var sessions: [RemoteSession]
    var worktrees: [RemoteWorktree] = []
    var created: [String] = []
    private var counter = 0

    init(sessions: [RemoteSession] = []) { self.sessions = sessions }

    func setCreateMode(_ mode: CreateMode) { createMode = mode }
    func setCloseMode(_ mode: CloseMode) { closeMode = mode }
    func setGated(_ on: Bool) { gated = on }
    func setWorktrees(_ list: [RemoteWorktree]) { worktrees = list }
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { connected = true; connections += 1; return pairing }
    func disconnect() async { connected = false }
    func isConnected() async -> Bool { connected }
    func count(_ method: String) -> Int { calls.filter { $0.method == method }.count }
    func params(of method: String) -> [[String: JSONValue]] { calls.filter { $0.method == method }.map(\.params) }
    func methods() -> [String] { calls.map(\.method) }

    private func encode<T: Encodable>(_ value: T) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(value)) }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        guard connected else { throw RemoteError.disconnected }
        try RequestValidation.validate(method: method, params: params, id: id)
        if method != "appearance.get" { calls.append(Call(method: method, params: params)) }
        switch method {
        case "projects.list": return .object(["projects": try JSONDecoder().decode(JSONValue.self, from: Data("[{\"id\":\"11111111-1111-4111-8111-111111111111\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}]".utf8))])
        case "worktrees.list": return .object(["worktrees": try encode(worktrees)])
        case "orchestrators.list": return .object(["orchestrators": .array([])])
        case "shells.list": return .object(["shells": try encode(sessions)])
        case "shell.output": return .object(["shell_id": params["shell_id"]!, "output": .string("screen of \(params["shell_id"]?.string ?? "")")])
        case "shell.resize": return .object(["shell_id": params["shell_id"]!, "columns": params["columns"]!, "rows": params["rows"]!])
        case "shell.resize.clear": return .object(["shell_id": params["shell_id"]!, "status": .string("cleared")])
        case "appearance.get": throw RemoteError.rpc(code: "not_found", message: "appearance not published")
        case "shell.create":
            while gated { try await Task.sleep(for: .milliseconds(3)) }
            switch createMode {
            case .unsupported: throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")
            case .notFound: throw RemoteError.rpc(code: "not_found", message: "project not found")
            case .harness: throw RemoteError.rpc(code: "harness_unavailable", message: "codex is not installed or is not on PATH")
            case .cliError(let text): throw RemoteError.rpc(code: "cli_error", message: text)
            case .timeout: throw RemoteError.timeout
            case .unreadable: return .object(["status": .string("created")])
            case .ok, .exited:
                counter += 1
                let id = String(format: "aaaaaaa%d-aaaa-4aaa-8aaa-aaaaaaaaaaaa", counter)
                let alive: Bool = { if case .exited = createMode { false } else { true } }()
                let kind = params["kind"]?.string ?? "shell"
                let harness: JSONValue = kind == "shell" ? .null : .string(kind)
                let project = params["project_id"]?.string ?? worktrees.first { $0.id == params["worktree_id"]?.string }?.project_id ?? ""
                let entry: JSONValue = .object(["id": .string(id), "project_id": .string(project), "worktree_id": params["worktree_id"] ?? .null, "kind": .string("project"),
                                                "cwd": .string("/fixture"), "harness": harness, "alive": .bool(alive), "created_at_unix": .number(100 + Double(counter))])
                created.append(id)
                if alive { sessions.append(try entry.decode(RemoteSession.self)) }
                return .object(["shell_id": .string(id), "shell": entry])
            }
        case "shell.close":
            switch closeMode {
            case .unsupported: throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")
            case .notFound:
                // Gone already: the desktop does not list it any more either.
                sessions.removeAll { $0.id == params["shell_id"]?.string }
                throw RemoteError.rpc(code: "not_found", message: "unknown shell")
            case .cliError(let text): throw RemoteError.rpc(code: "cli_error", message: text)
            case .timeout: throw RemoteError.timeout
            case .ok:
                let id = params["shell_id"]!.string!
                sessions.removeAll { $0.id == id }
                return .object(["shell_id": .string(id), "status": .string("closed")])
            }
        default: throw RemoteError.protocolViolation("Unknown method \(method)")
        }
    }
}

@MainActor final class NewTerminalAppTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let first = "44444444-4444-4444-8444-444444444444"
    private let second = "55555555-5555-4555-8555-555555555555"
    private let tree = "33333333-3333-4333-8333-333333333333"
    private var defaultsNames: [String] = []

    private func session(_ id: String, worktree: String? = nil, kind: String = "project", harness: String? = nil, alive: Bool = true, at: Int = 1) throws -> RemoteSession {
        try JSONDecoder().decode(RemoteSession.self, from: Data("""
        {"id":"\(id)","project_id":"\(project)","worktree_id":\(worktree.map { "\"\($0)\"" } ?? "null"),"kind":"\(kind)","cwd":"/fixture","harness":\(harness.map { "\"\($0)\"" } ?? "null"),"alive":\(alive),"created_at_unix":\(at)}
        """.utf8))
    }
    private func worktree(_ id: String, _ branch: String, primary: Bool) throws -> RemoteWorktree {
        try JSONDecoder().decode(RemoteWorktree.self, from: Data("{\"id\":\"\(id)\",\"project_id\":\"\(project)\",\"branch\":\"\(branch)\",\"path\":\"/fixture/\(branch)\",\"is_primary\":\(primary),\"created_at\":1}".utf8))
    }
    private func makeStore(selected: String?) throws -> KeychainStore {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = selected
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        return keychain
    }
    private func defaults() -> UserDefaults {
        let name = "com.riwork.tests.newterminal.\(UUID().uuidString)"
        defaultsNames.append(name)
        return UserDefaults(suiteName: name)!
    }
    override func tearDown() async throws {
        for name in defaultsNames { UserDefaults().removePersistentDomain(forName: name) }
        defaultsNames = []
    }
    private struct Rig {
        let model: RemoteModel; let transport: SpawnTransport; let keychain: KeychainStore; let defaults: UserDefaults
    }
    private func connected(_ sessions: [RemoteSession]? = nil, worktrees: [RemoteWorktree] = [], defaults suite: UserDefaults? = nil) async throws -> Rig {
        let list = try sessions ?? [session(first, at: 1)]
        let transport = SpawnTransport(sessions: list)
        await transport.setWorktrees(worktrees)
        let keychain = try makeStore(selected: list.first?.id)
        let defaults = suite ?? self.defaults()
        let model = RemoteModel(client: transport, keychain: keychain, defaults: defaults)
        await model.connect()
        XCTAssertEqual(model.state, .connected)
        return Rig(model: model, transport: transport, keychain: keychain, defaults: defaults)
    }
    private func request(_ kind: NewTerminalKind = .shell, unrestricted: Bool = false) throws -> NewTerminalRequest {
        try NewTerminalRequest(target: .project(project), kind: kind, unrestricted: unrestricted)
    }
    private func settle(_ condition: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async {
        let deadline = ContinuousClock.now + .seconds(5)
        while !condition(), ContinuousClock.now < deadline { try? await Task.sleep(for: .milliseconds(5)) }
        XCTAssertTrue(condition(), file: file, line: line)
    }

    // MARK: Creating

    func testCreatingATerminalSendsOneRequestSelectsTheNewShellAndRemembersTheKind() async throws {
        let rig = try await connected()
        let model = rig.model
        XCTAssertEqual(model.lastTerminalKind, .shell, "the first time: a plain shell")
        var created: [RemoteSession] = []
        let failure = await model.createTerminal(try request(.claude)) { created.append($0) }
        XCTAssertNil(failure)
        let sent = await rig.transport.params(of: "shell.create")
        XCTAssertEqual(sent, [["kind": .string("claude"), "project_id": .string(project)]])
        let createdIDs = await rig.transport.created
        let newID = try XCTUnwrap(createdIDs.first)
        XCTAssertEqual(created.map(\.id), [newID])
        XCTAssertEqual(model.sessionID, newID, "switched straight to the new terminal")
        XCTAssertEqual(model.shells.first { $0.id == newID }?.harness, "claude")
        XCTAssertEqual(model.outputSessionID, newID, "and its first screen was read")
        XCTAssertEqual(model.lastTerminalKind, .claude)
        XCTAssertEqual(rig.defaults.string(forKey: RemoteModel.newTerminalKindKey), "claude")
        XCTAssertFalse(model.creatingTerminal)
        XCTAssertEqual(model.terminalControl, .supported)
        // The choice survives a restart.
        let restarted = RemoteModel(client: SpawnTransport(), keychain: rig.keychain, defaults: rig.defaults)
        XCTAssertEqual(restarted.lastTerminalKind, .claude)
        XCTAssertEqual(restarted.desktop?.projectSessionIDs?[project], newID)
        await model.disconnect()
    }
    func testAnUnrestrictedAgentIsAskedForExplicitlyAndNothingElseIs() async throws {
        let rig = try await connected()
        _ = await rig.model.createTerminal(try request(.codex, unrestricted: true))
        _ = await rig.model.createTerminal(try request(.grok))
        _ = await rig.model.createTerminal(try request(.shell))
        let sent = await rig.transport.params(of: "shell.create")
        XCTAssertEqual(sent.map { $0["unrestricted"] }, [.bool(true), nil, nil])
        XCTAssertEqual(sent.map { $0["kind"]?.string }, ["codex", "grok", "shell"])
        await rig.model.disconnect()
    }
    func testAnUnsetOrUnknownRememberedKindFallsBackToShell() async throws {
        let suite = defaults()
        suite.set("bash", forKey: RemoteModel.newTerminalKindKey)
        let rig = try await connected(defaults: suite)
        XCTAssertEqual(rig.model.lastTerminalKind, .shell)
        suite.set("grok", forKey: RemoteModel.newTerminalKindKey)
        XCTAssertEqual(rig.model.lastTerminalKind, .grok)
        XCTAssertEqual(rig.model.newTerminalForm()?.kind, .grok, "the sheet opens on it")
        XCTAssertEqual(rig.model.newTerminalForm()?.unrestricted, false, "and never on unrestricted")
        await rig.model.disconnect()
    }
    func testASecondRequestWhileOneIsOnTheWireIsRefusedNotSent() async throws {
        let rig = try await connected()
        await rig.transport.setGated(true)
        let model = rig.model
        let shellRequest = try request(.shell)
        let one = Task { await model.createTerminal(shellRequest) }
        await settle { model.creatingTerminal }
        let refused = await model.createTerminal(try request(.codex))
        XCTAssertEqual(refused, .busy)
        await rig.transport.setGated(false)
        let result = await one.value
        XCTAssertNil(result)
        let count = await rig.transport.count("shell.create")
        XCTAssertEqual(count, 1)
        await model.disconnect()
    }
    func testTheSheetSendsOnlyOneRequestForAHeldReturnOrADoubleTap() async throws {
        let rig = try await connected()
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        var dismissed = 0
        sheet.dismiss = { dismissed += 1 }
        await rig.transport.setGated(true)
        sheet.create(); sheet.create(); sheet.create()
        XCTAssertTrue(sheet.busy)
        XCTAssertFalse(sheet.canCreate, "the button is off while it runs")
        await rig.transport.setGated(false)
        await sheet.pending?.value
        let count = await rig.transport.count("shell.create")
        XCTAssertEqual(count, 1)
        XCTAssertGreaterThanOrEqual(dismissed, 1)
        XCTAssertNil(sheet.error)
        XCTAssertFalse(sheet.busy)
        await rig.model.disconnect()
    }
    func testFailuresKeepTheSheetOpenWithTheReason() async throws {
        let rig = try await connected()
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        var dismissed = 0
        sheet.dismiss = { dismissed += 1 }
        let before = rig.model.sessionID
        func attempt(_ mode: SpawnTransport.CreateMode) async -> String? {
            await rig.transport.setCreateMode(mode)
            sheet.create()
            await sheet.pending?.value
            return sheet.error?.message
        }
        sheet.select(kind: .codex)
        let harness = await attempt(.harness)
        XCTAssertEqual(harness, "Codex isn’t installed on the Mac (or isn’t on its PATH).")
        let missing = await attempt(.notFound)
        XCTAssertEqual(missing, "That project or worktree no longer exists on the Mac. Refresh and try again.")
        let other = await attempt(.cliError("tmux: no server running"))
        XCTAssertEqual(other, "tmux: no server running")
        let exited = await attempt(.exited)
        XCTAssertEqual(exited, "The terminal started but exited right away. Check it on the Mac.")
        let unreadable = await attempt(.unreadable)
        XCTAssertEqual(unreadable, TerminalControlError.unreadableReply.message)
        XCTAssertEqual(dismissed, 0, "errors stay in the sheet")
        XCTAssertEqual(rig.model.sessionID, before, "nothing was selected")
        XCTAssertFalse(sheet.busy)
        XCTAssertNotEqual(rig.model.lastTerminalKind, .codex, "only a success is remembered")
        // Choosing something else takes the old message down.
        sheet.select(kind: .shell)
        XCTAssertNil(sheet.error)
        await rig.model.disconnect()
    }
    func testAnOlderDesktopDisablesFurtherAttemptsAndAReconnectTriesAgain() async throws {
        let rig = try await connected()
        let model = rig.model
        XCTAssertTrue(model.canOpenNewTerminal)
        XCTAssertEqual(model.terminalControl, .unknown)
        await rig.transport.setCreateMode(.unsupported)
        let failure = await model.createTerminal(try request())
        XCTAssertEqual(failure, .unsupported)
        XCTAssertEqual(failure?.message, "Update RiWork on your Mac to open terminals from the phone.")
        XCTAssertEqual(model.terminalControl, .unsupported)
        XCTAssertFalse(model.canOpenNewTerminal, "⌘N does nothing")
        XCTAssertEqual(model.terminalControlNotice, TerminalControlError.unsupportedMessage)
        // No more requests go out, and the sheet explains instead.
        let again = await model.createTerminal(try request())
        XCTAssertEqual(again, .unsupported)
        let sent = await rig.transport.count("shell.create")
        XCTAssertEqual(sent, 1)
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: model))
        XCTAssertEqual(sheet.message, .unsupported)
        XCTAssertFalse(sheet.canCreate)
        sheet.create()
        let afterSheet = await rig.transport.count("shell.create")
        XCTAssertEqual(afterSheet, 1)
        XCTAssertFalse(model.canClose(try session(first)), "closing needs the same support")
        // A new connection may reach an upgraded desktop.
        await model.disconnect()
        await rig.transport.setCreateMode(.ok)
        await model.connect()
        XCTAssertEqual(model.terminalControl, .unknown)
        XCTAssertTrue(model.canOpenNewTerminal)
        let retried = await model.createTerminal(try request())
        XCTAssertNil(retried)
        XCTAssertEqual(model.terminalControl, .supported)
        await model.disconnect()
    }
    func testALostAnswerLeavesTheOutcomeUnknownAndIsNeverRetried() async throws {
        let rig = try await connected()
        await rig.transport.setCreateMode(.timeout)
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.create()
        await sheet.pending?.value
        XCTAssertEqual(sheet.error, .outcomeUnknown(.create(.shell)))
        XCTAssertEqual(sheet.error?.outcomeIsUncertain, true)
        XCTAssertTrue(sheet.error?.message.contains("Check the terminal list before trying again") == true)
        // Nothing sends it again by itself: not a reconnect, not time.
        try? await Task.sleep(for: .milliseconds(300))
        let count = await rig.transport.count("shell.create")
        XCTAssertEqual(count, 1)
        let createdIDs = await rig.transport.created
        XCTAssertTrue(createdIDs.allSatisfy { $0 != rig.model.sessionID })
        await rig.model.disconnect()
    }
    func testNothingIsSentWhileDisconnectedOrWithoutAProject() async throws {
        let rig = try await connected()
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        await rig.model.disconnect()
        XCTAssertFalse(rig.model.canOpenNewTerminal)
        XCTAssertFalse(sheet.canCreate)
        let failure = await rig.model.createTerminal(try request())
        XCTAssertEqual(failure, .notConnected)
        let count = await rig.transport.count("shell.create")
        XCTAssertEqual(count, 0)
        let bare = RemoteModel(client: SpawnTransport(), keychain: KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)"), defaults: defaults())
        XCTAssertNil(bare.newTerminalForm())
        XCTAssertNil(NewTerminalSheetModel(model: bare))
    }
    func testTheSheetPreselectsTheWorktreeOfTheTerminalOnScreen() async throws {
        let main = try worktree("66666666-6666-4666-8666-666666666666", "main", primary: true)
        let feature = try worktree(tree, "feature", primary: false)
        let onFeature = try session(first, worktree: tree)
        let rig = try await connected([onFeature], worktrees: [feature, main])
        let form = try XCTUnwrap(rig.model.newTerminalForm())
        XCTAssertEqual(form.targets.map(\.id), ["worktree:\(main.id)", "worktree:\(feature.id)"])
        XCTAssertEqual(form.target?.worktreeID, tree, "the worktree being looked at")
        XCTAssertEqual(try form.request().params["worktree_id"], .string(tree))
        // Looking at an orchestrator or nothing: the main worktree.
        let rigMain = try await connected([try session(second, kind: "orchestrator")], worktrees: [feature, main])
        XCTAssertEqual(rigMain.model.newTerminalForm()?.target?.worktreeID, main.id)
        await rig.model.disconnect(); await rigMain.model.disconnect()
    }
    func testWorktreesThatArriveUnderAnOpenSheetKeepWhatWasChosen() async throws {
        let rig = try await connected()
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        XCTAssertEqual(sheet.form.targets.map(\.id), ["project:\(project)"])
        sheet.select(kind: .grok)
        rig.model.worktrees = [try worktree(tree, "feature", primary: false), try worktree("66666666-6666-4666-8666-666666666666", "main", primary: true)]
        sheet.refreshTargets()
        XCTAssertEqual(sheet.form.targets.count, 2)
        XCTAssertEqual(sheet.form.target?.branchLabel, "main · root", "the main worktree, now that it is known")
        XCTAssertEqual(sheet.form.kind, .grok)
        sheet.select(targetAt: 1)
        sheet.refreshTargets()
        XCTAssertEqual(sheet.form.target?.worktreeID, tree, "a choice that still exists stays")
        await rig.model.disconnect()
    }
    // MARK: Closing

    func testClosingAskedTerminalMovesToTheNeighbourFirstThenRemovesIt() async throws {
        let a = try session(first, at: 3), b = try session(second, at: 2), c = try session("66666666-6666-4666-8666-666666666666", at: 1)
        let rig = try await connected([a, b, c])
        let model = rig.model
        XCTAssertEqual(model.openSessions.map(\.id), [a.id, b.id, c.id])
        await model.chooseSession(b)
        XCTAssertTrue(model.canClose(b))
        let failure = await model.closeTerminal(b)
        XCTAssertNil(failure)
        XCTAssertEqual(model.shells.map(\.id), [a.id, c.id])
        XCTAssertEqual(model.sessionID, c.id, "the next tab")
        let operations = await rig.transport.methods()
        let closeIndex = try XCTUnwrap(operations.firstIndex(of: "shell.close"))
        let outputAfterSwitch = operations[..<closeIndex].lastIndex(of: "shell.output")
        XCTAssertNotNil(outputAfterSwitch, "the neighbour was selected before the close went out")
        let sent = await rig.transport.params(of: "shell.close")
        XCTAssertEqual(sent, [["shell_id": .string(b.id)]])
        XCTAssertNil(model.error)
        XCTAssertNil(model.closingTerminalID)
        // The last tab falls back to the one before.
        let again = await model.closeTerminal(c)
        XCTAssertNil(again)
        XCTAssertEqual(model.sessionID, a.id)
        await model.disconnect()
    }
    func testClosingTheOnlyTerminalClearsTheSelection() async throws {
        let rig = try await connected([try session(first)])
        let model = rig.model
        XCTAssertEqual(model.sessionID, first)
        let failure = await model.closeTerminal(try session(first))
        XCTAssertNil(failure)
        XCTAssertNil(model.sessionID)
        XCTAssertTrue(model.shells.isEmpty)
        XCTAssertNil(model.viewportSessionID)
        await model.disconnect()
    }
    func testClosingAnotherTabLeavesTheSelectionAlone() async throws {
        let a = try session(first, at: 2), b = try session(second, at: 1)
        let rig = try await connected([a, b])
        let failure = await rig.model.closeTerminal(b)
        XCTAssertNil(failure)
        XCTAssertEqual(rig.model.sessionID, a.id)
        XCTAssertEqual(rig.model.shells.map(\.id), [a.id])
        await rig.model.disconnect()
    }
    func testOrchestratorsAreNotClosedFromThePhone() async throws {
        let orchestrator = try session(second, kind: "orchestrator")
        let rig = try await connected([try session(first)])
        XCTAssertFalse(rig.model.canClose(orchestrator))
        let failure = await rig.model.closeTerminal(orchestrator)
        XCTAssertNotNil(failure)
        let count = await rig.transport.count("shell.close")
        XCTAssertEqual(count, 0)
        await rig.model.disconnect()
    }
    func testAFailedCloseKeepsTheTerminalAndPutsBackTheSelection() async throws {
        let a = try session(first, at: 2), b = try session(second, at: 1)
        let rig = try await connected([a, b])
        await rig.transport.setCloseMode(.cliError("tmux timed out"))
        let failure = await rig.model.closeTerminal(a)
        XCTAssertEqual(failure, .failed("tmux timed out"))
        XCTAssertEqual(rig.model.shells.map(\.id).sorted(), [a.id, b.id].sorted())
        XCTAssertEqual(rig.model.sessionID, a.id, "back where it was")
        // A lost answer is uncertain and says so.
        await rig.transport.setCloseMode(.timeout)
        let lost = await rig.model.closeTerminal(a)
        XCTAssertEqual(lost, .outcomeUnknown(.close))
        XCTAssertTrue(lost?.outcomeIsUncertain == true)
        let attempts = await rig.transport.count("shell.close")
        XCTAssertEqual(attempts, 2, "one per tap, never retried")
        await rig.model.disconnect()
    }
    func testClosingATerminalThatIsAlreadyGoneIsDone() async throws {
        let a = try session(first, at: 2), b = try session(second, at: 1)
        let rig = try await connected([a, b])
        await rig.transport.setCloseMode(.notFound)
        let failure = await rig.model.closeTerminal(b)
        XCTAssertNil(failure)
        XCTAssertEqual(rig.model.shells.map(\.id), [a.id])
        await rig.model.disconnect()
    }
    func testClosingOnAnOlderDesktopSaysSo() async throws {
        let rig = try await connected([try session(first, at: 2), try session(second, at: 1)])
        await rig.transport.setCloseMode(.unsupported)
        let failure = await rig.model.closeTerminal(try session(second))
        XCTAssertEqual(failure, .unsupported)
        XCTAssertEqual(rig.model.terminalControl, .unsupported)
        XCTAssertEqual(rig.model.shells.count, 2)
        XCTAssertFalse(rig.model.canOpenNewTerminal, "creating is out too: it is the same release")
        await rig.model.disconnect()
    }
    func testClosingDropsTheKeysStillWaitingForThatTerminal() async throws {
        let a = try session(first, at: 2), b = try session(second, at: 1)
        let rig = try await connected([a, b])
        let key = KeyBufferKey(desktopID: try XCTUnwrap(rig.model.selectedDesktopID), shellID: b.id)
        var buffer = KeyBuffer()
        XCTAssertTrue(buffer.append([.text("ls")], now: Date()))
        rig.model.keyBuffers[key] = buffer
        let failure = await rig.model.closeTerminal(b)
        XCTAssertNil(failure)
        XCTAssertNil(rig.model.keyBuffers[key])
        await rig.model.disconnect()
    }

    // MARK: Keyboard

    private func window(with view: UIView) -> UIWindow {
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 480))
        window.rootViewController = UIViewController()
        window.rootViewController?.view.addSubview(view)
        window.makeKeyAndVisible()
        retained.append(window)
        return window
    }
    private var retained: [UIWindow] = []

    func testTheSheetKeysAreArrowsTabSpaceReturnAndEscapeWithPriority() throws {
        let view = NewTerminalKeyView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        let commands = try XCTUnwrap(view.keyCommands)
        func command(_ input: String, _ flags: UIKeyModifierFlags = []) -> UIKeyCommand? { commands.first { $0.input == input && $0.modifierFlags == flags } }
        let expected: [(String, UIKeyModifierFlags)] = [(UIKeyCommand.inputUpArrow, []), (UIKeyCommand.inputDownArrow, []), (UIKeyCommand.inputLeftArrow, []),
                                                        (UIKeyCommand.inputRightArrow, []), ("\t", []), ("\t", .shift), (" ", []), ("\r", []), (UIKeyCommand.inputEscape, [])]
        for (input, flags) in expected {
            let found = try XCTUnwrap(command(input, flags), "\(input.debugDescription) \(flags)")
            XCTAssertTrue(found.wantsPriorityOverSystemBehavior, "the system must not use \(input.debugDescription) first")
        }
        XCTAssertEqual(commands.count, 9)
        XCTAssertFalse(commands.contains { $0.modifierFlags.contains(.command) }, "⌘ combinations are left to the app")
        XCTAssertTrue(view.canBecomeFirstResponder)
        XCTAssertFalse(view is UIKeyInput, "no software keyboard comes up for it")
    }
    func testEachKeyReachesTheSheetAsWhatItMeans() throws {
        let view = NewTerminalKeyView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        var received: [NewTerminalSheetKey] = []
        view.onKey = { received.append($0) }
        let table: [(String, UIKeyModifierFlags, NewTerminalSheetKey)] = [
            (UIKeyCommand.inputUpArrow, [], .form(.up)), (UIKeyCommand.inputDownArrow, [], .form(.down)),
            (UIKeyCommand.inputLeftArrow, [], .form(.left)), (UIKeyCommand.inputRightArrow, [], .form(.right)),
            ("\t", [], .form(.tab)), ("\t", .shift, .form(.backTab)), (" ", [], .form(.space)),
            ("\r", [], .create), (UIKeyCommand.inputEscape, [], .cancel)
        ]
        for (input, flags, _) in table {
            view.fired(UIKeyCommand(input: input, modifierFlags: flags, action: #selector(NewTerminalKeyView.fired(_:))))
        }
        XCTAssertEqual(received, table.map(\.2))
        // Anything else is ignored.
        view.fired(UIKeyCommand(input: "x", modifierFlags: .command, action: #selector(NewTerminalKeyView.fired(_:))))
        XCTAssertEqual(received.count, table.count)
    }
    func testTheSheetTakesTheKeyboardAsSoonAsItIsOnScreen() async throws {
        let view = NewTerminalKeyView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        _ = window(with: view)
        await settle { view.isFirstResponder }
        // And again after something else (a menu) had it.
        _ = view.resignFirstResponder()
        XCTAssertFalse(view.isFirstResponder)
        view.takeFocus()
        await settle { view.isFirstResponder }
    }
    func testKeysDriveTheSheetModelEndToEnd() async throws {
        let rig = try await connected()
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        var dismissed = 0
        sheet.dismiss = { dismissed += 1 }
        XCTAssertFalse(sheet.keyboardInUse)
        sheet.press(.down)                                   // Shell -> Codex
        XCTAssertEqual(sheet.form.kind, .codex)
        XCTAssertTrue(sheet.keyboardInUse)
        sheet.press(.tab)                                    // kind -> unrestricted toggle (agents have one)
        XCTAssertEqual(sheet.form.focus, .unrestricted)
        sheet.press(.space)
        XCTAssertTrue(sheet.form.unrestricted)
        sheet.press(.tab)                                    // -> Create
        XCTAssertEqual(sheet.form.focus, .create)
        sheet.press(.space)                                  // space presses the focused Create button
        await sheet.pending?.value
        let sent = await rig.transport.params(of: "shell.create")
        XCTAssertEqual(sent, [["kind": .string("codex"), "project_id": .string(project), "unrestricted": .bool(true)]])
        XCTAssertGreaterThanOrEqual(dismissed, 1)
        // Escape is Cancel.
        let cancelled = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        var closed = false
        cancelled.dismiss = { closed = true }
        cancelled.cancel()
        XCTAssertTrue(closed)
        let count = await rig.transport.count("shell.create")
        XCTAssertEqual(count, 1)
        await rig.model.disconnect()
    }
    func testTouchTakesTheFocusRingAwayAndChoosesLikeTheKeyboard() async throws {
        let rig = try await connected()
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        sheet.press(.down)
        XCTAssertTrue(sheet.keyboardInUse)
        sheet.select(kind: .grok)
        XCTAssertFalse(sheet.keyboardInUse)
        XCTAssertEqual(sheet.form.kind, .grok)
        sheet.setUnrestricted(true)
        XCTAssertTrue(sheet.form.unrestricted)
        sheet.select(kind: .shell)
        XCTAssertFalse(sheet.form.unrestricted)
        await rig.model.disconnect()
    }

    // MARK: ⌘N

    func testTheTerminalsKeyViewLeavesCommandNToTheApp() throws {
        let view = KeyCaptureView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        let commands = try XCTUnwrap(view.keyCommands)
        XCTAssertFalse(commands.contains { $0.input == "n" && $0.modifierFlags.contains(.command) }, "⌘N is not claimed, so it reaches the hosting controller")
        view.togglePalette()
        XCTAssertTrue(try XCTUnwrap(view.keyCommands).contains { $0.input == "n" && $0.modifierFlags == .command }, "only the open hotkey menu takes ⌘N")
        view.togglePalette()
        XCTAssertFalse(try XCTUnwrap(view.keyCommands).contains { $0.input == "n" && $0.modifierFlags.contains(.command) })
        XCTAssertFalse(view.canPerformAction(NSSelectorFromString("newTerminal:"), withSender: nil))
    }
    func testCommandNIsAKeyCommandOfTheHostingController() async throws {
        var fired = 0
        let host = UIHostingController(rootView: Button("New terminal") { fired += 1 }.keyboardShortcut("n", modifiers: .command))
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 480))
        window.rootViewController = host
        window.makeKeyAndVisible()
        retained.append(window)
        host.view.layoutIfNeeded()
        try? await Task.sleep(for: .milliseconds(100))
        // SwiftUI registers a button's shortcut as a key command on its hosting controller.
        let found = (host.keyCommands ?? []).contains { $0.input?.lowercased() == "n" && $0.modifierFlags == .command }
        XCTAssertTrue(found, "keyCommands: \((host.keyCommands ?? []).map { "\($0.input ?? "") \($0.modifierFlags.rawValue)" })")
        XCTAssertEqual(fired, 0)
    }
}
