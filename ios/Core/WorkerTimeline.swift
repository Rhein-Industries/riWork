import Foundation

// RiWork's own workers (a worker shell or a worker chat) as cards in the transcript of the chat that started them. The desktop's chat
// host writes a marker item into that chat's log when the chat starts a worker, and writes the same item again (same id) with the
// worker's final snapshot when it ends: the phone gets both through `chat.events` / `chat.snapshot`, like any other item. While a
// worker runs, its state comes from the lists the phone already reads (`shells.list`, `chats.list`), joined by the marker's session id.
// The desktop applies the same rules to the same items.

/// A marker's `presentation.worker`. Read leniently: a word the phone does not know is `.unknown`, a field it does not know is
/// ignored, and a marker without a session id or a time is no marker (the item is then the plain tool row it also is).
public struct ChatWorker: Codable, Sendable, Equatable {
    public enum Kind: String, Codable, Sendable { case shell, chat, unknown }
    /// What the host wrote when the worker ended.
    public struct Final: Codable, Sendable, Equatable {
        public enum Status: String, Codable, Sendable {
            case done, exited, failed, stopped, unknown
            public var title: String {
                switch self { case .done: "Done"; case .exited: "Exited"; case .failed: "Failed"; case .stopped: "Stopped"; case .unknown: "Status not reported" }
            }
        }
        public let status: Status
        /// Unix seconds, as the desktop's clock had it.
        public let endedAt: UInt64?
        /// The last lines of its output; may be empty.
        public let tail: String
        public init(status: Status, endedAt: UInt64?, tail: String = "") {
            self.status = status; self.endedAt = endedAt; self.tail = String(tail.prefix(ChatWorker.tailLimit))
        }
        private enum Keys: String, CodingKey { case status, tail; case endedAt = "ended_at" }
        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: Keys.self)
            status = c.lenient(Status.self, forKey: .status, default: .unknown)
            endedAt = LenientNumber.seconds(c.tolerant(JSONValue.self, forKey: .endedAt))
            tail = String((c.tolerant(String.self, forKey: .tail) ?? "").prefix(ChatWorker.tailLimit))
        }
        public func encode(to encoder: any Encoder) throws {
            var c = encoder.container(keyedBy: Keys.self)
            try c.encode(status, forKey: .status); try c.encodeIfPresent(endedAt, forKey: .endedAt); try c.encode(tail, forKey: .tail)
        }
    }

    static let tailLimit = 4096
    static let assignmentLimit = 240

    /// The worker shell's or worker chat's id: what joins the card to the lists.
    public let sessionID: String
    public let kind: Kind
    public let title: String
    public let harness: String?
    public let model: String?
    public let worktreeID: String?
    /// The first line of what the chat sent the worker.
    public let assignment: String?
    /// Unix seconds when the host wrote (or last re-assigned) this marker, as the desktop's clock had it.
    public let notedAt: UInt64
    /// Written by reconciliation, after the worker had started: the chat did not see it start.
    public let late: Bool
    /// The chat gave a worker it already had more work.
    public let followUp: Bool
    /// Present once the worker ended.
    public let final: Final?

    public init(sessionID: String, kind: Kind = .shell, title: String, harness: String? = nil, model: String? = nil, worktreeID: String? = nil,
                assignment: String? = nil, notedAt: UInt64, late: Bool = false, followUp: Bool = false, final: Final? = nil) {
        self.sessionID = sessionID; self.kind = kind; self.title = title; self.harness = harness; self.model = model; self.worktreeID = worktreeID
        self.assignment = assignment.map { String($0.prefix(Self.assignmentLimit)) }; self.notedAt = notedAt
        self.late = late; self.followUp = followUp; self.final = final
    }

    private enum Keys: String, CodingKey {
        case kind, title, harness, model, assignment, late, final
        case sessionID = "session_id", worktreeID = "worktree_id", notedAt = "noted_at", followUp = "follow_up"
    }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let session = try c.decode(String.self, forKey: .sessionID)
        guard !session.isEmpty else { throw DecodingError.dataCorruptedError(forKey: .sessionID, in: c, debugDescription: "Empty session id") }
        guard let noted = LenientNumber.seconds(c.tolerant(JSONValue.self, forKey: .notedAt)) else {
            throw DecodingError.dataCorruptedError(forKey: .notedAt, in: c, debugDescription: "No time")
        }
        sessionID = session
        notedAt = noted
        kind = c.lenient(Kind.self, forKey: .kind, default: .unknown)
        title = c.tolerant(String.self, forKey: .title).map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .flatMap { $0.isEmpty ? nil : String($0.prefix(160)) } ?? "Worker"
        harness = c.tolerant(String.self, forKey: .harness)
        model = c.tolerant(String.self, forKey: .model)
        worktreeID = c.tolerant(String.self, forKey: .worktreeID)
        assignment = c.tolerant(String.self, forKey: .assignment).map { String($0.prefix(Self.assignmentLimit)) }
        late = c.tolerant(Bool.self, forKey: .late) ?? false
        followUp = c.tolerant(Bool.self, forKey: .followUp) ?? false
        // Any object is a final snapshot (its fields are read leniently); something else under `final` makes the marker unreadable,
        // as it does on the desktop, rather than a worker shown as still running.
        final = try c.decodeIfPresent(Final.self, forKey: .final)
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(sessionID, forKey: .sessionID); try c.encode(kind, forKey: .kind); try c.encode(title, forKey: .title)
        try c.encodeIfPresent(harness, forKey: .harness); try c.encodeIfPresent(model, forKey: .model); try c.encodeIfPresent(worktreeID, forKey: .worktreeID)
        try c.encodeIfPresent(assignment, forKey: .assignment); try c.encode(notedAt, forKey: .notedAt)
        if late { try c.encode(true, forKey: .late) }
        if followUp { try c.encode(true, forKey: .followUp) }
        try c.encodeIfPresent(final, forKey: .final)
    }
}

/// What the phone has listed that can say how a running worker is doing. A list is nil when the phone has none that would include this
/// chat's workers (no chat list from the desktop; the shells of another project): the card then says the status is unavailable rather
/// than that the worker ended.
public struct WorkerInventory: Sendable, Equatable {
    public var shells: [RemoteSession]?
    public var chats: [ChatInfo]?
    public init(shells: [RemoteSession]?, chats: [ChatInfo]?) { self.shells = shells; self.chats = chats }
    public static let unavailable = WorkerInventory(shells: nil, chats: nil)

    func phase(of worker: ChatWorker) -> WorkerCardPhase {
        switch worker.kind {
        case .chat:
            guard let chats else { return .unavailable }
            guard let chat = chats.first(where: { $0.id == worker.sessionID }) else { return .unlisted }
            return switch chat.state {
            case .starting: .listed(status: "Starting", activity: .working)
            case .running: .listed(status: "Working", activity: .working)
            case .waiting: .listed(status: "Waiting for input", activity: .waiting)
            case .idle: .listed(status: "Done", activity: .done)
            case .stopped: .listed(status: "Stopped", activity: .exited)
            case .failed: .listed(status: "Failed", activity: .exited)
            case .unknown: .listed(status: "Unknown", activity: .unknown)
            }
        case .shell, .unknown:
            // A session id is unique: a match in any list is this worker, whatever the list is of.
            if let shell = shells?.first(where: { $0.id == worker.sessionID }) {
                return .listed(status: WorkerShellCard(session: shell).status, activity: shell.shownActivity)
            }
            return shells == nil ? .unavailable : .unlisted
        }
    }
}

/// Where a card stands, apart from the clock.
public enum WorkerCardPhase: Sendable, Equatable {
    /// The host wrote the worker's final snapshot into this card.
    case finished(ChatWorker.Final)
    /// A later card of this chat is for the same worker: this one is history.
    case continued
    /// In the phone's list: the status it gives, and what it is doing (working, waiting, or listed in a state the desktop does not name
    /// count as running; done is finished its turn; exited is gone, stopped or failed).
    case listed(status: String, activity: AgentActivity)
    /// Not in a list that would have it: not listed yet (just started), or gone without a final snapshot.
    case unlisted
    /// The phone has no list that could say.
    case unavailable
}

/// One marker of a transcript, with where it stands.
public struct WorkerCard: Sendable, Equatable, Identifiable {
    /// A worker missing from the list this soon after it was noted is taken to be starting (the list has not caught up yet).
    public static let startingWindow: UInt64 = 30

    public let itemID: String
    public let worker: ChatWorker
    public let phase: WorkerCardPhase
    public var id: String { itemID }

    public init(itemID: String, worker: ChatWorker, phase: WorkerCardPhase) { self.itemID = itemID; self.worker = worker; self.phase = phase }

    /// No final snapshot and nothing after it for the same worker: its state is the lists', and they are kept fresh while it is shown.
    public var isLive: Bool {
        switch phase { case .finished, .continued: false; case .listed, .unlisted, .unavailable: true }
    }
    private func starting(_ now: UInt64) -> Bool { now < worker.notedAt || now - worker.notedAt < Self.startingWindow }

    public func status(now: UInt64) -> String {
        switch phase {
        case .finished(let final): final.status.title
        case .continued: "Continued below"
        case .listed(let status, _): status
        case .unlisted: starting(now) ? "Starting" : "Ended · details unavailable"
        case .unavailable: "Status unavailable"
        }
    }
    /// What the "workers running" chip counts.
    public func running(now: UInt64) -> Bool {
        switch phase {
        case .listed(_, let activity): activity == .working || activity == .waiting || activity == .unknown
        case .unlisted: starting(now)
        case .finished, .continued, .unavailable: false
        }
    }
    /// The indicator beside the card's title: working (also while starting), waiting, done; nothing for the rest.
    public func activity(now: UInt64) -> AgentActivity {
        switch phase {
        case .listed(_, let activity): activity
        case .unlisted: starting(now) ? .working : .exited
        case .finished(let final): final.status == .done ? .done : .exited
        case .continued, .unavailable: .unknown
        }
    }
    /// Seconds since the worker was noted (to its end, once it has one); nil when there is nothing honest to count.
    public func elapsed(now: UInt64) -> UInt64? {
        switch phase {
        case .finished(let final): final.endedAt.map { $0 > worker.notedAt ? $0 - worker.notedAt : 0 }
        case .listed: now > worker.notedAt ? now - worker.notedAt : 0
        case .unlisted: starting(now) ? (now > worker.notedAt ? now - worker.notedAt : 0) : nil
        case .continued, .unavailable: nil
        }
    }
    /// "45s", "3m 07s", "2h 05m".
    public static func duration(_ seconds: UInt64) -> String {
        if seconds < 60 { return "\(seconds)s" }
        if seconds < 3600 { return "\(seconds / 60)m " + String(format: "%02ds", Int(seconds % 60)) }
        return "\(seconds / 3600)h " + String(format: "%02dm", Int(seconds / 60 % 60))
    }
}

/// The worker cards of one transcript. Only a worker's latest marker in a chat is live; an earlier one without a final snapshot is
/// "Continued below". Built once per transcript change, in one pass.
public struct WorkerTimeline: Sendable, Equatable {
    /// In transcript order.
    public let cards: [WorkerCard]
    private let byItem: [String: Int]

    public init(items: [ChatItem], inventory: WorkerInventory) {
        var latest: [String: Int] = [:]
        for (at, item) in items.enumerated() { if let worker = item.worker { latest[worker.sessionID] = at } }
        var cards: [WorkerCard] = []
        var byItem: [String: Int] = [:]
        if !latest.isEmpty {
            for (at, item) in items.enumerated() {
                guard let worker = item.worker else { continue }
                let phase: WorkerCardPhase = if let final = worker.final { .finished(final) }
                    else if latest[worker.sessionID] != at { .continued }
                    else { inventory.phase(of: worker) }
                byItem[item.id] = cards.count
                cards.append(WorkerCard(itemID: item.id, worker: worker, phase: phase))
            }
        }
        self.cards = cards; self.byItem = byItem
    }

    public func card(for itemID: String) -> WorkerCard? { byItem[itemID].map { cards[$0] } }
    /// Some card still takes its state from the lists: they are read again while the chat is on screen.
    public var hasLive: Bool { cards.contains(where: \.isLive) }
    /// The cards that count as running now, in transcript order.
    public func running(now: UInt64) -> [WorkerCard] { cards.filter { $0.running(now: now) } }

    /// The chip over the transcript while running workers are scrolled out of view: nil for none.
    public static func chipTitle(running count: Int) -> String? {
        count <= 0 ? nil : count == 1 ? "1 worker running ↑" : "\(count) workers running ↑"
    }
}
