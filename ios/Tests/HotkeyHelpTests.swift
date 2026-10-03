import XCTest
@testable import RiWorkCore

final class HotkeyHelpTests: XCTestCase {
    private func chord(_ letter: String, _ modifiers: ChordModifiers = .command) -> KeyChord { KeyChord(keyCode: HIDKey.code(forCharacter: letter)!, modifiers: modifiers) }
    private func hotkey(_ label: String, _ steps: [KeyItem] = [.text("x")], id: String? = nil, chord: KeyChord? = nil) -> Hotkey {
        Hotkey(id: id ?? label.lowercased(), label: label, steps: steps, chord: chord)
    }

    // MARK: What it lists

    func testTheHotkeysAreInTwoGroupsThoseWithAShortcutFirstEachInTheOrderOfTheKeyBar() {
        let help = HotkeyHelp(hotkeys: [
            hotkey("Plain", [.text("a")]), hotkey("Clear", [.text("/clear"), .key(.enter)], chord: chord("l")),
            hotkey("Compact", [.text("/compact")]), hotkey("Review", [.text("/review")], chord: chord("r", [.command, .shift]))
        ])
        XCTAssertEqual(help.withShortcut.map(\.label), ["Clear", "Review"])
        XCTAssertEqual(Array(help.withoutShortcut.map(\.label).prefix(2)), ["Plain", "Compact"], "theirs first, in their order")
        XCTAssertEqual(help.rows.map(\.label), help.withShortcut.map(\.label) + help.withoutShortcut.map(\.label))
        XCTAssertEqual(Set(help.rows.map(\.id)).count, help.rows.count, "ids are unique")
    }
    func testEachRowShowsItsShortcutInGlyphsItsLabelAndWhatItSends() throws {
        let help = HotkeyHelp(hotkeys: [
            hotkey("Clear", [.text("/clear"), .key(.enter)], chord: chord("l", [.command, .shift])), hotkey("Plain", [.key(.escape)])
        ])
        let clear = try XCTUnwrap(help.rows.first { $0.label == "Clear" })
        XCTAssertEqual(clear.shortcut, "⇧⌘L", "the same glyphs as everywhere else, in the same order")
        XCTAssertEqual(clear.sends, "/clear ⏎")
        let plain = try XCTUnwrap(help.rows.first { $0.label == "Plain" })
        XCTAssertNil(plain.shortcut, "the view shows a dash")
        XCTAssertEqual(plain.sends, "⎋")
        XCTAssertEqual(clear.hotkey.items, [.text("/clear"), .key(.enter)], "a tap on the row sends exactly this")
        XCTAssertEqual(HotkeyHelp(hotkeys: [hotkey("Tap", chord: KeyChord(keyCode: HIDKey.leftControl))]).withShortcut.first?.shortcut, "Tap Left ⌃")
    }
    func testTheBuiltInHotkeysAreThereUnlessOneOfTheirsSendsTheSame() {
        let bare = HotkeyHelp(hotkeys: [])
        XCTAssertEqual(bare.withShortcut, [])
        XCTAssertEqual(bare.withoutShortcut.map(\.label), Hotkey.builtIn.map(\.label), "every built-in one")
        let theirs = HotkeyHelp(hotkeys: [hotkey("Interrupt", [.key(.control("c"))], chord: chord("c", [.command, .shift]))])
        XCTAssertEqual(theirs.rows.filter { $0.sends == "^C" }.map(\.label), ["Interrupt"], "theirs, with its shortcut, stands in for ^C")
        XCTAssertEqual(theirs.withoutShortcut.count, Hotkey.builtIn.count - 1)
    }
    func testTheClicksTemplateListsEveryShortcutItAddsAndTheBuiltInOnesItDoesNotCover() {
        var library = HotkeyLibrary()
        library.merge(.clicks)
        let help = HotkeyHelp(hotkeys: library.hotkeys)
        XCTAssertEqual(help.withShortcut.map(\.label), HotkeyTemplate.clicks.hotkeys.map(\.label))
        XCTAssertEqual(Array(help.withShortcut.compactMap(\.shortcut).prefix(3)), ["⌘E", "⌘T", "⇧⌘T"], "Esc, Tab, Shift-Tab")
        XCTAssertEqual(help.withShortcut.first { $0.label == "^C" }?.shortcut, "⇧⌘C")
        XCTAssertEqual(help.withoutShortcut.map(\.label), ["^A", "^E", "^U", "^W", "Esc Esc"], "^C ^D ^Z ^L ^R are the template's already")
        XCTAssertEqual(help.rows.count, 14 + 5)
    }

    // MARK: The app's own shortcuts

    func testTheAppShortcutsAreListedWithTheKeysThatDoThem() {
        let plain = HotkeyHelp(hotkeys: [])
        XCTAssertEqual(plain.appShortcuts.map(\.title), ["Hotkey menu", "Hotkey settings", "New terminal", "This help"])
        XCTAssertEqual(plain.appShortcuts.map(\.keys), ["⌘K", "⌘,", "⌘N", "⌘/"])
        XCTAssertEqual(Set(plain.appShortcuts.map(\.id)).count, 4)
    }
    func testTheExtraShortcutsThePersonAddedAreListedWithTheFixedOnes() {
        let settings = ShortcutSettings(paletteChords: [KeyChord(keyCode: HIDKey.leftControl), KeyChord(keyCode: HIDKey.rightControl)],
                                        helpChords: [chord("j")])
        let help = HotkeyHelp(hotkeys: [], shortcuts: settings)
        XCTAssertEqual(help.appShortcuts.first { $0.title == "Hotkey menu" }?.keys, "⌘K · Tap Left ⌃ · Tap Right ⌃")
        XCTAssertEqual(help.appShortcuts.first { $0.title == "This help" }?.keys, "⌘/ · ⌘J")
        XCTAssertEqual(help.appShortcuts.first { $0.title == "Hotkey settings" }?.keys, "⌘,", "no extra ones for that")
    }

    // MARK: A hotkey can never take what the help is on

    func testNoHotkeyCanBeGivenCommandSlashAndTheTemplateDoesNotUseIt() {
        let taken = Hotkey(label: "Mine", steps: [.text("x")], chord: .helpDefault)
        XCTAssertThrowsError(try taken.validate())
        var library = HotkeyLibrary()
        XCTAssertThrowsError(try library.add(taken))
        XCTAssertFalse(HotkeyTemplate.clicks.hotkeys.contains { $0.chord == .helpDefault })
        XCTAssertTrue(HotkeyTemplate.clicks.hotkeys.allSatisfy { $0.chord != nil && (try? $0.chord?.validate()) != nil }, "and every one of its shortcuts is usable")
    }
}
