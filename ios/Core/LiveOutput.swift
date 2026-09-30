import Foundation

/// How the phone follows the desktop's screen.
public enum SyncMode: Sendable, Equatable {
    /// Not learned yet on this connection.
    case unknown
    /// Interval polling: an older desktop, or one that cannot wait for a change.
    case poll
    /// A long poll (`if_changed` + `wait_ms`) is always waiting on the desktop, which answers as soon as the screen changes.
    case live
    public var label: String { switch self { case .unknown: "…"; case .poll: "poll"; case .live: "live" } }
}

/// One `shell.output` request as this app asks for it.
///
/// Colors are asked for (`styled`) until a desktop turns out not to know the field: one that predates it answers `invalid_request`
/// (unknown field), and the app then reads plain text from it for the rest of the connection.
/// `if_changed` + `wait_ms` are added only once a `hash` has been seen: that is how a desktop shows it can wait for a change.
public struct OutputRequest: Sendable, Equatable {
    public var shellID: String
    public var lines: Int
    /// Ask for SGR colors and attributes.
    public var styled: Bool
    /// The `hash` of the screen already on the phone. The desktop answers `unchanged` while it still matches.
    public var ifChanged: String?
    /// How long the desktop may hold the request back waiting for a change. Only meaningful with `ifChanged`.
    public var waitMilliseconds: Int

    public init(shellID: String, lines: Int, ifChanged: String? = nil, waitMilliseconds: Int = 0, styled: Bool = true) {
        self.shellID = shellID; self.lines = lines; self.styled = styled; self.ifChanged = ifChanged
        self.waitMilliseconds = max(0, min(LiveSync.maximumWaitMilliseconds, waitMilliseconds))
    }
    public var isLongPoll: Bool { ifChanged != nil && waitMilliseconds > 0 }
    public var params: [String: JSONValue] {
        var params: [String: JSONValue] = ["shell_id": .string(shellID), "lines": .number(Double(lines))]
        if styled { params["styled"] = .bool(true) }
        if let ifChanged {
            params["if_changed"] = .string(ifChanged)
            if waitMilliseconds > 0 { params["wait_ms"] = .number(Double(waitMilliseconds)) }
        }
        return params
    }
}

public enum LiveSync {
    /// How long the desktop may hold a long poll back.
    public static let waitMilliseconds = 8000
    public static let maximumWaitMilliseconds = 10_000
    /// The request timeout is the wait plus this, so a healthy long poll never times out and a dead link is still noticed (the
    /// keepalive ping notices one within 20 s anyway). The connector gives the CLI the wait plus 8 s, and a capture or two of up to
    /// 5 s each come on top of the wait; the desktop's advice is the wait plus about 20 s.
    public static let timeoutSlack: Duration = .seconds(20)
    public static func timeout(waitMilliseconds: Int) -> Duration { .milliseconds(max(0, waitMilliseconds)) + timeoutSlack }
    /// Whether an error to a `shell.output` that used the new parameters means "this desktop does not know them": a connector that
    /// predates them answers `invalid_request` (unknown field), and one paired with an older `riwork` CLI says its CLI does not
    /// support styled output.
    public static func rejectsNewParameters(code: String, message: String) -> Bool {
        code == "invalid_request" || (code == "cli_error" && message.lowercased().contains("styled"))
    }
    /// The longest `hash` the desktop accepts back as `if_changed`.
    public static let maximumHashLength = 64
    /// A `hash` the app is willing to hold and send back: 1-64 printable ASCII characters, no spaces.
    public static func isUsableHash(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= maximumHashLength && value.utf8.allSatisfy { $0 > 0x20 && $0 < 0x7F }
    }
}

/// The delay between failed attempts of the long-poll loop: 250 ms, doubling up to 2 s, and back to 250 ms after a success.
public struct LongPollBackoff: Sendable, Equatable {
    public static let initial: Duration = .milliseconds(250)
    public static let maximum: Duration = .seconds(2)
    public private(set) var attempts = 0
    public init() {}
    /// The wait before the next attempt after one more failure.
    public mutating func failure() -> Duration {
        let wait = min(Self.maximum, Self.initial * (1 << min(attempts, 6)))
        attempts += 1
        return wait
    }
    public mutating func success() { attempts = 0 }
}

/// The desktop lets one device have at most two `shell.output` requests waiting at once. A long poll that was cancelled here
/// (a session switch, a pause, a caller that wants the screen now) is still waiting there until its wait runs out, so it keeps
/// holding a slot. This counts them, so the app never asks for a third and the desktop never has to refuse.
public struct WaitSlots: Sendable, Equatable {
    public static let maximumWaiting = 2
    /// The desktop may notice the end of a wait a little after it is due.
    public static let margin = 0.5
    /// When each cancelled wait ends on the desktop (monotonic seconds).
    private var endings: [Double] = []
    public init() {}
    /// A long poll that started at `start` and was asked to wait `wait` seconds was cancelled here.
    public mutating func abandon(startedAt start: Double, wait: Double) { endings.append(start + wait + Self.margin) }
    /// Whether one more waiting request fits next to the cancelled ones that are still waiting.
    public mutating func canWait(at now: Double) -> Bool {
        endings.removeAll { $0 <= now }
        return endings.count < Self.maximumWaiting
    }
    /// Cancelled waits still holding a slot.
    public func stillWaiting(at now: Double) -> Int { endings.filter { $0 > now }.count }
    public mutating func reset() { endings = [] }
}

/// What the desktop answered to `shell.output`.
public enum OutputReply: Sendable, Equatable {
    case screen(ShellOutput)
    /// `{"shell_id", "unchanged": true, "hash"}`: the screen still has the hash the request named; there is no `output`.
    case unchanged(shellID: String, hash: String)

    public init(result: JSONValue) throws {
        if case .bool(true) = result["unchanged"] {
            guard let shellID = result["shell_id"].string, let hash = Self.hash(result["hash"]) else {
                throw RemoteError.protocolViolation("Session output identity mismatch.")
            }
            self = .unchanged(shellID: shellID, hash: hash)
        } else {
            self = .screen(try ShellOutput(result: result))
        }
    }
    /// A usable `hash` field, or nil. A malformed one is ignored, never a reason to lose the screen.
    static func hash(_ value: JSONValue) -> String? {
        guard case .string(let text) = value, LiveSync.isUsableHash(text) else { return nil }
        return text
    }
}

// MARK: - Latency

/// The last `capacity` samples, in milliseconds.
public struct RollingAverage: Sendable, Equatable {
    public let capacity: Int
    private var samples: [Double] = []
    private var next = 0
    public private(set) var last: Double?
    public init(capacity: Int = 20) { self.capacity = max(1, capacity) }
    public var count: Int { samples.count }
    public var average: Double? { samples.isEmpty ? nil : samples.reduce(0, +) / Double(samples.count) }
    public mutating func add(_ milliseconds: Double) {
        guard milliseconds.isFinite, milliseconds >= 0 else { return }
        last = milliseconds
        if samples.count < capacity { samples.append(milliseconds) } else { samples[next] = milliseconds }
        next = (next + 1) % capacity
    }
    public mutating func reset() { samples = []; next = 0; last = nil }
}

/// The numbers behind the "Show latency" overlay. Pure: every call is told what time it is (a monotonic clock in seconds).
///
/// - `keys`: the round trip of the latest `shell.keys` batches.
/// - `output`: the round trip of `shell.output`. A long poll spends most of its time waiting on purpose, so it counts only when
///   the wait can be taken out: a read that did not wait, or an `unchanged` answer that arrives once the wait is over
///   (its time minus the wait, which is the link plus the desktop's own delay).
/// - `echo`: from sending a keys batch to the first changed screen after it.
public struct LatencyBook: Sendable, Equatable {
    public static let window = 20
    /// A screen change later than this after a key batch is not counted as its echo.
    public static let echoTimeout = 5.0

    public private(set) var keys = RollingAverage(capacity: Self.window)
    public private(set) var output = RollingAverage(capacity: Self.window)
    public private(set) var echo = RollingAverage(capacity: Self.window)
    /// Size of the latest changed screen (its text, in UTF-8 bytes).
    public private(set) var payloadBytes: Int?
    /// When the screen last changed.
    public private(set) var lastChangeAt: Double?
    private var pendingEcho: Double?
    public init() {}

    /// A keys batch is about to leave. The earliest one not yet echoed starts the clock.
    public mutating func keysSent(at now: Double) {
        if let started = pendingEcho, now - started <= Self.echoTimeout { return }
        pendingEcho = now
    }
    public mutating func keysAnswered(seconds: Double) { keys.add(seconds * 1000) }

    /// A `shell.output` answer. `waited` is the time the desktop was allowed to hold the request back.
    public mutating func outputAnswered(seconds elapsed: Double, waited: Double, unchanged: Bool, bytes: Int?) {
        if waited <= 0 { output.add(elapsed * 1000) }
        else if unchanged, elapsed >= waited { output.add((elapsed - waited) * 1000) }
        if let bytes, !unchanged { payloadBytes = bytes }
    }
    /// The screen on the phone changed.
    public mutating func screenChanged(at now: Double) {
        lastChangeAt = now
        if let started = pendingEcho {
            if now - started <= Self.echoTimeout { echo.add((now - started) * 1000) }
            pendingEcho = nil
        }
    }
    /// How long ago the screen last changed, in seconds.
    public func age(at now: Double) -> Double? { lastChangeAt.map { max(0, now - $0) } }
    public mutating func reset() { self = LatencyBook() }

    // MARK: Text

    public static func format(milliseconds: Double?) -> String {
        guard let milliseconds else { return "–" }
        return milliseconds < 10 ? String(format: "%.1f", milliseconds) : String(Int(milliseconds.rounded()))
    }
    public static func format(age: Double?) -> String {
        guard let age else { return "–" }
        return age < 10 ? String(format: "%.1fs", age) : (age < 100 ? "\(Int(age.rounded()))s" : "\(Int((age / 60).rounded()))m")
    }
    public static func format(bytes: Int?) -> String {
        guard let bytes else { return "–" }
        return bytes < 1024 ? "\(bytes) B" : String(format: "%.1f KB", Double(bytes) / 1024)
    }
    /// "45 ms (avg 52)" or "–".
    public static func format(_ series: RollingAverage) -> String {
        guard let last = series.last, let average = series.average else { return "–" }
        return "\(format(milliseconds: last)) ms (avg \(format(milliseconds: average)))"
    }
}
