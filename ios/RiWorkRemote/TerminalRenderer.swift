import SwiftUI
import UIKit
import CoreText
import RiWorkCore

/// Turns a parsed screen line into styled text for the terminal view.
///
/// - Colors come from the synced terminal palette (`TerminalRenderColors`); nothing here reads a color of its own.
/// - Bold and italic are font traits of the same Menlo, whose faces all advance by the same amount, so attributes never move a cell.
/// - A glyph the font has to borrow from another font (a symbol, CJK, emoji) rarely has Menlo's advance. Each such character gets a
///   kern that brings its advance back to the number of cells the terminal counts for it, so the columns after it stay put.
/// - The cursor cell is inverted: the cursor color behind, the terminal background as the text color.
enum TerminalRenderer {
    struct Settings: Equatable, Sendable {
        var colors: TerminalRenderColors
        var cursorBackground: RGB
        var cursorForeground: RGB
        var showCursor: Bool
        /// The underline of links: the theme's accent.
        var link: RGB
        /// The size the columns were measured at. The text itself scales with the view's font (pinch), so this only steers kerns.
        var fontSize: Double
    }

    static func color(_ rgb: RGB) -> Color {
        Color(.sRGB, red: Double(rgb.red) / 255, green: Double(rgb.green) / 255, blue: Double(rgb.blue) / 255, opacity: 1)
    }

    /// A piece of a line that is drawn alike: one style, and for a glyph the font borrows from another font, the kern that brings its
    /// advance back to the cells the terminal counts for it.
    struct Segment: Equatable {
        var text: String
        var style: CellStyle
        /// The cursor cell: drawn inverted.
        var cursor = false
        var kern = 0.0
    }

    /// One line cut into the pieces it is drawn in, with the cursor cell (a character index) apart. Pure and thread-safe.
    static func segments(text: String, runs: [StyleRun], cursorColumn: Int?, settings: Settings) -> [Segment] {
        let characters = Array(text)
        let cell = TerminalFont.cell(size: settings.fontSize).width
        var result: [Segment] = []
        /// One piece per run of ASCII (whose advance is Menlo's) and one per other character, each with the kern its glyph needs.
        func append(_ slice: ArraySlice<Character>, _ style: CellStyle, cursor: Bool) {
            guard !slice.isEmpty else { return }
            var pending = ""
            func flush() {
                guard !pending.isEmpty else { return }
                result.append(Segment(text: pending, style: style, cursor: cursor))
                pending = ""
            }
            for character in slice {
                if character.isASCII { pending.append(character); continue }
                let width = Double(TerminalText.cellWidth(character)) * cell
                let kern = width - advance(of: character, size: settings.fontSize)
                guard abs(kern) > 0.05 else { pending.append(character); continue }
                flush()
                result.append(Segment(text: String(character), style: style, cursor: cursor, kern: kern))
            }
            flush()
        }
        var offset = 0
        for run in runs {
            let end = min(characters.count, offset + max(0, run.length))
            guard offset < end else { continue }
            if let cursor = cursorColumn, cursor >= offset, cursor < end {
                append(characters[offset..<cursor], run.style, cursor: false)
                append(characters[cursor...cursor], run.style, cursor: true)
                append(characters[(cursor + 1)..<end], run.style, cursor: false)
            } else {
                append(characters[offset..<end], run.style, cursor: false)
            }
            offset = end
        }
        return result
    }

    /// One line as SwiftUI attributed text, with the cursor cell (a character index) inverted. Pure and thread-safe.
    static func attributed(text: String, runs: [StyleRun], cursorColumn: Int?, settings: Settings) -> AttributedString {
        var result = AttributedString()
        var containers: [CellStyle: AttributeContainer] = [:]
        func container(_ style: CellStyle) -> AttributeContainer {
            if let known = containers[style] { return known }
            let made = attributes(for: style, colors: settings.colors)
            containers[style] = made
            return made
        }
        func cursorAttributes(_ style: CellStyle) -> AttributeContainer {
            var made = AttributeContainer()
            made.backgroundColor = color(settings.cursorBackground)
            made.foregroundColor = color(settings.cursorForeground)
            if style.attributes.contains(.bold) && style.attributes.contains(.italic) { made.inlinePresentationIntent = [.stronglyEmphasized, .emphasized] }
            else if style.attributes.contains(.bold) { made.inlinePresentationIntent = .stronglyEmphasized }
            else if style.attributes.contains(.italic) { made.inlinePresentationIntent = .emphasized }
            return made
        }
        for segment in segments(text: text, runs: runs, cursorColumn: cursorColumn, settings: settings) {
            var piece = AttributedString(segment.text)
            piece.mergeAttributes(segment.cursor ? cursorAttributes(segment.style) : container(segment.style))
            if segment.kern != 0 { piece.kern = segment.kern }
            result.append(piece)
        }
        return result
    }

    private static func attributes(for style: CellStyle, colors: TerminalRenderColors) -> AttributeContainer {
        var made = AttributeContainer()
        let resolved = colors.resolve(style)
        made.foregroundColor = color(resolved.foreground)
        if let background = resolved.background { made.backgroundColor = color(background) }
        let bold = style.attributes.contains(.bold), italic = style.attributes.contains(.italic)
        if bold && italic { made.inlinePresentationIntent = [.stronglyEmphasized, .emphasized] }
        else if bold { made.inlinePresentationIntent = .stronglyEmphasized }
        else if italic { made.inlinePresentationIntent = .emphasized }
        if style.attributes.contains(.underline) { made.underlineStyle = .single }
        if style.attributes.contains(.strikethrough) { made.strikethroughStyle = .single }
        return made
    }

    // MARK: Glyph advances

    private struct AdvanceKey: Hashable { let character: Character; let size: Double }
    private static let advances = AdvanceCache()
    private final class AdvanceCache: @unchecked Sendable {
        private let lock = NSLock()
        private var values: [AdvanceKey: Double] = [:]
        func value(for key: AdvanceKey, compute: () -> Double) -> Double {
            lock.lock()
            if let known = values[key] { lock.unlock(); return known }
            lock.unlock()
            let made = compute()
            lock.lock()
            if values.count > 4096 { values.removeAll() }
            values[key] = made
            lock.unlock()
            return made
        }
    }
    /// How far the font really advances for this character at `size`, whichever font ends up providing its glyph.
    private static func advance(of character: Character, size: Double) -> Double {
        advances.value(for: AdvanceKey(character: character, size: size)) {
            let text = NSAttributedString(string: String(character), attributes: [.font: TerminalFont.uiFont(size: size)])
            return Double(CTLineGetTypographicBounds(CTLineCreateWithAttributedString(text), nil, nil, nil))
        }
    }
}
