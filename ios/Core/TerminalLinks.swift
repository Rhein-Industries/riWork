import Foundation

/// A web or mail link on one terminal line: the characters it covers (indexes into the line's `Character`s) and where it goes.
public struct TerminalLinkSpan: Sendable, Equatable {
    public let range: Range<Int>
    public let url: URL
    public init(range: Range<Int>, url: URL) { self.range = range; self.url = url }
}

/// The `http`, `https` and `mailto` URLs in terminal text, found the way the desktop finds them under a ⌘-click (`src/terminal_links.rs`):
///
/// - A URL starts at a scheme that does not follow a letter or digit (`xhttp://` is not one) and runs over URL characters, taken
///   liberally: anything but blanks, controls, `< > " ` \ ^ { } |`, `…` and box drawing. The earliest start wins, so
///   `https://a.example/?next=https://b.example` is one URL.
/// - The end is trimmed of sentence punctuation and of closing brackets that have no partner inside the URL: `(https://example.com).` is
///   `https://example.com`; the parentheses of `https://en.wikipedia.org/wiki/Rust_(video_game)` stay.
/// - A URL that runs into `…` was cut short on screen and is not a link; nor is a scheme alone (`https://`).
///
/// **Wrapped lines.** The protocol sends one line per screen row and does not say which rows the terminal wrapped (`capture-pane -J`
/// is not used), so a wrap is inferred: a row as wide as the pane (`wrapColumns`) continues on the next row. That joins every URL tmux
/// wrapped. It also joins a row that happens to end exactly at the edge with a hard line break after it, which matters only when a URL
/// touches that edge and the next row starts with URL characters. Without the pane width nothing is joined.
///
/// OSC 8 hyperlinks never reach the phone (the desktop removes every OSC from `shell.output`), so only the text is read.
public enum TerminalLinks {
    /// Longest URL taken, as on the desktop.
    public static let maximumLength = 4096
    /// Most rows one wrapped line is followed over, up and down from a row.
    public static let maximumWrappedRows = 32

    private static let schemes: [[Character]] = ["https://", "http://", "mailto:"].map(Array.init)

    // MARK: Characters

    /// A character a URL can hold; the end is trimmed separately.
    static func isURLCharacter(_ character: Character) -> Bool {
        if character.isWhitespace { return false }
        guard let first = character.unicodeScalars.first else { return false }
        switch first.value {
        case 0x00...0x1F, 0x7F...0x9F: return false
        // `<` `>` `"` `` ` `` `\` `^` `{` `}` `|` `…`
        case 0x3C, 0x3E, 0x22, 0x60, 0x5C, 0x5E, 0x7B, 0x7D, 0x7C, 0x2026: return false
        // Box drawing and blocks frame text in agent output.
        case 0x2500...0x259F: return false
        default: return true
        }
    }

    /// The length of the scheme that starts at `index`, compared without case, or nil.
    private static func scheme(in characters: [Character], at index: Int) -> Int? {
        for scheme in schemes where index + scheme.count <= characters.count {
            var matches = true
            for (offset, expected) in scheme.enumerated() where characters[index + offset].lowercased() != String(expected) {
                matches = false
                break
            }
            if matches { return scheme.count }
        }
        return nil
    }

    /// Sentence punctuation and unbalanced closing brackets off the end of `characters[start..<end]`.
    static func trimmedEnd(_ characters: [Character], start: Int, end: Int) -> Int {
        var end = end
        while end > start {
            func count(_ character: Character) -> Int { characters[start..<end].reduce(0) { $0 + ($1 == character ? 1 : 0) } }
            let drop: Bool = switch characters[end - 1] {
            case ".", ",", ";", ":", "!", "?", "'", "*": true
            case ")": count(")") > count("(")
            case "]": count("]") > count("[")
            default: false
            }
            guard drop else { break }
            end -= 1
        }
        return end
    }

    // MARK: One run of text

    /// The URLs in `characters`, as ranges and targets.
    static func find(in characters: [Character]) -> [(range: Range<Int>, url: URL)] {
        var found: [(Range<Int>, URL)] = []
        var index = 0
        let count = characters.count
        while index < count {
            let character = characters[index]
            // Only `h` and `m` start a scheme; the boundary rules out `xhttp://`.
            guard character == "h" || character == "H" || character == "m" || character == "M",
                  index == 0 || !characters[index - 1].isLetter && !characters[index - 1].isNumber,
                  let length = scheme(in: characters, at: index) else { index += 1; continue }
            var end = index + length
            while end < count, end - index < maximumLength, isURLCharacter(characters[end]) { end += 1 }
            // `https://host/a…` was cut short on screen; opening it would open another page.
            if end < count, characters[end] == "…" { index = end + 1; continue }
            let trimmed = trimmedEnd(characters, start: index + length, end: end)
            let text = String(characters[index..<trimmed])
            // A scheme alone, `https://`, is not an address.
            if characters[(index + length)..<trimmed].contains(where: { $0.isLetter || $0.isNumber }),
               let url = URL(string: text.replacingOccurrences(of: "\u{FE0E}", with: "")), url.scheme != nil {
                found.append((index..<trimmed, url))
            }
            index = max(end, index + 1)
        }
        return found
    }

    /// The links on a single line of text, nothing joined.
    public static func spans(in text: String) -> [TerminalLinkSpan] {
        guard mayHoldLink(text) else { return [] }
        return find(in: Array(text)).map { TerminalLinkSpan(range: $0.range, url: $0.url) }
    }

    /// Cheap test: every scheme has a colon. Most terminal lines have none, and are not looked at further.
    static func mayHoldLink(_ text: String) -> Bool { text.utf8.contains(0x3A) }

    // MARK: Rows

    /// Whether a row fills the pane exactly, so that the terminal wrapped it onto the next row (see the type's notes). A row wider than
    /// the pane is from before a resize and is read alone.
    static func continues(_ line: StyledLine, wrapColumns: Int?) -> Bool {
        guard let wrapColumns, wrapColumns > 0 else { return false }
        return !line.isMissing && line.columns == wrapColumns
    }

    /// The rows of the wrapped line that `row` is part of: up while the row above fills the pane, down while the row itself does.
    /// `line` answers nil past the rows held.
    public static func wrappedRows(around row: Int, wrapColumns: Int?, line: (Int) -> StyledLine?) -> Range<Int> {
        var first = row
        while row - first < maximumWrappedRows, let above = line(first - 1), continues(above, wrapColumns: wrapColumns) { first -= 1 }
        var last = row
        while last - first < maximumWrappedRows, let current = line(last), continues(current, wrapColumns: wrapColumns), line(last + 1) != nil { last += 1 }
        return first..<(last + 1)
    }

    /// The links of consecutive rows that form one wrapped line, read as the one line they are, then cut back into rows: a URL that
    /// crosses a row edge has a span on each of its rows, all with the same target.
    public static func spans(inWrapped lines: [StyledLine]) -> [[TerminalLinkSpan]] {
        var result = Array(repeating: [TerminalLinkSpan](), count: lines.count)
        guard lines.contains(where: { mayHoldLink($0.text) }) else { return result }
        var characters: [Character] = []
        var starts: [Int] = []
        for line in lines {
            starts.append(characters.count)
            characters.append(contentsOf: line.text)
        }
        starts.append(characters.count)
        for (range, url) in find(in: characters) {
            for row in lines.indices {
                let lower = max(range.lowerBound, starts[row]), upper = min(range.upperBound, starts[row + 1])
                if lower < upper { result[row].append(TerminalLinkSpan(range: (lower - starts[row])..<(upper - starts[row]), url: url)) }
            }
        }
        return result
    }
}
