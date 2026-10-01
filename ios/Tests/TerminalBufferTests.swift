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
            // A hole's lines are blank placeholders, and only inside a hole.
            let inHole = buffer.holes.contains { $0.contains(index) }
            XCTAssertEqual(buffer[index]?.isMissing, inHole, "absolute index \(index): placeholders are exactly the holes", file: file, line: line)
            if inHole { continue }
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
        // The history grew by 100 lines meanwhile, and the desktop says nothing exists above the 600 lines it would have ended at,
        // when 100 of them are above the lines held here.
        XCTAssertEqual(buffer.merge(page: [], historySize: 1000, complete: true, for: fetch), .inconsistent)
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
        // The 400 lines that scrolled by are a hole between the 100 held and the new answer; nothing is thrown away.
        XCTAssertEqual(buffer.holes, [2000..<2300])
        XCTAssertEqual(buffer.start, 1900)
        assertConsistent(buffer, offset: 0)
    }
    func testMoreOutputThanAnAnswerReachesBackLeavesAHoleAndKeepsTheOlderLines() throws {
        // 2000 lines of history, 100 in the answer. Output writes 350 lines between two answers: more than the answer reaches back.
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        desktop.write(350)
        let change = buffer.applyLive(desktop.live(lines: 100))
        XCTAssertEqual(change.shift, 350)
        XCTAssertFalse(change.droppedOlder, "the history_size announced the shift, so what is held is still where it was")
        XCTAssertFalse(change.rebased)
        XCTAssertEqual(change.holeLines, 250, "lines 2000 … 2249 scrolled by unseen")
        XCTAssertEqual(buffer.holes, [2000..<2250])
        XCTAssertEqual(buffer.missingLines, 250)
        XCTAssertEqual(buffer.start, 1900, "the 100 lines held before stay put")
        XCTAssertEqual(buffer.liveStart, 2250)
        XCTAssertTrue(buffer[2100]?.isMissing == true)
        assertConsistent(buffer, offset: 0)
        // The hole is asked for first, with a few held lines on both sides so the seams are checked.
        let fetch = try XCTUnwrap(buffer.nextFetch())
        XCTAssertTrue(fetch.fillsHole)
        XCTAssertEqual(fetch.overlap, HistoryLimits.verifyLines)
        XCTAssertEqual(fetch.end, 92)
        XCTAssertEqual(fetch.lines, 266, "250 missing, 8 held above and 8 below")
        let reply = page(desktop, fetch)
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch), .merged(added: 250))
        XCTAssertTrue(buffer.holes.isEmpty)
        XCTAssertEqual(buffer.start, 1900)
        assertConsistent(buffer, offset: 0)
        XCTAssertFalse(try XCTUnwrap(buffer.nextFetch()).fillsHole, "back to the lines above the oldest held")
    }
    func testAHoleBiggerThanAPageIsFilledNewestFirstAndOutputMeanwhileShiftsThePages() throws {
        var desktop = FakeScrollback(history: 3000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        desktop.write(900)
        buffer.applyLive(desktop.live(lines: 100))
        XCTAssertEqual(buffer.holes, [3000..<3800])
        var rounds = 0
        while !buffer.holes.isEmpty {
            let fetch = try XCTUnwrap(buffer.nextFetch(pageLines: 300))
            XCTAssertTrue(fetch.fillsHole)
            // Output keeps coming: the page is taken from a screen further down than the one the fetch was worked out for.
            if rounds == 1 { desktop.write(7) }
            let reply = page(desktop, fetch)
            let before = buffer.holes.last!
            let result = buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch)
            if case .merged = result { XCTAssertLessThan(buffer.holes.last?.upperBound ?? 0, before.upperBound + 1) } else { XCTAssertEqual(result, .retry) }
            XCTAssertNotEqual(result, .inconsistent)
            assertConsistent(buffer, offset: 0)
            rounds += 1
            if rounds == 1 { buffer.applyLive(desktop.live(lines: 100)) }
            XCTAssertLessThan(rounds, 12)
        }
        XCTAssertGreaterThanOrEqual(rounds, 3, "800 lines at 300 a page")
        buffer.applyLive(desktop.live(lines: 100))
        assertConsistent(buffer, offset: 0)
        XCTAssertTrue(buffer.holes.isEmpty)
    }
    func testASecondGapWhileAHoleIsOpenMakesASecondHole() {
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        desktop.write(300)
        buffer.applyLive(desktop.live(lines: 100))
        XCTAssertEqual(buffer.holes, [2000..<2200])
        desktop.write(40)
        buffer.applyLive(desktop.live(lines: 100))
        XCTAssertEqual(buffer.holes, [2000..<2200], "within reach of the answer: nothing new is missing")
        desktop.write(500)
        buffer.applyLive(desktop.live(lines: 100))
        XCTAssertEqual(buffer.holes.count, 2)
        XCTAssertEqual(buffer.holes.first, 2000..<2200)
        XCTAssertEqual(buffer.holes.last, 2340..<2740, "the lines of the earlier answers stay, with the second hole after them")
        assertConsistent(buffer, offset: 0)
    }
    func testAnAnswerThatReachesBackIntoAHoleReplacesItsUpperPart() {
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        desktop.write(300)
        buffer.applyLive(desktop.live(lines: 100))
        XCTAssertEqual(buffer.holes, [2000..<2200])
        // A bigger answer now reaches back 250 lines: the lines it carries are real, so the hole shrinks.
        buffer.applyLive(desktop.live(lines: 250))
        XCTAssertEqual(buffer.holes, [2000..<2050])
        assertConsistent(buffer, offset: 0)
        buffer.applyLive(desktop.live(lines: 500))
        XCTAssertTrue(buffer.holes.isEmpty)
        assertConsistent(buffer, offset: 0)
    }
    func testWithoutAnnouncedHistoryTheGapStillStartsOver() {
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100, reportsHistory: false))
        desktop.write(350)
        let change = buffer.applyLive(desktop.live(lines: 100, reportsHistory: false))
        // Nothing announces the shift; the lines of the two answers share nothing to match: the numbering starts over.
        XCTAssertTrue(change.rebased || change.droppedOlder)
        XCTAssertTrue(buffer.holes.isEmpty)
    }
    func testAGapBiggerThanThePhoneKeepsIsNotHeldAsAHole() {
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        desktop.write(HistoryLimits.heldLines + 500)
        let change = buffer.applyLive(desktop.live(lines: 100))
        XCTAssertTrue(change.droppedOlder)
        XCTAssertTrue(buffer.holes.isEmpty)
        XCTAssertEqual(buffer.heldHistory, 100)
        XCTAssertEqual(buffer.start, buffer.screenTop - 100)
    }
    func testHolesWithoutOlderLinesHeldAreNotMade() {
        var desktop = FakeScrollback(history: 40, rows: 10)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 0))
        XCTAssertEqual(buffer.heldHistory, 0)
        desktop.write(300)
        let change = buffer.applyLive(desktop.live(lines: 100))
        XCTAssertEqual(change.holeLines, 0, "nothing is held that a hole would keep company")
        XCTAssertTrue(buffer.holes.isEmpty)
    }
    func testAHolePageThatDoesNotMatchTheLinesAroundItDropsEverythingOlder() throws {
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        desktop.write(350)
        buffer.applyLive(desktop.live(lines: 100))
        let fetch = try XCTUnwrap(buffer.nextFetch())
        // The desktop's numbering moved under the request (its history was cleared and refilled): the seam lines differ.
        let reply = page(desktop, fetch)
        let changed = reply.lines.map { StyledLine(text: "x" + $0.text, runs: [], columns: $0.columns + 1) }
        XCTAssertEqual(buffer.merge(page: changed, historySize: reply.historySize, complete: reply.complete, for: fetch), .inconsistent)
        XCTAssertTrue(buffer.holes.isEmpty)
        XCTAssertEqual(buffer.start, buffer.liveStart, "only the live answer's scrollback is left")
        assertConsistent(buffer, offset: 0)
    }
    func testAnEmptyPageWhereLinesAreMissingIsNotBelieved() throws {
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        desktop.write(350)
        buffer.applyLive(desktop.live(lines: 100))
        let fetch = try XCTUnwrap(buffer.nextFetch())
        XCTAssertEqual(buffer.merge(page: [], historySize: 2350, complete: false, for: fetch), .inconsistent)
        XCTAssertTrue(buffer.holes.isEmpty)
    }
    func testTrimmingTheOldestLinesTakesTheHolesBelowTheNewStartWithThem() {
        var desktop = FakeScrollback(history: 3000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        desktop.write(200)
        buffer.applyLive(desktop.live(lines: 100))
        XCTAssertEqual(buffer.holes, [3000..<3100])
        // Pretend the cap is reached by a lot of new output that the answers keep up with, and the hole is trimmed from the bottom.
        var lines = buffer.lines
        _ = lines.popLast()
        XCTAssertFalse(lines.isEmpty)
        desktop.write(HistoryLimits.heldLines)
        buffer.applyLive(desktop.live(lines: 100))
        XCTAssertTrue(buffer.holes.allSatisfy { $0.lowerBound >= buffer.start }, "a hole never reaches below the first line held")
    }
    func testAFullHistoryDiscoveredWhileAHoleIsOpenDropsTheOlderLines() {
        var desktop = FakeScrollback(history: 1000, cap: 1000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        XCTAssertFalse(buffer.drifting)
        // A burst that history_size cannot announce (it is full) is a drift, not a hole.
        desktop.write(300)
        let change = buffer.applyLive(desktop.live(lines: 100))
        XCTAssertTrue(buffer.holes.isEmpty)
        XCTAssertTrue(change.rebased || change.droppedOlder || buffer.drifting)
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
        // Another page, whose lines cannot be ours: it claims the history grew by 50 lines, which puts it 50 lines further down than
        // the lines it carries really are, so it disagrees with what is held where they meet.
        fetch = try! XCTUnwrap(buffer.nextFetch())
        reply = page(desktop, fetch)
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: 2050, complete: false, for: fetch), .inconsistent)
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

    // MARK: a history that is drawn again

    func testAPageTakenBeforeTheHistoryWasWipedIsNeverStitchedOn() throws {
        // An inline agent clears its scrollback and draws it again when the pane changes width (history_size 85 -> 0 -> 108).
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 120))
        let fetch = try XCTUnwrap(buffer.nextFetch())
        let era = buffer.era
        let reply = page(desktop, fetch)      // taken from the old history
        // The answers that follow show the history cleared and growing again.
        desktop = FakeScrollback(history: 0)
        buffer.applyLive(desktop.live(lines: 120))
        XCTAssertGreaterThan(buffer.era, era, "a shrinking history is a new history")
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch), .stale)
        XCTAssertEqual(buffer.heldHistory, 0, "nothing of the old history was stitched on")
        XCTAssertEqual(buffer.screenTop, 0)
    }
    func testAPageWhoseHistoryIsShorterThanTheLastAnswerIsStaleNotPlaced() throws {
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 120))
        let fetch = try XCTUnwrap(buffer.nextFetch())
        // The page is taken after the history shrank, before the phone has an answer that shows it.
        desktop = FakeScrollback(history: 1200)
        let reply = page(desktop, fetch)
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch), .stale)
        XCTAssertEqual(buffer.heldHistory, 120, "untouched: the next answer sorts it out")
    }
    func testARebuildChangesTheEraAndTheEpoch() throws {
        let desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 120))
        let (epoch, era) = (buffer.epoch, buffer.era)
        buffer.reset()
        XCTAssertNotEqual(buffer.epoch, epoch); XCTAssertNotEqual(buffer.era, era)
        buffer.applyLive(desktop.live(lines: 120))
        XCTAssertGreaterThan(buffer.era, era)
        // A history that only grows keeps its era, however fast.
        let steady = buffer.era
        var more = desktop
        more.write(40)
        buffer.applyLive(more.live(lines: 120))
        XCTAssertEqual(buffer.era, steady)
    }
    func testThePlainTextOfTheScreenAndItsLatestScrollback() {
        var buffer = TerminalBuffer()
        buffer.applyLive(FakeScrollback(history: 50, rows: 5).live(lines: 40))
        let text = buffer.plainText(scrollbackLines: 3)
        XCTAssertEqual(text, ["L47", "L48", "L49", "L50", "L51", "L52", "L53", "L54"].joined(separator: "\n"))
        XCTAssertEqual(buffer.plainText(scrollbackLines: 1_000_000).split(separator: "\n").count, 45)
        XCTAssertEqual(TerminalBuffer().plainText(scrollbackLines: 10), "")
    }

    // MARK: cap

    func testPagingStopsAtTheCapAndTheLastPageIsShortened() {
        let desktop = FakeScrollback(history: 80_000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        var pages = 0
        while let fetch = buffer.nextFetch() {
            let reply = page(desktop, fetch)
            buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch)
            pages += 1
            XCTAssertLessThanOrEqual(buffer.heldHistory, HistoryLimits.heldLines)
            XCTAssertLessThan(pages, 200)
        }
        XCTAssertEqual(buffer.heldHistory, HistoryLimits.heldLines)
        XCTAssertTrue(buffer.limitReached)
        XCTAssertFalse(buffer.atTop, "older lines exist, they are just not loaded")
        XCTAssertEqual(buffer.start, 30_000)
        assertConsistent(buffer, offset: 0)
    }
    func testNewOutputAtTheCapDropsTheOldestLines() {
        var desktop = FakeScrollback(history: 80_000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        while let fetch = buffer.nextFetch() {
            let reply = page(desktop, fetch)
            buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch)
        }
        desktop.write(40)
        buffer.applyLive(desktop.live())
        XCTAssertEqual(buffer.heldHistory, HistoryLimits.heldLines, "the cap holds")
        XCTAssertEqual(buffer.start, 30_040, "the oldest 40 lines went")
        XCTAssertFalse(buffer.atTop)
        assertConsistent(buffer, offset: 0)
        // While the view is being scrolled the oldest lines stay, so nothing above the reader moves.
        desktop.write(10)
        buffer.applyLive(desktop.live(), allowTrim: false)
        XCTAssertEqual(buffer.start, 30_040)
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
        // Two more lines arrive before the page is taken: history_size cannot say so, but the overlap fits two lines lower, and the page
        // is placed there instead of being thrown away with everything prefetched.
        desktop.write(2)
        let reply = page(desktop, fetch)
        guard case .merged(let added) = buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch) else { return XCTFail("placed where the overlap fits") }
        XCTAssertEqual(added, 300 - 2, "the page sits two lines lower than worked out, so two fewer of its lines are new")
        assertConsistent(buffer, offset: 0)
        XCTAssertGreaterThan(buffer.heldHistory, 505)
        // After the next live answer the same kind of fetch works too.
        buffer.applyLive(desktop.live())
        assertConsistent(buffer, offset: 0)
        let again = try! XCTUnwrap(buffer.nextFetch())
        let good = page(desktop, again)
        XCTAssertEqual(buffer.merge(page: good.lines, historySize: good.historySize, complete: good.complete, for: again), .merged(added: 300))
        assertConsistent(buffer, offset: 0)
    }
    func testAPageThatFitsNowhereWithinReachIsRefusedAndTheOlderHistoryDropped() {
        var desktop = FakeScrollback(history: 3000, cap: 3000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        desktop.write(5)
        buffer.applyLive(desktop.live())
        let fetch = try! XCTUnwrap(buffer.nextFetch())
        // More than the phone is willing to search for scrolled in meanwhile.
        desktop.write(HistoryLimits.verifyLines + 100)
        let reply = page(desktop, fetch)
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch), .inconsistent)
        XCTAssertEqual(buffer.heldHistory, 500)
    }
    func testAPageOvertakenByALiveAnswerIsPlacedByTheDifferenceNotThrownAway() throws {
        // Two requests are in the desktop's shared slots at once, and the answer to the later one gets here first.
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 120))
        let fetch = try XCTUnwrap(buffer.nextFetch())
        let reply = page(desktop, fetch)      // taken at history_size 2000
        desktop.write(30)
        buffer.applyLive(desktop.live(lines: 120))   // seen first: 2030
        XCTAssertEqual(buffer.historySize, 2030)
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch), .merged(added: 300), "its lines are where they always were")
        assertConsistent(buffer, offset: 0)
        XCTAssertEqual(buffer.start, 1880 - 300)
    }
    func testAHoleOfSeveralPagesStartsWithALookAtItsSeamWithTheOlderLines() throws {
        var desktop = FakeScrollback(history: 3000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        desktop.write(900)
        buffer.applyLive(desktop.live(lines: 100))
        XCTAssertEqual(buffer.holes, [3000..<3800])
        // The first request is a few lines either side of the seam, not 300 lines of the hole.
        let probe = try XCTUnwrap(buffer.nextFetch(pageLines: 300))
        XCTAssertTrue(probe.fillsHole)
        XCTAssertEqual(probe.lines, 2 * HistoryLimits.verifyLines)
        let reply = page(desktop, probe)
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: probe), .merged(added: HistoryLimits.verifyLines))
        XCTAssertEqual(buffer.holes, [3008..<3800], "the seam matched, and the first lines of the hole came with the look")
        // Once looked at, the next is a page of the hole from its newer end.
        let next = try XCTUnwrap(buffer.nextFetch(pageLines: 300))
        XCTAssertEqual(next.lines, 300 + HistoryLimits.verifyLines, "a page and its overlap with the lines held under it")
        assertConsistent(buffer, offset: 0)
    }
    func testAHoleWhoseOlderSideIsNotTheHistoryHeldIsFoundBeforeItIsDownloaded() throws {
        var desktop = FakeScrollback(history: 3000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        // The desktop's history was wiped and has grown past the old size with other lines; the old ones are still held.
        desktop = FakeScrollback(history: 3900); desktop.serials = desktop.serials.map { $0 + 50_000 }
        buffer.applyLive(desktop.live(lines: 100))
        XCTAssertEqual(buffer.holes, [3000..<3800], "nothing in the answer overlaps the lines held, so the shift is taken as announced")
        let probe = try XCTUnwrap(buffer.nextFetch(pageLines: 300))
        XCTAssertEqual(probe.lines, 2 * HistoryLimits.verifyLines)
        let reply = page(desktop, probe)
        XCTAssertEqual(buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: probe), .inconsistent, "found with one small request")
        XCTAssertTrue(buffer.holes.isEmpty)
        XCTAssertEqual(buffer.heldHistory, 100, "and the old lines are gone")
    }
    func testHolesCanBeLeftAloneAndTheLinesAboveAskedForInstead() throws {
        var desktop = FakeScrollback(history: 3000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        desktop.write(700)
        buffer.applyLive(desktop.live(lines: 100))
        XCTAssertTrue(try XCTUnwrap(buffer.nextFetch()).fillsHole)
        let above = try XCTUnwrap(buffer.nextFetch(fillHoles: false))
        XCTAssertFalse(above.fillsHole)
        XCTAssertEqual(above.end, buffer.heldHistory, "just above the oldest line held")
        // Where the hole is, relative to a view.
        XCTAssertEqual(buffer.holes, [3000..<3600])
        XCTAssertTrue(buffer.holeNear(top: 3300, rows: 50, screens: 2), "in it")
        XCTAssertTrue(buffer.holeNear(top: 3690, rows: 50, screens: 2), "two screens below it")
        XCTAssertFalse(buffer.holeNear(top: 3710, rows: 50, screens: 2), "further below")
        XCTAssertTrue(buffer.holeNear(top: 2900, rows: 50, screens: 2), "just above it")
        XCTAssertFalse(buffer.holeNear(top: 2000, rows: 50, screens: 2), "far above it")
    }
    func testTheTextOfAHoleIsNotCopied() {
        var desktop = FakeScrollback(history: 2000)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live(lines: 100))
        desktop.write(300)
        buffer.applyLive(desktop.live(lines: 100))
        let text = buffer.plainText(scrollbackLines: 1000)
        XCTAssertFalse(text.contains("\n\n"), "no blank lines for lines that were never seen")
        XCTAssertEqual(text.split(separator: "\n").count, 100 + 100 + 10)
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
    /// output, each under the index it had before, apart from the blank placeholders of holes (which are exactly the holes) -- and
    /// once the holes are fetched, nothing is missing.
    func testRandomSequencesNeverLeaveGapsDuplicatesOrMovedLines() {
        var merged = 0, retried = 0, inconsistent = 0, rebased = 0, dropped = 0, drifted = 0, holed = 0, filledHoles = 0
        defer { print("FUZZ merged \(merged) retry \(retried) inconsistent \(inconsistent) rebased \(rebased) droppedOlder \(dropped) drifted \(drifted) holes \(holed) filled \(filledHoles)") }
        for cap in [nil, 300, 4000] as [Int?] {
            // A big history makes each step slower to check; fewer runs of it do as well.
            for seed in 1...(cap == 4000 ? 8 : 25) {
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
                func check(_ label: String) -> Bool {
                    // Consecutive, in order, at the numbering they have always had; placeholders exactly in the holes. Every line near
                    // the ends and around the holes, and every 61st between: a skipped or repeated line shifts all that follow it, so
                    // it cannot hide.
                    func good(_ index: Int) -> Bool {
                        guard let line = buffer[index] else { return true }
                        let inHole = buffer.holes.contains { $0.contains(index) }
                        if line.isMissing != inHole { XCTFail("\(label): index \(index) placeholder \(line.isMissing), in a hole \(inHole)"); return false }
                        if !inHole, line.text != "L\(index + known)" { XCTFail("\(label): index \(index) is \(line.text), expected L\(index + known)"); return false }
                        return true
                    }
                    var probes = Set<Int>()
                    for index in stride(from: buffer.start, to: buffer.endIndex, by: 61) { probes.insert(index) }
                    for index in buffer.start..<min(buffer.endIndex, buffer.start + 40) { probes.insert(index) }
                    for index in max(buffer.start, buffer.endIndex - 160)..<buffer.endIndex { probes.insert(index) }
                    for hole in buffer.holes { for index in [hole.lowerBound - 1, hole.lowerBound, hole.lowerBound + 1, hole.upperBound - 1, hole.upperBound, hole.upperBound + 1] { probes.insert(index) } }
                    for index in probes.sorted() where !good(index) { return false }
                    if !buffer.holes.allSatisfy({ $0.lowerBound >= buffer.start && $0.upperBound <= buffer.liveStart }) { XCTFail("\(label): holes \(buffer.holes) outside \(buffer.start)..<\(buffer.liveStart)"); return false }
                    return true
                }
                for step in 0..<150 {
                    let what = Int.random(in: 0..<5, using: &rng)
                    switch what {
                    case 0:
                        desktop.write(Int.random(in: 0...40, using: &rng))
                        let change = buffer.applyLive(desktop.live(lines: [100, 300, 500].randomElement(using: &rng)!), allowTrim: Bool.random(using: &rng))
                        if change.rebased { rebased += 1 }; if change.droppedOlder { dropped += 1 }; if change.holeLines > 0 { holed += 1 }
                    case 1, 2:
                        guard let fetch = buffer.nextFetch(pageLines: [300, 75, 20].randomElement(using: &rng)!) else { break }
                        if what == 2 { desktop.write(Int.random(in: 0...15, using: &rng)) }
                        let reply = desktop.page(end: fetch.end, lines: fetch.lines)
                        let hadHoles = !buffer.holes.isEmpty
                        switch buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch) {
                        case .merged: merged += 1; if hadHoles && fetch.fillsHole { filledHoles += 1 }
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
                        if change.rebased { rebased += 1 }; if change.droppedOlder { dropped += 1 }; if change.holeLines > 0 { holed += 1 }
                    }
                    if buffer.epoch != epoch { epoch = buffer.epoch; known = offset() }
                    if buffer.drifting { drifted += 1 }
                    let label = "cap \(String(describing: cap)) seed \(seed) step \(step) op \(what)"
                    if !check(label) { return }
                    // after a live answer, the screen is where the desktop's is
                    if what == 0 || what >= 3 { XCTAssertEqual(buffer.screenTop + known, desktop.screenTopSerial, label) }
                }
                // The holes go away by fetching them, one page after another, whatever scrolls in meanwhile.
                var rounds = 0
                while !buffer.holes.isEmpty, rounds < 50 {
                    guard let fetch = buffer.nextFetch(pageLines: 300) else { break }
                    XCTAssertTrue(fetch.fillsHole, "holes come first")
                    let reply = desktop.page(end: fetch.end, lines: fetch.lines)
                    // A page that lands inside lines already held means the screen moved on: the next live answer brings it up to date.
                    if buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch) == .retry {
                        buffer.applyLive(desktop.live(lines: 100))
                    }
                    if !check("cap \(String(describing: cap)) seed \(seed) filling") { return }
                    rounds += 1
                }
                XCTAssertTrue(buffer.holes.isEmpty, "cap \(String(describing: cap)) seed \(seed): the holes were fetched")
                XCTAssertEqual(buffer.missingLines, 0)
            }
        }
        XCTAssertGreaterThan(merged, 500)
        XCTAssertGreaterThan(retried, 0)
        XCTAssertGreaterThan(inconsistent, 0, "capped histories with output arriving mid-page are caught by the overlap")
        XCTAssertGreaterThan(rebased, 0)
        XCTAssertGreaterThan(drifted, 0)
        XCTAssertGreaterThan(holed, 20, "uncapped histories make holes of bursts")
        XCTAssertGreaterThan(filledHoles, 20)
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
    func testAPageIsReadAsTheDesktopWritesItAndCheckedAgainstItsLineCount() {
        // The wire: lines joined by line breaks, none after the last; `line_count` says how many there are.
        func read(_ text: String, _ count: Int) -> [String] { TerminalText.styledLines(page: text, expecting: count).map(\.text) }
        XCTAssertEqual(read("a\nb", 2), ["a", "b"])
        XCTAssertEqual(read("", 1), [""], "one blank line is an empty string with a count of 1")
        XCTAssertEqual(read("", 0), [], "and an empty page is one with a count of 0")
        XCTAssertEqual(read("\n\n", 3), ["", "", ""], "pages end in blank lines as often as anything: three blank lines")
        XCTAssertEqual(read("a\n", 2), ["a", ""], "a line and a blank one under it")
        XCTAssertEqual(read("\nz", 2), ["", "z"])
        // A terminator after every line (an older desktop, the fixture) is read as one.
        XCTAssertEqual(read("a\nb\n", 2), ["a", "b"])
        XCTAssertEqual(read("a\n\n", 2), ["a", ""])
        // A page that is not what it says is not forced into shape: the caller sees the wrong count.
        XCTAssertNotEqual(read("a\nb", 5).count, 5)
        XCTAssertEqual(TerminalText.styledLines(page: "a\nb\n\n", expecting: nil).map(\.text), ["a", "b", ""], "without a count the old reading holds")
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
        XCTAssertEqual(HistoryRequest(shellID: shell, end: -4, lines: 50).params["end"], .number(0))
        XCTAssertEqual(HistoryRequest(shellID: shell, end: 0, lines: 9000).params["lines"], .number(5000), "the most the protocol allows since 2026-10-01 (1000 before)")
        try RequestValidation.validate(method: "shell.history", params: request.params, id: UUID().uuidString.lowercased())
        try RequestValidation.validate(method: "shell.history", params: request.plain.params, id: UUID().uuidString.lowercased())
        for bad: [String: JSONValue] in [
            ["shell_id": .string(shell), "end": .number(0)],
            ["shell_id": .string(shell), "end": .number(-1), "lines": .number(10)],
            ["shell_id": .string(shell), "end": .number(0), "lines": .number(0)],
            ["shell_id": .string(shell), "end": .number(0), "lines": .number(5001)],
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
