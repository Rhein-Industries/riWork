import XCTest
@testable import RiWorkCore

/// Where lines sit in the scroll view: `index * lineHeight`, whatever is loaded above, so a page of older lines or a trim moves nothing.
final class TerminalScrollGeometryTests: XCTestCase {
    private func geometry(first: Int = 1000, end: Int = 1400, height: Double = 14, top: Double = 4, bottom: Double = 4, view: Double = 700) -> TerminalScrollGeometry {
        TerminalScrollGeometry(lineHeight: height, firstRow: first, endRow: end, topPadding: top, bottomPadding: bottom, viewportHeight: view)
    }

    func testALinesPlaceIsItsIndexTimesTheLineHeightWhateverIsLoadedAboveIt() {
        let short = geometry(first: 1300)
        let deep = geometry(first: 0)
        for index in [1300, 1350, 1399] {
            XCTAssertEqual(short.top(ofRow: index), Double(index) * 14)
            XCTAssertEqual(short.top(ofRow: index), deep.top(ofRow: index), "a page of 1,300 older lines moved nothing")
            XCTAssertEqual(short.y(ofRow: index, offset: 18_500), deep.y(ofRow: index, offset: 18_500))
        }
        XCTAssertEqual(short.contentHeight, deep.contentHeight)
    }
    func testThePagesOnlyChangeHowFarUpTheReaderMayScroll() {
        let short = geometry(first: 1300)
        let deep = geometry(first: 1000)
        XCTAssertEqual(short.minOffset, 1300 * 14 - 4)
        XCTAssertEqual(deep.minOffset, 1000 * 14 - 4)
        XCTAssertEqual(short.topInset, -short.minOffset, "the inset is the negative of the lowest offset")
        XCTAssertEqual(short.maxOffset, deep.maxOffset)
        XCTAssertEqual(short.maxOffset, 1400 * 14 + 4 - 700)
        // And the metrics StickyBottom follows: the offset is the reader's, and does not change; the content and the inset do.
        let offset = 18_000.0
        let (a, b) = (short.metrics(offset: offset), deep.metrics(offset: offset))
        XCTAssertEqual(a.offset, b.offset)
        XCTAssertNotEqual(a.contentHeight, b.contentHeight)
        XCTAssertEqual(a.distanceFromBottom, b.distanceFromBottom, accuracy: 0.0001, "same distance from the bottom, pages or none")
        XCTAssertEqual(a.distanceFromBottom, short.maxOffset - offset, accuracy: 0.0001)
        XCTAssertEqual(a.distanceFromTop, offset - short.minOffset, accuracy: 0.0001)
        XCTAssertTrue(b.resized(since: a), "a page does resize the content, as far as the follow logic goes")
    }
    func testTheVisibleRowsAreExactlyThoseThatReachIntoTheView() {
        let g = geometry(first: 0, end: 5000, height: 14, top: 0, bottom: 0, view: 700)
        XCTAssertEqual(g.visibleRows(offset: 0), 0..<50)
        XCTAssertEqual(g.visibleRows(offset: 7), 0..<51, "half of row 0 is still in, row 50 begins inside the view")
        XCTAssertEqual(g.visibleRows(offset: 14), 1..<51)
        XCTAssertEqual(g.visibleRows(offset: 14 * 4000 + 3), 4000..<4051)
        XCTAssertEqual(g.visibleRows(offset: g.maxOffset), 4950..<5000)
        XCTAssertEqual(g.visibleRows(offset: -100), 0..<43, "overscroll at the top")
        XCTAssertEqual(g.visibleRows(offset: g.maxOffset + 100), 4957..<5000, "overscroll at the bottom")
        // A row that begins exactly at the bottom edge is not in view.
        XCTAssertFalse(g.visibleRows(offset: 0).contains(50))
        XCTAssertTrue(TerminalScrollGeometry(lineHeight: 14, firstRow: 5, endRow: 5, viewportHeight: 700).visibleRows(offset: 0).isEmpty)
        XCTAssertTrue(TerminalScrollGeometry(lineHeight: 14, firstRow: 5, endRow: 50, viewportHeight: 0).visibleRows(offset: 0).isEmpty)
    }
    func testAShortTerminalSitsAtTheTopLikeARealOne() {
        let g = geometry(first: 0, end: 12, height: 14, top: 4, bottom: 4, view: 700)
        XCTAssertEqual(g.minOffset, -4)
        XCTAssertEqual(g.maxOffset, g.minOffset, "nothing to scroll")
        XCTAssertEqual(g.y(ofRow: 0, offset: g.minOffset), 4, "the first line is at the top padding")
        XCTAssertEqual(g.visibleRows(offset: g.maxOffset), 0..<12)
    }
    func testTheFirstLineSitsUnderTheTopPaddingAtTheTopOfWhatIsLoaded() {
        let g = geometry()
        XCTAssertEqual(g.y(ofRow: 1000, offset: g.minOffset), 4)
        XCTAssertEqual(g.y(ofRow: 1399, offset: g.maxOffset) + 14, 700 - 4, accuracy: 0.0001, "and the last line ends above the bottom padding")
    }
    func testRoomKeptForFloatingStatusIsPartOfTheBottomPadding() {
        let plain = geometry(bottom: 4)
        let withChip = geometry(bottom: 4 + 28)
        XCTAssertEqual(withChip.maxOffset - plain.maxOffset, 28)
        XCTAssertEqual(withChip.bottomInset, 32)
    }
    func testClampingAndDistances() {
        let g = geometry()
        XCTAssertEqual(g.clamped(offset: 0), g.minOffset)
        XCTAssertEqual(g.clamped(offset: 1_000_000), g.maxOffset)
        XCTAssertEqual(g.clamped(offset: 15_000), 15_000)
        XCTAssertEqual(g.distanceFromBottom(offset: g.maxOffset), 0)
        XCTAssertEqual(g.distanceFromBottom(offset: g.maxOffset - 100), 100)
        let (row, hidden) = g.topRow(offset: 14 * 1200 + 5)
        XCTAssertEqual(row, 1200); XCTAssertEqual(hidden, 5, accuracy: 0.0001)
    }
    func testFollowingTheBottomSurvivesAnyChange() {
        let old = geometry()
        let grown = geometry(end: 1410)
        XCTAssertEqual(grown.offset(after: old, offset: old.maxOffset, following: true), grown.maxOffset)
        let keyboard = geometry(view: 400)
        XCTAssertEqual(keyboard.offset(after: old, offset: old.maxOffset, following: true), keyboard.maxOffset)
    }
    func testAReaderStaysPutWhenLinesAreAddedAndWhenThePaneChangesSize() {
        let old = geometry()
        let offset = 17_000.0
        XCTAssertEqual(geometry(first: 700).offset(after: old, offset: offset, following: false), offset, "a page above")
        XCTAssertEqual(geometry(end: 1500).offset(after: old, offset: offset, following: false), offset, "output below")
        XCTAssertEqual(geometry(view: 380).offset(after: old, offset: offset, following: false), offset, "the keyboard came up")
    }
    func testAChangeOfLineHeightKeepsTheLineAtTheTopOfTheView() {
        let old = geometry(height: 14)
        let bigger = geometry(height: 21)
        let offset = 14 * 1200.0 + 7   // half way down row 1200
        let moved = bigger.offset(after: old, offset: offset, following: false)
        XCTAssertEqual(moved, 21 * 1200.0 + 10.5, accuracy: 0.0001)
        XCTAssertEqual(bigger.topRow(offset: moved).row, 1200)
        XCTAssertEqual(bigger.offset(after: old, offset: 1_000_000, following: false), bigger.maxOffset, "clamped")
    }
    func testRenumberedLinesKeepTheDistanceFromTheBottom() {
        let old = geometry(first: 1000, end: 1400)
        let renumbered = geometry(first: 300, end: 700)
        let offset = old.maxOffset - 640
        XCTAssertEqual(renumbered.distanceFromBottom(offset: renumbered.offset(after: old, offset: offset, following: false, renumbered: true)), 640, accuracy: 0.0001)
        // Further from the bottom than the new numbering has: the top.
        XCTAssertEqual(renumbered.offset(after: old, offset: old.maxOffset - 1_000_000, following: false, renumbered: true), renumbered.minOffset)
    }
    func testNumbersAtTheSizeOfAFullHistoryStayExact() {
        // 100,000 lines of 15 points: 1.5 million points down. Doubles keep every row's place to the last bit.
        let g = TerminalScrollGeometry(lineHeight: 15, firstRow: 50_000, endRow: 100_040, topPadding: 4, bottomPadding: 4, viewportHeight: 810)
        let lastScreenTop: Double = 100_040.0 * 15.0 + 4.0 - 810.0
        XCTAssertEqual(g.y(ofRow: 100_000, offset: g.maxOffset), 1_500_000.0 - lastScreenTop)
        XCTAssertEqual(g.visibleRows(offset: g.maxOffset).upperBound, 100_040)
        XCTAssertEqual(g.visibleRows(offset: g.maxOffset).lowerBound, 100_040 - 54)
        XCTAssertEqual(g.visibleRows(offset: g.minOffset).lowerBound, 50_000)
    }
    func testNonsenseIsClampedNotTrusted() {
        let g = TerminalScrollGeometry(lineHeight: 0, firstRow: 10, endRow: 3, viewportHeight: -5)
        XCTAssertEqual(g.lineHeight, 1)
        XCTAssertEqual(g.endRow, 10)
        XCTAssertEqual(g.viewportHeight, 0)
        XCTAssertTrue(g.visibleRows(offset: 0).isEmpty)
    }
}
