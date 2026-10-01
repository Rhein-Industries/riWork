import XCTest
@testable import RiWorkCore

final class HotkeyTemplateTests: XCTestCase {
    private func hotkey(_ label: String, _ steps: [KeyItem] = [.text("x")], id: String? = nil, chord: KeyChord? = nil) -> Hotkey {
        Hotkey(id: id ?? label.lowercased(), label: label, steps: steps, chord: chord)
    }
    private func chord(_ letter: String, _ modifiers: ChordModifiers = .command) -> KeyChord { KeyChord(keyCode: HIDKey.code(forCharacter: letter)!, modifiers: modifiers) }

    // MARK: The Clicks template

    func testEveryHotkeyOfTheClicksTemplateIsValidAndUnique() throws {
        let template = HotkeyTemplate.clicks
        XCTAssertEqual(template.id, "clicks")
        XCTAssertEqual(template.name, "Clicks keyboard")
        for hotkey in template.hotkeys {
            XCTAssertNoThrow(try hotkey.validate(), hotkey.label)
            XCTAssertNoThrow(try KeyItem.validate(batch: hotkey.items), hotkey.label)
            XCTAssertFalse(hotkey.isBuiltIn, "so they can be edited and deleted")
            XCTAssertNotNil(hotkey.chord, "\(hotkey.label) has a shortcut")
            XCTAssertFalse(hotkey.showsOnBar, "the key bar has these keys; the shortcuts stay out of its way")
            XCTAssertTrue(hotkey.id.hasPrefix("template.clicks."))
        }
        XCTAssertEqual(Set(template.hotkeys.map(\.id)).count, template.hotkeys.count)
        XCTAssertEqual(Set(template.hotkeys.compactMap(\.chord)).count, template.hotkeys.count, "no two share a shortcut")
        XCTAssertLessThan(template.hotkeys.count, HotkeyLibrary.maxHotkeys / 2, "room is left for the person's own")
    }
    func testTheTemplateCoversWhatATerminalAndCodingAgentsNeedThatTheClicksLacks() {
        let sent = Set(HotkeyTemplate.clicks.hotkeys.flatMap(\.items))
        for key in [TerminalKey.escape, .tab, .backTab, .up, .down, .left, .right, .pageUp, .pageDown,
                    .control("c"), .control("d"), .control("z"), .control("r"), .control("l")] {
            XCTAssertTrue(sent.contains(.key(key)), key.title)
        }
    }
    func testTheShortcutsFollowTheRuleCommandPlusLetterIsAKeyAndCommandShiftPlusLetterIsCtrl() {
        let byKey = Dictionary(uniqueKeysWithValues: HotkeyTemplate.clicks.hotkeys.map { ($0.items, $0.chord!) })
        XCTAssertEqual(byKey[[.key(.escape)]], chord("e"))
        XCTAssertEqual(byKey[[.key(.tab)]], chord("t"))
        XCTAssertEqual(byKey[[.key(.backTab)]], chord("t", [.command, .shift]))
        XCTAssertEqual(byKey[[.key(.up)]], chord("w")); XCTAssertEqual(byKey[[.key(.left)]], chord("a"))
        XCTAssertEqual(byKey[[.key(.down)]], chord("s")); XCTAssertEqual(byKey[[.key(.right)]], chord("d"))
        XCTAssertEqual(byKey[[.key(.pageUp)]], chord("b")); XCTAssertEqual(byKey[[.key(.pageDown)]], chord("f"))
        for letter in ["c", "d", "z", "r", "l"] {
            XCTAssertEqual(byKey[[.key(.control(Character(letter)))]], chord(letter, [.command, .shift]), "⌘⇧\(letter.uppercased())")
        }
    }
    func testNoShortcutTakesATypingKeyOrOneThatIOSKeepsForItself() {
        let reserved: Set<KeyChord> = [.paletteDefault, .settingsDefault, chord("h"), KeyChord(keyCode: HIDKey.space, modifiers: .command), KeyChord(keyCode: HIDKey.tab, modifiers: .command)]
        for hotkey in HotkeyTemplate.clicks.hotkeys {
            let chord = hotkey.chord!
            XCTAssertTrue(chord.modifiers.contains(.command), "\(hotkey.label) sits on ⌘")
            XCTAssertFalse(reserved.contains(chord), hotkey.label)
        }
    }
    func testTheMenuOpensOnATapOfControlToo() {
        XCTAssertEqual(HotkeyTemplate.clicks.paletteChords, [KeyChord(keyCode: HIDKey.leftControl), KeyChord(keyCode: HIDKey.rightControl)])
        XCTAssertTrue(HotkeyTemplate.clicks.paletteChords.allSatisfy(\.isTap))
        XCTAssertEqual(HotkeyTemplate.all.map(\.id), ["clicks"])
    }

    // MARK: Merging

    func testInstallingIntoAnEmptyLibraryAddsEverythingInOrder() {
        var library = HotkeyLibrary()
        let result = library.merge(.clicks)
        XCTAssertEqual(result.added, HotkeyTemplate.clicks.hotkeys)
        XCTAssertEqual(result.skipped, [])
        XCTAssertEqual(library.hotkeys, HotkeyTemplate.clicks.hotkeys)
        XCTAssertTrue(result.changedAnything)
        XCTAssertEqual(result.summary, "Added 14 hotkeys.")
    }
    func testInstallingKeepsEveryHotkeyThePersonHasInItsPlaceAndAppends() throws {
        var library = HotkeyLibrary()
        let mine = [hotkey("Clear", [.text("/clear"), .key(.enter)]), hotkey("Bail", [.key(.control("c")), .text("exit"), .key(.enter)], chord: chord("x", .control))]
        for hotkey in mine { try library.add(hotkey) }
        let result = library.merge(.clicks)
        XCTAssertEqual(Array(library.hotkeys.prefix(2)), mine, "untouched, same order, same shortcuts")
        XCTAssertEqual(Array(library.hotkeys.dropFirst(2)), HotkeyTemplate.clicks.hotkeys)
        XCTAssertEqual(result.skipped, [])
    }
    func testInstallingTwiceChangesNothing() {
        var library = HotkeyLibrary()
        library.merge(.clicks)
        let before = library
        let again = library.merge(.clicks)
        XCTAssertEqual(library, before)
        XCTAssertEqual(again.added, [])
        XCTAssertEqual(again.skipped.count, HotkeyTemplate.clicks.hotkeys.count)
        XCTAssertTrue(again.skipped.allSatisfy { $0.reason == .alreadyThere })
        XCTAssertFalse(again.changedAnything)
        XCTAssertEqual(again.summary, "Nothing new: all 14 are already there or taken.")
    }
    func testACopyThePersonEditedIsKeptAsEditedAndADeletedOneComesBack() throws {
        var library = HotkeyLibrary()
        library.merge(.clicks)
        var edited = library.hotkeys[0]
        edited.steps = [.key(.escape), .key(.escape)]
        try library.update(edited)
        library.remove(id: library.hotkeys[1].id)
        let before = library
        library.merge(.clicks)
        XCTAssertEqual(library.hotkeys.first, edited, "their edit stays")
        XCTAssertEqual(library.hotkeys.count, before.hotkeys.count + 1, "the deleted one comes back, since it is simply missing")
        XCTAssertEqual(library.hotkeys.filter { $0.id == edited.id }.count, 1)
    }
    func testASameStepsSameShortcutHotkeyUnderAnotherIdIsADuplicate() throws {
        var library = HotkeyLibrary()
        let template = HotkeyTemplate.clicks
        let esc = template.hotkeys[0]
        try library.add(Hotkey(id: "mine", label: "Escape", steps: esc.steps, chord: esc.chord))
        let result = library.merge(template)
        XCTAssertEqual(result.skipped.map(\.hotkey.id), [esc.id])
        XCTAssertEqual(result.skipped.first?.reason, .alreadyThere)
        XCTAssertEqual(library.hotkeys.count, 1 + template.hotkeys.count - 1)
    }
    func testAShortcutThatIsAlreadyTheirsIsLeftTheirsAndTheTemplateHotkeyIsSkipped() throws {
        var library = HotkeyLibrary()
        try library.add(Hotkey(id: "mine", label: "Mine", steps: [.text("hello")], chord: chord("e")))
        let result = library.merge(.clicks)
        XCTAssertEqual(result.skipped.count, 1)
        XCTAssertEqual(result.skipped.first?.hotkey.id, "template.clicks.esc")
        XCTAssertEqual(result.skipped.first?.reason, .shortcutTaken(by: "Mine"))
        XCTAssertEqual(library.hotkey(for: chord("e"))?.id, "mine")
        XCTAssertEqual(result.added.count, HotkeyTemplate.clicks.hotkeys.count - 1)
        XCTAssertEqual(result.summary, "Added 13 hotkeys, skipped 1 you already have.")
    }
    func testAHotkeyOfTheirsWithTheSameStepsButNoShortcutDoesNotBlockTheShortcutVersion() throws {
        var library = HotkeyLibrary()
        try library.add(Hotkey(id: "mine", label: "Esc", steps: [.key(.escape)]))
        let result = library.merge(.clicks)
        XCTAssertEqual(result.added.count, HotkeyTemplate.clicks.hotkeys.count, "the template's Esc is the one with the shortcut")
        XCTAssertNil(library.hotkeys.first?.chord, "and theirs is unchanged")
    }
    func testAFullLibraryTakesWhatFitsAndSaysWhatDidNot() throws {
        var library = HotkeyLibrary()
        for index in 0..<(HotkeyLibrary.maxHotkeys - 5) { try library.add(hotkey("H\(index)", id: "h\(index)")) }
        let result = library.merge(.clicks)
        XCTAssertEqual(result.added.count, 5)
        XCTAssertEqual(result.skipped.count, HotkeyTemplate.clicks.hotkeys.count - 5)
        XCTAssertTrue(result.skipped.allSatisfy { $0.reason == .libraryFull })
        XCTAssertEqual(library.hotkeys.count, HotkeyLibrary.maxHotkeys)
        XCTAssertEqual(Array(library.hotkeys.prefix(HotkeyLibrary.maxHotkeys - 5)).map(\.id), (0..<(HotkeyLibrary.maxHotkeys - 5)).map { "h\($0)" })
    }
    func testAMergedLibrarySurvivesStorage() {
        var library = HotkeyLibrary()
        library.merge(.clicks)
        XCTAssertEqual(HotkeyLibrary(encoded: library.encoded), library, "shortcuts and the hidden-from-the-bar flag come back")
    }
    func testTheMenuShortcutsAreMergedToo() throws {
        var settings = ShortcutSettings()
        let library = HotkeyLibrary()
        XCTAssertEqual(settings.merge(.clicks, library: library), HotkeyTemplate.clicks.paletteChords)
        XCTAssertEqual(settings.merge(.clicks, library: library), [], "once")
        var other = ShortcutSettings(paletteChords: [KeyChord(keyCode: HIDKey.leftControl)])
        XCTAssertEqual(other.merge(.clicks, library: library), [KeyChord(keyCode: HIDKey.rightControl)], "only what is new")
        XCTAssertEqual(other.paletteChords.count, 2)
    }
}
