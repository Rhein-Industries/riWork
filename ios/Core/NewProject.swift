import Foundation

// Creating a project on the desktop: `project.create`.
//
// The request builder validates exactly what the desktop validates (docs/remote-protocol.md, "Project creation extension"), so a
// request that leaves the phone is one the desktop will not reject as malformed. The phone never names a place: the desktop creates the
// folder in its own default projects folder, and the name is the only thing sent. Everything here is pure and `Sendable`; the app
// owns the timing.

// MARK: - The name

/// Why a name, or a request, cannot be sent. Nothing has left the phone when one of these is thrown.
public enum NewProjectValidationError: Error, Equatable, Sendable, LocalizedError {
    case empty, surroundingWhitespace, tooLong, tooManyBytes, controlCharacters, separator, leadingDot, leadingDash
    /// A request read back from wire parameters that is not shaped like one.
    case malformed
    public var errorDescription: String? { message }
    public var message: String {
        switch self {
        case .empty: "Enter a name for the project."
        case .surroundingWhitespace: "A project name cannot start or end with a space."
        case .tooLong: "A project name is at most \(NewProjectName.characterLimit) characters."
        case .tooManyBytes: "That name is too long for a folder name on the Mac."
        case .controlCharacters: "A project name is one line without control characters."
        case .separator: "A project name cannot contain “/” or “\\”."
        case .leadingDot: "A project name cannot start with a dot."
        case .leadingDash: "A project name cannot start with “-”."
        case .malformed: "The new project request is malformed."
        }
    }
}

/// What the person has typed, judged for a form that checks as they go.
public enum NewProjectNameCheck: Equatable, Sendable {
    /// Nothing to send yet, and nothing to complain about.
    case empty
    /// Fine; this is the name that would be sent (what was typed, trimmed).
    case valid(String)
    case invalid(NewProjectValidationError)

    public var name: String? { if case .valid(let name) = self { name } else { nil } }
    /// The complaint to show while typing: none for an empty field.
    public var problem: NewProjectValidationError? { if case .invalid(let problem) = self { problem } else { nil } }
}

/// The rules for a project name, the desktop's own (`remote/src/rpc.rs`, `project_name`): the name becomes a folder name in the
/// desktop's projects folder and the project's display name.
public enum NewProjectName {
    /// Unicode scalar values, not `Character`s: the desktop counts `char`s.
    public static let characterLimit = 100
    /// Bytes of UTF-8: a folder name on the Mac takes at most 255.
    public static let byteLimit = 255

    /// `typed` without the white space (Unicode `White_Space`, as Rust's `trim` reads it) at both ends. The sheet sends this.
    public static func trimmed(_ typed: String) -> String {
        let scalars = Array(typed.unicodeScalars)
        var start = 0, end = scalars.count
        while start < end, scalars[start].properties.isWhitespace { start += 1 }
        while end > start, scalars[end - 1].properties.isWhitespace { end -= 1 }
        var view = String.UnicodeScalarView()
        view.append(contentsOf: scalars[start..<end])
        return String(view)
    }

    /// What is wrong with `name` as it would be sent, or nil. It is not trimmed first: the desktop refuses a name with white space
    /// at its ends rather than guessing which name was meant.
    public static func problem(with name: String) -> NewProjectValidationError? {
        let scalars = name.unicodeScalars
        guard let first = scalars.first, let last = scalars.last else { return .empty }
        guard !first.properties.isWhitespace, !last.properties.isWhitespace else { return .surroundingWhitespace }
        if scalars.contains(where: { KeyItem.isForbidden($0) }) { return .controlCharacters }
        if scalars.count > characterLimit { return .tooLong }
        if name.utf8.count > byteLimit { return .tooManyBytes }
        if scalars.contains(where: { $0 == "/" || $0 == "\\" }) { return .separator }
        if first == "." { return .leadingDot }
        if first == "-" { return .leadingDash }
        return nil
    }

    public static func validate(_ name: String) throws {
        if let problem = problem(with: name) { throw problem }
    }

    /// The live judgement of what is in the text field.
    public static func check(typed: String) -> NewProjectNameCheck {
        let name = trimmed(typed)
        if name.isEmpty { return .empty }
        if let problem = problem(with: name) { return .invalid(problem) }
        return .valid(name)
    }
}

// MARK: - Request

/// `project.create` parameters, validated like the desktop does.
public struct NewProjectRequest: Sendable, Equatable {
    public let name: String
    /// Whether the desktop runs `git init` in the new folder. On unless the person turns it off.
    public let git: Bool

    /// Throws for a name that is not ready to be sent as it is (see `NewProjectName.problem(with:)`): trim what was typed first.
    public init(name: String, git: Bool = true) throws {
        try NewProjectName.validate(name)
        self.name = name; self.git = git
    }

    /// The wire parameters. `git` is sent only when it is off, so the default stays the desktop's.
    public var params: [String: JSONValue] {
        var params: [String: JSONValue] = ["name": .string(name)]
        if !git { params["git"] = .bool(false) }
        return params
    }

    /// Reads wire parameters back through the same rules (the transport checks every request this way).
    public init(params: [String: JSONValue]) throws {
        guard Set(params.keys).isSubset(of: ["name", "git"]) else { throw NewProjectValidationError.malformed }
        guard case .string(let name)? = params["name"] else { throw NewProjectValidationError.malformed }
        var git = true
        if let value = params["git"] {
            guard case .bool(let flag) = value else { throw NewProjectValidationError.malformed }
            git = flag
        }
        try self.init(name: name, git: git)
    }
}

// MARK: - Errors

/// What went wrong with creating a project, in words for the person using the phone.
public enum ProjectCreateError: Error, Equatable, Sendable, LocalizedError {
    /// The desktop predates the method.
    case unsupported
    /// A project of that name, or a folder of that name in the Mac's projects folder, is there already. The text is the desktop's.
    case alreadyExists(String)
    /// The desktop refused the request as malformed (a bug on one side), or a name it does not take.
    case invalid(String)
    /// The connection dropped or timed out: the desktop may or may not have done it. Never retried automatically.
    case outcomeUnknown
    /// The desktop answered, but not with the project that was asked for.
    case unreadableReply
    case notConnected
    case busy
    case failed(String)

    public static let unsupportedMessage = "Update RiWork on your Mac to create projects from the phone."

    public var errorDescription: String? { message }
    public var message: String {
        switch self {
        case .unsupported: Self.unsupportedMessage
        case .alreadyExists(let text): text
        case .invalid(let text): "The Mac refused the request: \(text)"
        case .outcomeUnknown: "The connection dropped before the Mac answered, so the project may or may not have been created. Check the project list before trying again: if it exists, creating it again says so."
        case .unreadableReply: "The Mac’s answer wasn’t understood. Refresh the project list to see whether the project was created."
        case .notConnected: "Connect to your Mac first."
        case .busy: "Another project is still being created."
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
    public static func from(_ error: any Error) -> ProjectCreateError {
        if let known = error as? ProjectCreateError { return known }
        if RemoteError.isUnsupportedMethod(error) { return .unsupported }
        if error is CancellationError || error is URLError { return .outcomeUnknown }
        if let remote = error as? RemoteError {
            switch remote {
            case .rpc(let code, let message):
                switch code {
                case "already_exists": return .alreadyExists(TerminalControlError.readable(message))
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

// MARK: - Replies

/// The `project.create` result: the new project's id and its `projects.list` entry.
public struct NewProjectReply: Sendable, Equatable {
    public let project: RemoteProject

    /// `requestedName` is the name that was sent: the desktop creates exactly that, so an entry of another name is not the answer
    /// to this request (the project may exist all the same, which is why this is an uncertain outcome).
    public static func parse(_ result: JSONValue, requestedName: String) throws -> NewProjectReply {
        guard let projectID = result["project_id"].string, NewTerminalRequest.isCanonicalUUID(projectID),
              case .object = result["project"], let project = try? result["project"].decode(RemoteProject.self),
              project.id == projectID, project.name == requestedName else { throw ProjectCreateError.unreadableReply }
        return NewProjectReply(project: project)
    }
}

// MARK: - Transport

extension RemoteTransport {
    /// Creates a project. Never retried: the caller reports an uncertain outcome instead of asking again.
    public func createProject(_ request: NewProjectRequest, id: String = UUID().uuidString.lowercased()) async throws -> NewProjectReply {
        do { return try NewProjectReply.parse(try await self.request(method: "project.create", params: request.params, id: id), requestedName: request.name) }
        catch { throw ProjectCreateError.from(error) }
    }
}

// MARK: - The sheet's keyboard model

/// The "New project" form as a keyboard sees it: the name, the Git switch and Create, and which has the focus ring. The same
/// state drives touch, so both agree on what is chosen.
public struct NewProjectForm: Equatable, Sendable {
    public enum Field: Equatable, Sendable, CaseIterable { case name, git, create }
    public enum Key: Equatable, Sendable { case up, down, left, right, tab, backTab, space }

    /// Exactly what is in the text field.
    public var name = ""
    public private(set) var git = true
    public var focus = Field.name

    public init(name: String = "", git: Bool = true) {
        self.name = name
        self.git = git
    }

    public var check: NewProjectNameCheck { NewProjectName.check(typed: name) }
    /// The name that would be sent.
    public var trimmedName: String { NewProjectName.trimmed(name) }
    public var isReady: Bool { check.name != nil }

    public mutating func setGit(_ on: Bool) { git = on }
    public mutating func moveFocus(by steps: Int) {
        let list = Field.allCases
        let index = list.firstIndex(of: focus) ?? 0
        focus = list[((index + steps) % list.count + list.count) % list.count]
    }

    /// Tab, down and right move the focus ring on, Shift-Tab, up and left back, round the three controls. Space flips the switch
    /// when it has the ring. Return and Escape are the sheet's: create and cancel.
    public mutating func handle(_ key: Key) {
        switch key {
        case .tab, .down, .right: moveFocus(by: 1)
        case .backTab, .up, .left: moveFocus(by: -1)
        case .space: if focus == .git { git.toggle() }
        }
    }

    /// The request for what is typed, or why there is none.
    public func request() throws -> NewProjectRequest {
        switch check {
        case .valid(let name): return try NewProjectRequest(name: name, git: git)
        case .empty: throw NewProjectValidationError.empty
        case .invalid(let problem): throw problem
        }
    }
}
