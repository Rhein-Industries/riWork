import Foundation

/// A stretch of consecutive characters that share a style. `length` counts Swift `Character`s, the same unit as
/// `StyledScreen.cursorOffset`, so a run never depends on how many UTF-16 units or scalars its characters take.
public struct StyleRun: Sendable, Equatable {
    public let length: Int
    public let style: CellStyle
    public init(length: Int, style: CellStyle) { self.length = length; self.style = style }
}

/// One screen line: its text, its styles as runs that cover the whole text, and how many terminal cells it spans.
public struct StyledLine: Sendable, Equatable {
    public let text: String
    public let runs: [StyleRun]
    /// Width in terminal cells (a wide character counts two).
    public let columns: Int
    public init(text: String, runs: [StyleRun], columns: Int) { self.text = text; self.runs = runs; self.columns = columns }
}

extension StyledLine {
    /// Stands for a line that scrolled by unseen (see `TerminalBuffer.holes`): blank, and told apart from a real empty line by its width.
    public static let missing = StyledLine(text: "", runs: [], columns: -1)
    public var isMissing: Bool { columns < 0 }
    /// The same line of text, allowing for trailing spaces that one capture kept and another trimmed.
    public func sameText(as other: StyledLine) -> Bool {
        if text == other.text { return true }
        func trimmed(_ text: String) -> Substring { text.dropLast(text.reversed().prefix { $0 == " " }.count) }
        return trimmed(text) == trimmed(other.text)
    }
}

/// A screen ready to draw: the text, its styles as runs that cover the whole text (line breaks included), and the cursor cell.
/// Colors are still symbolic (`TerminalColor`), so a new palette needs no new parse.
///
/// The screen is also kept line by line, so a view can build only the lines that are on screen and redraw only the lines that changed.
public struct StyledScreen: Sendable, Equatable {
    public let text: String
    public let runs: [StyleRun]
    /// Character offset of the cursor cell within `text`.
    public let cursorOffset: Int?
    public let lines: [StyledLine]
    /// The cursor cell as a line and a character index within that line.
    public let cursorLine: Int?
    public let cursorColumn: Int?
    /// The widest line in terminal cells.
    public let columns: Int
    /// How many of `lines` lie above the visible screen (scrollback). Zero when the desktop did not say where the screen starts.
    public let historyLines: Int
    public static let empty = StyledScreen(text: "", runs: [], cursorOffset: nil, lines: [], cursorLine: nil, cursorColumn: nil, columns: 0)
    public init(text: String, runs: [StyleRun], cursorOffset: Int?, lines: [StyledLine] = [], cursorLine: Int? = nil, cursorColumn: Int? = nil, columns: Int = 0, historyLines: Int = 0) {
        self.text = text; self.runs = runs; self.cursorOffset = cursorOffset
        self.lines = lines; self.cursorLine = cursorLine; self.cursorColumn = cursorColumn; self.columns = columns
        self.historyLines = max(0, min(historyLines, lines.count))
    }
    /// The same screen without styles.
    public var plain: TerminalScreen { TerminalScreen(text: text, cursorOffset: cursorOffset) }
    /// True when every cell has the default look, as in the output of an older desktop.
    public var isPlain: Bool { runs.allSatisfy { $0.style.isPlain } }
}

extension TerminalText {
    // MARK: Text presentation

    /// Characters that must keep their look: an explicit variation selector, keycaps, joined sequences, skin tones, tag sequences.
    private static func isDeliberateEmoji(_ scalar: Unicode.Scalar) -> Bool {
        switch scalar.value {
        case 0xFE0E, 0xFE0F, 0x20E3, 0x200D, 0x1F3FB...0x1F3FF, 0xE0020...0xE007F: true
        default: false
        }
    }

    /// The character with text presentation forced: U+FE0E (VARIATION SELECTOR-15) is added after a symbol that can also be an emoji
    /// (`Emoji` yes) but is text by default (`Emoji_Presentation` no) — ⏺ ⏸ ⚠ ✔ ▶ ℹ ✂ ☑. iOS otherwise shows some of these as colorful emoji
    /// where the desktop terminal shows plain glyphs.
    ///
    /// Left alone: ASCII (digits, `#` and `*` are formally emoji), anything with an explicit selector (FE0E or FE0F), keycaps, ZWJ
    /// sequences, skin-tone and tag sequences, and characters that are emoji by default (✅ 🚀 ⭐), which the desktop draws as emoji
    /// two cells wide. The cell width of the character does not change, and it stays one `Character`.
    public static func textPresentation(_ character: Character) -> Character {
        var scalars = character.unicodeScalars
        guard let first = scalars.first, !first.isASCII else { return character }
        let properties = first.properties
        guard properties.isEmoji, !properties.isEmojiPresentation else { return character }
        scalars.removeFirst()
        if scalars.contains(where: isDeliberateEmoji) { return character }
        var text = String.UnicodeScalarView()
        text.append(first)
        text.append("\u{FE0E}")
        text.append(contentsOf: scalars)
        return Character(String(text))
    }

    /// `textPresentation` for every character of `text`.
    public static func textPresentation(_ text: String) -> String {
        var out = String()
        out.reserveCapacity(text.utf8.count + 8)
        for character in text { out.append(textPresentation(character)) }
        return out
    }

    // MARK: Styled screens

    struct StyledCell {
        var character: Character
        var style: UInt32
    }
    struct ScannedScreen {
        var rows: [[StyledCell]]
        var styles: [CellStyle]
        func isBlank(_ row: [StyledCell]) -> Bool {
            row.allSatisfy { $0.character.isWhitespace && !styles[Int($0.style)].paintsBlank }
        }
    }

    /// Where the sequence that starts at `start` (an ESC) ends, and, for a plain SGR sequence, its parameters.
    /// Everything else is dropped. A sequence that breaks off at a control character or non-ASCII text ends there, so
    /// that character is still read as text.
    static func escape(in scalars: [Unicode.Scalar], at start: Int) -> (next: Int, sgr: [SGR.Parameter]?) {
        let count = scalars.count
        var j = start + 1
        guard j < count else { return (count, nil) }
        switch scalars[j].value {
        case 0x5B:   // CSI
            j += 1
            var parameters: [UInt8] = []
            var marked = false, intermediates = false, tooLong = false
            while j < count {
                let c = scalars[j].value
                switch c {
                case 0x30...0x3F:
                    if c >= 0x3C { marked = true }
                    if parameters.count < 256 { parameters.append(UInt8(c)) } else { tooLong = true }
                    j += 1
                case 0x20...0x2F:
                    intermediates = true
                    j += 1
                case 0x40...0x7E:
                    j += 1
                    if c == 0x6D, !marked, !intermediates, !tooLong, let parsed = SGR.parameters(parameters[...]) { return (j, parsed) }
                    return (j, nil)
                default:
                    return (j, nil)
                }
            }
            return (count, nil)
        case 0x5D, 0x50, 0x58, 0x5E, 0x5F:   // OSC, DCS, SOS, PM, APC: a string that ends at BEL or ST
            j += 1
            while j < count {
                let c = scalars[j].value
                if c == 0x07 { return (j + 1, nil) }
                if c == 0x1B { return (j + 1 < count && scalars[j + 1].value == 0x5C ? j + 2 : j, nil) }
                // An unterminated string ends with its line; it does not eat the screen.
                if c == 0x0A { return (j, nil) }
                j += 1
            }
            return (count, nil)
        default:
            // ESC, intermediates, one final byte (charset selection, keypad modes, index …).
            while j < count, (0x20...0x2F).contains(scalars[j].value) { j += 1 }
            if j < count, (0x30...0x7E).contains(scalars[j].value) { j += 1 }
            return (max(j, start + 1), nil)
        }
    }

    /// Reads the desktop's text into styled rows. SGR sequences set the running style; every other escape sequence and control
    /// character is dropped; CR overprints keep only what was written last; glyphs the font cannot show become one blank cell
    /// with `keepCells` (columns still line up) and vanish otherwise.
    static func scanStyled(_ input: String, keepCells: Bool, textPresentation forceText: Bool) -> ScannedScreen {
        let scalars = Array(input.unicodeScalars)
        var styles: [CellStyle] = [.plain]
        var known: [CellStyle: UInt32] = [.plain: 0]
        var current = CellStyle.plain
        var currentID: UInt32 = 0
        var rows: [[StyledCell]] = []
        var line = String.UnicodeScalarView()
        var lineScalars = 0
        var marks: [(at: Int, style: UInt32)] = [(0, 0)]

        func restartMarks() { marks.removeAll(keepingCapacity: true); marks.append((0, currentID)) }
        func finishLine() {
            var cells: [StyledCell] = []
            cells.reserveCapacity(lineScalars)
            var mark = 0, position = 0
            for character in String(line) {
                while mark + 1 < marks.count, marks[mark + 1].at <= position { mark += 1 }
                cells.append(StyledCell(character: forceText ? textPresentation(character) : character, style: marks[mark].style))
                position += character.isASCII ? 1 : character.unicodeScalars.count
            }
            rows.append(cells)
            line.removeAll(keepingCapacity: true)
            lineScalars = 0
            restartMarks()
        }
        func setStyle(_ style: CellStyle) {
            current = style
            let id: UInt32
            if let existing = known[style] { id = existing }
            else if styles.count < Int(UInt32.max) { id = UInt32(styles.count); styles.append(style); known[style] = id }
            else { id = 0 }
            guard id != currentID else { return }
            currentID = id
            if marks.last?.at == lineScalars { marks.removeLast() }
            marks.append((lineScalars, id))
        }

        var i = 0
        let end = scalars.count
        while i < end {
            let scalar = scalars[i]
            switch scalar.value {
            case 0x1B:
                let (next, sgr) = escape(in: scalars, at: i)
                if let sgr { var style = current; SGR.apply(sgr, to: &style); setStyle(style) }
                i = next
            case 0x0A:
                finishLine()
                i += 1
            case 0x0D:
                if i + 1 < end, scalars[i + 1].value == 0x0A { i += 1; continue }
                // A bare CR returns to the start of the line: what follows overwrites it.
                line.removeAll(keepingCapacity: true)
                lineScalars = 0
                restartMarks()
                i += 1
            case 0x09:
                line.append(scalar); lineScalars += 1
                i += 1
            case 0x00...0x1F, 0x7F...0x9F:
                i += 1
            default:
                if scalar.value >= 0xE000, scalar.properties.generalCategory == .privateUse {
                    if keepCells { line.append(" "); lineScalars += 1 }
                } else {
                    line.append(scalar); lineScalars += 1
                }
                i += 1
            }
        }
        finishLine()
        return ScannedScreen(rows: rows, styles: styles)
    }

    /// The lines, text and runs of rows. A line whose runs cannot describe its text exactly (which takes grapheme rules that merge
    /// characters across cells) is drawn unstyled rather than misaligned.
    private static func build(_ screen: ScannedScreen, rows: [[StyledCell]], cursor: (row: Int, index: Int)?, historyLines: Int = 0) -> StyledScreen {
        var lines: [StyledLine] = []
        lines.reserveCapacity(rows.count)
        var columns = 0
        for row in rows {
            var text = ""
            text.reserveCapacity(row.count)
            var runs: [StyleRun] = []
            var runStyle: UInt32 = 0
            var runLength = 0
            var width = 0
            for cell in row {
                text.append(cell.character)
                width += cellWidth(cell.character)
                if runLength > 0, cell.style == runStyle { runLength += 1; continue }
                if runLength > 0 { runs.append(StyleRun(length: runLength, style: screen.styles[Int(runStyle)])) }
                runStyle = cell.style; runLength = 1
            }
            if runLength > 0 { runs.append(StyleRun(length: runLength, style: screen.styles[Int(runStyle)])) }
            let actual = text.count
            if actual != row.count { runs = actual > 0 ? [StyleRun(length: actual, style: .plain)] : [] }
            lines.append(StyledLine(text: text, runs: runs, columns: width))
            columns = max(columns, width)
        }
        // The flat form: the lines joined by plain line breaks.
        var text = ""
        text.reserveCapacity(lines.reduce(0) { $0 + $1.text.utf8.count + 1 })
        var runs: [StyleRun] = []
        func add(_ run: StyleRun) {
            if let last = runs.last, last.style == run.style { runs[runs.count - 1] = StyleRun(length: last.length + run.length, style: run.style) }
            else { runs.append(run) }
        }
        var offset = 0
        var cursorOffset: Int?, cursorColumn: Int?, cursorLine: Int?
        for (index, line) in lines.enumerated() {
            if index > 0 { text.append("\n"); add(StyleRun(length: 1, style: .plain)); offset += 1 }
            if let cursor, cursor.row == index, !line.text.isEmpty {
                let column = min(cursor.index, line.text.count - 1)
                cursorOffset = offset + column; cursorColumn = column; cursorLine = index
            }
            text.append(line.text)
            for run in line.runs { add(run) }
            offset += line.text.count
        }
        return StyledScreen(text: text, runs: runs, cursorOffset: cursorOffset, lines: lines, cursorLine: cursorLine, cursorColumn: cursorColumn, columns: columns, historyLines: historyLines)
    }

    /// The styled counterpart of `screen(_:cursor:rows:)`: the same rows, trimming and cursor cell, with the SGR styles kept and
    /// (unless switched off) text presentation forced on symbols.
    ///
    /// The cursor cell is `(x, y)` counted in terminal cells from the first line of the visible screen, which is the last `rows`
    /// lines of `input`. Styles never change what a cell is: the cursor lands on the same character as it does in the plain screen.
    public static func styledScreen(_ input: String, cursor: (x: Int, y: Int)?, rows screenRows: Int?, textPresentation forceText: Bool = true) -> StyledScreen {
        guard let cursor, let screenRows, screenRows > 0, cursor.x >= 0, cursor.y >= 0, cursor.y < screenRows else {
            var scanned = scanStyled(input, keepCells: false, textPresentation: forceText)
            while let last = scanned.rows.last, scanned.isBlank(last) { scanned.rows.removeLast() }
            return build(scanned, rows: scanned.rows, cursor: nil)
        }
        var scanned = scanStyled(input, keepCells: true, textPresentation: forceText)
        var rows = scanned.rows
        // A trailing newline terminates the last line; it does not start another one.
        if rows.count > 1, rows.last?.isEmpty == true { rows.removeLast() }
        // The lines above the screen are scrollback; the screen starts `historyLines` lines in.
        let historyLines = max(0, rows.count - screenRows)
        let row = historyLines + cursor.y
        while rows.count <= row { rows.append([]) }
        while rows.count - 1 > row, let last = rows.last, scanned.isBlank(last) { rows.removeLast() }
        var cells = rows[row]
        var columns = 0
        var index = 0
        while index < cells.count, columns + cellWidth(cells[index].character) <= cursor.x { columns += cellWidth(cells[index].character); index += 1 }
        // Past the end of the line: pad up to the column. On the second cell of a wide character, stay on that character.
        if index >= cells.count {
            let blank = StyledCell(character: " ", style: 0)
            if columns < cursor.x { cells.append(contentsOf: Array(repeating: blank, count: cursor.x - columns)) }
            index = cells.count
            cells.append(blank)
        }
        rows[row] = cells
        scanned.rows = rows
        return build(scanned, rows: rows, cursor: (row, index), historyLines: historyLines)
    }

    /// The lines of a `shell.history` page: scrollback lines, top to bottom, with the same styles and text presentation as the live
    /// screen.
    ///
    /// The desktop joins the lines with line breaks and puts none after the last (`"a\nb"`, one blank line is `""`, three blank lines are
    /// `"\n\n"`), and says how many lines there are (`line_count`), which is what tells an empty page from one blank line and a page that
    /// ends in blank lines from one that ends in a line break. With `expecting` the page is read that way and checked against the count;
    /// a page that is one break longer (a terminator after every line) is read that way. Without it a trailing break is a terminator:
    /// `"a\n\n"` is `a` and a blank line. The caller checks the number of lines against the `line_count` the desktop reported.
    public static func styledLines(page input: String, expecting count: Int? = nil, textPresentation forceText: Bool = true) -> [StyledLine] {
        if count == 0 || (count == nil && input.isEmpty) { return [] }
        let scanned = scanStyled(input, keepCells: true, textPresentation: forceText)
        var rows = scanned.rows
        // `scanStyled` ends a row at every break and once more at the end: n breaks make n + 1 rows, exactly the lines of the protocol.
        if let count {
            if rows.count == count + 1, rows.last?.isEmpty == true, input.utf8.last == 0x0A { rows.removeLast() }
        } else if input.utf8.last == 0x0A, rows.last?.isEmpty == true {
            rows.removeLast()
        }
        return build(scanned, rows: rows, cursor: nil).lines
    }
}
