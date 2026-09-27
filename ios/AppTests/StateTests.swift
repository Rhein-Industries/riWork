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
        case "shells.list": raw = "{\"shells\":[{\"id\":\"44444444-4444-4444-8444-444444444444\",\"project_id\":\"11111111-1111-4111-8111-111111111111\",\"worktree_id\":null,\"kind\":\"project\",\"cwd\":\"/fixture\",\"harness\":\"codex\",\"alive\":true,\"created_at_unix\":1}]}"
        case "shell.output": return .object(["shell_id": params["shell_id"]!, "output": .string("existing session output")])
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
    func testSubmissionUsesReviewedLineAndRejectsChangedSelection() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let model = RemoteModel(client: transport, keychain: keychain)
        await model.connect()
        model.draft = "unreviewed edit"
        await model.submit(expectedSessionID: "55555555-5555-4555-8555-555555555555", line: "reviewed line")
        let rejected = await transport.counts()
        XCTAssertTrue(rejected.1.isEmpty)
        await model.submit(expectedSessionID: shell, line: "reviewed line")
        let submitted = await transport.lines()
        XCTAssertEqual(submitted, ["reviewed line"])
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
}
