import XCTest
import Foundation
@testable import RiWorkCore

// The line cache must change nothing but the work done. These tests compare three parsers on the same input:
//   * `LegacyParser`, a frozen copy of the parser as it was before the cache existed,
//   * `TerminalText.styledScreen(...)` without a cache,
//   * `TerminalText.styledScreen(..., cache:)`, with caches that live on from one input to the next, as in the app.


// MARK: - Input

private struct FuzzRandom: RandomNumberGenerator {
    var state: UInt64
    init(seed: UInt64) { state = seed }
    mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }
    mutating func below(_ n: Int) -> Int { Int(next() % UInt64(n)) }
    mutating func chance(_ percent: Int) -> Bool { below(100) < percent }
    mutating func pick<T>(_ items: [T]) -> T { items[below(items.count)] }
}

private let E = "\u{1B}"

/// Pieces of terminal output, from plain text to malformed escape sequences.
private enum Pieces {
    static let text = ["a", "hello", "foo bar", "   ", "x", "0123456789", "--", "... ", "Read src/main.rs", "\u{2502}", "\u{2500}\u{250C}\u{2510}\u{2514}\u{2518}", "\u{E9}", "\u{F1}", "$ ", "> ", "ok", "  "]
    static let wide = ["\u{6F22}\u{5B57}", "\u{304B}\u{306A}", "\u{D55C}\u{AE00}", "\u{FF71}\u{FF72}", "\u{1F680}", "\u{2705}", "\u{2B50}", "\u{65E5}\u{672C}\u{8A9E} text", "\u{FF21}\u{FF22}", "\u{4F60}\u{597D}"]
    static let emoji = ["\u{23FA}", "\u{26A0}", "\u{26A0}\u{FE0F}", "\u{2714}\u{FE0E}", "\u{25B6}", "\u{2139}", "\u{2702}", "\u{2611}", "1\u{FE0F}\u{20E3}", "#\u{FE0F}\u{20E3}", "*\u{20E3}",
                        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", "\u{1F44D}\u{1F3FD}", "\u{1F1FA}\u{1F1F8}", "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}", "\u{2764}\u{FE0F}", "\u{A9}", "\u{2122}", "\u{2194}", "\u{2733}", "\u{273B}"]
    static let combining = ["e\u{301}", "a\u{300}\u{301}", "\u{301}", "\u{200D}", "\u{FE0F}", "\u{FE0E}", "n\u{303}", "\u{1100}\u{1161}\u{11A8}",
                            "\u{0E01}\u{0E33}", "\u{200B}", "\u{2028}", "\u{2029}", "\u{FFFD}", "\u{0085}", "\u{FEFF}", "\u{3099}"]
    static let privateUse = ["\u{E0B0}", "\u{F8FF}", "\u{E000}", "\u{F0001}", "\u{100000}", "\u{10FFFD}", "\u{E0B2}"]
    static let sgr = [E + "[0m", E + "[m", E + "[1m", E + "[2m", E + "[3m", E + "[4m", E + "[7m", E + "[8m", E + "[9m", E + "[22m", E + "[24m", E + "[27m",
                      E + "[39m", E + "[49m", E + "[31m", E + "[1;32m", E + "[0;31;44m", E + "[44m", E + "[7;33m", E + "[90m", E + "[104m",
                      E + "[38;5;196m", E + "[48;5;22m", E + "[38;2;215;119;87m", E + "[48;2;0;0;0m", E + "[38:5:33m", E + "[38:2::255:0:0m",
                      E + "[38:2:1:2:3m", E + "[4:3m", E + "[4:0m", E + "[58;5;1m", E + "[0;1;38;5;42;48;5;17m"]
    static let malformedSGR = [E + "[;m", E + "[1;;3m", E + "[38;5m", E + "[38;2;1;2m", E + "[38;9;1m", E + "[999999999m", E + "[31;;;m", E + "[:m", E + "[::m",
                               E + "[1:2:3:4:5:6:7:8m", E + "[+m", E + "[1 m", E + "[?25h", E + "[?1049h", E + "[>c", E + "[2J", E + "[H", E + "[1;1H",
                               E + "[K", E + "[3 q", E + "[<1m", E + "[=1m", E + "[38;5;999m", E + "[48;2;256;0;0m", E + "[31",
                               E + "[" + String(repeating: "1;", count: 400) + "m", E + "[" + String(repeating: "9", count: 300) + "m"]
    static let otherEscapes = [E + "]0;title\u{07}", E + "]8;;http://x.y" + E + "\\", E + "]0;unterminated", E + "]52;c;abc" + E + "\\", E + "]0;t" + E,
                               E + "]0;t" + E + "[31m", E + "Pq data" + E + "\\", E + "P1$r0m" + E + "\\", E + "Xsos" + E + "\\", E + "^pm\u{07}",
                               E + "_apc" + E + "\\", E + "(B", E + ")0", E + "(", E + "=", E + ">", E + "M", E + "7", E + "8", E + "c", E + "#8", E + "%G",
                               E, E + E, E + E + "[31m", E + "\u{E9}", E + "[1\u{E9}", E + "\u{7}", E + "\t", E + "[1\t", E + "\r", E + " F", E + "\u{7F}", E + "]", E + "P"]
    static let controls = ["\u{07}", "\u{08}", "\u{00}", "\u{0B}", "\u{0C}", "\u{0E}", "\u{0F}", "\u{7F}", "\u{1A}", "\u{18}", "\u{9B}", "\u{90}", "\u{80}", "\u{9F}"]
    static let breaks = ["\n", "\n", "\n", "\n", "\n", "\n", "\n", "\n", "\r\n", "\r\n", "\r\r\n", "\n\r", "\r\n\r\n"]
}

private func randomPiece(_ g: inout FuzzRandom) -> String {
    switch g.below(100) {
    case 0..<26: return g.pick(Pieces.text)
    case 26..<48: return g.pick(Pieces.sgr)
    case 48..<52: return g.pick(Pieces.malformedSGR)
    case 52..<60: return g.pick(Pieces.wide)
    case 60..<68: return g.pick(Pieces.emoji)
    case 68..<72: return g.pick(Pieces.combining)
    case 72..<75: return g.pick(Pieces.privateUse)
    case 75..<80: return g.pick(Pieces.otherEscapes)
    case 80..<83: return g.pick(Pieces.controls)
    case 83..<88: return "\t"
    case 88..<92: return "\r"
    case 92..<96: return String(repeating: " ", count: 1 + g.below(12))
    default: return g.pick(Pieces.text) + g.pick(Pieces.sgr)
    }
}

private func randomLine(_ g: inout FuzzRandom) -> String {
    var line = ""
    for _ in 0..<g.below(10) { line += randomPiece(&g) }
    return line
}

/// A screen as a list of lines and what ends each of them; mutated a little from one input to the next, the way a live screen is.
private struct FuzzScreen {
    var lines: [String] = []
    var ends: [String] = []   // ends[i] follows lines[i]; the last one may be empty (no trailing break)

    mutating func randomize(_ g: inout FuzzRandom) {
        lines.removeAll(); ends.removeAll()
        for _ in 0..<(1 + g.below(30)) { lines.append(randomLine(&g)); ends.append(g.pick(Pieces.breaks)) }
        if g.chance(25) { ends[ends.count - 1] = g.pick(["", "\r", "\r\n", "\n\n"]) }
    }

    var text: String { zip(lines, ends).map { $0 + $1 }.joined() }

    mutating func mutate(_ g: inout FuzzRandom) {
        if lines.isEmpty { randomize(&g); return }
        switch g.below(14) {
        case 0: break   // unchanged
        case 1, 2:
            // typing: the last line with something typed at its end
            let i = lines.count - 1 - (g.chance(30) ? min(1, lines.count - 1) : 0)
            lines[i] += randomPiece(&g)
        case 3:
            // output: lines scroll in at the bottom and out at the top
            let k = 1 + g.below(3)
            for _ in 0..<k {
                lines.append(randomLine(&g)); ends.append(g.pick(Pieces.breaks))
                if lines.count > 4, g.chance(80) { lines.removeFirst(); ends.removeFirst() }
            }
        case 4:
            let k = min(lines.count - 1, 1 + g.below(5))
            lines.removeFirst(k); ends.removeFirst(k)
        case 5:
            let i = g.below(lines.count)
            lines[i] = randomLine(&g)
        case 6:
            // an edit inside a line
            let i = g.below(lines.count)
            var scalars = Array(lines[i].unicodeScalars)
            let at = scalars.isEmpty ? 0 : g.below(scalars.count + 1)
            scalars.insert(contentsOf: randomPiece(&g).unicodeScalars, at: at)
            lines[i] = String(String.UnicodeScalarView(scalars))
        case 7:
            // a style set early that nothing resets: the lines below start in another style
            let i = g.below(min(lines.count, 4))
            lines[i] = g.pick(Pieces.sgr) + lines[i]
        case 8:
            // the other way round: take every escape out of an early line, so the style no longer carries on
            let i = g.below(min(lines.count, 6))
            lines[i] = lines[i].replacingOccurrences(of: E, with: "")
        case 9:
            let i = g.below(lines.count)
            for _ in 0..<(1 + g.below(3)) { lines.insert(lines[i], at: i); ends.insert(ends[i], at: i) }
        case 10:
            // the last line cut off anywhere: partial escape sequences and half a CRLF at the end of the input
            let i = lines.count - 1
            let scalars = Array(lines[i].unicodeScalars)
            lines[i] = String(String.UnicodeScalarView(scalars.prefix(g.below(scalars.count + 1))))
            ends[i] = g.pick(["", "", "\r", "\n"])
        case 11:
            if lines.count > 1 { let a = g.below(lines.count), b = g.below(lines.count); lines.swapAt(a, b) }
        case 12:
            let i = g.below(lines.count)
            ends[i] = g.pick(Pieces.breaks)
        default:
            randomize(&g)
        }
    }
}

// MARK: - Comparison

/// Strings are equal in Swift when they are canonically equivalent; the cache must give the very same scalars, so compare the bytes.
private func sameBytes(_ a: String, _ b: String) -> Bool { a.utf8.elementsEqual(b.utf8) }

/// The first difference between two screens, or nil when they are identical in every field.
private func difference(_ a: StyledScreen, _ b: StyledScreen) -> String? {
    if !sameBytes(a.text, b.text) { return "text \(a.text.debugDescription) vs \(b.text.debugDescription)" }
    if a.runs != b.runs { return "runs \(a.runs) vs \(b.runs)" }
    if a.cursorOffset != b.cursorOffset { return "cursorOffset \(String(describing: a.cursorOffset)) vs \(String(describing: b.cursorOffset))" }
    if a.cursorLine != b.cursorLine { return "cursorLine \(String(describing: a.cursorLine)) vs \(String(describing: b.cursorLine))" }
    if a.cursorColumn != b.cursorColumn { return "cursorColumn \(String(describing: a.cursorColumn)) vs \(String(describing: b.cursorColumn))" }
    if a.columns != b.columns { return "columns \(a.columns) vs \(b.columns)" }
    if a.historyLines != b.historyLines { return "historyLines \(a.historyLines) vs \(b.historyLines)" }
    if a.lines.count != b.lines.count { return "line count \(a.lines.count) vs \(b.lines.count)" }
    for (i, (x, y)) in zip(a.lines, b.lines).enumerated() {
        if !sameBytes(x.text, y.text) { return "line \(i) text \(x.text.debugDescription) vs \(y.text.debugDescription)" }
        if x.runs != y.runs { return "line \(i) runs \(x.runs) vs \(y.runs)" }
        if x.columns != y.columns { return "line \(i) columns \(x.columns) vs \(y.columns)" }
    }
    return a == b ? nil : "== says different"
}

/// What `flatten` must give, computed the plain way from the lines: joined by line breaks, runs merged, the cursor from its line.
private func flatFormProblem(_ screen: StyledScreen) -> String? {
    var text = ""
    var runs: [StyleRun] = []
    func add(_ run: StyleRun) {
        if let last = runs.last, last.style == run.style { runs[runs.count - 1] = StyleRun(length: last.length + run.length, style: run.style) } else { runs.append(run) }
    }
    var offset = 0
    var starts: [Int] = []
    var columns = 0
    for (i, line) in screen.lines.enumerated() {
        if i > 0 { text += "\n"; add(StyleRun(length: 1, style: .plain)); offset += 1 }
        starts.append(offset)
        text += line.text
        for run in line.runs { add(run) }
        if line.runs.reduce(0, { $0 + $1.length }) != line.text.count { return "line \(i): its runs do not cover its text" }
        offset += line.text.count
        columns = max(columns, line.columns)
    }
    if screen.text != text { return "flat text" }
    if screen.runs != runs { return "flat runs" }
    if screen.columns != columns { return "columns" }
    if let line = screen.cursorLine, let column = screen.cursorColumn {
        if screen.cursorOffset != starts[line] + column { return "cursor offset" }
        if column >= screen.lines[line].text.count { return "cursor column outside its line" }
    } else if screen.cursorOffset != nil || screen.cursorLine != nil || screen.cursorColumn != nil { return "cursor fields disagree" }
    if screen.runs.reduce(0, { $0 + $1.length }) != screen.text.count { return "runs do not cover the text" }
    return nil
}

final class StyledLineCacheTests: XCTestCase {
    private typealias Cursor = (x: Int, y: Int)

    /// Parses `input` every way there is and reports the first disagreement.
    private func problem(_ input: String, cursor: Cursor?, rows: Int?, textPresentation: Bool, caches: [StyledLineCache]) -> String? {
        let legacy = LegacyParser.styledScreen(input, cursor: cursor, rows: rows, textPresentation: textPresentation)
        let plain = TerminalText.styledScreen(input, cursor: cursor, rows: rows, textPresentation: textPresentation)
        if let d = difference(legacy, plain) { return "uncached differs from the legacy parser: \(d)" }
        if let d = flatFormProblem(plain) { return "flat form of the uncached screen: \(d)" }
        // history pages go through the same line building
        let breaks = input.utf8.filter { $0 == 0x0A }.count
        for expecting in [nil, breaks + 1, breaks] as [Int?] {
            let page = TerminalText.styledLines(page: input, expecting: expecting, textPresentation: textPresentation)
            let old = LegacyParser.styledLines(page: input, expecting: expecting, textPresentation: textPresentation)
            if page.count != old.count || zip(page, old).contains(where: { !sameBytes($0.text, $1.text) || $0.runs != $1.runs || $0.columns != $1.columns }) {
                return "styledLines(page:, expecting: \(String(describing: expecting))) differs from the legacy parser"
            }
        }
        for (n, cache) in caches.enumerated() {
            // twice: the first parse may find some lines, the second finds all of them
            for pass in 1...2 {
                let cached = TerminalText.styledScreen(input, cursor: cursor, rows: rows, textPresentation: textPresentation, cache: cache)
                if let d = difference(plain, cached) { return "cache \(n), pass \(pass) differs: \(d)" }
                if let d = flatFormProblem(cached) { return "flat form of the cached screen (cache \(n), pass \(pass)): \(d)" }
            }
        }
        return nil
    }

    // MARK: Hand-picked inputs

    private let edgeCases: [String] = [
        "", "\n", "\n\n\n", "\r", "\r\n", "\r\n\r\n", "a", "a\n", "a\r", "a\r\n", "a\rb", "a\rb\n", "a\rb\r\nc", "a\r\r\nb", "a\n\rb",
        "abc\r", "abc\r\n\r", "\r\rabc", "x\r\ny\r",
        E + "[31mred\n" + "still red\n" + E + "[0mplain",
        E + "[31m\n\n\n" + "after",                                  // the style runs through empty lines
        E + "[31mred\r\nstill\r\n" + E + "[0m\r\nplain\r\n",
        E + "[44m   \n" + E + "[0m\nx",                              // blank rows that paint
        "a\n" + E + "[7m \n" + E + "[0m\n\n",                        // inverse blank at the end is kept
        "a\n" + E + "[1;31m \n\n   \n",                              // bold red blank: dropped
        "ab" + E, "ab" + E + "\n" + "cd", "ab" + E + "\r\n" + "cd", "ab" + E + "[", "ab" + E + "[\ncd", "ab" + E + "[3\ncd", "ab" + E + "[31",
        "ab" + E + "[31\r\ncd" + E + "[0m", "ab" + E + "]0;title", "ab" + E + "]0;title\ncd", "ab" + E + "]0;title\r\ncd", "ab" + E + "]0;t" + E + "\ncd",
        "ab" + E + "]0;t" + E + "\r\ncd", "ab" + E + "]0;t" + E + "\\cd", "ab" + E + "P1;2\r\n" + "cd", "ab" + E + "(\ncd", "ab" + E + "(B\ncd",
        "ab" + E + " \ncd", "ab" + E + "\u{7F}\ncd",
        "ab" + E + "]0;title\r", "ab" + E + "]0;title\r\r\n" + "x", "ab" + E + "\r\n" + E + "[31m" + "x",
        "\t", "\ta\tb\t\n\t\n", "a\tb\r\n\tc", "tab\u{9}\rover",
        "a\u{07}b\u{08}c\u{00}d\u{7F}e\n" + "\u{85}x\n" + "\u{9B}31mnot an escape",
        "\u{E0B0} prompt\n" + "x\u{E0B0}y\n", "\u{F0001}\n",
        "\u{23FA} done\n" + "\u{2714}\u{FE0E} ok\n" + "\u{26A0}\n" + "1\u{FE0F}\u{20E3} one\n" + "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} family\n" + "\u{1F44D}\u{1F3FD}\n" + "\u{1F1FA}\u{1F1F8}\n",
        "e\u{301}\n" + "\u{301}lone mark\n" + "\n" + "\u{200D}x\n",
        "\u{65E5}\u{672C}\u{8A9E}\n" + "\u{D55C}\u{AE00}\n" + "\u{FF21}\u{FF22}\n",
        "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n"
    ]

    func testHandPickedInputsGiveTheSameScreenWithAndWithoutACache() {
        let caches = [StyledLineCache(), StyledLineCache(maximumLines: 3)]
        let cursors: [Cursor?] = [nil, (0, 0), (5, 0), (0, 1), (3, 2), (10, 5), (79, 39), (2, 0), (200, 1)]
        for input in edgeCases {
            for cursor in cursors {
                for rows in [nil, 1, 2, 3, 6, 40] as [Int?] {
                    for presentation in [true, false] {
                        if let p = problem(input, cursor: cursor, rows: rows, textPresentation: presentation, caches: caches) {
                            XCTFail("\(p)\n  input \(input.debugDescription), cursor \(String(describing: cursor)), rows \(String(describing: rows)), presentation \(presentation)")
                            return
                        }
                    }
                }
            }
        }
    }

    // MARK: Differential fuzz

    /// Thousands of inputs, each a small change of the one before, through caches that live on (one roomy, two tiny ones that evict all the
    /// time), compared with the legacy parser and the uncached one in every field.
    func testDifferentialFuzz() {
        let iterations = 6000
        var g = FuzzRandom(seed: 0x5EED_CAFE)
        var screen = FuzzScreen()
        screen.randomize(&g)
        let caches = [StyledLineCache(), StyledLineCache(maximumLines: 9, maximumBytes: 6000), StyledLineCache(maximumLines: 40)]
        var presentation = true
        var rows: Int? = 6
        var cursor: Cursor? = (3, 2)
        for iteration in 0..<iterations {
            for _ in 0..<g.below(4) { screen.mutate(&g) }
            if g.chance(10) { presentation.toggle() }
            if g.chance(15) { rows = g.pick([nil, 1, 2, 3, 5, 8, 12, 40]) }
            switch g.below(10) {
            case 0: cursor = nil
            case 1: cursor = (x: -1, y: 0)
            case 2: cursor = (x: g.below(10), y: (rows ?? 3) + g.below(2))   // maybe below the screen
            default: if g.chance(30) { cursor = (x: g.below(70), y: g.below(max(1, rows ?? 6))) }
            }
            let input = screen.text
            if let p = problem(input, cursor: cursor, rows: rows, textPresentation: presentation, caches: caches) {
                XCTFail("iteration \(iteration): \(p)\n  input \(input.debugDescription), cursor \(String(describing: cursor)), rows \(String(describing: rows)), presentation \(presentation)")
                return
            }
        }
        // The roomy cache has seen both hits and misses; the tiny ones had to drop lines.
        let seen = caches[0].counters
        XCTAssertGreaterThan(seen.hits, 1000)
        XCTAssertGreaterThan(seen.misses, 1000)
        XCTAssertLessThanOrEqual(caches[1].count, 9)
        XCTAssertLessThanOrEqual(caches[2].count, 40)
        print("DIFFERENTIAL FUZZ: \(iterations) inputs x 2 passes x \(caches.count) caches; roomy cache \(seen.hits) hits, \(seen.misses) misses, \(seen.bypassed) bypassed")
    }

    /// The same, with inputs that are not shaped like a screen at all: random pieces glued together.
    func testDifferentialFuzzOfRandomGlue() {
        var g = FuzzRandom(seed: 7)
        let cache = StyledLineCache()
        for iteration in 0..<3000 {
            var input = ""
            for _ in 0..<g.below(40) { input += g.chance(15) ? g.pick(Pieces.breaks) : randomPiece(&g) }
            let rows: Int? = g.chance(25) ? nil : 1 + g.below(6)
            let cursor: Cursor? = g.chance(25) ? nil : (x: g.below(20), y: g.below(7))
            if let p = problem(input, cursor: cursor, rows: rows, textPresentation: g.chance(50), caches: [cache]) {
                XCTFail("iteration \(iteration): \(p)\n  input \(input.debugDescription), cursor \(String(describing: cursor)), rows \(String(describing: rows))")
                return
            }
        }
    }

    // MARK: A hit is the same line

    private func hasSameStorage(_ a: StyledLine, _ b: StyledLine) -> Bool {
        let t1 = a.text.utf8.withContiguousStorageIfAvailable { $0.baseAddress }
        let t2 = b.text.utf8.withContiguousStorageIfAvailable { $0.baseAddress }
        let r1 = a.runs.withUnsafeBufferPointer { $0.baseAddress }
        let r2 = b.runs.withUnsafeBufferPointer { $0.baseAddress }
        return t1 != nil && t1 == t2 && r1 == r2
    }

    private func transcript(_ first: Int, _ count: Int = 160, edit: Int = 0) -> String {
        var lines = SyntheticTranscript.lines(first, count)
        if edit > 0 { lines[lines.count - 1] += String(repeating: "x", count: edit) }
        return lines.joined(separator: "\n") + "\n"
    }

    func testAnUnchangedScreenIsNotScannedAgainAndSharesItsLines() {
        let cache = StyledLineCache()
        let text = transcript(5000)
        let first = TerminalText.styledScreen(text, cursor: (3, 39), rows: 40, cache: cache)
        let afterFirst = cache.counters
        XCTAssertEqual(afterFirst.hits + afterFirst.misses + afterFirst.bypassed, 160, "every line is looked up (or is the cursor line)")
        XCTAssertEqual(afterFirst.bypassed, 1, "the cursor line is parsed afresh")
        XCTAssertGreaterThan(afterFirst.misses, 100)
        XCTAssertGreaterThan(afterFirst.hits, 0, "the transcript repeats some lines, and a repeat is a hit")

        cache.resetCounters()
        let second = TerminalText.styledScreen(text, cursor: (3, 39), rows: 40, cache: cache)
        XCTAssertEqual(cache.counters, StyledLineCache.Counters(hits: 159, misses: 0, bypassed: 1), "nothing is scanned except the cursor line")
        XCTAssertEqual(first, second)
        XCTAssertEqual(second, TerminalText.styledScreen(text, cursor: (3, 39), rows: 40))

        var shared = 0, long = 0
        for k in 0..<159 where first.lines[k].text.utf8.count > 15 {
            long += 1
            if hasSameStorage(first.lines[k], second.lines[k]) { shared += 1 }
        }
        XCTAssertGreaterThan(long, 100)
        XCTAssertEqual(shared, long, "a hit is the very StyledLine that was built before, not an equal copy")
        // Lines that repeat within one screen share storage too.
        var seen: [String: StyledLine] = [:]
        for line in second.lines where line.text.utf8.count > 15 {
            if let earlier = seen[line.text], earlier.runs == line.runs, hasSameStorage(earlier, line) { shared += 1 }
            seen[line.text] = line
        }
    }

    func testEditingTheLastLineScansOnlyThatLine() {
        let cache = StyledLineCache()
        _ = TerminalText.styledScreen(transcript(5000), cursor: nil, rows: nil, cache: cache)
        cache.resetCounters()
        let edited = transcript(5000, edit: 3)
        let screen = TerminalText.styledScreen(edited, cursor: nil, rows: nil, cache: cache)
        XCTAssertEqual(cache.counters.misses, 1, "only the line that changed is parsed")
        XCTAssertEqual(cache.counters.hits, 160, "159 lines and the empty row after the last line feed")
        XCTAssertEqual(screen, TerminalText.styledScreen(edited, cursor: nil, rows: nil))
    }

    func testScrollingTwoLinesInScansTwoLines() {
        let cache = StyledLineCache()
        var previous: StyledScreen?
        var totalMisses = 0
        for step in 0..<20 {
            let text = transcript(5000 + 2 * step)
            cache.resetCounters()
            let screen = TerminalText.styledScreen(text, cursor: (0, 39), rows: 40, cache: cache)
            XCTAssertEqual(screen, TerminalText.styledScreen(text, cursor: (0, 39), rows: 40))
            if step > 0 {
                // two new lines, and the cursor line (which is the last of them, so it is one of the two new ones or an old one)
                XCTAssertLessThanOrEqual(cache.counters.misses, 2, "step \(step)")
                totalMisses += cache.counters.misses
                if let previous {
                    // the lines that stayed are the same values
                    var same = 0
                    for k in 2..<150 where previous.lines[k].text.utf8.count > 15 && hasSameStorage(previous.lines[k], screen.lines[k - 2]) { same += 1 }
                    XCTAssertGreaterThan(same, 100)
                }
            }
            previous = screen
        }
        XCTAssertLessThanOrEqual(totalMisses, 2 * 19)
    }

    func testALineIsParsedAgainWhenTheStyleInEffectWhenItStartsDiffers() {
        let cache = StyledLineCache()
        let tail = "two\nthree\n"
        let a = TerminalText.styledScreen("one\n" + tail, cursor: nil, rows: nil, cache: cache)
        XCTAssertTrue(a.isPlain)
        cache.resetCounters()
        // the same lines, but a color is left running from the first line
        let b = TerminalText.styledScreen("\(E)[31mone\n" + tail, cursor: nil, rows: nil, cache: cache)
        XCTAssertEqual(cache.counters.hits, 0, "same text, different style in effect: not a hit")
        XCTAssertEqual(cache.counters.misses, 4, "three lines and the empty row after the last line feed, which starts in red too")
        XCTAssertFalse(b.isPlain)
        XCTAssertEqual(b.runs, TerminalText.styledScreen("\(E)[31mone\n" + tail, cursor: nil, rows: nil).runs)
        // back to the first input: all hits, from the entries made under the plain style
        cache.resetCounters()
        let c = TerminalText.styledScreen("one\n" + tail, cursor: nil, rows: nil, cache: cache)
        XCTAssertEqual(cache.counters.misses, 0)
        XCTAssertEqual(a, c)
        // a reset in the middle brings the style back to plain: the line after it is a hit even though the first lines differ
        cache.resetCounters()
        _ = TerminalText.styledScreen("\(E)[1;34mdifferent\n\(E)[0mtwo\nthree\n", cursor: nil, rows: nil, cache: cache)
        XCTAssertEqual(cache.counters.hits, 2, "'three' and the empty row after it start in the plain style, as before")
    }

    func testTheOptionsAreAPartOfTheKey() {
        let cache = StyledLineCache()
        let input = "\u{23FA} done \u{E0B0} x\nplain\n"
        let withPresentation = TerminalText.styledScreen(input, cursor: nil, rows: nil, textPresentation: true, cache: cache)
        let without = TerminalText.styledScreen(input, cursor: nil, rows: nil, textPresentation: false, cache: cache)
        XCTAssertNotEqual(withPresentation.text, without.text)
        XCTAssertEqual(without, TerminalText.styledScreen(input, cursor: nil, rows: nil, textPresentation: false))
        XCTAssertEqual(withPresentation, TerminalText.styledScreen(input, cursor: nil, rows: nil, textPresentation: true))
        // keepCells: the private-use glyph is a blank cell with a cursor (the cursor parse) and gone without one
        let keep = TerminalText.styledScreen(input, cursor: (0, 1), rows: 2, textPresentation: true, cache: cache)
        XCTAssertEqual(keep, TerminalText.styledScreen(input, cursor: (0, 1), rows: 2, textPresentation: true))
        XCTAssertNotEqual(keep.lines[0].text, withPresentation.lines[0].text)
        XCTAssertEqual(withPresentation, TerminalText.styledScreen(input, cursor: nil, rows: nil, textPresentation: true, cache: cache))
    }

    func testTheCursorLineIsAlwaysParsedAfresh() {
        let cache = StyledLineCache()
        let text = transcript(7000)
        for y in [0, 10, 39] {
            let a = TerminalText.styledScreen(text, cursor: (2, y), rows: 40, cache: cache)
            let b = TerminalText.styledScreen(text, cursor: (2, y), rows: 40)
            XCTAssertEqual(a, b)
            XCTAssertEqual(a.cursorLine, 120 + y)
        }
        // a cursor below the text, and one in a row that the cache holds as an ordinary line
        XCTAssertEqual(TerminalText.styledScreen("a\nb\n", cursor: (4, 9), rows: 10, cache: cache), TerminalText.styledScreen("a\nb\n", cursor: (4, 9), rows: 10))
    }

    // MARK: Bounds

    func testTheCacheStaysBounded() {
        let cache = StyledLineCache()
        var peak = 0
        for step in 0..<400 {
            // every answer has 100 lines that no other answer has
            let text = (0..<100).map { "answer \(step) line \($0) \(E)[3\($0 % 8)mcolored\(E)[0m" }.joined(separator: "\n") + "\n"
            _ = TerminalText.styledScreen(text, cursor: (0, 39), rows: 40, cache: cache)
            peak = max(peak, cache.count)
            XCTAssertLessThanOrEqual(cache.count, 2 * 100 + 64 + 100, "step \(step)")
        }
        XCTAssertGreaterThan(peak, 100)
        XCTAssertGreaterThan(cache.counters.misses, 39_000)
        // One answer after another with the same lines: nothing grows.
        let same = transcript(100, 150)
        for _ in 0..<50 { _ = TerminalText.styledScreen(same, cursor: nil, rows: nil, cache: cache) }
        XCTAssertLessThanOrEqual(cache.count, 2 * 150 + 64 + 100)
    }

    func testTheHardLimitsHold() {
        let small = StyledLineCache(maximumLines: 50, maximumBytes: 1 << 20)
        for step in 0..<30 {
            let text = (0..<200).map { "step \(step) line \($0)" }.joined(separator: "\n")
            let screen = TerminalText.styledScreen(text, cursor: nil, rows: nil, cache: small)
            XCTAssertEqual(screen.lines.count, 200)
            XCTAssertLessThanOrEqual(small.count, 50)
        }
        let tight = StyledLineCache(maximumLines: 10_000, maximumBytes: 2000)
        for step in 0..<30 {
            let text = (0..<200).map { "step \(step) line \($0)" }.joined(separator: "\n")
            _ = TerminalText.styledScreen(text, cursor: nil, rows: nil, cache: tight)
            XCTAssertLessThanOrEqual(tight.bytes, 2000)
        }
        // a line too long to keep is parsed every time, and still right
        let cache = StyledLineCache()
        let huge = String(repeating: "\(E)[31mx\(E)[0m", count: 3000)
        XCTAssertGreaterThan(huge.utf8.count, StyledLineCache.longestLine)
        let text = "a\n\(huge)\nb\n"
        let one = TerminalText.styledScreen(text, cursor: nil, rows: nil, cache: cache)
        XCTAssertEqual(cache.counters.bypassed, 1)
        XCTAssertEqual(one, TerminalText.styledScreen(text, cursor: nil, rows: nil))
        XCTAssertEqual(cache.count, 3, "'a', 'b' and the empty row; the long line is not kept")
        cache.removeAll()
        XCTAssertEqual(cache.count, 0)
        XCTAssertEqual(cache.bytes, 0)
        XCTAssertEqual(TerminalText.styledScreen(text, cursor: nil, rows: nil, cache: cache), one)
    }

    func testShellOutputParsesThroughACache() throws {
        let cache = StyledLineCache()
        let result: JSONValue = .object(["shell_id": .string("s"), "output": .string(transcript(300, 60)), "rows": .number(40), "cols": .number(50),
                                         "cursor": .object(["x": .number(1), "y": .number(39)])])
        let output = try ShellOutput(result: result)
        XCTAssertEqual(output.styledScreen(cache: cache), output.styledScreen)
        cache.resetCounters()
        XCTAssertEqual(output.styledScreen(cache: cache), output.styledScreen)
        XCTAssertEqual(cache.counters.misses, 0)
    }

    // MARK: Threads

    func testSeveralThreadsShareACacheSafely() {
        let cache = StyledLineCache()
        let group = DispatchGroup()
        let failures = LockedCounter()
        for thread in 0..<4 {
            group.enter()
            DispatchQueue.global().async {
                var g = FuzzRandom(seed: UInt64(100 + thread))
                var screen = FuzzScreen()
                screen.randomize(&g)
                for _ in 0..<300 {
                    screen.mutate(&g)
                    let input = screen.text
                    let cached = TerminalText.styledScreen(input, cursor: (2, 3), rows: 6, cache: cache)
                    if cached != TerminalText.styledScreen(input, cursor: (2, 3), rows: 6) { failures.increment() }
                }
                group.leave()
            }
        }
        group.wait()
        XCTAssertEqual(failures.value, 0)
    }
}

private final class LockedCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    func increment() { lock.withLock { count += 1 } }
    var value: Int { lock.withLock { count } }
}

// MARK: - The parser as it was

/// A frozen copy of the parser as it was before the line cache (the code of `scanStyled`, `build`, `styledScreen` and `styledLines`
/// at the commit the cache was added on). It is the reference every other path is compared with: the uncached parse in the package and
/// the cached one must give exactly what this gives. Do not "fix" it; if the parser is changed on purpose, change this on purpose too.
enum LegacyParser {
    static func scan(_ input: String, keepCells: Bool, textPresentation forceText: Bool) -> TerminalText.ScannedScreen {
        let scalars = Array(input.unicodeScalars)
        var styles: [CellStyle] = [.plain]
        var known: [CellStyle: UInt32] = [.plain: 0]
        var current = CellStyle.plain
        var currentID: UInt32 = 0
        var rows: [[TerminalText.StyledCell]] = []
        var line = String.UnicodeScalarView()
        var lineScalars = 0
        var marks: [(at: Int, style: UInt32)] = [(0, 0)]

        func restartMarks() { marks.removeAll(keepingCapacity: true); marks.append((0, currentID)) }
        func finishLine() {
            var cells: [TerminalText.StyledCell] = []
            cells.reserveCapacity(lineScalars)
            var mark = 0, position = 0
            for character in String(line) {
                while mark + 1 < marks.count, marks[mark + 1].at <= position { mark += 1 }
                cells.append(TerminalText.StyledCell(character: forceText ? TerminalText.textPresentation(character) : character, style: marks[mark].style))
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
                let (next, sgr) = TerminalText.escape(in: scalars, at: i)
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
        return TerminalText.ScannedScreen(rows: rows, styles: styles)
    }

    static func build(_ screen: TerminalText.ScannedScreen, rows: [[TerminalText.StyledCell]], cursor: (row: Int, index: Int)?, historyLines: Int = 0) -> StyledScreen {
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
                width += TerminalText.cellWidth(cell.character)
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

    static func styledScreen(_ input: String, cursor: (x: Int, y: Int)?, rows screenRows: Int?, textPresentation forceText: Bool = true) -> StyledScreen {
        guard let cursor, let screenRows, screenRows > 0, cursor.x >= 0, cursor.y >= 0, cursor.y < screenRows else {
            var scanned = scan(input, keepCells: false, textPresentation: forceText)
            while let last = scanned.rows.last, scanned.isBlank(last) { scanned.rows.removeLast() }
            return build(scanned, rows: scanned.rows, cursor: nil)
        }
        var scanned = scan(input, keepCells: true, textPresentation: forceText)
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
        while index < cells.count, columns + TerminalText.cellWidth(cells[index].character) <= cursor.x { columns += TerminalText.cellWidth(cells[index].character); index += 1 }
        // Past the end of the line: pad up to the column. On the second cell of a wide character, stay on that character.
        if index >= cells.count {
            let blank = TerminalText.StyledCell(character: " ", style: 0)
            if columns < cursor.x { cells.append(contentsOf: Array(repeating: blank, count: cursor.x - columns)) }
            index = cells.count
            cells.append(blank)
        }
        rows[row] = cells
        scanned.rows = rows
        return build(scanned, rows: rows, cursor: (row, index), historyLines: historyLines)
    }

    static func styledLines(page input: String, expecting count: Int? = nil, textPresentation forceText: Bool = true) -> [StyledLine] {
        if count == 0 || (count == nil && input.isEmpty) { return [] }
        let scanned = scan(input, keepCells: true, textPresentation: forceText)
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
