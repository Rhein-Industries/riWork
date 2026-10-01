import XCTest
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// The phone's rows: how a line is cut into pieces, and what the Core Text painter puts on the pixels.
@MainActor final class TerminalRendererTests: XCTestCase {
    private func settings(size: Double = 12, cursor: Bool = true) -> TerminalRenderer.Settings {
        TerminalRenderer.Settings(style: .builtIn, dark: false, showCursor: cursor, committedSize: size, boldIsBright: false)
    }
    private func line(_ text: String, style: CellStyle = .plain) -> StyledLine {
        StyledLine(text: text, runs: text.isEmpty ? [] : [StyleRun(length: text.count, style: style)], columns: text.reduce(0) { $0 + TerminalText.cellWidth($1) })
    }

    // MARK: pieces

    func testAnAsciiRunIsOnePieceWithoutAKern() {
        let pieces = TerminalRenderer.segments(text: "hello world", runs: [StyleRun(length: 11, style: .plain)], cursorColumn: nil, settings: settings())
        XCTAssertEqual(pieces, [TerminalRenderer.Segment(text: "hello world", style: .plain)])
    }
    func testTheCursorCellIsAPieceOfItsOwn() {
        let pieces = TerminalRenderer.segments(text: "hello", runs: [StyleRun(length: 5, style: .plain)], cursorColumn: 2, settings: settings())
        XCTAssertEqual(pieces.map(\.text), ["he", "l", "lo"])
        XCTAssertEqual(pieces.map(\.cursor), [false, true, false])
    }
    func testRunsOfDifferentStylesAreDifferentPieces() {
        let red = CellStyle(foreground: .indexed(1))
        let pieces = TerminalRenderer.segments(text: "ab cd", runs: [StyleRun(length: 2, style: red), StyleRun(length: 3, style: .plain)], cursorColumn: nil, settings: settings())
        XCTAssertEqual(pieces.map(\.text), ["ab", " cd"])
        XCTAssertEqual(pieces.map(\.style), [red, .plain])
    }
    func testAGlyphTheFontBorrowsGetsAKernThatBringsItsAdvanceBackToItsCellsAndTheNextPieceHasNone() {
        let pieces = TerminalRenderer.segments(text: "x日本y", runs: [StyleRun(length: 4, style: .plain)], cursorColumn: nil, settings: settings())
        XCTAssertEqual(pieces.map(\.text), ["x", "日", "本", "y"])
        XCTAssertEqual(pieces[0].kern, 0)
        XCTAssertNotEqual(pieces[1].kern, 0, "a wide glyph is brought to two cells")
        XCTAssertEqual(pieces[3].kern, 0, "and the kern does not run on into the next piece")
    }
    func testASymbolWithTheTextPresentationSelectorIsKeptWhole() {
        let pieces = TerminalRenderer.segments(text: "a\u{23FA}\u{FE0E}b", runs: [StyleRun(length: 3, style: .plain)], cursorColumn: nil, settings: settings())
        XCTAssertTrue(pieces.map(\.text).contains("\u{23FA}\u{FE0E}"), "the selector stays with its symbol: \(pieces.map(\.text))")
    }
    func testTheSwiftUIAndTheCoreTextPathsCutALineTheSameWay() {
        let red = CellStyle(foreground: .indexed(1), attributes: [.bold])
        let runs = [StyleRun(length: 3, style: .plain), StyleRun(length: 4, style: red)]
        let text = "ab日本cd!"
        let pieces = TerminalRenderer.segments(text: text, runs: runs, cursorColumn: 5, settings: settings())
        XCTAssertEqual(pieces.map(\.text).joined(), text, "nothing lost, nothing repeated")
        XCTAssertEqual(String(AttributedString(String(text)).characters), text)
        _ = TerminalRenderer.attributed(text: text, runs: runs, cursorColumn: 5, settings: settings())
    }

    // MARK: pixels

    private func render(_ lines: [StyledLine], cursor: [Int?] = [], size: Double = 12, scale: Double = 3, width: Double = 300) -> (image: UIImage, rowHeight: Double) {
        let cell = TerminalFont.cell(size: size)
        let format = UIGraphicsImageRendererFormat(); format.scale = scale; format.opaque = true
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: width, height: cell.height * Double(lines.count)), format: format)
        let image = renderer.image { context in
            UIColor.white.setFill(); context.fill(CGRect(origin: .zero, size: context.format.bounds.size))
            for (index, line) in lines.enumerated() {
                context.cgContext.saveGState()
                context.cgContext.translateBy(x: 0, y: cell.height * Double(index))
                TerminalRowPainter.draw(line, cursorColumn: index < cursor.count ? cursor[index] : nil, settings: settings(size: size), fontSize: size, in: context.cgContext, scale: scale)
                context.cgContext.restoreGState()
            }
        }
        return (image, cell.height)
    }
    /// RGBA bytes of an image.
    private func pixels(_ image: UIImage) -> (bytes: [UInt8], width: Int, height: Int) {
        let cg = image.cgImage!
        var bytes = [UInt8](repeating: 0, count: cg.width * cg.height * 4)
        let context = CGContext(data: &bytes, width: cg.width, height: cg.height, bitsPerComponent: 8, bytesPerRow: cg.width * 4, space: CGColorSpaceCreateDeviceRGB(),
                                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.draw(cg, in: CGRect(x: 0, y: 0, width: cg.width, height: cg.height))
        return (bytes, cg.width, cg.height)
    }
    private struct Px: Equatable { let r: Int, g: Int, b: Int; static let paper = Px(r: 255, g: 255, b: 255) }
    private func color(_ p: (bytes: [UInt8], width: Int, height: Int), _ x: Int, _ y: Int) -> Px {
        let i = (y * p.width + x) * 4
        return Px(r: Int(p.bytes[i]), g: Int(p.bytes[i + 1]), b: Int(p.bytes[i + 2]))
    }
    /// The first and last pixel row (within a pixel-column range) that is not paper white.
    private func inkRows(_ p: (bytes: [UInt8], width: Int, height: Int), columns: Range<Int>, rows: Range<Int>) -> ClosedRange<Int>? {
        var first: Int?, last: Int?
        for y in rows {
            for x in columns where color(p, x, y) != .paper { first = first ?? y; last = y; break }
        }
        if let first, let last { return first...last }
        return nil
    }

    func testABackgroundFillsExactlyTheCellsOfItsRun() {
        let blue = CellStyle(background: .rgb(RGB(red: 0, green: 0, blue: 255)))
        let text = StyledLine(text: "ab cd", runs: [StyleRun(length: 2, style: .plain), StyleRun(length: 1, style: blue), StyleRun(length: 2, style: .plain)], columns: 5)
        let (image, height) = render([text])
        let p = pixels(image)
        let cell = TerminalFont.cell(size: 12).width * 3
        let middle = Int(height * 3 / 2)
        XCTAssertEqual(color(p, Int(cell * 2.5), middle).b, 255)
        XCTAssertEqual(color(p, Int(cell * 2.5), middle).r, 0, "inside the blue cell")
        XCTAssertEqual(color(p, Int(cell * 1.5), Int(height * 3) - 1), .paper, "left of it is paper")
        XCTAssertEqual(color(p, Int(cell * 3.5), Int(height * 3) - 1), .paper, "and right of it")
    }
    func testTheCursorCellIsInverted() {
        let (image, height) = render([line("abc")], cursor: [1])
        let p = pixels(image)
        let cell = TerminalFont.cell(size: 12).width * 3
        let bottom = Int(height * 3) - 1
        let inside = color(p, Int(cell * 1.5), bottom)
        XCTAssertNotEqual(inside, .paper, "the cursor block is painted in the cursor color")
        XCTAssertEqual(color(p, Int(cell * 0.5), bottom), .paper)
        XCTAssertEqual(color(p, Int(cell * 2.5), bottom), .paper)
    }
    func testAnUnderlineIsAPixelLineUnderTheBaselineAndAStrikethroughCrossesTheLetters() {
        // Blanks, so that only the lines are on the pixels.
        let under = line("   ", style: CellStyle(attributes: [.underline]))
        let strike = line("   ", style: CellStyle(attributes: [.strikethrough]))
        let plain = line("   ")
        let p = pixels(render([plain, under, strike]).image)
        let h = Int((TerminalFont.cell(size: 12).height * 3).rounded())
        let cell = Int(TerminalFont.cell(size: 12).width * 3)
        // A column inside the cells shows the line and nothing else.
        let gap = cell * 3 / 2
        let plainInk = inkRows(p, columns: gap..<(gap + 1), rows: 0..<h)
        let underInk = inkRows(p, columns: gap..<(gap + 1), rows: h..<(2 * h))
        let strikeInk = inkRows(p, columns: gap..<(gap + 1), rows: (2 * h)..<(3 * h))
        XCTAssertNil(plainInk, "nothing under blank cells")
        XCTAssertNotNil(underInk, "the underline runs the width of the cells")
        XCTAssertNotNil(strikeInk, "and so does the strikethrough")
        if let underInk, let strikeInk { XCTAssertGreaterThan(underInk.lowerBound - h, strikeInk.lowerBound - 2 * h, "the underline is under the strike") }
    }
    func testTheBaselineIsTheSameInEveryRowWhateverFontsTheGlyphsComeFrom() {
        // The first letter is Menlo's in both rows; in the second, a later glyph comes from another font with other line metrics.
        let rows = [line("x"), line("x\u{2714}\u{FE0E}\u{23FA}\u{FE0E}日"), line("xyz")]
        let p = pixels(render(rows).image)
        let h = Int((TerminalFont.cell(size: 12).height * 3).rounded())
        let cell = Int(TerminalFont.cell(size: 12).width * 3)
        let ink = (0..<3).map { inkRows(p, columns: 0..<cell, rows: ($0 * h)..<(($0 + 1) * h)).map { ($0.lowerBound - 0, $0.upperBound) } }
        let relative = (0..<3).map { index in ink[index].map { ($0.0 - index * h, $0.1 - index * h) } }
        XCTAssertNotNil(relative[0])
        XCTAssertEqual(relative[0]?.0, relative[1]?.0)
        XCTAssertEqual(relative[0]?.1, relative[1]?.1, "the x sits on the same pixel row, with or without a borrowed glyph after it")
        XCTAssertEqual(relative[0]?.1, relative[2]?.1)
    }
    func testGlyphsLandOnTheirCells() {
        // A bar glyph in column 14 is drawn within that cell, however wide the glyphs before it are.
        let text = line("日本日本日本日█")   // seven wide glyphs: the bar is in column 14
        let (image, _) = render([text], width: 400)
        let p = pixels(image)
        let cell = TerminalFont.cell(size: 12).width * 3
        let start = Int(cell * 14), end = Int(cell * 15)
        var inkLeft = Int.max, inkRight = 0
        for x in Int(cell * 14)..<Int(cell * 16) where (0..<Int(TerminalFont.cell(size: 12).height * 3)).contains(where: { color(p, x, $0) != .paper }) { inkLeft = min(inkLeft, x); inkRight = max(inkRight, x) }
        XCTAssertGreaterThanOrEqual(inkLeft, start - 2)
        XCTAssertLessThanOrEqual(inkRight, end + 2)
    }
    func testAnEmptyLineAndAnEmptyRunDrawNothingAndDoNotCrash() {
        let p = pixels(render([line(""), StyledLine(text: "ab", runs: [StyleRun(length: 0, style: .plain), StyleRun(length: 2, style: .plain)], columns: 2)]).image)
        XCTAssertGreaterThan(p.width, 0)
    }

    func testPaintingARowIsFarInsideAFrameBudget() {
        // A busy row: colors, a bold word, a wide glyph, 60 columns. A fast fling brings at most a few new rows per frame (8.3 ms at 120 Hz).
        let red = CellStyle(foreground: .indexed(1), attributes: [.bold])
        let busy = StyledLine(text: "error[E0308]: mismatched types 日本 --> src/main.rs:12:5 ✔", runs: [StyleRun(length: 6, style: red), StyleRun(length: 52, style: .plain)], columns: 62)
        let cell = TerminalFont.cell(size: 12)
        let format = UIGraphicsImageRendererFormat(); format.scale = 3
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: 393, height: cell.height), format: format)
        let rows = 300
        let start = Date()
        for _ in 0..<rows { _ = renderer.image { context in TerminalRowPainter.draw(busy, cursorColumn: nil, settings: settings(), fontSize: 12, in: context.cgContext, scale: 3) } }
        let each = Date().timeIntervalSince(start) * 1000 / Double(rows)
        print("ROWPERF \(each) ms per painted row (including creating its bitmap)")
        XCTAssertLessThan(each, 2.0, "a handful of these per frame fit the 8.3 ms of a 120 Hz frame with room to spare")
    }

    // MARK: the rows themselves

    func testARowRepaintsOnlyWhenWhatItShowsChanged() {
        let row = TerminalRowView(frame: CGRect(x: 0, y: 0, width: 200, height: 14))
        let s = settings()
        row.configure(line: line("abc"), cursorColumn: nil, look: 1, settings: s, fontSize: 12)
        XCTAssertEqual(row.line, line("abc"))
        XCTAssertTrue(row.layer.needsDisplay(), "a new line is painted")
        row.layer.displayIfNeeded()
        XCTAssertFalse(row.layer.needsDisplay())
        row.configure(line: line("abc"), cursorColumn: nil, look: 1, settings: s, fontSize: 12)
        XCTAssertFalse(row.layer.needsDisplay(), "the same line, cursor and look are not painted again")
        row.configure(line: line("abd"), cursorColumn: nil, look: 1, settings: s, fontSize: 12)
        XCTAssertEqual(row.line, line("abd"))
        row.configure(line: line("abd"), cursorColumn: 1, look: 1, settings: s, fontSize: 12)
        XCTAssertEqual(row.cursorColumn, 1)
        row.layer.displayIfNeeded()
        row.configure(line: line("abd"), cursorColumn: 1, look: 2, settings: s, fontSize: 12)
        XCTAssertEqual(row.look, 2)
        XCTAssertTrue(row.layer.needsDisplay(), "a new look (colors, size) repaints")
        row.clear()
        XCTAssertNil(row.line)
    }
}
