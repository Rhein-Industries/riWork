import Foundation

public enum RequestValidation {
    public static func validate(method: String, params: [String: JSONValue], id: String) throws {
        func uuid(_ value: String?) throws {
            guard let value, UUID(uuidString: value)?.uuidString.lowercased() == value else { throw RemoteError.protocolViolation("A full canonical UUID is required.") }
        }
        try uuid(id)
        let required: Set<String>
        let optional: Set<String>
        switch method {
        case "projects.list", "orchestrators.list": required = []; optional = []
        case "worktrees.list", "shells.list": required = ["project_id"]; optional = []
        case "tasks.list": required = ["project_id"]; optional = ["worktree_id"]
        case "shell.output": required = ["shell_id"]; optional = ["lines"]
        case "shell.input": required = ["shell_id", "line"]; optional = []
        case "shell.resize": required = ["shell_id", "columns", "rows"]; optional = []
        case "shell.resize.clear": required = ["shell_id"]; optional = []
        default: throw RemoteError.protocolViolation("Unsupported operation.")
        }
        let keys = Set(params.keys)
        guard required.isSubset(of: keys), keys.isSubset(of: required.union(optional)) else { throw RemoteError.protocolViolation("Invalid request parameters.") }
        for key in ["project_id", "worktree_id", "shell_id"] where params[key] != nil { try uuid(params[key]?.string) }
        if method == "shell.input" { guard let line = params["line"]?.string else { throw RemoteError.protocolViolation("Missing input.") }; try InputValidation.validate(line) }
        if let lines = params["lines"] { guard case .number(let value) = lines, value >= 1, value <= 2000, value.rounded() == value else { throw RemoteError.protocolViolation("Output lines must be 1–2000.") } }
        if method == "shell.resize" {
            for (key, range) in [("columns", 20.0...300.0), ("rows", 8.0...160.0)] {
                guard case .number(let value) = params[key], range.contains(value), value.rounded() == value else { throw RemoteError.protocolViolation("Invalid terminal cell dimensions.") }
            }
        }
    }
}

extension Duration {
    var seconds: Double { Double(components.seconds) + Double(components.attoseconds) / 1e18 }
}

/// A single receive loop owns the WebSocket. Only authenticated ready enables RPCs.
/// Every reconnect throws away old keys; input is never retried by this transport.
///
/// Cancelling one caller detaches only that caller. The frame it sealed still goes out in order (the
/// peer accepts only the exact next counter) and its late response is discarded.
public actor RelayClient: RemoteTransport {
    private let connector: any WebSocketConnecting
    private var socket: (any WebSocketConnection)?
    private var cipher: SessionCipher?
    private var generation = UUID()
    private var reader: Task<Void, Never>?
    private var keepAlive: Task<Void, Never>?
    private var sendTail: Task<Void, Never>?
    private struct Pending {
        let continuation: CheckedContinuation<JSONValue, any Error>
        let isInput: Bool
        let timeout: Task<Void, Never>
    }
    private var pending: [String: Pending] = [:]
    /// Requests whose caller was cancelled after the frame was sealed; their response is read and dropped.
    private var abandoned: Set<String> = []
    private let requestTimeout: Duration
    private let pingInterval: Duration
    private let handshakeTimeout: Duration
    public init(requestTimeout: Duration = .seconds(15), pingInterval: Duration = .seconds(10), handshakeTimeout: Duration = .seconds(10), connector: any WebSocketConnecting = URLSessionConnector()) {
        self.connector = connector
        self.requestTimeout = requestTimeout
        self.pingInterval = pingInterval
        self.handshakeTimeout = handshakeTimeout
    }
    public func isConnected() -> Bool { cipher != nil && socket != nil }
    public func connect(pairing: Pairing, allowLocalDevelopment: Bool = false) async throws {
        disconnect()
        try pairing.validate(allowLocalDevelopment: allowLocalDevelopment)
        guard let url = URL(string: pairing.relay_url) else { throw RemoteError.invalidPairing("Invalid relay URL.") }
        let token = generation
        let ws = connector.connection(to: url)
        socket = ws
        ws.resume()
        let deadline = Date().addingTimeInterval(handshakeTimeout.seconds)
        do {
            try await Self.send(.object(["v": .number(1), "type": .string("register"), "route_id": .string(pairing.route_id), "role": .string("mobile"), "token": .string(pairing.relay_token)]), on: ws)
            let registered = try await Self.receive(on: ws, deadline: deadline)
            guard generation == token, registered["v"] == .number(1), registered["type"].string == "registered", case .bool(let online) = registered["peer_online"] else { throw RemoteError.protocolViolation("Relay registration failed.") }
            if !online {
                let peer = try await Self.receive(on: ws, deadline: deadline)
                guard peer["v"] == .number(1), peer["type"].string == "peer", peer["online"] == .bool(true) else { throw RemoteError.remote("Desktop is offline. Start its connector and reconnect.") }
            }
            let handshake = try ClientHandshake(pairing: pairing)
            try await Self.send(handshake.hello, on: ws)
            let hello = try await Self.receive(on: ws, deadline: deadline)
            let accepted = try handshake.accept(hello)
            try await Self.send(accepted.finish, on: ws)
            var sessionCipher = accepted.cipher
            let ready = try sessionCipher.open(try await Self.receive(on: ws, deadline: deadline))
            guard generation == token, ready["type"].string == "ready", ready["desktop_id"].string == pairing.desktop_id, ready["device_id"].string == pairing.device_id else { throw RemoteError.protocolViolation("Desktop did not authenticate readiness.") }
            cipher = sessionCipher
            reader = Task { await self.readLoop(ws: ws, token: token) }
            keepAlive = Task { await self.keepAliveLoop(ws: ws, token: token) }
        } catch {
            let failure = await Self.explain(error, on: ws, current: generation == token)
            if generation == token { endConnection(error: failure) }
            throw failure
        }
    }
    public func disconnect() { endConnection(error: RemoteError.disconnected) }
    private func endConnection(error: any Error) {
        generation = UUID()
        reader?.cancel(); reader = nil
        keepAlive?.cancel(); keepAlive = nil
        sendTail?.cancel(); sendTail = nil
        socket?.cancel(with: .goingAway, reason: nil); socket = nil
        cipher = nil
        abandoned.removeAll()
        let outstanding = pending
        pending.removeAll()
        for item in outstanding.values {
            item.timeout.cancel()
            item.continuation.resume(throwing: item.isInput ? RemoteError.uncertainDelivery : error)
        }
    }
    public func request(method: String, params: [String: JSONValue] = [:], id: String = UUID().uuidString.lowercased()) async throws -> JSONValue {
        try RequestValidation.validate(method: method, params: params, id: id)
        // Before sealing: a cancelled caller must not consume a send counter.
        try Task.checkCancellation()
        guard let ws = socket, var activeCipher = cipher else { throw RemoteError.disconnected }
        guard pending[id] == nil, !abandoned.contains(id) else { throw RemoteError.protocolViolation("Request already pending.") }
        let envelope = try activeCipher.seal(.object(["v": .number(1), "type": .string("request"), "id": .string(id), "method": .string(method), "params": .object(params)]))
        cipher = activeCipher
        let token = generation
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                let deadline = Task { [requestTimeout] in
                    do { try await Task.sleep(for: requestTimeout) } catch { return }
                    self.expire(id: id, token: token)
                }
                pending[id] = Pending(continuation: continuation, isInput: method == "shell.input", timeout: deadline)
                let previous = sendTail
                // The counter is already spent, so this frame is sent even if its caller is cancelled.
                sendTail = Task {
                    await previous?.value
                    guard self.generation == token, !Task.isCancelled else { return }
                    do { try await Self.send(envelope, on: ws) }
                    catch { self.failIfCurrent(token: token, error: await Self.explain(error, on: ws, current: self.generation == token)) }
                }
            }
        } onCancel: { Task { await self.abandon(id: id, token: token) } }
    }
    /// Detaches one waiter. The socket, keys and every other request stay intact.
    private func abandon(id: String, token: UUID) {
        guard generation == token, let item = pending.removeValue(forKey: id) else { return }
        item.timeout.cancel()
        abandoned.insert(id)
        // The frame may already be on the wire, so an input can no longer be reported as "not sent".
        item.continuation.resume(throwing: item.isInput ? RemoteError.uncertainDelivery : CancellationError())
    }
    private func expire(id: String, token: UUID) { if generation == token, pending[id] != nil { endConnection(error: RemoteError.timeout) } }
    private func failIfCurrent(token: UUID, error: any Error) { if generation == token { endConnection(error: error) } }
    private func readLoop(ws: any WebSocketConnection, token: UUID) async {
        do {
            while !Task.isCancelled {
                let frame = try await Self.receive(on: ws)
                guard generation == token, var activeCipher = cipher else { return }
                // Relay peer controls can invalidate connectivity, never authenticate a payload.
                if frame["type"].string == "peer", frame["v"] == .number(1), frame["online"] == .bool(false) { throw RemoteError.disconnected }
                let response = try activeCipher.open(frame)
                cipher = activeCipher
                guard response["type"].string == "response", let id = response["id"].string, case .bool(let ok) = response["ok"] else { throw RemoteError.protocolViolation("Unexpected response.") }
                guard let item = pending.removeValue(forKey: id) else {
                    if abandoned.remove(id) != nil { continue }
                    throw RemoteError.protocolViolation("Unexpected response.")
                }
                item.timeout.cancel()
                if ok { item.continuation.resume(returning: response["result"]) }
                else {
                    let detail = response["error"]
                    item.continuation.resume(throwing: RemoteError.rpc(code: detail["code"].string ?? "invalid_response", message: detail["message"].string ?? "Desktop rejected the request."))
                }
            }
        } catch { failIfCurrent(token: token, error: await Self.explain(error, on: ws, current: generation == token)) }
    }
    /// The relay never pings a mobile socket, and URLSession treats silence as idle, so the phone pings.
    private func keepAliveLoop(ws: any WebSocketConnection, token: UUID) async {
        while !Task.isCancelled {
            do { try await Task.sleep(for: pingInterval) } catch { return }
            guard generation == token else { return }
            do { try await Self.ping(ws, timeout: pingInterval * 2) }
            catch {
                failIfCurrent(token: token, error: await Self.explain(error, on: ws, current: generation == token))
                return
            }
        }
    }
    /// Names relay close codes; the code can land a moment after the failing read or write.
    private static func explain(_ error: any Error, on ws: any WebSocketConnection, current: Bool) async -> any Error {
        guard current, !(error is RemoteError), !(error is CancellationError) else { return error }
        var code = ws.closeCode
        if code == .invalid { try? await Task.sleep(for: .milliseconds(50)); code = ws.closeCode }
        if code != .invalid { return RemoteError.relayClosed(code: code.rawValue, reason: ws.closeReason.flatMap { String(data: $0, encoding: .utf8) }) }
        if (error as? URLError)?.code == .timedOut { return RemoteError.timeout }
        return error
    }
    private static func send(_ payload: JSONValue, on ws: any WebSocketConnection) async throws {
        let data = try JSONEncoder().encode(payload)
        guard data.count <= 262144, let text = String(data: data, encoding: .utf8) else { throw RemoteError.protocolViolation("Frame too large.") }
        try await ws.send(text)
    }
    /// Runs `operation`, cancelling the socket if it outlives `seconds`. Cancelling is what fails a hung read or ping.
    private static func within<T: Sendable>(_ seconds: Double, on ws: any WebSocketConnection, _ operation: @escaping @Sendable () async throws -> T) async throws -> T {
        let fired = Fired()
        do {
            return try await withThrowingTaskGroup(of: T.self) { group in
                group.addTask { try await operation() }
                group.addTask {
                    try await Task.sleep(for: .seconds(seconds))
                    fired.set()
                    ws.cancel(with: .goingAway, reason: nil)
                    throw RemoteError.timeout
                }
                defer { group.cancelAll() }
                return try await group.next()!
            }
        } catch {
            // Our own socket cancel also fails the operation; report the cause, not the symptom.
            throw fired.value ? RemoteError.timeout : error
        }
    }
    private final class Fired: @unchecked Sendable {
        private let lock = NSLock()
        private var flag = false
        func set() { lock.withLock { flag = true } }
        var value: Bool { lock.withLock { flag } }
    }
    private static func ping(_ ws: any WebSocketConnection, timeout: Duration) async throws {
        try await within(timeout.seconds, on: ws) { try await ws.ping() }
    }
    private static func receive(on ws: any WebSocketConnection, deadline: Date? = nil) async throws -> JSONValue {
        let message: SocketMessage
        if let deadline {
            let remaining = deadline.timeIntervalSinceNow
            guard remaining > 0 else { throw RemoteError.timeout }
            message = try await within(remaining, on: ws) { try await ws.receive() }
        } else { message = try await ws.receive() }
        guard case .text(let text) = message, text.utf8.count <= 262144 else { throw RemoteError.protocolViolation("Expected bounded text frame.") }
        let result = try JSONDecoder().decode(JSONValue.self, from: Data(text.utf8))
        guard result["v"] == .number(1) else { throw RemoteError.protocolViolation("Unsupported relay version.") }
        return result
    }
}
