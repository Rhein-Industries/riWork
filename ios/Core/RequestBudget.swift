import Foundation

/// How many requests one relay socket may have outstanding, and of which kind. `RelayClient` holds every request to it.
///
/// The relay gives each socket a queue of 16 frames and closes a socket whose peer's queue overflows (`remote/src/relay.rs`).
/// Every request gets at most one reply frame, so holding a socket to 12 outstanding requests also bounds what the desktop can
/// queue toward this phone; the 4 left over cover the relay's own control frames.
///
/// - Long polls (a `shell.output`, `chat.events` or `state.watch` that asks the desktop to wait) take at most 2: as many as the
///   desktop lets one device keep waiting.
/// - Calls (every other request) always have 3 places to themselves, and share the rest.
///
/// A request that does not fit waits for its turn, in order within its kind. It is sealed, sent and timed only once it has a place,
/// and keeps it until its reply arrives (a caller that let go still holds it: the reply is on its way) or the session ends. The place
/// never goes back while that reply may still come: a request unanswered when its own time runs out ends the session, whether or not
/// its caller still waits.
public struct RequestBudget: Sendable, Equatable {
    public enum Kind: Sendable, Equatable { case longPoll, call }

    public static let maximumOutstanding = 12
    public static let maximumLongPolls = 2
    public static let reservedForCalls = 3

    public private(set) var calls = 0
    public private(set) var longPolls = 0
    public init() {}

    public var outstanding: Int { calls + longPolls }

    /// The kind of a request: a long poll when it asks the desktop to wait (`wait_ms` above 0), else a call.
    public static func kind(method: String, params: [String: JSONValue]) -> Kind {
        switch method {
        case "shell.output", "chat.events", "state.watch":
            if case .number(let wait)? = params["wait_ms"], wait > 0 { return .longPoll }
            return .call
        default: return .call
        }
    }

    /// Whether one more request of `kind` fits now.
    public func admits(_ kind: Kind) -> Bool {
        guard outstanding < Self.maximumOutstanding else { return false }
        switch kind {
        case .call: return true
        // The kinds other than calls together never take the places kept for calls.
        case .longPoll: return longPolls < Self.maximumLongPolls && longPolls < Self.maximumOutstanding - Self.reservedForCalls
        }
    }

    public mutating func take(_ kind: Kind) {
        switch kind {
        case .call: calls += 1
        case .longPoll: longPolls += 1
        }
    }

    public mutating func give(_ kind: Kind) {
        switch kind {
        case .call: calls = max(0, calls - 1)
        case .longPoll: longPolls = max(0, longPolls - 1)
        }
    }
}
