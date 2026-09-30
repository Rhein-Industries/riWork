import XCTest
import CryptoKit
@testable import RiWorkCore

/// In-process relay socket plus a desktop that speaks the real handshake and enforces the exact-next-counter rule.
private final class ScriptedSocket: WebSocketConnection, @unchecked Sendable {
    private let lock = NSLock()
    private var queue: [Result<SocketMessage, any Error>] = []
    private var waiter: CheckedContinuation<SocketMessage, any Error>?
    private var pingWaiter: CheckedContinuation<Void, any Error>?
    private var gate: [CheckedContinuation<Void, Never>] = []
    private var gated = false
    private var isCancelled = false
    private var code = URLSessionWebSocketTask.CloseCode.invalid
    private var reason: Data?
    private var sent: [String] = []
    private var pingCount = 0
    private var blockedSends = 0
    var onSend: (@Sendable (String) -> Void)?
    var pingBehavior = PingBehavior.answer
    enum PingBehavior { case answer, fail, hang }

    var cancelled: Bool { lock.withLock { isCancelled } }
    var sentCount: Int { lock.withLock { sent.count } }
    var pings: Int { lock.withLock { pingCount } }
    var blocked: Int { lock.withLock { blockedSends } }
    var closeCode: URLSessionWebSocketTask.CloseCode { lock.withLock { code } }
    var closeReason: Data? { lock.withLock { reason } }
    func resume() {}
    func send(_ text: String) async throws {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            lock.lock()
            if gated { gate.append(continuation); blockedSends += 1; lock.unlock() } else { lock.unlock(); continuation.resume() }
        }
        try lock.withLock { if isCancelled { throw URLError(.cancelled) }; sent.append(text) }
        onSend?(text)
    }
    func receive() async throws -> SocketMessage {
        try await withCheckedThrowingContinuation { continuation in
            lock.lock()
            if queue.isEmpty { waiter = continuation; lock.unlock() }
            else { let next = queue.removeFirst(); lock.unlock(); continuation.resume(with: next) }
        }
    }
    func ping() async throws {
        let behavior = lock.withLock { pingCount += 1; return pingBehavior }
        switch behavior {
        case .answer: return
        case .fail: throw URLError(.networkConnectionLost)
        case .hang: try await withCheckedThrowingContinuation { (c: CheckedContinuation<Void, any Error>) in lock.withLock { pingWaiter = c } }
        }
    }
    func cancel(with code: URLSessionWebSocketTask.CloseCode, reason: Data?) {
        let (w, p, held) = lock.withLock { () -> (CheckedContinuation<SocketMessage, any Error>?, CheckedContinuation<Void, any Error>?, [CheckedContinuation<Void, Never>]) in
            isCancelled = true
            defer { waiter = nil; pingWaiter = nil; gate = [] }
            return (waiter, pingWaiter, gate)
        }
        w?.resume(throwing: URLError(.cancelled)); p?.resume(throwing: URLError(.cancelled)); held.forEach { $0.resume() }
    }
    // MARK: test controls
    func push(_ payload: JSONValue) { deliver(.success(.text(String(data: try! JSONEncoder().encode(payload), encoding: .utf8)!))) }
    func closeFromPeer(code: URLSessionWebSocketTask.CloseCode, reason: String? = nil) {
        lock.withLock { self.code = code; self.reason = reason.map { Data($0.utf8) } }
        deliver(.failure(URLError(.networkConnectionLost)))
    }
    func holdSends() { lock.withLock { gated = true } }
    func releaseSends() {
        let held = lock.withLock { () -> [CheckedContinuation<Void, Never>] in gated = false; blockedSends = 0; defer { gate = [] }; return gate }
        held.forEach { $0.resume() }
    }
    private func deliver(_ result: Result<SocketMessage, any Error>) {
        let w = lock.withLock { () -> CheckedContinuation<SocketMessage, any Error>? in
            if let waiter { self.waiter = nil; return waiter }
            queue.append(result); return nil
        }
        w?.resume(with: result)
    }
}

private struct Connector: WebSocketConnecting {
    let socket: ScriptedSocket
    func connection(to url: URL) -> any WebSocketConnection { socket }
}

private final class ScriptedDesktop: @unchecked Sendable {
    enum Policy { case respond, hold, silent }
    let pairing: Pairing
    let socket: ScriptedSocket
    private let lock = NSLock()
    private var cipher: SessionCipher?
    private var c2dNext: UInt64 = 0, d2cNext: UInt64 = 0
    private var requests: [(id: String, method: String, counter: UInt64, params: JSONValue)] = []
    private var counterViolations = 0
    private var held: [String: JSONValue] = [:]
    var silent = false
    var peerOnline = true
    var corruptServerProof = false
    var withholdReady = false
    var policy: @Sendable (String) -> Policy = { _ in .respond }

    init(socket: ScriptedSocket, pairing: Pairing) {
        self.socket = socket; self.pairing = pairing
        socket.onSend = { self.handle($0) }
    }
    var requestCount: Int { lock.withLock { requests.count } }
    var violations: Int { lock.withLock { counterViolations } }
    func methods() -> [String] { lock.withLock { requests.map(\.method) } }
    func counters() -> [UInt64] { lock.withLock { requests.map(\.counter) } }
    func ids() -> [String] { lock.withLock { requests.map(\.id) } }
    func params() -> [JSONValue] { lock.withLock { requests.map(\.params) } }

    private func handle(_ text: String) {
        guard !silent, let frame = try? JSONDecoder().decode(JSONValue.self, from: Data(text.utf8)) else { return }
        switch frame["type"].string {
        case "register": socket.push(.object(["v": .number(1), "type": .string("registered"), "peer_online": .bool(peerOnline)]))
        case "client_hello": serverHello(frame)
        case "client_finish": if !withholdReady { socket.push(sealReady()) }
        case "encrypted": handleEncrypted(frame)
        default: break
        }
    }
    private func serverHello(_ hello: JSONValue) {
        let clientNonce = try! Base64URL.decode(hello["client_nonce"].string!, bytes: 32)
        let handshake = try! ClientHandshake(pairing: pairing, nonce: clientNonce)
        let desktopNonce = Data((0..<32).map { UInt8($0 &+ 64) })
        let transcript = Data("riwork/v1/session\0".utf8) + handshake.identity + clientNonce + desktopNonce
        let proof = handshake.mac(Data("riwork/v1/server-hello\0".utf8) + transcript)
        let psk = SymmetricKey(data: try! Base64URL.decode(pairing.pairing_secret, bytes: 32))
        lock.withLock { cipher = SessionCipher(psk: psk, transcript: transcript) }
        socket.push(.object(["v": .number(1), "type": .string("server_hello"), "desktop_nonce": .string(Base64URL.encode(desktopNonce)), "mac": .string(corruptServerProof ? Base64URL.encode(Data(repeating: 7, count: 32)) : proof)]))
    }
    private func sealReady() -> JSONValue { seal(.object(["v": .number(1), "type": .string("ready"), "desktop_id": .string(pairing.desktop_id), "device_id": .string(pairing.device_id)])) }
    private func seal(_ payload: JSONValue) -> JSONValue {
        lock.withLock {
            let cipher = self.cipher!, counter = d2cNext; d2cNext += 1
            let nonce = try! ChaChaPoly.Nonce(data: Data(repeating: 0, count: 4) + SessionCipher.counterBytes(counter))
            let box = try! ChaChaPoly.seal(try! JSONEncoder().encode(payload), using: cipher.d2c, nonce: nonce, authenticating: cipher.aad(direction: "d2c", counter: counter))
            return .object(["v": .number(1), "type": .string("encrypted"), "session_id": .string(Base64URL.encode(cipher.sessionID)), "direction": .string("d2c"), "counter": .string(String(counter)), "ciphertext": .string(Base64URL.encode(box.ciphertext + box.tag))])
        }
    }
    private func handleEncrypted(_ frame: JSONValue) {
        let request: JSONValue? = lock.withLock {
            guard let cipher, frame["counter"].string == String(c2dNext), let bytes = try? Base64URL.decode(frame["ciphertext"].string ?? "") else { counterViolations += 1; return nil }
            let counter = c2dNext
            guard let nonce = try? ChaChaPoly.Nonce(data: Data(repeating: 0, count: 4) + SessionCipher.counterBytes(counter)),
                  let box = try? ChaChaPoly.SealedBox(nonce: nonce, ciphertext: bytes.dropLast(16), tag: bytes.suffix(16)),
                  let plain = try? ChaChaPoly.open(box, using: cipher.c2d, authenticating: cipher.aad(direction: "c2d", counter: counter)),
                  let value = try? JSONDecoder().decode(JSONValue.self, from: plain) else { counterViolations += 1; return nil }
            c2dNext += 1
            requests.append((value["id"].string ?? "", value["method"].string ?? "", counter, value["params"]))
            return value
        }
        guard let request, let id = request["id"].string else { return }
        // `shell.keys` is answered the way the desktop does (the request's identity plus a status); everything else echoes its method.
        let result: JSONValue = request["method"].string == "shell.keys"
            ? .object(["shell_id": request["params"]["shell_id"], "batch": request["params"]["batch"], "status": .string("sent")])
            : .object(["method": request["method"]])
        let response = JSONValue.object(["v": .number(1), "type": .string("response"), "id": .string(id), "ok": .bool(true), "result": result])
        switch policy(request["method"].string ?? "") {
        case .respond: socket.push(seal(response))
        case .hold: lock.withLock { held[id] = response }
        case .silent: break
        }
    }
    /// Delivers a response the desktop was holding, long after its caller gave up.
    func release(id: String) {
        guard let response = lock.withLock({ held.removeValue(forKey: id) }) else { return }
        socket.push(seal(response))
    }
    func sealAndPush(_ payload: JSONValue) { socket.push(seal(payload)) }
}

@MainActor final class RelayClientTests: XCTestCase {
    private let shell = "44444444-4444-4444-8444-444444444444"
    private func pairing() throws -> Pairing {
        try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
    }
    private func rig(requestTimeout: Duration = .seconds(5), pingInterval: Duration = .seconds(60), handshakeTimeout: Duration = .seconds(2), configure: (ScriptedDesktop) -> Void = { _ in }) throws -> (RelayClient, ScriptedSocket, ScriptedDesktop) {
        let socket = ScriptedSocket(), desktop = ScriptedDesktop(socket: socket, pairing: try pairing())
        configure(desktop)
        return (RelayClient(requestTimeout: requestTimeout, pingInterval: pingInterval, handshakeTimeout: handshakeTimeout, connector: Connector(socket: socket)), socket, desktop)
    }
    private func connected(requestTimeout: Duration = .seconds(5), pingInterval: Duration = .seconds(60), configure: (ScriptedDesktop) -> Void = { _ in }) async throws -> (RelayClient, ScriptedSocket, ScriptedDesktop) {
        let (client, socket, desktop) = try rig(requestTimeout: requestTimeout, pingInterval: pingInterval, configure: configure)
        try await client.connect(pairing: pairing())
        return (client, socket, desktop)
    }
    private func eventually(_ what: String, timeout: Double = 3, _ condition: () async -> Bool) async {
        let end = Date().addingTimeInterval(timeout)
        while await !condition(), Date() < end { try? await Task.sleep(for: .milliseconds(5)) }
        let met = await condition()
        XCTAssertTrue(met, what)
    }
    private func outputParams() -> [String: JSONValue] { ["shell_id": .string(shell)] }
    private let batch = "55555555-5555-4555-8555-555555555555"
    private func keysParams(items: [KeyItem] = [.text("ls"), .key(.enter)], batch: String? = nil) -> [String: JSONValue] {
        ["shell_id": .string(shell), "batch": .string(batch ?? self.batch), "items": .array(items.map(\.json))]
    }
    private func isUncertain(_ error: any Error) -> Bool { if case RemoteError.uncertainDelivery = error { true } else { false } }
    /// What a request fails with when the connection ends while it is in flight and unanswered.
    private func failure(of method: String, params: [String: JSONValue], requestTimeout: Duration = .seconds(5), ending end: (RelayClient, ScriptedSocket) async -> Void) async throws -> any Error {
        let (client, socket, desktop) = try await connected(requestTimeout: requestTimeout) { $0.policy = { _ in .silent } }
        let call = Task { try await client.request(method: method, params: params, id: "dddddddd-dddd-4ddd-8ddd-dddddddddddd") }
        await eventually("\(method) reached the desktop") { desktop.requestCount == 1 }
        await end(client, socket)
        do { _ = try await call.value; XCTFail("\(method) succeeded although the connection ended"); return RemoteError.remote("succeeded") } catch { return error }
    }

    func testHandshakeReachesReadyAndRequestsRoundTrip() async throws {
        let (client, _, desktop) = try await connected()
        let connectedNow = await client.isConnected()
        XCTAssertTrue(connectedNow)
        let result = try await client.request(method: "projects.list")
        XCTAssertEqual(result["method"].string, "projects.list")
        XCTAssertEqual(desktop.violations, 0)
    }

    // MARK: cancellation

    func testCancellingOneRequestKeepsTheConnectionAndCounterSequence() async throws {
        let (client, socket, desktop) = try await connected { $0.policy = { $0 == "shell.output" ? .hold : .respond } }
        let slow = Task { try await client.request(method: "shell.output", params: self.outputParams(), id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa") }
        await eventually("desktop saw the request") { desktop.requestCount == 1 }
        slow.cancel()
        do { _ = try await slow.value; XCTFail("cancelled request must throw") } catch { XCTAssertTrue(error is CancellationError, "\(error)") }
        let stillConnected = await client.isConnected()
        XCTAssertTrue(stillConnected, "cancelling one caller must not drop the socket")
        XCTAssertFalse(socket.cancelled)
        // A later request uses the next counter, and the abandoned request's late response is ignored.
        let next = try await client.request(method: "projects.list")
        XCTAssertEqual(next["method"].string, "projects.list")
        desktop.release(id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
        let after = try await client.request(method: "orchestrators.list")
        XCTAssertEqual(after["method"].string, "orchestrators.list")
        XCTAssertEqual(desktop.violations, 0)
        XCTAssertEqual(desktop.methods(), ["shell.output", "projects.list", "orchestrators.list"])
        let alive = await client.isConnected()
        XCTAssertTrue(alive)
    }

    func testQueuedRequestCancelledBeforeItsSendIsStillSentInOrder() async throws {
        let (client, socket, desktop) = try await connected()
        socket.holdSends()
        let first = Task { try await client.request(method: "projects.list") }
        await eventually("first frame is mid-send") { socket.blocked == 1 }
        let second = Task { try await client.request(method: "shell.output", params: self.outputParams(), id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb") }
        try await Task.sleep(for: .milliseconds(50))
        second.cancel()
        do { _ = try await second.value; XCTFail() } catch { XCTAssertTrue(error is CancellationError) }
        socket.releaseSends()
        _ = try await first.value
        // The cancelled request's counter was spent, so its frame must still reach the desktop before the next one.
        await eventually("both frames delivered in order") { desktop.requestCount == 2 }
        let third = try await client.request(method: "orchestrators.list")
        XCTAssertEqual(third["method"].string, "orchestrators.list")
        XCTAssertEqual(desktop.violations, 0)
        XCTAssertEqual(desktop.methods(), ["projects.list", "shell.output", "orchestrators.list"])
        let alive = await client.isConnected()
        XCTAssertTrue(alive)
    }

    func testAlreadyCancelledCallerNeverConsumesACounter() async throws {
        let (client, _, desktop) = try await connected()
        let task = Task {
            while !Task.isCancelled { await Task.yield() }
            return try await client.request(method: "projects.list")
        }
        task.cancel()
        do { _ = try await task.value; XCTFail() } catch { XCTAssertTrue(error is CancellationError) }
        XCTAssertEqual(desktop.requestCount, 0)
        _ = try await client.request(method: "projects.list")
        XCTAssertEqual(desktop.violations, 0)
        XCTAssertEqual(desktop.counters(), [0])
    }

    func testCancelledInputReportsUncertainDeliveryAndKeepsTheConnection() async throws {
        let (client, _, desktop) = try await connected { $0.policy = { $0 == "shell.input" ? .hold : .respond } }
        let id = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
        let input = Task { try await client.request(method: "shell.input", params: ["shell_id": .string(self.shell), "line": .string("echo hi")], id: id) }
        await eventually("input reached the desktop") { desktop.requestCount == 1 }
        input.cancel()
        do { _ = try await input.value; XCTFail() } catch {
            guard case RemoteError.uncertainDelivery = error else { return XCTFail("\(error)") }
        }
        _ = try await client.request(method: "projects.list")
        XCTAssertEqual(desktop.violations, 0)
    }

    func testDisconnectFailsOutstandingRequestsAndClosesTheSocket() async throws {
        let (client, socket, desktop) = try await connected { $0.policy = { _ in .silent } }
        let outstanding = Task { try await client.request(method: "projects.list") }
        await eventually("request reached the desktop") { desktop.requestCount == 1 }
        await client.disconnect()
        do { _ = try await outstanding.value; XCTFail() } catch { guard case RemoteError.disconnected = error else { return XCTFail("\(error)") } }
        XCTAssertTrue(socket.cancelled)
        let alive = await client.isConnected()
        XCTAssertFalse(alive)
    }

    // MARK: shell.keys

    func testShellKeysRoundTripsThroughTheRealClientOnTheNextCounter() async throws {
        let (client, _, desktop) = try await connected()
        _ = try await client.request(method: "projects.list")
        let items: [KeyItem] = [.text("ls -la"), .key(.enter), .key(.control("c")), .text("héllo 🙂")]
        let status = try await client.keys(shellID: shell, batch: batch, items: items)
        XCTAssertEqual(status, .sent)
        XCTAssertEqual(desktop.methods(), ["projects.list", "shell.keys"])
        XCTAssertEqual(desktop.counters(), [0, 1], "shell.keys takes the next counter")
        let sent = desktop.params()[1]
        XCTAssertEqual(sent["shell_id"].string, shell)
        XCTAssertEqual(sent["batch"].string, batch)
        XCTAssertEqual(sent["items"], .array(items.map(\.json)))
        // The raw request returns the desktop's result untouched.
        let raw = try await client.request(method: "shell.keys", params: keysParams(), id: "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee")
        XCTAssertEqual(raw["shell_id"].string, shell)
        XCTAssertEqual(raw["batch"].string, batch)
        XCTAssertEqual(raw["status"].string, "sent")
        XCTAssertEqual(desktop.counters(), [0, 1, 2])
        XCTAssertEqual(desktop.violations, 0)
        let alive = await client.isConnected()
        XCTAssertTrue(alive)
    }

    func testConnectionLossDuringShellKeysIsNotUncertainWhereShellInputIs() async throws {
        let inputParams: [String: JSONValue] = ["shell_id": .string(shell), "line": .string("echo hi")]
        // The relay closes the socket.
        let closeRelay: (RelayClient, ScriptedSocket) async -> Void = { _, socket in socket.closeFromPeer(code: .noStatusReceived) }
        let keysClosed = try await failure(of: "shell.keys", params: keysParams(), ending: closeRelay)
        guard case RemoteError.relayClosed(let code, _) = keysClosed else { return XCTFail("\(keysClosed)") }
        XCTAssertEqual(code, 1005)
        let inputClosed = try await failure(of: "shell.input", params: inputParams, ending: closeRelay)
        XCTAssertTrue(isUncertain(inputClosed), "\(inputClosed)")

        // The app disconnects.
        let disconnect: (RelayClient, ScriptedSocket) async -> Void = { client, _ in await client.disconnect() }
        let keysDisconnected = try await failure(of: "shell.keys", params: keysParams(), ending: disconnect)
        guard case RemoteError.disconnected = keysDisconnected else { return XCTFail("\(keysDisconnected)") }
        let inputDisconnected = try await failure(of: "shell.input", params: inputParams, ending: disconnect)
        XCTAssertTrue(isUncertain(inputDisconnected), "\(inputDisconnected)")

        // The desktop goes quiet and the request times out. shell.keys has its own, longer floor (see the next test),
        // so only shell.input is timed out here.
        let wait: (RelayClient, ScriptedSocket) async -> Void = { _, _ in }
        let inputTimedOut = try await failure(of: "shell.input", params: inputParams, requestTimeout: .milliseconds(120), ending: wait)
        XCTAssertTrue(isUncertain(inputTimedOut), "\(inputTimedOut)")
    }

    /// The desktop pauses ~150 ms between a text and the key after it, so a 64-item batch can take ~5 s to answer.
    func testShellKeysGetsALongerTimeoutFloorWhileOtherCallsKeepTheConfiguredTimeout() async throws {
        XCTAssertEqual(RelayClient.timeout(for: "shell.keys", default: .milliseconds(120)), .seconds(10))
        XCTAssertGreaterThanOrEqual(RelayClient.timeout(for: "shell.keys", default: .seconds(1)), .seconds(6), "never below the desktop's worst case")
        XCTAssertEqual(RelayClient.timeout(for: "shell.keys", default: .seconds(15)), .seconds(15), "a longer configured timeout is kept")
        XCTAssertEqual(RelayClient.timeout(for: "shell.output", default: .milliseconds(120)), .milliseconds(120))
        XCTAssertEqual(RelayClient.timeout(for: "shell.input", default: .milliseconds(120)), .milliseconds(120))
        // Behaviour: an answer that arrives after the configured timeout would have fired still completes the call.
        let (client, _, desktop) = try await connected(requestTimeout: .milliseconds(120)) { $0.policy = { $0 == "shell.keys" ? .hold : .respond } }
        let id = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"
        let call = Task { try await client.keys(shellID: self.shell, batch: self.batch, items: [.text("ls"), .key(.enter)], id: id) }
        await eventually("the desktop saw the batch") { desktop.requestCount == 1 }
        try await Task.sleep(for: .milliseconds(400))
        let stillUp = await client.isConnected()
        XCTAssertTrue(stillUp, "the 120 ms request timeout did not tear the connection down")
        desktop.release(id: id)
        let status = try await call.value
        XCTAssertEqual(status, .sent)
        XCTAssertEqual(desktop.violations, 0)
    }

    func testShellKeysOnAClientThatIsNotConnectedFailsAsDisconnected() async throws {
        let (client, socket, desktop) = try rig()
        do { _ = try await client.keys(shellID: shell, batch: batch, items: [.key(.enter)]); XCTFail() } catch {
            guard case RemoteError.disconnected = error else { return XCTFail("\(error)") }
        }
        XCTAssertEqual(socket.sentCount, 0)
        XCTAssertEqual(desktop.requestCount, 0)
    }

    func testCancellingShellKeysThrowsCancellationErrorAndKeepsTheConnectionAndCounterSequence() async throws {
        let (client, socket, desktop) = try await connected { $0.policy = { $0 == "shell.keys" ? .hold : .respond } }
        let id = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
        let keys = Task { try await client.request(method: "shell.keys", params: self.keysParams(), id: id) }
        await eventually("desktop saw the keys") { desktop.requestCount == 1 }
        keys.cancel()
        do { _ = try await keys.value; XCTFail("a cancelled request must throw") } catch {
            XCTAssertTrue(error is CancellationError, "not uncertain, the batch is simply retried: \(error)")
            XCTAssertFalse(isUncertain(error))
        }
        let stillConnected = await client.isConnected()
        XCTAssertTrue(stillConnected, "cancelling one caller must not drop the socket")
        XCTAssertFalse(socket.cancelled)
        // The next request uses the next counter, and the abandoned request's late response is dropped.
        let next = try await client.request(method: "projects.list")
        XCTAssertEqual(next["method"].string, "projects.list")
        desktop.release(id: id)
        let after = try await client.request(method: "orchestrators.list")
        XCTAssertEqual(after["method"].string, "orchestrators.list")
        XCTAssertEqual(desktop.violations, 0)
        XCTAssertEqual(desktop.methods(), ["shell.keys", "projects.list", "orchestrators.list"])
        XCTAssertEqual(desktop.counters(), [0, 1, 2])
        let alive = await client.isConnected()
        XCTAssertTrue(alive)
    }

    func testRetryingABatchWithANewRequestIDIsADistinctRequest() async throws {
        let (client, _, desktop) = try await connected { $0.policy = { $0 == "shell.keys" ? .hold : .respond } }
        let items: [KeyItem] = [.text("git status"), .key(.enter)]
        let first = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", second = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        let attempt = Task { try await client.keys(shellID: self.shell, batch: self.batch, items: items, id: first) }
        await eventually("first attempt reached the desktop") { desktop.requestCount == 1 }
        attempt.cancel()
        do { _ = try await attempt.value; XCTFail() } catch { XCTAssertTrue(error is CancellationError, "\(error)") }
        // Same batch, new request id: the client accepts it while the first is still abandoned and unanswered.
        let retry = Task { try await client.keys(shellID: self.shell, batch: self.batch, items: items, id: second) }
        await eventually("retry reached the desktop") { desktop.requestCount == 2 }
        desktop.release(id: second)
        let status = try await retry.value
        XCTAssertEqual(status, .sent)
        desktop.release(id: first)   // the stale answer to the abandoned attempt is discarded
        _ = try await client.request(method: "projects.list")
        XCTAssertEqual(Array(desktop.ids().prefix(2)), [first, second])
        XCTAssertNotEqual(desktop.ids()[0], desktop.ids()[1])
        XCTAssertEqual(desktop.params()[0], desktop.params()[1], "same batch, same content")
        XCTAssertEqual(desktop.params()[1]["batch"].string, batch)
        XCTAssertEqual(desktop.methods(), ["shell.keys", "shell.keys", "projects.list"])
        XCTAssertEqual(desktop.counters(), [0, 1, 2], "each attempt spends its own counter")
        XCTAssertEqual(desktop.violations, 0)
        let alive = await client.isConnected()
        XCTAssertTrue(alive)
    }

    func testEachKeysCallGetsAFreshRequestIDByDefault() async throws {
        let (client, _, desktop) = try await connected()
        for _ in 0..<3 { let status = try await client.keys(shellID: shell, batch: batch, items: [.text("x")]); XCTAssertEqual(status, .sent) }
        XCTAssertEqual(Set(desktop.ids()).count, 3)
        XCTAssertEqual(Set(desktop.params().map { $0["batch"].string }), [batch])
        XCTAssertEqual(desktop.counters(), [0, 1, 2])
        XCTAssertEqual(desktop.violations, 0)
    }

    func testInvalidShellKeysRequestsAreRejectedBeforeAnythingIsSealed() async throws {
        let (client, socket, desktop) = try await connected()
        let framesBefore = socket.sentCount
        let enter = JSONValue.object(["key": .string("Enter")])
        var missingBatch = keysParams(); missingBatch["batch"] = nil
        var extra = keysParams(); extra["line"] = .string("ls")
        var missingShell = keysParams(); missingShell["shell_id"] = nil
        var notArray = keysParams(); notArray["items"] = enter
        let invalid: [(String, [String: JSONValue])] = [
            ("65 items", ["shell_id": .string(shell), "batch": .string(batch), "items": .array(Array(repeating: enter, count: 65))]),
            ("no items", ["shell_id": .string(shell), "batch": .string(batch), "items": .array([])]),
            ("upper-case batch", keysParams(batch: "ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDEF")),
            ("short batch", keysParams(batch: "5555")),
            ("missing batch", missingBatch),
            ("missing shell", missingShell),
            ("unknown parameter", extra),
            ("items not an array", notArray),
            ("text and key together", ["shell_id": .string(shell), "batch": .string(batch), "items": .array([.object(["text": .string("a"), "key": .string("Enter")])])]),
            ("unknown key", ["shell_id": .string(shell), "batch": .string(batch), "items": .array([.object(["key": .string("C-A")])])]),
            ("newline in text", ["shell_id": .string(shell), "batch": .string(batch), "items": .array([.object(["text": .string("a\nb")])])]),
            ("4097 text bytes", ["shell_id": .string(shell), "batch": .string(batch), "items": .array([.object(["text": .string(String(repeating: "a", count: 4097))])])])
        ]
        for (why, params) in invalid {
            do { _ = try await client.request(method: "shell.keys", params: params); XCTFail("accepted: \(why)") } catch {
                guard case RemoteError.protocolViolation = error else { return XCTFail("\(why): \(error)") }
            }
        }
        // The typed API refuses the same before it asks the transport for anything.
        for items in [[], [KeyItem.text("a\u{1b}")], Array(repeating: KeyItem.key(.tab), count: 65)] {
            do { _ = try await client.keys(shellID: shell, batch: batch, items: items); XCTFail() } catch { guard case RemoteError.protocolViolation = error else { return XCTFail("\(error)") } }
        }
        do { _ = try await client.keys(shellID: shell, batch: batch, items: [.key(.enter)], id: "not-a-uuid"); XCTFail() } catch { guard case RemoteError.protocolViolation = error else { return XCTFail("\(error)") } }
        XCTAssertEqual(desktop.requestCount, 0)
        XCTAssertEqual(socket.sentCount, framesBefore, "no frame was sealed or sent")
        // No counter was consumed: the next valid request is still counter 0.
        _ = try await client.request(method: "projects.list")
        XCTAssertEqual(desktop.counters(), [0])
        XCTAssertEqual(desktop.violations, 0)
        let alive = await client.isConnected()
        XCTAssertTrue(alive)
    }

    // MARK: handshake failures and timeouts

    func testBadDesktopProofFailsHandshakeAndClosesSocket() async throws {
        let (client, socket, _) = try rig { $0.corruptServerProof = true }
        do { try await client.connect(pairing: pairing()); XCTFail() } catch {
            guard case RemoteError.protocolViolation = error else { return XCTFail("\(error)") }
        }
        XCTAssertTrue(socket.cancelled)
        let alive = await client.isConnected()
        XCTAssertFalse(alive)
        do { _ = try await client.request(method: "projects.list"); XCTFail() } catch { guard case RemoteError.disconnected = error else { return XCTFail("\(error)") } }
    }

    func testMissingReadyTimesOutAtTheHandshakeDeadline() async throws {
        let (client, socket, _) = try rig(handshakeTimeout: .milliseconds(150)) { $0.withholdReady = true }
        let started = Date()
        do { try await client.connect(pairing: pairing()); XCTFail() } catch { guard case RemoteError.timeout = error else { return XCTFail("\(error)") } }
        XCTAssertLessThan(Date().timeIntervalSince(started), 2)
        XCTAssertTrue(socket.cancelled)
    }

    func testOfflineDesktopIsReportedAfterRegistration() async throws {
        let (client, socket, _) = try rig { $0.peerOnline = false }
        let connect = Task { try await client.connect(pairing: self.pairing()) }
        await eventually("register sent") { socket.sentCount == 1 }
        socket.push(.object(["v": .number(1), "type": .string("peer"), "online": .bool(false)]))
        do { _ = try await connect.value; XCTFail() } catch { XCTAssertTrue(error.localizedDescription.contains("offline"), error.localizedDescription) }
        XCTAssertTrue(socket.cancelled)
    }
    func testDesktopThatComesOnlineWhileRegisteringCompletesTheHandshake() async throws {
        let (client, socket, desktop) = try rig { $0.peerOnline = false }
        let connect = Task { try await client.connect(pairing: self.pairing()) }
        await eventually("register sent") { socket.sentCount == 1 }
        socket.push(.object(["v": .number(1), "type": .string("peer"), "online": .bool(true)]))
        _ = try await connect.value
        _ = try await client.request(method: "projects.list")
        XCTAssertEqual(desktop.violations, 0)
    }

    func testRelayCloseDuringHandshakeIsNamedNotOpaque() async throws {
        let (client, socket, _) = try rig { $0.silent = true }
        let connect = Task { try await client.connect(pairing: self.pairing()) }
        await eventually("register sent") { socket.sentCount == 1 }
        socket.closeFromPeer(code: .policyViolation, reason: "duplicate")
        do { _ = try await connect.value; XCTFail() } catch {
            guard case RemoteError.relayClosed(let code, let reason) = error else { return XCTFail("\(error)") }
            XCTAssertEqual(code, 1008); XCTAssertEqual(reason, "duplicate")
            XCTAssertTrue(error.localizedDescription.contains("duplicate"))
        }
    }

    func testRelayCloseWhileConnectedFailsPendingWithTheCloseCode() async throws {
        let (client, socket, desktop) = try await connected { $0.policy = { _ in .silent } }
        let outstanding = Task { try await client.request(method: "projects.list") }
        await eventually("request reached the desktop") { desktop.requestCount == 1 }
        socket.closeFromPeer(code: .noStatusReceived)
        do { _ = try await outstanding.value; XCTFail() } catch {
            guard case RemoteError.relayClosed(let code, _) = error else { return XCTFail("\(error)") }
            XCTAssertEqual(code, 1005)
        }
        let alive = await client.isConnected()
        XCTAssertFalse(alive)
    }

    func testUnansweredRequestTimesOutAndTearsDownTheConnection() async throws {
        let (client, socket, _) = try await connected(requestTimeout: .milliseconds(120)) { $0.policy = { _ in .silent } }
        do { _ = try await client.request(method: "projects.list"); XCTFail() } catch { guard case RemoteError.timeout = error else { return XCTFail("\(error)") } }
        XCTAssertTrue(socket.cancelled)
        let alive = await client.isConnected()
        XCTAssertFalse(alive)
    }

    // MARK: keepalive

    func testIdleConnectionSendsPingsAndStaysUp() async throws {
        let (client, socket, _) = try await connected(pingInterval: .milliseconds(20))
        await eventually("several pings while idle") { socket.pings >= 3 }
        let alive = await client.isConnected()
        XCTAssertTrue(alive)
    }

    func testFailedPingEndsTheConnectionWithoutWaitingForARequest() async throws {
        let (client, socket, _) = try await connected(pingInterval: .milliseconds(20)) { _ in }
        socket.pingBehavior = .fail
        await eventually("connection dropped by ping failure") { await !client.isConnected() }
        XCTAssertGreaterThanOrEqual(socket.pings, 1)
    }

    func testMissingPongTimesOutAndEndsTheConnection() async throws {
        let (client, socket, _) = try await connected(pingInterval: .milliseconds(30))
        socket.pingBehavior = .hang
        await eventually("a missing pong ends the connection") { await !client.isConnected() }
        XCTAssertTrue(socket.cancelled)
    }
}
