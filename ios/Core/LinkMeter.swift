import Foundation

// What the phone knows about the link to the desktop, and how it works that out.
//
// The path from the phone to the desktop is a WebSocket through a blind relay, usually over Tailscale, on Wi-Fi or cellular. A reply's
// time is not the link's: it is
//
//     elapsed = round trip + what the desktop did (the CLI starting, tmux capturing, compressing) + the transfer
//
// and only the last term says how fast the link is. The desktop reports its own share in every reply (`server_ms`), and every small
// reply (a key acknowledgement, a long poll that found nothing new) is a clean sample of the round trip, because there is nothing to
// transfer. Taking both out of a page leaves the transfer: `LinkMeter` does that, and refuses the samples it cannot trust (a page
// that shared the socket with a big live reply, a page too small to tell).
//
// A desktop that does not report `server_ms` is measured the way it always was: a ten-line probe teaches the fixed cost of a request,
// which is then taken out of the bigger pages.

/// What the network path and the device say about spending data and battery. Filled from `NWPath` (`isConstrained` is Low Data Mode,
/// `isExpensive` cellular or a personal hotspot) and from Low Power Mode.
public struct LinkConditions: Sendable, Equatable {
    public var constrained: Bool
    public var expensive: Bool
    public var lowPower: Bool
    public init(constrained: Bool = false, expensive: Bool = false, lowPower: Bool = false) {
        self.constrained = constrained; self.expensive = expensive; self.lowPower = lowPower
    }
    public var isRestricted: Bool { constrained || expensive || lowPower }
}

/// The flags of the network path, with a memory. `isExpensive` and `isConstrained` are read through a VPN tunnel (the relay is
/// usually reached through Tailscale), and while it connects, roams or re-keys they can switch several times within seconds. A
/// restriction applies the moment it is seen, but is only lifted after the path has been free of it for `calmSeconds`: a flag that
/// flaps does not make the phone fetch everything for a moment and then stop again.
public struct ConditionHold: Sendable, Equatable {
    public static let calmSeconds = 10.0
    public var calm: Double
    private var constrainedUntil = -Double.infinity
    private var expensiveUntil = -Double.infinity
    private var last = LinkConditions()
    public init(calm: Double = ConditionHold.calmSeconds) { self.calm = calm }

    /// Notes what the path says at `now`, and returns what to act on. A flag is held for `calm` seconds from the last time it was seen on:
    /// that is now, or, if it has just gone off, the moment of this report (however long it had been on before).
    @discardableResult
    public mutating func apply(_ raw: LinkConditions, at now: Double) -> LinkConditions {
        if raw.constrained || last.constrained { constrainedUntil = now + calm }
        if raw.expensive || last.expensive { expensiveUntil = now + calm }
        last = raw
        return effective(raw, at: now)
    }
    /// What to act on at `now`, given what the path says now.
    public func effective(_ raw: LinkConditions, at now: Double) -> LinkConditions {
        LinkConditions(constrained: raw.constrained || now < constrainedUntil, expensive: raw.expensive || now < expensiveUntil, lowPower: raw.lowPower)
    }
    /// Seconds until a restriction that is only being held lapses, or nil if none is.
    public func lapse(after now: Double, raw: LinkConditions) -> Double? {
        var soonest: Double?
        if !raw.constrained, constrainedUntil > now { soonest = constrainedUntil - now }
        if !raw.expensive, expensiveUntil > now { soonest = min(soonest ?? .infinity, expensiveUntil - now) }
        return soonest
    }
}

/// Which kind of link the traffic leaves on ("wifi", "cellular", "wired", "other", "none"), with a memory. Through a tunnel the path
/// monitor reports several paths within seconds while the VPN connects or re-keys, and a change of interface makes the model forget
/// what it measured of the link. A change is therefore only passed on once it has lasted `settleSeconds`; the first report is where the
/// phone is, not a change; and a report that brings back the interface in force cancels a pending change.
public struct InterfaceDebounce: Sendable, Equatable {
    public static let settleSeconds = 2.0
    public private(set) var interface: String
    private var pendingKind: String?
    private var pendingSince = 0.0
    private var heard = false
    public init(initial: String = "other") { interface = initial }

    /// The path now says `kind`. Returns true when `interface` changed right away (only the first report does that).
    public mutating func report(_ kind: String, at now: Double) -> Bool {
        if !heard {
            heard = true; pendingKind = nil
            defer { interface = kind }
            return kind != interface
        }
        if kind == interface { pendingKind = nil }
        else if pendingKind != kind { pendingKind = kind; pendingSince = now }
        return false
    }
    /// Seconds until the pending change has lasted long enough, nil if none is pending.
    public func settleDelay(at now: Double) -> Double? {
        pendingKind.map { _ in max(0, pendingSince + Self.settleSeconds - now) }
    }
    /// The pending change has lasted `settleSeconds`: `interface` becomes it. Returns whether it did.
    public mutating func settle(at now: Double) -> Bool {
        guard let kind = pendingKind, now - pendingSince >= Self.settleSeconds - 0.001 else { return false }
        interface = kind; pendingKind = nil
        return true
    }
}

/// How fast the link carries a page, from slowest to fastest.
public enum LinkTier: Int, Sendable, Comparable {
    /// Not measured yet.
    case unknown
    /// Under 250 KB/s (2 Mbit/s).
    case slow
    /// 250 KB/s to 1 MB/s.
    case good
    /// 1 MB/s (8 Mbit/s) and above.
    case fast
    public static func < (lhs: LinkTier, rhs: LinkTier) -> Bool { lhs.rawValue < rhs.rawValue }
    public var label: String { switch self { case .unknown: "measuring"; case .slow: "slow"; case .good: "good"; case .fast: "fast" } }
}

/// One reply that carried a page of history, as the meter takes it.
public struct LinkSample: Sendable, Equatable {
    /// The reply as it travelled (the envelope text).
    public var wireBytes: Int
    /// The JSON it held, after any inflating.
    public var jsonBytes: Int
    /// The encrypted payload, compressed or not.
    public var sealedBytes: Int
    public var lines: Int
    /// Request sent to reply received.
    public var elapsed: Double
    /// The desktop's own time (`server_ms`), nil when it does not say.
    public var serverSeconds: Double?
    /// Bytes of other replies that arrived while this one was out.
    public var concurrentBytes: Int
    public var compressed: Bool
    public init(wireBytes: Int, jsonBytes: Int? = nil, sealedBytes: Int? = nil, lines: Int, elapsed: Double, serverSeconds: Double? = nil, concurrentBytes: Int = 0, compressed: Bool = false) {
        self.wireBytes = wireBytes; self.jsonBytes = jsonBytes ?? wireBytes; self.sealedBytes = sealedBytes ?? wireBytes
        self.lines = lines; self.elapsed = elapsed; self.serverSeconds = serverSeconds; self.concurrentBytes = concurrentBytes; self.compressed = compressed
    }
    public init(timing: ReplyTiming, lines: Int) {
        self.init(wireBytes: timing.wireBytes, jsonBytes: timing.jsonBytes, sealedBytes: timing.sealedBytes, lines: lines, elapsed: timing.elapsed,
                  serverSeconds: timing.serverSeconds, concurrentBytes: timing.concurrentBytes, compressed: timing.compressed)
    }
}

/// What pages of history say about the link: how long a request costs before any data moves, how fast data moves, how big a line is.
///
/// Delays only ever add to a page's time, so a rate worked out from one page is more likely too low than too high, and the more so
/// the smaller the transfer is next to the jitter of the round trip. The rate kept is therefore the best of the last three pages
/// (within the last 45 s): the capacity the link has shown lately. A link that turns slow is noticed after three slow pages, and the
/// tier has a band around each threshold (`hysteresis`) so that a link sitting on one does not change tier with every page.
public struct LinkMeter: Sendable, Equatable {
    /// At 1 MB/s a page at the size limit (about 110 KiB on the wire) takes about 110 ms, and the whole 50,000-line history, which
    /// compresses to a megabyte or so, a second or two. Home Wi-Fi and a direct Tailscale path are well above this; a relayed one over
    /// LTE usually is not.
    public static let fastBytesPerSecond = 1_000_000.0
    /// At 250 KB/s a 30 KB page (about 3,000 lines of build output, compressed) still takes about 120 ms of transfer. Below it, a page
    /// that is worth fetching blocks the socket for too long: look ahead only.
    public static let goodBytesPerSecond = 250_000.0
    /// A tier is left only when the rate falls this far below the threshold that entered it, so a link that sits on a threshold does
    /// not flap: fast is left under 700 KB/s, good under 175 KB/s.
    public static let hysteresis = 0.7
    /// Pages under this are too short for a rate: their time is mostly the round trip.
    public static let measurableBytes = 8 * 1024
    /// Replies under this tell the round trip (or, without `server_ms`, the fixed cost).
    public static let smallBytes = 4 * 1024
    /// A page that shared the socket with this much (of its own size) of other replies says little about the link, and is not used.
    public static let contendedShare = 0.2
    static let smoothing = 0.4
    static let recentPages = 3
    /// Rate samples older than this (seconds) are forgotten when a new one comes in; round trips, 60.
    static let rateMemory = 45.0
    static let roundTripMemory = 60.0
    static let roundTripWindow = 16

    private struct Timed: Sendable, Equatable { var value: Double; var at: Double }

    /// Without `server_ms`: the fastest small request seen, round trip plus the desktop's own work.
    public private(set) var fixedSeconds: Double?
    private var roundTrips: [Timed] = []
    private var rates: [Timed] = []
    /// The tier, with hysteresis.
    public private(set) var tier: LinkTier = .unknown
    /// How many times `tier` changed since the link was last reset (a steady link changes it at most once or twice).
    public private(set) var tierChanges = 0
    /// Bytes per line on the wire, smoothed.
    public private(set) var bytesPerLine: Double?
    /// Bytes of JSON per line, smoothed: what the phone has to parse.
    public private(set) var jsonBytesPerLine: Double?
    /// The desktop's own time per page, smoothed (seconds).
    public private(set) var desktopSeconds: Double?
    /// JSON bytes per sealed byte of the compressed pages, smoothed.
    public private(set) var compressionRatio: Double?
    public private(set) var lastSeconds: Double?
    /// The last page's time less the desktop's and one round trip: what the link spent on it.
    public private(set) var lastTransferSeconds: Double?
    public private(set) var lastLines: Int?
    /// Whether `bytesPerLine` has been worked out from a page (it is a live answer's estimate until then).
    private var lineWeightFromPages = false
    public private(set) var pages = 0
    /// Pages left out of the rate because they shared the socket with other replies.
    public private(set) var contended = 0
    private var clock = 0.0
    public init() {}

    /// Bytes per second on pages big enough to tell: the best of the last three.
    public var bytesPerSecond: Double? { rates.map(\.value).max() }
    /// The network's round trip, from small replies: the fastest of the last minute. Nil until one was seen (or without `server_ms`).
    public var roundTripSeconds: Double? { roundTrips.map(\.value).min() }
    /// How far the typical round trip sits above the fastest (the median less the minimum).
    public var jitterSeconds: Double? {
        guard roundTrips.count >= 3 else { return nil }
        let sorted = roundTrips.map(\.value).sorted()
        return sorted[sorted.count / 2] - sorted[0]
    }
    /// Whether the cost of a request apart from its transfer is known, so a page can be measured without a probe.
    public var knowsRequestCost: Bool { roundTripSeconds != nil || fixedSeconds != nil }
    /// Whether the desktop reports its own time.
    public var desktopReportsTime: Bool { desktopSeconds != nil || !roundTrips.isEmpty }

    private func smoothed(_ old: Double?, _ new: Double) -> Double { old.map { $0 + (new - $0) * Self.smoothing } ?? new }

    /// A reply that was not a page (a key batch's acknowledgement, a screen read, a long poll that found nothing): if the desktop
    /// reports its time and the reply was small, it is a clean sample of the round trip. A long poll's wait is part of the desktop's
    /// time (`server_ms` runs for as long as the request is held), so it falls out like the rest.
    public mutating func observe(_ timing: ReplyTiming, at now: Double) {
        guard timing.wireBytes < Self.smallBytes, let network = timing.networkSeconds, timing.elapsed.isFinite else { return }
        noteRoundTrip(network, at: now)
    }
    private mutating func noteRoundTrip(_ seconds: Double, at now: Double) {
        roundTrips.removeAll { $0.at < now - Self.roundTripMemory }
        roundTrips.append(Timed(value: max(0.0005, seconds), at: now))
        if roundTrips.count > Self.roundTripWindow { roundTrips.removeFirst() }
    }

    /// A live answer (the screen and some recent scrollback): the first idea of what a line weighs, before any page has said. Pages
    /// replace it; a link change does not (see `pathChanged`).
    public mutating func noteLines(wireBytes: Int, jsonBytes: Int, lines: Int) {
        guard !lineWeightFromPages, lines >= 20, wireBytes > 0, jsonBytes > 0 else { return }
        bytesPerLine = Double(wireBytes) / Double(lines)
        jsonBytesPerLine = Double(jsonBytes) / Double(lines)
    }
    /// What a line weighs on the wire, and how well it compresses, were learned under a setting that has changed (compression on or
    /// off): they are learned again, from the next live answer and the pages after it.
    public mutating func forgetContent() {
        bytesPerLine = nil; jsonBytesPerLine = nil; compressionRatio = nil; lineWeightFromPages = false
    }

    /// A page of `sample.lines` lines arrived at `now` (a monotonic clock, in seconds).
    public mutating func record(_ sample: LinkSample, at now: Double) {
        guard sample.elapsed.isFinite, sample.elapsed > 0, sample.wireBytes >= 0, sample.lines > 0 else { return }
        clock = max(clock, now)
        pages += 1
        lastSeconds = sample.elapsed; lastLines = sample.lines
        // The first page replaces the live answer's estimate; after that the weight is smoothed.
        let wirePerLine = Double(sample.wireBytes) / Double(sample.lines), jsonPerLine = Double(sample.jsonBytes) / Double(sample.lines)
        bytesPerLine = lineWeightFromPages ? smoothed(bytesPerLine, wirePerLine) : wirePerLine
        jsonBytesPerLine = lineWeightFromPages ? smoothed(jsonBytesPerLine, jsonPerLine) : jsonPerLine
        lineWeightFromPages = true
        if let server = sample.serverSeconds, server.isFinite, server >= 0 { desktopSeconds = smoothed(desktopSeconds, server) }
        if sample.compressed, sample.jsonBytes >= Self.measurableBytes, sample.sealedBytes > 0 {
            compressionRatio = smoothed(compressionRatio, Double(sample.jsonBytes) / Double(sample.sealedBytes))
        }
        let small = sample.wireBytes < Self.smallBytes
        if let server = sample.serverSeconds {
            if small { noteRoundTrip(max(0, sample.elapsed - server), at: now) }
        } else if small || sample.lines <= HistoryPrefetch.probeLines {
            // The ten-line probe teaches the fixed cost even where its lines are so wide that it is not small in bytes.
            fixedSeconds = min(fixedSeconds ?? .infinity, sample.elapsed)
        }
        // The page's own transfer, as far as it can be told.
        let overhead = sample.serverSeconds.map { $0 + (roundTripSeconds ?? 0) } ?? fixedSeconds
        // An overhead that was an unlucky high must not turn the rest of the time into nothing: at most ten times the naive rate.
        let transfer = overhead.map { max(sample.elapsed - $0, (sample.elapsed - (sample.serverSeconds ?? 0)) * 0.1, 0.002) }
        if let transfer { lastTransferSeconds = transfer }
        // Without the request's cost a rate would only say how long the round trip is.
        guard sample.wireBytes >= Self.measurableBytes, let transfer else { return }
        // Other replies shared the socket for part of this time. A few bytes of them are added back; a lot says nothing.
        guard Double(sample.concurrentBytes) <= Self.contendedShare * Double(sample.wireBytes) else { contended += 1; return }
        let rate = Double(sample.wireBytes + sample.concurrentBytes) / transfer
        rates.removeAll { $0.at < now - Self.rateMemory }
        rates.append(Timed(value: min(200_000_000, max(5_000, rate)), at: now))
        if rates.count > Self.recentPages { rates.removeFirst() }
        settleTier()
    }

    /// Compatibility for callers (and tests) that only have a page's size and time: no desktop time, a clock that runs on by itself.
    public mutating func record(wireBytes: Int, lines: Int, seconds: Double) {
        record(LinkSample(wireBytes: wireBytes, lines: lines, elapsed: seconds), at: clock + max(0, seconds.isFinite ? seconds : 0))
    }

    private mutating func settleTier() {
        guard let rate = bytesPerSecond else { return }
        var next = tier
        if next == .unknown { next = rate >= Self.fastBytesPerSecond ? .fast : (rate >= Self.goodBytesPerSecond ? .good : .slow) }
        else {
            // Up when a threshold is reached, down only when the rate has fallen well below the threshold that was entered.
            if next < .fast, rate >= Self.fastBytesPerSecond { next = .fast }
            else if next == .slow, rate >= Self.goodBytesPerSecond { next = .good }
            if next == .fast, rate < Self.fastBytesPerSecond * Self.hysteresis { next = .good }
            if next == .good, rate < Self.goodBytesPerSecond * Self.hysteresis { next = .slow }
        }
        if next != tier { tier = next; tierChanges += 1 }
    }

    /// The kind of link changed (Wi-Fi to cellular, another relay): the speed and round trip of the old one are of no use. What is
    /// known about the desktop and the history (bytes per line, compression, the desktop's time) is not about the link, and stays.
    public mutating func pathChanged() {
        fixedSeconds = nil; roundTrips = []; rates = []
        tier = .unknown; tierChanges = 0; pages = 0; contended = 0
        lastSeconds = nil; lastTransferSeconds = nil
    }
    /// Forgets everything.
    public mutating func reset() { self = LinkMeter() }
}
