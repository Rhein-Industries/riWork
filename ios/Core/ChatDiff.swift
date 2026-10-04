import Foundation

// The unified diff of a file-change card: a list of lines the view draws in monospace, green for added and red for removed.
//
// A provider may send a real `git diff` (`diff --git`, `index`, `---`/`+++`, `@@ -a,b +c,d @@`), only hunks, a bare list of `+`/`-`
// lines with no header at all, or, for a new file, the plain content. Reading is lenient and never fails. The text can be tens of
// megabytes (a generated file), so it is read in one pass over its UTF-8 bytes: lines are classified in place, only the first
// `maxLines` become strings, and the counts keep running to the end. Everything here is pure and `Sendable`.

public enum ChatDiffLineKind: Sendable, Equatable { case added, removed, context, hunk, meta }

public struct ChatDiffLine: Sendable, Equatable {
    public var kind: ChatDiffLineKind
    /// The line as written, without the leading `+`, `-` or blank of an added, removed or context line; hunk and meta lines keep
    /// their whole text.
    public var text: String
    /// Line numbers from the hunk header's counters. Nil on hunk and meta lines, on lines outside any hunk, `oldLine` on an added
    /// line and `newLine` on a removed one.
    public var oldLine: Int?
    public var newLine: Int?
    public init(kind: ChatDiffLineKind, text: String, oldLine: Int? = nil, newLine: Int? = nil) {
        self.kind = kind; self.text = text; self.oldLine = oldLine; self.newLine = newLine
    }
}

public struct ChatDiff: Sendable, Equatable {
    /// At most `maxLines` of them.
    public var lines: [ChatDiffLine]
    /// Counted over the whole input, not just the kept lines, so the card header is right when the body is cut.
    public var added: Int
    public var removed: Int
    /// Lines left out by the cap; 0 when none.
    public var hiddenLines: Int
    /// No lines at all: nothing to show and nothing cut.
    public var isEmpty: Bool { lines.isEmpty && hiddenLines == 0 }

    public init(lines: [ChatDiffLine] = [], added: Int = 0, removed: Int = 0, hiddenLines: Int = 0) {
        self.lines = lines; self.added = added; self.removed = removed; self.hiddenLines = hiddenLines
    }

    /// The lines of `text`. A "\r" before a newline is not part of the line, and the empty piece after a final newline is not a
    /// line. A line longer than `maxLineLength` characters is cut there (on a character boundary, never inside an emoji) and ends
    /// in "…", so one minified file cannot make a card that scrolls for ever.
    public static func parse(_ text: String, maxLines: Int = 2000, maxLineLength: Int = 4000) -> ChatDiff {
        var source = text
        let cap = max(0, maxLines)
        let width = max(1, maxLineLength)
        return source.withUTF8 { bytes in
            var diff = ChatDiff()
            guard let base = bytes.baseAddress else { return diff }
            var scanner = DiffScanner(bytes: bytes)
            let end = bytes.count
            var position = 0
            while position < end {
                var stop = end
                if let found = memchr(base + position, 10, end - position) { stop = UnsafeRawPointer(base).distance(to: UnsafeRawPointer(found)) }
                var lineEnd = stop
                if lineEnd > position, bytes[lineEnd - 1] == 13 { lineEnd -= 1 }
                let line = scanner.classify(position, lineEnd, next: stop + 1)
                if line.kind == .added { diff.added += 1 } else if line.kind == .removed { diff.removed += 1 }
                if diff.lines.count < cap {
                    diff.lines.append(ChatDiffLine(kind: line.kind, text: string(bytes, position + line.skip, lineEnd, limit: width), oldLine: line.old, newLine: line.new))
                } else {
                    diff.hiddenLines += 1
                }
                position = stop + 1
            }
            return diff
        }
    }

    private static func string(_ bytes: UnsafeBufferPointer<UInt8>, _ lo: Int, _ hi: Int, limit: Int) -> String {
        let text = String(decoding: UnsafeBufferPointer(rebasing: bytes[lo..<hi]), as: UTF8.self)
        // Fewer bytes than the limit means fewer characters, so most lines skip the walk.
        guard hi - lo > limit, let cut = text.index(text.startIndex, offsetBy: limit, limitedBy: text.endIndex), cut < text.endIndex else { return text }
        return String(text[..<cut]) + "…"
    }
}

/// Decides what each line is, with the state a diff carries from line to line: how many lines the current hunk still owes, where
/// the line numbers stand, and whether a header has been seen.
private struct DiffScanner {
    let bytes: UnsafeBufferPointer<UInt8>
    /// Lines the hunk header promised and have not come yet. While either is above zero a line is judged by its first byte alone,
    /// so a removed line that reads `-- note` (`--- note`) is a removed line and not a file header.
    var oldLeft = 0
    var newLeft = 0
    /// The numbers the next old and new line get; nil before the first hunk header and after a `diff --git` (no counters to
    /// count from). They keep counting after the counters run out, so a header with a wrong count still numbers its lines.
    var oldNext: Int?
    var newNext: Int?
    /// A `diff --git`, a `---`/`+++` pair or a hunk header has been seen: from then on `--- ` and `+++ ` are file headers, and a
    /// line that starts with a blank is a context line (so its blank is the marker). Without one, the same lines are plain
    /// content, and a leading blank belongs to the text.
    var sawHeader = false

    /// Header lines that start with these words are `.meta` outside a hunk (`index` is checked separately).
    private static let metaPrefixes: [StaticString] = [
        "new file mode", "deleted file mode", "old mode", "new mode", "similarity index", "dissimilarity index",
        "rename from", "rename to", "copy from", "copy to", "Binary files", "GIT binary patch",
    ]

    var inHunk: Bool { oldLeft > 0 || newLeft > 0 }

    /// `lo..<hi` is the line without its newline; `next` is where the next line starts (it may be past the end).
    /// `skip` is how many bytes of the marker come off the text.
    mutating func classify(_ lo: Int, _ hi: Int, next: Int) -> (kind: ChatDiffLineKind, skip: Int, old: Int?, new: Int?) {
        guard hi > lo else { return context(skip: 0) }
        let first = bytes[lo]
        // Not "\" alone: LaTeX and shell files have lines that start with a backslash.
        if first == 92, starts(lo, hi, "\\ No newline") { return (.meta, 0, nil, nil) }
        if inHunk {
            if starts(lo, hi, "diff --git ") || (starts(lo, hi, "@@ ") && hunkHeader(lo, hi) != nil) {
                // The counters were too big: the next file or hunk begins here.
                oldLeft = 0
                newLeft = 0
            } else {
                switch first {
                case 43: return added()
                case 45: return removed()
                case 32: return context(skip: 1)
                default: return context(skip: 0)
                }
            }
        }
        if starts(lo, hi, "diff --git ") {
            sawHeader = true
            oldNext = nil
            newNext = nil
            return (.meta, 0, nil, nil)
        }
        if first == 64, starts(lo, hi, "@@") {
            sawHeader = true
            if let header = hunkHeader(lo, hi) {
                oldLeft = header.oldCount
                newLeft = header.newCount
                oldNext = header.oldStart
                newNext = header.newStart
            } else {
                oldNext = nil
                newNext = nil
            }
            return (.hunk, 0, nil, nil)
        }
        switch first {
        case 45:
            // `--- a/p` followed by `+++ b/p` is a file header even when nothing came before it; alone it is a removed line.
            if starts(lo, hi, "--- "), sawHeader || starts(next, bytes.count, "+++ ") {
                sawHeader = true
                return (.meta, 0, nil, nil)
            }
            return removed()
        case 43:
            if sawHeader, starts(lo, hi, "+++ ") { return (.meta, 0, nil, nil) }
            return added()
        case 32:
            return context(skip: sawHeader ? 1 : 0)
        default:
            if isIndexLine(lo, hi) || Self.metaPrefixes.contains(where: { starts(lo, hi, $0) }) { return (.meta, 0, nil, nil) }
            return context(skip: 0)
        }
    }

    private mutating func added() -> (kind: ChatDiffLineKind, skip: Int, old: Int?, new: Int?) {
        let number = newNext
        newNext = newNext.map { $0 + 1 }
        newLeft = max(0, newLeft - 1)
        return (.added, 1, nil, number)
    }

    private mutating func removed() -> (kind: ChatDiffLineKind, skip: Int, old: Int?, new: Int?) {
        let number = oldNext
        oldNext = oldNext.map { $0 + 1 }
        oldLeft = max(0, oldLeft - 1)
        return (.removed, 1, number, nil)
    }

    private mutating func context(skip: Int) -> (kind: ChatDiffLineKind, skip: Int, old: Int?, new: Int?) {
        let old = oldNext
        let new = newNext
        oldNext = oldNext.map { $0 + 1 }
        newNext = newNext.map { $0 + 1 }
        oldLeft = max(0, oldLeft - 1)
        newLeft = max(0, newLeft - 1)
        return (.context, skip, old, new)
    }

    // MARK: Reading bytes

    private func starts(_ lo: Int, _ hi: Int, _ prefix: StaticString) -> Bool {
        let count = prefix.utf8CodeUnitCount
        guard hi - lo >= count else { return false }
        let expected = prefix.utf8Start
        for offset in 0..<count where bytes[lo + offset] != expected[offset] { return false }
        return true
    }

    /// `index 83db48f..bf2a3d7 100644`: a git line, not a line of code that begins with the word `index`.
    private func isIndexLine(_ lo: Int, _ hi: Int) -> Bool {
        guard starts(lo, hi, "index ") else { return false }
        var i = lo + 6
        let digitsStart = i
        while i < hi, isHex(bytes[i]) { i += 1 }
        return i > digitsStart && i < hi && (bytes[i] == 46 || bytes[i] == 44)
    }

    private func isHex(_ byte: UInt8) -> Bool { (byte >= 48 && byte <= 57) || (byte >= 97 && byte <= 102) || (byte >= 65 && byte <= 70) }

    /// `@@ -a,b +c,d @@ section`; a missing count is 1.
    private func hunkHeader(_ lo: Int, _ hi: Int) -> (oldStart: Int, oldCount: Int, newStart: Int, newCount: Int)? {
        var i = lo + 2
        func skipBlank() { while i < hi, bytes[i] == 32 { i += 1 } }
        func number() -> Int? {
            var value = 0
            var digits = 0
            while i < hi, bytes[i] >= 48, bytes[i] <= 57, digits < 10 { value = value * 10 + Int(bytes[i] - 48); digits += 1; i += 1 }
            return digits == 0 || digits == 10 ? nil : value
        }
        func range() -> (start: Int, count: Int)? {
            guard let start = number() else { return nil }
            guard i < hi, bytes[i] == 44 else { return (start, 1) }
            i += 1
            guard let count = number() else { return nil }
            return (start, count)
        }
        skipBlank()
        guard i < hi, bytes[i] == 45 else { return nil }
        i += 1
        guard let old = range() else { return nil }
        skipBlank()
        guard i < hi, bytes[i] == 43 else { return nil }
        i += 1
        guard let new = range() else { return nil }
        return (old.start, old.count, new.start, new.count)
    }
}
