import Foundation

// A chat's transcript as events build it, and the cursor that keeps it in step with the desktop's `chat.events`.

/// A chat's transcript as events build it: what the chat screen draws. This is `Transcript::apply` of `src/chat/model.rs`, line for line,
/// so the phone shows what the desktop's own tab shows: the same events in the same order give the same transcript, whichever side folds them.
public struct ChatTranscript: Sendable, Equatable {
    public private(set) var info: ChatInfo?
    public private(set) var state: ChatState = .starting
    public private(set) var items: [ChatItem] = []
    /// Requests still waiting for the user, in arrival order.
    public private(set) var approvals: [ChatApproval] = []
    public private(set) var questions: [ChatQuestion] = []
    public private(set) var usage: ChatUsage?
    public private(set) var turnID: String?
    /// Where each item is, by id, so a delta finds its item without a search.
    private var index: [String: Int] = [:]

    public init() {}

    public mutating func apply(_ event: ChatEvent) {
        switch event {
        case .info(let info):
            state = info.state
            self.info = info
        case .state(let state):
            self.state = state
            info?.state = state
        case .turnStarted(let turnID):
            self.turnID = turnID
        case .turnCompleted(let turnID, let outcome):
            if self.turnID == turnID { self.turnID = nil }
            // Whatever the provider left open in this turn is over now. An item that names no turn is closed by any turn's end.
            let closed: ChatItemStatus = switch outcome {
            case .completed: .completed
            case .interrupted: .interrupted
            case .failed: .failed
            }
            for at in items.indices where items[at].status == .inProgress && (items[at].turnID == nil || items[at].turnID == turnID) {
                items[at].status = closed
            }
            approvals.removeAll()
            questions.removeAll()
        case .itemStarted(let item), .itemCompleted(let item):
            if let at = index[item.id] {
                items[at] = item
            } else {
                index[item.id] = items.count
                items.append(item)
            }
        case .itemDelta(let itemID, let delta):
            guard let at = index[itemID] else { return }
            switch (items[at].body, delta) {
            case (.agentMessage(let text), .text(let more)): items[at].body = .agentMessage(text + more)
            case (.reasoning(let text), .text(let more)): items[at].body = .reasoning(text + more)
            case (.command(let command, let cwd, let output, let exitCode), .output(let more)):
                items[at].body = .command(command: command, cwd: cwd, output: output + more, exitCode: exitCode)
            default: break
            }
        case .approvalRequested(let approval):
            approvals.removeAll { $0.requestID == approval.requestID }
            approvals.append(approval)
        case .approvalResolved(let requestID, _):
            approvals.removeAll { $0.requestID == requestID }
        case .questionRequested(let question):
            questions.removeAll { $0.requestID == question.requestID }
            questions.append(question)
        case .questionResolved(let requestID):
            questions.removeAll { $0.requestID == requestID }
        case .usage(let usage):
            self.usage = usage
        }
    }

    /// The item with this id.
    public func item(_ id: String) -> ChatItem? { index[id].map { items[$0] } }
    /// The text of the last message the person sent, for "Retry" on a failed chat.
    public var lastUserMessage: String? {
        for item in items.reversed() { if case .userMessage(let text) = item.body { return text } }
        return nil
    }
}

// MARK: - The reply of chat.events

/// One entry of a `chat.events` reply. `event` is nil for one the phone cannot read (a kind it has not heard of): the entry is skipped,
/// but its number still counts, so the next request does not ask for it again.
public struct ChatEnvelope: Sendable, Equatable {
    public let seq: UInt64
    public let event: ChatEvent?
    public init(seq: UInt64, event: ChatEvent?) { self.seq = seq; self.event = event }
}

extension ChatEnvelope: Decodable {
    private enum Keys: String, CodingKey { case seq, event }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        seq = try c.decode(UInt64.self, forKey: .seq)
        event = c.tolerant(ChatEvent.self, forKey: .event)
    }
}

/// What `chat.events` answered: the events after `since`, oldest first, and where to continue.
public struct ChatEventsReply: Sendable, Equatable {
    public let chatID: String
    public let events: [ChatEnvelope]
    /// The `since` for the next request.
    public let next: UInt64
    /// The reply was cut (by `max_events` or its size): ask again at once.
    public let more: Bool
    public init(chatID: String, events: [ChatEnvelope], next: UInt64, more: Bool) {
        self.chatID = chatID; self.events = events; self.next = next; self.more = more
    }
    /// Entries the phone could not read.
    public var skipped: Int { events.reduce(0) { $0 + ($1.event == nil ? 1 : 0) } }

    /// Reads a reply to a request for `chatID`. Entries that are not shaped like `{seq, event}` are dropped, and unknown events are kept
    /// as skipped; the reply as a whole must be an object for that chat with a `next`.
    public static func parse(_ result: JSONValue, chatID: String) throws -> ChatEventsReply {
        guard case .object = result, result["chat_id"].string == chatID, case .number(let next) = result["next"], let cursor = UInt64(exactly: next),
              case .array = result["events"] else { throw ChatControlError.unreadableReply }
        let data = try JSONEncoder().encode(result["events"])
        let entries = (try? JSONDecoder().decode([Lenient<ChatEnvelope>].self, from: data)) ?? []
        var more = false
        if case .bool(let flag) = result["more"] { more = flag }
        return ChatEventsReply(chatID: chatID, events: entries.compactMap(\.value), next: cursor, more: more)
    }
}

// MARK: - The cursor

/// A chat as the phone follows it: the transcript, and how far into the desktop's event log it has read. `chat.events` is asked with
/// `since: next`; whatever comes back is folded in, in order, and moves `next` on. Pure: the app owns the timing.
public struct ChatFeed: Sendable, Equatable {
    public private(set) var transcript = ChatTranscript()
    /// The `since` of the next request: the last sequence number taken in.
    public private(set) var next: UInt64 = 0
    /// Some answer has been taken in, so the screen can stop saying it is loading.
    public private(set) var loaded = false
    /// Events the phone could not read, since the feed began.
    public private(set) var skipped = 0

    public init() {}

    public enum Outcome: Sendable, Equatable {
        /// This many events were folded in.
        case applied(Int)
        /// The desktop's numbering went backwards (the log was replaced): everything held was dropped and the next request starts over.
        case restarted
    }

    /// Takes in the answer to a request made with `since`. Entries at or below what is already held (an answer that arrives twice) are
    /// ignored, so folding the same reply again changes nothing.
    @discardableResult
    public mutating func accept(_ reply: ChatEventsReply, since: UInt64) -> Outcome {
        if reply.next < since {
            self = ChatFeed()
            return .restarted
        }
        var applied = 0
        for envelope in reply.events where envelope.seq > next {
            if let event = envelope.event { transcript.apply(event); applied += 1 } else { skipped += 1 }
            next = envelope.seq
        }
        next = max(next, reply.next)
        loaded = true
        return .applied(applied)
    }
}
