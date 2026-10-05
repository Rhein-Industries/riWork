import Foundation
import RiWorkCore
@testable import RiWorkRemote

/// A desktop with native chats, scripted: it keeps each chat's event log (sequence numbers from 1, as the host numbers them), answers
/// `chat.events` the way the real one does (at once when there is something after `since`, else after the wait), and records what it was
/// asked. A test appends events, makes requests fail, drops the connection or holds an answer back, and reads the calls.
actor ChatTransport: RemoteTransport {
    struct Call {
        let method: String
        let params: [String: JSONValue]
        let at: ContinuousClock.Instant
    }
    enum CreateMode { case ok, unsupported, notFound, timeout, harness, gated }
    static let project = "11111111-1111-4111-8111-111111111111"
    static let shell = "44444444-4444-4444-8444-444444444444"

    var connected = false
    var connections = 0
    var chatFeature = true
    var calls: [Call] = []
    /// The chats `chats.list` gives, as the wire has them.
    var chats: [ChatInfo] = []
    var log: [String: [JSONValue]] = [:]
    var createMode = CreateMode.ok
    /// The next this many `chat.events` fail with a CLI error.
    var eventFailures = 0
    /// The next `chat.command` fails with this.
    var commandError: RemoteError?
    var gatedCommands = false
    /// `chat.events` answers `not_found`.
    var chatsGone = false
    /// What the host does about a command: the events it appends.
    var onCommand: (@Sendable (String, ChatCommand) -> [ChatEvent])?
    /// What `appearance.get` gives; nil is "not published", and the built-in look.
    var appearance: JSONValue?
    private var created = 0

    init(chats: [ChatInfo] = [], appearance: JSONValue? = nil) { self.chats = chats; self.appearance = appearance }

    // MARK: Script

    func setFeature(_ on: Bool) { chatFeature = on }
    func setCreateMode(_ mode: CreateMode) { createMode = mode }
    func failEvents(_ count: Int) { eventFailures = count }
    func failCommand(_ error: RemoteError?) { commandError = error }
    func gateCommands(_ on: Bool) { gatedCommands = on }
    func setGone(_ gone: Bool) { chatsGone = gone }
    func setChats(_ list: [ChatInfo]) { chats = list }
    func handleCommands(_ handler: (@Sendable (String, ChatCommand) -> [ChatEvent])?) { onCommand = handler }
    func drop() { connected = false }
    func append(_ chat: String, _ events: [ChatEvent]) {
        for event in events { log[chat, default: []].append((try? JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(event))) ?? .null) }
    }
    /// A line the phone cannot read, where an event would be.
    func appendRaw(_ chat: String, _ json: String) {
        log[chat, default: []].append((try? JSONDecoder().decode(JSONValue.self, from: Data(json.utf8))) ?? .null)
    }

    // MARK: What happened

    func count(_ method: String) -> Int { calls.filter { $0.method == method }.count }
    func params(of method: String) -> [[String: JSONValue]] { calls.filter { $0.method == method }.map(\.params) }
    func calls(of method: String) -> [Call] { calls.filter { $0.method == method } }
    func sinces() -> [Int] {
        params(of: "chat.events").map { params in
            if case .number(let number)? = params["since"] { Int(number) } else { -1 }
        }
    }
    func commands() -> [JSONValue] { params(of: "chat.command").map { $0["command"] ?? .null } }

    // MARK: RemoteTransport

    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { connected = true; connections += 1; return pairing }
    func disconnect() async { connected = false }
    func isConnected() async -> Bool { connected }
    func desktopFeatures() async -> DesktopFeatures {
        chatFeature ? DesktopFeatures(ready: .object(["features": .object(["chat": .bool(true)])])) : DesktopFeatures()
    }

    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        guard connected else { throw RemoteError.disconnected }
        try RequestValidation.validate(method: method, params: params, id: id)
        if method != "appearance.get" { calls.append(Call(method: method, params: params, at: .now)) }
        switch method {
        case "projects.list":
            return .object(["projects": try JSONDecoder().decode(JSONValue.self, from: Data("[{\"id\":\"\(Self.project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}]".utf8))])
        case "worktrees.list": return .object(["worktrees": .array([])])
        case "orchestrators.list": return .object(["orchestrators": .array([])])
        case "shells.list":
            let entry = "[{\"id\":\"\(Self.shell)\",\"project_id\":\"\(Self.project)\",\"kind\":\"project\",\"cwd\":\"/fixture\",\"harness\":null,\"alive\":true,\"created_at_unix\":5}]"
            return .object(["shells": try JSONDecoder().decode(JSONValue.self, from: Data(entry.utf8))])
        case "shell.output": return .object(["shell_id": params["shell_id"]!, "output": .string("screen")])
        case "shell.resize": return .object(["shell_id": params["shell_id"]!, "columns": params["columns"]!, "rows": params["rows"]!])
        case "shell.resize.clear": return .object(["shell_id": params["shell_id"]!, "status": .string("cleared")])
        case "appearance.get":
            if let appearance { return appearance }
            throw RemoteError.rpc(code: "not_found", message: "appearance not published")
        case "chats.list":
            guard chatFeature else { throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method") }
            return .object(["chats": try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(chats))])
        case "chat.create":
            while createMode == .gated { try await Task.sleep(for: .milliseconds(3)) }
            switch createMode {
            case .unsupported: throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")
            case .notFound: throw RemoteError.rpc(code: "not_found", message: "project not found")
            case .timeout: throw RemoteError.timeout
            case .harness: throw RemoteError.rpc(code: "harness_unavailable", message: "claude is not on PATH")
            case .ok, .gated:
                created += 1
                let provider = ChatProvider(rawValue: params["provider"]?.string ?? "codex") ?? .codex
                let mode = ChatApprovalMode(rawValue: params["approval_mode"]?.string ?? "") ?? .supervised
                let info = ChatInfo(id: String(format: "cccccccc-0000-4000-8000-%012d", created), provider: provider, projectID: params["project_id"]?.string, worktreeID: params["worktree_id"]?.string,
                                    cwd: "/fixture", title: provider.chatTitle, createdAtUnix: 100 + UInt64(created), approvalMode: mode, state: .starting)
                chats.append(info)
                return .object(["chat": try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(info))])
            }
        case "chat.events":
            guard chatFeature else { throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method") }
            return try await events(params)
        case "chat.command":
            while gatedCommands { try await Task.sleep(for: .milliseconds(3)) }
            if let commandError { self.commandError = nil; throw commandError }
            if let chat = params["chat_id"]?.string, let command = try? params["command"]?.decode(ChatCommand.self), let onCommand { append(chat, onCommand(chat, command)) }
            return .object(["status": .string("ok")])
        case "chat.stop": return .object(["status": .string("stopped")])
        default: throw RemoteError.protocolViolation("Unknown method \(method)")
        }
    }

    /// `chat.events`: what is after `since` at once, else what arrives within the wait, else nothing.
    private func events(_ params: [String: JSONValue]) async throws -> JSONValue {
        guard let chat = params["chat_id"]?.string, case .number(let sinceNumber)? = params["since"], case .number(let waitNumber)? = params["wait_ms"] else {
            throw RemoteError.rpc(code: "invalid_request", message: "bad request")
        }
        if chatsGone || (!chats.contains { $0.id == chat } && log[chat] == nil) { throw RemoteError.rpc(code: "not_found", message: "unknown chat") }
        if eventFailures > 0 { eventFailures -= 1; throw RemoteError.rpc(code: "cli_error", message: "the chat host is not answering") }
        let since = Int(sinceNumber)
        let deadline = ContinuousClock.now + .milliseconds(Int(waitNumber))
        let limit: Int = { if case .number(let n)? = params["max_events"] { Int(n) } else { 500 } }()
        while true {
            guard connected else { throw RemoteError.disconnected }
            let all = log[chat] ?? []
            if all.count > since {
                let page = Array(all[since..<min(all.count, since + limit)].enumerated())
                let more = since + page.count < all.count
                return .object(["chat_id": .string(chat), "events": .array(page.map { .object(["seq": .number(Double(since + $0.offset + 1)), "event": $0.element]) }),
                                "next": .number(Double(since + page.count)), "more": .bool(more)])
            }
            if ContinuousClock.now >= deadline {
                return .object(["chat_id": .string(chat), "events": .array([]), "next": .number(Double(since)), "more": .bool(false)])
            }
            try await Task.sleep(for: .milliseconds(5))
        }
    }
}
