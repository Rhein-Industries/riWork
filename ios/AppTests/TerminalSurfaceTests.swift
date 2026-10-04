import XCTest
import UIKit
import RiWorkCore
@testable import RiWorkRemote

@MainActor final class StubSource: TerminalLineSource {
    var buffer = TerminalBuffer()
    var columns: Int?
    var terminalBuffer: TerminalBuffer { buffer }
    var terminalColumns: Int? { columns }
}

/// The scroll surface on its own, against a plain buffer: how rows are placed, recycled and kept in place.
@MainActor final class TerminalSurfaceTests: XCTestCase {
    private let size = CGSize(width: 393, height: 700)

    private func look(size: Double = 12, floating: Double = 0, dark: Bool = false) -> TerminalSurfaceView.Look {
        TerminalSurfaceView.Look(settings: TerminalRenderer.Settings(style: .builtIn, dark: dark, showCursor: true, committedSize: size, boldIsBright: false),
                                 fontSize: size, padding: 4, floatingInset: floating, headerSize: 10)
    }
    /// A buffer of lines `L<index>` numbered from `start` to `end`, the screen being the last 12 of them.
    private func buffer(start: Int, end: Int) -> TerminalBuffer {
        let rows = 12
        let lines = (start..<end).map { StyledLine(text: "L\($0)", runs: [StyleRun(length: "L\($0)".count, style: .plain)], columns: "L\($0)".count) }
        var buffer = TerminalBuffer()
        buffer.applyLive(LiveTail(lines: lines, historyLines: lines.count - rows, historySize: end - rows, cursorLine: lines.count - 1, cursorColumn: 0))
        return buffer
    }
    private struct Host { let window: UIWindow, surface: TerminalSurfaceView, source: StubSource }
    private func host(_ buffer: TerminalBuffer, header: HistoryHeader? = nil, following: @escaping () -> Bool = { true }) throws -> Host {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene") }
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(origin: .zero, size: size)
        window.rootViewController = UIViewController()
        window.makeKeyAndVisible()
        let source = StubSource()
        source.buffer = buffer
        let surface = TerminalSurfaceView(look: look())
        surface.source = source
        surface.header = header
        surface.isFollowing = following
        surface.frame = CGRect(origin: .zero, size: size)
        window.rootViewController?.view.addSubview(surface)
        surface.layoutIfNeeded()
        return Host(window: window, surface: surface, source: source)
    }
    private func texts(_ surface: TerminalSurfaceView) -> [Int: String] { surface.shownRows.compactMapValues { $0.line?.text } }
    private var lineHeight: Double { TerminalFont.cell(size: 12).height }

    func testRowsAreOnlyThoseInViewAtTheirFixedPlaceAndTheViewOpensAtTheBottom() throws {
        let h = try host(buffer(start: 1000, end: 3000))
        let g = h.surface.geometry
        XCTAssertEqual(h.surface.scrollView.contentOffset.y, g.maxOffset, accuracy: 0.001)
        let shown = texts(h.surface)
        XCTAssertEqual(Set(shown.keys), Set(g.visibleRows(offset: g.maxOffset)))
        XCTAssertEqual(shown[2999], "L2999")
        XCTAssertLessThan(shown.count, 60)
        for (index, row) in h.surface.shownRows {
            // In the view: the place of the line in the content, minus the offset (the canvas shows it relative to the view).
            let place = Double(row.frame.minY) - Double(h.surface.subviews[0].bounds.origin.y)
            XCTAssertEqual(place, g.y(ofRow: index, offset: g.maxOffset), accuracy: 0.001)
        }
        XCTAssertEqual(h.surface.scrollView.contentSize.height, 3000 * lineHeight, accuracy: 0.001)
        XCTAssertEqual(h.surface.scrollView.contentInset.top, 4 - 1000 * lineHeight, accuracy: 0.001, "the first line is the top of what can be scrolled to")
    }

    func testRowsAreRecycledNotPiledUpWhileScrollingThroughThousands() throws {
        let h = try host(buffer(start: 0, end: 6000))
        let canvas = h.surface.subviews[0]
        var most = 0
        for step in stride(from: h.surface.geometry.maxOffset, to: 0, by: -97.3) {
            h.surface.scrollView.contentOffset = CGPoint(x: 0, y: step)
            most = max(most, canvas.subviews.count)
        }
        XCTAssertLessThan(most, 90, "views are reused: at most the rows in view, a few spares, and the header and highlight")
        XCTAssertLessThan(h.surface.shownRows.count, 60)
        for (index, row) in h.surface.shownRows { XCTAssertEqual(row.line?.text, "L\(index)") }
    }

    func testAnOffsetIsNeverFarFromTheCanvasOriginSoThePixelsStayExact() throws {
        let h = try host(buffer(start: 0, end: 60_000))
        let canvas = h.surface.subviews[0]
        for offset in [h.surface.geometry.maxOffset, 400_000.5, 123_456.25, 14.5, 300_000.0] {
            h.surface.scrollView.contentOffset = CGPoint(x: 0, y: offset)
            XCTAssertLessThan(abs(canvas.bounds.origin.y), 1600, "rows are positioned in a small coordinate space")
            for row in h.surface.shownRows.values { XCTAssertLessThan(abs(row.frame.minY), 3000) }
        }
    }

    func testLinesAddedAboveAndBelowLeaveEveryRowWhereItWas() throws {
        var following = true
        let h = try host(buffer(start: 2000, end: 3000), following: { following })
        let g = h.surface.geometry
        following = false
        h.surface.scrollView.contentOffset = CGPoint(x: 0, y: g.minOffset + 400 * lineHeight)
        let before = (offset: h.surface.scrollView.contentOffset.y, rows: h.surface.shownRows.mapValues { ($0.frame.minY, $0.line?.text) }, canvas: h.surface.subviews[0].bounds.origin.y)
        h.source.buffer = buffer(start: 1500, end: 3040)   // a page above, output below
        h.surface.refresh()
        XCTAssertEqual(h.surface.scrollView.contentOffset.y, before.offset, "not by a point")
        XCTAssertEqual(h.surface.subviews[0].bounds.origin.y, before.canvas, accuracy: 0.001)
        for (index, was) in before.rows {
            let row = try XCTUnwrap(h.surface.shownRows[index])
            XCTAssertEqual(row.frame.minY, was.0, accuracy: 0.001)
            XCTAssertEqual(row.line?.text, was.1)
        }
    }

    func testAnOverscrolledViewIsNotPulledBackByARefresh() throws {
        var following = true
        let h = try host(buffer(start: 2000, end: 3000), following: { following })
        following = false
        let wall = h.surface.geometry.minOffset
        h.surface.scrollView.contentOffset = CGPoint(x: 0, y: wall - 60)   // rubber-banding past the first line
        h.surface.refresh()
        h.surface.setNeedsLayout(); h.surface.layoutIfNeeded()
        XCTAssertEqual(h.surface.scrollView.contentOffset.y, wall - 60, accuracy: 0.001, "the bounce is UIKit's to finish")
    }

    func testAWallThatMovesDownPastTheReaderPushesThemAndOnlyThen() throws {
        var following = true
        let h = try host(buffer(start: 1000, end: 3000), following: { following })
        following = false
        let near = h.surface.geometry.minOffset + 10 * lineHeight
        h.surface.scrollView.contentOffset = CGPoint(x: 0, y: near)
        // The oldest 500 lines go (the cap): the reader was reading line 1010.
        h.source.buffer = buffer(start: 1500, end: 3000)
        h.surface.refresh()
        XCTAssertEqual(h.surface.scrollView.contentOffset.y, h.surface.geometry.minOffset, accuracy: 0.001, "the first line that is left")
        // Further down, nobody is moved.
        let far = h.surface.geometry.minOffset + 900 * lineHeight
        h.surface.scrollView.contentOffset = CGPoint(x: 0, y: far)
        h.source.buffer = buffer(start: 1600, end: 3000)
        h.surface.refresh()
        XCTAssertEqual(h.surface.scrollView.contentOffset.y, far, accuracy: 0.001)
    }

    func testFollowingTheBottomSurvivesNewLinesAKeyboardAndATextSizeChange() throws {
        let h = try host(buffer(start: 1000, end: 3000))
        h.source.buffer = buffer(start: 1000, end: 3010)
        h.surface.refresh()
        XCTAssertEqual(h.surface.scrollView.contentOffset.y, h.surface.geometry.maxOffset, accuracy: 0.001)
        h.surface.frame = CGRect(origin: .zero, size: CGSize(width: size.width, height: 400))   // the keyboard
        h.surface.layoutIfNeeded()
        XCTAssertEqual(h.surface.scrollView.contentOffset.y, h.surface.geometry.maxOffset, accuracy: 0.001)
        h.surface.configure(look(size: 16))
        h.surface.layoutIfNeeded()
        XCTAssertEqual(h.surface.scrollView.contentOffset.y, h.surface.geometry.maxOffset, accuracy: 0.001)
        XCTAssertEqual(h.surface.geometry.lineHeight, TerminalFont.cell(size: 16).height)
        XCTAssertTrue(h.surface.shownRows.values.allSatisfy { Double($0.frame.height) == TerminalFont.cell(size: 16).height })
    }

    func testRenumberedLinesKeepTheReaderTheSameDistanceFromTheBottom() throws {
        var following = true
        let h = try host(buffer(start: 5000, end: 7000), following: { following })
        following = false
        h.surface.scrollView.contentOffset = CGPoint(x: 0, y: h.surface.geometry.maxOffset - 640)
        var renumbered = buffer(start: 100, end: 1500)
        // A different numbering is a different epoch.
        renumbered.reset(); renumbered.applyLive(LiveTail(lines: (100..<1500).map { StyledLine(text: "W\($0)", runs: [], columns: 4) }, historyLines: 1388, historySize: 1488))
        h.source.buffer = renumbered
        h.surface.refresh()
        XCTAssertEqual(h.surface.geometry.distanceFromBottom(offset: h.surface.scrollView.contentOffset.y), 640, accuracy: 0.001)
    }

    func testTheCursorRowAndALookChangeRepaintOnlyWhatShows() throws {
        let h = try host(buffer(start: 0, end: 500))
        let last = try XCTUnwrap(h.surface.shownRows[499])
        XCTAssertEqual(last.cursorColumn, 0, "the cursor cell is drawn on the last line")
        XCTAssertNil(h.surface.shownRows[498]?.cursorColumn)
        let before = h.surface.shownRows.values.map(\.look)
        h.surface.configure(look(dark: true))
        h.surface.layoutIfNeeded()
        XCTAssertTrue(h.surface.shownRows.values.allSatisfy { $0.look == (before.first ?? 0) + 1 }, "every visible row is painted again in the new colors")
    }

    func testTheHeaderRowIsTheFirstRowAndSaysWhatIsHappening() throws {
        let h = try host(buffer(start: 1000, end: 3000), header: HistoryHeader(text: "Loading…", failed: false))
        XCTAssertEqual(h.surface.geometry.firstRow, 999)
        h.surface.scrollView.contentOffset = CGPoint(x: 0, y: h.surface.geometry.minOffset)
        XCTAssertTrue(h.surface.headerShown)
        XCTAssertEqual(h.surface.headerRowText, "Loading…")
        XCTAssertNil(h.surface.shownRows[999], "the header slot is not a line")
        XCTAssertEqual(h.surface.shownRows[1000]?.line?.text, "L1000")
        h.surface.header = nil
        h.surface.layoutIfNeeded()
        XCTAssertEqual(h.surface.geometry.firstRow, 1000)
        XCTAssertFalse(h.surface.headerShown)
    }

    func testTheReaderAndTheFollowLogicAreToldWhereTheViewIs() throws {
        let h = try host(buffer(start: 1000, end: 3000))
        var readers: [(Int, Int)] = []
        var metrics: [(Bool)] = []
        h.surface.onReader = { readers.append(($0, $1)) }
        h.surface.onMetrics = { _, _, lineHeight, moving in
            XCTAssertEqual(lineHeight, self.lineHeight)
            metrics.append(moving)
            return .none
        }
        h.surface.scrollView.contentOffset = CGPoint(x: 0, y: Double(2000) * lineHeight)
        let last = try XCTUnwrap(readers.last)
        XCTAssertEqual(last.0, 2000)
        XCTAssertEqual(last.1, Int(size.height / lineHeight))
        XCTAssertEqual(metrics.last, false, "a programmatic move is not a finger")
        // A reader above the first line held is reported as at that line (the header's slot is not a line).
        h.surface.scrollView.contentOffset = CGPoint(x: 0, y: h.surface.geometry.minOffset)
        XCTAssertEqual(readers.last?.0, 1000)
    }

    func testAMissingLineIsABlankRowAndIsNotCopyable() throws {
        var source = buffer(start: 1000, end: 2000)
        // Output bursts past what an answer reaches back: the lines between are placeholders.
        let tail = LiveTail(lines: (2600..<2700).map { StyledLine(text: "L\($0)", runs: [], columns: 5) }, historyLines: 88, historySize: 2688)
        source.applyLive(tail)
        XCTAssertFalse(source.holes.isEmpty)
        let h = try host(source)
        var following = false
        h.surface.isFollowing = { following }
        following = false
        let hole = try XCTUnwrap(source.holes.first)
        h.surface.scrollView.contentOffset = CGPoint(x: 0, y: Double(hole.lowerBound + 5) * lineHeight)
        let row = try XCTUnwrap(h.surface.shownRows[hole.lowerBound + 5])
        XCTAssertTrue(row.line?.isMissing == true)
        XCTAssertEqual(row.line?.text, "")
    }

    func testAJumpRequestGoesToTheBottomEvenWhenNotFollowing() throws {
        var following = true
        let h = try host(buffer(start: 1000, end: 3000), following: { following })
        following = false
        h.surface.scrollView.contentOffset = CGPoint(x: 0, y: h.surface.geometry.minOffset + 3000)
        h.surface.jumpToken = 1
        h.surface.layoutIfNeeded()
        XCTAssertEqual(h.surface.scrollView.contentOffset.y, h.surface.geometry.maxOffset, accuracy: 0.001)
    }

    func testVoiceOverCanScrollAPageAtATimeAndRetryAFailedHeader() throws {
        var following = true
        let h = try host(buffer(start: 1000, end: 3000), header: HistoryHeader(text: "Couldn't load older lines · tap to retry", failed: true), following: { following })
        following = false
        var retried = 0
        h.surface.onRetry = { retried += 1 }
        let bottom = h.surface.scrollView.contentOffset.y
        XCTAssertTrue(h.surface.accessibilityScroll(.down), "toward older lines")
        let page = bottom - h.surface.scrollView.contentOffset.y
        XCTAssertGreaterThan(page, size.height / 2)
        XCTAssertLessThan(page, size.height)
        XCTAssertTrue(h.surface.accessibilityScroll(.up))
        XCTAssertEqual(h.surface.scrollView.contentOffset.y, bottom, accuracy: 0.001)
        XCTAssertFalse(h.surface.accessibilityScroll(.left))
        let action = try XCTUnwrap(h.surface.accessibilityCustomActions?.first)
        XCTAssertEqual(action.name, "Retry loading older lines")
        _ = action.actionHandler?(action)
        XCTAssertEqual(retried, 1)
        h.surface.header = HistoryHeader(text: "Loading…", failed: false)
        XCTAssertNil(h.surface.accessibilityCustomActions, "only while there is something to retry")
    }
}
