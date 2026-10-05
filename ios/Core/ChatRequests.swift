import Foundation

// Talking to the desktop's chats: `chats.list`, `chat.create`, `chat.events`, `chat.command` and `chat.stop`.
//
// The request builders validate what the desktop validates, so a request that leaves the phone is one the desktop will not reject as
// malformed (it denies unknown fields and checks every id and length before any command runs). The reply parsers check that an answer is
// the one that was asked for. Everything here is pure and `Sendable`; the app owns the timing.

// MARK: - Errors

/// Why a request cannot be sent. Nothing has left the phone when one of these is thrown.
public enum ChatValidationError: Error, Equatable, Sendable, LocalizedError {
    case needsOneTarget, invalidID, invalidWait, invalidCount, textTooLong(field: String, limit: Int), blankMessage, messageTooLong, malformed
    /// A `configure` that changes nothing, or sets a model or an effort to nothing: the desktop refuses it, so it is not sent.
    case emptyConfigure, blankSetting(field: String)
    public var errorDescription: String? { message }
    public var message: String {
        switch self {
        case .needsOneTarget: "Choose a project or a worktree."
        case .invalidID: "The chat, project or worktree is not a full UUID."
        case .invalidWait: "A chat wait is 0 to \(ChatLimits.maximumWaitMilliseconds) ms."
        case .invalidCount: "A chat page is 1 to \(ChatLimits.maximumEvents) events."
        case .textTooLong(let field, let limit): "The chat \(field) is at most \(limit) bytes."
        case .blankMessage: "Write a message first."
        case .messageTooLong: "A message is at most 64 KiB."
        case .malformed: "The chat request is malformed."
        case .emptyConfigure: "Choose a model, an effort, a mode or Fast to change."
        case .blankSetting(let field): "The chat \(field) is blank."
        }
    }
}

/// What went wrong with a chat request, in words for the person using the phone.
public enum ChatControlError: Error, Equatable, Sendable, LocalizedError {
    public enum Operation: Equatable, Sendable {
        case list, create(ChatProvider), events, command, stop
    }
    /// The desktop predates chats.
    case unsupported
    /// The chat, project or worktree is not on the desktop any more.
    case notFound(Operation)
    case harnessUnavailable(ChatProvider)
    /// The desktop refused the request as malformed (a bug on one side).
    case invalid(String)
    /// The desktop answered, but not with what was asked for.
    case unreadableReply
    /// The connection dropped or timed out: the desktop may or may not have done it. Never retried automatically.
    case outcomeUnknown(Operation)
    case notConnected
    case busy
    case failed(String)
    public enum Resource: String, Sendable { case snapshot = "snapshot_limit", response = "response_too_large" }
    case resourceLimit(Resource, String)

    public static let unsupportedMessage = "Update RiWork on your Mac to use chats from the phone."

    public var errorDescription: String? { message }
    public var message: String {
        switch self {
        case .unsupported: Self.unsupportedMessage
        case .notFound(.create): "That project or worktree no longer exists on the Mac. Refresh and try again."
        case .notFound: "That chat is gone from the Mac. Refresh the tab list."
        case .harnessUnavailable(let provider): "\(provider.title) isn’t installed on the Mac (or isn’t on its PATH)."
        case .invalid(let text): "The Mac refused the request: \(text)"
        case .unreadableReply: "The Mac’s answer wasn’t understood. Refresh to see what happened."
        case .outcomeUnknown(.create): "The connection dropped before the Mac answered, so the chat may or may not have been created. Check the tab list before trying again."
        case .outcomeUnknown(.command): "The connection dropped before the Mac answered, so that may or may not have gone through. Check the chat before sending it again."
        case .outcomeUnknown: "The connection dropped before the Mac answered. Refresh and look again."
        case .notConnected: "Connect to your Mac first."
        case .busy: "Another chat request is still running."
        case .failed(let text), .resourceLimit(_, let text): text
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
    public static func from(_ error: any Error, operation: Operation) -> ChatControlError {
        if let known = error as? ChatControlError { return known }
        if let invalid = error as? ChatValidationError { return .invalid(invalid.message) }
        if RemoteError.isUnsupportedMethod(error) { return .unsupported }
        if error is CancellationError || error is URLError { return .outcomeUnknown(operation) }
        if let remote = error as? RemoteError {
            switch remote {
            case .rpc(let code, let message):
                switch code {
                case "snapshot_limit", "response_too_large": return .resourceLimit(Resource(rawValue: code)!, TerminalControlError.readable(message))
                case "not_found": return .notFound(operation)
                case "harness_unavailable":
                    if case .create(let provider) = operation { return .harnessUnavailable(provider) }
                    return .failed(TerminalControlError.readable(message))
                case "invalid_request": return .invalid(TerminalControlError.readable(message))
                case "outcome_unknown": return .outcomeUnknown(operation)
                default: return .failed(TerminalControlError.readable(message))
                }
            case .timeout, .disconnected, .relayClosed, .uncertainDelivery: return .outcomeUnknown(operation)
            default: return .failed(TerminalControlError.readable(remote.localizedDescription))
            }
        }
        return .failed(TerminalControlError.readable(error.localizedDescription))
    }
}

public enum ChatLimits {
    /// The most `chat.events` may hold the request back (the desktop's own bound).
    public static let maximumWaitMilliseconds = 25_000
    /// How long the phone asks it to wait: well inside the bound and the request timeout, long enough that an idle chat costs one request
    /// every 20 s.
    public static let waitMilliseconds = 20_000
    public static let maximumEvents = 2000
    public static let defaultEvents = 500
    public static let modelBytes = 100, effortBytes = 32, titleBytes = 200
    /// `Send.text`, in UTF-8 bytes.
    public static let messageBytes = 64 * 1024
    /// The request timeout of a `chat.events` that waits: the wait plus this, so a healthy poll never times out and a dead link is
    /// still noticed (the keepalive ping notices one within 20 s anyway).
    public static let timeoutSlack: Duration = .seconds(20)
    public static func timeout(waitMilliseconds: Int) -> Duration { .milliseconds(max(0, waitMilliseconds)) + timeoutSlack }
}

// MARK: - chats.list

/// `chats.list`: every chat of one project, or of all projects.
public struct ChatListRequest: Sendable, Equatable {
    public let projectID: String?
    public init(projectID: String? = nil) throws {
        if let projectID { guard NewTerminalRequest.isCanonicalUUID(projectID) else { throw ChatValidationError.invalidID } }
        self.projectID = projectID
    }
    public var params: [String: JSONValue] { projectID.map { ["project_id": .string($0)] } ?? [:] }
    public init(params: [String: JSONValue]) throws {
        guard Set(params.keys).isSubset(of: ["project_id"]) else { throw ChatValidationError.malformed }
        switch params["project_id"] {
        case nil: try self.init()
        case .string(let id)?: try self.init(projectID: id)
        default: throw ChatValidationError.invalidID
        }
    }
    /// The chats of the answer, in the desktop's order. One the phone cannot read (a provider it does not know) is left out.
    public func parse(_ result: JSONValue) throws -> [ChatInfo] {
        guard case .object = result, case .array = result["chats"] else { throw ChatControlError.unreadableReply }
        let data = try JSONEncoder().encode(result["chats"])
        return ((try? JSONDecoder().decode([Lenient<ChatInfo>].self, from: data)) ?? []).compactMap(\.value)
    }
}

// MARK: - chat.create

/// `chat.create`. A chat starts in a project (its main worktree) or in one of its worktrees, never both.
public struct ChatCreateRequest: Sendable, Equatable {
    public typealias Target = NewTerminalRequest.Target
    public let provider: ChatProvider
    public let target: Target
    /// Sent only when set, so the default stays the desktop's.
    public let approvalMode: ChatApprovalMode?
    public let model: String?
    public let effort: String?
    /// Start with the provider's fast mode on. Sent only when on: a desktop that predates it denies the unknown field, and off is the default.
    public let fast: Bool
    public let title: String?

    public init(provider: ChatProvider, target: Target, approvalMode: ChatApprovalMode? = nil, model: String? = nil, effort: String? = nil, fast: Bool = false, title: String? = nil) throws {
        switch target {
        case .project(let id), .worktree(let id): guard NewTerminalRequest.isCanonicalUUID(id) else { throw ChatValidationError.invalidID }
        }
        for (field, value, limit) in [("model", model, ChatLimits.modelBytes), ("effort", effort, ChatLimits.effortBytes), ("title", title, ChatLimits.titleBytes)] {
            if let value, value.utf8.count > limit { throw ChatValidationError.textTooLong(field: field, limit: limit) }
        }
        self.provider = provider; self.target = target; self.approvalMode = approvalMode; self.model = model; self.effort = effort; self.fast = fast; self.title = title
    }

    public var params: [String: JSONValue] {
        var params: [String: JSONValue] = ["provider": .string(provider.rawValue)]
        switch target {
        case .project(let id): params["project_id"] = .string(id)
        case .worktree(let id): params["worktree_id"] = .string(id)
        }
        if let approvalMode { params["approval_mode"] = .string(approvalMode.rawValue) }
        if let model { params["model"] = .string(model) }
        if let effort { params["effort"] = .string(effort) }
        if fast { params["fast"] = .bool(true) }
        if let title { params["title"] = .string(title) }
        return params
    }

    /// Reads wire parameters back through the same rules (the transport checks every request this way).
    public init(params: [String: JSONValue]) throws {
        guard Set(params.keys).isSubset(of: ["provider", "project_id", "worktree_id", "approval_mode", "model", "effort", "fast", "title"]) else { throw ChatValidationError.malformed }
        let target: Target
        switch (params["project_id"], params["worktree_id"]) {
        case (.string(let id)?, nil): target = .project(id)
        case (nil, .string(let id)?): target = .worktree(id)
        case (nil, nil), (_?, _?): throw ChatValidationError.needsOneTarget
        default: throw ChatValidationError.invalidID
        }
        guard case .string(let word)? = params["provider"], let provider = ChatProvider(rawValue: word) else { throw ChatValidationError.malformed }
        func text(_ key: String) throws -> String? {
            guard let value = params[key] else { return nil }
            guard case .string(let text) = value else { throw ChatValidationError.malformed }
            return text
        }
        var mode: ChatApprovalMode?
        if let word = try text("approval_mode") {
            guard let known = ChatApprovalMode(rawValue: word) else { throw ChatValidationError.malformed }
            mode = known
        }
        var fast = false
        if let value = params["fast"] {
            guard case .bool(let flag) = value else { throw ChatValidationError.malformed }
            fast = flag
        }
        try self.init(provider: provider, target: target, approvalMode: mode, model: try text("model"), effort: try text("effort"), fast: fast, title: try text("title"))
    }

    /// The new chat of the answer. It must be the kind that was asked for, in the place that was asked for: anything else is treated as
    /// an answer that cannot be trusted, and the caller reads the tab list again.
    public func parse(_ result: JSONValue) throws -> ChatInfo {
        guard case .object = result, case .object = result["chat"], let chat = try? result["chat"].decode(ChatInfo.self),
              NewTerminalRequest.isCanonicalUUID(chat.id), chat.provider == provider else { throw ChatControlError.unreadableReply }
        switch target {
        case .project(let id): guard chat.projectID == nil || chat.projectID == id else { throw ChatControlError.unreadableReply }
        case .worktree(let id): guard chat.worktreeID == nil || chat.worktreeID == id else { throw ChatControlError.unreadableReply }
        }
        return chat
    }
}

// MARK: - chat.events

/// `chat.events`: the events after `since`, waiting up to `waitMilliseconds` for the first one when there are none yet.
public struct ChatEventsRequest: Sendable, Equatable {
    public let chatID: String
    public let since: UInt64
    public let waitMilliseconds: Int
    /// Sent only when set; the desktop's default is 500.
    public let maxEvents: Int?
    public let complete: Bool
    public let bounded: Bool

    public init(chatID: String, since: UInt64, waitMilliseconds: Int, maxEvents: Int? = nil, complete: Bool = false, bounded: Bool = false) throws {
        guard NewTerminalRequest.isCanonicalUUID(chatID) else { throw ChatValidationError.invalidID }
        guard (0...ChatLimits.maximumWaitMilliseconds).contains(waitMilliseconds) else { throw ChatValidationError.invalidWait }
        if let maxEvents { guard (1...ChatLimits.maximumEvents).contains(maxEvents) else { throw ChatValidationError.invalidCount } }
        guard !(complete && bounded) else { throw ChatValidationError.malformed }
        self.bounded = bounded
        self.chatID = chatID; self.since = since; self.waitMilliseconds = waitMilliseconds; self.maxEvents = maxEvents; self.complete = complete
    }
    /// A request is a long poll when the desktop may hold it back: it takes one of the device's waiting slots.
    public var isLongPoll: Bool { waitMilliseconds > 0 }

    public var params: [String: JSONValue] {
        var params: [String: JSONValue] = ["chat_id": .string(chatID), "since": .number(Double(since)), "wait_ms": .number(Double(waitMilliseconds))]
        if complete { params["complete"] = .bool(true) }
        if bounded { params["bounded"] = .bool(true) }
        if let maxEvents { params["max_events"] = .number(Double(maxEvents)) }
        return params
    }
    public init(params: [String: JSONValue]) throws {
        guard Set(params.keys).isSubset(of: ["chat_id", "since", "wait_ms", "max_events", "complete", "bounded"]) else { throw ChatValidationError.malformed }
        func whole(_ key: String, in range: ClosedRange<Double>) throws -> Int? {
            guard let value = params[key] else { return nil }
            guard case .number(let number) = value, number.isFinite, number.rounded() == number, range.contains(number) else { throw ChatValidationError.malformed }
            return Int(number)
        }
        guard case .string(let id)? = params["chat_id"], case .number(let since)? = params["since"], since.isFinite, since >= 0, since.rounded() == since,
              since < 9_007_199_254_740_992, let wait = try whole("wait_ms", in: 0...Double(ChatLimits.maximumWaitMilliseconds)) else { throw ChatValidationError.malformed }
        for key in ["complete", "bounded"] {
            if let raw = params[key], case .bool = raw {} else if params[key] != nil { throw ChatValidationError.malformed }
        }
        try self.init(chatID: id, since: UInt64(since), waitMilliseconds: wait, maxEvents: try whole("max_events", in: 1...Double(ChatLimits.maximumEvents)), complete: params["complete"] == .bool(true), bounded: params["bounded"] == .bool(true))
    }
    public func parse(_ result: JSONValue) throws -> ChatEventsReply {
        let reply = try ChatEventsReply.parse(result, chatID: chatID)
        if complete || bounded {
            var last = since
            for envelope in reply.events {
                guard last < UInt64.max, envelope.seq == last + 1 else { throw ChatControlError.unreadableReply }
                last = envelope.seq
            }
            guard reply.next == last else { throw ChatControlError.unreadableReply }
        }
        return reply
    }
}

// MARK: - chat.command and chat.stop

/// `chat.command`: one command for one chat. Sent once; the app never retries one.
public struct ChatCommandRequest: Sendable, Equatable {
    public let chatID: String
    public let command: ChatCommand

    public init(chatID: String, command: ChatCommand) throws {
        guard NewTerminalRequest.isCanonicalUUID(chatID) else { throw ChatValidationError.invalidID }
        if case .send(let text) = command {
            guard text.utf8.count <= ChatLimits.messageBytes else { throw ChatValidationError.messageTooLong }
            guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw ChatValidationError.blankMessage }
        }
        if case .configure(let model, let effort, let mode, let fast) = command {
            guard model != nil || effort != nil || mode != nil || fast != nil else { throw ChatValidationError.emptyConfigure }
            for (field, value, limit) in [("model", model, ChatLimits.modelBytes), ("effort", effort, ChatLimits.effortBytes)] {
                guard let value else { continue }
                guard !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { throw ChatValidationError.blankSetting(field: field) }
                guard value.utf8.count <= limit else { throw ChatValidationError.textTooLong(field: field, limit: limit) }
            }
        }
        self.chatID = chatID; self.command = command
    }
    public var params: [String: JSONValue] {
        let command = (try? JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(command))) ?? .null
        return ["chat_id": .string(chatID), "command": command]
    }
    public init(params: [String: JSONValue]) throws {
        guard Set(params.keys) == ["chat_id", "command"], case .string(let id)? = params["chat_id"], let raw = params["command"], case .object = raw,
              let command = try? raw.decode(ChatCommand.self) else { throw ChatValidationError.malformed }
        try self.init(chatID: id, command: command)
    }
    public func parse(_ result: JSONValue) throws {
        guard result["status"].string == "ok" else { throw ChatControlError.unreadableReply }
    }
}

/// `chat.stop`: stops the provider process. The chat stays; the next message resumes it.
public struct ChatStopRequest: Sendable, Equatable {
    public let chatID: String
    public init(chatID: String) throws {
        guard NewTerminalRequest.isCanonicalUUID(chatID) else { throw ChatValidationError.invalidID }
        self.chatID = chatID
    }
    public var params: [String: JSONValue] { ["chat_id": .string(chatID)] }
    public func parse(_ result: JSONValue) throws {
        guard result["status"].string == "stopped" else { throw ChatControlError.unreadableReply }
    }
}

// MARK: - Transport

extension RemoteTransport {
    /// Reading the list is harmless to repeat, so a failure is just reported.
    public func listChats(_ request: ChatListRequest, id: String = UUID().uuidString.lowercased()) async throws -> [ChatInfo] {
        do { return try request.parse(try await self.request(method: "chats.list", params: request.params, id: id)) }
        catch { throw ChatControlError.from(error, operation: .list) }
    }
    /// Creates a chat. Never retried: the caller reports an uncertain outcome instead of asking again.
    public func createChat(_ request: ChatCreateRequest, id: String = UUID().uuidString.lowercased()) async throws -> ChatInfo {
        do { return try request.parse(try await self.request(method: "chat.create", params: request.params, id: id)) }
        catch { throw ChatControlError.from(error, operation: .create(request.provider)) }
    }
    /// One read of the chat's events. A cancelled read is passed on as the cancellation it is: the caller is leaving, nothing failed.
    public func chatEvents(_ request: ChatEventsRequest, id: String = UUID().uuidString.lowercased()) async throws -> ChatEventsReply {
        do { return try request.parse(try await self.request(method: "chat.events", params: request.params, id: id)) }
        catch is CancellationError { throw CancellationError() }
        catch { throw ChatControlError.from(error, operation: .events) }
    }
    public func sendChatCommand(_ request: ChatCommandRequest, id: String = UUID().uuidString.lowercased()) async throws {
        do { try request.parse(try await self.request(method: "chat.command", params: request.params, id: id)) }
        catch { throw ChatControlError.from(error, operation: .command) }
    }
    public func stopChat(_ request: ChatStopRequest, id: String = UUID().uuidString.lowercased()) async throws {
        do { try request.parse(try await self.request(method: "chat.stop", params: request.params, id: id)) }
        catch { throw ChatControlError.from(error, operation: .stop) }
    }
}


/// Snapshot v1 returns full current rows and controls at one live event cursor.
public struct ChatSnapshotRow: Sendable, Equatable, Codable {
    public let order: UInt64
    public let item: ChatItem
    public init(order: UInt64, item: ChatItem) { self.order = order; self.item = item }
}
public struct ChatSnapshotReply: Sendable, Equatable, Codable {
    public let v: Int
    public let chatID: String
    public let cursor: String
    public let next: UInt64
    public let before: UInt64
    public let more: Bool
    public let items: [ChatSnapshotRow]
    public let controls: [ChatEvent]
    enum CodingKeys: String, CodingKey { case v, chatID = "chat_id", cursor, next, before, more, items, controls }
    public init(chatID: String, cursor: String, next: UInt64, before: UInt64, more: Bool, items: [ChatSnapshotRow], controls: [ChatEvent]) {
        v = 1; self.chatID = chatID; self.cursor = cursor; self.next = next; self.before = before; self.more = more; self.items = items; self.controls = controls
    }
}
extension RemoteTransport {
    public func chatSnapshot(chatID: String, cursor: String? = nil, before: UInt64? = nil, itemIDs: [String] = []) async throws -> ChatSnapshotReply {
        guard NewTerminalRequest.isCanonicalUUID(chatID) else { throw ChatValidationError.invalidID }
        var params: [String: JSONValue] = ["chat_id": .string(chatID), "limit": .number(50)]
        if let cursor { params["cursor"] = .string(cursor) }
        if !itemIDs.isEmpty { params["item_ids"] = .array(itemIDs.map { .string($0) }) }
        if let before { params["before"] = .number(Double(before)) }
        let result = try await request(method: "chat.snapshot", params: params, id: UUID().uuidString.lowercased())
        let reply = try result.decode(ChatSnapshotReply.self)
        guard reply.v == 1, reply.chatID == chatID, reply.cursor.count <= 80, !reply.cursor.isEmpty,
              reply.items.count <= (itemIDs.isEmpty ? 50 : 100), cursor == nil || reply.cursor == cursor,
              cursor == nil || reply.controls.isEmpty,
              reply.before == reply.items.first?.order ?? 0,
              !reply.more || !reply.items.isEmpty else { throw ChatControlError.unreadableReply }
        var last: UInt64 = 0
        var ids = Set<String>()
        for row in reply.items {
            guard row.order > last, row.order < (before ?? UInt64.max), row.order <= reply.next, ids.insert(row.item.id).inserted else { throw ChatControlError.unreadableReply }
            last = row.order
        }
        return reply
    }
}
