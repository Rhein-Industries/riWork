import Foundation

// Markdown for agent chat messages. The chat view draws paragraphs, headings and list items with SwiftUI `Text(AttributedString)`,
// so this file does the BLOCK structure (headings, code, lists, quotes, tables, rules) and leaves inline syntax (emphasis, code
// spans, links) as source strings that `ChatMarkdown.inline` turns into styled text. Everything here is pure and `Sendable`.
//
// The text is STREAMING: the agent's reply is parsed again after every delta, so any prefix of a valid message must give
// sensible blocks and never trap (a fence that is not closed yet is `.code(closed: false)`, a lone `#` or `-` is a paragraph, a
// table without its delimiter row is a paragraph until the row arrives). Each line is looked at a bounded number of times per
// nesting level and the nesting depth is capped, so a long or hostile message cannot make a delta slow.

/// One block of a message.
public indirect enum ChatMarkdownBlock: Sendable, Equatable {
    /// `level` is 1...6; `text` is inline-markdown source without the `#`s.
    case heading(level: Int, text: String)
    /// Inline-markdown source; the lines of the paragraph are joined with "\n" because the view keeps the line breaks.
    case paragraph(String)
    /// Fenced (``` or ~~~) or indented code. `text` has no trailing newline. `closed` is false while a streamed fence has not
    /// been closed yet, so the view can show it as still arriving.
    case code(language: String?, text: String, closed: Bool)
    case list(ordered: Bool, start: Int, items: [ChatMarkdownListItem])
    case quote([ChatMarkdownBlock])
    case rule
    /// A GitHub pipe table. Cells are inline-markdown source; every row has exactly as many cells as the header.
    case table(header: [String], rows: [[String]])
}

public struct ChatMarkdownListItem: Sendable, Equatable {
    /// "- [ ]" is false, "- [x]" is true, any other item is nil.
    public var checked: Bool?
    /// The first block is normally a `.paragraph`; nested lists and code follow it.
    public var blocks: [ChatMarkdownBlock]
    public init(checked: Bool? = nil, blocks: [ChatMarkdownBlock]) { self.checked = checked; self.blocks = blocks }
}

public enum ChatMarkdown {
    /// The blocks of a message. Empty source (or only blank lines) is no blocks. Never empty paragraphs, never throws.
    ///
    /// Not implemented on purpose: setext headings (`Title` over `===`), HTML blocks, footnotes and reference-style links. They
    /// stay plain paragraphs, which is what an agent that wrote them for a terminal expects to see.
    public static func parse(_ source: String) -> [ChatMarkdownBlock] {
        guard !source.isEmpty else { return [] }
        return MarkdownBlocks.blocks(MarkdownBlocks.lines(of: source), depth: 0)
    }

    /// Inline markdown to styled text for display: emphasis, code spans, strikethrough and links, with the line breaks kept. Text
    /// the parser cannot read (a half-typed `**bo`, anything odd) comes back as plain text, so this never throws.
    ///
    /// Agent text is untrusted, so a link survives ONLY when its scheme is http or https: `javascript:`, `tel:`, `file:`, the app's
    /// own `riwork:` and every other scheme (and a link with no scheme at all) lose the link and keep the words, so nothing a model
    /// writes can make a tap do more than open a web page.
    public static func inline(_ source: String) -> AttributedString {
        var options = AttributedString.MarkdownParsingOptions()
        options.interpretedSyntax = .inlineOnlyPreservingWhitespace
        options.failurePolicy = .returnPartiallyParsedIfPossible
        var text = (try? AttributedString(markdown: source, options: options)) ?? AttributedString(source)
        var unsafe: [Range<AttributedString.Index>] = []
        for (link, range) in text.runs[\.link] {
            if let link, !isWebLink(link) { unsafe.append(range) }
        }
        for range in unsafe { text[range].link = nil }
        return text
    }

    private static func isWebLink(_ url: URL) -> Bool {
        guard let scheme = url.scheme?.lowercased() else { return false }
        return scheme == "http" || scheme == "https"
    }
}

// MARK: - Block parser

/// The block parser. A container (quote, list item) collects its own lines with the container's markers and indentation taken off
/// and parses them again one level deeper, which keeps every rule local to one kind of block.
private enum MarkdownBlocks {
    /// Quotes and list items nested deeper than this are plain text. It bounds both the recursion and the work per line, which is
    /// what keeps a 100 KB run of `>` or `- ` from being slow.
    static let maximumDepth = 12

    struct ListMarker {
        var ordered: Bool
        var number: Int
        /// The column where the item's text starts; continuation lines indented at least this far belong to the item.
        var contentColumn: Int
        var content: Substring
    }

    // MARK: Lines and indentation

    /// "\n", "\r\n" and a lone "\r" end a line. Whatever follows the last one is a line too (empty after a final newline), and blank
    /// lines are ignored by the parser, so a streamed prefix that stops after "\r" of "\r\n" parses like the whole.
    static func lines(of source: String) -> [Substring] {
        var result: [Substring] = []
        let bytes = source.utf8
        var start = source.startIndex
        var index = start
        while index < bytes.endIndex {
            let byte = bytes[index]
            if byte == 10 || byte == 13 {
                result.append(source[start..<index])
                var next = bytes.index(after: index)
                if byte == 13, next < bytes.endIndex, bytes[next] == 10 { next = bytes.index(after: next) }
                start = next
                index = next
            } else {
                index = bytes.index(after: index)
            }
        }
        result.append(source[start...])
        return result
    }

    /// The leading blanks of a line as a width in columns (a tab goes to the next multiple of 4; `column` is where the text sits
    /// in the line, for blanks after a list marker) and what follows them. Tabs count as columns only here; the text keeps them.
    static func measure(_ line: Substring, from column: Int = 0) -> (columns: Int, rest: Substring) {
        var columns = column
        var index = line.startIndex
        let bytes = line.utf8
        while index < bytes.endIndex {
            let byte = bytes[index]
            if byte == 32 { columns += 1 } else if byte == 9 { columns += 4 - columns % 4 } else { break }
            index = bytes.index(after: index)
        }
        return (columns - column, line[index...])
    }

    /// The line with up to `limit` columns of leading blanks taken off. A tab that straddles the limit is replaced by the blanks
    /// that are left of it.
    static func strip(_ line: Substring, columns limit: Int) -> Substring {
        var columns = 0
        var index = line.startIndex
        let bytes = line.utf8
        while index < bytes.endIndex, columns < limit {
            let byte = bytes[index]
            if byte == 32 {
                columns += 1
            } else if byte == 9 {
                let width = 4 - columns % 4
                if columns + width > limit {
                    return Substring(String(repeating: " ", count: columns + width - limit) + line[bytes.index(after: index)...])
                }
                columns += width
            } else {
                break
            }
            index = bytes.index(after: index)
        }
        return line[index...]
    }

    static func rtrim(_ text: Substring) -> Substring {
        let bytes = text.utf8
        var end = text.endIndex
        while end > text.startIndex {
            let before = bytes.index(before: end)
            guard bytes[before] == 32 || bytes[before] == 9 else { break }
            end = before
        }
        return text[text.startIndex..<end]
    }

    static func trimmed(_ text: Substring) -> Substring { rtrim(measure(text).rest) }

    // MARK: Blocks

    static func blocks(_ lines: [Substring], depth: Int) -> [ChatMarkdownBlock] {
        let nests = depth < maximumDepth
        var result: [ChatMarkdownBlock] = []
        var paragraph: [Substring] = []
        func flush() {
            guard !paragraph.isEmpty else { return }
            result.append(.paragraph(paragraph.joined(separator: "\n")))
            paragraph.removeAll(keepingCapacity: true)
        }
        var i = 0
        while i < lines.count {
            let (indent, rest) = measure(lines[i])
            if rest.isEmpty { flush(); i += 1; continue }
            if indent >= 4 {
                // Four columns in is code, unless it continues a paragraph: an indented line never interrupts one.
                if paragraph.isEmpty {
                    let (block, next) = indentedCode(lines, from: i)
                    result.append(block)
                    i = next
                } else {
                    paragraph.append(rtrim(rest))
                    i += 1
                }
                continue
            }
            if let fence = fenceOpening(rest) {
                flush()
                let (block, next) = fencedCode(lines, from: i, indent: indent, fence: fence)
                result.append(block)
                i = next
                continue
            }
            // Before lists: `* * *` and `- - -` are rules, not bullets.
            if isThematicBreak(rest) { flush(); result.append(.rule); i += 1; continue }
            if let heading = atxHeading(rest) { flush(); result.append(heading); i += 1; continue }
            if nests, rest.utf8.first == 62, let (block, next) = quote(lines, from: i, depth: depth) {
                flush()
                result.append(block)
                i = next
                continue
            }
            // A bullet interrupts a paragraph, an ordered item only when it starts at 1 (so "in 2024. we" stays a sentence).
            if nests, let marker = listMarker(rest, indent: indent), paragraph.isEmpty || !marker.ordered || marker.number == 1 {
                flush()
                let (block, next) = list(lines, from: i, marker: marker, indent: indent, depth: depth)
                result.append(block)
                i = next
                continue
            }
            if rest.utf8.contains(124), let (block, next) = table(lines, from: i) {
                flush()
                result.append(block)
                i = next
                continue
            }
            paragraph.append(rtrim(rest))
            i += 1
        }
        flush()
        return result
    }

    // MARK: Headings and rules

    /// 1-6 `#`, a blank, then text. `#hashtag`, seven `#`s and a heading with no text yet (`#`, `# `) are not headings, so a
    /// streamed `#` does not flash an empty heading.
    static func atxHeading(_ rest: Substring) -> ChatMarkdownBlock? {
        let bytes = rest.utf8
        var index = bytes.startIndex
        var level = 0
        while index < bytes.endIndex, bytes[index] == 35, level < 7 { level += 1; index = bytes.index(after: index) }
        guard (1...6).contains(level), index < bytes.endIndex, bytes[index] == 32 || bytes[index] == 9 else { return nil }
        var text = trimmed(rest[index...])
        // An optional closing run of `#`s ("## Title ##") is not part of the text; "C#" keeps its own.
        let textBytes = text.utf8
        var end = text.endIndex
        while end > text.startIndex, textBytes[textBytes.index(before: end)] == 35 { end = textBytes.index(before: end) }
        if end < text.endIndex {
            let head = text[text.startIndex..<end]
            if head.isEmpty { text = head } else if let last = head.utf8.last, last == 32 || last == 9 { text = rtrim(head) }
        }
        return text.isEmpty ? nil : .heading(level: level, text: String(text))
    }

    /// Three or more of the same `-`, `*` or `_`, with blanks allowed between.
    static func isThematicBreak(_ rest: Substring) -> Bool {
        guard let first = rest.utf8.first, first == 45 || first == 42 || first == 95 else { return false }
        var count = 0
        for byte in rest.utf8 {
            if byte == first { count += 1 } else if byte != 32 && byte != 9 { return false }
        }
        return count >= 3
    }

    // MARK: Code

    /// A run of three or more backticks or tildes. A backtick fence cannot have a backtick in its info string (that is inline code
    /// that happens to start a line).
    static func fenceOpening(_ rest: Substring) -> (byte: UInt8, count: Int, info: Substring)? {
        let bytes = rest.utf8
        guard let first = bytes.first, first == 96 || first == 126 else { return nil }
        var index = bytes.startIndex
        var count = 0
        while index < bytes.endIndex, bytes[index] == first { count += 1; index = bytes.index(after: index) }
        guard count >= 3 else { return nil }
        let info = trimmed(rest[index...])
        if first == 96, info.utf8.contains(96) { return nil }
        return (first, count, info)
    }

    static func closesFence(_ rest: Substring, byte: UInt8, count: Int) -> Bool {
        let bytes = rest.utf8
        var index = bytes.startIndex
        var run = 0
        while index < bytes.endIndex, bytes[index] == byte { run += 1; index = bytes.index(after: index) }
        guard run >= count else { return false }
        return rest[index...].utf8.allSatisfy { $0 == 32 || $0 == 9 }
    }

    /// Everything up to a closing fence of the same character that is at least as long, or to the end of the container (then
    /// `closed` is false). The opening fence's own indentation is taken off the content lines.
    static func fencedCode(_ lines: [Substring], from start: Int, indent: Int, fence: (byte: UInt8, count: Int, info: Substring)) -> (ChatMarkdownBlock, Int) {
        var body: [Substring] = []
        var closed = false
        var i = start + 1
        while i < lines.count {
            let (lineIndent, rest) = measure(lines[i])
            i += 1
            if lineIndent <= 3, closesFence(rest, byte: fence.byte, count: fence.count) { closed = true; break }
            body.append(strip(lines[i - 1], columns: indent))
        }
        let language = fence.info.split(whereSeparator: { $0 == " " || $0 == "\t" }).first.map(String.init)
        return (.code(language: language, text: body.joined(separator: "\n"), closed: closed), i)
    }

    /// Lines indented four columns or more (blank lines between them included), up to the first line that is not. Trailing blank
    /// lines are not part of it.
    static func indentedCode(_ lines: [Substring], from start: Int) -> (ChatMarkdownBlock, Int) {
        var body: [Substring] = []
        var kept = 0
        var i = start
        while i < lines.count {
            let (indent, rest) = measure(lines[i])
            if rest.isEmpty {
                body.append("")
            } else if indent >= 4 {
                body.append(strip(lines[i], columns: 4))
                kept = body.count
            } else {
                break
            }
            i += 1
        }
        return (.code(language: nil, text: body.prefix(kept).joined(separator: "\n"), closed: true), i)
    }

    // MARK: Quotes

    /// Consecutive `>` lines (one optional blank after the `>` is part of the marker), parsed as blocks of their own. A quote with
    /// nothing in it yet (a streamed `>`) is not one; the caller keeps the line as text. No lazy continuation: a line without `>`
    /// ends the quote.
    static func quote(_ lines: [Substring], from start: Int, depth: Int) -> (ChatMarkdownBlock, Int)? {
        var inner: [Substring] = []
        var i = start
        while i < lines.count {
            let (indent, rest) = measure(lines[i])
            guard indent <= 3, rest.utf8.first == 62 else { break }
            inner.append(strip(rest[rest.utf8.index(after: rest.startIndex)...], columns: 1))
            i += 1
        }
        guard inner.contains(where: { !measure($0).rest.isEmpty }) else { return nil }
        return (.quote(blocks(inner, depth: depth + 1)), i)
    }

    // MARK: Lists

    /// `-`, `*` or `+`, or up to nine digits and `.` or `)`, then a blank and some text. An empty item (`-`, `1.`, `- `) is not a
    /// list yet: a streamed marker stays a paragraph until its text starts.
    static func listMarker(_ rest: Substring, indent: Int) -> ListMarker? {
        let bytes = rest.utf8
        guard let first = bytes.first else { return nil }
        var index = bytes.startIndex
        var ordered = false
        var number = 0
        var width = 1
        if first == 45 || first == 42 || first == 43 {
            index = bytes.index(after: index)
        } else if first >= 48 && first <= 57 {
            ordered = true
            var digits = 0
            while index < bytes.endIndex, bytes[index] >= 48, bytes[index] <= 57, digits < 10 {
                number = number * 10 + Int(bytes[index] - 48)
                digits += 1
                index = bytes.index(after: index)
            }
            guard digits <= 9, index < bytes.endIndex, bytes[index] == 46 || bytes[index] == 41 else { return nil }
            index = bytes.index(after: index)
            width = digits + 1
        } else {
            return nil
        }
        guard index < bytes.endIndex, bytes[index] == 32 || bytes[index] == 9 else { return nil }
        let (gap, content) = measure(rest[index...], from: indent + width)
        guard !content.isEmpty else { return nil }
        // Five blanks or more after the marker is a marker plus one blank and then indented text.
        return ListMarker(ordered: ordered, number: number, contentColumn: indent + width + (gap <= 4 ? gap : 1), content: rtrim(content))
    }

    /// One list: the item at `start` and every following item of the same kind, with the lines that belong to each.
    ///
    /// A line belongs to the item above it when it is indented to the item's text (CommonMark), or at least two columns past its
    /// marker. The second rule is for agents that nest under `1. ` with two spaces, where CommonMark would end the list. Blank
    /// lines inside the list are kept (one list), a line that starts another item of the same kind is the next item, and anything
    /// else ends the list, including text with no indent: there is no lazy continuation, because agents rarely wrap lines and mean
    /// that text as a new paragraph.
    ///
    /// Bullets are one kind whatever the character, ordered items are one kind whatever the delimiter; bullet and ordered lists
    /// are different lists.
    static func list(_ lines: [Substring], from start: Int, marker first: ListMarker, indent firstIndent: Int, depth: Int) -> (ChatMarkdownBlock, Int) {
        var items: [ChatMarkdownListItem] = []
        var marker = first
        var indent = firstIndent
        var i = start
        while true {
            var itemLines: [Substring] = [marker.content]
            let threshold = min(marker.contentColumn, indent + 2)
            var j = i + 1
            var blanks = 0
            var sibling: (marker: ListMarker, indent: Int)?
            while j < lines.count {
                let (lineIndent, rest) = measure(lines[j])
                if rest.isEmpty { blanks += 1; j += 1; continue }
                if lineIndent >= threshold {
                    for _ in 0..<blanks { itemLines.append("") }
                    itemLines.append(strip(lines[j], columns: marker.contentColumn))
                    blanks = 0
                    j += 1
                    continue
                }
                if lineIndent <= 3, let next = listMarker(rest, indent: lineIndent), next.ordered == first.ordered { sibling = (next, lineIndent) }
                break
            }
            items.append(item(itemLines, depth: depth))
            guard let next = sibling else {
                return (.list(ordered: first.ordered, start: first.ordered ? first.number : 1, items: items), j - blanks)
            }
            marker = next.marker
            indent = next.indent
            i = j
        }
    }

    static func item(_ lines: [Substring], depth: Int) -> ChatMarkdownListItem {
        var lines = lines
        var checked: Bool?
        if let task = taskMarker(lines[0]) {
            checked = task.checked
            if task.rest.isEmpty { lines.removeFirst() } else { lines[0] = task.rest }
        }
        return ChatMarkdownListItem(checked: checked, blocks: blocks(lines, depth: depth + 1))
    }

    /// `[ ]`, `[x]` or `[X]` at the start of an item's text, followed by a blank or the end of the line.
    static func taskMarker(_ content: Substring) -> (checked: Bool, rest: Substring)? {
        let head = Array(content.utf8.prefix(4))
        guard head.count >= 3, head[0] == 91, head[2] == 93 else { return nil }
        let checked: Bool
        switch head[1] {
        case 32: checked = false
        case 120, 88: checked = true
        default: return nil
        }
        if head.count == 4, head[3] != 32, head[3] != 9 { return nil }
        return (checked, trimmed(content[content.utf8.index(content.startIndex, offsetBy: 3)...]))
    }

    // MARK: Tables

    /// A header row, a delimiter row of the same width, then rows while they have a pipe. The header may directly follow a
    /// paragraph line (agents often leave out the blank line). Without the delimiter row the lines are a paragraph, which is what a
    /// streamed table is until its second line arrives.
    static func table(_ lines: [Substring], from start: Int) -> (ChatMarkdownBlock, Int)? {
        guard start + 1 < lines.count else { return nil }
        let head = splitRow(trimmed(lines[start]))
        guard head.pipes > 0, !head.cells.isEmpty else { return nil }
        let (indent, rest) = measure(lines[start + 1])
        guard indent <= 3 else { return nil }
        let delimiter = splitRow(rest)
        guard delimiter.pipes > 0, delimiter.cells.count == head.cells.count, delimiter.cells.allSatisfy(isDelimiterCell) else { return nil }
        var rows: [[String]] = []
        var i = start + 2
        while i < lines.count {
            let row = splitRow(trimmed(lines[i]))
            guard row.pipes > 0 else { break }
            var cells = Array(row.cells.prefix(head.cells.count))
            while cells.count < head.cells.count { cells.append("") }
            rows.append(cells)
            i += 1
        }
        return (.table(header: head.cells, rows: rows), i)
    }

    /// `---`, `:--`, `--:` or `:-:`.
    static func isDelimiterCell(_ cell: String) -> Bool {
        var text = Substring(cell)
        if text.hasPrefix(":") { text = text.dropFirst() }
        if text.hasSuffix(":") { text = text.dropLast() }
        return !text.isEmpty && text.allSatisfy { $0 == "-" }
    }

    /// The trimmed cells of a row and how many unescaped pipes it has. The pipes at the edges are not separators (`| a | b |` has
    /// two cells), and `\|` is a pipe inside the cell, written as a plain `|` (as GitHub does) so it also reads right in a code span.
    static func splitRow(_ line: Substring) -> (cells: [String], pipes: Int) {
        var pieces: [String] = []
        var current = String.UnicodeScalarView()
        var pipes = 0
        var escaped = false
        for scalar in line.unicodeScalars {
            if escaped {
                escaped = false
                if scalar == "|" { current.append(scalar) } else { current.append("\\"); current.append(scalar) }
            } else if scalar == "\\" {
                escaped = true
            } else if scalar == "|" {
                pieces.append(String(current))
                current = String.UnicodeScalarView()
                pipes += 1
            } else {
                current.append(scalar)
            }
        }
        if escaped { current.append("\\") }
        pieces.append(String(current))
        func isBlank(_ text: String) -> Bool { trimmed(Substring(text)).isEmpty }
        if pipes > 0, let first = pieces.first, isBlank(first) { pieces.removeFirst() }
        if pipes > 0, let last = pieces.last, isBlank(last) { pieces.removeLast() }
        return (pieces.map { String(trimmed(Substring($0))) }, pipes)
    }
}
