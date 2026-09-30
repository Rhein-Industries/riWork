import Foundation

/// What one live `shell.output` answer puts into the buffer: the lines of the reply (up to `lines` of scrollback, then the screen
/// down to the cursor) and where the screen starts among them.
public struct LiveTail: Sendable, Equatable {
    public let lines: [StyledLine]
    /// How many of `lines` lie above the screen.
    public let historyLines: Int
    /// The desktop's `history_size` (scrollback lines above its screen), when it reports one.
    public let historySize: Int?
    /// The cursor as an index into `lines`, and a character index within that line.
    public let cursorLine: Int?
    public let cursorColumn: Int?

    public init(lines: [StyledLine], historyLines: Int, historySize: Int?, cursorLine: Int? = nil, cursorColumn: Int? = nil) {
        self.lines = lines
        self.historyLines = max(0, min(historyLines, lines.count))
        self.historySize = historySize
        self.cursorLine = cursorLine; self.cursorColumn = cursorColumn
    }
    public init(screen: StyledScreen, historySize: Int?) {
        self.init(lines: screen.lines, historyLines: screen.historyLines, historySize: historySize, cursorLine: screen.cursorLine, cursorColumn: screen.cursorColumn)
    }
}

/// What the next `shell.history` request should ask for, and which state of the buffer it was worked out from.
public struct HistoryFetch: Sendable, Equatable {
    /// Scrollback lines directly above the screen to skip: the lines already held above the screen, less `overlap`.
    public let end: Int
    /// Lines to ask for, `overlap` included.
    public let lines: Int
    /// Lines at the bottom of the page that are already held and only there to be compared.
    public let overlap: Int
    /// The buffer's `epoch` when this was worked out. A page for another epoch is thrown away.
    public let epoch: Int
    public init(end: Int, lines: Int, overlap: Int, epoch: Int) { self.end = end; self.lines = lines; self.overlap = overlap; self.epoch = epoch }
}

/// How a history page was taken in.
public enum HistoryMerge: Sendable, Equatable {
    /// `added` older lines now sit above what was held.
    case merged(added: Int)
    /// The buffer was rebuilt while the page was on its way; nothing was done.
    case stale
    /// The page held nothing new (the desktop's screen moved on past it); ask again.
    case retry
    /// The page did not line up with what is held (a gap, or different lines where they should match). The older history was
    /// dropped, to be fetched again from the live part.
    case inconsistent
}

/// What a live answer did to the buffer.
public struct LiveChange: Sendable, Equatable {
    /// How many lines the screen moved down the history (new output), or up (lines taken back from history).
    public var shift = 0
    /// Every absolute index was renumbered: the lines held could not be matched with the new answer.
    public var rebased = false
    /// Older lines could not be kept next to the new answer and were dropped.
    public var droppedOlder = false
}

/// The lines the phone shows for a shell: the latest scrollback and screen of the live answers, and older scrollback pages above.
///
/// Every line has a stable **absolute index** that does not change when lines are added above or below it: the first line held by the
/// first answer is numbered so that the screen's top row is `history_size` (the index counts from the top of the desktop's history);
/// lines that scroll into history keep their number, and numbers simply continue past the end. So a view that keys its rows by
/// absolute index does not move what is being read when older pages are prepended or new output is appended.
///
/// Consistency is kept by the `history_size` the desktop reports: when it grows by k between two answers, the screen has moved k
/// lines down the numbering. It is backed by comparing the lines the two answers share (their text), which also finds a shift
/// `history_size` cannot announce (the desktop's history is full and drops its oldest lines as new ones arrive, or the desktop
/// predates `history_size`). When the lines cannot be matched at all (the pane was re-wrapped by a resize, the history was cleared),
/// the buffer is rebuilt from the answer and `epoch` changes.
public struct TerminalBuffer: Sendable, Equatable {
    /// Bumped when absolute indexes stop meaning what they did: the first answer, and every rebuild.
    public private(set) var epoch = 0
    /// Absolute index of `lines[0]`.
    public private(set) var start = 0
    /// The lines held, scrollback first, then the screen down to its last shown row. Consecutive absolute indexes from `start`.
    public private(set) var lines: [StyledLine] = []
    /// Absolute index of the screen's top row: lines before it are scrollback.
    public private(set) var screenTop = 0
    /// Absolute index of the first line of the latest live answer. Lines before it came from history pages.
    public private(set) var liveStart = 0
    /// The desktop's `history_size` as of the latest live answer. Nil: the desktop does not report it, so it cannot page history.
    public private(set) var historySize: Int?
    /// Everything above `start` is known not to exist.
    public private(set) var atTop = false
    /// A shift was found that `history_size` did not announce: the desktop's history is full and drops its oldest lines as new ones
    /// arrive. Pages then overlap what is held, so the seam can be checked. It stays set for the shell: a full history stays full.
    public private(set) var drifting = false
    /// Where the cursor is, as an absolute index and a character index within that line.
    public private(set) var cursorIndex: Int?
    public private(set) var cursorColumn: Int?
    private var filled = false

    public init() {}

    public var isEmpty: Bool { lines.isEmpty }
    /// One past the absolute index of the last line.
    public var endIndex: Int { start + lines.count }
    public var indices: Range<Int> { start..<endIndex }
    /// Scrollback lines held above the screen.
    public var heldHistory: Int { max(0, min(screenTop, endIndex) - start) }
    /// The desktop can be asked for older scrollback.
    public var canPage: Bool { filled && historySize != nil }
    /// The phone holds as much scrollback as it keeps.
    public var limitReached: Bool { heldHistory >= HistoryLimits.heldLines }
    public subscript(index: Int) -> StyledLine? {
        guard index >= start, index < endIndex else { return nil }
        return lines[index - start]
    }

    // MARK: Live answers

    /// Takes in a live answer: works out how far the screen moved since the last one, keeps the older lines next to it when they
    /// still match, and replaces everything from the answer's first line on. `allowTrim` false leaves the oldest lines alone even
    /// when over the cap (the view is being scrolled and the lines above it must not move).
    @discardableResult
    public mutating func applyLive(_ tail: LiveTail, allowTrim: Bool = true) -> LiveChange {
        var change = LiveChange()
        guard filled else { rebase(tail); change.rebased = true; return change }
        let n = tail.historyLines
        let oldTop = screenTop
        var newTop = oldTop
        var resolved = false
        if let size = tail.historySize, let previous = historySize {
            let candidate = oldTop + (size - previous)
            if matches(tail, top: candidate) { newTop = candidate; resolved = true }
        }
        if !resolved, let shift = alignedShift(tail) {
            newTop = oldTop + shift
            resolved = true
            // The desktop said the history did not change (or grew by another amount) but the lines moved: its history is full.
            if tail.historySize != nil { drifting = true }
        }
        guard resolved else { rebase(tail); change.rebased = true; return change }
        change.shift = newTop - oldTop

        let tailStart = newTop - n
        let keepEnd = min(tailStart, oldTop)
        var dropped = false
        if keepEnd < tailStart {
            // A gap between the scrollback held and the new answer: more lines scrolled past than the answer reaches back.
            dropped = heldHistory > 0
            lines.removeAll(keepingCapacity: true)
            start = tailStart
        } else if keepEnd <= start {
            lines.removeAll(keepingCapacity: true)
            start = tailStart
        } else {
            lines.removeSubrange((keepEnd - start)...)
        }
        lines.append(contentsOf: tail.lines)
        screenTop = newTop
        liveStart = tailStart
        cursorIndex = tail.cursorLine.map { tailStart + $0 }
        cursorColumn = tail.cursorLine == nil ? nil : tail.cursorColumn
        historySize = tail.historySize
        if dropped { atTop = false; change.droppedOlder = true }
        if allowTrim, trimToCap() { atTop = false }
        refreshAtTop()
        return change
    }

    /// Drops the oldest lines past the cap. Returns whether any went.
    private mutating func trimToCap() -> Bool {
        let excess = heldHistory - HistoryLimits.heldLines
        guard excess > 0 else { return false }
        lines.removeFirst(excess)
        start += excess
        return true
    }

    /// Whether the lines held reach the oldest line the desktop has.
    private mutating func refreshAtTop() {
        guard let size = historySize else { atTop = false; return }
        if start <= screenTop - size { atTop = true }
    }

    /// Starts over from this answer: new numbering, nothing older.
    private mutating func rebase(_ tail: LiveTail) {
        epoch &+= 1
        filled = true
        let n = tail.historyLines
        // The screen's top row is numbered `history_size` (the count from the top of the desktop's history), or `n` when it is not reported.
        let top = max(tail.historySize ?? n, n)
        screenTop = top
        start = top - n
        liveStart = start
        lines = tail.lines
        historySize = tail.historySize
        cursorIndex = tail.cursorLine.map { start + $0 }
        cursorColumn = tail.cursorLine == nil ? nil : tail.cursorColumn
        atTop = false
        refreshAtTop()
    }

    /// Whether the scrollback of `tail`, numbered as if the screen's top row were `top`, agrees with the scrollback held where they
    /// overlap. Only scrollback is compared: the screen rows held are not final.
    ///
    /// `strict` is for a shift that is being guessed rather than announced: then the overlap must be long enough (8 lines, or all of
    /// the answer's scrollback when that is shorter) and hold some text, since a few blank lines agree by chance.
    private func matches(_ tail: LiveTail, top: Int, strict: Bool = false) -> Bool {
        let tailStart = top - tail.historyLines
        let low = max(start, tailStart)
        let high = min(top, screenTop)
        if strict, high - low < min(tail.historyLines, 8) { return false }
        var sawText = false
        var index = low
        while index < high {
            let held = lines[index - start]
            if !held.sameText(as: tail.lines[index - tailStart]) { return false }
            if !held.text.isEmpty { sawText = true }
            index += 1
        }
        return !strict || sawText
    }

    /// The smallest shift of the screen (forward first, then back) under which the answer's scrollback agrees with the scrollback
    /// held, or nil. No scrollback on either side leaves nothing to compare: no shift.
    private func alignedShift(_ tail: LiveTail) -> Int? {
        let n = tail.historyLines
        guard n > 0, heldHistory > 0 else { return 0 }
        for shift in 0..<n where matches(tail, top: screenTop + shift, strict: true) { return shift }
        for shift in 1..<n where screenTop - shift > start && matches(tail, top: screenTop - shift, strict: true) { return -shift }
        return nil
    }

    // MARK: Older pages

    /// The next page to ask the desktop for, or nil when there is none to ask for: the desktop does not page, everything is held,
    /// or the cap is reached. `pageLines` is the number of new lines wanted.
    public func nextFetch(pageLines: Int = HistoryLimits.pageLines) -> HistoryFetch? {
        guard canPage, !atTop else { return nil }
        let held = heldHistory
        let room = HistoryLimits.heldLines - held
        let fresh = min(pageLines, room)
        guard fresh > 0 else { return nil }
        // Once the desktop's history is full (seen dropping lines, or as large as its limit) `history_size` no longer says how far the
        // screen has moved, so pages overlap what is held and the seam is checked by comparing lines.
        let full = drifting || (historySize ?? 0) >= HistoryLimits.desktopHistoryLines
        let overlap = full ? min(HistoryLimits.verifyLines, held) : 0
        return HistoryFetch(end: held - overlap, lines: min(HistoryLimits.maximumPageLines, fresh + overlap), overlap: overlap, epoch: epoch)
    }

    /// Puts a page above the scrollback held.
    ///
    /// `reported` is the `history_size` the page came with. The page was taken from the desktop's screen as it was then, which may
    /// be further down the history than the phone's last live answer: the page is placed by that difference, and the lines it shares
    /// with what is held are checked and left out.
    @discardableResult
    public mutating func merge(page: [StyledLine], historySize reported: Int?, complete: Bool, for fetch: HistoryFetch) -> HistoryMerge {
        guard filled, fetch.epoch == epoch else { return .stale }
        var top = screenTop
        if let reported, let known = historySize { top += reported - known }
        let bottom = top - fetch.end
        let count = page.count
        // A page that ends above what is held leaves lines out between them: a gap. (An empty page claims that nothing exists above
        // its bottom, which is the same thing when lines are held above that.)
        guard bottom >= start else { dropOlder(); return .inconsistent }
        if count == 0 {
            if complete { atTop = true }
            return .merged(added: 0)
        }
        let pageStart = bottom - count
        // Page line j is numbered pageStart + j. The ones below `start` are new; the ones that are held scrollback too must agree.
        var number = max(pageStart, start)
        while number < min(bottom, screenTop) {
            if !lines[number - start].sameText(as: page[number - pageStart]) { dropOlder(); return .inconsistent }
            number += 1
        }
        let added = min(count, max(0, start - pageStart))
        guard added > 0 else { return .retry }
        lines.insert(contentsOf: page[0..<added], at: 0)
        start -= added
        if complete { atTop = true }
        return .merged(added: added)
    }

    /// Forgets the pages above the latest live answer; they are fetched again from there.
    public mutating func dropOlder() {
        let drop = liveStart - start
        if drop > 0 {
            lines.removeFirst(drop)
            start = liveStart
        }
        atTop = false
        refreshAtTop()
    }

    /// Back to nothing, as for another shell.
    public mutating func reset() {
        let next = epoch &+ 1
        self = TerminalBuffer()
        epoch = next
    }
}
