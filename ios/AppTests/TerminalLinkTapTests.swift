import XCTest
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// Links on the scroll surface: underlined where they are, opened by a tap through the opener the surface is given, and nothing else
/// about a tap changed.
@MainActor final class TerminalLinkTapTests: XCTestCase {
    private let size = CGSize(width: 393, height: 700)
    private let padding = 4.0
    private var cell: (width: Double, height: Double) { TerminalFont.cell(size: 12) }

    private func line(_ text: String) -> StyledLine {
        StyledLine(text: text, runs: [StyleRun(length: text.count, style: .plain)], columns: text.reduce(0) { $0 + TerminalText.cellWidth($1) })
    }
    /// A buffer whose last lines are `tail`, under 200 filler lines; the screen is the last 12.
    private func buffer(_ tail: [String]) -> TerminalBuffer {
        let lines = (0..<200).map { line("L\($0)") } + tail.map(line)
        var buffer = TerminalBuffer()
        buffer.applyLive(LiveTail(lines: lines, historyLines: lines.count - 12, historySize: lines.count - 12, cursorLine: lines.count - 1, cursorColumn: 0))
        return buffer
    }
    private struct Host { let window: UIWindow, surface: TerminalSurfaceView, source: StubSource }
    private func host(_ buffer: TerminalBuffer, columns: Int? = nil) throws -> Host {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene") }
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(origin: .zero, size: size)
        window.rootViewController = UIViewController()
        window.makeKeyAndVisible()
        let source = StubSource()
        source.buffer = buffer
        source.columns = columns
        let look = TerminalSurfaceView.Look(settings: TerminalRenderer.Settings(style: .builtIn, dark: false, showCursor: true, committedSize: 12, boldIsBright: false),
                                            fontSize: 12, padding: padding, floatingInset: 0, headerSize: 10)
        let surface = TerminalSurfaceView(look: look)
        surface.source = source
        surface.frame = CGRect(origin: .zero, size: size)
        window.rootViewController?.view.addSubview(surface)
        surface.layoutIfNeeded()
        return Host(window: window, surface: surface, source: source)
    }
    /// The middle of a cell, in the scroll view's own space.
    private func point(row: Int, column: Int) -> CGPoint {
        CGPoint(x: padding + (Double(column) + 0.5) * cell.width, y: (Double(row) + 0.5) * cell.height)
    }
    /// A whole tap: the touch coming down, then the tap ending.
    private func tap(_ surface: TerminalSurfaceView, row: Int, column: Int) {
        surface.touchBegan(at: point(row: row, column: column))
        surface.tap(at: point(row: row, column: column))
    }

    func testATapOnALinkOpensItAndATapBesideItDoesNot() throws {
        let h = try host(buffer(["see https://example.com/docs. for more", "$ "]))
        var opened: [URL] = []
        var touches: [Bool] = []
        h.surface.onOpenLink = { opened.append($0) }
        h.surface.onLinkTouch = { touches.append($0) }
        let row = 200
        let shown = try XCTUnwrap(h.surface.shownRows[row])
        XCTAssertEqual(shown.links, [4..<28], "the link is underlined, the full stop after it is not")

        tap(h.surface, row: row, column: 10)
        XCTAssertEqual(opened, [URL(string: "https://example.com/docs")!])
        tap(h.surface, row: row, column: 33)
        tap(h.surface, row: row + 1, column: 0)
        XCTAssertEqual(opened.count, 1, "a tap on plain text opens nothing")
        XCTAssertEqual(touches, [true, false, false], "the keyboard tap is told which touches were on a link")
        // A tap that does not start on a link (a drag that ended over one) opens nothing.
        h.surface.touchBegan(at: point(row: row, column: 33))
        h.surface.tap(at: point(row: row, column: 10))
        XCTAssertEqual(opened.count, 1)
    }

    func testAURLWrappedOverTwoRowsOpensWholeFromEitherRow() throws {
        // A pane 20 cells wide: the first row is full, so it continues on the next.
        let h = try host(buffer(["a https://example.co", "m/wrapped/page now", "$ "]), columns: 20)
        var opened: [URL] = []
        h.surface.onOpenLink = { opened.append($0) }
        XCTAssertEqual(h.surface.shownRows[200]?.links, [2..<20])
        XCTAssertEqual(h.surface.shownRows[201]?.links, [0..<14])
        tap(h.surface, row: 200, column: 5)
        tap(h.surface, row: 201, column: 2)
        XCTAssertEqual(opened, Array(repeating: URL(string: "https://example.com/wrapped/page")!, count: 2))
    }

    func testWithoutThePaneWidthARowIsReadAlone() throws {
        let h = try host(buffer(["a https://example.co", "m/wrapped/page now"]))
        var opened: [URL] = []
        h.surface.onOpenLink = { opened.append($0) }
        tap(h.surface, row: 200, column: 5)
        XCTAssertEqual(opened, [URL(string: "https://example.co")!])
        XCTAssertEqual(h.surface.shownRows[201]?.links, [])
    }

    func testLinksFollowNewOutputAndVoiceOverCanOpenThem() throws {
        let h = try host(buffer(["plain", "$ "]))
        var opened: [URL] = []
        h.surface.onOpenLink = { opened.append($0) }
        XCTAssertEqual(h.surface.shownRows[200]?.links, [])
        XCTAssertNil(h.surface.accessibilityCustomActions)
        h.source.buffer = buffer(["mail mailto:dev@example.com", "$ "])
        h.surface.refresh()
        XCTAssertEqual(h.surface.shownRows[200]?.links, [5..<27], "a new answer is read again")
        let action = try XCTUnwrap(h.surface.accessibilityCustomActions?.first)
        XCTAssertEqual(action.name, "Open mailto:dev@example.com")
        _ = action.actionHandler?(action)
        XCTAssertEqual(opened, [URL(string: "mailto:dev@example.com")!])
    }
}
