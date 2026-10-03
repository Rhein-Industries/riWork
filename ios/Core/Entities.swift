import Foundation

public struct RemoteProject: Codable, Sendable, Identifiable, Hashable {
    public let id: String
    public let name: String
    public let root: String
    public let created_at: UInt64
    /// Unix seconds of the newest source edit in the project, from a desktop that works it out. Nil when it does not, or has not yet.
    public let last_edited_unix: UInt64?
    /// Unix seconds at which the newest of the project's terminals (its orchestrator included) last had output, from a desktop that
    /// reports it. Nil when it does not, or when the project has no live terminal.
    public let last_activity_unix: UInt64?
    /// The agents at work in the project's terminals. Nil from a desktop that does not count them.
    public let agents: ProjectAgents?
    private enum CodingKeys: String, CodingKey { case id, name, root, created_at, last_edited_unix, last_activity_unix, agents }
}
extension RemoteProject {
    /// The four fields every desktop sends are required, as before. The three that came later are read leniently (`AgentActivity.swift`):
    /// whatever is wrong with them leaves them nil and the list intact.
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        root = try c.decode(String.self, forKey: .root)
        created_at = try c.decode(UInt64.self, forKey: .created_at)
        last_edited_unix = LenientNumber.seconds(try? c.decode(JSONValue.self, forKey: .last_edited_unix))
        last_activity_unix = LenientNumber.seconds(try? c.decode(JSONValue.self, forKey: .last_activity_unix))
        agents = ProjectAgents(wire: try? c.decode(JSONValue.self, forKey: .agents))
    }
}
public struct RemoteWorktree: Codable, Sendable, Identifiable, Hashable {
    public let id: String
    public let project_id: String
    public let branch: String
    public let path: String
    public let is_primary: Bool
    public let created_at: UInt64
}
public struct RemoteTask: Codable, Sendable, Identifiable, Hashable {
    public let id: String
    public let project_id: String
    public let title: String
    public let details: String
    public let status: String
    public let worktree_id: String?
    public let created_at: UInt64
    public let updated_at: UInt64
    public var statusLabel: String { switch status { case "in_progress": "In progress"; case "done": "Done"; default: "To do" } }
}
public struct RemoteSession: Codable, Sendable, Identifiable, Hashable {
    public let id: String
    public let project_id: String?
    public let worktree_id: String?
    public let kind: String
    public let cwd: String
    public let harness: String?
    public let alive: Bool
    public let created_at_unix: UInt64
    /// What the agent in this terminal is doing; `.unknown` from a desktop that does not say.
    public let activity: AgentActivity
    /// Unix seconds since when it has been in that state.
    public let activity_since_unix: UInt64?
    /// Subagents at work for this terminal's agent; zero when none, or not said.
    public let subagents_working: Int
    private enum CodingKeys: String, CodingKey {
        case id, project_id, worktree_id, kind, cwd, harness, alive, created_at_unix, activity, activity_since_unix, subagents_working
    }
    public var title: String {
        if kind == "orchestrator" { return project_id == nil ? "Global orchestrator" : "Project orchestrator" }
        return harness.map { $0.capitalized + " worker" } ?? "Terminal"
    }
    public var shortID: String { String(id.prefix(8)) }
    /// The state to draw: what the desktop said, except that a terminal that is no longer alive is exited, whatever it said last.
    public var shownActivity: AgentActivity { alive ? activity : .exited }
    /// "Working, 2 subagents", "Waiting for input" or "Done" when the state is drawn; nil otherwise (VoiceOver).
    public var activitySummary: String? { shownActivity.spoken(subagents: subagents_working) }
}
extension RemoteSession {
    /// As for `RemoteProject`: the entry's own fields are required as before, the activity fields are read leniently.
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        project_id = try c.decodeIfPresent(String.self, forKey: .project_id)
        worktree_id = try c.decodeIfPresent(String.self, forKey: .worktree_id)
        kind = try c.decode(String.self, forKey: .kind)
        cwd = try c.decode(String.self, forKey: .cwd)
        harness = try c.decodeIfPresent(String.self, forKey: .harness)
        alive = try c.decode(Bool.self, forKey: .alive)
        created_at_unix = try c.decode(UInt64.self, forKey: .created_at_unix)
        activity = AgentActivity(wire: try? c.decode(JSONValue.self, forKey: .activity))
        activity_since_unix = LenientNumber.seconds(try? c.decode(JSONValue.self, forKey: .activity_since_unix))
        subagents_working = LenientNumber.count(try? c.decode(JSONValue.self, forKey: .subagents_working)) ?? 0
    }
}

public protocol RemoteTransport: Sendable {
    @discardableResult
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue
    func disconnect() async
    func isConnected() async -> Bool
    /// `request`, together with how the reply travelled (see `ReplyTiming`). The default has no timing.
    func timedRequest(method: String, params: [String: JSONValue], id: String) async throws -> TimedReply
    /// What the desktop announced when this session began (`DesktopFeatures`). The default is nothing, as for an older desktop.
    func desktopFeatures() async -> DesktopFeatures
    /// Whether to ask the desktop for compressed replies (on by default). Takes effect now if connected, and for later sessions.
    func setCompression(_ enabled: Bool) async
    /// Whether the desktop has agreed to compress this session's replies.
    func compressionActive() async -> Bool
}
