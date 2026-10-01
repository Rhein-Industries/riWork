import XCTest
@testable import RiWorkCore

final class HotkeyPaletteTests: XCTestCase {
    private func hotkey(_ label: String, _ steps: [KeyItem] = [.text("x")], id: String? = nil, chord: KeyChord? = nil) -> Hotkey {
        Hotkey(id: id ?? label.lowercased(), label: label, steps: steps, chord: chord)
    }
    private func type(_ palette: inout HotkeyPalette, _ text: String) { for character in text { palette.apply(.text(String(character))) } }

    // MARK: Rows

    func testTheRowsAreTheirsThenBuiltInThenPlainKeysThenTheActions() {
        let palette = HotkeyPalette(hotkeys: [hotkey("Clear", [.text("/clear"), .key(.enter)]), hotkey("Bail", [.key(.control("c")), .text("exit")])])
        let titles = palette.entries.map(\.title)
        XCTAssertEqual(Array(titles.prefix(2)), ["Clear", "Bail"])
        XCTAssertEqual(titles.last, "Configure hotkeys…")
        XCTAssertEqual(titles[titles.count - 2], "New hotkey…")
        XCTAssertTrue(titles.contains("^C"))
        XCTAssertTrue(titles.contains("Esc") && titles.contains("Page Up") && titles.contains("Shift-Tab"), "keys a keyboard may lack")
        XCTAssertEqual(Set(palette.entries.map(\.id)).count, palette.entries.count, "ids are unique")
        XCTAssertEqual(palette.results, palette.entries)
        XCTAssertEqual(palette.selection, 0)
    }
    func testAHotkeyOfTheirsThatSendsTheSameKeyReplacesTheBuiltInOrPlainOne() {
        let esc = hotkey("Esc", [.key(.escape)], chord: KeyChord(keyCode: HIDKey.e, modifiers: .command))
        let ctrlC = hotkey("^C", [.key(.control("c"))], id: "t-c", chord: KeyChord(keyCode: HIDKey.e, modifiers: [.command, .shift]))
        let palette = HotkeyPalette(hotkeys: [esc, ctrlC])
        XCTAssertEqual(palette.entries.filter { $0.title == "Esc" }.count, 1, "not twice")
        XCTAssertEqual(palette.entries.filter { $0.title == "^C" }.count, 1)
        XCTAssertEqual(palette.entries.first { $0.title == "Esc" }?.shortcut, "⌘E", "theirs, with its shortcut")
        XCTAssertEqual(palette.entries.first { $0.title == "^C" }?.shortcut, "⇧⌘E")
        XCTAssertNotNil(palette.entries.first { $0.title == "Tab" }, "the rest are still there")
    }

    // MARK: Filter

    func testTypingFiltersAndTheBestMatchComesFirst() {
        var palette = HotkeyPalette(hotkeys: [hotkey("Compact", [.text("/compact")]), hotkey("Clear", [.text("/clear"), .key(.enter)]), hotkey("Review", [.text("/review")])])
        type(&palette, "cl")
        XCTAssertEqual(palette.results.first?.title, "Clear", "a prefix of the name beats a match elsewhere")
        XCTAssertFalse(palette.results.map(\.title).contains("Review"))
        XCTAssertEqual(palette.query, "cl")
        palette.apply(.backspace)
        XCTAssertEqual(palette.query, "c")
        palette.apply(.clearQuery)
        XCTAssertEqual(palette.query, "")
        XCTAssertEqual(palette.results, palette.entries)
    }
    func testTheFilterIsCaseBlindMatchesWholeWordsInAnyOrderAndWhatTheKeySends() {
        var palette = HotkeyPalette(hotkeys: [])
        type(&palette, "CTRL c")
        XCTAssertEqual(palette.results.first?.title, "^C", "Ctrl+C is found by its spoken name")
        palette.apply(.clearQuery); type(&palette, "page")
        XCTAssertEqual(Set(palette.results.map(\.title)).intersection(["Page Up", "Page Down"]).count, 2)
        palette.apply(.clearQuery); type(&palette, "escape")
        XCTAssertEqual(palette.results.first?.title, "Esc", "by the contract's name for the key")
        palette.apply(.clearQuery); type(&palette, "zzzz")
        XCTAssertEqual(palette.results, [])
        XCTAssertNil(palette.selected)
    }
    func testAShortcutAndTheSentTextAreSearchable() {
        var palette = HotkeyPalette(hotkeys: [hotkey("Go", [.text("git status")], chord: KeyChord(keyCode: HIDKey.code(forCharacter: "g")!, modifiers: .command))])
        type(&palette, "status")
        XCTAssertEqual(palette.results.first?.title, "Go")
        palette.apply(.clearQuery); type(&palette, "⌘g")
        XCTAssertEqual(palette.results.first?.title, "Go")
    }
    func testTheConfigurationIsReachableByTypingForIt() {
        var palette = HotkeyPalette(hotkeys: [])
        type(&palette, "clicks")
        XCTAssertEqual(palette.results.first?.title, "Configure hotkeys…", "the Clicks template lives there")
        XCTAssertEqual(palette.apply(.activate), .configure)
        palette.apply(.clearQuery); type(&palette, "new")
        XCTAssertEqual(palette.apply(.activate), .newHotkey)
    }
    func testControlCharactersAndLeadingSpacesNeverEnterTheQuery() {
        var palette = HotkeyPalette(hotkeys: [])
        XCTAssertEqual(palette.apply(.text("\u{1b}\u{7}")), .none)
        palette.apply(.text("   ")); XCTAssertEqual(palette.query, "")
        palette.apply(.text("a\nb")); XCTAssertEqual(palette.query, "ab")
        palette.apply(.text(String(repeating: "x", count: 100)))
        XCTAssertEqual(palette.query.count, HotkeyPalette.maxQueryLength)
        palette.apply(.backspace); palette.apply(.clearQuery); palette.apply(.backspace)
        XCTAssertEqual(palette.query, "")
    }

    // MARK: Moving and choosing

    func testUpAndDownMoveAndWrapAndTheQueryResetsTheSelection() {
        var palette = HotkeyPalette(hotkeys: [hotkey("One"), hotkey("Two"), hotkey("Three")])
        XCTAssertEqual(palette.selected?.title, "One")
        palette.apply(.down); XCTAssertEqual(palette.selected?.title, "Two")
        palette.apply(.up); palette.apply(.up)
        XCTAssertEqual(palette.selected?.title, palette.entries.last?.title, "up from the top wraps to the end")
        palette.apply(.down)
        XCTAssertEqual(palette.selected?.title, "One", "and down from the end to the top")
        palette.apply(.down); palette.apply(.down)
        type(&palette, "t")
        XCTAssertEqual(palette.selection, 0)
    }
    func testPageKeysJumpAndStopAtTheEnds() {
        var palette = HotkeyPalette(hotkeys: (0..<12).map { hotkey("H\($0)") })
        palette.apply(.pageDown)
        XCTAssertEqual(palette.selection, HotkeyPalette.pageSize)
        palette.apply(.pageDown); palette.apply(.pageDown)
        XCTAssertEqual(palette.selection, 3 * HotkeyPalette.pageSize)
        for _ in 0..<20 { palette.apply(.pageDown) }
        XCTAssertEqual(palette.selection, palette.results.count - 1, "no wrapping")
        for _ in 0..<20 { palette.apply(.pageUp) }
        XCTAssertEqual(palette.selection, 0)
    }
    func testNothingMovesOrIsChosenWhenNothingMatches() {
        var palette = HotkeyPalette(hotkeys: [])
        type(&palette, "qqqqq")
        palette.apply(.down); palette.apply(.up)
        XCTAssertEqual(palette.selection, 0)
        XCTAssertEqual(palette.apply(.activate), .none)
        XCTAssertEqual(palette.apply(.editSelected), .none)
    }
    func testReturnSendsTheSelectedHotkeyOrKey() {
        let clear = hotkey("Clear", [.text("/clear"), .key(.enter)])
        var palette = HotkeyPalette(hotkeys: [clear])
        XCTAssertEqual(palette.apply(.activate), .hotkey(clear))
        palette.apply(.clearQuery); type(&palette, "page up")
        XCTAssertEqual(palette.apply(.activate), .key(.pageUp))
        palette.apply(.clearQuery); type(&palette, "^c")
        if case .hotkey(let found) = palette.apply(.activate) { XCTAssertEqual(found.items, [.key(.control("c"))]) } else { XCTFail("the built-in ^C") }
    }
    func testATapOnARowChoosesIt() {
        let clear = hotkey("Clear"), bail = hotkey("Bail")
        var palette = HotkeyPalette(hotkeys: [clear, bail])
        XCTAssertEqual(palette.choose(index: 1), .hotkey(bail))
        XCTAssertEqual(palette.selection, 1)
        XCTAssertEqual(palette.choose(index: 999), .none)
        XCTAssertEqual(palette.choose(index: -1), .none)
    }
    func testEditingIsOnlyForTheirOwnHotkeys() {
        let clear = hotkey("Clear")
        var palette = HotkeyPalette(hotkeys: [clear])
        XCTAssertEqual(palette.apply(.editSelected), .edit(clear))
        palette.apply(.clearQuery); type(&palette, "^c")
        XCTAssertEqual(palette.apply(.editSelected), .none, "built-ins cannot be edited")
        palette.apply(.clearQuery); type(&palette, "tab")
        XCTAssertEqual(palette.apply(.editSelected), .none, "nor can plain keys")
        XCTAssertEqual(palette.apply(.newHotkey), .newHotkey)
    }
}
