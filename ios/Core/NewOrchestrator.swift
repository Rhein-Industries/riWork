import Foundation

// Opening an orchestrator on the desktop: `orchestrator.create`.
//
// `orchestrator.create {project_id?}` starts the orchestrator of one project (`project_id` present) or the global one (absent) and
// answers `{"orchestrator": <orchestrators.list entry>, "created": bool}`. When there already is one for that scope it is returned
// unchanged and `created` is false: the phone never makes a second. The entry says how the Mac runs it (`mode`, `chat_id`,
// `provider`; see `ProjectTabs.swift`), which decides whether the phone opens a chat or a terminal.
//
// Like every other request that makes something, it is sent once and never retried by the app. Everything here is pure and `Sendable`;
// the app owns the timing.

/// `orchestrator.create` parameters.
public struct NewOrchestratorRequest: Sendable, Equatable {
    /// The project whose orchestrator it is; nil for the global one.
    public let projectID: String?

    public init(projectID: String?) throws {
        if let projectID { guard NewTerminalRequest.isCanonicalUUID(projectID) else { throw NewTerminalValidationError.invalidID } }
        self.projectID = projectID
    }
    public var isGlobal: Bool { projectID == nil }

    /// The wire parameters: none for the global orchestrator.
    public var params: [String: JSONValue] { projectID.map { ["project_id": .string($0)] } ?? [:] }

    /// Reads wire parameters back through the same rules (the transport checks every request this way).
    public init(params: [String: JSONValue]) throws {
        guard Set(params.keys).isSubset(of: ["project_id"]) else { throw NewTerminalValidationError.malformed }
        switch params["project_id"] {
        case nil: try self.init(projectID: nil)
        case .string(let id)?: try self.init(projectID: id)
        default: throw NewTerminalValidationError.invalidID
        }
    }

    /// What the scope is called ("Project orchestrator", "Global orchestrator").
    public var title: String { isGlobal ? NewTerminalKind.globalOrchestrator.title : NewTerminalKind.projectOrchestrator.title }
}

// MARK: - Errors

/// What went wrong with opening an orchestrator, in words for the person using the phone.
public enum OrchestratorControlError: Error, Equatable, Sendable, LocalizedError {
    /// The desktop predates the method.
    case unsupported
    /// The project is not on the desktop any more.
    case notFound
    /// The desktop refused the request as malformed (a bug on one side).
    case invalid(String)
    /// The orchestrator is a terminal and was started but is not running any more.
    case exitedRightAway
    /// The desktop answered, but not with the orchestrator that was asked for.
    case unreadableReply
    /// The connection dropped or timed out: the desktop may or may not have done it. Never retried automatically.
    case outcomeUnknown
    case notConnected
    case busy
    case failed(String)

    public static let unsupportedMessage = "Update RiWork on your Mac to open orchestrators from the phone."

    public var errorDescription: String? { message }
    public var message: String {
        switch self {
        case .unsupported: Self.unsupportedMessage
        case .notFound: "That project no longer exists on the Mac. Refresh and try again."
        case .invalid(let text): "The Mac refused the request: \(text)"
        case .exitedRightAway: "The orchestrator started but exited right away. Check it on the Mac."
        case .unreadableReply: "The Mac’s answer wasn’t understood. Refresh the tabs to see whether the orchestrator is there."
        case .outcomeUnknown: "The connection dropped before the Mac answered, so the orchestrator may or may not have started. Check the tab list before trying again."
        case .notConnected: "Connect to your Mac first."
        case .busy: "Another orchestrator request is still running."
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

    /// Maps what the transport threw. Only a reply from the desktop (an `rpc` error) says the request did not happen; anything the
    /// connection did to the request leaves the outcome unknown.
    public static func from(_ error: any Error) -> OrchestratorControlError {
        if let known = error as? OrchestratorControlError { return known }
        if let invalid = error as? NewTerminalValidationError { return .invalid(invalid.localizedDescription) }
        if RemoteError.isUnsupportedMethod(error) { return .unsupported }
        if error is CancellationError || error is URLError { return .outcomeUnknown }
        if let remote = error as? RemoteError {
            switch remote {
            case .rpc(let code, let message):
                switch code {
                case "not_found": return .notFound
                case "invalid_request": return .invalid(TerminalControlError.readable(message))
                case "outcome_unknown": return .outcomeUnknown
                default: return .failed(TerminalControlError.readable(message))
                }
            case .timeout, .disconnected, .relayClosed, .uncertainDelivery: return .outcomeUnknown
            default: return .failed(TerminalControlError.readable(remote.localizedDescription))
            }
        }
        return .failed(TerminalControlError.readable(error.localizedDescription))
    }
}

// MARK: - Reply

/// The `orchestrator.create` result: the orchestrator's entry and whether this call made it.
public struct NewOrchestratorReply: Sendable, Equatable {
    public let session: RemoteSession
    /// False when the orchestrator was there already and the desktop returned it.
    public let created: Bool

    /// Checks that the answer is the orchestrator that was asked for: an orchestrator entry, of the scope of the request. `created` is
    /// taken as true when the desktop does not say, because the only thing it changes is a note that says "already running".
    public static func parse(_ result: JSONValue, for request: NewOrchestratorRequest) throws -> NewOrchestratorReply {
        guard case .object = result["orchestrator"], let session = try? result["orchestrator"].decode(RemoteSession.self),
              NewTerminalRequest.isCanonicalUUID(session.id), session.kind == "orchestrator",
              session.project_id == request.projectID else { throw OrchestratorControlError.unreadableReply }
        // A chat is not held to `alive` (it is started again by its next message); a terminal that is not running cannot be opened.
        if session.mode != .chat, !session.alive { throw OrchestratorControlError.exitedRightAway }
        var created = true
        if case .bool(let flag) = result["created"] { created = flag }
        return NewOrchestratorReply(session: session, created: created)
    }
}

// MARK: - Transport

extension RemoteTransport {
    /// Opens the orchestrator, starting it if it is not there. Never retried: the caller reports an uncertain outcome instead of asking again.
    public func createOrchestrator(_ request: NewOrchestratorRequest, id: String = UUID().uuidString.lowercased()) async throws -> NewOrchestratorReply {
        do { return try NewOrchestratorReply.parse(try await self.request(method: "orchestrator.create", params: request.params, id: id), for: request) }
        catch { throw OrchestratorControlError.from(error) }
    }
}
