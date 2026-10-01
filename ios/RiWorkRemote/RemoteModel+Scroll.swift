import Foundation
import UIKit
import RiWorkCore

/// Whether the connected desktop answers `shell.history`, learned from the first request.
enum HistorySupport: Equatable { case unknown, supported, unsupported }

// Scrolling. The live answers and the older pages of scrollback are one list of lines (`terminal`), each under an absolute index that
// never changes while lines are added above or below it. The surface draws line `i` at `i * lineHeight`, so neither a page at the top
// nor output at the bottom moves what is being read, and a page is put in the moment it arrives.
//
// Older pages are a second step, after the live screen: once the first answer is in, the phone fetches history in the background,
// one `shell.history` request at a time, so that scrolling is local (no round trip per scroll). `HistoryPrefetch` (RiWorkCore) decides
// before every page whether to fetch, how many lines, and whether to wait; this file runs the loop and keeps what it needs to know:
//
// - how fast the link is: every page is timed (`linkMeter`); the path says Low Data Mode, a metered link, Low Power Mode;
// - where the reader is: the surface reports the first line in view (`noteReader`);
// - whether keys are going out (typing) and whether the live long poll and cancelled ones hold the desktop's shared slots;
// - whether the history is being drawn again (the pane was just resized, or it shrank).
//
// A desktop that does not know `shell.history` (or does not report `history_size`) keeps today's behaviour: the screen and the
// latest 500 lines, no paging. Parsing runs off the main actor.
extension RemoteModel: TerminalLineSource {
    var terminalBuffer: TerminalBuffer { terminal }

    // MARK: Live answers

    /// A live answer arrived: it goes into the list of lines, unless a full-screen program is showing (that screen has no
    /// scrollback, and the list of the normal screen stays as it was for when the program ends).
    func takeIn(screen: ShellOutput, styled: StyledScreen) {
        setAlternateScreen(screen.alternate ?? false)
        guard !alternateScreen else { return }
        if terminal.isEmpty { restoreTerminal() }
        let wasFilled = !terminal.isEmpty
        let era = terminal.era
        terminal.applyLive(LiveTail(screen: styled, historySize: screen.historySize), allowTrim: trimAllowed)
        // Lines renumbered, or the history got shorter: a program drew it again (or it was cleared). A page on its way is of the old
        // one; the new one is left alone until it has settled.
        if wasFilled, terminal.era != era { historyRedrawn(after: HistoryPrefetch.settleAfterShrinkSeconds) }
        scrollFollow.contentChanged(end: terminal.endIndex, epoch: terminal.epoch)
        surface?.refresh()
        kickPrefetch()
    }

    /// The oldest lines are only dropped past the cap when the reader is not looking at them (and then only so far).
    private var trimAllowed: Bool {
        guard let top = readerTop else { return true }
        let nearStart = top - terminal.start < 3 * readerRows
        return !nearStart || terminal.heldHistory > HistoryLimits.heldLines + HistoryLimits.trimDeferralLines
    }

    func setAlternateScreen(_ on: Bool) {
        guard alternateScreen != on else { return }
        alternateScreen = on
        // Into a program or out of it, the view starts over at the bottom. Its scroll view goes away.
        scrollFollow.reset()
        if on {
            defaults.set(defaults.integer(forKey: Self.alternateEntriesKey) + 1, forKey: Self.alternateEntriesKey)
            // A page for the normal screen is of no use while a program has the other one.
            cancelHistory()
        } else {
            kickPrefetch()
        }
    }
    /// The hint "Scrolling the app" is shown at full strength for the first few full-screen programs (counted across launches), then
    /// only faintly.
    static let loudAlternateHints = 3
    static let alternateEntriesKey = "riwork.alternateEntries"
    var alternateHintLoud: Bool { defaults.integer(forKey: Self.alternateEntriesKey) <= Self.loudAlternateHints }

    func resetTerminal() {
        terminal.reset()
        scrollFollow.reset()
        alternateScreen = false
        cancelHistory()
        historyFailed = false
        readerTop = nil; lastKickTop = Int.min
        settleUntil = nil; historyDemand = false; historyMisses = 0; lastHistoryAnswerAt = nil
        // A pause that another shell's trouble earned is not this shell's.
        historyRetryAfter = nil
    }

    // MARK: Shells that are not on screen

    private var cacheKey: String? {
        guard let desktopID = selectedDesktopID, let id = outputSessionID else { return nil }
        return "\(desktopID)/\(id)"
    }
    /// Keeps the lines of the shell being left, so that coming back to it does not fetch them again.
    func stashTerminal() {
        guard !terminal.isEmpty, let key = cacheKey else { return }
        terminalCache.store(terminal, key: key, columns: terminalViewport?.columns)
    }
    /// Takes back the lines kept for the shell now being shown. The first live answer lines them up with the desktop's (by
    /// `history_size` and by comparing text) and starts over if they do not fit, so a shell that was cleared or drawn again while it
    /// was away shows none of its old lines. Lines wrapped at another width were not kept.
    private func restoreTerminal() {
        guard let desktopID = selectedDesktopID, let id = sessionID, let cached = terminalCache.take(key: "\(desktopID)/\(id)", columns: terminalViewport?.columns) else { return }
        terminal = cached
    }

    // MARK: Live answer size

    /// The iPhone scrolls through `terminal`; the iPad keeps the two-axis screen it has always had, which is the latest answer alone
    /// (so it neither fetches history nor asks for fewer lines).
    var scrollsThroughBuffer: Bool { UIDevice.current.userInterfaceIdiom != .pad }

    /// The `lines` of scrollback a live answer asks for. Until older lines are known to come from `shell.history`, and whenever the
    /// desktop's history is full, it is the whole 500; after that 120 are enough to join one answer to the next.
    var liveScrollbackLines: Int {
        HistoryLimits.liveLines(pagingProved: scrollsThroughBuffer && historySupport == .supported && terminal.canPage,
                                fullAgain: terminal.drifting || (terminal.historySize ?? 0) >= HistoryLimits.desktopHistoryLines)
    }

    // MARK: Where the reader is

    func attach(surface: TerminalSurfaceView) { self.surface = surface }

    /// The surface reports the first line in view and how many rows fit. Cheap: it is called for every frame of a scroll.
    func noteReader(top: Int, rows: Int) {
        readerTop = top; readerRows = rows
        // Looking again at whether to fetch is only worth it when the reader moved half a screen.
        guard lastKickTop == Int.min || abs(top - lastKickTop) >= max(1, rows / 2) else { return }
        lastKickTop = top
        // A reader who has come close to the top does not wait out the pause between two background pages.
        if historyTask != nil, Double(top - terminal.start) < HistoryPrefetch.urgentScreens * Double(rows) { historySleeper?.cancel() }
        kickPrefetch()
    }

    // MARK: History

    /// Whether the header of the list has anything to say: paging is possible on this desktop.
    var historyPaging: Bool { terminal.canPage && historySupport != .unsupported }

    /// What the header row at the top of the loaded lines says, or nil when there is no such row.
    var historyHeader: HistoryHeader? {
        guard historyPaging, terminal.heldHistory > 0 else { return nil }
        if historyFailed { return HistoryHeader(text: "Couldn't load older lines · tap to retry", failed: true) }
        if terminal.atTop { return HistoryHeader(text: "Beginning of history", failed: false) }
        if terminal.limitReached { return HistoryHeader(text: "Showing the last \(HistoryLimits.heldLines.formatted()) lines", failed: false) }
        return HistoryHeader(text: historyLoading ? "Loading…" : " ", failed: false)
    }

    /// Whether history can be fetched at all right now: a live connection to a shell that is showing, on the normal screen.
    private var canFetchHistory: Bool {
        guard scrollsThroughBuffer, state == .connected, terminalVisible, appActive, !alternateScreen, historySupport != .unsupported, let id = sessionID,
              outputSessionID == id, !missingSessionIDs.contains(id) else { return false }
        if let after = historyRetryAfter, ContinuousClock.now < after { return false }
        return true
    }

    /// Fetches one older page now, whatever the link or the reader's place (the retry row, the manual call). Cheap to call often.
    func loadOlderHistory() {
        historyDemand = true
        startHistory()
        // Asked for, but nothing could start (the terminal is hidden, nothing to fetch): the ask is not kept for later.
        if historyTask == nil { historyDemand = false }
    }
    /// Looks at whether history should be fetched, and starts the loop if so. Cheap to call often: it is called with every live answer
    /// and every half screen of scrolling.
    func kickPrefetch() {
        guard prefetchEnabled else { return }
        startHistory()
    }
    private func startHistory() {
        guard historyTask == nil, canFetchHistory, let id = sessionID else { return }
        // Not worth a task when there is nothing to do; a wait (typing, a history being drawn again) is.
        if case .idle = prefetchDecision(id: id, running: false, sinceLastPage: nil) { return }
        startHistoryTask(id: id)
    }
    private func startHistoryTask(id: String) {
        historyLoading = true; historyFailed = false
        let token = generation
        let run = UUID()
        historyRun = run
        historyTask = Task { [weak self] in await self?.runHistory(id: id, token: token, run: run) }
    }
    /// The "couldn't load" row was tapped.
    func retryHistory() {
        historyRetryAfter = nil; historyMisses = 0; historyFailed = false
        loadOlderHistory()
    }
    func cancelHistory() {
        historyTask?.cancel(); historyTask = nil
        historySleeper?.cancel(); historySleeper = nil
        historyRun = UUID()
        historyLoading = false
    }

    /// Keys are going out, or were a moment ago.
    private var typingActive: Bool {
        if keySender != nil || sending || hasPendingKeys(forDesktop: selectedDesktopID) { return true }
        guard let last = lastKeyActivity else { return false }
        return Date().timeIntervalSince(last) < HistoryPrefetch.typingQuietSeconds
    }

    /// With a lookahead only (a poor or restricted link) a hole is wanted when the reader is near it, not for its own sake: a flood of
    /// output while the phone is watching can leave tens of thousands of lines missing.
    private var wantsHoles: Bool {
        guard prefetchEnabled, !historyDemand else { return true }
        guard case .screens(let screens) = HistoryAppetite.appetite(tier: linkMeter.tier, conditions: linkConditions) else { return true }
        return terminal.holeNear(top: readerTop ?? terminal.screenTop, rows: readerRows, screens: screens)
    }

    private func prefetchDecision(id: String, running: Bool, sinceLastPage: Double?) -> HistoryPrefetch.Decision {
        let now = ContinuousClock.now
        let holes = wantsHoles
        var input = HistoryPrefetch.Input()
        input.canFetch = terminal.nextFetch(pageLines: 1, fillHoles: holes) != nil
        input.aboveReader = max(0, (readerTop ?? terminal.screenTop) - terminal.start)
        input.viewRows = readerRows
        input.missing = holes ? terminal.missingLines : 0
        // With the background fetch off, a page asked for is the default size: the link is not adapted to.
        input.meter = prefetchEnabled ? linkMeter : LinkMeter()
        input.conditions = linkConditions
        input.typing = typingActive
        input.liveBusy = (latency.age(at: ProcessInfo.processInfo.systemUptime) ?? .infinity) < 0.5
        input.waitsHeld = (outputFlightIsLongPoll ? 1 : 0) + waitSlots.stillWaiting(at: ProcessInfo.processInfo.systemUptime)
        input.sinceLastPage = sinceLastPage
        input.running = running
        input.demand = historyDemand
        input.pageCap = historyPageLines[id]
        if let settleUntil { input.settleRemaining = max(0, (settleUntil - now).timeInterval) }
        return HistoryPrefetch.decide(input)
    }

    // MARK: The pane was resized, or its history drawn again

    /// `shell.resize` took effect. The inline agents (Codex, Grok, Claude Code without the alternate screen) wipe their scrollback and
    /// draw the transcript again at the new width, and are done about half a second later.
    func viewportWasApplied() { historyRedrawn(after: HistoryPrefetch.settleAfterResizeSeconds) }
    /// A page taken before is of a history that is about to be replaced: forget it, wait, and start again from the live screen.
    private func historyRedrawn(after seconds: Double) {
        settleUntil = .now + .seconds(seconds * settleScale)
        let wasFetching = historyTask != nil
        cancelHistory()
        if wasFetching { kickPrefetch() }
    }

    /// The network path changed. A change of the kind of link makes what was measured of the old one worthless.
    func linkChanged() {
        guard let watcher = linkWatcher else { return }
        linkConditions = watcher.conditions
        if watcher.interface != linkInterface { linkInterface = watcher.interface; linkMeter.reset() }
        kickPrefetch()
    }

    // MARK: The loop

    private func runHistory(id: String, token: UUID, run: UUID) async {
        func current() -> Bool {
            generation == token && historyRun == run && sessionID == id && state == .connected && !alternateScreen && !Task.isCancelled
        }
        defer {
            if historyRun == run { historyTask = nil; historyLoading = false }
        }
        var retries = 0
        while current() {
            if !prefetchEnabled && !historyDemand { return }
            if let after = historyRetryAfter, ContinuousClock.now < after { return }
            guard terminalVisible, appActive else { return }
            let since = lastHistoryAnswerAt.map { (ContinuousClock.now - $0).timeInterval }
            switch prefetchDecision(id: id, running: true, sinceLastPage: since) {
            case .idle:
                return
            case .wait(let seconds):
                let sleeper = Task<Void, Never> { _ = try? await Task.sleep(for: .seconds(seconds)) }
                historySleeper = sleeper
                await withTaskCancellationHandler { await sleeper.value } onCancel: { sleeper.cancel() }
                if historySleeper == sleeper { historySleeper = nil }
                continue
            case .fetch(let lines, let urgent):
                switch await fetchPage(id: id, lines: lines, urgent: urgent, token: token, run: run) {
                case .landed:
                    retries = 0; historyMisses = 0
                    historyDemand = false
                case .again:
                    // The page did not take (the screen moved past it, or it did not line up): ask again from where things are now,
                    // a few times, and then leave it for a while. Nothing is shown: this is the history being redrawn, not an error.
                    retries += 1
                    if retries > 3 { missHistory(); return }
                case .adjusted:
                    continue
                case .stop:
                    return
                }
            }
        }
    }

    /// `landed`: a page went in. `again`: it did not take; plan again. `adjusted`: the request itself was changed (fewer lines, no
    /// `styled`); plan again without counting a miss. `stop`: this run is over.
    private enum PageOutcome { case landed, again, adjusted, stop }

    /// Silent backoff for pages that keep missing: 2 s, 4 s, … up to a minute.
    private func missHistory() {
        historyMisses += 1
        historyDemand = false
        historyRetryAfter = ContinuousClock.now + .seconds(min(60, 1 << min(historyMisses, 6)))
    }

    private func fetchPage(id: String, lines pageLines: Int, urgent: Bool, token: UUID, run: UUID) async -> PageOutcome {
        func current() -> Bool { generation == token && historyRun == run && sessionID == id && state == .connected && !alternateScreen && !Task.isCancelled }
        guard let fetch = terminal.nextFetch(pageLines: pageLines, fillHoles: wantsHoles) else { return .stop }
        let request = HistoryRequest(shellID: id, end: fetch.end, lines: fetch.lines, styled: historyStyled)
        let raw: JSONValue
        let started = ContinuousClock.now
        do {
            raw = try await client.request(method: "shell.history", params: request.params, id: UUID().uuidString.lowercased())
        } catch {
            guard current() else { return .stop }
            if RemoteError.isUnsupportedMethod(error) { historySupport = .unsupported; return .stop }
            if case RemoteError.rpc(let code, let message) = error {
                if code == "response_too_large", (historyPageLines[id] ?? pageLines) > HistoryLimits.minimumPageLines {
                    // Half as many lines, and the same for the next pages of this session.
                    historyPageLines[id] = max(HistoryLimits.minimumPageLines, min(historyPageLines[id] ?? pageLines, pageLines) / 2)
                    return .adjusted
                }
                if code == "not_found" {
                    // The output poll learns that the session is gone; until then, not one request per scroll step.
                    historyRetryAfter = ContinuousClock.now + .seconds(3)
                    return .stop
                }
                if historyStyled, Self.rejectsStyled(code: code, message: message) {
                    // Retry once without the newer field, like `shell.output`.
                    historyStyled = false
                    return .adjusted
                }
            }
            failHistory()
            return .stop
        }
        let elapsed = (ContinuousClock.now - started).timeInterval
        guard current() else { return .stop }
        let reply: HistoryReply
        do { reply = try HistoryReply(result: raw) } catch { failHistory(); return .stop }
        guard reply.shellID == id else { failHistory(); return .stop }
        historySupport = .supported
        let page = await Task.detached(priority: urgent ? .userInitiated : .utility) { TerminalText.styledLines(page: reply.text, expecting: reply.lineCount) }.value
        guard current() else { return .stop }
        // A page that does not hold the lines the desktop counted cannot be placed by its end.
        if let count = reply.lineCount, count != page.count { failHistory(); return .stop }
        linkMeter.record(wireBytes: reply.wireBytes, lines: max(1, page.count), seconds: elapsed)
        lastHistoryAnswerAt = .now
        let outcome = terminal.merge(page: page, historySize: reply.historySize, complete: reply.complete, for: fetch)
        surface?.refresh()
        switch outcome {
        case .merged(let added):
            // A page with nothing in it that does not end the history would be asked for again at once, forever.
            if added == 0, !terminal.atTop { failHistory(); return .stop }
            return .landed
        case .stale:
            // The numbering changed under the request (the history was drawn again): the next plan starts from the live screen.
            return .again
        case .retry, .inconsistent:
            return .again
        }
    }

    /// Whether an error to `shell.history` with `styled` says the desktop does not know that field: `invalid_request` naming it (or an
    /// unknown field), or a connector with an older CLI saying it cannot do styled output. Any other `invalid_request` is about the
    /// request itself, and asking without `styled` would not help.
    static func rejectsStyled(code: String, message: String) -> Bool {
        let text = message.lowercased()
        switch code {
        case "invalid_request": return text.contains("styled") || text.contains("unknown field") || text.contains("unknown parameter")
        case "cli_error": return text.contains("styled")
        default: return false
        }
    }
    private func failHistory() {
        historyFailed = true; historyDemand = false
        historyRetryAfter = ContinuousClock.now + .seconds(3)
    }

    // MARK: Jumping to the latest output

    /// Follow again and scroll to the bottom (the menu command, the pill, sending a line). Typing does this by itself.
    func jumpToLatest() {
        scrollFollow.jumpToBottom()
        jumpRequests &+= 1
    }
    /// The view reports where it is scrolled to. Returns `scrollToBottom` when it was following and the bottom moved away from it.
    func scrollMetricsChanged(from old: ScrollMetrics?, to new: ScrollMetrics, lineHeight: Double, userDriven: Bool) -> StickyBottom.Response {
        var next = scrollFollow
        let response = next.metricsChanged(from: old, to: new, lineHeight: lineHeight, userDriven: userDriven)
        if next != scrollFollow { scrollFollow = next }
        return response
    }

    // MARK: Copying

    /// The text "Copy screen text" puts on the pasteboard: the screen and the last 500 lines above it, as the person would have typed
    /// them (without the U+FE0E that makes symbols draw as text).
    var screenTextForCopy: String {
        let text = alternateScreen || terminal.isEmpty ? output : terminal.plainText(scrollbackLines: HistoryLimits.fullScrollbackLines)
        return text.replacingOccurrences(of: "\u{FE0E}", with: "")
    }

    // MARK: The alternate screen

    /// The screen of a full-screen program: the last rows of the latest answer, without any scrollback that came with it.
    var alternateLines: ArraySlice<StyledLine> { styledOutput.lines.dropFirst(styledOutput.historyLines) }
}
