import Foundation

/// Read-only fields the existing shell list honestly carries. `parent_id` proves
/// worker ancestry; older unparented shells cannot be classified as workers.
public struct WorkerShellCard: Sendable, Equatable, Identifiable {
    public let session: RemoteSession
    public let tasks: [RemoteTask]
    public var id: String { session.id }
    public var status: String {
        switch session.shownActivity {
        case .working: "Working"
        case .waiting: "Waiting for input"
        case .done: "Done"
        case .exited: "Exited"
        case .unknown: "Unknown"
        }
    }
    public var finished: Bool { session.shownActivity == .done || session.shownActivity == .exited }
    public var harness: String { session.harness?.capitalized ?? "Shell" }
    public var model: String? { session.model }
    public var title: String { tasks.first?.title ?? session.title }
    public func elapsed(now: UInt64) -> UInt64 {
        let end = session.shownActivity == .done ? session.activity_since_unix ?? now : now
        return end >= session.created_at_unix ? end - session.created_at_unix : 0
    }
    public init(session: RemoteSession, tasks: [RemoteTask] = []) {
        self.session = session
        self.tasks = tasks.filter { $0.project_id == session.project_id && session.worktree_id != nil && $0.worktree_id == session.worktree_id }
    }
    public static func project(_ project: String?, sessions: [RemoteSession], tasks: [RemoteTask] = []) -> [Self] {
        guard let project else { return [] }
        return sessions.filter { $0.project_id == project && $0.kind != "orchestrator" && $0.mode == .terminal && $0.parent_id != nil }
            .map { Self(session: $0, tasks: tasks) }
    }
}

/// Additive delegation presentation, with exact legacy Claude/Codex tool fallbacks.
public struct ChatSubagent: Codable, Sendable, Equatable {
    public enum Status: String, Codable, Sendable { case running, done, failed, interrupted, declined, unknown
        public var title: String { switch self { case .running: "Running"; case .done: "Done"; case .failed: "Failed"; case .interrupted: "Interrupted"; case .declined: "Declined"; case .unknown: "Status not reported" } }
    }
    public struct Child: Codable, Sendable, Equatable {
        public let id: String
        public let status: Status
        public let summary: String?
    }
    public let name: String
    public let purpose: String?
    public let status: Status
    public let summary: String?
    public let children: [Child]
    public var result: String {
        ([summary].compactMap { $0 } + children.map { "\($0.id) · \($0.status.title)" + ($0.summary.map { "\n" + $0 } ?? "") }).joined(separator: "\n")
    }
    private enum Keys: String, CodingKey { case name, purpose, status, summary, children }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        name = String(try c.decode(String.self, forKey: .name).prefix(160))
        purpose = try c.decodeIfPresent(String.self, forKey: .purpose).map { String($0.prefix(500)) }
        status = (try? c.decode(Status.self, forKey: .status)) ?? .unknown
        summary = try c.decodeIfPresent(String.self, forKey: .summary).map { String($0.prefix(4096)) }
        children = Array(((try? c.decode([Child].self, forKey: .children)) ?? []).prefix(32))
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(name,forKey:.name); try c.encodeIfPresent(purpose,forKey:.purpose); try c.encode(status,forKey:.status)
        try c.encodeIfPresent(summary,forKey:.summary); try c.encode(children,forKey:.children)
    }
    private init(name: String, purpose: String?, status: Status, summary: String?, children: [Child] = []) {
        self.name = String(name.prefix(160)); self.purpose = purpose.map { String($0.prefix(500)) }; self.status = status
        self.summary = summary.map { String($0.prefix(4096)) }; self.children = children
    }
    public static func of(_ item: ChatItem, provider: ChatProvider) -> Self? {
        if let run = item.subagent { return run }
        guard case .toolCall(let server, let tool, let input, let output) = item.body, server == nil else { return nil }
        if provider == .claude, tool == "Task" || tool == "Agent" {
            let foregroundStatus: Status = switch item.status { case .inProgress: .running; case .completed: .done; case .failed: .failed; case .interrupted: .interrupted; case .declined: .declined }
            let status: Status = input["run_in_background"] == .bool(true) && item.status == .completed ? .unknown : foregroundStatus
            return Self(name: input["subagent_type"].string ?? "Subagent", purpose: input["description"].string ?? input["prompt"].string, status: status, summary: output)
        }
        if provider == .codex, tool == "collabAgentToolCall" {
            var children: [Child] = []
            if case .object(let states) = input["agentsStates"] {
                children = states.keys.sorted().prefix(32).map { id in
                    let state = states[id] ?? .null
                    let status: Status = switch state["status"].string { case "running", "pendingInit": .running; case "completed": .done; case "errored": .failed; case "shutdown", "interrupted": .interrupted; default: .unknown }
                    return Child(id: String(id.prefix(120)), status: status, summary: state["message"].string.map { String($0.prefix(4096)) })
                }
            }
            let status: Status = children.contains { $0.status == .running } ? .running : children.contains { $0.status == .failed } ? .failed :
                !children.isEmpty && children.allSatisfy { $0.status == .done } ? .done : .unknown
            return Self(name: "Subagents · " + (input["tool"].string ?? "delegation"), purpose: input["prompt"].string, status: status, summary: nil, children: children)
        }
        return nil
    }
    public static func inTranscript(_ item: ChatItem, items: [ChatItem], provider: ChatProvider) -> Self? {
        guard let run = of(item,provider:provider) else { return nil }
        guard provider == .codex, !run.children.isEmpty else { return run }
        var latest: [String: Child] = [:]
        for source in items { for child in of(source,provider:provider)?.children ?? [] { latest[child.id] = child } }
        let children = run.children.map { latest[$0.id] ?? $0 }
        let status: Status = children.contains { $0.status == .running } ? .running : children.contains { $0.status == .failed } ? .failed :
            children.allSatisfy { $0.status == .done } ? .done : children.allSatisfy { $0.status == .interrupted } ? .interrupted : .unknown
        return Self(name:run.name,purpose:run.purpose,status:status,summary:run.summary,children:children)
    }
}
