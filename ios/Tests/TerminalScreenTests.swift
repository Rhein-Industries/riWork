import XCTest
@testable import RiWorkCore

private let shell = "44444444-4444-4444-8444-444444444444"

private func parse(_ text: String) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: Data(text.utf8)) }
private func result(output: String = "hello", _ extra: [String: JSONValue] = [:]) -> JSONValue {
    .object(["shell_id": .string(shell), "output": .string(output)].merging(extra) { _, new in new })
}
private func cursor(_ x: Double, _ y: Double) -> JSONValue { .object(["x": .number(x), "y": .number(y)]) }

/// Where the cursor cell sits in a rendered screen: line and column (in characters) and the character under it.
private struct Located: Equatable { let line: Int; let column: Int; let character: Character }
private func locate(_ screen: TerminalScreen) -> Located? {
    guard let offset = screen.cursorOffset else { return nil }
    let characters = Array(screen.text)
    guard offset >= 0, offset < characters.count else { return nil }
    var line = 0, column = 0
    for character in characters[..<offset] { if character == "\n" { line += 1; column = 0 } else { column += 1 } }
    return Located(line: line, column: column, character: characters[offset])
}

@MainActor final class TerminalScreenTests: XCTestCase {
    // MARK: - h. ShellOutput

    func testAnOldPayloadHasNoCursorOrGeometry() throws {
        let output = try ShellOutput(result: parse(#"{"shell_id":"\#(shell)","output":"hello\nworld"}"#))
        XCTAssertEqual(output.shellID, shell)
        XCTAssertEqual(output.text, "hello\nworld")
        XCTAssertNil(output.cursor)
        XCTAssertNil(output.rows)
        XCTAssertNil(output.cols)
        XCTAssertFalse(output.inMode)
        XCTAssertNil(output.screen.cursorOffset)
        XCTAssertEqual(output.screen.text, TerminalText.readable("hello\nworld"))
    }

    func testANewPayloadIsParsed() throws {
        let output = try ShellOutput(result: parse(#"{"shell_id":"\#(shell)","output":"a\nb\n","cursor":{"x":3,"y":1},"rows":5,"cols":80,"in_mode":true}"#))
        XCTAssertEqual(output.cursor, ShellOutput.Cursor(x: 3, y: 1))
        XCTAssertEqual(output.rows, 5)
        XCTAssertEqual(output.cols, 80)
        XCTAssertTrue(output.inMode)
        XCTAssertEqual(output.text, "a\nb\n")
        XCTAssertEqual(output.screen, TerminalText.screen("a\nb\n", cursor: (3, 1), rows: 5))
        let origin = try ShellOutput(result: result(["cursor": cursor(0, 0), "rows": .number(1), "cols": .number(20), "in_mode": .bool(false)]))
        XCTAssertEqual(origin.cursor, ShellOutput.Cursor(x: 0, y: 0))
        XCTAssertFalse(origin.inMode)
        let last = try ShellOutput(result: result(["cursor": cursor(79, 23), "rows": .number(24), "cols": .number(80)]))
        XCTAssertEqual(last.cursor, ShellOutput.Cursor(x: 79, y: 23))
    }

    func testAMissingIdentityOrOutputThrows() {
        func rejects(_ value: JSONValue, _ why: String) {
            XCTAssertThrowsError(try ShellOutput(result: value), why) { error in
                guard case RemoteError.protocolViolation = error else { return XCTFail("\(why): \(error)") }
            }
        }
        rejects(.object(["output": .string("x")]), "no shell_id")
        rejects(.object(["shell_id": .string(shell)]), "no output")
        rejects(.object([:]), "empty object")
        rejects(.object(["shell_id": .null, "output": .string("x")]), "null shell_id")
        rejects(.object(["shell_id": .string(shell), "output": .null]), "null output")
        rejects(.object(["shell_id": .number(1), "output": .string("x")]), "numeric shell_id")
        rejects(.object(["shell_id": .string(shell), "output": .number(1)]), "numeric output")
        rejects(.null, "null result")
        rejects(.string("output"), "string result")
        rejects(.array([]), "array result")
        XCTAssertNoThrow(try ShellOutput(result: result(output: "")), "an empty screen is a valid answer")
    }

    func testMalformedGeometryIsIgnoredAndTheScreenSurvives() throws {
        let unusable: [(String, JSONValue)] = [
            ("negative", .number(-1)), ("fractional", .number(5.5)), ("string", .string("5")), ("huge", .number(1e300)), ("nan", .number(.nan)),
            ("infinite", .number(.infinity)), ("zero", .number(0)), ("null", .null), ("bool", .bool(true)), ("array", .array([.number(5)])), ("object", .object([:]))
        ]
        for (why, value) in unusable {
            let rows = try ShellOutput(result: result(output: "keep me", ["rows": value, "cols": .number(80), "cursor": cursor(1, 0)]))
            XCTAssertNil(rows.rows, "rows: \(why)")
            XCTAssertNil(rows.cursor, "a cursor needs a usable rows (\(why))")
            XCTAssertEqual(rows.cols, 80)
            XCTAssertEqual(rows.text, "keep me")
            XCTAssertEqual(rows.screen.text, "keep me")
            XCTAssertNil(rows.screen.cursorOffset)

            let cols = try ShellOutput(result: result(output: "keep me", ["rows": .number(5), "cols": value, "cursor": cursor(1, 0)]))
            XCTAssertNil(cols.cols, "cols: \(why)")
            XCTAssertEqual(cols.rows, 5, "rows are independent of cols (\(why))")
            XCTAssertEqual(cols.cursor, ShellOutput.Cursor(x: 1, y: 0))
        }
    }

    func testMalformedCursorIsIgnored() throws {
        let unusable: [(String, JSONValue)] = [
            ("negative x", cursor(-1, 0)), ("negative y", cursor(0, -1)), ("fractional x", cursor(1.5, 0)), ("fractional y", cursor(0, 0.5)),
            ("huge x", cursor(1e300, 0)), ("huge y", cursor(0, 1e300)), ("nan x", cursor(.nan, 0)), ("infinite y", cursor(0, .infinity)),
            ("y equal to rows", cursor(0, 5)), ("y beyond rows", cursor(0, 9)),
            ("string x", .object(["x": .string("1"), "y": .number(0)])), ("string y", .object(["x": .number(1), "y": .string("0")])),
            ("missing x", .object(["y": .number(0)])), ("missing y", .object(["x": .number(1)])), ("null x", .object(["x": .null, "y": .number(0)])),
            ("empty object", .object([:])), ("string", .string("1,0")), ("array", .array([.number(1), .number(0)])), ("null", .null), ("number", .number(3))
        ]
        for (why, value) in unusable {
            let output = try ShellOutput(result: result(output: "prompt$ ", ["rows": .number(5), "cols": .number(80), "cursor": value, "in_mode": .bool(true)]))
            XCTAssertNil(output.cursor, why)
            XCTAssertEqual(output.rows, 5, why)
            XCTAssertEqual(output.cols, 80, why)
            XCTAssertTrue(output.inMode, why)
            XCTAssertEqual(output.text, "prompt$ ", why)
            XCTAssertEqual(output.screen, TerminalScreen(text: "prompt$ ", cursorOffset: nil), why)
        }
        let edge = try ShellOutput(result: result(["rows": .number(5), "cursor": cursor(0, 4)]))
        XCTAssertEqual(edge.cursor, ShellOutput.Cursor(x: 0, y: 4), "the last row is inside the screen")
        let withoutRows = try ShellOutput(result: result(["cursor": cursor(0, 0)]))
        XCTAssertNil(withoutRows.cursor, "a cursor without rows cannot be placed")
        XCTAssertNil(withoutRows.rows)
    }

    func testInModeIsOnlyTrueForABoolean() throws {
        XCTAssertTrue(try ShellOutput(result: result(["in_mode": .bool(true)])).inMode)
        XCTAssertFalse(try ShellOutput(result: result(["in_mode": .bool(false)])).inMode)
        for value: JSONValue in [.string("true"), .number(1), .null, .array([]), .object([:])] {
            XCTAssertFalse(try ShellOutput(result: result(["in_mode": value])).inMode, "\(value)")
        }
        XCTAssertFalse(try ShellOutput(result: result()).inMode)
    }

    // MARK: - i. Screen mapping

    func testWithoutACursorTheTextIsExactlyTheReadableText() {
        let raw = "\u{1b}[32muser\u{1b}[0m\u{e0b0} ls\r\nold\rnew\n\u{1b}]0;title\u{7}done\n\n  \n"
        let expected = TerminalScreen(text: TerminalText.readable(raw), cursorOffset: nil)
        XCTAssertEqual(expected.text, "user ls\nnew\ndone")
        XCTAssertEqual(TerminalText.screen(raw, cursor: nil, rows: 5), expected)
        XCTAssertEqual(TerminalText.screen(raw, cursor: nil, rows: nil), expected)
        XCTAssertEqual(TerminalText.screen(raw, cursor: (2, 1), rows: nil), expected, "a cursor cannot be placed without rows")
        XCTAssertEqual(TerminalText.screen(raw, cursor: (2, 1), rows: 0), expected)
        XCTAssertEqual(TerminalText.screen(raw, cursor: (2, 1), rows: -3), expected)
        XCTAssertEqual(TerminalText.screen(raw, cursor: (2, 5), rows: 5), expected, "y == rows is outside the screen")
        XCTAssertEqual(TerminalText.screen(raw, cursor: (2, 9), rows: 5), expected)
        XCTAssertEqual(TerminalText.screen(raw, cursor: (-1, 1), rows: 5), expected)
        XCTAssertEqual(TerminalText.screen(raw, cursor: (2, -1), rows: 5), expected)
        XCTAssertEqual(TerminalText.screen("", cursor: nil, rows: 3), TerminalScreen(text: "", cursorOffset: nil))
    }

    func testTheCursorIsRelativeToTheLastRowsOfTheOutput() throws {
        let history = (0..<30).map { "history \($0)" }
        let visible = ["row0 $ ", "row1 data", "row2 > prompt", "row3 data", "row4 data"]
        let output = (history + visible).joined(separator: "\n") + "\n"
        let screen = TerminalText.screen(output, cursor: (x: 5, y: 2), rows: 5)
        XCTAssertEqual(screen.text, (history + visible).joined(separator: "\n"))
        let located = try XCTUnwrap(locate(screen))
        XCTAssertEqual(located, Located(line: 32, column: 5, character: ">"), "y=2 is the third row of the screen, not the third line of the output")
        XCTAssertEqual(screen.text.split(separator: "\n", omittingEmptySubsequences: false)[located.line], "row2 > prompt")
        // The first screen row and the last one.
        XCTAssertEqual(locate(TerminalText.screen(output, cursor: (0, 0), rows: 5)), Located(line: 30, column: 0, character: "r"))
        XCTAssertEqual(locate(TerminalText.screen(output, cursor: (4, 4), rows: 5)), Located(line: 34, column: 4, character: " "))
        // History is kept above the screen.
        XCTAssertTrue(screen.text.hasPrefix("history 0\nhistory 1\n"))
        // The same through the daemon result.
        let viaResult = try ShellOutput(result: result(output: output, ["cursor": cursor(5, 2), "rows": .number(5)]))
        XCTAssertEqual(viaResult.screen, screen)
    }

    func testShorterOutputThanTheScreenStartsAtRowZero() {
        let screen = TerminalText.screen("one\ntwo", cursor: (1, 1), rows: 10)
        XCTAssertEqual(screen.text, "one\ntwo")
        XCTAssertEqual(locate(screen), Located(line: 1, column: 1, character: "w"))
    }

    func testATrailingNewlineDoesNotStartAnotherLine() throws {
        let withNewline = TerminalText.screen("a\nb\n", cursor: (0, 1), rows: 2)
        let without = TerminalText.screen("a\nb", cursor: (0, 1), rows: 2)
        XCTAssertEqual(withNewline, without)
        XCTAssertEqual(withNewline.text, "a\nb")
        XCTAssertEqual(withNewline.cursorOffset, 2)
        XCTAssertEqual(locate(withNewline), Located(line: 1, column: 0, character: "b"))
        // One row, one prompt: y=0 is the prompt, not a phantom empty line after it.
        XCTAssertEqual(locate(TerminalText.screen("prompt\n", cursor: (0, 0), rows: 1)), Located(line: 0, column: 0, character: "p"))
        XCTAssertEqual(TerminalText.screen("prompt\n", cursor: (0, 0), rows: 1).text, "prompt")
        // A CRLF-terminated screen behaves the same.
        XCTAssertEqual(TerminalText.screen("a\r\nb\r\n", cursor: (0, 1), rows: 2), withNewline)
        // Only one newline is a terminator; a second one is a real blank row.
        let blankLast = TerminalText.screen("a\n\n", cursor: (0, 1), rows: 2)
        XCTAssertEqual(blankLast.text, "a\n ")
        XCTAssertEqual(locate(blankLast), Located(line: 1, column: 0, character: " "))
    }

    func testTheCursorOnABlankRowBelowThePromptIsKeptAndLaterBlankRowsAreTrimmed() throws {
        let output = "user$ ls\nfile.txt\n\n\n\n"
        let screen = TerminalText.screen(output, cursor: (0, 2), rows: 5)
        XCTAssertEqual(screen.text, "user$ ls\nfile.txt\n ", "the cursor row survives, the two blank rows after it do not")
        XCTAssertEqual(locate(screen), Located(line: 2, column: 0, character: " "))
        XCTAssertEqual(screen.cursorOffset, screen.text.count - 1)
        XCTAssertEqual(TerminalText.readable(output), "user$ ls\nfile.txt", "without a cursor every trailing blank row goes")
        // Blank rows between the cursor and later text stay.
        let between = TerminalText.screen("a\n\n\nd", cursor: (0, 1), rows: 4)
        XCTAssertEqual(between.text, "a\n \n\nd")
        XCTAssertEqual(locate(between), Located(line: 1, column: 0, character: " "))
        // Whitespace-only rows count as blank.
        XCTAssertEqual(TerminalText.screen("a\n\n  \n \n", cursor: (0, 0), rows: 4).text, "a")
        // The cursor row below the last output line pads with blank rows.
        let below = TerminalText.screen("a", cursor: (0, 3), rows: 5)
        XCTAssertEqual(below.text, "a\n\n\n ")
        XCTAssertEqual(locate(below), Located(line: 3, column: 0, character: " "))
    }

    func testTheCursorPastTheEndOfALinePadsWithSpaces() {
        let far = TerminalText.screen("ab\n", cursor: (5, 0), rows: 1)
        XCTAssertEqual(far.text, "ab    ", "three pad cells and the cursor cell")
        XCTAssertEqual(locate(far), Located(line: 0, column: 5, character: " "))
        let farBelow = TerminalText.screen("ab\ncd\n", cursor: (4, 1), rows: 2)
        XCTAssertEqual(farBelow.text, "ab\ncd   ")
        XCTAssertEqual(locate(farBelow), Located(line: 1, column: 4, character: " "))
        let empty = TerminalText.screen("\n", cursor: (3, 0), rows: 1)
        XCTAssertEqual(empty.text, "    ")
        XCTAssertEqual(locate(empty), Located(line: 0, column: 3, character: " "))
        XCTAssertEqual(TerminalText.screen("", cursor: (0, 0), rows: 1), TerminalScreen(text: " ", cursorOffset: 0))
    }

    func testTheCursorAtTheEndOfThePromptAddsOneCell() {
        let prompt = "user@host:~$ "
        let screen = TerminalText.screen(prompt + "\n", cursor: (prompt.count, 0), rows: 1)
        XCTAssertEqual(screen.text, prompt + " ")
        XCTAssertEqual(screen.cursorOffset, 13)
        XCTAssertEqual(locate(screen), Located(line: 0, column: 13, character: " "))
        XCTAssertEqual(TerminalText.screen("$", cursor: (1, 0), rows: 1), TerminalScreen(text: "$ ", cursorOffset: 1))
        // On a character, nothing is added.
        XCTAssertEqual(TerminalText.screen("$ ", cursor: (1, 0), rows: 1), TerminalScreen(text: "$ ", cursorOffset: 1))
        XCTAssertEqual(TerminalText.screen("hello", cursor: (1, 0), rows: 1), TerminalScreen(text: "hello", cursorOffset: 1))
        XCTAssertEqual(TerminalText.screen("hello", cursor: (0, 0), rows: 1), TerminalScreen(text: "hello", cursorOffset: 0))
        XCTAssertEqual(TerminalText.screen("hello", cursor: (4, 0), rows: 1), TerminalScreen(text: "hello", cursorOffset: 4))
        XCTAssertEqual(TerminalText.screen("hello", cursor: (5, 0), rows: 1), TerminalScreen(text: "hello ", cursorOffset: 5))
    }

    func testCursorOffsetPointsAtTheRightCharacterOfTheText() throws {
        let output = "ab\ncd\nef"
        for (x, y, line, column, expected) in [(0, 0, 0, 0, "a"), (1, 0, 0, 1, "b"), (0, 1, 1, 0, "c"), (1, 1, 1, 1, "d"), (0, 2, 2, 0, "e"), (1, 2, 2, 1, "f")] as [(Int, Int, Int, Int, Character)] {
            let screen = TerminalText.screen(output, cursor: (x, y), rows: 3)
            XCTAssertEqual(screen.text, output)
            let located = try XCTUnwrap(locate(screen))
            XCTAssertEqual(located, Located(line: line, column: column, character: expected), "(\(x), \(y))")
            let offset = try XCTUnwrap(screen.cursorOffset)
            XCTAssertEqual(screen.text[screen.text.index(screen.text.startIndex, offsetBy: offset)], expected)
        }
        XCTAssertEqual(TerminalText.screen(output, cursor: (1, 2), rows: 3).cursorOffset, 7)
        // Lines with several kinds of characters before the cursor row.
        let mixed = TerminalText.screen("héllo 🙂\n你好\nz", cursor: (0, 2), rows: 3)
        XCTAssertEqual(locate(mixed), Located(line: 2, column: 0, character: "z"))
        XCTAssertEqual(mixed.cursorOffset, "héllo 🙂\n你好\n".count)
    }

    func testWideCharactersCountTwoCells() throws {
        let line = "你好ab"
        for (x, offset, character) in [(0, 0, "你"), (2, 1, "好"), (4, 2, "a"), (5, 3, "b")] as [(Int, Int, Character)] {
            let screen = TerminalText.screen(line, cursor: (x, 0), rows: 1)
            XCTAssertEqual(screen.text, line, "x=\(x)")
            XCTAssertEqual(screen.cursorOffset, offset, "x=\(x)")
            XCTAssertEqual(locate(screen)?.character, character, "x=\(x)")
        }
        let end = TerminalText.screen(line, cursor: (6, 0), rows: 1)
        XCTAssertEqual(end, TerminalScreen(text: "你好ab ", cursorOffset: 4), "after 6 cells the line is over")
        let past = TerminalText.screen(line, cursor: (8, 0), rows: 1)
        XCTAssertEqual(past, TerminalScreen(text: "你好ab   ", cursorOffset: 6))
        XCTAssertEqual(TerminalText.screen("🙂a", cursor: (2, 0), rows: 1), TerminalScreen(text: "🙂a", cursorOffset: 1), "emoji are two cells")
        XCTAssertEqual(TerminalText.screen("a你b", cursor: (3, 0), rows: 1), TerminalScreen(text: "a你b", cursorOffset: 2))
        XCTAssertEqual(TerminalText.screen("한글x", cursor: (2, 0), rows: 1), TerminalScreen(text: "한글x", cursorOffset: 1), "Hangul syllables are wide")
        XCTAssertEqual(TerminalText.screen("ｱｲ", cursor: (1, 0), rows: 1), TerminalScreen(text: "ｱｲ", cursorOffset: 1), "half-width katakana are narrow")
    }

    func testCursorOnTheSecondCellOfAWideCharacterStaysOnThatCharacter() throws {
        // A real terminal can put the cursor on the right half of a wide character. The cursor must stay on that
        // character; it must not fall off the end of the line.
        let leading = TerminalText.screen("你a", cursor: (1, 0), rows: 1)
        XCTAssertEqual(leading, TerminalScreen(text: "你a", cursorOffset: 0), "x=1 is the right half of 你")
        let middle = TerminalText.screen("a你b", cursor: (2, 0), rows: 1)
        XCTAssertEqual(middle, TerminalScreen(text: "a你b", cursorOffset: 1), "x=2 is the right half of 你")
    }

    func testAPrivateUseGlyphKeepsColumnsAlignedWhenThereIsACursor() throws {
        let prompt = "\u{e0b0} ~ $ "
        XCTAssertEqual(TerminalText.readable(prompt), " ~ $ ", "without a cursor the glyph is dropped")
        let onTilde = TerminalText.screen(prompt, cursor: (2, 0), rows: 1)
        XCTAssertEqual(onTilde.text, "  ~ $ ", "with a cursor it stays one cell wide, as a space")
        XCTAssertEqual(locate(onTilde), Located(line: 0, column: 2, character: "~"))
        let onGlyph = TerminalText.screen(prompt, cursor: (0, 0), rows: 1)
        XCTAssertEqual(locate(onGlyph), Located(line: 0, column: 0, character: " "))
        let atEnd = TerminalText.screen(prompt, cursor: (6, 0), rows: 1)
        XCTAssertEqual(atEnd.text, "  ~ $  ")
        XCTAssertEqual(atEnd.cursorOffset, 6)
        let inside = TerminalText.screen("a\u{e000}b", cursor: (2, 0), rows: 1)
        XCTAssertEqual(inside.text, "a b")
        XCTAssertEqual(locate(inside)?.character, "b")
        // Every terminal row keeps the same treatment, not just the cursor row.
        let multi = TerminalText.screen("\u{e0b0} one\n\u{e0b0} two\n", cursor: (2, 1), rows: 2)
        XCTAssertEqual(multi.text, "  one\n  two")
        XCTAssertEqual(locate(multi), Located(line: 1, column: 2, character: "t"))
    }

    func testEscapeSequencesAreStrippedBeforeTheCursorIsMapped() throws {
        let raw = "\u{1b}[1;32muser@host\u{1b}[0m:\u{1b}[34m~\u{1b}[0m$ \u{1b}]0;window title\u{7}"
        let atEnd = TerminalText.screen(raw, cursor: (13, 0), rows: 1)
        XCTAssertEqual(atEnd.text, "user@host:~$  ")
        XCTAssertEqual(atEnd.cursorOffset, 13)
        XCTAssertFalse(atEnd.text.unicodeScalars.contains { $0.value < 32 })
        XCTAssertEqual(locate(TerminalText.screen(raw, cursor: (10, 0), rows: 1))?.character, "~")
        XCTAssertEqual(locate(TerminalText.screen(raw, cursor: (9, 0), rows: 1))?.character, ":")
        let overprint = TerminalText.screen("old text\rnew", cursor: (3, 0), rows: 1)
        XCTAssertEqual(overprint, TerminalScreen(text: "new ", cursorOffset: 3), "a carriage return overprints the row")
        let crlf = TerminalText.screen("a\r\nb\r\n", cursor: (0, 1), rows: 2)
        XCTAssertEqual(crlf, TerminalScreen(text: "a\nb", cursorOffset: 2))
        // Escape sequences on scrollback rows do not shift the cursor row either.
        let history = "\u{1b}[31mred\u{1b}[0m\n\u{1b}[32mgreen\u{1b}[0m\nprompt> \n"
        XCTAssertEqual(locate(TerminalText.screen(history, cursor: (8, 0), rows: 1)), Located(line: 2, column: 8, character: " "))
    }

    func testReadableTerminalTextIsUnchanged() {
        XCTAssertEqual(TerminalText.readable("\u{1b}[32mhello\u{1b}[0m\r\nold\rnew\n\u{1b}]0;title\u{7}done"), "hello\nnew\ndone")
        XCTAssertEqual(TerminalText.readable("\u{e000}branch\n\n  \n"), "branch")
        XCTAssertEqual(TerminalText.readable(""), "")
        XCTAssertEqual(TerminalText.readable("a  \n \n"), "a  ", "only whole blank rows are trimmed")
        XCTAssertEqual(TerminalText.readable("\n\na"), "\n\na", "leading blank rows stay")
        XCTAssertEqual(TerminalText.readable("a\tb"), "a\tb")
        XCTAssertEqual(TerminalText.readable("a\u{7}b\u{7f}c"), "abc")
        XCTAssertEqual(TerminalText.readable("你好 🙂 é"), "你好 🙂 é")
    }

    func testCellWidths() {
        for character: Character in ["a", "Z", " ", "é", "~", "ｱ", "\u{301}", "1", "#", "©", "→", "✓"] { XCTAssertEqual(TerminalText.cellWidth(character), 1, "\(character)") }
        for character: Character in ["你", "好", "한", "글", "あ", "カ", "Ａ", "🙂", "🤖", "🚀", "✅", "✨", "❌", "⭐", "🩷"] { XCTAssertEqual(TerminalText.cellWidth(character), 2, "\(character)") }
    }

    // MARK: - j. PollCadence

    func testCadenceFollowsTheElapsedTimeSinceKeyActivity() {
        let cadence = PollCadence(resting: .seconds(3))
        XCTAssertEqual(cadence.interval(sinceKeyActivity: nil), .seconds(3), "no keys, resting")
        XCTAssertEqual(cadence.interval(sinceKeyActivity: 0), .milliseconds(300))
        XCTAssertEqual(cadence.interval(sinceKeyActivity: 0.5), .milliseconds(300))
        XCTAssertEqual(cadence.interval(sinceKeyActivity: 1.999), .milliseconds(300))
        XCTAssertEqual(cadence.interval(sinceKeyActivity: 2), .seconds(1))
        XCTAssertEqual(cadence.interval(sinceKeyActivity: 5), .seconds(1))
        XCTAssertEqual(cadence.interval(sinceKeyActivity: 11.999), .seconds(1))
        XCTAssertEqual(cadence.interval(sinceKeyActivity: 12), .seconds(3))
        XCTAssertEqual(cadence.interval(sinceKeyActivity: 3600), .seconds(3))
    }

    func testCadenceTreatsANonsensicalElapsedTimeAsResting() {
        let cadence = PollCadence(resting: .seconds(3))
        XCTAssertEqual(cadence.interval(sinceKeyActivity: -1), .seconds(3), "a clock that went backwards")
        XCTAssertEqual(cadence.interval(sinceKeyActivity: .nan), .seconds(3))
        XCTAssertEqual(cadence.interval(sinceKeyActivity: .infinity), .seconds(3))
    }

    func testCadenceNeverExceedsTheRestingInterval() {
        let tiny = PollCadence(resting: .milliseconds(10))
        for elapsed in [nil, 0, 1, 2, 5, 11.9, 12, 100] as [TimeInterval?] {
            XCTAssertEqual(tiny.interval(sinceKeyActivity: elapsed), .milliseconds(10), "\(String(describing: elapsed))")
        }
        let short = PollCadence(resting: .milliseconds(500))
        XCTAssertEqual(short.interval(sinceKeyActivity: 0), .milliseconds(300), "fast is still faster than resting")
        XCTAssertEqual(short.interval(sinceKeyActivity: 5), .milliseconds(500), "medium is capped at resting")
        XCTAssertEqual(short.interval(sinceKeyActivity: 50), .milliseconds(500))
        let long = PollCadence(resting: .seconds(30))
        XCTAssertEqual(long.interval(sinceKeyActivity: 0), .milliseconds(300))
        XCTAssertEqual(long.interval(sinceKeyActivity: 5), .seconds(1))
        XCTAssertEqual(long.interval(sinceKeyActivity: 50), .seconds(30))
        for resting in [Duration.milliseconds(10), .milliseconds(300), .seconds(1), .seconds(3), .seconds(60)] {
            let cadence = PollCadence(resting: resting)
            for elapsed in stride(from: 0.0, through: 30.0, by: 0.25) { XCTAssertLessThanOrEqual(cadence.interval(sinceKeyActivity: elapsed), resting) }
        }
    }

    // MARK: - k. TerminalLayout and TerminalFontSize

    private func grid(_ layout: TerminalLayout, _ width: Double, _ height: Double, font: Double = 12) -> TerminalViewport? {
        let cell = TerminalLayout.approximateCell(fontSize: font)
        return layout.viewport(width: width, height: height, cellWidth: cell.width, lineHeight: cell.height)
    }

    func testLayoutPaddings() {
        XCTAssertEqual(TerminalLayout.normal.padding, 4)
        XCTAssertEqual(TerminalLayout.focus.padding, 2)
        XCTAssertEqual(TerminalLayout.normal, TerminalLayout(padding: 4))
        XCTAssertNotEqual(TerminalLayout.normal, TerminalLayout.focus)
    }

    func testApproximateCellGrowsWithTheFont() {
        let twelve = TerminalLayout.approximateCell(fontSize: 12)
        XCTAssertEqual(twelve.width, 7.224, accuracy: 1e-9)
        XCTAssertEqual(twelve.height, 15)
        XCTAssertEqual(TerminalLayout.approximateCell(fontSize: 8).width, 4.816, accuracy: 1e-9)
        XCTAssertEqual(TerminalLayout.approximateCell(fontSize: 8).height, 10)
        XCTAssertEqual(TerminalLayout.approximateCell(fontSize: 24).height, 29)
        var previous = TerminalLayout.approximateCell(fontSize: 8)
        for size in 9...24 {
            let next = TerminalLayout.approximateCell(fontSize: Double(size))
            XCTAssertGreaterThan(next.width, previous.width)
            XCTAssertGreaterThanOrEqual(next.height, previous.height)
            previous = next
        }
    }

    func testTheLayoutFitsTheViewportWithItsPaddingRemoved() {
        let cell = TerminalLayout.approximateCell(fontSize: 12)
        for layout in [TerminalLayout.normal, .focus] {
            XCTAssertEqual(layout.viewport(width: 393, height: 760, cellWidth: cell.width, lineHeight: cell.height),
                           TerminalViewport.fit(width: 393 - 2 * layout.padding, height: 760 - 2 * layout.padding, cellWidth: cell.width, lineHeight: cell.height))
        }
        XCTAssertEqual(grid(.normal, 393, 760), TerminalViewport(columns: 53, rows: 50))
        XCTAssertEqual(grid(.focus, 393, 760), TerminalViewport(columns: 53, rows: 50), "this particular size gains nothing")
    }

    func testFocusModeGainsCellsOnRealisticPhoneSizes() {
        XCTAssertEqual(grid(.normal, 388, 755), TerminalViewport(columns: 52, rows: 49))
        XCTAssertEqual(grid(.focus, 388, 755), TerminalViewport(columns: 53, rows: 50))
        XCTAssertEqual(grid(.normal, 390, 700), TerminalViewport(columns: 52, rows: 46))
        XCTAssertEqual(grid(.focus, 390, 700), TerminalViewport(columns: 53, rows: 46))
        XCTAssertEqual(grid(.normal, 375, 600), TerminalViewport(columns: 50, rows: 39))
        XCTAssertEqual(grid(.focus, 375, 600), TerminalViewport(columns: 51, rows: 39))
        XCTAssertEqual(grid(.normal, 430, 800), TerminalViewport(columns: 58, rows: 52))
        XCTAssertEqual(grid(.focus, 430, 800), TerminalViewport(columns: 58, rows: 53))
    }

    func testFocusModeNeverHasFewerCellsThanTheNormalLayout() throws {
        var columnGains = 0, rowGains = 0, total = 0
        for font in [10.0, 12, 14] {
            for width in stride(from: 320.0, through: 440, by: 1) {
                for height in stride(from: 500.0, through: 950, by: 1) {
                    let normal = try XCTUnwrap(grid(.normal, width, height, font: font))
                    let focus = try XCTUnwrap(grid(.focus, width, height, font: font))
                    total += 1
                    if focus.columns < normal.columns || focus.rows < normal.rows { return XCTFail("focus is smaller at \(width)x\(height) @\(font)pt: \(focus) < \(normal)") }
                    if focus.columns > normal.columns { columnGains += 1 }
                    if focus.rows > normal.rows { rowGains += 1 }
                }
            }
        }
        XCTAssertGreaterThan(columnGains, 0, "focus mode must add columns for some phone sizes (of \(total))")
        XCTAssertGreaterThan(rowGains, 0, "focus mode must add rows for some phone sizes (of \(total))")
    }

    func testALargerFontMeansFewerColumnsAndRows() throws {
        for layout in [TerminalLayout.normal, .focus] {
            var previous = try XCTUnwrap(grid(layout, 393, 760, font: 8))
            for size in 9...24 {
                let next = try XCTUnwrap(grid(layout, 393, 760, font: Double(size)))
                XCTAssertLessThanOrEqual(next.columns, previous.columns, "\(size)pt")
                XCTAssertLessThanOrEqual(next.rows, previous.rows, "\(size)pt")
                previous = next
            }
            let small = try XCTUnwrap(grid(layout, 393, 760, font: 8)), large = try XCTUnwrap(grid(layout, 393, 760, font: 24))
            XCTAssertGreaterThan(small.columns, large.columns)
            XCTAssertGreaterThan(small.rows, large.rows)
            XCTAssertLessThan(try XCTUnwrap(grid(layout, 393, 760, font: 16)).columns, try XCTUnwrap(grid(layout, 393, 760, font: 12)).columns)
        }
    }

    func testTheViewportBoundsStillApply() {
        for layout in [TerminalLayout.normal, .focus] {
            XCTAssertEqual(grid(layout, 60, 60), TerminalViewport(columns: 20, rows: 8), "tiny containers still get the minimum grid")
            XCTAssertEqual(grid(layout, 5000, 5000, font: 8), TerminalViewport(columns: 300, rows: 160))
            XCTAssertEqual(grid(layout, 5000, 300, font: 8)?.columns, 300)
            XCTAssertEqual(grid(layout, 393, 5000, font: 8)?.rows, 160)
            XCTAssertEqual(grid(layout, 200, 5000, font: 24)?.columns, 20)
            XCTAssertNil(grid(layout, 0, 500))
            XCTAssertNil(grid(layout, 400, 0))
            XCTAssertNil(grid(layout, -10, 500))
            XCTAssertNil(grid(layout, .nan, 500))
            XCTAssertNil(grid(layout, 400, .infinity))
            XCTAssertNil(layout.viewport(width: 400, height: 500, cellWidth: 0, lineHeight: 15))
            XCTAssertNil(layout.viewport(width: 400, height: 500, cellWidth: 7, lineHeight: .nan))
        }
        // The padding is taken off before fitting: an 8pt wide pane has nothing left with 4pt padding but something with 2pt.
        XCTAssertNil(grid(.normal, 8, 500))
        XCTAssertEqual(grid(.focus, 8, 500)?.columns, 20)
        XCTAssertNil(grid(.focus, 4, 500))
        XCTAssertNil(grid(.normal, 500, 8))
        XCTAssertEqual(grid(.focus, 500, 8)?.rows, 8)
    }

    func testFontSizeRangeAndClamping() {
        XCTAssertEqual(TerminalFontSize.range, 8...24)
        XCTAssertEqual(TerminalFontSize.standard, 12)
        XCTAssertEqual(TerminalFontSize.step, 1)
        for (input, expected) in [(12.0, 12.0), (8, 8), (24, 24), (7, 8), (0, 8), (-5, 8), (25, 24), (1000, 24), (12.4, 12), (12.5, 13), (12.6, 13), (7.6, 8), (23.6, 24), (9.49, 9)] {
            XCTAssertEqual(TerminalFontSize.clamped(input), expected, "\(input)")
        }
        XCTAssertEqual(TerminalFontSize.clamped(.nan), TerminalFontSize.standard)
    }

    func testFontSizeStepping() {
        XCTAssertEqual(TerminalFontSize.stepped(12, by: 1), 13)
        XCTAssertEqual(TerminalFontSize.stepped(12, by: -1), 11)
        XCTAssertEqual(TerminalFontSize.stepped(12, by: 0), 12)
        XCTAssertEqual(TerminalFontSize.stepped(12, by: 3), 15)
        XCTAssertEqual(TerminalFontSize.stepped(12, by: -4), 8)
        XCTAssertEqual(TerminalFontSize.stepped(12, by: 100), 24)
        XCTAssertEqual(TerminalFontSize.stepped(12, by: -100), 8)
        XCTAssertEqual(TerminalFontSize.stepped(24, by: 1), 24)
        XCTAssertEqual(TerminalFontSize.stepped(8, by: -1), 8)
        XCTAssertEqual(TerminalFontSize.stepped(12, by: Int.max), 24)
        XCTAssertEqual(TerminalFontSize.stepped(12, by: Int.min), 8)
        XCTAssertEqual(TerminalFontSize.stepped(.nan, by: 1), TerminalFontSize.standard, "an unreadable stored size falls back to the standard")
        var size = 8.0
        for _ in 0..<30 { size = TerminalFontSize.stepped(size, by: 1) }
        XCTAssertEqual(size, 24)
    }

    func testPinchScalesFromTheSizeWhenTheGestureBegan() {
        XCTAssertEqual(TerminalFontSize.pinched(from: 12, scale: 1.5), 18)
        XCTAssertEqual(TerminalFontSize.pinched(from: 12, scale: 1), 12)
        XCTAssertEqual(TerminalFontSize.pinched(from: 12, scale: 2), 24)
        XCTAssertEqual(TerminalFontSize.pinched(from: 12, scale: 0.75), 9)
        XCTAssertEqual(TerminalFontSize.pinched(from: 12, scale: 1.04), 12, "small movements round away")
        XCTAssertEqual(TerminalFontSize.pinched(from: 12, scale: 1.05), 13)
        XCTAssertEqual(TerminalFontSize.pinched(from: 20, scale: 0.5), 10)
        XCTAssertEqual(TerminalFontSize.pinched(from: 12, scale: 3), 24, "36pt clamps")
        XCTAssertEqual(TerminalFontSize.pinched(from: 12, scale: 1000), 24)
        XCTAssertEqual(TerminalFontSize.pinched(from: 12, scale: 0.5), 8, "6pt clamps")
        XCTAssertEqual(TerminalFontSize.pinched(from: 12, scale: 0.0001), 8)
        XCTAssertEqual(TerminalFontSize.pinched(from: 5, scale: 1), 8, "an out-of-range start is clamped")
        for scale in [0.0, -1, .nan, .infinity, -.infinity] {
            XCTAssertEqual(TerminalFontSize.pinched(from: 14, scale: scale), 14, "scale \(scale) leaves the size alone")
        }
    }
}
