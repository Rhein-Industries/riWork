import Foundation

public struct RemoteProject: Codable, Sendable, Identifiable, Hashable {
    public let id: String
    public let name: String
    public let root: String
    public let created_at: UInt64
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
    public var title: String {
        if kind == "orchestrator" { return project_id == nil ? "Global orchestrator" : "Project orchestrator" }
        return harness.map { $0.capitalized + " worker" } ?? "Terminal"
    }
    public var shortID: String { String(id.prefix(8)) }
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
