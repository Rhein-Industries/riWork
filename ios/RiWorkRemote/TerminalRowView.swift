import UIKit
import CoreText
import RiWorkCore

/// Draws one grid row with Core Text, cell by cell.
///
/// Why not `NSAttributedString.draw`: a glyph borrowed from another font (a check mark, a bullet, CJK) has other line metrics, and
/// TextKit moves the whole line's baseline to fit it, so a row with a symbol sits a pixel off its neighbours. Here the baseline is
/// Menlo's, on a whole device pixel, for every row. Backgrounds, underlines and strikethroughs are painted on the grid (column ×
/// cell width) rather than from glyph extents, so a fill is exactly the cells the terminal counts for it.
@MainActor enum TerminalRowPainter {
    private struct FontKey: Hashable { let size: Double, bold: Bool, italic: Bool }
    private static var fonts: [FontKey: CTFont] = [:]

    private static func font(size: Double, bold: Bool, italic: Bool) -> CTFont {
        let key = FontKey(size: size, bold: bold, italic: italic)
        if let known = fonts[key] { return known }
        if fonts.count > 64 { fonts.removeAll() }
        let made = CTFontCreateWithName(TerminalFont.uiFont(size: size, bold: bold, italic: italic).fontName as CFString, CGFloat(size), nil)
        fonts[key] = made
        return made
    }
    private static func cgColor(_ rgb: RGB) -> CGColor {
        CGColor(srgbRed: CGFloat(rgb.red) / 255, green: CGFloat(rgb.green) / 255, blue: CGFloat(rgb.blue) / 255, alpha: 1)
    }

    /// The distance from the top of a row to the baseline: Menlo's ascent, on a whole pixel.
    static func baseline(size: Double, scale: Double) -> Double {
        let ascent = Double(CTFontGetAscent(font(size: size, bold: false, italic: false)))
        return (ascent * scale).rounded() / scale
    }

    static func draw(_ line: StyledLine, cursorColumn: Int?, settings: TerminalRenderer.Settings, fontSize: Double, in context: CGContext, scale: Double) {
        guard !line.text.isEmpty else { return }
        let segments = TerminalRenderer.segments(text: line.text, runs: line.runs, cursorColumn: cursorColumn, settings: settings)
        guard !segments.isEmpty else { return }
        let cell = TerminalFont.cell(size: fontSize)
        let baseline = baseline(size: fontSize, scale: scale)
        let regular = font(size: fontSize, bold: false, italic: false)

        // One attributed string for the glyphs.
        let attributed = CFAttributedStringCreateMutable(nil, 0)!
        var painted: [(column: Int, cells: Int, segment: TerminalRenderer.Segment, foreground: CGColor, background: CGColor?)] = []
        var column = 0
        for segment in segments {
            let bold = segment.style.attributes.contains(.bold), italic = segment.style.attributes.contains(.italic)
            var foreground: RGB, background: RGB?
            if segment.cursor { foreground = settings.cursorForeground; background = settings.cursorBackground }
            else { (foreground, background) = settings.colors.resolve(segment.style) }
            let fg = cgColor(foreground)
            let start = CFAttributedStringGetLength(attributed)
            CFAttributedStringReplaceString(attributed, CFRange(location: start, length: 0), segment.text as CFString)
            let range = CFRange(location: start, length: CFAttributedStringGetLength(attributed) - start)
            CFAttributedStringSetAttribute(attributed, range, kCTFontAttributeName, font(size: fontSize, bold: bold, italic: italic))
            CFAttributedStringSetAttribute(attributed, range, kCTForegroundColorAttributeName, fg)
            // Always set: text added to an attributed string takes on the attributes before it, so a kern would run on into the next piece.
            CFAttributedStringSetAttribute(attributed, range, kCTKernAttributeName, segment.kern as CFNumber)
            let cells = segment.text.reduce(0) { $0 + TerminalText.cellWidth($1) }
            painted.append((column, cells, segment, fg, background.map(cgColor)))
            column += cells
        }

        // Backgrounds first, on the grid.
        for item in painted {
            guard let background = item.background else { continue }
            context.setFillColor(background)
            context.fill(CGRect(x: Double(item.column) * cell.width, y: 0, width: Double(item.cells) * cell.width, height: cell.height))
        }
        // The glyphs, on Menlo's baseline. UIKit's context is flipped; Core Text wants its own orientation.
        context.saveGState()
        context.textMatrix = CGAffineTransform(scaleX: 1, y: -1)
        context.textPosition = CGPoint(x: 0, y: baseline)
        let ctLine = CTLineCreateWithAttributedString(attributed)
        CTLineDraw(ctLine, context)
        context.restoreGState()
        // Underline and strikethrough, one device pixel thick (or the font's, when thicker), in the text color.
        let thickness = max(1 / scale, Double(CTFontGetUnderlineThickness(regular)))
        let underline = baseline - Double(CTFontGetUnderlinePosition(regular))
        let strike = baseline - Double(CTFontGetXHeight(regular)) / 2
        for item in painted {
            let attributes = item.segment.style.attributes
            guard attributes.contains(.underline) || attributes.contains(.strikethrough) else { continue }
            context.setFillColor(item.foreground)
            let x = Double(item.column) * cell.width, width = Double(item.cells) * cell.width
            if attributes.contains(.underline) { context.fill(CGRect(x: x, y: snapped(underline - thickness / 2, scale), width: width, height: thickness)) }
            if attributes.contains(.strikethrough) { context.fill(CGRect(x: x, y: snapped(strike - thickness / 2, scale), width: width, height: thickness)) }
        }
    }
    private static func snapped(_ value: Double, _ scale: Double) -> Double { (value * scale).rounded() / scale }
}

/// One row of the terminal: a line, painted once when it is given and left alone while the compositor moves it.
@MainActor final class TerminalRowView: UIView {
    private(set) var line: StyledLine?
    private(set) var cursorColumn: Int?
    /// Which look the row was painted with: a counter the surface bumps when colors, font or size change.
    private(set) var look = -1
    private var settings: TerminalRenderer.Settings?
    private var fontSize = 12.0

    override init(frame: CGRect) {
        super.init(frame: frame)
        isOpaque = false
        backgroundColor = .clear
        isUserInteractionEnabled = false
        clipsToBounds = true
        contentMode = .redraw
    }
    required init?(coder: NSCoder) { fatalError("not used") }

    /// Shows `line`; paints again only when something about it differs from what is already painted.
    func configure(line: StyledLine, cursorColumn: Int?, look: Int, settings: TerminalRenderer.Settings, fontSize: Double) {
        guard line != self.line || cursorColumn != self.cursorColumn || look != self.look else { return }
        self.line = line; self.cursorColumn = cursorColumn; self.look = look
        self.settings = settings; self.fontSize = fontSize
        setNeedsDisplay()
    }
    /// Forgets the line (the row goes back to the pool).
    func clear() {
        guard line != nil else { return }
        line = nil; cursorColumn = nil; look = -1
        setNeedsDisplay()
    }

    override func draw(_ rect: CGRect) {
        guard let line, let settings, let context = UIGraphicsGetCurrentContext() else { return }
        Perf.count("rowPaint")
        let scale = Double(window?.screen.scale ?? traitCollection.displayScale)
        TerminalRowPainter.draw(line, cursorColumn: cursorColumn, settings: settings, fontSize: fontSize, in: context, scale: scale > 0 ? scale : TerminalFont.pixelsPerPoint)
    }
}
