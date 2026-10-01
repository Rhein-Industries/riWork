import Foundation

// Fetching older scrollback in the background, as a second step after the live screen.
//
// What this file decides, all of it pure so it can be tested without a network:
// - how good the link is, from the pages already fetched (`LinkMeter`) and from what the network path says (`LinkConditions`);
// - how much history to fetch ahead of the reader (`HistoryAppetite`): everything the phone keeps on a good link, a few screens
//   ahead on a poor, metered or restricted one;
// - how big the next page is and how long to leave the link alone after it (`HistoryPrefetch.decide`);
// - when to hold back altogether: while keys are being typed, while two long polls already hold the desktop's shared slots.
//
// The model runs the loop (one `shell.history` request in flight, ever) and asks `decide` before every page.

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
}

/// What pages of history say about the link: how long a request costs before any data moves, how fast data moves, how big a line is.
///
/// A page's time is `fixed + bytes / rate`. `fixed` is the round trip plus the desktop starting its CLI and reading tmux (about 50 to
/// 150 ms by itself); it is learned from the smallest requests (a ten-line probe goes first), and taken out of the bigger pages, so a
/// fast link with a long round trip is not mistaken for a slow one. Bytes are what went on the wire (`HistoryReply.wireBytes`).
///
/// Delays only ever add to a page's time, so a rate worked out from one page is more likely too low than too high, and the more so the
/// smaller the transfer is next to the jitter of the round trip. The rate kept is therefore the best of the last three pages: the
/// capacity the link has shown lately. A link that turns slow is noticed after three slow pages.
public struct LinkMeter: Sendable, Equatable {
    /// At 1 MB/s a page at the protocol's size limit (128 KiB) takes about 130 ms, and the whole 50,000-line history (about 3 MB) 3 s.
    /// Home Wi-Fi and a direct Tailscale path are well above this; a relayed one over LTE usually is not.
    public static let fastBytesPerSecond = 1_000_000.0
    /// At 250 KB/s a 20 KB page (about 350 lines) still takes under 100 ms of transfer, and the whole history 12 s of a link that
    /// is also carrying the keystrokes' echo. Below it, a page that is worth fetching blocks the socket for too long: look ahead only.
    public static let goodBytesPerSecond = 250_000.0
    /// Pages under this are too short for a rate: their time is mostly the fixed cost.
    public static let measurableBytes = 8 * 1024
    /// Requests under this tell the fixed cost.
    public static let smallBytes = 4 * 1024
    static let smoothing = 0.4
    static let recentPages = 3

    /// The fastest small request seen: round trip plus the desktop's own work.
    public private(set) var fixedSeconds: Double?
    /// Bytes per second on pages big enough to tell: the best of the last three.
    public var bytesPerSecond: Double? { recentRates.max() }
    private var recentRates: [Double] = []
    /// Bytes per line, smoothed.
    public private(set) var bytesPerLine: Double?
    public private(set) var lastSeconds: Double?
    public private(set) var lastLines: Int?
    public private(set) var pages = 0
    public init() {}

    public var tier: LinkTier {
        guard let rate = bytesPerSecond else { return .unknown }
        return rate >= Self.fastBytesPerSecond ? .fast : (rate >= Self.goodBytesPerSecond ? .good : .slow)
    }

    private func smoothed(_ old: Double?, _ new: Double) -> Double { old.map { $0 + (new - $0) * Self.smoothing } ?? new }

    /// A page of `lines` lines weighing `wireBytes` took `seconds` from request to answer.
    public mutating func record(wireBytes: Int, lines: Int, seconds: Double) {
        guard seconds.isFinite, seconds > 0, wireBytes >= 0, lines > 0 else { return }
        pages += 1
        lastSeconds = seconds; lastLines = lines
        bytesPerLine = smoothed(bytesPerLine, Double(wireBytes) / Double(lines))
        if wireBytes < Self.smallBytes { fixedSeconds = min(fixedSeconds ?? .infinity, seconds) }
        // Without the fixed cost a rate would only say how long the round trip is.
        guard wireBytes >= Self.measurableBytes, let fixed = fixedSeconds else { return }
        // A fixed cost that was an unlucky high must not turn the rest of the time into nothing: at most ten times the naive rate.
        let transfer = max(seconds - fixed, seconds * 0.1)
        recentRates.append(min(200_000_000, max(5_000, Double(wireBytes) / transfer)))
        if recentRates.count > Self.recentPages { recentRates.removeFirst() }
    }

    /// The path changed (Wi-Fi to cellular, another relay): what was learned about the old one is of no use.
    public mutating func reset() { self = LinkMeter() }
}

/// How much history to fetch ahead of the reader.
public enum HistoryAppetite: Sendable, Equatable {
    /// Everything the phone keeps, one page after another.
    case everything
    /// Keep this many screens loaded above the top of the view; fetch again when fewer than half are left.
    case screens(Int)

    /// A good link on an unrestricted path wants it all; a slow one, a metered one, Low Power Mode or Low Data Mode look ahead only.
    /// Low Data Mode is the person asking for less traffic, so it gets the shortest lookahead.
    public static func appetite(tier: LinkTier, conditions: LinkConditions) -> HistoryAppetite {
        if conditions.constrained { return .screens(5) }
        if conditions.expensive || conditions.lowPower || tier == .slow { return .screens(10) }
        return .everything
    }
}

public enum HistoryPrefetch {
    /// The most a page may weigh on the wire. The reply cap is 128 KiB of encrypted JSON.
    public static let maximumPageWireBytes = 80.0 * 1024
    /// The probe that learns the fixed cost.
    public static let probeLines = 10
    /// The shortest page planned.
    public static let minimumPlannedLines = 40
    /// A reader this close to the top of what is loaded (in screens) is waiting: no pauses between pages.
    public static let urgentScreens = 1.5
    /// How long keys must have been quiet before a page may start.
    public static let typingQuietSeconds = 0.8
    /// How long to leave the history alone after the phone resized the pane. The inline agents (Codex, Grok, Claude Code without the
    /// alternate screen) wipe their scrollback and draw the transcript again at the new width; it settles about half a second later,
    /// and a page taken meanwhile is of a history that is about to be replaced.
    public static let settleAfterResizeSeconds = 0.8
    /// The same after an answer showed the history shrink or the numbering start over (something else redrew).
    public static let settleAfterShrinkSeconds = 0.6

    public struct Input: Sendable {
        /// There is something to fetch: a hole, or older lines the phone does not hold and has room for.
        public var canFetch = true
        /// Lines loaded above the first line in view.
        public var aboveReader = 0
        /// Lines that fit on the screen.
        public var viewRows = 40
        /// Lines missing in the middle (output that scrolled by between two live answers), newest first. Always wanted.
        public var missing = 0
        public var meter = LinkMeter()
        public var conditions = LinkConditions()
        /// Keys are being typed or sent.
        public var typing = false
        /// The live screen changed a moment ago (something is printing).
        public var liveBusy = false
        /// Waits that hold one of the desktop's three shared slots: the live poll in flight and cancelled ones still running there.
        public var waitsHeld = 0
        /// Seconds since the last page was answered.
        public var sinceLastPage: Double?
        /// A fetch loop is already going: it carries on to the far end of a lookahead before it stops.
        public var running = false
        /// Somebody asked (the retry row, the manual fetch): no waiting.
        public var demand = false
        /// Lines per page that the desktop's reply cap let through for this shell, when it had to be cut.
        public var pageCap: Int?
        /// Seconds left of the quiet time after the pane was resized or its history shrank (see `settleSeconds`).
        public var settleRemaining = 0.0
        public init() {}
    }

    public enum Decision: Sendable, Equatable {
        /// Nothing to fetch for now. Asked again when the reader moves, a live answer arrives or the conditions change.
        case idle
        /// Ask again after this many seconds.
        case wait(Double)
        /// Fetch a page of this many lines. `urgent`: the reader is at the top.
        case fetch(lines: Int, urgent: Bool)
    }

    public static func decide(_ input: Input) -> Decision {
        guard input.canFetch else { return .idle }
        // Whatever the reader wants, nothing is asked while the history is being drawn again.
        if input.settleRemaining > 0 { return .wait(max(0.05, input.settleRemaining)) }
        let rows = max(8, input.viewRows)
        let urgent = input.demand || Double(input.aboveReader) < urgentScreens * Double(rows)
        let appetite = HistoryAppetite.appetite(tier: input.meter.tier, conditions: input.conditions)
        if !input.demand, input.missing == 0, case .screens(let n) = appetite {
            let want = n * rows
            let trigger = want / 2
            if input.aboveReader >= (input.running ? want : trigger) { return .idle }
        }
        // Typing comes first. The keys have a lane of their own on the desktop, but a page in flight shares the socket with their echo.
        if input.typing && !input.demand { return .wait(0.4) }
        if !urgent {
            // Two waits and a page fill the desktop's three shared slots: a plain read (the screen after Return) would queue behind it.
            if input.waitsHeld >= 2 { return .wait(0.5) }
            if let since = input.sinceLastPage {
                let gap = gapAfterPage(meter: input.meter, liveBusy: input.liveBusy)
                if since < gap { return .wait(max(0.05, gap - since)) }
            }
            // The first request of the session is a few lines: it costs one round trip and tells how much of the next one is transfer.
            if input.meter.fixedSeconds == nil, input.missing == 0 { return .fetch(lines: probeLines, urgent: false) }
        }
        return .fetch(lines: pageLines(meter: input.meter, cap: input.pageCap), urgent: urgent)
    }

    /// How long the link is left alone after a page, so the echo of a keystroke or a live answer fits between two pages.
    /// A fast link is used almost back to back; a slow one at a quarter of its time.
    public static func gapAfterPage(meter: LinkMeter, liveBusy: Bool) -> Double {
        let last = meter.lastSeconds ?? 0.2
        var gap: Double
        switch meter.tier {
        case .fast: gap = max(0.03, 0.15 * last)
        case .good: gap = max(0.15, last)
        case .slow: gap = max(0.4, 3 * last)
        case .unknown: gap = 0.1
        }
        if liveBusy { gap *= 2 }
        return gap
    }

    /// Lines for the next page: as many as take about 0.3 s (fast), 0.2 s (good) or 0.12 s (slow) to transfer, at most 80 KiB on the
    /// wire and 1000 lines, growing by at most double from one page to the next.
    ///
    /// That time is how long the page can hold up the answer to a keystroke, which comes down the same socket behind it.
    public static func pageLines(meter: LinkMeter, cap: Int? = nil) -> Int {
        let bytesPerLine = max(8, meter.bytesPerLine ?? 60)
        var lines: Int
        if let rate = meter.bytesPerSecond {
            let budget: Double = switch meter.tier { case .fast: 0.30; case .good: 0.20; default: 0.12 }
            lines = Int(min(maximumPageWireBytes, rate * budget) / bytesPerLine)
            if let last = meter.lastLines { lines = min(lines, max(last * 2, HistoryLimits.pageLines)) }
        } else {
            // Sparse history: enough lines to weigh something a rate can be read from.
            lines = max(HistoryLimits.pageLines, Int(Double(LinkMeter.measurableBytes) * 1.5 / bytesPerLine))
        }
        lines = max(minimumPlannedLines, min(HistoryLimits.maximumPageLines, lines))
        if let cap { lines = min(lines, max(cap, HistoryLimits.minimumPageLines)) }
        return lines
    }
}
