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

/// A single receive loop owns the WebSocket. Only authenticated ready enables RPCs.
/// Every reconnect throws away old keys; input is never retried by this transport.
public actor RelayClient: RemoteTransport {
    private let urlSession: URLSession
    private var socket: URLSessionWebSocketTask?
    private var cipher: SessionCipher?
    private var generation = UUID()
    private var reader: Task<Void, Never>?
    private var sendTail: Task<Void, Never>?
    private struct Pending {
        let continuation: CheckedContinuation<JSONValue, any Error>
        let isInput: Bool
        let timeout: Task<Void, Never>
    }
    private var pending: [String: Pending] = [:]
    private let requestTimeout: Duration
    public init(requestTimeout: Duration = .seconds(15)) {
        let config = URLSessionConfiguration.ephemeral
        config.urlCache = nil
        config.httpCookieStorage = nil
        config.timeoutIntervalForRequest = 15
        urlSession = URLSession(configuration: config)
        self.requestTimeout = requestTimeout
    }
    public func isConnected() -> Bool { cipher != nil && socket != nil }
    public func connect(pairing: Pairing, allowLocalDevelopment: Bool = false) async throws {
        disconnect()
        try pairing.validate(allowLocalDevelopment: allowLocalDevelopment)
        guard let url = URL(string: pairing.relay_url) else { throw RemoteError.invalidPairing("Invalid relay URL.") }
        let token = generation
        let ws = urlSession.webSocketTask(with: url)
        ws.maximumMessageSize = 262144
        socket = ws
        ws.resume()
        let deadline = Date().addingTimeInterval(10)
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
        } catch {
            if generation == token { endConnection(error: error) }
            throw error
        }
    }
    public func disconnect() { endConnection(error: RemoteError.disconnected) }
    private func endConnection(error: any Error) {
        generation = UUID()
        reader?.cancel(); reader = nil
        sendTail?.cancel(); sendTail = nil
        socket?.cancel(with: .goingAway, reason: nil); socket = nil
        cipher = nil
        let outstanding = pending
        pending.removeAll()
        for item in outstanding.values {
            item.timeout.cancel()
            item.continuation.resume(throwing: item.isInput ? RemoteError.uncertainDelivery : error)
        }
    }
    public func request(method: String, params: [String: JSONValue] = [:], id: String = UUID().uuidString.lowercased()) async throws -> JSONValue {
        try RequestValidation.validate(method: method, params: params, id: id)
        guard let ws = socket, var activeCipher = cipher else { throw RemoteError.disconnected }
        guard pending[id] == nil else { throw RemoteError.protocolViolation("Request already pending.") }
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
                sendTail = Task {
                    await previous?.value
                    guard self.generation == token, !Task.isCancelled else { return }
                    do { try await Self.send(envelope, on: ws) }
                    catch { self.failIfCurrent(token: token, error: error) }
                }
            }
        } onCancel: { Task { await self.failIfCurrent(token: token, error: CancellationError()) } }
    }
    private func expire(id: String, token: UUID) { if generation == token, pending[id] != nil { endConnection(error: RemoteError.timeout) } }
    private func failIfCurrent(token: UUID, error: any Error) { if generation == token { endConnection(error: error) } }
    private func readLoop(ws: URLSessionWebSocketTask, token: UUID) async {
        do {
            while !Task.isCancelled {
                let frame = try await Self.receive(on: ws)
                guard generation == token, var activeCipher = cipher else { return }
                // Relay peer controls can invalidate connectivity, never authenticate a payload.
                if frame["type"].string == "peer", frame["v"] == .number(1), frame["online"] == .bool(false) { throw RemoteError.disconnected }
                let response = try activeCipher.open(frame)
                cipher = activeCipher
                guard response["type"].string == "response", let id = response["id"].string, case .bool(let ok) = response["ok"], let item = pending.removeValue(forKey: id) else { throw RemoteError.protocolViolation("Unexpected response.") }
                item.timeout.cancel()
                if ok { item.continuation.resume(returning: response["result"]) }
                else {
                    let detail = response["error"]
                    item.continuation.resume(throwing: RemoteError.rpc(code: detail["code"].string ?? "invalid_response", message: detail["message"].string ?? "Desktop rejected the request."))
                }
            }
        } catch { failIfCurrent(token: token, error: error) }
    }
    private static func send(_ payload: JSONValue, on ws: URLSessionWebSocketTask) async throws {
        let data = try JSONEncoder().encode(payload)
        guard data.count <= 262144, let text = String(data: data, encoding: .utf8) else { throw RemoteError.protocolViolation("Frame too large.") }
        try await ws.send(.string(text))
    }
    private static func receive(on ws: URLSessionWebSocketTask, deadline: Date? = nil) async throws -> JSONValue {
        let message: URLSessionWebSocketTask.Message
        if let deadline {
            let remaining = deadline.timeIntervalSinceNow
            guard remaining > 0 else { throw RemoteError.timeout }
            message = try await withThrowingTaskGroup(of: URLSessionWebSocketTask.Message.self) { group in
                group.addTask { try await ws.receive() }
                group.addTask {
                    try await Task.sleep(for: .seconds(remaining))
                    ws.cancel(with: .goingAway, reason: nil)
                    throw RemoteError.timeout
                }
                defer { group.cancelAll() }
                return try await group.next()!
            }
        } else { message = try await ws.receive() }
        guard case .string(let text) = message, text.utf8.count <= 262144 else { throw RemoteError.protocolViolation("Expected bounded text frame.") }
        let result = try JSONDecoder().decode(JSONValue.self, from: Data(text.utf8))
        guard result["v"] == .number(1) else { throw RemoteError.protocolViolation("Unsupported relay version.") }
        return result
    }
}
