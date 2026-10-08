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
    /// What `orchestrator.create` does: the host opens (or starts) the orchestrator, refuses, goes quiet, answers with something else, or
    /// holds the answer until the test lets go.
    enum OrchestratorMode { case ok, unsupported, notFound, timeout, garbled, gated }
    static let project = "11111111-1111-4111-8111-111111111111"
    static let shell = "44444444-4444-4444-8444-444444444444"

    var connected = false
    var connections = 0
    var chatFeature = true
    /// `ready.features.orchestrator_create`.
    var orchestratorFeature = false
    /// `ready.features.chat_provider_switch`, `chat_models` and `shell_create_as_settings`.
    var switchFeature = false
    var modelsFeature = false
    var asSettingsFeature = false
    /// What `chat.models` lists per provider (nothing unless a test says), and the `error` it gives for each.
    var providerModels: [ChatProvider: [ChatModelOption]] = [:]
    var providerModelsErrors: [ChatProvider: String] = [:]
    var orchestratorMode = OrchestratorMode.ok
    /// A new orchestrator runs as a chat (`mode: "chat"` with a `chat_id`) rather than in a terminal.
    var newOrchestratorsAreChats = true
    private var orchestratorsMade = 0
    var calls: [Call] = []
    /// The chats `chats.list` gives, as the wire has them.
    var chats: [ChatInfo] = []
    var log: [String: [JSONValue]] = [:]
    var createMode = CreateMode.ok
    /// The next this many `chat.events` fail with a CLI error.
    var eventFailures = 0
    var eventsGated = false
    /// The next `chat.command` fails with this.
    var commandError: RemoteError?
    var gatedCommands = false
    /// `chat.events` answers `not_found`.
    var chatsGone = false
    /// The entries `orchestrators.list` gives, as the wire has them (none unless a test says).
    var orchestratorEntries: [JSONValue] = []
    /// What the host does about a command: the events it appends.
    var onCommand: (@Sendable (String, ChatCommand) -> [ChatEvent])?
    /// What `appearance.get` gives; nil is "not published", and the built-in look.
    var appearance: JSONValue?
    /// What `shell.output` gives for the fixture shell.
    var shellOutput = "screen"
    private var created = 0

    init(chats: [ChatInfo] = [], appearance: JSONValue? = nil) { self.chats = chats; self.appearance = appearance }

    // MARK: Script

    func setFeature(_ on: Bool) { chatFeature = on }
    func setAppearance(_ value: JSONValue?) { appearance = value }
    func setShellOutput(_ text: String) { shellOutput = text }
    func setOrchestratorFeature(_ on: Bool) { orchestratorFeature = on }
    /// A desktop that lets a chat go on with the other provider and lists a provider's models.
    func setSwitchFeatures(_ on: Bool) { switchFeature = on; modelsFeature = on }
    func setAsSettingsFeature(_ on: Bool) { asSettingsFeature = on }
    func setProviderModels(_ provider: ChatProvider, _ models: [ChatModelOption], error: String? = nil) { providerModels[provider] = models; providerModelsErrors[provider] = error }
    func setOrchestratorMode(_ mode: OrchestratorMode) { orchestratorMode = mode }
    func setNewOrchestratorsAreChats(_ on: Bool) { newOrchestratorsAreChats = on }
    func setCreateMode(_ mode: CreateMode) { createMode = mode }
    func gateEvents(_ on: Bool) { eventsGated = on }
    func failEvents(_ count: Int) { eventFailures = count }
    func failCommand(_ error: RemoteError?) { commandError = error }
    func gateCommands(_ on: Bool) { gatedCommands = on }
    func setGone(_ gone: Bool) { chatsGone = gone }
    func setChats(_ list: [ChatInfo]) { chats = list }
    /// The orchestrators, each a JSON object as the desktop writes it.
    func setOrchestrators(_ entries: [String]) { orchestratorEntries = entries.map { (try? JSONDecoder().decode(JSONValue.self, from: Data($0.utf8))) ?? .null } }
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
        var features: [String: JSONValue] = [:]
        if chatFeature { features["chat"] = .bool(true) }
        if orchestratorFeature { features["orchestrator_create"] = .bool(true) }
        if switchFeature { features["chat_provider_switch"] = .bool(true) }
        if modelsFeature { features["chat_models"] = .bool(true) }
        if asSettingsFeature { features["shell_create_as_settings"] = .bool(true) }
        return features.isEmpty ? DesktopFeatures() : DesktopFeatures(ready: .object(["features": .object(features)]))
    }

    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        guard connected else { throw RemoteError.disconnected }
        try RequestValidation.validate(method: method, params: params, id: id)
        if method != "appearance.get" { calls.append(Call(method: method, params: params, at: .now)) }
        switch method {
        case "projects.list":
            return .object(["projects": try JSONDecoder().decode(JSONValue.self, from: Data("[{\"id\":\"\(Self.project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}]".utf8))])
        case "worktrees.list": return .object(["worktrees": .array([])])
        case "orchestrators.list": return .object(["orchestrators": .array(orchestratorEntries)])
        case "shells.list":
            let entry = "[{\"id\":\"\(Self.shell)\",\"project_id\":\"\(Self.project)\",\"kind\":\"project\",\"cwd\":\"/fixture\",\"harness\":null,\"alive\":true,\"created_at_unix\":5}]"
            return .object(["shells": try JSONDecoder().decode(JSONValue.self, from: Data(entry.utf8))])
        case "shell.output": return .object(["shell_id": params["shell_id"]!, "output": .string(shellOutput)])
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
        case "chat.snapshot": return try await snapshot(params)
        case "chat.events":
            guard chatFeature else { throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method") }
            return try await events(params)
        case "chat.command":
            while gatedCommands { try await Task.sleep(for: .milliseconds(3)) }
            if let commandError { self.commandError = nil; throw commandError }
            if let chat = params["chat_id"]?.string, let command = try? params["command"]?.decode(ChatCommand.self), let onCommand { append(chat, onCommand(chat, command)) }
            return .object(["status": .string("ok")])
        case "chat.stop": return .object(["status": .string("stopped")])
        case "chat.models":
            guard modelsFeature else { throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method") }
            let provider = ChatProvider(rawValue: params["provider"]?.string ?? "") ?? .codex
            let models = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(providerModels[provider] ?? []))
            return .object(["provider": .string(provider.rawValue), "models": models, "configured": .array([]), "account_label": .null,
                            "error": providerModelsErrors[provider].map { .string($0) } ?? .null])
        case "orchestrator.create":
            guard orchestratorFeature else { throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method") }
            return try await createOrchestrator(params)
        default: throw RemoteError.protocolViolation("Unknown method \(method)")
        }
    }

    /// `orchestrator.create`: the orchestrator of the scope if the host has one (`created: false`), otherwise a new one.
    private func createOrchestrator(_ params: [String: JSONValue]) async throws -> JSONValue {
        while orchestratorMode == .gated { try await Task.sleep(for: .milliseconds(3)) }
        switch orchestratorMode {
        case .unsupported: throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")
        case .notFound: throw RemoteError.rpc(code: "not_found", message: "project not found")
        case .timeout: throw RemoteError.timeout
        case .garbled: return .object(["orchestrator": .object(["id": .string("nope")]), "created": .bool(true)])
        case .ok, .gated:
            let project = params["project_id"]?.string
            if let existing = orchestratorEntries.first(where: { entry in
                guard entry["kind"].string == "orchestrator" else { return false }
                return entry["project_id"].string == project
            }) {
                return .object(["orchestrator": existing, "created": .bool(false)])
            }
            orchestratorsMade += 1
            let id = String(format: "aaaaaaaa-0000-4000-8000-%012d", orchestratorsMade)
            let chat = String(format: "cccccccc-0000-4000-8000-%012d", 500 + orchestratorsMade)
            var entry: [String: JSONValue] = [
                "id": .string(id), "project_id": project.map { .string($0) } ?? .null, "worktree_id": .null, "kind": .string("orchestrator"),
                "cwd": .string("/fixture"), "harness": .null, "alive": .bool(true), "created_at_unix": .number(Double(40 + orchestratorsMade))
            ]
            if newOrchestratorsAreChats {
                entry["mode"] = .string("chat"); entry["chat_id"] = .string(chat); entry["provider"] = .string("claude")
                log[chat] = log[chat] ?? []
            }
            orchestratorEntries.append(.object(entry))
            return .object(["orchestrator": .object(entry), "created": .bool(true)])
        }
    }

    /// `chat.events`: what is after `since` at once, else what arrives within the wait, else nothing.
    var snapshotsEnabled = false
    var snapshotGated = false
    var historyGated = false
    var boundedGateAfter: Int?
    func gateHistory(_ on: Bool) { historyGated = on }
    func gateBoundedReplay(after: Int?) { boundedGateAfter = after }
    enum BoundedReplyFailure { case advancingEmptyPage, connectorInvalidPage, transientCLI, transientNetwork }
    var boundedReplyFailure: BoundedReplyFailure?
    var boundedReplyFailures = 0
    func failBoundedReplies(_ failure: BoundedReplyFailure) { boundedReplyFailure = failure; boundedReplyFailures = 0 }
    func boundedFailures() -> Int { boundedReplyFailures }
    var resourceLimits = false
    var completeSizeFailures = 0
    func rejectedCompleteEvents() -> Int { completeSizeFailures }
    var cappedLog = false
    var historyExpired = false
    func enforceResourceLimits(cappedLog: Bool = false) { resourceLimits = true; self.cappedLog = cappedLog }
    func expireHistory() { historyExpired = true }
    func truncate(_ id: String, to count: Int) { log[id] = Array((log[id] ?? []).prefix(count)) }
    func enableSnapshots(_ on: Bool = true) { snapshotsEnabled = on }
    func gateSnapshots(_ on: Bool) { snapshotGated = on }
    private func snapshot(_ params: [String: JSONValue]) async throws -> JSONValue {
        guard snapshotsEnabled else { throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method") }
        while snapshotGated || (historyGated && params["before"] != nil) { try await Task.sleep(for: .milliseconds(5)) }
        if cappedLog { throw RemoteError.rpc(code: "snapshot_limit", message: "snapshot file limit exceeded") }
        if params["cursor"] != nil, historyExpired { historyExpired = false; throw RemoteError.rpc(code: "snapshot_expired", message: "snapshot expired") }
        let id = params["chat_id"]!.string!
        let all = log[id] ?? []
        let next = params["cursor"]?.string.flatMap(Int.init) ?? all.count
        guard next <= all.count else { throw RemoteError.rpc(code: "snapshot_expired", message: "snapshot expired") }
        let before = params["before"].flatMap { if case .number(let n) = $0 { return UInt64(n) }; return nil } ?? UInt64.max
        let requested = params["item_ids"].flatMap { if case .array(let a) = $0 { return Set(a.compactMap(\.string)) }; return nil }
        var transcript = ChatTranscript(), orders: [String: UInt64] = [:]
        for (at, value) in all.prefix(next).enumerated() {
            let event = try value.decode(ChatEvent.self)
            switch event {
            case .itemStarted(let item), .itemCompleted(let item): if orders[item.id] == nil { orders[item.id] = UInt64(at + 1) }
            default: break
            }
            transcript.apply(event)
        }
        let rows = transcript.items.filter { orders[$0.id]! < before && (requested == nil || requested!.contains($0.id)) }
        let items = requested == nil ? Array(rows.suffix(50)) : rows
        var controls: [ChatEvent] = []
        if params["cursor"] == nil {
            if let info = transcript.info { controls.append(.info(info)) }
            controls.append(.state(transcript.state)); controls.append(.models(transcript.models))
            if let usage = transcript.usage { controls.append(.usage(usage)) }
            if let turn = transcript.turnID { controls.append(.turnStarted(turnID: turn)) }
            controls += transcript.approvals.map { .approvalRequested($0) }
            controls += transcript.questions.map { .questionRequested($0) }
        }
        let page = ChatSnapshotReply(chatID: id, cursor: String(next), next: UInt64(next), before: items.first.map { orders[$0.id]! } ?? 0,
            more: requested == nil && rows.count > items.count, items: items.map { ChatSnapshotRow(order: orders[$0.id]!, item: $0) }, controls: controls)
        let encoded = try JSONEncoder().encode(page)
        if resourceLimits, encoded.count > 120_000 { throw RemoteError.rpc(code: "snapshot_limit", message: "snapshot response limit exceeded") }
        return try JSONDecoder().decode(JSONValue.self, from: encoded)
    }

    private func events(_ params: [String: JSONValue]) async throws -> JSONValue {
        while eventsGated { try await Task.sleep(for: .milliseconds(5)) }
        guard let chat = params["chat_id"]?.string, case .number(let sinceNumber)? = params["since"], case .number(let waitNumber)? = params["wait_ms"] else {
            throw RemoteError.rpc(code: "invalid_request", message: "bad request")
        }
        if chatsGone || (!chats.contains { $0.id == chat } && log[chat] == nil) { throw RemoteError.rpc(code: "not_found", message: "unknown chat") }
        if eventFailures > 0 { eventFailures -= 1; throw RemoteError.rpc(code: "cli_error", message: "the chat host is not answering") }
        let since = Int(sinceNumber)
        if params["bounded"] == .bool(true), since > 0, let failure = boundedReplyFailure {
            switch failure {
            case .advancingEmptyPage:
                boundedReplyFailures += 1
                return .object(["chat_id": .string(chat), "events": .array([]), "next": .number(Double(since + 1)), "more": .bool(false)])
            case .connectorInvalidPage:
                boundedReplyFailures += 1
                throw RemoteError.rpc(code: "invalid_reply", message: "CLI returned a page of events that does not fit the request")
            case .transientCLI, .transientNetwork:
                if boundedReplyFailures == 0 {
                    boundedReplyFailures += 1
                    if case .transientCLI = failure { throw RemoteError.rpc(code: "cli_error", message: "the chat host is not answering") }
                    throw RemoteError.timeout
                }
            }
        }
        while params["bounded"] == .bool(true), let bound = boundedGateAfter, since >= bound { try await Task.sleep(for: .milliseconds(5)) }
        if since > (log[chat] ?? []).count { throw RemoteError.rpc(code: "invalid_request", message: "cannot continue after the current log") }
        let deadline = ContinuousClock.now + .milliseconds(Int(waitNumber))
        let limit: Int = { if case .number(let n)? = params["max_events"] { Int(n) } else { 500 } }()
        while true {
            guard connected else { throw RemoteError.disconnected }
            let all = log[chat] ?? []
            if all.count > since {
                var values: [JSONValue] = [], bytes = 0
                for value in all[since..<min(all.count, since + limit)] {
                    var represented = value
                    if resourceLimits, try JSONEncoder().encode(value).count > 120_000 {
                        if !values.isEmpty { break }
                        if params["bounded"] == .bool(true), value["event"].string == "item_completed" || value["event"].string == "item_started" {
                            guard case .object(var fields) = value, case .object(var item) = value["item"] else { throw RemoteError.rpc(code: "response_too_large", message: "invalid body") }
                            item["body"] = .object(["type": .string("agent_message"), "text": .string("Long message shortened. Full text is on your Mac. " + String((value["item"]["body"]["text"].string ?? "").prefix(2048)) + "…")])
                            fields["item"] = .object(item); represented = .object(fields)
                        } else {
                            if params["complete"] == .bool(true) { completeSizeFailures += 1 }
                            throw RemoteError.rpc(code: "response_too_large", message: "complete event exceeds response limit")
                        }
                    }
                    let size = try JSONEncoder().encode(represented).count
                    if resourceLimits, bytes + size > 120_000, !values.isEmpty { break }
                    values.append(represented); bytes += size
                }
                let page = Array(values.enumerated())
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
