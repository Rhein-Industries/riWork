import Foundation

/// A worker shell as the shell list (`shells.list`) describes it: the status words a live worker card shows for it. The list is joined to
/// the cards by the session id their marker carries (`WorkerTimeline`), so only this chat's workers ever appear.
public struct WorkerShellCard: Sendable, Equatable, Identifiable {
    public let session: RemoteSession
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
    public func elapsed(now: UInt64) -> UInt64 {
        let end = session.shownActivity == .done ? session.activity_since_unix ?? now : now
        return end >= session.created_at_unix ? end - session.created_at_unix : 0
    }
    public init(session: RemoteSession) { self.session = session }
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
                    return Child(id: String(id.prefix(120)), status: status, summary: state["message"].string.map { String(ChatFileCitation.replacing(in: $0).prefix(4096)) })
                }
            }
            let status: Status = children.contains { $0.status == .running } ? .running : children.contains { $0.status == .failed } ? .failed :
                !children.isEmpty && children.allSatisfy { $0.status == .done } ? .done : .unknown
            return Self(name: "Subagents · " + (input["tool"].string ?? "delegation"), purpose: input["prompt"].string, status: status, summary: nil, children: children)
        }
        return nil
    }
    /// Resolve every card once per transcript change. A later child report may
    /// update a card that appeared much earlier in the transcript.
    public static func cards(in items: [ChatItem], provider: ChatProvider) -> [String: Self] {
        let runs = items.compactMap { item in of(item, provider: provider).map { (item.id, $0) } }
        guard provider == .codex else { return Dictionary(runs, uniquingKeysWith: { _, latest in latest }) }
        var latest: [String: Child] = [:]
        for (_, run) in runs { for child in run.children { latest[child.id] = child } }
        return Dictionary(runs.map { id, run in (id, run.resolving(children: latest)) }, uniquingKeysWith: { _, latest in latest })
    }

    public static func inTranscript(_ item: ChatItem, items: [ChatItem], provider: ChatProvider) -> Self? {
        guard let run = of(item, provider: provider) else { return nil }
        guard provider == .codex, !run.children.isEmpty else { return run }
        var latest: [String: Child] = [:]
        for source in items { for child in of(source, provider: provider)?.children ?? [] { latest[child.id] = child } }
        return run.resolving(children: latest)
    }

    private func resolving(children latest: [String: Child]) -> Self {
        guard !children.isEmpty else { return self }
        let children = children.map { latest[$0.id] ?? $0 }
        let status: Status = children.contains { $0.status == .running } ? .running : children.contains { $0.status == .failed } ? .failed :
            children.allSatisfy { $0.status == .done } ? .done : children.allSatisfy { $0.status == .interrupted } ? .interrupted : .unknown
        return Self(name: name, purpose: purpose, status: status, summary: summary, children: children)
    }
}
