import Foundation

// What the desktop says its agents (Claude, Codex, ...) are doing, as additive, optional fields of `projects.list`,
// `shells.list` and `orchestrators.list` (docs/remote-protocol.md). An older desktop sends none of them, and a newer one may send
// words or numbers this phone does not know. Reading them is therefore lenient: a field that is missing, of another type or out of
// range is ignored, and never fails the list it came in. Everything here is pure and `Sendable`.

/// What the agent in one terminal is doing.
public enum AgentActivity: String, Sendable, Hashable, Encodable, CaseIterable {
    /// Busy: thinking, running a tool, or waiting for its subagents.
    case working
    /// Stopped and needs a person (a question, an approval).
    case waiting
    /// Finished its turn and has nothing more to do.
    case done
    /// Not an agent, or the desktop cannot tell.
    case unknown
    /// The process ended.
    case exited

    /// The word the desktop sent. Anything else (a state added later, another type, nothing) is `.unknown`.
    public init(wire value: JSONValue?) {
        guard let word = value?.string?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased(), let known = Self(rawValue: word) else {
            self = .unknown
            return
        }
        self = known
    }

    /// Whether a tab or row says anything about it. `unknown` and `exited` look like they always did.
    public var isShown: Bool { self == .working || self == .waiting || self == .done }

    /// What VoiceOver says, with the subagents at work when there are any ("Working, 2 subagents"). Nil when nothing is shown.
    public func spoken(subagents: Int = 0) -> String? {
        switch self {
        case .working: subagents > 0 ? "Working, \(Self.count(subagents, "subagent"))" : "Working"
        case .waiting: "Waiting for input"
        case .done: "Done"
        case .unknown, .exited: nil
        }
    }
    static func count(_ n: Int, _ noun: String) -> String { n == 1 ? "1 \(noun)" : "\(n) \(noun)s" }
}

/// The agents at work in a project, counted over its terminals by the desktop.
public struct ProjectAgents: Sendable, Hashable, Encodable {
    public let working: Int
    public let waiting: Int
    public init(working: Int, waiting: Int) { self.working = working; self.waiting = waiting }
    /// An object with `working` and `waiting`; each is read on its own, so one bad number leaves the other. Nil for anything else.
    public init?(wire value: JSONValue?) {
        guard case .object = value else { return nil }
        self.init(working: LenientNumber.count(value?["working"]) ?? 0, waiting: LenientNumber.count(value?["waiting"]) ?? 0)
    }
    /// Nothing to show.
    public var isIdle: Bool { working == 0 && waiting == 0 }
    /// "2 agents working, 1 waiting for input"; nil when idle.
    public var spoken: String? {
        let parts = [working > 0 ? "\(AgentActivity.count(working, "agent")) working" : nil,
                     waiting > 0 ? (working > 0 ? "\(waiting) waiting for input" : "\(AgentActivity.count(waiting, "agent")) waiting for input") : nil]
        let text = parts.compactMap { $0 }.joined(separator: ", ")
        return text.isEmpty ? nil : text
    }
}

/// Numbers from the wire, read leniently: the wrong type, a negative or fractional count or a number past what a counter can hold is
/// nil, and the caller falls back to "unknown".
enum LenientNumber {
    private static let limit = 9_007_199_254_740_992.0   // 2^53: every whole Double below it is exact
    /// A count: a whole number, zero or more.
    static func count(_ value: JSONValue?) -> Int? {
        guard case .number(let number)? = value, number.isFinite, number >= 0, number < limit, number == number.rounded(.towardZero) else { return nil }
        return Int(number)
    }
    /// Unix seconds, more than zero. A fraction is cut off: a desktop that sends 1790000000.5 meant 1790000000.
    static func seconds(_ value: JSONValue?) -> UInt64? {
        guard case .number(let number)? = value, number.isFinite, number >= 1, number < limit else { return nil }
        return UInt64(number)
    }
}
