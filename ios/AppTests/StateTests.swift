import XCTest
import RiWorkCore
@testable import RiWorkRemote

actor FixtureTransport: RemoteTransport {
    var connected = false
    var connections = 0
    var inputs: [String] = []
    var inputUncertain = false
    var events: [String] = []
    var inputLines: [String] = []
    var listedShells: [RemoteSession]?
    var missingOutputs: Set<String> = []
    var outputLineRequests: [Int] = []
    var oversizeAbove: Int?
    var oversizeCode = "response_too_large"
    var blockedMethod: String?
    var blockedCount = 0
    // shell.keys behaviour, like the desktop's batch ledger.
    enum KeysMode { case ok, unsupported, inputUnavailable, notFound, uncertain, dropLinkAfterDelivery }
    struct KeyCall { let request: String; let batch: String; let shell: String; let items: [JSONValue] }
    var keysMode = KeysMode.ok
    var keysDelay: Duration?
    var keyCalls: [KeyCall] = []
    var seenBatches: Set<String> = []
    var resizes: [TerminalViewport] = []
    // appearance.get behaviour, like the desktop's: a palette, "not published yet", an old desktop, or nonsense.
    enum AppearanceMode { case notPublished, unsupported, garbage, ok(JSONValue) }
    var appearanceMode = AppearanceMode.notPublished
    var appearanceCalls = 0
    var outputText = "existing session output"
    var outputExtras: [String: JSONValue] = [:]
    var inFlight: [String: Int] = [:]
    var maxInFlight: [String: Int] = [:]
    // Live sync, like a desktop that can wait for a change: results carry a hash, `if_changed` is answered `unchanged`, and
    // `wait_ms` holds the request back until the screen changes (at most `longPollCap`, so tests do not wait 8 s).
    var hashMode = false
    var screenVersion = 0
    var longPollCap = Duration.milliseconds(120)
    var outputRequestLog: [[String: JSONValue]] = []
    var outputRequestTimes: [ContinuousClock.Instant] = []
    var failOutputs = 0
    var failOutputCode = "unavailable"
    /// An older desktop: it knows `shell_id` and `lines` only and refuses anything else (`invalid_request`, unknown field); or a
    /// newer connector whose `riwork` CLI is older (`cli_error`).
    enum OldDesktop { case no, strictFields, oldCLI }
    var oldDesktop = OldDesktop.no
    // Scrolling: a scripted scrollback (like tmux's) that `shell.output` and `shell.history` answer from, when set.
    var scrollback: ScriptedScrollback?
    var reportsHistorySize = true
    var alternate: Bool?
    enum HistoryMode { case ok, unsupported, rejectsStyled, tooLarge(above: Int), failing(code: String), wrongCount }
    var historyMode = HistoryMode.ok
    var historyRequestLog: [[String: JSONValue]] = []
    var historyGated = false
    /// A link the history pages travel over: every page takes `fixed` plus its bytes at `bytesPerSecond`.
    var historyFixed = Duration.zero
    var historyBytesPerSecond: Double?
    var historyTimes: [ContinuousClock.Instant] = []
    private var waiters: [CheckedContinuation<Void, any Error>] = []
    func setSessions(_ sessions: [RemoteSession]) { listedShells = sessions }
    func setMissing(_ id: String) { missingOutputs.insert(id) }
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing {
        connected = true
        connections += 1
        if pairing.v == 2, pairing.root_key == nil {
            return pairing.established(rootKey: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8")
        }
        return pairing
    }
    func disconnect() async { connected = false; events.append("transport.disconnect") }
    func setOversize(above lines: Int?, code: String = "response_too_large") { oversizeAbove = lines; oversizeCode = code }
    func lineRequests() -> [Int] { outputLineRequests }
    /// Parks the named RPC like an in-flight request. Cancelling its caller throws but keeps the connection, as RelayClient does.
    func block(_ method: String) { blockedMethod = method; blockedCount = 0 }
    func unblock() { blockedMethod = nil; let all = waiters; waiters = []; all.forEach { $0.resume() } }
    func waitUntilBlocked() async { while blockedCount == 0 { try? await Task.sleep(for: .milliseconds(5)) } }
    private func cancelWaiters() { let all = waiters; waiters = []; all.forEach { $0.resume(throwing: CancellationError()) } }
    func isConnected() async -> Bool { connected }
    func setUncertain() { inputUncertain = true }
    func setOutput(_ text: String, extras: [String: JSONValue] = [:]) { outputText = text; outputExtras = extras; screenVersion += 1 }
    func setHashMode(_ on: Bool, cap: Duration = .milliseconds(120)) { hashMode = on; longPollCap = cap }
    func outputRequests() -> [[String: JSONValue]] { outputRequestLog }
    func outputTimes() -> [ContinuousClock.Instant] { outputRequestTimes }
    func currentHash() -> String { "hash-\(screenVersion)" }
    func failNextOutputs(_ count: Int, code: String = "unavailable") { failOutputs = count; failOutputCode = code }
    func pendingOutputFailures() -> Int { failOutputs }
    func setOldDesktop(_ kind: OldDesktop) { oldDesktop = kind }
    func setScrollback(_ value: ScriptedScrollback?, reportsHistorySize: Bool = true) { scrollback = value; self.reportsHistorySize = reportsHistorySize; screenVersion += 1 }
    /// Output arrives on the scripted desktop: its screen changes, so a waiting long poll returns.
    func write(_ count: Int) { scrollback?.write(count); screenVersion += 1 }
    func setAlternate(_ on: Bool?) { alternate = on; screenVersion += 1 }
    /// The flag changes but the screen (and so its hash) does not: only `unchanged` answers carry it.
    func setAlternateQuietly(_ on: Bool?) { alternate = on }
    func setHistoryMode(_ mode: HistoryMode) { historyMode = mode }
    func historyRequests() -> [[String: JSONValue]] { historyRequestLog }
    /// Holds `shell.history` requests back (after they are logged) until released, so output can arrive while a page is on its way.
    func gateHistory(_ on: Bool) { historyGated = on }
    func setLink(fixed: Duration, bytesPerSecond: Double?) { historyFixed = fixed; historyBytesPerSecond = bytesPerSecond }
    func historyRequestTimes() -> [ContinuousClock.Instant] { historyTimes }
    func scriptedScrollback() -> ScriptedScrollback? { scrollback }
    func setKeysMode(_ mode: KeysMode) { keysMode = mode }
    func setKeysDelay(_ delay: Duration?) { keysDelay = delay }
    func setAppearance(_ mode: AppearanceMode) { appearanceMode = mode }
    func appearanceRequests() -> Int { appearanceCalls }
    func setConnected(_ value: Bool) { connected = value }
    func calls() -> [KeyCall] { keyCalls }
    func resizeRequests() -> [TerminalViewport] { resizes }
    func peakInFlight(_ method: String) -> Int { maxInFlight[method] ?? 0 }
    func inFlightCount(_ method: String) -> Int { inFlight[method] ?? 0 }
    /// What reached the shell, in order, once per batch (a retried batch counts once, as the desktop dedupes it).
    func delivered(shell: String) -> String {
        var seen: Set<String> = []
        var out = ""
        for call in keyCalls where call.shell == shell && seenBatches.contains(call.batch) && seen.insert(call.batch).inserted {
            for item in call.items { out += (try? KeyItem(json: item))?.symbol ?? "?" }
        }
        return out
    }
    func counts() -> (Int, [String]) { (connections, inputs) }
    func operations() -> [String] { events }
    func lines() -> [String] { inputLines }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        guard connected else { throw RemoteError.disconnected }
        try RequestValidation.validate(method: method, params: params, id: id)
        // Theme sync runs on its own schedule; keeping it out of `events` leaves the ordering assertions about everything else intact.
        if method == "appearance.get" { appearanceCalls += 1 } else { events.append("\(method):\(params["shell_id"]?.string ?? "")") }
        inFlight[method, default: 0] += 1
        maxInFlight[method] = max(maxInFlight[method] ?? 0, inFlight[method]!)
        defer { inFlight[method, default: 1] -= 1 }
        if blockedMethod == method {
            blockedCount += 1
            try await withTaskCancellationHandler {
                try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in waiters.append(continuation) }
            } onCancel: { Task { await self.cancelWaiters() } }
        }
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
            var lines = 0
            if case .number(let value)? = params["lines"] { lines = Int(value) }
            outputLineRequests.append(lines)
            outputRequestLog.append(params)
            outputRequestTimes.append(.now)
            if failOutputs > 0 { failOutputs -= 1; throw RemoteError.rpc(code: failOutputCode, message: "output failed") }
            if params["styled"] != nil || params["if_changed"] != nil || params["wait_ms"] != nil {
                switch oldDesktop {
                case .no: break
                case .strictFields: throw RemoteError.rpc(code: "invalid_request", message: "unknown field `styled`, expected `shell_id` or `lines`")
                case .oldCLI: throw RemoteError.rpc(code: "cli_error", message: "the installed riwork CLI does not support styled output or waiting for changes; update RiWork")
                }
            }
            if let oversizeAbove, lines > oversizeAbove { throw RemoteError.rpc(code: oversizeCode, message: "reply too large") }
            if let shell = params["shell_id"]?.string, missingOutputs.contains(shell) {
                throw RemoteError.rpc(code: "not_found", message: "Selected session is unavailable.")
            }
            if hashMode, case .string(let known)? = params["if_changed"], known == currentHash() {
                if case .number(let wait)? = params["wait_ms"], wait > 0 {
                    let version = screenVersion
                    let deadline = ContinuousClock.now + min(.milliseconds(Int(wait)), longPollCap)
                    while screenVersion == version, ContinuousClock.now < deadline { try await Task.sleep(for: .milliseconds(3)) }
                }
                if known == currentHash() {
                    var unchanged: [String: JSONValue] = ["shell_id": params["shell_id"]!, "unchanged": .bool(true), "hash": .string(known)]
                    if let scrollback {
                        if reportsHistorySize { unchanged["history_size"] = .number(Double(alternate == true ? 0 : scrollback.historySize)) }
                        if let alternate { unchanged["alternate"] = .bool(alternate) }
                    }
                    return .object(unchanged)
                }
            }
            var reply: [String: JSONValue] = ["shell_id": params["shell_id"]!, "output": .string(outputText)]
            reply.merge(outputExtras) { $1 }
            if let scrollback { reply.merge(scrollback.outputFields(lines: lines, reportsHistorySize: reportsHistorySize, alternate: alternate)) { $1 } }
            else if let alternate { reply["alternate"] = .bool(alternate) }
            if hashMode { reply["hash"] = .string(currentHash()) }
            return .object(reply)
        case "shell.history":
            historyRequestLog.append(params)
            historyTimes.append(.now)
            switch historyMode {
            case .ok: break
            case .unsupported: throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")
            case .rejectsStyled: if params["styled"] != nil { throw RemoteError.rpc(code: "invalid_request", message: "unknown field `styled`") }
            case .tooLarge(let above): if case .number(let n)? = params["lines"], Int(n) > above { throw RemoteError.rpc(code: "response_too_large", message: "reply too large") }
            case .failing(let code): throw RemoteError.rpc(code: code, message: "history failed")
            case .wrongCount: break
            }
            if let shell = params["shell_id"]?.string, missingOutputs.contains(shell) { throw RemoteError.rpc(code: "not_found", message: "Selected session is unavailable.") }
            while historyGated { try await Task.sleep(for: .milliseconds(3)) }
            guard let scrollback, case .number(let end)? = params["end"], case .number(let count)? = params["lines"] else { throw RemoteError.rpc(code: "invalid_request", message: "no scrollback") }
            var fields = scrollback.historyFields(shellID: params["shell_id"]!, end: Int(end), lines: Int(count))
            if case .wrongCount = historyMode, case .number(let n)? = fields["line_count"] { fields["line_count"] = .number(n + 1) }
            if historyFixed > .zero || historyBytesPerSecond != nil {
                // The link: a fixed cost, and the bytes the page weighs at the link's rate.
                let bytes = HistoryReply.wireBytes(of: fields["output"]?.string ?? "")
                let transfer = historyBytesPerSecond.map { Duration.seconds(Double(bytes) / $0) } ?? .zero
                try await Task.sleep(for: historyFixed + transfer)
            }
            return .object(fields)
        case "shell.input":
            inputs.append(id)
            inputLines.append(params["line"]!.string!)
            if inputUncertain { connected = false; throw RemoteError.uncertainDelivery }
            return .object(["shell_id": params["shell_id"]!, "status": .string("sent")])
        case "shell.keys":
            let batch = params["batch"]!.string!
            keyCalls.append(KeyCall(request: id, batch: batch, shell: params["shell_id"]!.string!, items: params["items"]!.array))
            switch keysMode {
            case .unsupported: throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")
            case .inputUnavailable: throw RemoteError.rpc(code: "input_unavailable", message: "The pane has input disabled.")
            case .notFound: throw RemoteError.rpc(code: "not_found", message: "Selected session is unavailable.")
            case .uncertain: return .object(["shell_id": params["shell_id"]!, "batch": params["batch"]!, "status": .string("uncertain")])
            case .dropLinkAfterDelivery:
                // The desktop took the batch, then the link died before the phone heard back.
                keysMode = .ok; seenBatches.insert(batch); connected = false
                throw RemoteError.disconnected
            case .ok: break
            }
            if let keysDelay { try await Task.sleep(for: keysDelay) }
            let status = seenBatches.insert(batch).inserted ? "sent" : "duplicate"
            return .object(["shell_id": params["shell_id"]!, "batch": params["batch"]!, "status": .string(status)])
        case "shell.resize":
            if case .number(let c)? = params["columns"], case .number(let r)? = params["rows"] { resizes.append(TerminalViewport(columns: Int(c), rows: Int(r))) }
            return .object(["shell_id": params["shell_id"]!, "columns": params["columns"]!, "rows": params["rows"]!])
        case "shell.resize.clear": return .object(["shell_id": params["shell_id"]!, "status": .string("cleared")])
        case "appearance.get":
            switch appearanceMode {
            case .notPublished: throw RemoteError.rpc(code: "not_found", message: "appearance not published")
            case .unsupported: throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")
            case .garbage: return .object(["v": .number(1), "dark": .string("yes")])
            case .ok(let value): return value
            }
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

    func testPollInFlightAtDisconnectDetachesAndViewportReleaseRunsBeforeTheSocketCloses() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let model = RemoteModel(client: transport, keychain: keychain, pollInterval: .milliseconds(10))
        model.reportViewport(TerminalViewport(columns: 43, rows: 17)); model.setTerminalVisible(true)
        await model.connect()
        XCTAssertEqual(model.viewportSessionID, shell)
        await transport.block("shell.output")
        await transport.waitUntilBlocked()
        await model.disconnect()
        await transport.unblock()
        let operations = await transport.operations()
        let clear = try XCTUnwrap(operations.lastIndex(of: "shell.resize.clear:\(shell)"), "release must run even though a poll was in flight")
        let close = try XCTUnwrap(operations.lastIndex(of: "transport.disconnect"))
        XCTAssertLessThan(clear, close, "release must precede closing the socket")
        XCTAssertEqual(model.state, .disconnected)
        XCTAssertNil(model.error, "a cancelled poll is not a failure")
        XCTAssertNil(model.viewportSessionID)
    }
    func testBackingOutOfAProjectLoadKeepsTheConnectionAndReloadsOnReturn() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let model = RemoteModel(client: transport, keychain: keychain)
        await model.connect()
        let other = "66666666-6666-4666-8666-666666666666"
        await transport.block("shells.list")
        let choose = Task { await model.chooseProject(other) }
        await transport.waitUntilBlocked()
        choose.cancel()
        await choose.value
        let stillConnected = await transport.isConnected()
        XCTAssertTrue(stillConnected, "Back during a load must not drop the socket")
        XCTAssertEqual(model.state, .connected)
        XCTAssertNil(model.error)
        XCTAssertTrue(model.shells.isEmpty)
        await transport.unblock()
        // The same project is chosen again on return; the half-finished load must run again.
        await model.chooseProject(other)
        XCTAssertEqual(model.projectID, other)
        XCTAssertEqual(model.shells.map(\.id), [shell])
        XCTAssertEqual(model.outputSessionID, shell)
        XCTAssertEqual(model.state, .connected)
        await model.disconnect()
    }
    func testProjectLoadDoesNotFetchUnusedTasks() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let model = RemoteModel(client: transport, keychain: keychain)
        await model.connect()
        await model.chooseProject("66666666-6666-4666-8666-666666666666")
        let operations = await transport.operations()
        XCTAssertFalse(operations.contains { $0.hasPrefix("tasks.list") })
        XCTAssertTrue(operations.contains { $0.hasPrefix("shells.list") })
        await model.disconnect()
    }
    func testManualDisconnectStaysDisconnectedThroughBackgroundAndForeground() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        let model = RemoteModel(client: transport, keychain: keychain)
        await model.connect()
        await model.disconnect()
        XCTAssertEqual(model.state, .disconnected)
        await model.disconnect(background: true)
        XCTAssertEqual(model.state, .disconnected, "a manual disconnect is not \"paused in background\"")
        await model.resume()
        XCTAssertEqual(model.state, .disconnected)
        let counts = await transport.counts()
        XCTAssertEqual(counts.0, 1, "returning to the foreground must not reconnect a manual disconnect")
    }
    func testOversizedOutputHalvesLinesAndRemembersThePerSessionValue() async throws {
        for code in ["response_too_large", "cli_error"] {
            let keychain = try makeStore(); defer { try? keychain.delete() }
            let transport = FixtureTransport()
            await transport.setOversize(above: 130, code: code)
            let model = RemoteModel(client: transport, keychain: keychain)
            await model.connect()
            let first = await transport.lineRequests()
            XCTAssertEqual(first, [500, 250, 125], code)
            XCTAssertFalse(model.snapshotStale); XCTAssertNil(model.error); XCTAssertTrue(model.canSend, "a fitting size must restore Send")
            await model.readOutput()
            let second = await transport.lineRequests()
            XCTAssertEqual(second.suffix(1), [125], "the size that fit is remembered for this session")
            await model.disconnect()
        }
    }
    func testOutputLinesStopHalvingAtTheFloor() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.setOversize(above: 0, code: "cli_error")
        let model = RemoteModel(client: transport, keychain: keychain)
        await model.connect()
        let attempts = await transport.lineRequests()
        XCTAssertEqual(attempts, [500, 250, 125, 62, 31, 20])
        XCTAssertNotNil(model.error); XCTAssertTrue(model.snapshotStale)
        await model.readOutput()
        let later = await transport.lineRequests()
        XCTAssertEqual(Array(later.dropFirst(attempts.count)), [20], "a session that keeps failing costs one attempt per poll, not a new halving run")
        await model.disconnect()
    }
    func testUnreadableKeychainLibraryIsNeverOverwrittenAndCanBeRetried() async throws {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        defer { try? keychain.delete() }
        // A schema this build cannot decode, standing in for any read failure other than "not found".
        try keychain.write(["future": "schema"])
        let model = RemoteModel(client: FixtureTransport(), keychain: keychain)
        XCTAssertTrue(model.loadFailed)
        XCTAssertTrue(model.desktops.isEmpty)
        let pairing = """
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """
        XCTAssertThrowsError(try model.add(pairingText: pairing, name: "New", allowLocal: false))
        XCTAssertTrue(model.desktops.isEmpty)
        XCTAssertEqual(try keychain.read([String: String].self), ["future": "schema"], "the stored item must be untouched")
        // Fixing the cause and retrying loads it and re-enables writes.
        let saved = try Pairing.parse(pairing)
        try keychain.write(Library(desktops: [SavedDesktop(name: "Kept", pairing: saved, allowLocalDevelopment: false)]))
        model.loadLibrary()
        XCTAssertFalse(model.loadFailed)
        XCTAssertEqual(model.desktops.map(\.name), ["Kept"])
        try model.rename(id: saved.route_id, name: "Renamed")
        XCTAssertEqual(try keychain.read(Library.self)?.desktops.map(\.name), ["Renamed"])
    }
    func testMissingKeychainItemIsAnEmptyLibraryNotAFailure() async throws {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        defer { try? keychain.delete() }
        let model = RemoteModel(client: FixtureTransport(), keychain: keychain)
        XCTAssertFalse(model.loadFailed)
        XCTAssertTrue(model.desktops.isEmpty)
    }
    func testResetLibraryErasesAnUnreadableItemAfterExplicitCall() async throws {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        defer { try? keychain.delete() }
        try keychain.write(["future": "schema"])
        let model = RemoteModel(client: FixtureTransport(), keychain: keychain)
        XCTAssertTrue(model.loadFailed)
        try model.resetLibrary()
        XCTAssertFalse(model.loadFailed)
        XCTAssertNil(try keychain.read([String: String].self))
    }
    func testV2ConnectStoresTheRootAndDropsTheInviteSecret() async throws {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        defer { try? keychain.delete() }
        let secret = "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA"
        let pairing = try Pairing.parse("""
        {"v":2,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","invite_id":"55555555-5555-4555-8555-555555555555","invite_secret":"\(secret)","expires_at":1893456000,"invite_state":"pending"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project
        desktop.selectedSessionID = shell
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        let model = RemoteModel(client: FixtureTransport(), keychain: keychain)
        await model.connect()
        XCTAssertEqual(model.state, .connected)
        let saved = try XCTUnwrap(keychain.read(Library.self)?.desktops.first?.pairing)
        XCTAssertEqual(saved.invite_state, "established")
        XCTAssertNil(saved.invite_secret)
        XCTAssertEqual(saved.root_key, "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8")
        let stored = String(data: try JSONEncoder().encode(saved), encoding: .utf8)!
        XCTAssertFalse(stored.contains(secret))
        await model.disconnect()
    }
}
