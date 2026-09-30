import XCTest
@testable import RiWorkCore

/// A desktop's scrollback, as tmux keeps it: lines numbered in the order they were written, the last `rows` of them on the screen
/// and the rest in history, which drops its oldest lines once it holds `cap`.
struct FakeScrollback {
    var serials: [Int]
    let rows: Int
    var cap: Int?
    private var next: Int
    init(history: Int, rows: Int = 10, cap: Int? = nil) {
        self.rows = rows; self.cap = cap
        serials = Array(0..<(history + rows))
        next = history + rows
    }
    var historySize: Int { max(0, serials.count - rows) }
    /// The number of the screen's top row, counted from the first line ever written.
    var screenTopSerial: Int { serials.count > rows ? serials[serials.count - rows] : serials[0] }
    mutating func write(_ count: Int) {
        for _ in 0..<count { serials.append(next); next += 1 }
        if let cap, historySize > cap { serials.removeFirst(historySize - cap) }
    }
    static func line(_ serial: Int) -> StyledLine { StyledLine(text: "L\(serial)", runs: [StyleRun(length: "L\(serial)".count, style: .plain)], columns: "L\(serial)".count) }
    /// `shell.output` with `lines` of scrollback: the screen and min(lines, history) lines above it.
    func live(lines: Int = 500, reportsHistory: Bool = true) -> LiveTail {
        let history = min(lines, historySize)
        let slice = serials.suffix(history + rows)
        return LiveTail(lines: slice.map(Self.line), historyLines: history, historySize: reportsHistory ? historySize : nil,
                        cursorLine: slice.count - 1, cursorColumn: 0)
    }
    /// `shell.history`: the lines -(end+lines) … -(end+1) above the screen, clamped at the top of history.
    func page(end: Int, lines: Int) -> (lines: [StyledLine], historySize: Int, complete: Bool) {
        let hist = historySize
        let top = min(hist, end + lines), bottom = min(hist, end)
        let firstOfHistory = serials.count - rows - hist
        let slice = serials[(firstOfHistory + hist - top)..<(firstOfHistory + hist - bottom)]
        return (slice.map(Self.line), hist, end + lines >= hist)
    }
}

final class TerminalBufferTests: XCTestCase {
    /// Every line held is the line the numbering says: `offset` is serial minus absolute index, the same for all of them.
    private func assertConsistent(_ buffer: TerminalBuffer, offset: Int, file: StaticString = #filePath, line: UInt = #line) {
        for index in buffer.indices {
            XCTAssertEqual(buffer[index]?.text, "L\(index + offset)", "absolute index \(index)", file: file, line: line)
            if buffer[index]?.text != "L\(index + offset)" { return }
        }
    }
    private func page(_ desktop: FakeScrollback, _ fetch: HistoryFetch) -> (lines: [StyledLine], historySize: Int, complete: Bool) {
        desktop.page(end: fetch.end, lines: fetch.lines)
    }

    // MARK: numbering

    func testTheFirstAnswerNumbersTheScreensTopRowByHistorySize() {
        let desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        let change = buffer.applyLive(desktop.live())
        XCTAssertTrue(change.rebased)
        XCTAssertEqual(buffer.screenTop, 2000)
        XCTAssertEqual(buffer.start, 1500)
        XCTAssertEqual(buffer.endIndex, 2010)
        XCTAssertEqual(buffer.heldHistory, 500)
        XCTAssertEqual(buffer.cursorIndex, 2009)
        XCTAssertFalse(buffer.atTop)
        XCTAssertEqual(buffer.historySize, 2000)
        assertConsistent(buffer, offset: 0)
    }
    func testAShortHistoryIsAtItsTopAtOnce() {
        let desktop = FakeScrollback(history: 120)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        XCTAssertEqual(buffer.start, 0)
        XCTAssertTrue(buffer.atTop, "the answer reaches back to the first line")
        XCTAssertNil(buffer.nextFetch(), "nothing older to ask for")
    }
    func testNewOutputKeepsEveryLinesIndexAndOnlyAppends() {
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        let epoch = buffer.epoch
        let anchor = buffer[1800]
        desktop.write(7)
        let change = buffer.applyLive(desktop.live())
        XCTAssertEqual(change.shift, 7)
        XCTAssertFalse(change.rebased)
        XCTAssertEqual(buffer.epoch, epoch)
        XCTAssertEqual(buffer[1800], anchor, "a line keeps its index when lines are added below it")
        XCTAssertEqual(buffer.screenTop, 2007)
        XCTAssertEqual(buffer.endIndex, 2017)
        assertConsistent(buffer, offset: 0)
        for _ in 0..<50 { desktop.write(3); buffer.applyLive(desktop.live()) }
        XCTAssertEqual(buffer.screenTop, 2157)
        assertConsistent(buffer, offset: 0)
    }
    func testAnUnchangedAnswerChangesNothing() {
        let desktop = FakeScrollback(history: 800)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        let before = buffer
        let change = buffer.applyLive(desktop.live())
        XCTAssertEqual(change, LiveChange())
        XCTAssertEqual(buffer, before)
    }
    func testScreenRowsAreReplacedByEveryAnswer() {
        let desktop = FakeScrollback(history: 50)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        var tail = desktop.live()
        var lines = tail.lines
        lines[lines.count - 1] = StyledLine(text: "$ typed", runs: [], columns: 7)
        tail = LiveTail(lines: lines, historyLines: tail.historyLines, historySize: tail.historySize, cursorLine: lines.count - 1, cursorColumn: 7)
        buffer.applyLive(tail)
        XCTAssertEqual(buffer[buffer.endIndex - 1]?.text, "$ typed")
        XCTAssertEqual(buffer.cursorColumn, 7)
        XCTAssertEqual(buffer.screenTop, 50)
    }

    // MARK: paging

    func testPagingWalksUpTheHistoryInOrderWithEndOffsets() throws {
        let desktop = FakeScrollback(history: 1300)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        var ends: [Int] = []
        var guardCount = 0
        while let fetch = buffer.nextFetch() {
            guardCount += 1; XCTAssertLessThan(guardCount, 10)
            ends.append(fetch.end)
            XCTAssertEqual(fetch.lines, 300)
            XCTAssertEqual(fetch.overlap, 0)
            let reply = page(desktop, fetch)
            XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch), .merged(added: reply.lines.count))
            assertConsistent(buffer, offset: 0)
        }
        XCTAssertEqual(ends, [500, 800, 1100], "end = the lines already held above the screen")
        XCTAssertTrue(buffer.atTop)
        XCTAssertEqual(buffer.start, 0)
        XCTAssertEqual(buffer.heldHistory, 1300, "the last page was clamped at the top of history: 200 lines, not 300")
        XCTAssertEqual(buffer.endIndex, 1310)
        assertConsistent(buffer, offset: 0)
    }
    func testACompletePageEndsPagingEvenWhenTheLocalCountSaysThereIsMore() {
        let desktop = FakeScrollback(history: 900)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        let fetch = try! XCTUnwrap(buffer.nextFetch())
        // The desktop says nothing older exists, although the local count leaves 100 lines unaccounted for.
        let reply = desktop.page(end: fetch.end, lines: fetch.lines)
        buffer.merge(page: reply.lines, historySize: reply.historySize, complete: true, for: fetch)
        XCTAssertTrue(buffer.atTop)
        XCTAssertNil(buffer.nextFetch())
        // and it stays so through later live answers
        var more = desktop
        more.write(2)
        buffer.applyLive(more.live())
        XCTAssertTrue(buffer.atTop)
    }
    func testTrailingSpacesOneCaptureKeptAndAnotherTrimmedAreTheSameLine() {
        let desktop = FakeScrollback(history: 900)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        let fetch = try! XCTUnwrap(buffer.nextFetch())
        var reply = desktop.page(end: fetch.end, lines: fetch.lines)
        reply.lines = reply.lines.map { StyledLine(text: $0.text + "   ", runs: [], columns: $0.columns + 3) }
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch), .merged(added: 300))
        var grown = desktop
        grown.write(3)
        var tail = grown.live()
        tail = LiveTail(lines: tail.lines.map { StyledLine(text: $0.text + " ", runs: [], columns: $0.columns + 1) }, historyLines: tail.historyLines, historySize: tail.historySize,
                        cursorLine: tail.cursorLine, cursorColumn: tail.cursorColumn)
        XCTAssertEqual(buffer.applyLive(tail).shift, 3)
        XCTAssertEqual(buffer.epoch, 1, "no renumbering over a few spaces")
        XCTAssertTrue(StyledLine(text: "a ", runs: [], columns: 2).sameText(as: StyledLine(text: "a", runs: [], columns: 1)))
        XCTAssertFalse(StyledLine(text: " a", runs: [], columns: 2).sameText(as: StyledLine(text: "a", runs: [], columns: 1)), "leading spaces count")
    }
    func testAnEmptyPageThatClaimsTheTopAboveLinesThatAreHeldIsNotBelieved() {
        let desktop = FakeScrollback(history: 900)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        let fetch = try! XCTUnwrap(buffer.nextFetch())
        // The desktop says nothing exists above 700 lines, when 400 of them are above the lines held here.
        XCTAssertEqual(buffer.merge(page: [], historySize: 600, complete: true, for: fetch), .inconsistent)
        XCTAssertFalse(buffer.atTop)
    }
    func testAnEmptyCompletePageJustMarksTheTop() {
        let desktop = FakeScrollback(history: 900)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        let fetch = try! XCTUnwrap(buffer.nextFetch())
        XCTAssertEqual(buffer.merge(page: [], historySize: 900, complete: true, for: fetch), .merged(added: 0))
        XCTAssertTrue(buffer.atTop)
    }
    func testOutputArrivingWhileAPageIsOnItsWayShiftsTheWindowAndNothingIsLostOrRepeated() {
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        let fetch = try! XCTUnwrap(buffer.nextFetch())
        XCTAssertEqual(fetch.end, 500)
        desktop.write(12)   // history_size 2012 when the page is taken
        let reply = page(desktop, fetch)
        XCTAssertEqual(reply.historySize, 2012)
        // The page covers lines 2012-800 … 2012-501, i.e. numbers 1212 … 1511: 12 of them are lines already held.
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch), .merged(added: 288))
        assertConsistent(buffer, offset: 0)
        XCTAssertEqual(buffer.start, 1212)
        XCTAssertEqual(buffer.screenTop, 2000, "the screen moves with the next live answer")
        let change = buffer.applyLive(desktop.live())
        XCTAssertEqual(change.shift, 12)
        XCTAssertFalse(change.rebased)
        XCTAssertEqual(buffer.start, 1212, "older lines survive the live answer")
        assertConsistent(buffer, offset: 0)
        // The next page continues right above: end counts from the new screen.
        let next = try! XCTUnwrap(buffer.nextFetch())
        XCTAssertEqual(next.end, 800)
        let reply2 = page(desktop, next)
        XCTAssertEqual(buffer.merge(page: reply2.lines, historySize: reply2.historySize, complete: reply2.complete, for: next), .merged(added: 300))
        assertConsistent(buffer, offset: 0)
        XCTAssertEqual(buffer.start, 912)
    }
    func testHistoryThatGrewByMoreThanAPageAsksAgain() {
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        let fetch = try! XCTUnwrap(buffer.nextFetch(pageLines: 50))
        desktop.write(400)
        let reply = page(desktop, fetch)
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch), .retry, "the page lies entirely inside lines already held")
        XCTAssertEqual(buffer.heldHistory, 100)
        buffer.applyLive(desktop.live(lines: 100))
        assertConsistent(buffer, offset: 0)
    }
    func testGrowthAlsoShiftsTheWindowWhenTheGapToTheLiveAnswerWouldOtherwiseLeaveHoles() {
        // 2000 lines of history, 100 in the answer. Output writes 350 lines between two answers: more than the answer reaches back.
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        desktop.write(350)
        let change = buffer.applyLive(desktop.live(lines: 100))
        XCTAssertEqual(change.shift, 350)
        XCTAssertTrue(change.droppedOlder, "lines 1900 … 2249 are missing, so older lines cannot be kept next to the answer")
        XCTAssertFalse(change.rebased, "the numbering still holds: the lines of the answer keep the index they would have had")
        XCTAssertEqual(buffer.start, 2250)
        assertConsistent(buffer, offset: 0)
        XCTAssertEqual(buffer.nextFetch()?.end, 100)
    }
    func testAPageForAnotherNumberingIsThrownAway() {
        let desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        let fetch = try! XCTUnwrap(buffer.nextFetch())
        buffer.reset()
        buffer.applyLive(desktop.live())
        let reply = page(desktop, fetch)
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: false, for: fetch), .stale)
        XCTAssertEqual(buffer.heldHistory, 500)
    }
    func testAPageThatDoesNotLineUpDropsTheOlderHistoryAndItIsFetchedAgain() {
        let desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        var fetch = try! XCTUnwrap(buffer.nextFetch())
        var reply = page(desktop, fetch)
        buffer.merge(page: reply.lines, historySize: reply.historySize, complete: false, for: fetch)
        XCTAssertEqual(buffer.heldHistory, 800)
        // Another page, whose lines cannot be ours: the desktop's numbering moved under it (history_size shrank by 50).
        fetch = try! XCTUnwrap(buffer.nextFetch())
        reply = page(desktop, fetch)
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: 1950, complete: false, for: fetch), .inconsistent)
        XCTAssertEqual(buffer.heldHistory, 500, "only the live answer's scrollback is left")
        XCTAssertEqual(buffer.start, 1500)
        assertConsistent(buffer, offset: 0)
        // Fetching again starts from the live part and is fine.
        let again = try! XCTUnwrap(buffer.nextFetch())
        XCTAssertEqual(again.end, 500)
        let good = page(desktop, again)
        XCTAssertEqual(buffer.merge(page: good.lines, historySize: good.historySize, complete: false, for: again), .merged(added: 300))
        assertConsistent(buffer, offset: 0)
    }
    func testPageLinesAreHalvedByTheCallerAndTheNextFetchFollowsTheNewSize() {
        let desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        XCTAssertEqual(buffer.nextFetch(pageLines: 300)?.lines, 300)
        XCTAssertEqual(buffer.nextFetch(pageLines: 150)?.lines, 150)
        XCTAssertEqual(buffer.nextFetch(pageLines: 75)?.lines, 75)
        let fetch = try! XCTUnwrap(buffer.nextFetch(pageLines: 75))
        let reply = page(desktop, fetch)
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch), .merged(added: 75))
        XCTAssertEqual(buffer.nextFetch(pageLines: 75)?.end, 575)
    }

    // MARK: cap

    func testPagingStopsAtTheCapAndTheLastPageIsShortened() {
        let desktop = FakeScrollback(history: 30_000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        var pages = 0
        while let fetch = buffer.nextFetch() {
            let reply = page(desktop, fetch)
            buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch)
            pages += 1
            XCTAssertLessThanOrEqual(buffer.heldHistory, HistoryLimits.heldLines)
            XCTAssertLessThan(pages, 100)
        }
        XCTAssertEqual(buffer.heldHistory, HistoryLimits.heldLines)
        XCTAssertTrue(buffer.limitReached)
        XCTAssertFalse(buffer.atTop, "older lines exist, they are just not loaded")
        XCTAssertEqual(buffer.start, 10_000)
        assertConsistent(buffer, offset: 0)
    }
    func testNewOutputAtTheCapDropsTheOldestLines() {
        var desktop = FakeScrollback(history: 30_000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        while let fetch = buffer.nextFetch() {
            let reply = page(desktop, fetch)
            buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch)
        }
        desktop.write(40)
        buffer.applyLive(desktop.live())
        XCTAssertEqual(buffer.heldHistory, HistoryLimits.heldLines, "the cap holds")
        XCTAssertEqual(buffer.start, 10_040, "the oldest 40 lines went")
        XCTAssertFalse(buffer.atTop)
        assertConsistent(buffer, offset: 0)
        // While the view is being scrolled the oldest lines stay, so nothing above the reader moves.
        desktop.write(10)
        buffer.applyLive(desktop.live(), allowTrim: false)
        XCTAssertEqual(buffer.start, 10_040)
        XCTAssertEqual(buffer.heldHistory, HistoryLimits.heldLines + 10)
        desktop.write(1)
        buffer.applyLive(desktop.live())
        XCTAssertEqual(buffer.heldHistory, HistoryLimits.heldLines)
        assertConsistent(buffer, offset: 0)
    }

    // MARK: history that stops growing

    func testAFullHistoryThatDropsItsOldestLinesIsFollowedByMatchingTheLines() {
        // The desktop keeps 1000 lines of history; it is full, so history_size stays 1000 while lines keep arriving.
        var desktop = FakeScrollback(history: 1000, cap: 1000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        XCTAssertEqual(buffer.screenTop, 1000)
        let epoch = buffer.epoch
        desktop.write(9)
        XCTAssertEqual(desktop.historySize, 1000, "history_size does not show it")
        let change = buffer.applyLive(desktop.live())
        XCTAssertEqual(change.shift, 9)
        XCTAssertFalse(change.rebased)
        XCTAssertEqual(buffer.epoch, epoch, "the indexes of the lines held stay valid")
        XCTAssertEqual(buffer.screenTop, 1009)
        XCTAssertTrue(buffer.drifting)
        assertConsistent(buffer, offset: 0)
        for _ in 0..<20 { desktop.write(4); buffer.applyLive(desktop.live()) }
        XCTAssertEqual(buffer.epoch, epoch)
        assertConsistent(buffer, offset: 0)
    }
    func testPagesOfAFullHistoryOverlapWhatIsHeldSoTheSeamCanBeChecked() {
        var desktop = FakeScrollback(history: 3000, cap: 3000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        desktop.write(5)
        buffer.applyLive(desktop.live())
        XCTAssertTrue(buffer.drifting)
        let fetch = try! XCTUnwrap(buffer.nextFetch())
        XCTAssertEqual(fetch.overlap, HistoryLimits.verifyLines)
        XCTAssertEqual(buffer.heldHistory, 505, "the 5 lines that scrolled in are kept next to the 500")
        XCTAssertEqual(fetch.end, 505 - HistoryLimits.verifyLines)
        XCTAssertEqual(fetch.lines, 300 + HistoryLimits.verifyLines)
        // Two more lines arrive before the page is taken: history_size cannot say so, the overlap does not match any more.
        desktop.write(2)
        let reply = page(desktop, fetch)
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch), .inconsistent)
        XCTAssertEqual(buffer.heldHistory, 500, "nothing was stitched on wrongly: only the live answer's scrollback is left")
        // After the next live answer the same fetch works.
        buffer.applyLive(desktop.live())
        assertConsistent(buffer, offset: 0)
        let again = try! XCTUnwrap(buffer.nextFetch())
        let good = page(desktop, again)
        XCTAssertEqual(buffer.merge(page: good.lines, historySize: good.historySize, complete: good.complete, for: again), .merged(added: 300))
        assertConsistent(buffer, offset: 0)
    }
    func testAHistoryAsLargeAsTheDesktopsLimitIsAssumedFullFromTheStart() {
        let desktop = FakeScrollback(history: HistoryLimits.desktopHistoryLines, rows: 10)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        XCTAssertFalse(buffer.drifting, "nothing has been seen dropping yet")
        XCTAssertEqual(buffer.nextFetch()?.overlap, HistoryLimits.verifyLines, "but it cannot grow, so the seam is checked from the first page")
        let small = FakeScrollback(history: 5000)
        var other = TerminalBuffer()
        other.applyLive(small.live())
        XCTAssertEqual(other.nextFetch()?.overlap, 0)
    }
    func testADesktopWithoutHistorySizeIsFollowedByMatchingTheLinesAndCannotPage() {
        var desktop = FakeScrollback(history: 800)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(reportsHistory: false))
        XCTAssertNil(buffer.historySize)
        XCTAssertNil(buffer.nextFetch(), "no paging on a desktop that does not report its history")
        XCTAssertFalse(buffer.canPage)
        XCTAssertFalse(buffer.atTop)
        let epoch = buffer.epoch
        let anchor = buffer[buffer.screenTop - 10]
        desktop.write(6)
        let change = buffer.applyLive(desktop.live(reportsHistory: false))
        XCTAssertEqual(change.shift, 6)
        XCTAssertEqual(buffer.epoch, epoch)
        XCTAssertEqual(buffer[buffer.screenTop - 16], anchor, "the line keeps its index")
        for _ in 0..<30 { desktop.write(3); buffer.applyLive(desktop.live(reportsHistory: false)) }
        XCTAssertEqual(buffer.epoch, epoch)
    }
    func testLinesThatCannotBeMatchedRenumberEverything() {
        var desktop = FakeScrollback(history: 1000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        let epoch = buffer.epoch
        // The pane was re-wrapped by a resize: every line is different, and the history has a different length.
        desktop = FakeScrollback(history: 1200)
        var tail = desktop.live()
        tail = LiveTail(lines: tail.lines.map { StyledLine(text: "w" + $0.text, runs: [], columns: $0.columns + 1) }, historyLines: tail.historyLines,
                        historySize: tail.historySize, cursorLine: tail.cursorLine, cursorColumn: tail.cursorColumn)
        let change = buffer.applyLive(tail)
        XCTAssertTrue(change.rebased)
        XCTAssertNotEqual(buffer.epoch, epoch)
        XCTAssertEqual(buffer.screenTop, 1200)
        XCTAssertEqual(buffer.heldHistory, 500)
        XCTAssertEqual(buffer.start, 700)
    }
    func testClearedHistoryIsTakenOverWithoutRenumbering() {
        var desktop = FakeScrollback(history: 1000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        let epoch = buffer.epoch
        desktop = FakeScrollback(history: 0)   // clear-history: nothing above the screen any more
        buffer.applyLive(desktop.live())
        XCTAssertEqual(buffer.epoch, epoch)
        XCTAssertEqual(buffer.heldHistory, 0)
        XCTAssertEqual(buffer.screenTop, 0, "the screen moved back up the numbering")
        XCTAssertTrue(buffer.atTop)
    }
    func testAScreenThatGrowsTakesLinesBackFromHistory() {
        // The pane got taller (the keyboard went away): 10 lines that were history are screen rows now.
        let desktop = FakeScrollback(history: 400, rows: 10)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        let held = buffer[395]
        let taller = FakeScrollback(history: 390, rows: 20)
        let change = buffer.applyLive(taller.live())
        XCTAssertEqual(change.shift, -10)
        XCTAssertFalse(change.rebased)
        XCTAssertEqual(buffer[395], held)
        XCTAssertEqual(buffer.screenTop, 390)
        assertConsistent(buffer, offset: 0)
    }

    // MARK: random sequences

    private struct LCG: RandomNumberGenerator {
        var state: UInt64
        // splitmix64: all 64 bits are well mixed, which Swift's bounded random numbers rely on.
        mutating func next() -> UInt64 {
            state = state &+ 0x9E3779B97F4A7C15
            var z = state
            z = (z ^ (z >> 30)) &* 0xBF58476D1CE4E5B9
            z = (z ^ (z >> 27)) &* 0x94D049BB133111EB
            return z ^ (z >> 31)
        }
    }
    /// Live answers, page fetches (with and without output arriving while the page is on its way) and trims in any order, against
    /// a desktop with a full history and one without: whatever happens, the lines held are consecutive lines of the desktop's
    /// output, each under the index it had before.
    func testRandomSequencesNeverLeaveGapsDuplicatesOrMovedLines() {
        var merged = 0, retried = 0, inconsistent = 0, rebased = 0, dropped = 0, drifted = 0
        defer { print("FUZZ merged \(merged) retry \(retried) inconsistent \(inconsistent) rebased \(rebased) droppedOlder \(dropped) drifted \(drifted)") }
        for cap in [nil, 300, 4000] as [Int?] {
            for seed in 1...25 {
                var rng = LCG(state: UInt64(seed) &* 7919 &+ UInt64(cap ?? 1))
                // A capped desktop starts out full: the moment a history fills up is the one thing a page cannot be checked against.
                var desktop = FakeScrollback(history: cap ?? Int.random(in: 0...2500, using: &rng), rows: 10, cap: cap)
                var buffer = TerminalBuffer()
                buffer.applyLive(desktop.live(lines: 500))
                // A desktop whose history is full shows it as soon as output flows; one answer to learn it.
                desktop.write(3)
                buffer.applyLive(desktop.live(lines: 500))
                var epoch = buffer.epoch
                func offset() -> Int { Int(buffer[buffer.start]!.text.dropFirst())! - buffer.start }
                var known = offset()
                for step in 0..<150 {
                    let what = Int.random(in: 0..<5, using: &rng)
                    switch what {
                    case 0:
                        desktop.write(Int.random(in: 0...40, using: &rng))
                        let change = buffer.applyLive(desktop.live(lines: [100, 300, 500].randomElement(using: &rng)!), allowTrim: Bool.random(using: &rng))
                        if change.rebased { rebased += 1 }; if change.droppedOlder { dropped += 1 }
                    case 1, 2:
                        guard let fetch = buffer.nextFetch(pageLines: [300, 75, 20].randomElement(using: &rng)!) else { break }
                        if what == 2 { desktop.write(Int.random(in: 0...15, using: &rng)) }
                        let reply = desktop.page(end: fetch.end, lines: fetch.lines)
                        switch buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch) {
                        case .merged: merged += 1
                        case .retry: retried += 1
                        case .inconsistent: inconsistent += 1
                        case .stale: XCTFail("the numbering never changed")
                        }
                    case 3:
                        desktop.write(Int.random(in: 0...3, using: &rng))
                        buffer.applyLive(desktop.live(lines: 500))
                    default:
                        desktop.write(Int.random(in: 100...700, using: &rng))   // can be more than an answer reaches back
                        let change = buffer.applyLive(desktop.live(lines: 300))
                        if change.rebased { rebased += 1 }; if change.droppedOlder { dropped += 1 }
                    }
                    if buffer.epoch != epoch { epoch = buffer.epoch; known = offset() }
                    if buffer.drifting { drifted += 1 }
                    let label = "cap \(String(describing: cap)) seed \(seed) step \(step) op \(what)"
                    // consecutive, in order, at the numbering they have always had
                    var wrong: Int?
                    for index in buffer.indices where buffer[index]?.text != "L\(index + known)" { wrong = index; break }
                    XCTAssertNil(wrong, "\(label): index \(wrong ?? -1) is \(String(describing: wrong.flatMap { buffer[$0]?.text })), expected L\((wrong ?? 0) + known)")
                    if wrong != nil { return }
                    // after a live answer, the screen is where the desktop's is
                    if what == 0 || what >= 3 { XCTAssertEqual(buffer.screenTop + known, desktop.screenTopSerial, label) }
                }
            }
        }
        XCTAssertGreaterThan(merged, 500)
        XCTAssertGreaterThan(retried, 0)
        XCTAssertGreaterThan(inconsistent, 0, "capped histories with output arriving mid-page are caught by the overlap")
        XCTAssertGreaterThan(dropped, 0)
        XCTAssertGreaterThan(drifted, 0)
    }

    // MARK: styled pages

    func testAPageIsParsedIntoStyledLines() {
        let esc = "\u{1B}"
        let page = "plain\n\(esc)[31mred\(esc)[0m text\n\n"
        let lines = TerminalText.styledLines(page: page)
        XCTAssertEqual(lines.map(\.text), ["plain", "red text", ""], "a blank last line is a line; the terminator is not")
        XCTAssertFalse(lines[1].runs.allSatisfy { $0.style.isPlain })
        XCTAssertEqual(TerminalText.styledLines(page: "a\nb\n").map(\.text), ["a", "b"])
        XCTAssertEqual(TerminalText.styledLines(page: "a\nb").map(\.text), ["a", "b"], "a missing terminator is tolerated")
        XCTAssertEqual(TerminalText.styledLines(page: ""), [])
        XCTAssertEqual(TerminalText.styledLines(page: "\n").map(\.text), [""])
        XCTAssertEqual(TerminalText.styledLines(page: "\n\n\n").map(\.text), ["", "", ""])
    }
    func testTheLiveScreenSaysWhereItsScreenStarts() {
        let text = (0..<9).map { "row \($0)" }.joined(separator: "\n") + "\n\n\n\n"
        let screen = TerminalText.styledScreen(text, cursor: (x: 2, y: 1), rows: 5)
        XCTAssertEqual(screen.historyLines, 7)
        XCTAssertEqual(screen.cursorLine, 8)
        XCTAssertEqual(screen.lines.count, 9, "the blank rows under the cursor are trimmed, scrollback is not")
        let tail = LiveTail(screen: screen, historySize: 99)
        XCTAssertEqual(tail.historyLines, 7)
        XCTAssertEqual(tail.cursorLine, 8)
        XCTAssertEqual(TerminalText.styledScreen("a\nb\n", cursor: nil, rows: nil).historyLines, 0)
    }

    // MARK: protocol

    func testOutputExtrasAreReadLeniently() {
        let full = OutputExtras(result: .object(["history_size": .number(1234), "alternate": .bool(true)]))
        XCTAssertEqual(full.historySize, 1234)
        XCTAssertEqual(full.alternate, true)
        let older = OutputExtras(result: .object(["output": .string("x")]))
        XCTAssertNil(older.historySize); XCTAssertNil(older.alternate)
        let hostile = OutputExtras(result: .object(["history_size": .number(-3), "alternate": .string("yes")]))
        XCTAssertNil(hostile.historySize); XCTAssertNil(hostile.alternate)
        XCTAssertNil(OutputExtras(result: .object(["history_size": .number(1.5)])).historySize)
    }
    func testShellOutputAndItsUnchangedFormCarryTheNewFields() throws {
        let result = JSONValue.object(["shell_id": .string("s"), "output": .string("a\n"), "history_size": .number(40), "alternate": .bool(false), "hash": .string("h")])
        let output = try ShellOutput(result: result)
        XCTAssertEqual(output.historySize, 40)
        XCTAssertEqual(output.alternate, false)
        let older = try ShellOutput(result: .object(["shell_id": .string("s"), "output": .string("a\n")]))
        XCTAssertNil(older.historySize); XCTAssertNil(older.alternate)
        let unchanged = JSONValue.object(["shell_id": .string("s"), "unchanged": .bool(true), "hash": .string("h"), "history_size": .number(40), "alternate": .bool(true)])
        XCTAssertEqual(try OutputReply(result: unchanged), .unchanged(shellID: "s", hash: "h"))
        XCTAssertEqual(OutputExtras(result: unchanged).alternate, true)
    }
    func testAHistoryRequestAndItsReply() throws {
        let shell = "44444444-4444-4444-8444-444444444444"
        let request = HistoryRequest(shellID: shell, end: 500, lines: 300)
        XCTAssertEqual(request.params, ["shell_id": .string(shell), "end": .number(500), "lines": .number(300), "styled": .bool(true)])
        XCTAssertEqual(request.plain.params["styled"], nil, "plain text asks without the field")
        XCTAssertEqual(HistoryRequest(shellID: shell, end: -4, lines: 5000).params["end"], .number(0))
        XCTAssertEqual(HistoryRequest(shellID: shell, end: 0, lines: 5000).params["lines"], .number(1000))
        try RequestValidation.validate(method: "shell.history", params: request.params, id: UUID().uuidString.lowercased())
        try RequestValidation.validate(method: "shell.history", params: request.plain.params, id: UUID().uuidString.lowercased())
        for bad: [String: JSONValue] in [
            ["shell_id": .string(shell), "end": .number(0)],
            ["shell_id": .string(shell), "end": .number(-1), "lines": .number(10)],
            ["shell_id": .string(shell), "end": .number(0), "lines": .number(0)],
            ["shell_id": .string(shell), "end": .number(0), "lines": .number(1001)],
            ["shell_id": .string(shell), "end": .number(1.5), "lines": .number(10)],
            ["shell_id": .string(shell), "end": .number(0), "lines": .number(10), "styled": .string("yes")],
            ["shell_id": .string(shell), "end": .number(0), "lines": .number(10), "extra": .bool(true)]
        ] {
            XCTAssertThrowsError(try RequestValidation.validate(method: "shell.history", params: bad, id: UUID().uuidString.lowercased()), "\(bad)")
        }
        let reply = try HistoryReply(result: .object(["shell_id": .string(shell), "output": .string("a\nb\n"), "line_count": .number(2), "history_size": .number(9), "complete": .bool(true)]))
        XCTAssertEqual(reply.lineCount, 2); XCTAssertEqual(reply.historySize, 9); XCTAssertTrue(reply.complete)
        let sparse = try HistoryReply(result: .object(["shell_id": .string(shell), "output": .string("")]))
        XCTAssertNil(sparse.lineCount); XCTAssertNil(sparse.historySize); XCTAssertFalse(sparse.complete)
        XCTAssertThrowsError(try HistoryReply(result: .object(["output": .string("")])))
        XCTAssertTrue(RemoteError.rpc(code: "response_too_large", message: "x").asksForFewerLines)
        XCTAssertFalse(RemoteError.rpc(code: "cli_error", message: "x").asksForFewerLines)
    }
}
