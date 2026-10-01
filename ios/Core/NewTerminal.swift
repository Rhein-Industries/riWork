import Foundation

// Opening and closing terminals on the desktop: `shell.create` and `shell.close`.
//
// The request builders validate exactly what the desktop validates (docs/remote-protocol.md, "Terminal creation
// extension"), so a request that leaves the phone is one the desktop will not reject as malformed. Everything here is
// pure and `Sendable`; the app owns the timing.

/// What a new terminal runs: the Mac's default shell, or one of the agents RiWork launches.
public enum NewTerminalKind: String, CaseIterable, Sendable, Codable, Identifiable {
    case shell, codex, claude, grok
    public var id: String { rawValue }
    public var title: String {
        switch self {
        case .shell: "Shell"
        case .codex: "Codex"
        case .claude: "Claude"
        case .grok: "Grok"
        }
    }
    /// Agents can be started without approval prompts; a plain shell cannot.
    public var isAgent: Bool { self != .shell }
    /// The first time, and whenever the remembered value is not one of ours.
    public static let standard = NewTerminalKind.shell
    /// The kind after `steps` rows down (negative: up), wrapping at both ends.
    public func moved(by steps: Int) -> NewTerminalKind {
        let all = Self.allCases
        let index = all.firstIndex(of: self) ?? 0
        return all[((index + steps) % all.count + all.count) % all.count]
    }
}

/// Where a new terminal opens: a project (its main worktree) or one of its worktrees.
public enum NewTerminalTarget: Sendable, Equatable, Hashable, Identifiable {
    case project(id: String, name: String)
    case worktree(id: String, projectID: String, projectName: String, branch: String, isPrimary: Bool)

    public var id: String {
        switch self {
        case .project(let id, _): "project:\(id)"
        case .worktree(let id, _, _, _, _): "worktree:\(id)"
        }
    }
    public var projectID: String {
        switch self {
        case .project(let id, _): id
        case .worktree(_, let projectID, _, _, _): projectID
        }
    }
    public var worktreeID: String? { if case .worktree(let id, _, _, _, _) = self { id } else { nil } }
    public var title: String {
        switch self {
        case .project(_, let name): name
        case .worktree(_, _, let name, let branch, _): "\(name) · \(branch)"
        }
    }
    /// Short form for a row that already names the project.
    public var branchLabel: String? {
        if case .worktree(_, _, _, let branch, let isPrimary) = self { return isPrimary ? "\(branch) · root" : branch }
        return nil
    }
    /// The request target: a worktree wins over its project.
    public var requestTarget: NewTerminalRequest.Target {
        switch self {
        case .project(let id, _): .project(id)
        case .worktree(let id, _, _, _, _): .worktree(id)
        }
    }
}

public enum NewTerminalTargets {
    /// The choices for the project that is being looked at: its worktrees (the main one first), or the project itself when none
    /// are known. Another project is chosen on the project list, so the sheet never has to switch projects under the screen.
    /// Order is stable, so a keyboard can step through it.
    public static func options(projectID: String, projects: [RemoteProject], worktrees: [RemoteWorktree]) -> [NewTerminalTarget] {
        let name = projects.first { $0.id == projectID }?.name ?? "Project"
        let trees = worktrees.filter { $0.project_id == projectID }
        guard !trees.isEmpty else { return [.project(id: projectID, name: name)] }
        let ordered = trees.enumerated().sorted { ($0.element.is_primary ? 0 : 1, $0.offset) < ($1.element.is_primary ? 0 : 1, $1.offset) }.map(\.element)
        return ordered.map { .worktree(id: $0.id, projectID: projectID, projectName: name, branch: $0.branch, isPrimary: $0.is_primary) }
    }

    /// What is chosen when the sheet opens: the worktree of the terminal being looked at when it has one, else the main
    /// worktree, else the first choice (the project itself).
    public static func preselected(in options: [NewTerminalTarget], selectedWorktreeID: String?) -> Int {
        if let selectedWorktreeID, let index = options.firstIndex(where: { $0.worktreeID == selectedWorktreeID }) { return index }
        if let index = options.firstIndex(where: { if case .worktree(_, _, _, _, true) = $0 { true } else { false } }) { return index }
        return 0
    }
}

// MARK: - Request

/// Why a request cannot be sent. Nothing has left the phone when one of these is thrown.
public enum NewTerminalValidationError: Error, Equatable, Sendable, LocalizedError {
    case needsOneTarget, invalidID, unknownKind, unrestrictedNeedsAgent, commandNeedsShell
    case commandBlank, commandTooLong, commandHasControlCharacters, commandStartsWithDash, malformed
    public var errorDescription: String? {
        switch self {
        case .needsOneTarget: "Choose a project or a worktree."
        case .invalidID: "The project or worktree is not a full UUID."
        case .unknownKind: "Choose Shell, Codex, Claude or Grok."
        case .unrestrictedNeedsAgent: "Only an agent can run unrestricted."
        case .commandNeedsShell: "A command can only start a plain shell."
        case .commandBlank: "Enter a command, or leave it empty."
        case .commandTooLong: "A command is at most \(NewTerminalRequest.commandByteLimit) bytes."
        case .commandHasControlCharacters: "A command is one line without control characters."
        case .commandStartsWithDash: "A command cannot start with “-”."
        case .malformed: "The new terminal request is malformed."
        }
    }
}

/// `shell.create` parameters, validated like the desktop does.
public struct NewTerminalRequest: Sendable, Equatable {
    public enum Target: Sendable, Equatable {
        case project(String)
        case worktree(String)
    }
    public static let commandByteLimit = 4096

    public let target: Target
    public let kind: NewTerminalKind
    public let unrestricted: Bool
    public let command: String?

    public init(target: Target, kind: NewTerminalKind, unrestricted: Bool = false, command: String? = nil) throws {
        switch target {
        case .project(let id), .worktree(let id): guard Self.isCanonicalUUID(id) else { throw NewTerminalValidationError.invalidID }
        }
        if unrestricted, !kind.isAgent { throw NewTerminalValidationError.unrestrictedNeedsAgent }
        if let command {
            guard kind == .shell else { throw NewTerminalValidationError.commandNeedsShell }
            try Self.validate(command: command)
        }
        self.target = target; self.kind = kind; self.unrestricted = unrestricted; self.command = command
    }

    /// The wire parameters. `unrestricted` is sent only when it is on, so the default stays the desktop's.
    public var params: [String: JSONValue] {
        var params: [String: JSONValue] = ["kind": .string(kind.rawValue)]
        switch target {
        case .project(let id): params["project_id"] = .string(id)
        case .worktree(let id): params["worktree_id"] = .string(id)
        }
        if unrestricted { params["unrestricted"] = .bool(true) }
        if let command { params["command"] = .string(command) }
        return params
    }

    /// Reads wire parameters back through the same rules (the transport checks every request this way).
    public init(params: [String: JSONValue]) throws {
        guard Set(params.keys).isSubset(of: ["project_id", "worktree_id", "kind", "unrestricted", "command"]) else { throw NewTerminalValidationError.malformed }
        let target: Target
        switch (params["project_id"], params["worktree_id"]) {
        case (.string(let id)?, nil): target = .project(id)
        case (nil, .string(let id)?): target = .worktree(id)
        case (nil, nil), (_?, _?): throw NewTerminalValidationError.needsOneTarget
        default: throw NewTerminalValidationError.invalidID
        }
        guard case .string(let name)? = params["kind"], let kind = NewTerminalKind(rawValue: name) else { throw NewTerminalValidationError.unknownKind }
        var unrestricted = false
        if let value = params["unrestricted"] {
            guard case .bool(let flag) = value else { throw NewTerminalValidationError.malformed }
            unrestricted = flag
        }
        var command: String?
        if let value = params["command"] {
            guard case .string(let text) = value else { throw NewTerminalValidationError.malformed }
            command = text
        }
        try self.init(target: target, kind: kind, unrestricted: unrestricted, command: command)
    }

    static func isCanonicalUUID(_ value: String) -> Bool { value.utf8.count == 36 && UUID(uuidString: value)?.uuidString.lowercased() == value }

    /// 1 to 4096 UTF-8 bytes, not blank, no control characters, not an option.
    public static func validate(command: String) throws {
        guard command.utf8.count <= commandByteLimit else { throw NewTerminalValidationError.commandTooLong }
        guard !command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw NewTerminalValidationError.commandBlank }
        if command.unicodeScalars.contains(where: { $0.properties.generalCategory == .control || $0.value == 0x2028 || $0.value == 0x2029 }) {
            throw NewTerminalValidationError.commandHasControlCharacters
        }
        guard !command.hasPrefix("-") else { throw NewTerminalValidationError.commandStartsWithDash }
    }
}

// MARK: - Errors

/// What went wrong with opening or closing a terminal, in words for the person using the phone.
public enum TerminalControlError: Error, Equatable, Sendable, LocalizedError {
    public enum Operation: Equatable, Sendable {
        case create(NewTerminalKind)
        case close
    }
    /// The desktop predates the method.
    case unsupported
    /// The project or worktree (create), or the terminal (close), is not on the desktop any more.
    case notFound(Operation)
    case harnessUnavailable(NewTerminalKind)
    /// The desktop refused the request as malformed (a bug on one side).
    case invalid(String)
    /// The terminal was created but is not running any more.
    case exitedRightAway
    /// The desktop answered, but not with a terminal the phone can use.
    case unreadableReply
    /// The connection dropped or timed out: the desktop may or may not have done it. Never retried automatically.
    case outcomeUnknown(Operation)
    case notConnected
    case busy
    case failed(String)

    public static let unsupportedMessage = "Update RiWork on your Mac to open terminals from the phone."

    public var errorDescription: String? { message }
    public var message: String {
        switch self {
        case .unsupported: Self.unsupportedMessage
        case .notFound(.create): "That project or worktree no longer exists on the Mac. Refresh and try again."
        case .notFound(.close): "That terminal is already gone from the Mac. Refresh the terminal list."
        case .harnessUnavailable(let kind): "\(kind.title) isn’t installed on the Mac (or isn’t on its PATH)."
        case .invalid(let text): "The Mac refused the request: \(text)"
        case .exitedRightAway: "The terminal started but exited right away. Check it on the Mac."
        case .unreadableReply: "The Mac’s answer wasn’t understood. Refresh to see whether the terminal was created."
        case .outcomeUnknown(.create): "The connection dropped before the Mac answered, so the terminal may or may not have been created. Check the terminal list before trying again."
        case .outcomeUnknown(.close): "The connection dropped before the Mac answered, so the terminal may or may not have been closed. Refresh the terminal list."
        case .notConnected: "Connect to your Mac first."
        case .busy: "Another terminal request is still running."
        case .failed(let text): text
        }
    }
    /// The request may have taken effect.
    public var outcomeIsUncertain: Bool {
        switch self {
        case .outcomeUnknown, .unreadableReply: true
        default: false
        }
    }

    /// Maps what the transport threw. Only a reply from the desktop (an `rpc` error) says the request did not happen;
    /// anything the connection did to the request leaves the outcome unknown.
    public static func from(_ error: any Error, operation: Operation) -> TerminalControlError {
        if let known = error as? TerminalControlError { return known }
        if RemoteError.isUnsupportedMethod(error) { return .unsupported }
        if error is CancellationError || error is URLError { return .outcomeUnknown(operation) }
        if let remote = error as? RemoteError {
            switch remote {
            case .rpc(let code, let message):
                switch code {
                case "not_found": return .notFound(operation)
                case "harness_unavailable":
                    if case .create(let kind) = operation { return .harnessUnavailable(kind) }
                    return .failed(readable(message))
                case "invalid_request": return .invalid(readable(message))
                case "outcome_unknown": return .outcomeUnknown(operation)
                default: return .failed(readable(message))
                }
            case .timeout, .disconnected, .relayClosed, .uncertainDelivery: return .outcomeUnknown(operation)
            default: return .failed(readable(remote.localizedDescription))
            }
        }
        return .failed(readable(error.localizedDescription))
    }

    /// Text from the desktop is shown, but only printable, one line and bounded.
    static func readable(_ text: String) -> String {
        let cleaned = text.unicodeScalars.map { $0.properties.generalCategory == .control ? " " : String($0) }.joined()
        let collapsed = cleaned.split(separator: " ", omittingEmptySubsequences: true).joined(separator: " ")
        let bounded = String(collapsed.prefix(240))
        return bounded.isEmpty ? "The Mac could not do that." : bounded
    }
}

// MARK: - Replies

/// The `shell.create` result: the new terminal's id and its `shells.list` entry.
public struct NewTerminalReply: Sendable, Equatable {
    public let shellID: String
    public let session: RemoteSession

    public static func parse(_ result: JSONValue) throws -> NewTerminalReply {
        guard let shellID = result["shell_id"].string, NewTerminalRequest.isCanonicalUUID(shellID),
              case .object = result["shell"], let session = try? result["shell"].decode(RemoteSession.self),
              session.id == shellID, session.kind == "project" else { throw TerminalControlError.unreadableReply }
        guard session.alive else { throw TerminalControlError.exitedRightAway }
        return NewTerminalReply(shellID: shellID, session: session)
    }
}

/// `shell.close` parameters: one project terminal, by its full id.
public struct CloseTerminalRequest: Sendable, Equatable {
    public let shellID: String
    public init(shellID: String) throws {
        guard NewTerminalRequest.isCanonicalUUID(shellID) else { throw NewTerminalValidationError.invalidID }
        self.shellID = shellID
    }
    public var params: [String: JSONValue] { ["shell_id": .string(shellID)] }

    public func parse(_ result: JSONValue) throws {
        guard result["shell_id"].string == shellID, result["status"].string == "closed" else { throw TerminalControlError.unreadableReply }
    }
}

// MARK: - Transport

extension RemoteTransport {
    /// Opens a terminal. Never retried: the caller reports an uncertain outcome instead of asking again.
    public func createTerminal(_ request: NewTerminalRequest, id: String = UUID().uuidString.lowercased()) async throws -> NewTerminalReply {
        do { return try NewTerminalReply.parse(try await self.request(method: "shell.create", params: request.params, id: id)) }
        catch { throw TerminalControlError.from(error, operation: .create(request.kind)) }
    }
    public func closeTerminal(_ request: CloseTerminalRequest, id: String = UUID().uuidString.lowercased()) async throws {
        do { try request.parse(try await self.request(method: "shell.close", params: request.params, id: id)) }
        catch { throw TerminalControlError.from(error, operation: .close) }
    }
}

// MARK: - The sheet's keyboard model

/// The "New terminal" form as a keyboard sees it: which control has the focus ring and what the arrows do. The same state
/// drives touch, so both agree on what is chosen.
public struct NewTerminalForm: Equatable, Sendable {
    public enum Field: Equatable, Sendable, CaseIterable { case target, kind, unrestricted, create }
    public enum Key: Equatable, Sendable { case up, down, left, right, tab, backTab, space }

    public var targets: [NewTerminalTarget]
    public private(set) var targetIndex: Int
    public private(set) var kind: NewTerminalKind
    public private(set) var unrestricted = false
    public var focus = Field.kind

    public init(targets: [NewTerminalTarget], targetIndex: Int = 0, kind: NewTerminalKind = .standard) {
        self.targets = targets
        self.targetIndex = targets.isEmpty ? 0 : min(max(0, targetIndex), targets.count - 1)
        self.kind = kind
    }

    public var target: NewTerminalTarget? { targets.indices.contains(targetIndex) ? targets[targetIndex] : nil }
    /// The controls that can have focus now: the toggle only exists for agents.
    public var fields: [Field] { Field.allCases.filter { $0 != .unrestricted || kind.isAgent } }

    public mutating func select(kind: NewTerminalKind) {
        guard kind != self.kind else { return }
        self.kind = kind
        // Never carried over to another kind: it is chosen on purpose, each time.
        unrestricted = false
        if !fields.contains(focus) { focus = .kind }
    }
    public mutating func select(targetAt index: Int) { if targets.indices.contains(index) { targetIndex = index } }
    public mutating func setUnrestricted(_ on: Bool) { unrestricted = on && kind.isAgent }
    public mutating func moveTarget(by steps: Int) {
        guard !targets.isEmpty else { return }
        targetIndex = ((targetIndex + steps) % targets.count + targets.count) % targets.count
    }
    public mutating func moveFocus(by steps: Int) {
        let list = fields
        let index = list.firstIndex(of: focus) ?? 0
        focus = list[((index + steps) % list.count + list.count) % list.count]
    }

    /// Up and down choose (the target when its row has focus, otherwise the kind); left, right and tab move the focus ring;
    /// space flips the toggle when it has focus. Return and Escape are the sheet's: create and cancel.
    public mutating func handle(_ key: Key) {
        switch key {
        case .up, .down:
            let steps = key == .down ? 1 : -1
            if focus == .target { moveTarget(by: steps) } else { select(kind: kind.moved(by: steps)); focus = .kind }
        case .left, .backTab: moveFocus(by: -1)
        case .right, .tab: moveFocus(by: 1)
        case .space: if focus == .unrestricted { setUnrestricted(!unrestricted) }
        }
    }

    /// The request for what is chosen, or nil while there is no target.
    public func request() throws -> NewTerminalRequest {
        guard let target else { throw NewTerminalValidationError.needsOneTarget }
        return try NewTerminalRequest(target: target.requestTarget, kind: kind, unrestricted: unrestricted && kind.isAgent)
    }
}
