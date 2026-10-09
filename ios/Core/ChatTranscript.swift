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
    /// Requests the relay could not send in full: they wait on the Mac, where their details are, and nothing here can answer them.
    public private(set) var elidedRequests: [ChatElidedRequest] = []
    public private(set) var usage: ChatUsage?
    /// The models the provider offers, empty until its driver has said (an older driver, or an older desktop, never does).
    public private(set) var models: [ChatModelOption] = []
    /// The provider's usage windows, as its last `rate_limits` said (none from an older driver or desktop).
    public private(set) var rateLimits: [ChatRateWindow] = []
    public private(set) var turnID: String?
    /// Where each item is, by id, so a delta finds its item without a search.
    private var index: [String: Int] = [:]

    public init() {}

    public mutating func apply(_ event: ChatEvent) {
        switch event {
        case .info(let info):
            // A chat that went on with another provider starts over with what belongs to one: the new agent lists its own models and
            // counts its own usage.
            if let before = self.info, before.provider != info.provider {
                models = []
                rateLimits = []
                usage = nil
            }
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
            elidedRequests.removeAll()
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
            elidedRequests.removeAll { $0.requestID == approval.requestID }
            approvals.append(approval)
        case .approvalResolved(let requestID, _):
            approvals.removeAll { $0.requestID == requestID }
            elidedRequests.removeAll { $0.requestID == requestID }
        case .questionRequested(let question):
            questions.removeAll { $0.requestID == question.requestID }
            elidedRequests.removeAll { $0.requestID == question.requestID }
            questions.append(question)
        case .questionResolved(let requestID):
            questions.removeAll { $0.requestID == requestID }
            elidedRequests.removeAll { $0.requestID == requestID }
        case .usage(let usage):
            self.usage = usage
        case .models(let models):
            self.models = models
        case .rateLimits(let windows):
            rateLimits = windows
        case .elided(let elided):
            if let requestID = elided.requestID, elided.isRequest || elided.isResolution {
                // Its details are gone, so whatever was shown for that request can no longer be answered here: it waits on the Mac.
                approvals.removeAll { $0.requestID == requestID }
                questions.removeAll { $0.requestID == requestID }
                elidedRequests.removeAll { $0.requestID == requestID }
                if elided.isRequest { elidedRequests.append(ChatElidedRequest(requestID: requestID, question: elided.of == "question_requested")) }
                return
            }
            // The feed resolves these by sequence number; one that comes without (a snapshot's controls) is keyed by its place.
            let row = event.resolved(seq: UInt64(items.count))
            if row != event { apply(row) }
        }
    }

    /// Preserve already-authoritative controls while exceptional replay reconstructs older bodies.
    fileprivate func controlsOnly() -> ChatTranscript {
        var value = self; value.items = []; value.index = [:]; return value
    }
    fileprivate mutating func restoreControls(_ value: ChatTranscript) {
        info = value.info; state = value.state; approvals = value.approvals; questions = value.questions; elidedRequests = value.elidedRequests
        usage = value.usage; models = value.models; rateLimits = value.rateLimits; turnID = value.turnID
    }

    /// Merge only missing historical rows; live rows and controls remain authoritative.
    public mutating func mergeHistory(_ rows: [ChatSnapshotRow], orders: [String: UInt64]) {
        for row in rows where index[row.item.id] == nil { apply(.itemCompleted(row.item)) }
        items.sort { (orders[$0.id] ?? UInt64.max) < (orders[$1.id] ?? UInt64.max) }
        index = Dictionary(uniqueKeysWithValues: items.enumerated().map { ($0.element.id, $0.offset) })
    }

    /// The item with this id.
    public func item(_ id: String) -> ChatItem? { index[id].map { items[$0] } }
    /// The text of the last message the person sent, for "Retry" on a failed chat.
    public var lastUserMessage: String? {
        for item in items.reversed() { if case .userMessage(let text) = item.body { return text } }
        return nil
    }
}

/// A request whose details the relay left out: shown as waiting on the Mac, with nothing to press.
public struct ChatElidedRequest: Sendable, Equatable, Hashable {
    public let requestID: String
    public let question: Bool
    public init(requestID: String, question: Bool) { self.requestID = requestID; self.question = question }
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
    /// Newly arriving rows; historical prepends do not count as unread messages.
    public private(set) var itemArrivals = 0

    public private(set) var historyCursor: String?
    /// The host's dismissals as the last snapshot gave them (`ChatSnapshotReply.dismissedNotices`).
    public private(set) var dismissedNotices: Set<String> = []
    public private(set) var before: UInt64 = 0
    public private(set) var hasOlder = false
    private var orders: [String: UInt64] = [:]
    private var hidden = ChatTranscript()
    // Only edits to unloaded rows are retained. History applies them to its full base,
    // never to the live controls; replaying a page therefore cannot resolve a new request.
    private var deferred: [ChatEvent] = []
    private var deferredBytes = 0
    public private(set) var degradedReplay = false
    private var recoveryThrough: UInt64 = 0

    public init() {}

    public mutating func install(_ snapshot: ChatSnapshotReply) {
        self = ChatFeed()
        next = snapshot.next; loaded = true; historyCursor = snapshot.cursor; itemArrivals = snapshot.items.count
        before = snapshot.before; hasOlder = snapshot.more; dismissedNotices = Set(snapshot.dismissedNotices)
        for event in snapshot.controls { transcript.apply(event) }
        for row in snapshot.items { orders[row.item.id] = row.order }
        transcript.mergeHistory(snapshot.items, orders: orders)
    }

    public mutating func beginDegradedReplay() {
        let controls = transcript.controlsOnly(), checkpoint = next, arrivals = itemArrivals, dismissed = dismissedNotices
        self = ChatFeed(); transcript = controls; recoveryThrough = checkpoint; dismissedNotices = dismissed
        itemArrivals = arrivals; degradedReplay = true
    }

    public func hasItem(_ id: String) -> Bool { transcript.item(id) != nil || hidden.item(id) != nil }

    /// Hydrate preexisting unloaded rows before applying live changes to them.
    public mutating func hydrate(_ page: ChatSnapshotReply) {
        guard page.cursor == historyCursor else { return }
        for row in page.items { orders[row.item.id] = row.order }
        var base = ChatTranscript()
        for row in page.items { base.apply(.itemCompleted(row.item)) }
        for event in deferred { base.apply(event) }
        for item in base.items where !hasItem(item.id) { hidden.apply(.itemCompleted(item)) }
    }

    public mutating func prepend(_ page: ChatSnapshotReply, requestedBefore: UInt64) {
        guard page.cursor == historyCursor, requestedBefore == before, page.before < before || !page.more else { return }
        var historical = ChatTranscript()
        for row in page.items { orders[row.item.id] = row.order; historical.apply(.itemCompleted(row.item)) }
        for event in deferred { historical.apply(event) }
        let rows = historical.items.map { ChatSnapshotRow(order: orders[$0.id] ?? 0, item: hidden.item($0.id) ?? $0) }
        transcript.mergeHistory(rows, orders: orders)
        before = page.before; hasOlder = page.more
        deferred.removeAll { event in
            switch event {
            case .itemDelta(let id, _): return transcript.item(id) != nil
            case .itemStarted(let item), .itemCompleted(let item): return transcript.item(item.id) != nil
            default: return !hasOlder
            }
        }
        deferredBytes = deferred.reduce(0) { $0 + ((try? JSONEncoder().encode($1).count) ?? 0) }
    }

    private mutating func restart() {
        let degraded = degradedReplay
        self = ChatFeed()
        if degraded { beginDegradedReplay() }
    }

    /// An older relay could not send the event after `next` at all (`response_too_large` even for one event): it is passed over, with
    /// a note in its place, so the events after it still arrive. The full content is on the Mac.
    public mutating func skipOversized() {
        let seq = next + 1
        accept(ChatEventsReply(chatID: "", events: [ChatEnvelope(seq: seq, event: .elided(ChatElidedEvent(event: "update")))], next: seq, more: false), since: next)
    }

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
            restart()
            return .restarted
        }
        var applied = 0
        for envelope in reply.events where envelope.seq > next {
            if let event = envelope.event?.resolved(seq: envelope.seq) {
                let protectedControls = envelope.seq <= recoveryThrough ? transcript.controlsOnly() : nil
                if historyCursor != nil {
                    switch event {
                    case .itemStarted(let item), .itemCompleted(let item):
                        if orders[item.id] == nil { orders[item.id] = envelope.seq }
                    case .itemDelta(let id, _):
                        if !hasItem(id) { deferred.append(event); deferredBytes += (try? JSONEncoder().encode(event).count) ?? 0 }
                    case .turnCompleted: if hasOlder { deferred.append(event); deferredBytes += (try? JSONEncoder().encode(event).count) ?? 0 }
                    default: break
                    }
                }
                switch event {
                case .itemStarted(let item), .itemCompleted(let item):
                    // A provider notice is the banner row's, not a row of the transcript: it is not news there.
                    if !hasItem(item.id), envelope.seq > recoveryThrough, ChatNotices.isTranscriptRow(item) { itemArrivals += 1 }
                    if hidden.item(item.id) != nil && transcript.item(item.id) == nil { hidden.apply(event) }
                    else { transcript.apply(event) }
                case .itemDelta(let id, _):
                    if hidden.item(id) != nil && transcript.item(id) == nil { hidden.apply(event) }
                    else { transcript.apply(event) }
                default:
                    hidden.apply(event); transcript.apply(event)
                }
                if let protectedControls { transcript.restoreControls(protectedControls) }
                applied += 1
            } else { skipped += 1 }
            next = envelope.seq
        }
        if hidden.items.count > 1000 || deferred.count > 10_000 || deferredBytes > 8 * 1024 * 1024 {
            restart(); return .restarted
        }
        next = max(next, reply.next)
        loaded = true
        return .applied(applied)
    }
}
