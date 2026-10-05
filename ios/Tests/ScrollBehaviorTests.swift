import XCTest
@testable import RiWorkCore

final class StickyBottomTests: XCTestCase {
    private let line = 14.0
    /// A view 600 pt tall over `content` pt of text, scrolled `away` pt from the bottom.
    private func metrics(content: Double = 10_000, away: Double, viewport: Double = 600) -> ScrollMetrics {
        ScrollMetrics(offset: content - viewport - away, contentHeight: content, viewportHeight: viewport)
    }

    func testItStartsFollowingWithNoPill() {
        let sticky = StickyBottom()
        XCTAssertTrue(sticky.following)
        XCTAssertNil(sticky.pill)
        XCTAssertEqual(sticky.newLines, 0)
    }
    func testMetricsGiveDistancesFromTheEnds() {
        // The view is 650 tall; 20 of it is inset at the top and 30 at the bottom, so 600 is visible, starting 120 into the content.
        let m = ScrollMetrics(offset: 100, contentHeight: 2000, viewportHeight: 600, topInset: 20, bottomInset: 30)
        XCTAssertEqual(m.distanceFromBottom, 2000 - (100 + 20 + 600))
        XCTAssertEqual(m.distanceFromTop, 120)
        // At the very bottom the offset is content + bottom inset - view height.
        XCTAssertEqual(ScrollMetrics(offset: 2000 + 30 - 650, contentHeight: 2000, viewportHeight: 600, topInset: 20, bottomInset: 30).distanceFromBottom, 0)
        XCTAssertEqual(ScrollMetrics(offset: -20, contentHeight: 2000, viewportHeight: 600, topInset: 20, bottomInset: 30).distanceFromTop, 0)
        XCTAssertEqual(ScrollMetrics(offset: 1400, contentHeight: 2000, viewportHeight: 600).distanceFromBottom, 0)
        XCTAssertLessThan(ScrollMetrics(offset: 1460, contentHeight: 2000, viewportHeight: 600).distanceFromBottom, 0, "overscrolled past the bottom")
    }
    func testWithinOneLineOfTheBottomStillFollows() {
        var sticky = StickyBottom()
        let old = metrics(away: 0)
        XCTAssertEqual(sticky.metricsChanged(from: old, to: metrics(away: 13), lineHeight: line, userDriven: true), .none)
        XCTAssertTrue(sticky.following, "13 pt is less than a line")
        XCTAssertEqual(sticky.metricsChanged(from: old, to: metrics(away: 14), lineHeight: line, userDriven: true), .none)
        XCTAssertTrue(sticky.following, "exactly one line")
        _ = sticky.metricsChanged(from: old, to: metrics(away: 15), lineHeight: line, userDriven: true)
        XCTAssertFalse(sticky.following, "more than a line")
    }
    func testScrollingUpStopsFollowingAndCountsNewLines() {
        var sticky = StickyBottom()
        sticky.contentChanged(end: 100, epoch: 1)
        _ = sticky.metricsChanged(from: metrics(away: 0), to: metrics(away: 200), lineHeight: line, userDriven: true)
        XCTAssertFalse(sticky.following)
        XCTAssertNil(sticky.pill, "nothing new and not far away: no pill")
        sticky.contentChanged(end: 103, epoch: 1)
        XCTAssertEqual(sticky.newLines, 3)
        XCTAssertEqual(sticky.pill?.label, "↓ Live · 3 new")
        sticky.contentChanged(end: 104, epoch: 1)
        XCTAssertEqual(sticky.pill?.label, "↓ Live · 4 new")
        XCTAssertEqual(sticky.pill?.accessibilityLabel, "Jump to latest output, 4 new lines")
        sticky.contentChanged(end: 104, epoch: 1)
        XCTAssertEqual(sticky.newLines, 4, "redraws that add no lines add none")
        sticky.contentChanged(end: 100, epoch: 1)
        sticky.contentChanged(end: 101, epoch: 1)
        XCTAssertEqual(sticky.newLines, 5, "the screen was cleared and a line written: only growth counts")
    }
    func testFollowingCountsNothing() {
        var sticky = StickyBottom()
        sticky.contentChanged(end: 10, epoch: 1)
        sticky.contentChanged(end: 50, epoch: 1)
        XCTAssertEqual(sticky.newLines, 0)
        XCTAssertNil(sticky.pill)
    }
    func testReachingTheBottomAgainResumesFollowing() {
        var sticky = StickyBottom()
        sticky.contentChanged(end: 100, epoch: 1)
        _ = sticky.metricsChanged(from: metrics(away: 0), to: metrics(away: 300), lineHeight: line, userDriven: true)
        sticky.contentChanged(end: 120, epoch: 1)
        XCTAssertNotNil(sticky.pill)
        _ = sticky.metricsChanged(from: metrics(away: 300), to: metrics(away: 6), lineHeight: line, userDriven: true)
        XCTAssertTrue(sticky.following)
        XCTAssertEqual(sticky.newLines, 0)
        XCTAssertNil(sticky.pill)
    }
    func testTheBottomMovingAwayFromAFollowingViewScrollsItDown() {
        var sticky = StickyBottom()
        // Ten lines of output grew the content; the view has not followed yet.
        let grown = ScrollMetrics(offset: 9400, contentHeight: 10_140, viewportHeight: 600)
        XCTAssertEqual(sticky.metricsChanged(from: metrics(away: 0), to: grown, lineHeight: line, userDriven: false), .scrollToBottom)
        XCTAssertTrue(sticky.following)
        // The same growth while the user has scrolled up does not move them.
        var reading = StickyBottom()
        _ = reading.metricsChanged(from: metrics(away: 0), to: metrics(away: 500), lineHeight: line, userDriven: true)
        let readingGrown = ScrollMetrics(offset: metrics(away: 500).offset, contentHeight: 10_140, viewportHeight: 600)
        XCTAssertEqual(reading.metricsChanged(from: metrics(away: 500), to: readingGrown, lineHeight: line, userDriven: false), .none)
        XCTAssertFalse(reading.following)
    }
    func testAFingerRestingOnTheViewWhileOutputArrivesDoesNotEndFollowing() {
        var sticky = StickyBottom()
        // A tap or a hold: the phase says a finger is down, but the offset did not move; the content grew under it.
        let grown = ScrollMetrics(offset: metrics(away: 0).offset, contentHeight: 10_140, viewportHeight: 600)
        XCTAssertEqual(sticky.metricsChanged(from: metrics(away: 0), to: grown, lineHeight: line, userDriven: true), .scrollToBottom)
        XCTAssertTrue(sticky.following)
        // The moment the finger drags it away, following ends.
        let dragged = ScrollMetrics(offset: grown.offset - 300, contentHeight: 10_140, viewportHeight: 600)
        XCTAssertEqual(sticky.metricsChanged(from: grown, to: dragged, lineHeight: line, userDriven: true), .none)
        XCTAssertFalse(sticky.following)
        // Nothing that changed nothing starts a scroll.
        var idle = StickyBottom()
        XCTAssertEqual(idle.metricsChanged(from: metrics(away: 0), to: metrics(away: 0), lineHeight: line, userDriven: false), .none)
    }
    func testAShrinkingViewKeepsTheBottomWhileFollowing() {
        var sticky = StickyBottom()
        // The keyboard came up: the view is shorter, the offset unchanged, so the bottom is out of view.
        let old = metrics(away: 0, viewport: 600)
        let shorter = ScrollMetrics(offset: old.offset, contentHeight: old.contentHeight, viewportHeight: 350)
        XCTAssertEqual(sticky.metricsChanged(from: old, to: shorter, lineHeight: line, userDriven: false), .scrollToBottom)
    }
    func testAnOffsetOnlyChangeAwayFromTheBottomIsTheUserLeaving() {
        var sticky = StickyBottom()
        // A scroll that changed nothing else, not flagged as user-driven (a scroll to a position): not at the bottom, not following.
        _ = sticky.metricsChanged(from: metrics(away: 0), to: metrics(away: 900), lineHeight: line, userDriven: false)
        XCTAssertFalse(sticky.following)
    }
    func testBeingFarAwayShowsThePillWithoutNewLines() {
        var sticky = StickyBottom()
        _ = sticky.metricsChanged(from: metrics(away: 0), to: metrics(away: 5000), lineHeight: line, userDriven: true)
        XCTAssertTrue(sticky.far)
        XCTAssertEqual(sticky.pill?.label, "↓ Live", "a long way from the bottom, the pill is the way back even when nothing new came")
        _ = sticky.metricsChanged(from: metrics(away: 5000), to: metrics(away: 300), lineHeight: line, userDriven: true)
        XCTAssertNil(sticky.pill)
    }
    func testTypingOrTappingThePillJumpsToTheBottom() {
        var sticky = StickyBottom()
        sticky.contentChanged(end: 10, epoch: 1)
        _ = sticky.metricsChanged(from: metrics(away: 0), to: metrics(away: 3000), lineHeight: line, userDriven: true)
        sticky.contentChanged(end: 40, epoch: 1)
        XCTAssertEqual(sticky.newLines, 30)
        sticky.jumpToBottom()
        XCTAssertTrue(sticky.following)
        XCTAssertEqual(sticky.newLines, 0)
        XCTAssertNil(sticky.pill)
        sticky.contentChanged(end: 45, epoch: 1)
        XCTAssertEqual(sticky.newLines, 0)
    }
    func testANewNumberingDoesNotCountAsNewLines() {
        var sticky = StickyBottom()
        sticky.contentChanged(end: 100, epoch: 1)
        _ = sticky.metricsChanged(from: metrics(away: 0), to: metrics(away: 300), lineHeight: line, userDriven: true)
        sticky.contentChanged(end: 5000, epoch: 2)
        XCTAssertEqual(sticky.newLines, 0)
        sticky.contentChanged(end: 5002, epoch: 2)
        XCTAssertEqual(sticky.newLines, 2)
    }
    func testOpeningOrSwitchingAShellStartsAtTheBottom() {
        var sticky = StickyBottom()
        sticky.contentChanged(end: 10, epoch: 1)
        _ = sticky.metricsChanged(from: metrics(away: 0), to: metrics(away: 300), lineHeight: line, userDriven: true)
        sticky.contentChanged(end: 20, epoch: 1)
        sticky.reset()
        XCTAssertEqual(sticky, StickyBottom())
    }
}

final class AlternateRowsTests: XCTestCase {
    func testWholeRowsFitAndAtLeastOne() {
        XCTAssertEqual(AlternateRows.fitting(height: 340, lineHeight: 17), 20)
        XCTAssertEqual(AlternateRows.fitting(height: 339.9, lineHeight: 17), 19)
        XCTAssertEqual(AlternateRows.fitting(height: 5, lineHeight: 17), 1)
        XCTAssertEqual(AlternateRows.fitting(height: -20, lineHeight: 17), 1)
        XCTAssertEqual(AlternateRows.fitting(height: .infinity, lineHeight: 17), 1)
    }

    func testAScreenThatFitsIsDrawnWhole() {
        XCTAssertEqual(AlternateRows.shown(count: 24, fitting: 24, cursor: 23), 0..<24)
        XCTAssertEqual(AlternateRows.shown(count: 10, fitting: 24, cursor: nil), 0..<10)
        XCTAssertEqual(AlternateRows.shown(count: 0, fitting: 24, cursor: nil), 0..<0)
    }

    func testATallerScreenKeepsTheCursorInViewMovingAsFewRowsOffTheTopAsPossible() {
        // The keyboard came up: 44 rows from the desktop, room for 24. A prompt at the bottom stays in view.
        XCTAssertEqual(AlternateRows.shown(count: 44, fitting: 24, cursor: 43), 20..<44)
        XCTAssertEqual(AlternateRows.shown(count: 44, fitting: 24, cursor: 30), 7..<31)
        // A cursor that is in view from the top leaves the top where it is (vim's first line).
        XCTAssertEqual(AlternateRows.shown(count: 44, fitting: 24, cursor: 2), 0..<24)
        XCTAssertEqual(AlternateRows.shown(count: 44, fitting: 24, cursor: 99), 20..<44)
        // No cursor: the bottom rows, where programs keep their status and prompt.
        XCTAssertEqual(AlternateRows.shown(count: 44, fitting: 24, cursor: nil), 20..<44)
    }
}

final class SwipePagerTests: XCTestCase {
    func testADragOfEightyPercentOfTheViewSendsOnePageKey() {
        var pager = SwipePager()
        XCTAssertNil(pager.update(translation: 100, viewHeight: 500, now: 0))
        XCTAssertNil(pager.update(translation: 399, viewHeight: 500, now: 0.01))
        XCTAssertEqual(pager.update(translation: 400, viewHeight: 500, now: 0.02), .pageUp, "dragging the content down is Page Up")
        XCTAssertNil(pager.update(translation: 450, viewHeight: 500, now: 0.5), "the next page needs another 400 pt")
    }
    func testDraggingUpIsPageDown() {
        var pager = SwipePager()
        XCTAssertEqual(pager.update(translation: -420, viewHeight: 500, now: 0), .pageDown)
    }
    func testOneKeyPerPageOfDragInTheSameDirection() {
        var pager = SwipePager()
        var keys: [TerminalKey] = []
        var now = 0.0
        for translation in stride(from: 0.0, through: 1300, by: 20) {
            now += 0.05
            if let key = pager.update(translation: translation, viewHeight: 500, now: now) { keys.append(key) }
        }
        XCTAssertEqual(keys, [.pageUp, .pageUp, .pageUp], "1300 pt is three pages of 400")
    }
    func testKeysAreRateLimitedAndABurstIsNotQueuedWithoutBound() {
        var pager = SwipePager(minimumInterval: 0.12)
        XCTAssertEqual(pager.update(translation: 400, viewHeight: 500, now: 1.00), .pageUp)
        // A fast swipe covers 3000 pt within 50 ms: nothing more is sent before the interval has passed.
        XCTAssertNil(pager.update(translation: 3400, viewHeight: 500, now: 1.05))
        XCTAssertNil(pager.update(translation: 3400, viewHeight: 500, now: 1.11))
        XCTAssertEqual(pager.update(translation: 3400, viewHeight: 500, now: 1.13), .pageUp)
        XCTAssertEqual(pager.update(translation: 3400, viewHeight: 500, now: 1.26), .pageUp, "at most two pages of backlog were kept")
        XCTAssertNil(pager.update(translation: 3400, viewHeight: 500, now: 1.40), "and then it is over")
        XCTAssertNil(pager.update(translation: 3400, viewHeight: 500, now: 2.00))
    }
    func testReversingTheDragPagesTheOtherWay() {
        var pager = SwipePager()
        XCTAssertEqual(pager.update(translation: 400, viewHeight: 500, now: 0), .pageUp)
        XCTAssertNil(pager.update(translation: 300, viewHeight: 500, now: 1))
        XCTAssertEqual(pager.update(translation: -20, viewHeight: 500, now: 2), .pageDown, "420 pt back")
    }
    func testANewDragStartsFromNothing() {
        var pager = SwipePager()
        XCTAssertEqual(pager.update(translation: 450, viewHeight: 500, now: 0), .pageUp)
        XCTAssertNil(pager.end(predictedTranslation: 450, viewHeight: 500, now: 0.05), "a key already went for this swipe")
        pager.begin()
        XCTAssertNil(pager.update(translation: 100, viewHeight: 500, now: 1))
        XCTAssertEqual(pager.update(translation: 410, viewHeight: 500, now: 1.1), .pageUp)
    }
    func testAQuickFlickThatWouldGoAPagePagesOnceWhenItEnds() {
        var pager = SwipePager()
        pager.begin()
        XCTAssertNil(pager.update(translation: 150, viewHeight: 500, now: 0))
        XCTAssertEqual(pager.end(predictedTranslation: 900, viewHeight: 500, now: 0.03), .pageUp, "the momentum would carry it two pages")
        pager.begin()
        XCTAssertNil(pager.update(translation: -100, viewHeight: 500, now: 1))
        XCTAssertNil(pager.end(predictedTranslation: -250, viewHeight: 500, now: 1.02), "a short one does nothing")
        pager.begin()
        XCTAssertEqual(pager.end(predictedTranslation: -700, viewHeight: 500, now: 2), .pageDown)
        pager.begin()
        XCTAssertNil(pager.end(predictedTranslation: -700, viewHeight: 500, now: 2.05), "and the rate limit holds for flicks too")
        XCTAssertNil(pager.end(predictedTranslation: .nan, viewHeight: 500, now: 9))
    }
    func testTheDistanceFollowsTheViewHeight() {
        var tall = SwipePager(), short = SwipePager()
        XCTAssertNil(tall.update(translation: 500, viewHeight: 800, now: 0), "640 pt needed")
        XCTAssertEqual(short.update(translation: 500, viewHeight: 600, now: 0), .pageUp, "480 pt needed")
    }
    func testNonsenseNumbersSendNothing() {
        var pager = SwipePager()
        XCTAssertNil(pager.update(translation: .nan, viewHeight: 500, now: 0))
        XCTAssertNil(pager.update(translation: 900, viewHeight: 0, now: 0))
        XCTAssertNil(pager.update(translation: .infinity, viewHeight: 500, now: 0))
    }
}
