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
        case "projects.list", "orchestrators.list", "appearance.get": required = []; optional = []
        case "link.configure": required = []; optional = ["compression"]
        case "worktrees.list", "shells.list": required = ["project_id"]; optional = []
        case "tasks.list": required = ["project_id"]; optional = ["worktree_id"]
        case "shell.output": required = ["shell_id"]; optional = ["lines", "styled", "if_changed", "wait_ms"]
        case "shell.history": required = ["shell_id", "end", "lines"]; optional = ["styled"]
        case "shell.input": required = ["shell_id", "line"]; optional = []
        case "shell.keys": required = ["shell_id", "batch", "items"]; optional = []
        case "shell.resize": required = ["shell_id", "columns", "rows"]; optional = []
        case "shell.resize.clear": required = ["shell_id"]; optional = []
        case "shell.create": required = ["kind"]; optional = ["project_id", "worktree_id", "unrestricted", "command"]
        case "shell.close": required = ["shell_id"]; optional = []
        case "project.create": required = ["name"]; optional = ["git"]
        case "orchestrator.create": required = []; optional = ["project_id"]
        case "chats.list": required = []; optional = ["project_id"]
        case "chat.create": required = ["provider"]; optional = ["project_id", "worktree_id", "approval_mode", "model", "effort", "fast", "title"]
        case "chat.events": required = ["chat_id", "since", "wait_ms"]; optional = ["max_events"]
        case "chat.command": required = ["chat_id", "command"]; optional = []
        case "chat.stop": required = ["chat_id"]; optional = []
        case "upload.begin", "upload.chunk", "upload.finish", "upload.cancel", "shell.paste":
            (required, optional) = UploadRequests.methods[method] ?? ([], [])
        default: throw RemoteError.protocolViolation("Unsupported operation.")
        }
        let keys = Set(params.keys)
        guard required.isSubset(of: keys), keys.isSubset(of: required.union(optional)) else { throw RemoteError.protocolViolation("Invalid request parameters.") }
        for key in ["project_id", "worktree_id", "shell_id", "batch", "chat_id", "upload"] where params[key] != nil { try uuid(params[key]?.string) }
        try UploadRequests.validate(method: method, params: params, uuid: uuid)
        if method == "shell.keys" {
            guard case .array(let raw)? = params["items"] else { throw RemoteError.protocolViolation("Missing key items.") }
            try KeyItem.validate(batch: try raw.map { try KeyItem(json: $0) })
        }
        if method == "shell.output" {
            if let styled = params["styled"], case .bool = styled {} else if params["styled"] != nil { throw RemoteError.protocolViolation("Output styled must be a boolean.") }
            if let changed = params["if_changed"] { guard case .string(let hash) = changed, LiveSync.isUsableHash(hash) else { throw RemoteError.protocolViolation("Output if_changed must be a short printable string.") } }
            if let wait = params["wait_ms"] { guard case .number(let value) = wait, value >= 0, value <= Double(LiveSync.maximumWaitMilliseconds), value.rounded() == value else { throw RemoteError.protocolViolation("Output wait_ms must be 0–10000.") } }
        }
        if method == "link.configure", let mode = params["compression"] {
            guard case .string(let value) = mode, value == "deflate" || value == "none" else { throw RemoteError.protocolViolation("Link compression must be deflate or none.") }
        }
        if method == "shell.history" {
            guard case .number(let end)? = params["end"], end >= 0, end <= 4_294_967_295, end.rounded() == end else { throw RemoteError.protocolViolation("History end must be 0–4294967295.") }
            guard case .number(let lines)? = params["lines"], lines >= 1, lines <= Double(HistoryLimits.maximumPageLines), lines.rounded() == lines else { throw RemoteError.protocolViolation("History lines must be 1–5000.") }
            if let styled = params["styled"], case .bool = styled {} else if params["styled"] != nil { throw RemoteError.protocolViolation("History styled must be a boolean.") }
        }
        if method == "shell.create" {
            do { _ = try NewTerminalRequest(params: params) } catch { throw RemoteError.protocolViolation(error.localizedDescription) }
        }
        if method == "project.create" {
            do { _ = try NewProjectRequest(params: params) } catch { throw RemoteError.protocolViolation(error.localizedDescription) }
        }
        if method == "orchestrator.create" {
            do { _ = try NewOrchestratorRequest(params: params) } catch { throw RemoteError.protocolViolation(error.localizedDescription) }
        }
        do {
            switch method {
            case "chats.list": _ = try ChatListRequest(params: params)
            case "chat.create": _ = try ChatCreateRequest(params: params)
            case "chat.events": _ = try ChatEventsRequest(params: params)
            case "chat.command": _ = try ChatCommandRequest(params: params)
            default: break
            }
        } catch { throw RemoteError.protocolViolation(error.localizedDescription) }
        if method == "shell.input" { guard let line = params["line"]?.string else { throw RemoteError.protocolViolation("Missing input.") }; try InputValidation.validate(line) }
        // `shell.history` has its own range (checked above); this one is the live read's.
        if let lines = params["lines"], method == "shell.output" { guard case .number(let value) = lines, value >= 1, value <= 2000, value.rounded() == value else { throw RemoteError.protocolViolation("Output lines must be 1–2000.") } }
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
    private var uploadIdentity: UploadConnectionIdentity?
    private var reader: Task<Void, Never>?
    private var keepAlive: Task<Void, Never>?
    private var sendTail: Task<Void, Never>?
    private struct Pending {
        let continuation: CheckedContinuation<TimedReply, any Error>
        let isInput: Bool
        let timeout: Task<Void, Never>
        /// When the frame went to the socket (the start of the reply's timing); nil until it does.
        var sentAt: ContinuousClock.Instant?
    }
    private var pending: [String: Pending] = [:]
    /// Replies that arrived lately: when and how big. A reply that shared the socket with others took their share of the time.
    private var arrivals: [(at: ContinuousClock.Instant, bytes: Int)] = []
    private static let arrivalMemory: Duration = .seconds(60)
    /// What the desktop announced in `ready`, and whether it has agreed to compress (it does once it answers `link.configure`).
    private var features = DesktopFeatures()
    private var compressing = false
    private var wantsCompression = true
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
    /// `shell.keys` may pause ~150 ms between a text and the key after it (about 5 s for a 64-item batch), so it never
    /// gets less than 10 s: a timeout tears the whole connection down. A `shell.output` that asks the desktop to wait for a
    /// change (`wait_ms`) gets that wait plus 20 s (the connector gives up after the wait plus 8 s), for the same reason.
    static func timeout(for method: String, params: [String: JSONValue] = [:], default base: Duration) -> Duration {
        switch method {
        case "shell.keys": return max(base, .seconds(10))
        // Starting an agent can take 20 to 30 s on the desktop (it prepares its tools first); closing waits for tmux.
        case "shell.create": return max(base, .seconds(90))
        case "shell.close": return max(base, .seconds(30))
        // The desktop makes the folder, runs `git init` and registers it (the connector gives its CLI 60 s).
        case "project.create": return max(base, .seconds(90))
        // An orchestrator that is not there yet is started like any agent: the answer can take 20 to 30 s.
        case "orchestrator.create": return max(base, .seconds(90))
        // A chat starts its agent (and the chat host, if it is not running yet); a message to a stopped chat resumes it first. Like
        // `shell.create`, the answer can take a while, and a timeout here tears the whole connection down.
        case "chat.create": return max(base, .seconds(90))
        case "chat.command": return max(base, .seconds(60))
        case "chat.stop": return max(base, .seconds(30))
        // A file for a shell first asks the CLI and the shell list; finishing hashes the whole file; a paste waits for the shell's lock.
        case "upload.begin", "upload.finish", "shell.paste": return max(base, .seconds(30))
        case "chat.events":
            if case .number(let wait)? = params["wait_ms"], wait.isFinite, wait > 0 { return max(base, ChatLimits.timeout(waitMilliseconds: Int(min(wait, Double(ChatLimits.maximumWaitMilliseconds))))) }
            return base
        case "shell.output":
            if case .number(let wait)? = params["wait_ms"], wait.isFinite, wait > 0 { return max(base, LiveSync.timeout(waitMilliseconds: Int(min(wait, Double(LiveSync.maximumWaitMilliseconds))))) }
            return base
        default: return base
        }
    }
    public func isConnected() -> Bool { cipher != nil && socket != nil }
    @discardableResult
    public func connect(pairing: Pairing, allowLocalDevelopment: Bool = false) async throws -> Pairing {
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
            let started: (Pairing, SessionCipher)
            if pairing.v == 1 { started = try await handshakeV1(pairing, on: ws, deadline: deadline) }
            else if pairing.v == 2 { started = try await handshakeV2(pairing, on: ws, deadline: deadline) }
            else { throw RemoteError.invalidPairing("Unsupported version \(pairing.v).") }
            var sessionCipher = started.1
            let ready = try sessionCipher.open(try await Self.receive(on: ws, deadline: deadline))
            guard generation == token, ready["type"].string == "ready", ready["desktop_id"].string == pairing.desktop_id, ready["device_id"].string == pairing.device_id else { throw RemoteError.protocolViolation("Desktop did not authenticate readiness.") }
            uploadIdentity = UploadConnectionIdentity(pairing: started.0)
            cipher = sessionCipher
            features = DesktopFeatures(ready: ready)
            compressing = false
            reader = Task { await self.readLoop(ws: ws, token: token) }
            keepAlive = Task { await self.keepAliveLoop(ws: ws, token: token) }
            if wantsCompression, features.deflate { askForCompression(true) }
            return started.0
        } catch {
            let failure = await Self.explain(error, on: ws, current: generation == token)
            if generation == token { endConnection(error: failure) }
            throw failure
        }
    }
    private func handshakeV1(_ pairing: Pairing, on ws: any WebSocketConnection, deadline: Date) async throws -> (Pairing, SessionCipher) {
        let handshake = try ClientHandshake(pairing: pairing)
        try await Self.send(handshake.hello, on: ws)
        let hello = try await Self.receive(on: ws, deadline: deadline)
        let accepted = try handshake.accept(hello)
        try await Self.send(accepted.finish, on: ws)
        return (pairing, accepted.cipher)
    }
    /// Redeems a pending invite on this socket, then opens a fresh X25519 session. An established root skips the invite.
    private func handshakeV2(_ pairing: Pairing, on ws: any WebSocketConnection, deadline: Date) async throws -> (Pairing, SessionCipher) {
        var current = pairing
        if current.root_key == nil {
            let invite = try V2Invite(pairing: current)
            try await Self.send(invite.hello, on: ws)
            let response = try await Self.receive(on: ws, deadline: deadline)
            if let failure = Self.pairingFailure(response) { throw failure }
            let accepted = try invite.accept(response)
            try await Self.send(accepted.finish, on: ws)
            current = accepted.established
        }
        let session = try V2SessionHandshake(pairing: current)
        try await Self.send(session.hello, on: ws)
        let server = try await Self.receive(on: ws, deadline: deadline)
        if let failure = Self.pairingFailure(server) { throw failure }
        let accepted = try session.accept(server)
        try await Self.send(accepted.finish, on: ws)
        return (current, accepted.cipher)
    }
    /// Known pairing failures only. Anything else is a generic rejection so a peer cannot put secrets in the message.
    private static func pairingFailure(_ frame: JSONValue) -> RemoteError? {
        guard frame["type"].string == "pair_error" else { return nil }
        switch frame["error"].string {
        case "invite_expired", "invite_replay", "invite_race", "invite_rejected", "invite_malformed":
            return .protocolViolation("Pairing was rejected (\(frame["error"].string ?? "")).")
        default:
            return .protocolViolation("Pairing was rejected.")
        }
    }
    public func disconnect() { endConnection(error: RemoteError.disconnected) }
    private func endConnection(error: any Error) {
        uploadIdentity = nil
        generation = UUID()
        reader?.cancel(); reader = nil
        keepAlive?.cancel(); keepAlive = nil
        sendTail?.cancel(); sendTail = nil
        socket?.cancel(with: .goingAway, reason: nil); socket = nil
        cipher = nil
        abandoned.removeAll()
        arrivals.removeAll()
        compressing = false
        features = DesktopFeatures()
        let outstanding = pending
        pending.removeAll()
        for item in outstanding.values {
            item.timeout.cancel()
            item.continuation.resume(throwing: item.isInput ? RemoteError.uncertainDelivery : error)
        }
    }
    public func request(method: String, params: [String: JSONValue] = [:], id: String = UUID().uuidString.lowercased()) async throws -> JSONValue {
        try await timedRequest(method: method, params: params, id: id).value
    }
    public func request(method: String, params: [String: JSONValue], id: String, boundTo identity: UploadConnectionIdentity) async throws -> JSONValue {
        guard let uploadIdentity else { throw RemoteError.disconnected }
        guard uploadIdentity == identity else { throw CancellationError() }
        // No suspension between the identity check and timedRequest sealing on this actor.
        return try await timedRequest(method: method, params: params, id: id).value
    }
    // `async` although nothing here suspends: on the concrete type a sync actor method loses to the protocol's async default.
    public func desktopFeatures() async -> DesktopFeatures { features }
    public func compressionActive() async -> Bool { compressing }
    /// Whether to ask the desktop for compressed replies. Frames that arrive compressed are always understood; this decides only
    /// whether the desktop is asked to send them (on a desktop that offers it, from the start of the next session, or now).
    public func setCompression(_ enabled: Bool) async {
        guard wantsCompression != enabled else { return }
        wantsCompression = enabled
        guard cipher != nil, features.deflate else { return }
        // Unlike the ask at connect, this waits for the answer, so `compressionActive()` is true to the desktop's word when it returns.
        let token = generation
        let reply = try? await timedRequest(method: "link.configure", params: ["compression": .string(enabled ? "deflate" : "none")], id: UUID().uuidString.lowercased())
        compressionAnswered(reply?.value, token: token)
    }
    /// Sends `link.configure` and notes the answer. Nobody waits for it: an older desktop does not offer the feature and is not asked, and
    /// whatever the answer, replies are understood in both forms.
    private func askForCompression(_ enabled: Bool) {
        let token = generation
        Task {
            let reply = try? await self.timedRequest(method: "link.configure", params: ["compression": .string(enabled ? "deflate" : "none")], id: UUID().uuidString.lowercased())
            self.compressionAnswered(reply?.value, token: token)
        }
    }
    private func compressionAnswered(_ result: JSONValue?, token: UUID) {
        guard generation == token else { return }
        compressing = result?["compression"].string == "deflate"
    }
    public func timedRequest(method: String, params: [String: JSONValue] = [:], id: String = UUID().uuidString.lowercased()) async throws -> TimedReply {
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
                let deadline = Task { [requestTimeout = Self.timeout(for: method, params: params, default: requestTimeout)] in
                    do { try await Task.sleep(for: requestTimeout) } catch { return }
                    self.expire(id: id, token: token)
                }
                // Only `shell.input` reports an unknown outcome as uncertain. `shell.keys` carries a batch id the desktop dedupes,
                // so a lost connection or cancelled caller surfaces as itself and the batch is simply retried.
                pending[id] = Pending(continuation: continuation, isInput: method == "shell.input", timeout: deadline)
                let previous = sendTail
                // The counter is already spent, so this frame is sent even if its caller is cancelled.
                sendTail = Task {
                    await previous?.value
                    guard self.generation == token, !Task.isCancelled else { return }
                    self.sending(id: id)
                    do { try await Self.send(envelope, on: ws) }
                    catch { self.failIfCurrent(token: token, error: await Self.explain(error, on: ws, current: self.generation == token)) }
                }
            }
        } onCancel: { Task { await self.abandon(id: id, token: token) } }
    }
    /// The request's frame is about to go to the socket: its timing starts here, not when the caller asked (it may queue behind others).
    private func sending(id: String) { pending[id]?.sentAt = .now }
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
                let received = try await Self.receiveFrame(on: ws)
                let frame = received.value
                guard generation == token, var activeCipher = cipher else { return }
                // Relay peer controls can invalidate connectivity, never authenticate a payload.
                if frame["type"].string == "peer", frame["v"] == .number(1), frame["online"] == .bool(false) { throw RemoteError.disconnected }
                let opened = try activeCipher.openFrame(frame)
                cipher = activeCipher
                let response = opened.payload
                guard response["type"].string == "response", let id = response["id"].string, case .bool(let ok) = response["ok"] else { throw RemoteError.protocolViolation("Unexpected response.") }
                let timing = timing(of: opened, received: received, response: response, sentAt: pending[id]?.sentAt)
                guard let item = pending.removeValue(forKey: id) else {
                    if abandoned.remove(id) != nil { continue }
                    throw RemoteError.protocolViolation("Unexpected response.")
                }
                item.timeout.cancel()
                if ok { item.continuation.resume(returning: TimedReply(value: response["result"], timing: timing)) }
                else {
                    let detail = response["error"]
                    item.continuation.resume(throwing: RemoteError.rpc(code: detail["code"].string ?? "invalid_response", message: detail["message"].string ?? "Desktop rejected the request."))
                }
            }
        } catch { failIfCurrent(token: token, error: await Self.explain(error, on: ws, current: generation == token)) }
    }
    /// How a reply travelled. Every reply is noted for the ones that overlap it, whoever asked for it.
    private func timing(of opened: SessionCipher.OpenedFrame, received: Received, response: JSONValue, sentAt: ContinuousClock.Instant?) -> ReplyTiming? {
        defer {
            arrivals.append((received.at, received.bytes))
            let horizon = received.at - Self.arrivalMemory
            arrivals.removeAll { $0.at < horizon }
        }
        guard let sentAt else { return nil }
        var concurrent = 0, replies = 0
        for earlier in arrivals where earlier.at > sentAt && earlier.at <= received.at { concurrent += earlier.bytes; replies += 1 }
        var server: Double?
        if case .number(let ms) = response["server_ms"], ms.isFinite, ms >= 0, ms <= 3_600_000 { server = ms / 1000 }
        return ReplyTiming(elapsed: max(0, (received.at - sentAt).seconds), serverSeconds: server, wireBytes: received.bytes, sealedBytes: opened.sealedBytes,
                           jsonBytes: opened.jsonBytes, compressed: opened.compressed, concurrentBytes: concurrent, concurrentReplies: replies)
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
        try await receiveFrame(on: ws, deadline: deadline).value
    }
    /// A frame, the size it had on the socket, and the moment it arrived whole (taken here, before anything else can delay it).
    private struct Received: Sendable { let value: JSONValue; let bytes: Int; let at: ContinuousClock.Instant }
    private static func receiveFrame(on ws: any WebSocketConnection, deadline: Date? = nil) async throws -> Received {
        let message: SocketMessage
        if let deadline {
            let remaining = deadline.timeIntervalSinceNow
            guard remaining > 0 else { throw RemoteError.timeout }
            message = try await within(remaining, on: ws) { try await ws.receive() }
        } else { message = try await ws.receive() }
        let arrived = ContinuousClock.now
        guard case .text(let text) = message, text.utf8.count <= 262144 else { throw RemoteError.protocolViolation("Expected bounded text frame.") }
        let result = try JSONDecoder().decode(JSONValue.self, from: Data(text.utf8))
        guard result["v"] == .number(1) || result["v"] == .number(2) else { throw RemoteError.protocolViolation("Unsupported relay version.") }
        return Received(value: result, bytes: text.utf8.count, at: arrived)
    }
}
