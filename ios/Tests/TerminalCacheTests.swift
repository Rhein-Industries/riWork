import XCTest
@testable import RiWorkCore

final class TerminalCacheTests: XCTestCase {
    private func buffer(history: Int = 700, held: Int? = nil) -> TerminalBuffer {
        let desktop = FakeScrollback(history: history)
        var buffer = TerminalBuffer()
        buffer.applyLive(desktop.live())
        while let wanted = held, buffer.heldHistory < wanted, let fetch = buffer.nextFetch(pageLines: 1000) {
            let reply = desktop.page(end: fetch.end, lines: fetch.lines)
            buffer.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch)
        }
        return buffer
    }

    func testABufferComesBackOnceAndIsForgotten() {
        var cache = TerminalCache()
        let held = buffer()
        cache.store(held, key: "a", columns: 49)
        XCTAssertTrue(cache.contains("a"))
        XCTAssertEqual(cache.take(key: "a", columns: 49), held)
        XCTAssertNil(cache.take(key: "a", columns: 49), "it goes back to the model; a second take has nothing")
        XCTAssertEqual(cache.count, 0)
    }
    func testLinesWrappedAtAnotherWidthAreNotKept() {
        var cache = TerminalCache()
        cache.store(buffer(), key: "a", columns: 49)
        XCTAssertNil(cache.take(key: "a", columns: 52), "a font size or a focus mode changed the grid while it was away")
        cache.store(buffer(), key: "b", columns: nil)
        XCTAssertNotNil(cache.take(key: "b", columns: 49), "an unknown width is not a reason to throw them away")
        cache.store(buffer(), key: "c", columns: 49)
        XCTAssertNotNil(cache.take(key: "c", columns: nil))
    }
    func testAnEmptyBufferIsNotStored() {
        var cache = TerminalCache()
        cache.store(TerminalBuffer(), key: "a", columns: 49)
        XCTAssertFalse(cache.contains("a"))
    }
    func testTheLeastRecentlyUsedShellGoesFirstWhenThereAreTooMany() {
        var cache = TerminalCache()
        for key in ["a", "b", "c", "d", "e"] { cache.store(buffer(history: 100), key: key, columns: 49) }
        XCTAssertEqual(cache.count, TerminalCache.maximumShells)
        XCTAssertFalse(cache.contains("a"))
        XCTAssertTrue(cache.contains("e"))
        // Storing again refreshes a shell.
        cache.store(buffer(history: 100), key: "b", columns: 49)
        cache.store(buffer(history: 100), key: "f", columns: 49)
        XCTAssertTrue(cache.contains("b"))
        XCTAssertFalse(cache.contains("c"))
    }
    func testMemoryIsBoundedByTheTotalOfLinesAndTheNewestEntryAlwaysStays() {
        var cache = TerminalCache()
        let big = buffer(history: 40_000, held: 40_000)
        XCTAssertGreaterThan(big.lines.count, 40_000)
        cache.store(big, key: "a", columns: 49)
        cache.store(big, key: "b", columns: 49)
        cache.store(big, key: "c", columns: 49)
        XCTAssertLessThanOrEqual(cache.lineCount, TerminalCache.maximumLines)
        XCTAssertTrue(cache.contains("c"))
        XCTAssertFalse(cache.contains("a"))
        var one = TerminalCache()
        var huge = buffer(history: 130_000, held: HistoryLimits.heldLines)
        // Over the total with room to spare: a second copy of the lines stands in for a bigger cap.
        for _ in 0..<2 { huge = buffer(history: 130_000, held: HistoryLimits.heldLines) }
        var padded = TerminalCache()
        padded.store(huge, key: "y", columns: 49); padded.store(huge, key: "z", columns: 49); padded.store(huge, key: "w", columns: 49)
        XCTAssertLessThanOrEqual(padded.lineCount, TerminalCache.maximumLines + huge.lines.count)
        XCTAssertTrue(padded.contains("w"))
        one.store(huge, key: "x", columns: 49)
        XCTAssertTrue(one.contains("x"), "the shell just left is kept even when it alone is over the total")
    }
    func testRemovingAndClearing() {
        var cache = TerminalCache()
        cache.store(buffer(), key: "a", columns: 49); cache.store(buffer(), key: "b", columns: 49)
        cache.remove("a")
        XCTAssertFalse(cache.contains("a"))
        cache.removeAll()
        XCTAssertEqual(cache.count, 0)
    }

    // MARK: a restored buffer meets the next live answer

    func testARestoredBufferIsLinedUpWithTheNextAnswerAndKeepsItsOlderLines() {
        var desktop = FakeScrollback(history: 2000)
        var held = TerminalBuffer()
        held.applyLive(desktop.live(lines: 500))
        // Older pages were fetched before the shell was left.
        while held.heldHistory < 1500, let fetch = held.nextFetch() {
            let reply = desktop.page(end: fetch.end, lines: fetch.lines)
            held.merge(page: reply.lines, historySize: reply.historySize, complete: reply.complete, for: fetch)
        }
        var cache = TerminalCache()
        cache.store(held, key: "a", columns: 49)
        desktop.write(37)   // while away
        var restored = cache.take(key: "a", columns: 49)!
        let change = restored.applyLive(desktop.live(lines: 120))
        XCTAssertFalse(change.rebased)
        XCTAssertEqual(change.shift, 37)
        XCTAssertEqual(restored.start, held.start, "the 1,500 lines are still there")
        XCTAssertEqual(restored.heldHistory, held.heldHistory + 37)
        for index in restored.indices where restored[index]?.text != "L\(index)" { XCTFail("index \(index) is \(String(describing: restored[index]?.text))"); break }
    }
    func testARestoredBufferOfADesktopWhoseHistoryWasWipedAndDrawnShorterIsEmptiedByTheAnswer() {
        var desktop = FakeScrollback(history: 2000)
        var held = TerminalBuffer()
        held.applyLive(desktop.live(lines: 500))
        let era = held.era
        var cache = TerminalCache()
        cache.store(held, key: "a", columns: 49)
        // An inline agent drew its transcript again: now 1,300 lines. The lines held lie below the end of that history.
        desktop = FakeScrollback(history: 1300)
        var restored = cache.take(key: "a", columns: 49)!
        let change = restored.applyLive(desktop.live(lines: 120))
        XCTAssertLessThan(change.shift, 0)
        XCTAssertNotEqual(restored.era, era, "pages in flight from before are not for this history")
        XCTAssertEqual(restored.heldHistory, 120)
        XCTAssertEqual(restored.start, 1300 - 120)
        for index in restored.indices where restored[index]?.text != "L\(index)" { XCTFail("index \(index) is \(String(describing: restored[index]?.text))"); break }
    }
    func testARestoredBufferWhoseLinesAreWrappedDifferentlyIsThrownAwayByTheAnswer() {
        var desktop = FakeScrollback(history: 2000)
        var held = TerminalBuffer()
        held.applyLive(desktop.live(lines: 500))
        let era = held.era
        var cache = TerminalCache()
        cache.store(held, key: "a", columns: 49)
        // The same transcript, the same length, drawn at another width: every line differs.
        desktop = FakeScrollback(history: 2000)
        let drawn = desktop.live(lines: 120)
        let tail = LiveTail(lines: drawn.lines.map { StyledLine(text: "w" + $0.text, runs: [], columns: $0.columns + 1) }, historyLines: drawn.historyLines,
                            historySize: drawn.historySize, cursorLine: drawn.cursorLine, cursorColumn: drawn.cursorColumn)
        var restored = cache.take(key: "a", columns: 49)!
        let change = restored.applyLive(tail)
        XCTAssertTrue(change.rebased, "nothing of the old lines fits the new ones: start over")
        XCTAssertNotEqual(restored.era, era)
        XCTAssertEqual(restored.heldHistory, 120)
        XCTAssertEqual(restored.start, 2000 - 120)
    }
}
