import XCTest
@testable import RiWorkCore

private let esc = "\u{1B}"
private func sgr(_ body: String) -> String { "\(esc)[\(body)m" }

private func apply(_ body: String, to start: CellStyle = .plain) -> CellStyle {
    var style = start
    // A body that is not plain SGR parameters is dropped by the screen, so it changes nothing here either.
    guard let parameters = SGR.parameters(Array(body.utf8)[...]) else { return start }
    SGR.apply(parameters, to: &style)
    return style
}

final class SGRTests: XCTestCase {
    // MARK: colors

    func testSixteenColorsForegroundAndBackground() {
        for code in 30...37 { XCTAssertEqual(apply("\(code)").foreground, .indexed(UInt8(code - 30))) }
        for code in 90...97 { XCTAssertEqual(apply("\(code)").foreground, .indexed(UInt8(code - 90 + 8))) }
        for code in 40...47 { XCTAssertEqual(apply("\(code)").background, .indexed(UInt8(code - 40))) }
        for code in 100...107 { XCTAssertEqual(apply("\(code)").background, .indexed(UInt8(code - 100 + 8))) }
        XCTAssertEqual(apply("31;44"), CellStyle(foreground: .indexed(1), background: .indexed(4)))
    }
    func testDefaultColorsResetOnlyTheirOwnSide() {
        let colored = CellStyle(foreground: .indexed(2), background: .indexed(5), attributes: .bold)
        XCTAssertEqual(apply("39", to: colored), CellStyle(foreground: .default, background: .indexed(5), attributes: .bold))
        XCTAssertEqual(apply("49", to: colored), CellStyle(foreground: .indexed(2), background: .default, attributes: .bold))
    }
    func testXterm256Colors() {
        XCTAssertEqual(apply("38;5;196").foreground, .indexed(196))
        XCTAssertEqual(apply("48;5;17").background, .indexed(17))
        XCTAssertEqual(apply("38;5;0").foreground, .indexed(0))
        XCTAssertEqual(apply("38;5;255").foreground, .indexed(255))
        XCTAssertEqual(apply("38:5:33").foreground, .indexed(33), "colon form")
        XCTAssertEqual(apply("1;38;5;208;4"), CellStyle(foreground: .indexed(208), attributes: [.bold, .underline]), "parameters after the color still apply")
    }
    func testTrueColor() {
        XCTAssertEqual(apply("38;2;255;128;0").foreground, .rgb(RGB(red: 255, green: 128, blue: 0)))
        XCTAssertEqual(apply("48;2;1;2;3").background, .rgb(RGB(red: 1, green: 2, blue: 3)))
        XCTAssertEqual(apply("38:2:10:20:30").foreground, .rgb(RGB(red: 10, green: 20, blue: 30)), "colon form without colorspace")
        XCTAssertEqual(apply("38:2::10:20:30").foreground, .rgb(RGB(red: 10, green: 20, blue: 30)), "colon form with an empty colorspace")
        XCTAssertEqual(apply("38:2:0:10:20:30").foreground, .rgb(RGB(red: 10, green: 20, blue: 30)), "colon form with a colorspace id")
        XCTAssertEqual(apply("38;2;0;0;0").foreground, .rgb(RGB(red: 0, green: 0, blue: 0)), "black is a color, not the default")
        XCTAssertEqual(apply("38;2;1;2;3;48;2;4;5;6"), CellStyle(foreground: .rgb(RGB(red: 1, green: 2, blue: 3)), background: .rgb(RGB(red: 4, green: 5, blue: 6))))
    }

    // MARK: attributes and resets

    func testAttributes() {
        XCTAssertEqual(apply("1").attributes, .bold)
        XCTAssertEqual(apply("2").attributes, .dim)
        XCTAssertEqual(apply("3").attributes, .italic)
        XCTAssertEqual(apply("4").attributes, .underline)
        XCTAssertEqual(apply("7").attributes, .inverse)
        XCTAssertEqual(apply("8").attributes, .hidden)
        XCTAssertEqual(apply("9").attributes, .strikethrough)
        XCTAssertEqual(apply("21").attributes, .underline, "double underline is an underline")
        XCTAssertEqual(apply("4:3").attributes, .underline, "curly underline is an underline")
        XCTAssertEqual(apply("1;3;4;7").attributes, [.bold, .italic, .underline, .inverse])
    }
    func testIndividualResets() {
        let all = CellStyle(attributes: [.bold, .dim, .italic, .underline, .inverse, .hidden, .strikethrough])
        XCTAssertEqual(apply("22", to: all).attributes, [.italic, .underline, .inverse, .hidden, .strikethrough], "22 clears bold and dim")
        XCTAssertEqual(apply("23", to: all).attributes.contains(.italic), false)
        XCTAssertEqual(apply("24", to: all).attributes.contains(.underline), false)
        XCTAssertEqual(apply("4:0", to: all).attributes.contains(.underline), false)
        XCTAssertEqual(apply("27", to: all).attributes.contains(.inverse), false)
        XCTAssertEqual(apply("28", to: all).attributes.contains(.hidden), false)
        XCTAssertEqual(apply("29", to: all).attributes.contains(.strikethrough), false)
    }
    func testFullResets() {
        let busy = CellStyle(foreground: .indexed(1), background: .rgb(RGB(0x123456)), attributes: [.bold, .underline])
        XCTAssertEqual(apply("0", to: busy), .plain)
        XCTAssertEqual(apply("", to: busy), .plain, "ESC[m is a reset")
        XCTAssertEqual(apply(";", to: busy), .plain)
        XCTAssertEqual(apply("0;31", to: busy), CellStyle(foreground: .indexed(1)), "a reset followed by a color")
        XCTAssertEqual(apply("31;0", to: busy), .plain, "a color followed by a reset")
    }

    // MARK: malformed input

    func testMalformedColorSpecsChangeNothingAndNeverTrap() {
        let start = CellStyle(foreground: .indexed(2), attributes: .italic)
        for body in ["38;5", "38;5;", "38;5;256", "38;5;300", "38;5;-1", "38;2", "38;2;1;2", "38;2;1;2;", "38;2;256;0;0", "38;2;1;2;999", "38", "48", "38;9;1", "38:5", "38:2:1:2", "38:2:1:2:3:4:5:6", "38:5:999", "58;5;9", "58:2::1:2:3"] {
            let result = apply(body, to: start)
            XCTAssertEqual(result.foreground, start.foreground, body)
            XCTAssertEqual(result.background, .default, body)
        }
        // Parameters that follow a cut-short spec are swallowed with it; those after an unknown mode are not.
        XCTAssertEqual(apply("38;2;1;2;3;1").attributes, .bold, "a spec that is complete leaves the next parameter alone")
        XCTAssertEqual(apply("38;9;1").attributes, .bold, "an unknown mode is skipped alone")
    }
    func testUnknownAndOversizedParametersAreIgnored() {
        XCTAssertEqual(apply("5;6;25;53;55;59;10;11;12;19;20"), .plain, "blink, overline, fonts change nothing")
        XCTAssertEqual(apply("999999999;1").attributes, .bold, "a number too large to trust is skipped, the rest still applies")
        XCTAssertEqual(apply("1234567").attributes, [], "seven digits")
        XCTAssertEqual(apply("31" + String(repeating: ";1", count: 500)).foreground, .indexed(1), "an absurd number of parameters is cut off")
        XCTAssertNil(SGR.parameters(Array("31;x".utf8)[...]))
        XCTAssertNil(SGR.parameters(Array("?25".utf8)[...]))
        XCTAssertNil(SGR.parameters(Array("38;5;1 ".utf8)[...]))
    }

    // MARK: through the screen

    func testSequencesReachTheScreenAsRunsAndLeaveTheTextPlain() {
        let screen = TerminalText.styledScreen("a\(sgr("31"))b\(sgr("1;32"))cd\(sgr("0"))e", cursor: nil, rows: nil)
        XCTAssertEqual(screen.text, "abcde")
        XCTAssertEqual(screen.runs, [
            StyleRun(length: 1, style: .plain),
            StyleRun(length: 1, style: CellStyle(foreground: .indexed(1))),
            StyleRun(length: 2, style: CellStyle(foreground: .indexed(2), attributes: .bold)),
            StyleRun(length: 1, style: .plain)
        ])
        XCTAssertFalse(screen.isPlain)
    }
    func testStyleCarriesAcrossLinesUntilReset() {
        let screen = TerminalText.styledScreen("\(sgr("34"))one\ntwo\(sgr("0"))\nthree", cursor: nil, rows: nil)
        XCTAssertEqual(screen.text, "one\ntwo\nthree")
        let blue = CellStyle(foreground: .indexed(4))
        XCTAssertEqual(screen.runs, [
            StyleRun(length: 3, style: blue), StyleRun(length: 1, style: .plain),
            StyleRun(length: 3, style: blue), StyleRun(length: 6, style: .plain)
        ], "the line breaks themselves stay plain, so a background never bleeds to the edge of the screen")
        XCTAssertEqual(screen.runs.reduce(0) { $0 + $1.length }, screen.text.count)
    }
    func testAnEscapeThatIsNotSGRIsDroppedWhole() {
        let hostile = "\(esc)[2J\(esc)[H\(esc)[?25l\(esc)[>4;2m\(esc)[38;5;1;2q\(esc)]0;title\u{07}\(esc)]8;;http://x\(esc)\\\(esc)(B\(esc)=\(esc)Mok"
        let screen = TerminalText.styledScreen(hostile, cursor: nil, rows: nil)
        XCTAssertEqual(screen.text, "ok")
        XCTAssertTrue(screen.isPlain)
    }
    func testBrokenSequencesNeverEatText() {
        // An unterminated CSI at the end is dropped; a CSI cut off by a control character or text stays cut off.
        XCTAssertEqual(TerminalText.styledScreen("ab\(esc)[31", cursor: nil, rows: nil).text, "ab")
        XCTAssertEqual(TerminalText.styledScreen("ab\(esc)", cursor: nil, rows: nil).text, "ab")
        XCTAssertEqual(TerminalText.styledScreen("ab\(esc)[1\ncd", cursor: nil, rows: nil).text, "ab\ncd", "a newline ends the broken sequence and is kept")
        XCTAssertEqual(TerminalText.styledScreen("ab\(esc)[1é", cursor: nil, rows: nil).text, "abé", "non-ASCII ends it too")
        XCTAssertEqual(TerminalText.styledScreen("ab\(esc)[1\(esc)[31mc", cursor: nil, rows: nil).text, "abc", "an escape inside starts the next sequence")
        XCTAssertEqual(TerminalText.styledScreen("ab\(esc)]0;never ends\ncd", cursor: nil, rows: nil).text, "ab\ncd", "an unterminated title ends with its line")
        XCTAssertEqual(TerminalText.styledScreen("ab\(esc)[\(String(repeating: "1;", count: 400))mc", cursor: nil, rows: nil).text, "abc")
    }
    func testControlCharactersAndOverprintsAreResolved() {
        XCTAssertEqual(TerminalText.styledScreen("a\u{07}b\u{08}c\u{00}d\u{7F}e", cursor: nil, rows: nil).text, "abcde")
        XCTAssertEqual(TerminalText.styledScreen("x\ty", cursor: nil, rows: nil).text, "x\ty")
        XCTAssertEqual(TerminalText.styledScreen("one\r\ntwo", cursor: nil, rows: nil).text, "one\ntwo")
        let overprint = TerminalText.styledScreen("\(sgr("31"))old\rnew", cursor: nil, rows: nil)
        XCTAssertEqual(overprint.text, "new")
        XCTAssertEqual(overprint.runs, [StyleRun(length: 3, style: CellStyle(foreground: .indexed(1)))], "the style in effect at the return still applies")
    }
    func testPrivateUseGlyphsBecomeBlankCellsOnlyWhenColumnsMatter() {
        let input = "\u{E0B0}ab"
        XCTAssertEqual(TerminalText.styledScreen(input, cursor: nil, rows: nil).text, "ab")
        XCTAssertEqual(TerminalText.styledScreen(input, cursor: (x: 0, y: 0), rows: 1).text, " ab")
    }

    // MARK: robustness

    /// A deterministic generator, so a failure can be replayed.
    private struct Generator: RandomNumberGenerator {
        var state: UInt64
        mutating func next() -> UInt64 { state = state &* 6364136223846793005 &+ 1442695040888963407; return state ^ (state >> 29) }
    }
    func testRandomInputKeepsEveryInvariantAndNeverTraps() {
        var random = Generator(state: 42)
        let pieces = ["\(esc)[", "\(esc)]", "\(esc)", "m", ";", ":", "0", "1", "38", "5", "2", "255", "999999", "-", "?", "\n", "\r", "\r\n", "\t", "a", "Z", " ", "é", "你", "🚀", "⏺", "⚠", "\u{FE0E}", "\u{FE0F}", "\u{200D}", "\u{0301}", "\u{E0B0}", "\u{07}", "\u{9B}", "[", "]", "\\", "(", "B"]
        for round in 0..<400 {
            var input = ""
            for _ in 0..<Int.random(in: 0...80, using: &random) { input += pieces.randomElement(using: &random)! }
            let rows = Int.random(in: 1...6, using: &random)
            let cursor: (x: Int, y: Int)? = round % 2 == 0 ? (x: Int.random(in: 0...12, using: &random), y: Int.random(in: 0..<rows, using: &random)) : nil
            let screen = TerminalText.styledScreen(input, cursor: cursor, rows: cursor == nil ? nil : rows)
            XCTAssertEqual(screen.runs.reduce(0) { $0 + $1.length }, screen.text.count, "runs cover the text exactly: \(input.debugDescription)")
            XCTAssertFalse(screen.text.unicodeScalars.contains { $0.value == 0x1B }, "no escape survives: \(input.debugDescription)")
            if let offset = screen.cursorOffset { XCTAssertTrue((0..<screen.text.count).contains(offset), "cursor is on a character: \(input.debugDescription)") }
            XCTAssertEqual(cursor == nil, screen.cursorOffset == nil)
        }
    }

    func testAFullSizeStyledScreenParsesQuickly() {
        // 500 lines of 300 columns with a style change every few characters.
        var line = ""
        for column in 0..<60 { line += sgr(column % 2 == 0 ? "38;2;\(column * 4);100;200;1" : "0;48;5;\(column)") + "abcd " }
        let input = Array(repeating: line, count: 500).joined(separator: "\n")
        XCTAssertGreaterThan(input.utf8.count, 100_000)
        let started = Date()
        let screen = TerminalText.styledScreen(input, cursor: (x: 10, y: 20), rows: 40)
        let seconds = Date().timeIntervalSince(started)
        XCTAssertEqual(screen.text.split(separator: "\n", omittingEmptySubsequences: false).count, 500)
        XCTAssertEqual(screen.runs.reduce(0) { $0 + $1.length }, screen.text.count)
        XCTAssertLessThan(seconds, 3, "a debug build parses 150 000 cells well inside a few seconds (a release build takes a few milliseconds)")
    }
}
