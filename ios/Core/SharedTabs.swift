import Foundation
import OSLog

/// Mac-owned membership; selection and pane placement are deliberately absent. There are no pins: a `pinned` field an older desktop
/// still sends is not read, so every tab moves and closes alike.
public struct SharedTab: Codable, Sendable, Equatable, Identifiable {
    public enum Kind: String, Codable, Sendable { case chat, shell, unknown
        public init(from decoder: any Decoder) throws { let raw = try decoder.singleValueContainer().decode(String.self); self = Self(rawValue: raw) ?? .unknown; if self == .unknown { Logger(subsystem: "com.riwork.remote", category: "tabs").warning("Unknown tab kind: \(raw, privacy: .public)") } }
    }
    /// `idle`: a live shell at its prompt (or running a program RiWork does not follow).
    public enum Status: String, Codable, Sendable { case working, waiting, error, done, idle, stopped, unknown
        public init(from decoder: any Decoder) throws { let raw = try decoder.singleValueContainer().decode(String.self); self = Self(rawValue: raw) ?? .unknown; if self == .unknown { Logger(subsystem: "com.riwork.remote", category: "tabs").warning("Unknown tab status: \(raw, privacy: .public)") } }
    }
    public let key: String
    public let kind: Kind
    public let title: String
    public let status: Status
    public let hidden: Bool
    public let worker: Bool?
    public var isWorker: Bool { worker == true || parent != nil }
    public let order: Int
    public let parent: String?
    public let children: [SharedTab]
    public let childCount: Int
    public var id: String { key }
    public var sessionID: String { String(key.split(separator: ":", maxSplits: 1).last ?? "") }
    public var isTopLevelVisible: Bool { !hidden && !(kind == .shell && status == .stopped) }
    private enum CodingKeys: String, CodingKey {
        case key, kind, title, status, hidden, worker, order, parent, children
        case childCount = "child_count"
    }
}
public struct SharedTabsReply: Codable, Sendable, Equatable {
    /// Includes hidden entries and children. Draw only `visible` in the strip.
    public let epoch: String?
    public let revision: UInt64?
    public let entries: [SharedTab]
    public func supersedes(_ current: Self?) -> Bool {
        guard let current else { return true }
        return epoch != current.epoch || (revision ?? 0) >= (current.revision ?? 0)
    }
    public var allEntries: [SharedTab] {
        var stack = Array(entries.reversed()), result: [SharedTab] = []
        while let entry = stack.popLast() { result.append(entry); stack.append(contentsOf: entry.children.reversed()) }
        return result
    }
    public var visible: [SharedTab] { allEntries.filter(\.isTopLevelVisible).sorted { $0.order < $1.order } }
}
public enum TabUpdate: Sendable, Equatable {
    case hide(String), unhide(String)
    case move(String, before: String?)
    case rename(String, title: String)
    public var json: JSONValue {
        var fields: [String: JSONValue]
        switch self {
        case .hide(let key): fields = ["action": .string("hide"), "key": .string(key)]
        case .unhide(let key): fields = ["action": .string("unhide"), "key": .string(key)]
        case .move(let key, let before): fields = ["action": .string("move"), "key": .string(key), "before": before.map(JSONValue.string) ?? .null]
        case .rename(let key, let title): fields = ["action": .string("rename"), "key": .string(key), "title": .string(title)]
        }
        return .object(fields)
    }
}
public enum SharedTabsRequests {
    public static func validate(method: String, params: [String: JSONValue]) throws {
        guard NewTerminalRequest.isCanonicalUUID(params["project_id"]?.string ?? "") else { throw ChatValidationError.invalidID }
        if method == "tabs.open" {
            guard let value = params["key"]?.string, let separator = value.firstIndex(of: ":"), ["chat", "shell"].contains(String(value[..<separator])), NewTerminalRequest.isCanonicalUUID(String(value[value.index(after: separator)...])) else { throw ChatValidationError.invalidID }; return
        }
        guard method == "tabs.update" else { return }
        guard case .object(let update)? = params["update"], let action = update["action"]?.string else { throw ChatValidationError.malformed }
        func key(_ value: JSONValue?) throws {
            guard let key = value?.string, let separator = key.firstIndex(of: ":"), ["chat", "shell"].contains(String(key[..<separator])),
                  NewTerminalRequest.isCanonicalUUID(String(key[key.index(after: separator)...])) else { throw ChatValidationError.invalidID }
        }
        try key(update["key"])
        var allowed: Set<String> = ["action", "key"]
        switch action {
        case "hide", "unhide": break
        case "move":
            allowed.insert("before")
            if let before = update["before"], before != .null { try key(before) }
        case "rename":
            allowed.insert("title")
            guard let title = update["title"]?.string,
                  title.unicodeScalars.count <= 200, !title.unicodeScalars.contains(where: { ($0.value <= 0x1f || (0x7f...0x9f).contains($0.value)) || (0x202a...0x202e).contains($0.value) || (0x2066...0x2069).contains($0.value) || [0x200e, 0x200f, 0x061c].contains($0.value) }) else { throw ChatValidationError.malformed }
        default: throw ChatValidationError.malformed
        }
        guard Set(update.keys).isSubset(of: allowed) else { throw ChatValidationError.malformed }
    }
}
extension RemoteTransport {
    public func listTabs(projectID: String, id: String = UUID().uuidString.lowercased()) async throws -> SharedTabsReply {
        let params: [String: JSONValue] = ["project_id": .string(projectID)]
        try RequestValidation.validate(method: "tabs.list", params: params, id: id)
        return try await request(method: "tabs.list", params: params, id: id).decode(SharedTabsReply.self)
    }
    public func updateTabs(projectID: String, update: TabUpdate, id: String = UUID().uuidString.lowercased()) async throws -> SharedTabsReply {
        let params: [String: JSONValue] = ["project_id": .string(projectID), "update": update.json]
        try RequestValidation.validate(method: "tabs.update", params: params, id: id)
        return try await request(method: "tabs.update", params: params, id: id).decode(SharedTabsReply.self)
    }
}

public enum TabCloseBehavior: String, Codable, CaseIterable, Sendable, Identifiable {
    case ask, detach, exit
    public var id: String { rawValue }
    public var title: String { switch self { case .ask: "Ask"; case .detach: "Detach"; case .exit: "Exit" } }
    public static let settingKey = "tab_close_behavior"
    public func effectiveChoice(for tab: SharedTab) -> Self { tab.isWorker ? .detach : self }
}
extension RemoteTransport {
    public func openTab(projectID: String, key: String, id: String = UUID().uuidString.lowercased()) async throws -> SharedTabsReply {
        let params: [String: JSONValue] = ["project_id": .string(projectID), "key": .string(key)]
        try RequestValidation.validate(method: "tabs.open", params: params, id: id)
        return try await request(method: "tabs.open", params: params, id: id).decode(SharedTabsReply.self)
    }
}
