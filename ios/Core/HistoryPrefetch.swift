import Foundation

// Fetching older scrollback in the background, as a second step after the live screen.
//
// What this file decides, all of it pure so it can be tested without a network:
// - how much history to fetch ahead of the reader (`HistoryAppetite`), under the person's setting (`HistoryMode`), what the link
//   measures (`LinkMeter`, in LinkMeter.swift) and what the network path says (`LinkConditions`);
// - how big the next page is and how long to leave the link alone after it (`HistoryPrefetch.decide`);
// - when to hold back altogether: while keys are being typed, while two long polls already hold the desktop's shared slots.
//
// The model runs the loop (one `shell.history` request in flight, ever) and asks `decide` before every page.

/// What the person chose for the history download (Settings -> History download).
public enum HistoryMode: String, Sendable, CaseIterable, Equatable {
    /// The default: everything on a link that has the bandwidth for it and is not metered; a few screens ahead otherwise.
    case automatic
    /// The whole history, whatever the link says (the person has decided it is worth it).
    case everything
    /// A few screens above the reader, whatever the link says.
    case ahead
    /// Nothing in the background: a page when the reader scrolls close to the top of what is loaded, or taps to load.
    case off
}

/// How much history to fetch ahead of the reader.
public enum HistoryAppetite: Sendable, Equatable {
    /// Everything the phone keeps, one page after another.
    case everything
    /// Keep this many screens loaded above the top of the view; fetch again when fewer than half are left.
    case screens(Int)

    /// Screens kept above the reader when looking ahead only: on a poor or metered link, Low Power Mode, or when the person asked for
    /// it. Low Data Mode is the person asking for less traffic, so it gets the shortest lookahead.
    public static let lookaheadScreens = 10
    public static let lowDataScreens = 5
    /// With the background fetch off: the page comes when the reader is within 1.5 screens of the top (`urgentScreens`), up to three.
    public static let onDemandScreens = 3
    /// A history whose remainder weighs no more than this on the wire is fetched whole even where looking ahead would be the rule:
    /// compressed, the whole of a typical session is a few hundred KB, which is no reason to hold back on a slow or metered link.
    public static let cheapSlowBytes = 256.0 * 1024
    public static let cheapMeteredBytes = 512.0 * 1024
    public static let cheapLowDataBytes = 128.0 * 1024

    /// Why the appetite is what it is, for the settings screen.
    public enum Reason: String, Sendable, Equatable {
        case chosenEverything, chosenAhead, chosenOff
        case lowData, metered, lowPower, slowLink
        /// Looking ahead would be the rule, but what is left is small enough to fetch whole.
        case cheap
        case goodLink, measuring
    }

    public static func policy(mode: HistoryMode = .automatic, tier: LinkTier, conditions: LinkConditions, remainingWireBytes: Double? = nil) -> (appetite: HistoryAppetite, reason: Reason) {
        switch mode {
        case .everything: return (.everything, .chosenEverything)
        case .ahead: return (.screens(conditions.constrained ? lowDataScreens : lookaheadScreens), .chosenAhead)
        case .off: return (.screens(onDemandScreens), .chosenOff)
        case .automatic: break
        }
        func cheap(_ limit: Double) -> Bool { remainingWireBytes.map { $0 <= limit } ?? false }
        if conditions.constrained { return cheap(cheapLowDataBytes) ? (.everything, .cheap) : (.screens(lowDataScreens), .lowData) }
        if conditions.expensive { return cheap(cheapMeteredBytes) ? (.everything, .cheap) : (.screens(lookaheadScreens), .metered) }
        if conditions.lowPower { return cheap(cheapMeteredBytes) ? (.everything, .cheap) : (.screens(lookaheadScreens), .lowPower) }
        if tier == .slow { return cheap(cheapSlowBytes) ? (.everything, .cheap) : (.screens(lookaheadScreens), .slowLink) }
        return (.everything, tier == .unknown ? .measuring : .goodLink)
    }
    public static func appetite(mode: HistoryMode = .automatic, tier: LinkTier, conditions: LinkConditions, remainingWireBytes: Double? = nil) -> HistoryAppetite {
        policy(mode: mode, tier: tier, conditions: conditions, remainingWireBytes: remainingWireBytes).appetite
    }
}

public enum HistoryPrefetch {
    /// The most a page may weigh on the wire: about 84 KiB of encrypted payload, well inside the reply cap of 128 KiB, which a page
    /// that came out bigger than the last one predicted must not reach.
    public static let maximumPageWireBytes = 112.0 * 1024
    /// The most JSON a page may hold: the phone parses it all, off the main actor, before showing any of it.
    public static let maximumPageJSONBytes = 768.0 * 1024
    /// The probe that learns the fixed cost of a request, for a desktop that does not report its own time.
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
        /// Lines above the oldest one held that the phone would still fetch (the history short of its cap); nil when not known.
        public var remainingLines: Int?
        public var meter = LinkMeter()
        public var conditions = LinkConditions()
        public var mode = HistoryMode.automatic
        /// The most lines the desktop takes in one page (1000 for a desktop that does not say).
        public var maximumLines = HistoryLimits.legacyMaximumPageLines
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
        /// What the remaining history weighs on the wire, as far as the lines seen so far say.
        public var remainingWireBytes: Double? {
            guard let remainingLines, let perLine = meter.bytesPerLine else { return nil }
            return perLine * Double(remainingLines)
        }
    }

    public enum Decision: Sendable, Equatable {
        /// Nothing to fetch for now. Asked again when the reader moves, a live answer arrives or the conditions change.
        case idle
        /// Ask again after this many seconds.
        case wait(Double)
        /// Fetch a page of this many lines. `urgent`: the reader is at the top.
        case fetch(lines: Int, urgent: Bool)
    }

    public static func policy(_ input: Input) -> (appetite: HistoryAppetite, reason: HistoryAppetite.Reason) {
        HistoryAppetite.policy(mode: input.mode, tier: input.meter.tier, conditions: input.conditions, remainingWireBytes: input.remainingWireBytes)
    }

    public static func decide(_ input: Input) -> Decision {
        guard input.canFetch else { return .idle }
        // Whatever the reader wants, nothing is asked while the history is being drawn again.
        if input.settleRemaining > 0 { return .wait(max(0.05, input.settleRemaining)) }
        let rows = max(8, input.viewRows)
        let urgent = input.demand || Double(input.aboveReader) < urgentScreens * Double(rows)
        let appetite = policy(input).appetite
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
            // A desktop that does not report its own time: the first request of the session is a few lines, which costs one round
            // trip and tells how much of the next one is transfer. One that does has already shown its round trip in every reply.
            if !input.meter.knowsRequestCost, input.missing == 0 { return .fetch(lines: probeLines, urgent: false) }
        }
        return .fetch(lines: pageLines(meter: input.meter, cap: input.pageCap, maximumLines: input.maximumLines), urgent: urgent)
    }

    /// How long the link is left alone after a page, so the echo of a keystroke or a live answer fits between two pages.
    /// A fast link is used almost back to back; a slow one at a quarter of its time. The time is what the link spent on the page,
    /// without the desktop's own: the desktop is not in the way of an echo.
    public static func gapAfterPage(meter: LinkMeter, liveBusy: Bool) -> Double {
        let last = meter.lastTransferSeconds ?? meter.lastSeconds ?? 0.2
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

    /// Lines for the next page: as many as take about 0.3 s (fast), 0.2 s (good) or 0.12 s (slow) to transfer, at most 112 KiB on
    /// the wire, 768 KiB of JSON and the desktop's page limit, growing by at most double from one page to the next.
    ///
    /// That time is how long the page can hold up the answer to a keystroke, which comes down the same socket behind it.
    public static func pageLines(meter: LinkMeter, cap: Int? = nil, maximumLines: Int = HistoryLimits.legacyMaximumPageLines) -> Int {
        let bytesPerLine = max(2, meter.bytesPerLine ?? 60)
        var lines: Int
        if let rate = meter.bytesPerSecond {
            let budget: Double = switch meter.tier { case .fast: 0.30; case .good: 0.20; default: 0.12 }
            lines = Int(min(maximumPageWireBytes, rate * budget) / bytesPerLine)
            if let json = meter.jsonBytesPerLine { lines = min(lines, Int(maximumPageJSONBytes / max(8, json))) }
            if let last = meter.lastLines { lines = min(lines, max(last * 2, HistoryLimits.pageLines)) }
        } else {
            // Sparse history: enough lines to weigh something a rate can be read from.
            lines = max(HistoryLimits.pageLines, Int(Double(LinkMeter.measurableBytes) * 1.5 / bytesPerLine))
        }
        lines = max(minimumPlannedLines, min(min(maximumLines, HistoryLimits.maximumPageLines), lines))
        if let cap { lines = min(lines, max(cap, HistoryLimits.minimumPageLines)) }
        return lines
    }
}
