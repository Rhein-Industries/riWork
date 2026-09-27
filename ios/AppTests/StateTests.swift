import XCTest
import RiWorkCore
@testable import RiWorkRemote

private actor FixtureTransport: RemoteTransport {
    var connected = false
    var connections = 0
    var inputs: [String] = []
    var inputUncertain = false
    var events: [String] = []
    var inputLines: [String] = []
    var listedShells: [RemoteSession]?
    var missingOutputs: Set<String> = []
    func setSessions(_ sessions: [RemoteSession]) { listedShells = sessions }
    func setMissing(_ id: String) { missingOutputs.insert(id) }
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws { connected = true; connections += 1 }
    func disconnect() async { connected = false }
    func isConnected() async -> Bool { connected }
    func setUncertain() { inputUncertain = true }
    func counts() -> (Int, [String]) { (connections, inputs) }
    func operations() -> [String] { events }
    func lines() -> [String] { inputLines }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        guard connected else { throw RemoteError.disconnected }
        try RequestValidation.validate(method: method, params: params, id: id)
        events.append("\(method):\(params["shell_id"]?.string ?? "")")
        let raw: String
        switch method {
        case "projects.list": raw = "{\"projects\":[{\"id\":\"11111111-1111-4111-8111-111111111111\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}]}"
        case "worktrees.list": raw = "{\"worktrees\":[]}"
        case "tasks.list": raw = "{\"tasks\":[]}"
        case "orchestrators.list": raw = "{\"orchestrators\":[]}"
        case "shells.list":
            if let listedShells {
                let encoded = try JSONEncoder().encode(listedShells)
                return .object(["shells": try JSONDecoder().decode(JSONValue.self, from: encoded)])
            }
            raw = "{\"shells\":[{\"id\":\"44444444-4444-4444-8444-444444444444\",\"project_id\":\"11111111-1111-4111-8111-111111111111\",\"worktree_id\":null,\"kind\":\"project\",\"cwd\":\"/fixture\",\"harness\":\"codex\",\"alive\":true,\"created_at_unix\":1}]}"
        case "shell.output":
            if let shell = params["shell_id"]?.string, missingOutputs.contains(shell) {
                throw RemoteError.rpc(code: "not_found", message: "Selected session is unavailable.")
            }
            return .object(["shell_id": params["shell_id"]!, "output": .string("existing session output")])
        case "shell.input":
            inputs.append(id)
            inputLines.append(params["line"]!.string!)
            if inputUncertain { connected = false; throw RemoteError.uncertainDelivery }
            return .object(["shell_id": params["shell_id"]!, "status": .string("sent")])
        case "shell.resize": return .object(["shell_id": params["shell_id"]!, "columns": params["columns"]!, "rows": params["rows"]!])
        case "shell.resize.clear": return .object(["shell_id": params["shell_id"]!, "status": .string("cleared")])
        default: throw RemoteError.protocolViolation("Unknown method")
        }
        return try JSONDecoder().decode(JSONValue.self, from: Data(raw.utf8))
    }
}

@MainActor final class StateTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let shell = "44444444-4444-4444-8444-444444444444"
    private func makeStore() throws -> KeychainStore {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = shell
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        return keychain
    }
    func testBackgroundReconnectPreservesExistingSelectionAndMarksOutputStale() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let model = RemoteModel(client: transport, keychain: keychain)
        await model.connect()
        XCTAssertEqual(model.state, .connected)
        XCTAssertEqual(model.output, "existing session output")
        await model.disconnect(background: true)
        XCTAssertEqual(model.state, .suspended)
        XCTAssertTrue(model.snapshotStale)
        XCTAssertEqual(model.output, "existing session output")
        XCTAssertEqual(model.sessionID, shell)
        XCTAssertFalse(model.canSend)
        await model.resume()
        XCTAssertEqual(model.state, .connected)
        XCTAssertFalse(model.snapshotStale)
        XCTAssertEqual(model.sessionID, shell)
        let counts = await transport.counts()
        XCTAssertEqual(counts.0, 2)
        XCTAssertEqual(counts.1.count, 0)
        await model.disconnect()
        await model.resume()
        XCTAssertEqual(model.state, .disconnected)
    }
    func testUncertainSubmissionPersistsUUIDAndNeverResendsOnReconnectOrRestart() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let model = RemoteModel(client: transport, keychain: keychain)
        await model.connect()
        await transport.setUncertain()
        model.draft = "continue the existing work"
        await model.submit()
        let pending = try XCTUnwrap(model.pendingInput)
        XCTAssertEqual(pending.line, "continue the existing work")
        XCTAssertEqual(pending.shellID, shell)
        XCTAssertFalse(model.canSend)
        await model.disconnect()
        let restored = RemoteModel(client: transport, keychain: keychain)
        XCTAssertEqual(restored.pendingInput, pending)
        XCTAssertEqual(restored.sessionID, shell)
        await restored.connect()
        XCTAssertEqual(restored.pendingInput?.id, pending.id)
        let counts = await transport.counts()
        XCTAssertEqual(counts.1, [pending.id])
        try restored.acknowledgeUncertainInput()
        XCTAssertNil(restored.pendingInput)
        let acknowledgedCounts = await transport.counts()
        XCTAssertEqual(acknowledgedCounts.1, [pending.id])
        await restored.disconnect()
    }
    func testAcknowledgedSubmissionClearsPendingAndCannotSubmitMultiline() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let model = RemoteModel(client: transport, keychain: keychain)
        await model.connect()
        model.draft = "first\nsecond"
        await model.submit()
        XCTAssertNil(model.pendingInput)
        let before = await transport.counts()
        XCTAssertEqual(before.1.count, 0)
        model.draft = "one explicit line"
        await model.submit()
        XCTAssertNil(model.pendingInput)
        XCTAssertEqual(model.draft, "")
        let after = await transport.counts()
        XCTAssertEqual(after.1.count, 1)
        await model.disconnect()
    }
    func testViewportResizesBeforeOutputClearsAndReappliesAfterReconnect() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let model = RemoteModel(client: transport, keychain: keychain)
        model.reportViewport(TerminalViewport(columns: 43, rows: 17))
        model.setTerminalVisible(true)
        await model.connect()
        XCTAssertTrue(model.viewportReady)
        XCTAssertEqual(model.viewportSessionID, shell)
        let first = await transport.operations()
        let resize = try XCTUnwrap(first.firstIndex(of: "shell.resize:\(shell)"))
        let output = try XCTUnwrap(first.firstIndex(of: "shell.output:\(shell)"))
        XCTAssertLessThan(resize, output)
        model.reportViewport(TerminalViewport(columns: 43, rows: 8))
        XCTAssertTrue(model.canEditDraft, "Resizing must keep keyboard focus; disabling the field causes a geometry feedback loop")
        XCTAssertFalse(model.canSend, "Do not submit until the new grid has been acknowledged")
        await model.readOutput()
        XCTAssertTrue(model.canSend)
        await model.disconnect(background: true)
        XCTAssertNil(model.appliedViewport)
        let paused = await transport.operations()
        XCTAssertTrue(paused.contains("shell.resize.clear:\(shell)"))
        await model.resume()
        XCTAssertEqual(model.sessionID, shell)
        XCTAssertTrue(model.viewportReady)
        let reconnected = await transport.operations()
        XCTAssertEqual(reconnected.filter { $0 == "shell.resize:\(shell)" }.count, 3)
        let inputCount = await transport.counts()
        XCTAssertEqual(inputCount.1.count, 0)
        await model.disconnect()
    }
    func testDirectSendUsesCapturedLineAndRejectsChangedSelection() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let model = RemoteModel(client: transport, keychain: keychain)
        await model.connect()
        model.draft = "edit made after Send"
        await model.submit(expectedSessionID: "55555555-5555-4555-8555-555555555555", line: "line captured on Send")
        let rejected = await transport.counts()
        XCTAssertTrue(rejected.1.isEmpty)
        await model.submit(expectedSessionID: shell, line: "line captured on Send")
        let submitted = await transport.lines()
        XCTAssertEqual(submitted, ["line captured on Send"])
        await model.disconnect()
    }
    func testTabSwitchRestoresPreviousShellBeforeResizingAndReadingChosenShell() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let model = RemoteModel(client: transport, keychain: keychain)
        model.reportViewport(TerminalViewport(columns: 43, rows: 17))
        model.setTerminalVisible(true)
        await model.connect()
        let other = try JSONDecoder().decode(RemoteSession.self, from: Data("""
        {"id":"55555555-5555-4555-8555-555555555555","project_id":"11111111-1111-4111-8111-111111111111","kind":"project","cwd":"/fixture","alive":true,"created_at_unix":2}
        """.utf8))
        model.shells.append(other)
        let before = await transport.operations().count
        await model.chooseSession(other)
        let switched = Array(await transport.operations().dropFirst(before))
        XCTAssertEqual(switched.prefix(3), ["shell.resize.clear:\(shell)", "shell.resize:\(other.id)", "shell.output:\(other.id)"])
        XCTAssertEqual(model.outputSessionID, other.id)
        XCTAssertTrue(model.viewportReady)
        let restored = RemoteModel(client: transport, keychain: keychain)
        XCTAssertEqual(restored.sessionID, other.id)
        XCTAssertEqual(restored.desktop?.projectSessionIDs?[project], other.id)
        let inputs = await transport.counts()
        XCTAssertTrue(inputs.1.isEmpty)
        await model.disconnect()
    }
    private func session(_ id: String, alive: Bool = true) throws -> RemoteSession {
        try JSONDecoder().decode(RemoteSession.self, from: Data("""
        {"id":"\(id)","project_id":"\(project)","kind":"project","cwd":"/fixture","alive":\(alive),"created_at_unix":2}
        """.utf8))
    }
    func testRestoredAbsentSelectionFallsBackAndPersistsWithoutInput() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let other = try session("55555555-5555-4555-8555-555555555555")
        await transport.setSessions([other])
        let model = RemoteModel(client: transport, keychain: keychain)
        model.draft = "belongs to the absent tab"
        model.deliveryNotice = "old notice"
        model.output = "old output"; model.outputSessionID = shell; model.lastOutputAt = Date()
        await model.connect()
        XCTAssertEqual(model.sessionID, other.id)
        XCTAssertEqual(model.outputSessionID, other.id)
        XCTAssertEqual(model.draft, "")
        XCTAssertNil(model.deliveryNotice)
        XCTAssertTrue(model.canSend)
        let restored = RemoteModel(client: transport, keychain: keychain)
        XCTAssertEqual(restored.sessionID, other.id)
        XCTAssertEqual(restored.desktop?.projectSessionIDs?[project], other.id)
        let operations = await transport.operations()
        XCTAssertFalse(operations.contains("shell.output:\(shell)"))
        XCTAssertFalse(operations.contains(where: { $0.hasPrefix("shell.input:") }))
        await model.disconnect()
    }
    func testClosedLastTabClearsSelectionDraftOutputAndViewportRetainsPending() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let model = RemoteModel(client: transport, keychain: keychain)
        model.reportViewport(TerminalViewport(columns: 43, rows: 17)); model.setTerminalVisible(true)
        await model.connect()
        let pending = try PendingInput(shellID: shell, line: "unconfirmed original input")
        model.desktops[0].pendingInput = pending; try model.persist()
        model.draft = "old tab draft"; model.deliveryNotice = "old tab notice"
        await transport.setSessions([try session(shell, alive: false)])
        await model.refresh()
        XCTAssertNil(model.sessionID)
        XCTAssertNil(model.desktop?.projectSessionIDs?[project])
        XCTAssertEqual(model.output, ""); XCTAssertNil(model.outputSessionID); XCTAssertNil(model.lastOutputAt)
        XCTAssertEqual(model.draft, ""); XCTAssertNil(model.deliveryNotice)
        XCTAssertNil(model.viewportSessionID); XCTAssertNil(model.appliedViewport)
        XCTAssertFalse(model.canSend)
        XCTAssertEqual(model.pendingInput, pending)
        let restored = RemoteModel(client: transport, keychain: keychain)
        XCTAssertNil(restored.sessionID); XCTAssertEqual(restored.pendingInput, pending)
        let operations = await transport.operations()
        XCTAssertTrue(operations.contains("shell.resize.clear:\(shell)"))
        XCTAssertFalse(operations.contains(where: { $0.hasPrefix("shell.input:") }))
        await model.disconnect()
    }
    func testNotFoundRefreshesProjectOnceFallsBackWithoutResendingPending() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let model = RemoteModel(client: transport, keychain: keychain)
        await model.connect()
        let other = try session("55555555-5555-4555-8555-555555555555")
        // A raced or stale list can still claim the missing UUID is alive.
        await transport.setSessions([try session(shell), other])
        await transport.setMissing(shell)
        let pending = try PendingInput(shellID: shell, line: "pending on missing shell")
        model.desktops[0].pendingInput = pending; try model.persist()
        model.draft = "old draft"; model.deliveryNotice = "old notice"
        let before = await transport.operations().count
        await model.readOutput()
        for _ in 0..<3 { await model.readOutput() }
        XCTAssertEqual(model.sessionID, other.id); XCTAssertEqual(model.outputSessionID, other.id)
        XCTAssertEqual(model.draft, ""); XCTAssertNil(model.deliveryNotice)
        XCTAssertEqual(model.pendingInput, pending); XCTAssertFalse(model.canSend)
        XCTAssertEqual(model.openSessions.map(\.id), [other.id])
        let operations = Array(await transport.operations().dropFirst(before))
        XCTAssertEqual(operations.filter { $0 == "shells.list:" }.count, 1)
        XCTAssertEqual(operations.filter { $0 == "orchestrators.list:" }.count, 1)
        XCTAssertEqual(operations.filter { $0 == "shell.output:\(shell)" }.count, 1)
        XCTAssertFalse(operations.contains(where: { $0.hasPrefix("shell.input:") }))
        await model.disconnect()
    }
    func testNotFoundLastTabIsNotRepeatedlyPolled() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.setMissing(shell)
        let model = RemoteModel(client: transport, keychain: keychain)
        await model.connect()
        for _ in 0..<3 { await model.readOutput() }
        XCTAssertEqual(model.state, .connected)
        XCTAssertNil(model.sessionID); XCTAssertTrue(model.openSessions.isEmpty)
        XCTAssertEqual(model.output, ""); XCTAssertFalse(model.canSend)
        let operations = await transport.operations()
        XCTAssertEqual(operations.filter { $0 == "shells.list:" }.count, 2, "Initial listing plus one recovery refresh")
        XCTAssertEqual(operations.filter { $0 == "shell.output:\(shell)" }.count, 1)
        XCTAssertFalse(operations.contains(where: { $0.hasPrefix("shell.input:") }))
        await model.disconnect()
    }

}
