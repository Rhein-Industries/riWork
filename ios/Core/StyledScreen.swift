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
    ///
    /// `LineScanner.scan` follows the same rules for one line at a time (for `StyledLineCache`): a change of rules here is a change there,
    /// and `StyledLineCacheTests` fails until both agree.
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

    /// One row as a line: its text, its styles as runs and its width. A line whose runs cannot describe its text exactly (which takes
    /// grapheme rules that merge characters across cells) is drawn unstyled rather than misaligned.
    static func makeLine(_ row: [StyledCell], styles: [CellStyle]) -> StyledLine {
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
            if runLength > 0 { runs.append(StyleRun(length: runLength, style: styles[Int(runStyle)])) }
            runStyle = cell.style; runLength = 1
        }
        if runLength > 0 { runs.append(StyleRun(length: runLength, style: styles[Int(runStyle)])) }
        let actual = text.count
        if actual != row.count { runs = actual > 0 ? [StyleRun(length: actual, style: .plain)] : [] }
        return StyledLine(text: text, runs: runs, columns: width)
    }

    /// The lines, text and runs of rows.
    private static func build(_ screen: ScannedScreen, rows: [[StyledCell]], cursor: (row: Int, index: Int)?, historyLines: Int = 0) -> StyledScreen {
        flatten(buildLines(screen, rows: rows), cursor: cursor, historyLines: historyLines)
    }

    private static func buildLines(_ screen: ScannedScreen, rows: [[StyledCell]]) -> [StyledLine] {
        var lines: [StyledLine] = []
        lines.reserveCapacity(rows.count)
        for row in rows { lines.append(makeLine(row, styles: screen.styles)) }
        return lines
    }

    /// The flat form of lines: the lines joined by plain line breaks, with the cursor cell as an offset into that text.
    ///
    /// The length of a line in characters is the sum of its runs (they cover the whole text), so no line's text is counted again.
    static func flatten(_ lines: [StyledLine], cursor: (row: Int, index: Int)?, historyLines: Int) -> StyledScreen {
        var columns = 0, bytes = 0, runCount = lines.count
        for line in lines { columns = max(columns, line.columns); bytes += line.text.utf8.count + 1; runCount += line.runs.count }
        var text = ""
        text.reserveCapacity(bytes)
        var runs: [StyleRun] = []
        runs.reserveCapacity(runCount)
        func add(_ run: StyleRun) {
            if let last = runs.last, last.style == run.style { runs[runs.count - 1] = StyleRun(length: last.length + run.length, style: run.style) }
            else { runs.append(run) }
        }
        var offset = 0
        var cursorOffset: Int?, cursorColumn: Int?, cursorLine: Int?
        for (index, line) in lines.enumerated() {
            if index > 0 { text.append("\n"); add(StyleRun(length: 1, style: .plain)); offset += 1 }
            var characters = 0
            for run in line.runs { characters += run.length }
            if let cursor, cursor.row == index, characters > 0 {
                let column = min(cursor.index, characters - 1)
                cursorOffset = offset + column; cursorColumn = column; cursorLine = index
            }
            text.append(line.text)
            for run in line.runs { add(run) }
            offset += characters
        }
        return StyledScreen(text: text, runs: runs, cursorOffset: cursorOffset, lines: lines, cursorLine: cursorLine, cursorColumn: cursorColumn, columns: columns, historyLines: historyLines)
    }

    /// The index of the cell that holds terminal column `x` of a row. Past the end of the row the row is padded with plain blanks up to
    /// the column, and the cursor sits on one more blank. On the second cell of a wide character the cursor stays on that character.
    static func placeCursor(in cells: inout [StyledCell], x: Int) -> Int {
        var columns = 0
        var index = 0
        while index < cells.count, columns + cellWidth(cells[index].character) <= x { columns += cellWidth(cells[index].character); index += 1 }
        if index >= cells.count {
            let blank = StyledCell(character: " ", style: 0)
            if columns < x { cells.append(contentsOf: Array(repeating: blank, count: x - columns)) }
            index = cells.count
            cells.append(blank)
        }
        return index
    }

    /// The styled counterpart of `screen(_:cursor:rows:)`: the same rows, trimming and cursor cell, with the SGR styles kept and
    /// (unless switched off) text presentation forced on symbols.
    ///
    /// The cursor cell is `(x, y)` counted in terminal cells from the first line of the visible screen, which is the last `rows`
    /// lines of `input`. Styles never change what a cell is: the cursor lands on the same character as it does in the plain screen.
    ///
    /// With a `cache`, lines that were parsed before (the same text, read under the same running style) are not parsed again and come back
    /// as the very same `StyledLine` values; the result is identical to the one without a cache. The cursor line is always parsed afresh.
    public static func styledScreen(_ input: String, cursor: (x: Int, y: Int)?, rows screenRows: Int?, textPresentation forceText: Bool = true, cache: StyledLineCache? = nil) -> StyledScreen {
        if let cache { return cache.parse(input, cursor: cursor, rows: screenRows, textPresentation: forceText) }
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
        let index = placeCursor(in: &cells, x: cursor.x)
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
        return buildLines(scanned, rows: rows)
    }
}

// MARK: - Line cache

extension TerminalText {
    /// Reads one line (the bytes between two line feeds) under a given running style, with the rules of `scanStyled`: it is that
    /// function's loop for a single line, which can start in any style and says in which style it ends. Kept apart so a line can be
    /// read on its own, and so its scratch space is reused from line to line.
    ///
    /// A line never contains a line feed, so a CR in it is always a bare one (the CR of a CRLF is taken off by the caller).
    struct LineScanner {
        /// The cells of the last line scanned. Their style numbers index `styles`, where 0 is always the plain style.
        private(set) var cells: [TerminalText.StyledCell] = []
        private(set) var styles: [CellStyle] = [.plain]
        /// The running style after the last line scanned.
        private(set) var end = CellStyle.plain

        private var known: [CellStyle: UInt32] = [.plain: 0]
        private var marks: [(at: Int, style: UInt32)] = []
        private var scalars: [Unicode.Scalar] = []
        private var line = String.UnicodeScalarView()
        private var lineScalars = 0
        private var current = CellStyle.plain
        private var currentID: UInt32 = 0

        mutating func scan(_ bytes: UnsafeBufferPointer<UInt8>, start: CellStyle, keepCells: Bool, textPresentation forceText: Bool) {
            decode(bytes)
            styles.removeAll(keepingCapacity: true); styles.append(.plain)
            known.removeAll(keepingCapacity: true); known[.plain] = 0
            cells.removeAll(keepingCapacity: true)
            line.removeAll(keepingCapacity: true)
            lineScalars = 0
            current = start
            currentID = 0
            if start != .plain { styles.append(start); known[start] = 1; currentID = 1 }
            marks.removeAll(keepingCapacity: true)
            marks.append((0, currentID))

            var i = 0
            let count = scalars.count
            while i < count {
                let scalar = scalars[i]
                switch scalar.value {
                case 0x1B:
                    let (next, sgr) = TerminalText.escape(in: scalars, at: i)
                    if let sgr { var style = current; SGR.apply(sgr, to: &style); setStyle(style) }
                    i = next
                case 0x0D:
                    // A bare CR returns to the start of the line: what follows overwrites it.
                    line.removeAll(keepingCapacity: true)
                    lineScalars = 0
                    marks.removeAll(keepingCapacity: true); marks.append((0, currentID))
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
            var mark = 0, position = 0
            cells.reserveCapacity(lineScalars)
            for character in String(line) {
                while mark + 1 < marks.count, marks[mark + 1].at <= position { mark += 1 }
                cells.append(StyledCell(character: forceText ? TerminalText.textPresentation(character) : character, style: marks[mark].style))
                position += character.isASCII ? 1 : character.unicodeScalars.count
            }
            end = current
        }

        /// Whether every cell of the last line is a blank that paints nothing.
        var isBlank: Bool {
            cells.allSatisfy { $0.character.isWhitespace && !styles[Int($0.style)].paintsBlank }
        }

        private mutating func setStyle(_ style: CellStyle) {
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

        private mutating func decode(_ bytes: UnsafeBufferPointer<UInt8>) {
            scalars.removeAll(keepingCapacity: true)
            var ascii = true
            for byte in bytes where byte >= 0x80 { ascii = false; break }
            if ascii {
                scalars.reserveCapacity(bytes.count)
                for byte in bytes { scalars.append(Unicode.Scalar(byte)) }
            } else {
                // Always valid UTF-8: it comes from a `String`, cut at line feeds and carriage returns, which are single bytes.
                scalars.append(contentsOf: String(decoding: bytes, as: UTF8.self).unicodeScalars)
            }
        }
    }
}

/// Remembers parsed lines by their content, so a live screen that differs from the last one in a line or two is not parsed again from
/// its first byte. Pass one to `TerminalText.styledScreen(_:cursor:rows:textPresentation:cache:)`; the result is the same as without.
///
/// A line is found by its raw text (escape sequences included) and by the style that is running when it starts, since SGR state
/// carries over from one line to the next. A found line is the very `StyledLine` that was built before: same text and run storage,
/// no copy. The cursor line is never taken from here.
///
/// The cache holds the lines of the last parse and of the one before it, plus a little slack; older lines are dropped, and there is a
/// hard limit on lines and bytes. It is safe to use from several threads (parses take turns).
public final class StyledLineCache: @unchecked Sendable {
    /// What the cache did since it was made or last reset, counted in lines.
    public struct Counters: Sendable, Equatable {
        /// Lines taken from the cache.
        public var hits = 0
        /// Lines parsed because the cache did not have them.
        public var misses = 0
        /// Lines parsed without asking the cache: the cursor line, and lines too long to keep.
        public var bypassed = 0
        public init(hits: Int = 0, misses: Int = 0, bypassed: Int = 0) { self.hits = hits; self.misses = misses; self.bypassed = bypassed }
    }

    /// The most lines held, and the most memory (roughly, in bytes) they may take. Past either, new lines are parsed but not kept.
    public let maximumLines: Int
    public let maximumBytes: Int
    private let lock = NSLock()
    private var store: Store

    public init(maximumLines: Int = 4096, maximumBytes: Int = 4 << 20) {
        self.maximumLines = max(1, maximumLines)
        self.maximumBytes = max(1, maximumBytes)
        store = Store(maximumLines: max(1, maximumLines), maximumBytes: max(1, maximumBytes))
    }

    public var counters: Counters { lock.withLock { store.counters } }
    public func resetCounters() { lock.withLock { store.counters = Counters() } }
    /// How many lines are held.
    public var count: Int { lock.withLock { store.count } }
    /// Roughly how many bytes the held lines take.
    public var bytes: Int { lock.withLock { store.bytes } }
    /// Drops every line (for a memory warning, or a new session).
    public func removeAll() { lock.withLock { store.removeAll() } }

    func parse(_ input: String, cursor: (x: Int, y: Int)?, rows: Int?, textPresentation forceText: Bool) -> StyledScreen {
        var input = input
        return lock.withLock {
            input.withUTF8 { store.parse($0, cursor: cursor, rows: rows, textPresentation: forceText) }
        }
    }

    // MARK: Storage

    /// A parsed line with what is needed to take it from the cache: the raw text and style it was parsed under (the key), and what
    /// the next line needs to know about it.
    final class Entry {
        let hash: UInt64
        let flags: UInt8
        let start: CellStyle
        let raw: ContiguousArray<UInt8>
        let line: StyledLine
        /// The running style after the line.
        let end: CellStyle
        /// All cells are blanks that paint nothing (the screen drops such lines at its end).
        let blank: Bool
        /// The last parse that used this entry.
        var stamp: Int

        init(hash: UInt64, flags: UInt8, start: CellStyle, raw: ContiguousArray<UInt8>, line: StyledLine, end: CellStyle, blank: Bool, stamp: Int) {
            self.hash = hash; self.flags = flags; self.start = start; self.raw = raw
            self.line = line; self.end = end; self.blank = blank; self.stamp = stamp
        }
        var weight: Int { raw.count + line.text.utf8.count + 24 * line.runs.count + 160 }
        func matches(_ bytes: UnsafeBufferPointer<UInt8>) -> Bool {
            guard raw.count == bytes.count else { return false }
            if bytes.isEmpty { return true }
            return raw.withUnsafeBufferPointer { memcmp($0.baseAddress!, bytes.baseAddress!, bytes.count) == 0 }
        }
    }

    /// Lines longer than this are parsed every time.
    static let longestLine = 4096
    /// `flags` of an entry: what changes a line besides its text and start style.
    private static let keepCells: UInt8 = 1, textPresentation: UInt8 = 2

    struct Store {
        let maximumLines: Int
        let maximumBytes: Int
        /// Open addressing, linear probing, a power of two long, never more than half full. Entries leave only in a sweep, which
        /// builds a new table, so there are no tombstones.
        private var table: [Entry?] = Array(repeating: nil, count: 256)
        private(set) var count = 0
        private(set) var bytes = 0
        var counters = Counters()
        private var serial = 0
        private var used = 0, previousUsed = 0
        private var scanner = TerminalText.LineScanner()
        private var breaks: [Int] = []

        init(maximumLines: Int, maximumBytes: Int) { self.maximumLines = maximumLines; self.maximumBytes = maximumBytes }

        mutating func removeAll() {
            table = Array(repeating: nil, count: 256)
            count = 0; bytes = 0; used = 0; previousUsed = 0
        }

        // MARK: Parse

        mutating func parse(_ input: UnsafeBufferPointer<UInt8>, cursor: (x: Int, y: Int)?, rows screenRows: Int?, textPresentation forceText: Bool) -> StyledScreen {
            serial += 1
            defer { finish() }

            // Every line feed ends a row (no escape sequence takes one in), so the rows are what lies between the line feeds.
            breaks.removeAll(keepingCapacity: true)
            if let base = input.baseAddress {
                var offset = 0
                while offset < input.count, let found = memchr(base + offset, 0x0A, input.count - offset) {
                    let at = UnsafeRawPointer(base).distance(to: UnsafeRawPointer(found))
                    breaks.append(at)
                    offset = at + 1
                }
            }
            let rowCount = breaks.count + 1
            var style = CellStyle.plain   // SGR state runs on from one line to the next

            guard let cursor, let screenRows, screenRows > 0, cursor.x >= 0, cursor.y >= 0, cursor.y < screenRows else {
                let flags = forceText ? StyledLineCache.textPresentation : 0
                var lines: [StyledLine] = []
                lines.reserveCapacity(rowCount)
                var keep = 0   // rows up to the last one that shows something: trailing blank rows are dropped
                for k in 0..<rowCount {
                    let entry = line(slice(k, input), flags: flags, start: style, textPresentation: forceText)
                    lines.append(entry.line)
                    style = entry.end
                    if !entry.blank { keep = k + 1 }
                }
                lines.removeLast(lines.count - keep)
                return TerminalText.flatten(lines, cursor: nil, historyLines: 0)
            }

            // A trailing line feed terminates the last line; it does not start another one. Whether the last row is empty does not
            // depend on the style it starts in, and only a last row with input has to be read to tell.
            var count = rowCount
            if rowCount > 1 {
                let tail = slice(rowCount - 1, input)
                var empty = tail.isEmpty
                if !empty {
                    scanner.scan(tail, start: .plain, keepCells: true, textPresentation: forceText)
                    empty = scanner.cells.isEmpty
                }
                if empty { count -= 1 }
            }
            // The lines above the screen are scrollback; the screen starts `historyLines` lines in.
            let historyLines = max(0, count - screenRows)
            let row = historyLines + cursor.y
            let flags = StyledLineCache.keepCells | (forceText ? StyledLineCache.textPresentation : 0)
            var lines: [StyledLine] = []
            lines.reserveCapacity(max(count, row + 1))
            var keep = row + 1   // blank rows after the cursor row are dropped
            var index = 0
            for k in 0..<count {
                if k == row {
                    scanner.scan(slice(k, input), start: style, keepCells: true, textPresentation: forceText)
                    var cells = scanner.cells
                    index = TerminalText.placeCursor(in: &cells, x: cursor.x)
                    lines.append(TerminalText.makeLine(cells, styles: scanner.styles))
                    style = scanner.end
                    counters.bypassed += 1
                } else {
                    let entry = line(slice(k, input), flags: flags, start: style, textPresentation: forceText)
                    lines.append(entry.line)
                    style = entry.end
                    if k > row, !entry.blank { keep = k + 1 }
                }
            }
            if row >= count {
                // The cursor is below the text: empty rows down to it, and a blank to stand on.
                var cells: [TerminalText.StyledCell] = []
                index = TerminalText.placeCursor(in: &cells, x: cursor.x)
                while lines.count < row { lines.append(TerminalText.makeLine([], styles: [.plain])) }
                lines.append(TerminalText.makeLine(cells, styles: [.plain]))
            }
            lines.removeLast(lines.count - keep)
            return TerminalText.flatten(lines, cursor: (row, index), historyLines: historyLines)
        }

        /// Row `k` as the bytes the scanner reads. The CR of a CRLF does nothing, so it is not part of the row; a CR at the very end
        /// (no line feed after it) returns to the start of the line, and stays.
        private func slice(_ k: Int, _ input: UnsafeBufferPointer<UInt8>) -> UnsafeBufferPointer<UInt8> {
            let from = k == 0 ? 0 : breaks[k - 1] + 1
            var to = k < breaks.count ? breaks[k] : input.count
            if k < breaks.count, to > from, input[to - 1] == 0x0D { to -= 1 }
            return UnsafeBufferPointer(rebasing: input[from..<to])
        }

        // MARK: Lookup

        /// The entry for a row: the cached one, or a freshly parsed one that is kept if there is room.
        private mutating func line(_ row: UnsafeBufferPointer<UInt8>, flags: UInt8, start: CellStyle, textPresentation forceText: Bool) -> Entry {
            let keepCells = flags & StyledLineCache.keepCells != 0
            guard row.count <= StyledLineCache.longestLine else {
                counters.bypassed += 1
                return parsed(row, hash: 0, flags: flags, start: start, keepCells: keepCells, textPresentation: forceText)
            }
            let hash = StyledLineCache.hash(row)
            let mask = table.count - 1
            var slot = Int(truncatingIfNeeded: hash) & mask
            while let entry = table[slot] {
                if entry.hash == hash, entry.flags == flags, entry.start == start, entry.matches(row) {
                    entry.stamp = serial
                    counters.hits += 1; used += 1
                    return entry
                }
                slot = (slot + 1) & mask
            }
            counters.misses += 1; used += 1
            let entry = parsed(row, hash: hash, flags: flags, start: start, keepCells: keepCells, textPresentation: forceText)
            if count < maximumLines, bytes + entry.weight <= maximumBytes {
                table[slot] = entry
                count += 1; bytes += entry.weight
                if count * 2 > table.count { rehash(into: table.count * 2, keeping: 0) }
            }
            return entry
        }

        private mutating func parsed(_ row: UnsafeBufferPointer<UInt8>, hash: UInt64, flags: UInt8, start: CellStyle, keepCells: Bool, textPresentation forceText: Bool) -> Entry {
            scanner.scan(row, start: start, keepCells: keepCells, textPresentation: forceText)
            return Entry(hash: hash, flags: flags, start: start, raw: ContiguousArray(row), line: TerminalText.makeLine(scanner.cells, styles: scanner.styles),
                         end: scanner.end, blank: scanner.isBlank, stamp: serial)
        }

        // MARK: Bounds

        /// After a parse: keep what the last two parses used. Dropping is done in one sweep when the cache has grown to more than twice
        /// the lines a parse touches (or is full), which costs one pass over the table and happens every few dozen answers.
        private mutating func finish() {
            if count > 2 * max(used, previousUsed) + 64 || count >= maximumLines { rehash(into: 0, keeping: serial - 1) }
            previousUsed = used
            used = 0
        }

        /// A new table with `size` slots (or just enough) holding the entries last used in parse `stamp` or later.
        private mutating func rehash(into size: Int, keeping stamp: Int) {
            var kept: [Entry] = []
            kept.reserveCapacity(count)
            for case let entry? in table where entry.stamp >= stamp { kept.append(entry) }
            var slots = max(256, size)
            while slots < kept.count * 2 + 2 { slots *= 2 }
            var fresh: [Entry?] = Array(repeating: nil, count: slots)
            let mask = slots - 1
            var weight = 0
            for entry in kept {
                var slot = Int(truncatingIfNeeded: entry.hash) & mask
                while fresh[slot] != nil { slot = (slot + 1) & mask }
                fresh[slot] = entry
                weight += entry.weight
            }
            table = fresh
            count = kept.count
            bytes = weight
        }
    }

    /// A 64-bit hash of a row's bytes: a word at a time, then mixed. Collisions only cost a comparison.
    static func hash(_ row: UnsafeBufferPointer<UInt8>) -> UInt64 {
        let length = row.count
        var h = 0x9E37_79B9_7F4A_7C15 ^ UInt64(truncatingIfNeeded: length)
        if let base = row.baseAddress {
            let raw = UnsafeRawPointer(base)
            var i = 0
            while i + 8 <= length {
                h = (h ^ raw.loadUnaligned(fromByteOffset: i, as: UInt64.self)) &* 0xFF51_AFD7_ED55_8CCD
                h ^= h >> 32
                i += 8
            }
            if i < length {
                var tail: UInt64 = 0
                var shift: UInt64 = 0
                while i < length { tail |= UInt64(base[i]) << shift; shift += 8; i += 1 }
                h = (h ^ tail) &* 0xFF51_AFD7_ED55_8CCD
                h ^= h >> 32
            }
        }
        h = (h ^ (h >> 33)) &* 0xC4CE_B9FE_1A85_EC53
        return h ^ (h >> 29)
    }
}
