import Foundation
import RiWorkCore

/// Whether the connected desktop answers `shell.history`, learned from the first request.
enum HistorySupport: Equatable { case unknown, supported, unsupported }

// Scrolling: the live answers and the older pages of scrollback are kept as one list of lines (`terminal`), each under an absolute
// index that never changes while lines are added above or below it. The view anchors on those indexes, so neither a page prepended
// at the top nor output appended at the bottom moves what is being read.
//
// Older pages come from `shell.history`, one request at a time, only when the view is within a screen of the top of what is loaded.
// The live long poll goes on meanwhile and typing is not touched: the request is a task of its own. Parsing runs off the main actor.
// A desktop that does not know `shell.history` (or does not report `history_size`) keeps today's behaviour: the latest `lines` of
// scrollback and the screen, no paging.
extension RemoteModel {
    // MARK: Live answers

    /// A live answer arrived: it goes into the list of lines, unless a full-screen program is showing (that screen has no
    /// scrollback, and the list of the normal screen stays as it was for when the program ends).
    func takeIn(screen: ShellOutput, styled: StyledScreen) {
        setAlternateScreen(screen.alternate ?? false)
        guard !alternateScreen else { return }
        // Lines that would go from the top of the list while the view is being scrolled wait, so nothing above the reader moves.
        terminal.applyLive(LiveTail(screen: styled, historySize: screen.historySize), allowTrim: !scrollBusy)
        scrollFollow.contentChanged(end: terminal.endIndex, epoch: terminal.epoch)
    }

    func setAlternateScreen(_ on: Bool) {
        guard alternateScreen != on else { return }
        alternateScreen = on
        // Into a program or out of it, the view starts over at the bottom. Its scroll view goes away, and with it any word that a
        // finger or its momentum has ended.
        scrollFollow.reset()
        scrollBusy = false
        if on {
            defaults.set(defaults.integer(forKey: Self.alternateEntriesKey) + 1, forKey: Self.alternateEntriesKey)
            // A page for the normal screen is of no use while a program has the other one.
            cancelHistory()
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
        scrollBusy = false
        alternateScreen = false
        cancelHistory()
        historyFailed = false
    }

    // MARK: Older pages

    /// Whether the header of the list has anything to say: paging is possible on this desktop.
    var historyPaging: Bool { terminal.canPage && historySupport != .unsupported }

    /// Fetches the next older page unless one is on its way, everything is loaded, or the desktop cannot. Cheap to call often.
    func loadOlderHistory() {
        guard historyTask == nil, state == .connected, !alternateScreen, historySupport != .unsupported, let id = sessionID,
              outputSessionID == id, !missingSessionIDs.contains(id) else { return }
        if let after = historyRetryAfter, ContinuousClock.now < after { return }
        guard terminal.nextFetch(pageLines: historyPageLines[id] ?? HistoryLimits.pageLines) != nil else { return }
        historyLoading = true; historyFailed = false
        let token = generation
        let run = UUID()
        historyRun = run
        historyTask = Task { [weak self] in await self?.runHistory(id: id, token: token, run: run) }
    }
    /// The "couldn't load" row was tapped.
    func retryHistory() {
        historyRetryAfter = nil
        loadOlderHistory()
    }
    func cancelHistory() {
        historyTask?.cancel(); historyTask = nil
        historyRun = UUID()
        historyLoading = false
    }
    /// The view is (not) being moved by a finger or its momentum. A page that arrived meanwhile is put in when it stops.
    func setScrollBusy(_ busy: Bool) { scrollBusy = busy }

    private func runHistory(id: String, token: UUID, run: UUID) async {
        func current() -> Bool { generation == token && historyRun == run && sessionID == id && state == .connected && !alternateScreen && !Task.isCancelled }
        defer {
            if historyRun == run { historyTask = nil; historyLoading = false }
        }
        var retries = 0, inconsistencies = 0
        while current() {
            var pageLines = historyPageLines[id] ?? HistoryLimits.pageLines
            guard let fetch = terminal.nextFetch(pageLines: pageLines) else { return }
            let request = HistoryRequest(shellID: id, end: fetch.end, lines: fetch.lines, styled: historyStyled)
            let raw: JSONValue
            do {
                raw = try await client.request(method: "shell.history", params: request.params, id: UUID().uuidString.lowercased())
            } catch {
                guard current() else { return }
                if RemoteError.isUnsupportedMethod(error) { historySupport = .unsupported; return }
                if case RemoteError.rpc(let code, let message) = error {
                    if code == "response_too_large", pageLines > HistoryLimits.minimumPageLines {
                        // Half as many lines, and the same for the next pages of this session.
                        pageLines = max(HistoryLimits.minimumPageLines, pageLines / 2)
                        historyPageLines[id] = pageLines
                        continue
                    }
                    if code == "not_found" {
                        // The output poll learns that the session is gone; until then, not one request per scroll step.
                        historyRetryAfter = ContinuousClock.now + .seconds(3)
                        return
                    }
                    if historyStyled, Self.rejectsStyled(code: code, message: message) {
                        // Retry once without the newer field, like `shell.output`.
                        historyStyled = false
                        continue
                    }
                }
                failHistory()
                return
            }
            guard current() else { return }
            let reply: HistoryReply
            do { reply = try HistoryReply(result: raw) } catch { failHistory(); return }
            guard reply.shellID == id else { failHistory(); return }
            historySupport = .supported
            let lines = await Task.detached(priority: .userInitiated) { TerminalText.styledLines(page: reply.text) }.value
            // A page that does not hold the lines the desktop counted cannot be placed by its end.
            if let count = reply.lineCount, count != lines.count { failHistory(); return }
            // The list is not touched under a moving finger: a page prepended then would move what it is holding.
            while scrollBusy, current() { try? await Task.sleep(for: .milliseconds(40)) }
            guard current() else { return }
            switch terminal.merge(page: lines, historySize: reply.historySize, complete: reply.complete, for: fetch) {
            case .merged(let added):
                // A page with nothing in it that does not end the history would be asked for again at once, forever.
                if added == 0, !terminal.atTop { failHistory() }
                return
            case .retry:
                // The desktop's screen moved on past the page; the next fetch is worked out from where it is now.
                retries += 1
                if retries > 3 { failHistory(); return }
            case .inconsistent:
                // The older lines were dropped; asking again starts from the live answer.
                inconsistencies += 1
                if inconsistencies >= 3 { failHistory(); return }
            case .stale:
                return
            }
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
        historyFailed = true
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

    // MARK: The alternate screen

    /// The screen of a full-screen program: the last rows of the latest answer, without any scrollback that came with it.
    var alternateLines: ArraySlice<StyledLine> { styledOutput.lines.dropFirst(styledOutput.historyLines) }
}
