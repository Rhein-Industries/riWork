import XCTest
@testable import RiWorkCore

private let esc = "\u{1B}"
private func sgr(_ body: String) -> String { "\(esc)[\(body)m" }
private let vs15 = "\u{FE0E}", vs16 = "\u{FE0F}"

/// Where the cursor cell sits in a styled screen, and which characters and styles are around it.
private func cell(_ screen: StyledScreen, at offset: Int) -> (character: Character, style: CellStyle)? {
    let characters = Array(screen.text)
    guard offset >= 0, offset < characters.count else { return nil }
    var start = 0
    for run in screen.runs {
        if offset < start + run.length { return (characters[offset], run.style) }
        start += run.length
    }
    return nil
}

final class StyledScreenTests: XCTestCase {
    // MARK: the cursor cell does not care about attributes

    func testCursorLandsOnTheSameCharacterWithAndWithoutStyles() {
        let plain = "$ ls\nfile_one  file_two\n$ "
        let styled = "\(sgr("1;32"))$ \(sgr("0"))ls\n\(sgr("34"))file_one\(sgr("0"))  \(sgr("38;5;208"))file_two\(sgr("0"))\n\(sgr("1;32"))$ \(sgr("0"))"
        for (x, y) in [(2, 2), (0, 0), (3, 0), (8, 1), (14, 1), (20, 1), (0, 2)] {
            let a = TerminalText.screen(plain, cursor: (x, y), rows: 3)
            let b = TerminalText.styledScreen(styled, cursor: (x, y), rows: 3)
            XCTAssertEqual(b.text, a.text, "cursor \(x),\(y)")
            XCTAssertEqual(b.cursorOffset, a.cursorOffset, "cursor \(x),\(y)")
            XCTAssertEqual(b.runs.reduce(0) { $0 + $1.length }, b.text.count)
        }
    }
    func testCursorPastTheEndOfAStyledLineIsPaddedWithPlainBlanks() {
        let screen = TerminalText.styledScreen("\(sgr("31"))red\(sgr("0"))", cursor: (x: 6, y: 0), rows: 1)
        XCTAssertEqual(screen.text, "red    ")
        XCTAssertEqual(screen.cursorOffset, 6)
        let under = cell(screen, at: 6)
        XCTAssertEqual(under?.character, " ")
        XCTAssertEqual(under?.style, .plain, "the blank under the cursor takes no style from the text before it")
        XCTAssertEqual(cell(screen, at: 0)?.style, CellStyle(foreground: .indexed(1)))
    }
    func testCursorOnAStyledCharacterKeepsThatCharactersStyleAsTheCellBeneath() {
        let screen = TerminalText.styledScreen("ab\(sgr("1;33;44"))cd\(sgr("0"))ef", cursor: (x: 3, y: 0), rows: 1)
        XCTAssertEqual(screen.text, "abcdef")
        XCTAssertEqual(screen.cursorOffset, 3)
        XCTAssertEqual(cell(screen, at: 3)?.character, "d")
        XCTAssertEqual(cell(screen, at: 3)?.style, CellStyle(foreground: .indexed(3), background: .indexed(4), attributes: .bold))
    }
    func testWideCharactersCountTwoCellsWhateverTheirStyle() {
        let input = "\(sgr("31"))你好\(sgr("0")) ok"
        for (x, expected) in [(0, 0), (1, 0), (2, 1), (3, 1), (4, 2), (5, 3), (6, 4)] {
            let styled = TerminalText.styledScreen(input, cursor: (x, 0), rows: 1)
            let plain = TerminalText.screen("你好 ok", cursor: (x, 0), rows: 1)
            XCTAssertEqual(styled.cursorOffset, expected, "column \(x)")
            XCTAssertEqual(styled.cursorOffset, plain.cursorOffset, "column \(x)")
            XCTAssertEqual(styled.text, plain.text)
        }
        // Emoji by default are two cells; a symbol with text presentation is one, before and after the selector.
        let emoji = TerminalText.styledScreen("\(sgr("32"))🚀\(sgr("0"))x", cursor: (x: 2, y: 0), rows: 1)
        XCTAssertEqual(emoji.cursorOffset, 1)
        let symbol = TerminalText.styledScreen("\(sgr("1"))⏺\(sgr("0"))x", cursor: (x: 1, y: 0), rows: 1)
        XCTAssertEqual(symbol.cursorOffset, 1)
        XCTAssertEqual(cell(symbol, at: 1)?.character, "x")
        XCTAssertEqual(TerminalText.cellWidth(Character("⏺" + vs15)), 1)
    }
    func testCursorAtTheBottomOfALongStyledHistory() {
        var lines = (0..<30).map { "\(sgr("\(31 + $0 % 7)"))line \($0)\(sgr("0"))" }
        lines.append("\(sgr("1"))> \(sgr("0"))")
        let screen = TerminalText.styledScreen(lines.joined(separator: "\n") + "\n", cursor: (x: 2, y: 4), rows: 5)
        // The screen is the last five lines; the cursor is on the fifth, after "> ".
        XCTAssertEqual(screen.text.split(separator: "\n", omittingEmptySubsequences: false).last, ">  ", "the prompt and the blank cell the cursor sits on")
        XCTAssertEqual(screen.cursorOffset, screen.text.count - 1)
        XCTAssertEqual(cell(screen, at: screen.text.count - 1)?.character, " ")
    }

    // MARK: rows

    func testTrailingBlankRowsAreTrimmedUnlessTheyPaintSomething() {
        XCTAssertEqual(TerminalText.styledScreen("a\n\n   \n", cursor: nil, rows: nil).text, "a")
        // A status line made of blank cells with a background is visible and stays.
        let bar = TerminalText.styledScreen("a\n\(sgr("44"))      \(sgr("0"))\n", cursor: nil, rows: nil)
        XCTAssertEqual(bar.text, "a\n      ")
        XCTAssertEqual(cell(bar, at: 3)?.style, CellStyle(background: .indexed(4)))
        // Inverse and underlined blanks paint as well; a bold or colored-text-only blank does not.
        XCTAssertEqual(TerminalText.styledScreen("a\n\(sgr("7")) \(sgr("0"))", cursor: nil, rows: nil).text, "a\n ")
        XCTAssertEqual(TerminalText.styledScreen("a\n\(sgr("1;31")) \(sgr("0"))", cursor: nil, rows: nil).text, "a")
    }
    func testTheSameRowsAsThePlainScreenForPlainText() {
        let inputs = ["hello\nworld", "hello\nworld\n\n\n", "", "\n\n", "a\r\nb\r\n", "x\ty", "   \n  x  \n   ", "one\rtwo\nthree", "é\u{0301}a\n你 b"]
        for input in inputs {
            XCTAssertEqual(TerminalText.styledScreen(input, cursor: nil, rows: nil).text, TerminalText.readable(input), input.debugDescription)
            for cursor in [(x: 0, y: 0), (x: 3, y: 1), (x: 9, y: 2)] {
                let a = TerminalText.screen(input, cursor: cursor, rows: 3), b = TerminalText.styledScreen(input, cursor: cursor, rows: 3)
                XCTAssertEqual(b.text, a.text, "\(input.debugDescription) \(cursor)")
                XCTAssertEqual(b.cursorOffset, a.cursorOffset, "\(input.debugDescription) \(cursor)")
            }
        }
        XCTAssertTrue(TerminalText.styledScreen("plain\ntext", cursor: nil, rows: nil).isPlain)
        XCTAssertEqual(TerminalText.styledScreen("", cursor: nil, rows: nil), StyledScreen(text: "", runs: [], cursorOffset: nil))
    }
    func testAnInvalidCursorLeavesTheScreenWithoutOne() {
        for (x, y, rows) in [(-1, 0, 3), (0, -1, 3), (0, 3, 3), (0, 0, 0)] {
            XCTAssertNil(TerminalText.styledScreen("a", cursor: (x, y), rows: rows).cursorOffset)
        }
        XCTAssertNil(TerminalText.styledScreen("a", cursor: (0, 0), rows: nil).cursorOffset)
        XCTAssertNil(TerminalText.styledScreen("a", cursor: nil, rows: 3).cursorOffset)
    }
    func testRunsMergeAdjacentCellsWithTheSameStyle() {
        let screen = TerminalText.styledScreen("\(sgr("31"))ab\(sgr("31"))cd\(sgr("0;31"))ef", cursor: nil, rows: nil)
        XCTAssertEqual(screen.runs, [StyleRun(length: 6, style: CellStyle(foreground: .indexed(1)))])
    }

    // MARK: text presentation

    func testSymbolsThatCanBeEmojiGetTheTextSelector() {
        // Text by default, emoji-capable: ⏺ record, ⏸ pause, ⚠ warning, ✔ heavy check, ▶ play, ℹ info, ✂ scissors, ☑ ballot box with check.
        for symbol in ["\u{23FA}", "\u{23F8}", "\u{26A0}", "\u{2714}", "\u{25B6}", "\u{2139}", "\u{2702}", "\u{2611}"] {
            let expected = Character(symbol + vs15)
            XCTAssertEqual(TerminalText.textPresentation(Character(symbol)), expected, "U+\(String(symbol.unicodeScalars.first!.value, radix: 16))")
            XCTAssertEqual(String(TerminalText.textPresentation(Character(symbol))).unicodeScalars.count, 2)
        }
        XCTAssertEqual(TerminalText.textPresentation("⏺ done ⚠ careful ✔ ok"), "⏺\(vs15) done ⚠\(vs15) careful ✔\(vs15) ok")
    }
    func testSymbolsThatAreNotEmojiAndPlainTextAreLeftAlone() {
        for symbol in ["⎿", "✻", "☐", "✓", "★", "●", "◆", "│", "─", "→", "❯", "…", "é", "你", "a", "Z", " ", "{", "~"] {
            XCTAssertEqual(TerminalText.textPresentation(Character(symbol)), Character(symbol), symbol)
        }
        // Digits, # and * are formally emoji; they are text everywhere in a terminal and must stay untouched.
        for ascii in ["0", "1", "9", "#", "*"] { XCTAssertEqual(TerminalText.textPresentation(Character(ascii)), Character(ascii), ascii) }
        // Beyond ASCII the rule is the same for every symbol that can be an emoji: © is one, and Menlo draws it as text either way.
        XCTAssertEqual(TerminalText.textPresentation(Character("©")), Character("©" + vs15))
        let line = "$ ls -la | grep '#1' *.txt"
        XCTAssertEqual(TerminalText.textPresentation(line), line)
        XCTAssertEqual(TerminalText.textPresentation(""), "")
    }
    func testGenuineEmojiKeepTheirColor() {
        let keep: [(String, String)] = [
            ("⚠" + vs16, "warning with the emoji selector"), ("❤" + vs16, "heart with the emoji selector"), ("✔" + vs16, "check with the emoji selector"),
            ("⚠" + vs15, "already text"),
            ("1" + vs16 + "\u{20E3}", "keycap 1"), ("#" + vs16 + "\u{20E3}", "keycap #"), ("*" + vs16 + "\u{20E3}", "keycap *"), ("#\u{20E3}", "keycap without selector"),
            ("👨‍👩‍👧", "family, a ZWJ sequence"), ("🏳️‍🌈", "rainbow flag"), ("❤️‍🔥", "heart on fire, a ZWJ sequence starting with a text-default heart"),
            ("👍🏽", "thumbs up with a skin tone"), ("☝🏽", "index up with a skin tone (text-default base)"), ("🇩🇪", "flag"),
            ("🏴󠁧󠁢󠁥󠁮󠁧󠁿", "tag sequence flag"),
            ("✅", "emoji by default"), ("🚀", "emoji by default"), ("⭐", "emoji by default"), ("😀", "emoji by default"), ("❌", "emoji by default"), ("⏰", "emoji by default")
        ]
        for (text, why) in keep {
            let character = text.first!
            XCTAssertEqual(text.count, 1, "\(why) is one character")
            XCTAssertEqual(String(TerminalText.textPresentation(character)), text, why)
        }
        let mixed = "ok ✅ done 🚀 ⚠️ 1️⃣"
        XCTAssertEqual(TerminalText.textPresentation(mixed), mixed)
    }
    func testTheTransformationChangesNeitherTheCharacterCountNorTheWidthAndIsIdempotent() {
        let text = "⏺ Reading ⎿ ⚠ 3 files ✔ ✅ 🚀 ▶ ⏸ ℹ ☑ 你好"
        let once = TerminalText.textPresentation(text)
        XCTAssertEqual(once.count, text.count, "a selector joins the character before it")
        XCTAssertNotEqual(once, text)
        XCTAssertEqual(TerminalText.textPresentation(once), once)
        for (a, b) in zip(text, once) { XCTAssertEqual(TerminalText.cellWidth(a), TerminalText.cellWidth(b)) }
    }
    func testScreensCarryTheSelectorWithoutMovingTheCursor() {
        let input = "\(sgr("32"))⏺\(sgr("0")) Done ⚠ x"
        let screen = TerminalText.styledScreen(input, cursor: (x: 10, y: 0), rows: 1)
        XCTAssertEqual(screen.text, "⏺\(vs15) Done ⚠\(vs15) x ")
        XCTAssertEqual(screen.cursorOffset, 10)
        XCTAssertEqual(cell(screen, at: 10)?.character, " ")
        XCTAssertEqual(screen.text.count, 11, "the selectors add no characters")
        XCTAssertEqual(TerminalText.styledScreen(input, cursor: (x: 9, y: 0), rows: 1).cursorOffset, 9, "column 9 is the x, whatever came before it")
        let raw = TerminalText.styledScreen(input, cursor: (x: 10, y: 0), rows: 1, textPresentation: false)
        XCTAssertEqual(raw.text, "⏺ Done ⚠ x ")
        XCTAssertEqual(raw.cursorOffset, screen.cursorOffset)
        XCTAssertEqual(raw.runs, screen.runs)
    }

    // MARK: colors

    private func colors(boldIsBright: Bool = false) -> TerminalRenderColors {
        TerminalRenderColors(foreground: RGB(0xdddddd), background: RGB(0x101010), ansi: (0..<16).map { RGB(UInt32($0 + 1) * 0x101010 & 0xffffff) }, boldIsBright: boldIsBright)
    }
    func testThePaletteSuppliesTheSixteenColorsAndXtermTheRest() {
        let c = colors()
        for index in 0..<16 { XCTAssertEqual(c.palette(UInt8(index)), c.ansi[index]) }
        XCTAssertEqual(c.palette(16), RGB(0x000000))
        XCTAssertEqual(c.palette(21), RGB(0x0000ff))
        XCTAssertEqual(c.palette(46), RGB(0x00ff00))
        XCTAssertEqual(c.palette(196), RGB(0xff0000))
        XCTAssertEqual(c.palette(226), RGB(0xffff00))
        XCTAssertEqual(c.palette(231), RGB(0xffffff))
        XCTAssertEqual(c.palette(59), RGB(0x5f5f5f))
        XCTAssertEqual(c.palette(102), RGB(0x878787))
        XCTAssertEqual(c.palette(232), RGB(0x080808))
        XCTAssertEqual(c.palette(244), RGB(0x808080))
        XCTAssertEqual(c.palette(255), RGB(0xeeeeee))
    }
    func testDefaultsPaletteAndTrueColorResolve() {
        let c = colors()
        XCTAssertEqual(c.resolve(.plain).foreground, RGB(0xdddddd))
        XCTAssertNil(c.resolve(.plain).background, "the default background is not painted")
        XCTAssertEqual(c.resolve(CellStyle(foreground: .indexed(2), background: .indexed(9))).foreground, c.ansi[2])
        XCTAssertEqual(c.resolve(CellStyle(foreground: .indexed(2), background: .indexed(9))).background, c.ansi[9])
        XCTAssertEqual(c.resolve(CellStyle(foreground: .rgb(RGB(1, 2, 3)))).foreground, RGB(1, 2, 3))
        XCTAssertEqual(c.resolve(CellStyle(foreground: .indexed(200))).foreground, c.palette(200))
    }
    func testBoldIsBrightOnlyWhenAskedAndOnlyForTheEightBaseColors() {
        let bold = CellStyle(foreground: .indexed(1), attributes: .bold)
        XCTAssertEqual(colors().resolve(bold).foreground, colors().ansi[1], "off by default, like Ghostty")
        XCTAssertEqual(colors(boldIsBright: true).resolve(bold).foreground, colors().ansi[9])
        XCTAssertEqual(colors(boldIsBright: true).resolve(CellStyle(foreground: .indexed(9), attributes: .bold)).foreground, colors().ansi[9])
        XCTAssertEqual(colors(boldIsBright: true).resolve(CellStyle(foreground: .indexed(100), attributes: .bold)).foreground, colors().palette(100))
        XCTAssertEqual(colors(boldIsBright: true).resolve(CellStyle(foreground: .rgb(RGB(9, 9, 9)), attributes: .bold)).foreground, RGB(9, 9, 9))
        XCTAssertEqual(colors(boldIsBright: true).resolve(CellStyle(foreground: .default, attributes: .bold)).foreground, RGB(0xdddddd))
        XCTAssertEqual(colors(boldIsBright: true).resolve(CellStyle(background: .indexed(1), attributes: .bold)).background, colors().ansi[1], "backgrounds are never brightened")
    }
    func testInverseDimAndHidden() {
        let c = colors()
        let inverse = c.resolve(CellStyle(attributes: .inverse))
        XCTAssertEqual(inverse.foreground, RGB(0x101010))
        XCTAssertEqual(inverse.background, RGB(0xdddddd))
        let both = c.resolve(CellStyle(foreground: .indexed(1), background: .indexed(2), attributes: .inverse))
        XCTAssertEqual(both.foreground, c.ansi[2])
        XCTAssertEqual(both.background, c.ansi[1])
        let dim = c.resolve(CellStyle(attributes: .dim))
        XCTAssertEqual(dim.foreground, RGB(0xdddddd).blended(over: RGB(0x101010), opacity: 0.5))
        XCTAssertEqual(dim.foreground, RGB(0x777777))
        let hidden = c.resolve(CellStyle(foreground: .indexed(3), background: .indexed(4), attributes: .hidden))
        XCTAssertEqual(hidden.foreground, c.ansi[4], "hidden text takes the color behind it")
    }
    func testASyncedPaletteReachesTheRenderColors() throws {
        let terminal = TerminalColors(background: RGB(0x1e1e2e), foreground: RGB(0xcdd6f4), palette: (0..<16).map { RGB(UInt32(0x100000 + $0)) })
        let appearance = DesktopAppearance(updatedAt: 1, dark: true, palette: DesktopPalette(
            bg: RGB(0x090d14), panel: RGB(0x101720), panelActive: RGB(0x14212a), divider: RGB(0x253c45), cyan: RGB(0x55e6dc), magenta: RGB(0xce78ef), gold: RGB(0xf4bf75), text: RGB(0xd3e1e6), muted: RGB(0x8fa6ae)),
            terminal: terminal)
        let synced = DesktopTheme.resolve(appearance).terminalColors(dark: false)
        XCTAssertEqual(synced.ansi, terminal.palette, "every one of the 16 colors arrives as published, on either side")
        XCTAssertEqual(synced.background, terminal.background)
        XCTAssertEqual(synced.foreground, terminal.foreground)
        XCTAssertEqual(DesktopTheme.resolve(appearance).terminalColors(dark: true).ansi, terminal.palette)
        // Without a published palette the built-in colors of the desktop's side are used, and the built-in theme follows the phone's.
        let withoutTerminal = DesktopAppearance(updatedAt: 1, dark: false, palette: appearance.palette, terminal: nil)
        XCTAssertEqual(DesktopTheme.resolve(withoutTerminal).terminalColors(dark: false).ansi, TerminalRenderColors.fallbackAnsi(dark: false))
        XCTAssertEqual(DesktopTheme.builtIn.terminalColors(dark: true).ansi, TerminalRenderColors.fallbackAnsi(dark: true))
        XCTAssertEqual(DesktopTheme.builtIn.terminalColors(dark: false).ansi, TerminalRenderColors.fallbackAnsi(dark: false))
        XCTAssertEqual(DesktopTheme.builtIn.ansi.count, 16)
    }
}

private extension RGB {
    init(_ r: UInt8, _ g: UInt8, _ b: UInt8) { self.init(red: r, green: g, blue: b) }
}
