import Foundation

// A chat's transcript as events build it, and the cursor that keeps it in step with the desktop's `chat.events`.

/// A chat's transcript as events build it: what the chat screen draws. This is `Transcript::apply` of `src/chat/model.rs`, line for line,
/// so the phone shows what the desktop's own tab shows: the same events in the same order give the same transcript, whichever side folds them.
public struct ChatTranscript: Sendable, Equatable {
    /// Invalidates grouping only when projection metadata changes; body deltas keep the same grouping.
    public private(set) var projectionRevision = UUID()
    /// A cheap change token for file/attachment indexes, avoiding full item-array equality per token.
    public private(set) var itemsRevision = UUID()
    public private(set) var info: ChatInfo?
    public private(set) var state: ChatState = .starting
    public private(set) var items: [ChatItem] = []
    /// Requests still waiting for the user, in arrival order.
    public private(set) var approvals: [ChatApproval] = []
    public private(set) var questions: [ChatQuestion] = []
    /// Pending and resolved requests in transcript order, with the turn and sequence that introduced them.
    public private(set) var requests: [ChatRequestRecord] = []
    /// Requests the relay could not send in full: they wait on the Mac, where their details are, and nothing here can answer them.
    public private(set) var elidedRequests: [ChatElidedRequest] = []
    public private(set) var usage: ChatUsage?
    /// The models the provider offers, empty until its driver has said (an older driver, or an older desktop, never does).
    public private(set) var models: [ChatModelOption] = []
    /// The provider's usage windows, as its last `rate_limits` said (none from an older driver or desktop).
    public private(set) var rateLimits: [ChatRateWindow] = []
    public private(set) var turnID: String?
    /// Original item arrival sequences, retained across updates for projection's request boundaries.
    public private(set) var itemSequences: [String: UInt64] = [:]
    /// Host turn timestamps, retained for completed turns and merged with historical pages.
    public private(set) var turnTiming: [String: ChatTurnTiming] = [:]
    /// Where each item is, by id, so a delta finds its item without a search.
    private var index: [String: Int] = [:]
    private var lastSequence: UInt64 = 0

    public init() {}

    public static func == (lhs: ChatTranscript, rhs: ChatTranscript) -> Bool {
        lhs.info == rhs.info && lhs.state == rhs.state && lhs.items == rhs.items && lhs.approvals == rhs.approvals
            && lhs.questions == rhs.questions && lhs.requests == rhs.requests && lhs.elidedRequests == rhs.elidedRequests
            && lhs.usage == rhs.usage && lhs.models == rhs.models && lhs.rateLimits == rhs.rateLimits && lhs.turnID == rhs.turnID
            && lhs.itemSequences == rhs.itemSequences
            && lhs.turnTiming == rhs.turnTiming
    }

    public mutating func apply(_ event: ChatEvent, seq: UInt64? = nil) {
        if case .itemDelta = event {} else { projectionRevision = UUID() }
        switch event {
        case .itemStarted, .itemCompleted, .itemDelta, .turnCompleted: itemsRevision = UUID()
        default: break
        }
        let sequence = seq ?? (lastSequence == UInt64.max ? UInt64.max : lastSequence + 1)
        lastSequence = max(lastSequence, sequence)
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
        case .turnStarted(let turnID, let startedAt):
            self.turnID = turnID
            var timing = turnTiming[turnID] ?? ChatTurnTiming()
            timing.startedAt = timing.startedAt ?? startedAt; turnTiming[turnID] = timing
        case .turnCompleted(let turnID, let outcome, let endedAt):
            var timing = turnTiming[turnID] ?? ChatTurnTiming()
            timing.endedAt = endedAt ?? timing.endedAt; turnTiming[turnID] = timing
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
            for at in requests.indices where requests[at].resolvedSeq == nil && (requests[at].turnID == nil || requests[at].turnID == turnID) {
                requests[at].resolvedSeq = sequence; requests[at].resolution = "cancel"
            }
            approvals.removeAll()
            questions.removeAll()
            elidedRequests.removeAll()
        case .itemStarted(let item), .itemCompleted(let item):
            if itemSequences[item.id] == nil { itemSequences[item.id] = sequence }
            if let at = index[item.id] {
                var completed = item
                completed.startedAt = completed.startedAt ?? items[at].startedAt
                completed.presentation.parentID = completed.presentation.parentID ?? items[at].presentation.parentID
                completed.presentation.semantics = completed.presentation.semantics ?? items[at].presentation.semantics
                items[at] = completed
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
            retainRequest(ChatRequestRecord(requestID: approval.requestID, kind: .approval, title: approval.title,
                                            turnID: approval.itemID.flatMap { item($0)?.turnID } ?? turnID, itemID: approval.itemID, requestedSeq: sequence))
        case .approvalResolved(let requestID, let decision):
            resolveRequest(requestID, sequence: sequence, resolution: decision.rawValue)
            approvals.removeAll { $0.requestID == requestID }
            elidedRequests.removeAll { $0.requestID == requestID }
        case .questionRequested(let question):
            questions.removeAll { $0.requestID == question.requestID }
            elidedRequests.removeAll { $0.requestID == question.requestID }
            questions.append(question)
            retainRequest(ChatRequestRecord(requestID: question.requestID, kind: .question,
                                            title: question.questions.map(\.question).joined(separator: "; "),
                                            turnID: question.itemID.flatMap { item($0)?.turnID } ?? turnID,
                                            itemID: question.itemID, requestedSeq: sequence))
        case .questionResolved(let requestID, let answerSummary):
            resolveRequest(requestID, sequence: sequence, resolution: "answered", answerSummary: answerSummary)
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
                if elided.isRequest {
                    let question = elided.of == "question_requested"
                    elidedRequests.append(ChatElidedRequest(requestID: requestID, question: question))
                    retainRequest(ChatRequestRecord(requestID: requestID, kind: question ? .question : .approval,
                                                    title: requests.first { $0.requestID == requestID }?.title ?? "",
                                                    turnID: elided.turnID ?? elided.itemID.flatMap { item($0)?.turnID } ?? turnID,
                                                    itemID: elided.itemID, requestedSeq: sequence))
                } else {
                    resolveRequest(requestID, sequence: sequence, resolution: elided.of == "question_resolved" ? "answered" : nil)
                }
                return
            }
            // The feed resolves these by sequence number; one that comes without (a snapshot's controls) is keyed by its place.
            let row = event.resolved(seq: UInt64(items.count))
            if row != event { apply(row, seq: sequence) }
        }
    }

    private mutating func retainRequest(_ record: ChatRequestRecord) {
        if let at = requests.firstIndex(where: { $0.requestID == record.requestID }) {
            // A repeated pending request updates its title without changing its place in the transcript.
            requests[at].title = record.title
            requests[at].itemID = record.itemID ?? requests[at].itemID
            requests[at].turnID = record.turnID ?? requests[at].turnID
        } else {
            requests.append(record)
            requests.sort { $0.requestedSeq < $1.requestedSeq }
        }
    }
    private mutating func resolveRequest(_ id: String, sequence: UInt64, resolution: String?, answerSummary: String? = nil) {
        guard let at = requests.firstIndex(where: { $0.requestID == id }) else { return }
        requests[at].resolvedSeq = sequence; requests[at].resolution = resolution; requests[at].answerSummary = answerSummary
    }
    fileprivate mutating func installRequests(_ records: [ChatRequestRecord], through seq: UInt64) {
        projectionRevision = UUID()
        requests = records; lastSequence = seq
    }
    /// Historical pages recover original boundaries and missing records while newer live resolutions remain authoritative.
    fileprivate mutating func mergeRequestHistory(_ records: [ChatRequestRecord]) {
        projectionRevision = UUID()
        for record in records {
            if let at = requests.firstIndex(where: { $0.requestID == record.requestID }) {
                requests[at].requestedSeq = min(requests[at].requestedSeq, record.requestedSeq)
                requests[at].turnID = requests[at].turnID ?? record.turnID
                requests[at].itemID = requests[at].itemID ?? record.itemID
                if requests[at].title.isEmpty { requests[at].title = record.title }
                if let resolved = record.resolvedSeq, resolved > (requests[at].resolvedSeq ?? 0) {
                    requests[at].resolvedSeq = resolved; requests[at].resolution = record.resolution
                    requests[at].answerSummary = record.answerSummary
                }
            } else { requests.append(record) }
        }
        requests.sort { $0.requestedSeq < $1.requestedSeq }
    }
    /// Only records present at the recovery checkpoint override replay; older records can still be reconstructed.
    fileprivate mutating func restoreRequests(_ records: [ChatRequestRecord]) {
        projectionRevision = UUID()
        let historical = requests
        requests = records
        var known = Set(records.map(\.requestID))
        for record in historical where known.insert(record.requestID).inserted { requests.append(record) }
        requests.sort { $0.requestedSeq < $1.requestedSeq }
    }

    fileprivate mutating func applyRecovering(_ event: ChatEvent, seq: UInt64, requestTurnID: String?) {
        let liveTurnID = turnID
        turnID = requestTurnID
        apply(event, seq: seq)
        turnID = liveTurnID
    }

    /// Preserve already-authoritative controls while exceptional replay reconstructs older bodies.
    fileprivate func controlsOnly() -> ChatTranscript {
        var value = self; value.projectionRevision = UUID(); value.itemsRevision = UUID(); value.items = []; value.index = [:]; value.itemSequences = [:]; return value
    }
    fileprivate mutating func restoreControls(_ value: ChatTranscript) {
        projectionRevision = UUID()
        info = value.info; state = value.state; approvals = value.approvals; questions = value.questions; elidedRequests = value.elidedRequests
        usage = value.usage; models = value.models; rateLimits = value.rateLimits; turnID = value.turnID
        mergeTurnTiming(value.turnTiming, preferIncoming: true)
        lastSequence = max(lastSequence, value.lastSequence)
    }

    fileprivate mutating func mergeTurnTiming(_ records: [String: ChatTurnTiming], preferIncoming: Bool = false) {
        projectionRevision = UUID()
        for (id, record) in records {
            let existing = turnTiming[id] ?? ChatTurnTiming()
            turnTiming[id] = ChatTurnTiming(startedAt: preferIncoming ? record.startedAt ?? existing.startedAt : existing.startedAt ?? record.startedAt,
                                           endedAt: preferIncoming ? record.endedAt ?? existing.endedAt : existing.endedAt ?? record.endedAt)
        }
    }

    /// Merge only missing historical rows; live rows and controls remain authoritative.
    public mutating func mergeHistory(_ rows: [ChatSnapshotRow], orders: [String: UInt64]) {
        projectionRevision = UUID(); itemsRevision = UUID()
        for row in rows where index[row.item.id] == nil { apply(.itemCompleted(row.item)) }
        for item in items { if let order = orders[item.id] { itemSequences[item.id] = order } }
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

/// A retained request boundary (`RequestRecord`), also sent by snapshot v1 hosts that support tool activity.
public struct ChatRequestRecord: Codable, Sendable, Equatable, Identifiable {
    public enum Kind: String, Codable, Sendable { case approval, question }
    public var requestID: String
    public var kind: Kind
    public var title: String
    public var turnID: String?
    public var itemID: String?
    public var requestedSeq: UInt64
    public var resolvedSeq: UInt64?
    /// `accept`, `accept_for_session`, `decline`, `cancel` or `answered`; future words are retained.
    /// A resolved record with no known word (for example an elided approval resolution) has the prefix "Resolved".
    public var resolution: String?
    public var answerSummary: String?
    public var id: String { requestID }
    public init(requestID: String, kind: Kind, title: String = "", turnID: String? = nil, itemID: String? = nil,
                requestedSeq: UInt64 = 0, resolvedSeq: UInt64? = nil, resolution: String? = nil, answerSummary: String? = nil) {
        self.requestID = requestID; self.kind = kind; self.title = title; self.turnID = turnID; self.itemID = itemID
        self.requestedSeq = requestedSeq; self.resolvedSeq = resolvedSeq; self.resolution = resolution; self.answerSummary = answerSummary
    }
    private enum CodingKeys: String, CodingKey {
        case requestID = "request_id", kind, title, turnID = "turn_id", itemID = "item_id"
        case requestedSeq = "requested_seq", resolvedSeq = "resolved_seq", resolution, answerSummary = "answer_summary"
    }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        requestID = try c.decode(String.self, forKey: .requestID); kind = try c.decode(Kind.self, forKey: .kind)
        title = c.tolerant(String.self, forKey: .title) ?? ""; turnID = c.tolerant(String.self, forKey: .turnID)
        itemID = c.tolerant(String.self, forKey: .itemID); requestedSeq = c.tolerant(UInt64.self, forKey: .requestedSeq) ?? 0
        resolvedSeq = c.tolerant(UInt64.self, forKey: .resolvedSeq); resolution = c.tolerant(String.self, forKey: .resolution)
        answerSummary = c.tolerant(String.self, forKey: .answerSummary)
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
    /// The oldest loaded turn is incomplete because its snapshot hit the relay byte limit.
    /// Hydrating individual items cannot complete that boundary; prepending history can.
    public private(set) var provisional = false
    private var orders: [String: UInt64] = [:]
    private var hidden = ChatTranscript()
    // Edits to unloaded rows and request resolutions retain their original sequence. History applies them to its frozen base,
    // never to the live controls; replaying a page therefore cannot resolve a new request.
    private var deferred: [ChatEnvelope] = []
    private var deferredBytes = 0
    public private(set) var degradedReplay = false
    private var recoveryThrough: UInt64 = 0
    private var recoveryRequests: [ChatRequestRecord] = []
    private var recoveryRequestTurnID: String?

    public init() {}

    public mutating func install(_ snapshot: ChatSnapshotReply) {
        self = ChatFeed()
        next = snapshot.next; loaded = true; historyCursor = snapshot.cursor; itemArrivals = snapshot.items.count
        before = snapshot.before; hasOlder = snapshot.more; dismissedNotices = Set(snapshot.dismissedNotices)
        provisional = snapshot.provisional
        for event in snapshot.controls { transcript.apply(event) }
        transcript.mergeTurnTiming(snapshot.turnTiming, preferIncoming: true)
        for row in snapshot.items { orders[row.item.id] = row.order }
        transcript.mergeHistory(snapshot.items, orders: orders)
        transcript.installRequests(snapshot.requests ?? transcript.requests, through: snapshot.next)
    }

    public mutating func beginDegradedReplay() {
        let controls = transcript.controlsOnly(), checkpoint = next, arrivals = itemArrivals, dismissed = dismissedNotices
        self = ChatFeed(); transcript = controls; recoveryThrough = checkpoint; dismissedNotices = dismissed
        recoveryRequests = controls.requests
        itemArrivals = arrivals; degradedReplay = true
    }

    public func hasItem(_ id: String) -> Bool { transcript.item(id) != nil || hidden.item(id) != nil }

    /// Hydrate preexisting unloaded rows before applying live changes to them.
    public mutating func hydrate(_ page: ChatSnapshotReply) {
        guard page.cursor == historyCursor else { return }
        for row in page.items { orders[row.item.id] = row.order }
        var base = ChatTranscript()
        base.mergeTurnTiming(page.turnTiming)
        for row in page.items { base.apply(.itemCompleted(row.item), seq: row.order) }
        base.installRequests(page.requests ?? [], through: page.next)
        for envelope in deferred { if let event = envelope.event { base.apply(event, seq: envelope.seq) } }
        for item in base.items where !hasItem(item.id) { hidden.apply(.itemCompleted(item)) }
        hidden.mergeRequestHistory(base.requests)
        hidden.mergeTurnTiming(base.turnTiming)
    }

    public mutating func prepend(_ page: ChatSnapshotReply, requestedBefore: UInt64) {
        guard page.cursor == historyCursor, requestedBefore == before, page.before < before || !page.more else { return }
        var historical = ChatTranscript()
        historical.mergeTurnTiming(page.turnTiming)
        for row in page.items { orders[row.item.id] = row.order; historical.apply(.itemCompleted(row.item), seq: row.order) }
        historical.installRequests(page.requests ?? [], through: page.next)
        for envelope in deferred { if let event = envelope.event { historical.apply(event, seq: envelope.seq) } }
        let rows = historical.items.map { ChatSnapshotRow(order: orders[$0.id] ?? 0, item: hidden.item($0.id) ?? $0) }
        transcript.mergeHistory(rows, orders: orders)
        transcript.mergeRequestHistory(historical.requests)
        transcript.mergeTurnTiming(historical.turnTiming)
        before = page.before; hasOlder = page.more
        // A provisional newest edge joins the rows already held. An oldest edge
        // remains partial whenever a represented turn started before the cursor,
        // even if a legacy/no-turn item precedes that turn on the page.
        let clippedKnownTurn = page.items.contains { row in
            row.item.turnID.flatMap { page.turnStarts[$0] }.map { $0 < page.before } ?? false
        }
        let legacyBoundary = page.items.first.map { row in
            guard let turn = row.item.turnID else { return false }
            return page.turnStarts[turn] == nil && page.before == row.order
        } ?? true
        provisional = page.provisional && page.more && (clippedKnownTurn || legacyBoundary)
        deferred.removeAll { envelope in
            guard let event = envelope.event else { return true }
            switch event {
            case .itemDelta(let id, _): return transcript.item(id) != nil
            case .itemStarted(let item), .itemCompleted(let item): return transcript.item(item.id) != nil
            default: return !hasOlder
            }
        }
        deferredBytes = deferred.reduce(0) { $0 + ($1.event.flatMap { try? JSONEncoder().encode($0).count } ?? 0) }
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
                if protectedControls != nil, case .turnStarted(let id, _) = event { recoveryRequestTurnID = id }
                if historyCursor != nil {
                    switch event {
                    case .itemStarted(let item), .itemCompleted(let item):
                        if orders[item.id] == nil { orders[item.id] = envelope.seq }
                    case .itemDelta(let id, _):
                        if !hasItem(id) { deferred.append(envelope); deferredBytes += (try? JSONEncoder().encode(event).count) ?? 0 }
                    case .turnCompleted, .approvalResolved, .questionResolved:
                        if hasOlder { deferred.append(envelope); deferredBytes += (try? JSONEncoder().encode(event).count) ?? 0 }
                    case .elided(let elided) where elided.isResolution:
                        if hasOlder { deferred.append(envelope); deferredBytes += (try? JSONEncoder().encode(event).count) ?? 0 }
                    default: break
                    }
                }
                switch event {
                case .itemStarted(let item), .itemCompleted(let item):
                    // A provider notice is the banner row's, not a row of the transcript: it is not news there.
                    if !hasItem(item.id), envelope.seq > recoveryThrough, ChatNotices.isTranscriptRow(item) { itemArrivals += 1 }
                    if hidden.item(item.id) != nil && transcript.item(item.id) == nil { hidden.apply(event, seq: envelope.seq) }
                    else { transcript.apply(event, seq: envelope.seq) }
                case .itemDelta(let id, _):
                    if hidden.item(id) != nil && transcript.item(id) == nil { hidden.apply(event, seq: envelope.seq) }
                    else { transcript.apply(event, seq: envelope.seq) }
                default:
                    hidden.apply(event, seq: envelope.seq)
                    if protectedControls != nil { transcript.applyRecovering(event, seq: envelope.seq, requestTurnID: recoveryRequestTurnID) }
                    else { transcript.apply(event, seq: envelope.seq) }
                }
                if let protectedControls {
                    transcript.restoreControls(protectedControls)
                    transcript.restoreRequests(recoveryRequests)
                    if case .turnCompleted(let id, _, _) = event, id == recoveryRequestTurnID { recoveryRequestTurnID = nil }
                }
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
