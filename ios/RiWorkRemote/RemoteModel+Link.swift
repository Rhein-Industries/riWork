import Foundation
import RiWorkCore

// The link: what the phone knows about the way to the desktop, and the settings that steer what it does with it (the history
// download mode, asking for compressed replies). The arithmetic is in RiWorkCore (`LinkMeter`, `HistoryPrefetch`, `HistoryAppetite`,
// `ConditionHold`); this file feeds it. Every reply the transport timed lands in `noteReply`; every page of history in
// `noteHistoryPage`; the network path in `linkChanged`.
extension RemoteModel {
    var linkNow: Double { ProcessInfo.processInfo.systemUptime }

    // MARK: Feeding the meter

    /// A reply that was not a page: if it is small and the desktop timed itself, it is a clean sample of the round trip.
    func noteReply(_ timing: ReplyTiming?) {
        guard let timing else { return }
        linkMeter.observe(timing, at: linkNow)
        if timing.compressed { compressionAgreed = true }
    }

    /// A page of history arrived. The transport's timing (from when the request left to when the reply arrived whole, with the
    /// desktop's own time and the replies that shared the socket) is used when there is one; without it the page is timed here and
    /// weighed by its text, the way it was before the desktop reported anything.
    func noteHistoryPage(reply: HistoryReply, timing: ReplyTiming?, lines: Int, elapsed: Double) {
        if let timing {
            linkMeter.record(LinkSample(timing: timing, lines: lines), at: linkNow)
            if timing.compressed { compressionAgreed = true }
        } else {
            linkMeter.record(LinkSample(wireBytes: reply.wireBytes, lines: lines, elapsed: elapsed), at: linkNow)
        }
    }

    // MARK: What the desktop offers

    /// Reads what the desktop announced when the session began: whether it compresses, and how long a page may be.
    func learnDesktopFeatures(token: UUID) async {
        let features = await client.desktopFeatures()
        guard generation == token else { return }
        desktopFeatures = features
        historyLineLimit = features.historyMaximumLines
        compressionAgreed = await client.compressionActive()
        // The desktop's answer to the opt-in is on its way (the transport asked as soon as the session began): look again shortly.
        guard features.deflate, compressTraffic, !compressionAgreed else { return }
        Task { [weak self] in
            for delay in [0.3, 1.0, 3.0] {
                try? await Task.sleep(for: .seconds(delay))
                guard let self, self.generation == token else { return }
                let agreed = await self.client.compressionActive()
                if self.generation == token { self.compressionAgreed = agreed }
                if agreed { return }
            }
        }
    }
    /// Whether a refusal of a history page is the desktop saying it takes fewer lines than it announced, and how many. Its CLI may be an
    /// older build than its connector: `--lines needs an integer from 1 to 1000`, or the connector's own `lines must be 1..=1000`.
    static func rejectedPageLines(code: String, message: String, asked: Int) -> Int? {
        guard asked > HistoryLimits.legacyMaximumPageLines, code == "cli_error" || code == "invalid_request" else { return nil }
        let text = message.lowercased()
        guard text.contains("--lines") || text.contains("lines must") else { return nil }
        for marker in ["from 1 to ", "1..="] {
            guard let range = text.range(of: marker) else { continue }
            let digits = text[range.upperBound...].prefix(while: \.isNumber)
            if let limit = Int(digits), limit >= 1 { return min(limit, asked - 1) }
        }
        return HistoryLimits.legacyMaximumPageLines
    }

    // MARK: The path

    /// The network path changed. A restriction (Low Data Mode, a metered link) applies at once and is lifted only after the path has been
    /// free of it for a while. A settled change of the kind of link makes the speed measured on the old one worthless, but a moment
    /// without any link is not a different link.
    func linkChanged() {
        guard let watcher = linkWatcher else { return }
        let now = linkNow
        rawLinkConditions = watcher.conditions
        conditionHold.apply(rawLinkConditions, at: now)
        if watcher.interface != "none", watcher.interface != linkInterface {
            linkInterface = watcher.interface
            linkMeter.pathChanged()
        }
        scheduleConditionLapse()
        kickPrefetch()
    }
    /// When a restriction that is only being held lapses, the fetch is looked at again.
    private func scheduleConditionLapse() {
        conditionLapse?.cancel(); conditionLapse = nil
        guard let wait = conditionHold.lapse(after: linkNow, raw: rawLinkConditions) else { return }
        conditionLapse = Task { [weak self] in
            try? await Task.sleep(for: .seconds(wait + 0.05))
            guard !Task.isCancelled, let self else { return }
            self.kickPrefetch()
            self.scheduleConditionLapse()
        }
    }

    // MARK: Settings

    func setHistoryMode(_ mode: HistoryMode) {
        guard historyMode != mode else { return }
        historyMode = mode
        defaults.set(mode.rawValue, forKey: Self.historyModeKey)
        // A page in a pause the old mode asked for is looked at again under the new one.
        historySleeper?.cancel()
        kickPrefetch()
    }
    func setCompressTraffic(_ on: Bool) {
        guard compressTraffic != on else { return }
        compressTraffic = on
        defaults.set(on, forKey: Self.compressTrafficKey)
        // What a line weighs on the wire, and the pages that were too long for it, were learned under the other setting.
        linkMeter.forgetContent(); historyPageLines = [:]; historyCapStreak = [:]
        Task { [client] in
            await client.setCompression(on)
            let agreed = await client.compressionActive()
            self.compressionAgreed = agreed
        }
    }

    // MARK: What the settings screen shows

    /// Lines above the oldest one held that the phone would still fetch: the desktop's history, short of what the phone keeps.
    var remainingHistoryLines: Int {
        guard terminal.canPage, !terminal.atTop else { return terminal.missingLines }
        return max(0, min(terminal.start, HistoryLimits.heldLines - terminal.heldHistory)) + terminal.missingLines
    }
    /// What those lines weigh on the wire, from the lines fetched so far; nil before any.
    var remainingHistoryWireBytes: Double? { linkMeter.bytesPerLine.map { $0 * Double(remainingHistoryLines) } }

    var linkReadout: LinkReadout { LinkReadout(meter: linkMeter, conditions: linkConditions, interface: linkInterface, compression: compressionAgreed) }
    /// The lines of the latency overlay about the link: why the phone does what it does with history.
    var linkLines: [String] {
        let readout = linkReadout
        let held = terminal.heldHistory, total = terminal.historySize ?? 0
        let appetite: String
        switch historyPolicy.appetite { case .everything: appetite = "all"; case .screens(let n): appetite = "\(n) scr" }
        return [
            "link \(readout.roundTrip) · \(readout.rate) \(readout.tier)",
            "desk \(readout.desktop) · z \(readout.ratio)",
            "hist \(appetite) · \(held.formatted())/\(total.formatted())\(readout.conditions.isRestricted ? " ⚑" : "")",
        ]
    }

    /// What the history download is doing and why: the appetite under the mode and the measured link.
    var historyPolicy: (appetite: HistoryAppetite, reason: HistoryAppetite.Reason) {
        HistoryAppetite.policy(mode: historyMode, tier: linkMeter.tier, conditions: linkConditions, remainingWireBytes: remainingHistoryWireBytes)
    }
}

/// A line of text for the numbers the link meter holds, shared by the settings screen and the latency overlay.
struct LinkReadout {
    let meter: LinkMeter
    let conditions: LinkConditions
    let interface: String
    let compression: Bool

    var roundTrip: String { meter.roundTripSeconds.map { Self.milliseconds($0) } ?? "–" }
    var rate: String { meter.bytesPerSecond.map { Self.rate($0) } ?? "–" }
    var desktop: String { meter.desktopSeconds.map { Self.milliseconds($0) } ?? "–" }
    var tier: String { meter.tier.label }
    var ratio: String { compression ? (meter.compressionRatio.map { String(format: "%.1f×", $0) } ?? "on") : "off" }

    static func milliseconds(_ seconds: Double) -> String {
        let ms = seconds * 1000
        return ms < 10 ? String(format: "%.1f ms", ms) : "\(Int(ms.rounded())) ms"
    }
    static func rate(_ bytesPerSecond: Double) -> String {
        bytesPerSecond >= 1_000_000 ? String(format: "%.1f MB/s", bytesPerSecond / 1_000_000) : "\(Int((bytesPerSecond / 1000).rounded())) KB/s"
    }
    /// "Wi-Fi", "cellular", …
    var path: String {
        switch interface {
        case "wifi": "Wi-Fi"
        case "cellular": "cellular"
        case "wired": "wired"
        case "none": "offline"
        default: "tunnel or other"
        }
    }
    /// What restricts the path, in words ("metered, Low Data Mode"), or "none".
    var restrictions: String {
        var parts: [String] = []
        if conditions.expensive { parts.append("metered") }
        if conditions.constrained { parts.append("Low Data Mode") }
        if conditions.lowPower { parts.append("Low Power Mode") }
        return parts.isEmpty ? "none" : parts.joined(separator: ", ")
    }
}

extension HistoryAppetite {
    /// "the whole history", "10 screens ahead".
    var summary: String {
        switch self {
        case .everything: "the whole history"
        case .screens(let n): "\(n) screens ahead"
        }
    }
}
extension HistoryAppetite.Reason {
    var sentence: String {
        switch self {
        case .chosenEverything: "You chose everything."
        case .chosenAhead: "You chose a few screens ahead."
        case .chosenOff: "Background download is off: a page loads when you scroll near the top."
        case .lowData: "Low Data Mode is on."
        case .metered: "The connection is metered."
        case .lowPower: "Low Power Mode is on."
        case .slowLink: "The link is slow."
        case .cheap: "What is left is small enough to fetch whole."
        case .goodLink: "The link has the bandwidth."
        case .measuring: "Measuring the link."
        }
    }
}
extension HistoryMode {
    var title: String {
        switch self {
        case .automatic: "Automatic"
        case .everything: "Always everything"
        case .ahead: "Ahead only"
        case .off: "Off"
        }
    }
    var detail: String {
        switch self {
        case .automatic: "Everything when the link has the bandwidth and is not metered, a few screens ahead otherwise."
        case .everything: "The whole history in the background, whatever the link."
        case .ahead: "Keeps about ten screens loaded above where you are reading."
        case .off: "Nothing in the background. A page loads when you scroll near the top of what is loaded."
        }
    }
}
