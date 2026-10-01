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
    /// The buffer's `era` when this was worked out. A page for another era is thrown away.
    public let era: Int
    /// The page is for lines that scrolled by unseen between two live answers (a hole), not for the lines above the oldest held.
    public let fillsHole: Bool
    /// The desktop's `history_size` as of the last live answer when this was worked out. A page that reports less was taken from a
    /// history that has shrunk since; one that reports more (or the same) is placed by the difference to the latest answer.
    public let historySize: Int?
    public init(end: Int, lines: Int, overlap: Int, epoch: Int, era: Int = 0, fillsHole: Bool = false, historySize: Int? = nil) {
        self.end = end; self.lines = lines; self.overlap = overlap; self.epoch = epoch; self.era = era; self.fillsHole = fillsHole
        self.historySize = historySize
    }
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
    /// More lines scrolled by between two answers than the answer reaches back: the older lines were kept, and this many lines between
    /// them and the answer are missing, to be fetched as history.
    public var holeLines = 0
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
///
/// When more lines scroll by between two answers than an answer reaches back, the lines in between are missing. The older lines are
/// not thrown away for that (they may be 50,000 lines of prefetched history): the gap is held as blank placeholder lines under their
/// own indexes (`holes`), so nothing moves, and `nextFetch` asks for them first.
public struct TerminalBuffer: Sendable, Equatable {
    /// Bumped when absolute indexes stop meaning what they did: the first answer, and every rebuild.
    public private(set) var epoch = 0
    /// Bumped when the desktop's history is no longer the history that pages in flight were taken from: every rebuild, and every
    /// answer that shows the history shrinking (it was cleared, or a program wiped its scrollback and drew it again, as the inline
    /// agents do when their pane changes width). A page taken before is of the old history and is thrown away, never stitched on.
    public private(set) var era = 0
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
    /// Index ranges of lines that scrolled by unseen and are held as `StyledLine.missing` until `merge(page:)` fills them. Sorted,
    /// apart from each other, all below `liveStart`. Only ever made when the desktop announced the shift, so the indexes are sound.
    public private(set) var holes: [Range<Int>] = [] {
        // Every assignment (a live answer makes two to four) drops the seams of holes that are gone. Against a set of the holes' lower
        // edges, so that it costs the holes and the seams, not their product (output that outruns the answers can leave hundreds of both).
        didSet {
            guard !unverifiedSeams.isEmpty else { return }
            let edges = Set(holes.lazy.map(\.lowerBound))
            unverifiedSeams = unverifiedSeams.filter(edges.contains)
        }
    }
    /// Lower edges of holes whose seam with the older lines has not been compared with the desktop's yet. A burst that fits no answer
    /// is taken to be where the announced `history_size` says it is, with nothing to check that against; the seam is what does.
    private var unverifiedSeams: Set<Int> = []
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
    /// Whether a hole lies within `screens` screens of the view, which starts at line `top` and shows `rows` lines.
    public func holeNear(top: Int, rows: Int, screens: Int) -> Bool {
        let before = top - screens * rows, after = top + (screens + 1) * rows
        return holes.contains { $0.upperBound > before && $0.lowerBound < after }
    }
    /// Lines of the holes: shown blank until their page arrives.
    public var missingLines: Int { holes.reduce(0) { $0 + $1.count } }
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
        var announced = false
        let wasDrifting = drifting
        if let size = tail.historySize, let previous = historySize {
            let candidate = oldTop + (size - previous)
            if matches(tail, top: candidate) { newTop = candidate; resolved = true; announced = true }
        }
        if !resolved, let shift = alignedShift(tail) {
            newTop = oldTop + shift
            resolved = true
            // The desktop said the history did not change (or grew by another amount) but the lines moved: its history is full.
            if tail.historySize != nil { drifting = true }
        }
        guard resolved else { rebase(tail); change.rebased = true; return change }
        change.shift = newTop - oldTop
        // The history got shorter: lines left it. Pages taken before are of what it was.
        if let size = tail.historySize, let previous = historySize, size < previous { era &+= 1 }

        let tailStart = newTop - n
        let keepEnd = min(tailStart, oldTop)
        var dropped = false
        if keepEnd < tailStart {
            // A gap between the scrollback held and the new answer: more lines scrolled past than the answer reaches back. With the
            // shift announced by `history_size` the lines in between exist on the desktop and have a known place: keep what is held,
            // leave a hole, fetch it. Otherwise (or when the gap is more than the phone would keep) start over from the answer.
            let gap = tailStart - keepEnd
            if announced, !drifting, heldHistory > 0, gap <= HistoryLimits.heldLines {
                lines.removeSubrange((keepEnd - start)...)
                lines.append(contentsOf: repeatElement(StyledLine.missing, count: gap))
                clipHoles(from: keepEnd)
                holes.append(keepEnd..<tailStart)
                unverifiedSeams.insert(keepEnd)
                change.holeLines = gap
            } else {
                dropped = heldHistory > 0
                lines.removeAll(keepingCapacity: true)
                start = tailStart
                holes = []
            }
        } else if keepEnd <= start {
            lines.removeAll(keepingCapacity: true)
            start = tailStart
            holes = []
        } else {
            lines.removeSubrange((keepEnd - start)...)
            clipHoles(from: keepEnd)
        }
        lines.append(contentsOf: tail.lines)
        screenTop = newTop
        liveStart = tailStart
        cursorIndex = tail.cursorLine.map { tailStart + $0 }
        cursorColumn = tail.cursorLine == nil ? nil : tail.cursorColumn
        historySize = tail.historySize
        if dropped { atTop = false; change.droppedOlder = true }
        if allowTrim, trimToCap() { atTop = false }
        // A history that turns out to be full cannot place a hole's lines any more.
        if drifting, !wasDrifting, !holes.isEmpty { dropOlder(); change.droppedOlder = true }
        refreshAtTop()
        return change
    }

    /// Forgets the holes from this index up: the lines there are being replaced by an answer.
    private mutating func clipHoles(from index: Int) {
        holes = holes.compactMap { hole in
            if hole.upperBound <= index { return hole }
            return hole.lowerBound >= index ? nil : hole.lowerBound..<index
        }
    }
    /// Forgets the holes below this index: the lines there are gone.
    private mutating func clipHoles(below index: Int) {
        holes = holes.compactMap { hole in
            if hole.lowerBound >= index { return hole }
            return hole.upperBound <= index ? nil : index..<hole.upperBound
        }
    }
    /// The lines in `range` are no longer missing.
    private mutating func fill(_ range: Range<Int>) {
        var rest: [Range<Int>] = []
        for hole in holes {
            if hole.upperBound <= range.lowerBound || hole.lowerBound >= range.upperBound { rest.append(hole); continue }
            if hole.lowerBound < range.lowerBound { rest.append(hole.lowerBound..<range.lowerBound) }
            if hole.upperBound > range.upperBound { rest.append(range.upperBound..<hole.upperBound) }
        }
        holes = rest
    }

    /// Drops the oldest lines past the cap. Returns whether any went.
    private mutating func trimToCap() -> Bool {
        let excess = heldHistory - HistoryLimits.heldLines
        guard excess > 0 else { return false }
        lines.removeFirst(excess)
        start += excess
        clipHoles(below: start)
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
        era &+= 1
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
        holes = []
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
            // A line that is missing agrees with anything; the answer is what brings it.
            if held.isMissing { index += 1; continue }
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
    ///
    /// Holes come first, newest first: they are close to the live end, where the reader goes after the bottom.
    ///
    /// `fillHoles` false leaves them (the bandwidth policy does not want them now) and asks for the lines above the oldest held.
    /// `maximumLines` is the most the desktop takes in a page (1000 for a desktop that does not say otherwise).
    public func nextFetch(pageLines: Int = HistoryLimits.pageLines, fillHoles: Bool = true, maximumLines: Int = HistoryLimits.maximumPageLines) -> HistoryFetch? {
        guard canPage else { return nil }
        let maximumLines = max(1, min(maximumLines, HistoryLimits.maximumPageLines))
        if fillHoles, let hole = holes.last { return holeFetch(hole, pageLines: pageLines, maximumLines: maximumLines) }
        guard !atTop else { return nil }
        let held = heldHistory
        let room = HistoryLimits.heldLines - held
        let fresh = min(pageLines, room)
        guard fresh > 0 else { return nil }
        // Once the desktop's history is full (seen dropping lines, or as large as its limit) `history_size` no longer says how far the
        // screen has moved, so pages overlap what is held and the seam is checked by comparing lines.
        let full = drifting || (historySize ?? 0) >= HistoryLimits.desktopHistoryLines
        let overlap = full ? min(HistoryLimits.verifyLines, held) : 0
        return HistoryFetch(end: held - overlap, lines: min(maximumLines, fresh + overlap), overlap: overlap, epoch: epoch, era: era, historySize: historySize)
    }

    /// The part of a hole just above the live lines, with a few held lines on each side of what is missing so both seams are checked.
    private func holeFetch(_ hole: Range<Int>, pageLines: Int, maximumLines: Int) -> HistoryFetch {
        // A hole of several pages starts with a look at its seam with the older lines, a few lines either side: if the history is not
        // the one held (wiped and drawn again, a shell cleared while it was away) that is found before the hole is downloaded.
        if hole.count > pageLines, unverifiedSeams.contains(hole.lowerBound) {
            let top = max(start, hole.lowerBound - HistoryLimits.verifyLines)
            let bottom = min(hole.upperBound, hole.lowerBound + HistoryLimits.verifyLines)
            return HistoryFetch(end: screenTop - bottom, lines: bottom - top, overlap: 0, epoch: epoch, era: era, fillsHole: true, historySize: historySize)
        }
        let below = max(0, min(HistoryLimits.verifyLines, screenTop - hole.upperBound))
        let bottom = hole.upperBound + below
        var top = hole.upperBound - min(max(1, pageLines), hole.count)
        // The last page of a hole also covers the held lines right above it.
        if top <= hole.lowerBound { top = max(start, hole.lowerBound - HistoryLimits.verifyLines) }
        let lines = min(maximumLines, bottom - top)
        return HistoryFetch(end: screenTop - bottom, lines: lines, overlap: below, epoch: epoch, era: era, fillsHole: true, historySize: historySize)
    }

    /// Puts a page above the scrollback held, or into a hole.
    ///
    /// `reported` is the `history_size` the page came with. The page was taken from the desktop's screen as it was then, which may
    /// be further down the history than the phone's last live answer: the page is placed by that difference, and the lines it shares
    /// with what is held are checked and left out. A page that disagrees with a line held drops everything older than the live
    /// answer, to be fetched again.
    @discardableResult
    public mutating func merge(page: [StyledLine], historySize reported: Int?, complete: Bool, for fetch: HistoryFetch) -> HistoryMerge {
        guard filled, fetch.epoch == epoch, fetch.era == era else { return .stale }
        var top = screenTop
        if let reported, let known = historySize {
            // The history is shorter than it was when this was asked for: it was cleared or drawn again since, and this page, taken from
            // what it is now, cannot be placed in the numbering held. The next live answer sorts that out. (A page that reports less
            // than the latest answer, but not less than when it was asked for, was merely overtaken by that answer: it is placed by the
            // difference, which is then negative.)
            if reported < (fetch.historySize ?? known) { return .stale }
            top += reported - known
        }
        var bottom = top - fetch.end
        let count = page.count
        // A page that ends above what is held leaves lines out between them: a gap. (An empty page claims that nothing exists above
        // its bottom, which is the same thing when lines are held above that.)
        guard bottom >= start else { dropOlder(); return .inconsistent }
        if count == 0 {
            // Nothing where lines are missing is as wrong as it gets.
            if fetch.fillsHole { dropOlder(); return .inconsistent }
            if complete {
                // "Nothing is older than this": only believable when nothing is held older than where the page would have ended.
                guard bottom <= start else { dropOlder(); return .inconsistent }
                atTop = true
            }
            return .merged(added: 0)
        }
        // A desktop whose history is full cannot announce the lines that scrolled in since the last answer, so a page that overlaps the
        // lines held may sit a few lines lower than worked out. Look for where its overlap fits before giving up on it.
        if fetch.overlap > 0, !fetch.fillsHole, !agrees(page: page, bottom: bottom, strict: false) {
            guard let shift = (1...Self.maximumUnannouncedShift).first(where: { agrees(page: page, bottom: bottom + $0, strict: true) }) else {
                dropOlder(); return .inconsistent
            }
            bottom += shift
        }
        let pageStart = bottom - count
        // Page line j is numbered pageStart + j. The ones below `start` are new; the ones that are held scrollback too must agree,
        // unless they stand for lines that were missing, which they now fill.
        let heldEnd = min(bottom, screenTop)
        var number = max(pageStart, start)
        var filled = 0
        while number < heldEnd {
            let incoming = page[number - pageStart]
            if lines[number - start].isMissing { filled += 1 }
            else if !lines[number - start].sameText(as: incoming) { dropOlder(); return .inconsistent }
            number += 1
        }
        let added = min(count, max(0, start - pageStart))
        guard added > 0 || filled > 0 else { return .retry }
        if filled > 0 {
            for index in max(pageStart, start)..<heldEnd where lines[index - start].isMissing { lines[index - start] = page[index - pageStart] }
            fill(max(pageStart, start)..<heldEnd)
        }
        if added > 0 {
            lines.insert(contentsOf: page[0..<added], at: 0)
            start -= added
        }
        if complete {
            // The desktop says nothing is older than this page; lines held above it say otherwise.
            guard pageStart <= start else { dropOlder(); return .inconsistent }
            atTop = true
        }
        return .merged(added: added + filled)
    }

    /// How many lines may have scrolled in unannounced between the last answer and a page, on a desktop whose history is full.
    static let maximumUnannouncedShift = 24

    /// Whether the lines of `page` that fall on held scrollback, the page ending at `bottom`, are the lines held there. `strict` also
    /// wants a few lines that say something, since a run of blank lines fits anywhere.
    private func agrees(page: [StyledLine], bottom: Int, strict: Bool) -> Bool {
        let pageStart = bottom - page.count
        let end = min(bottom, screenTop)
        var number = max(pageStart, start)
        var compared = 0, text = 0
        while number < end {
            let held = lines[number - start]
            if !held.isMissing {
                if !held.sameText(as: page[number - pageStart]) { return false }
                compared += 1
                if !held.text.isEmpty { text += 1 }
            }
            number += 1
        }
        return !strict || (compared >= min(HistoryLimits.verifyLines, page.count) && text >= 2)
    }

    /// Forgets the pages above the latest live answer (and the holes); they are fetched again from there.
    public mutating func dropOlder() {
        let drop = liveStart - start
        if drop > 0 {
            lines.removeFirst(drop)
            start = liveStart
        }
        holes = []
        atTop = false
        refreshAtTop()
    }

    /// The scrollback and screen as plain text, one line per row: the screen and the last `scrollbackLines` lines above it.
    public func plainText(scrollbackLines: Int) -> String {
        guard !lines.isEmpty else { return "" }
        let first = max(start, screenTop - max(0, scrollbackLines))
        // The lines of a hole are not text the desktop ever showed this phone; they are left out, not copied as blank lines.
        return lines[(first - start)...].filter { !$0.isMissing }.map(\.text).joined(separator: "\n")
    }

    /// Back to nothing, as for another shell.
    public mutating func reset() {
        let (nextEpoch, nextEra) = (epoch &+ 1, era &+ 1)
        self = TerminalBuffer()
        epoch = nextEpoch; era = nextEra
    }
}
