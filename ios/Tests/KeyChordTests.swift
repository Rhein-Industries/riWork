import XCTest
@testable import RiWorkCore

final class KeyChordTests: XCTestCase {
    // MARK: Names

    func testHIDUsagesAreNamedTheWayAKeyboardLabelsThem() {
        XCTAssertEqual(HIDKey.name(of: 0x04), "A")
        XCTAssertEqual(HIDKey.name(of: 0x1D), "Z")
        XCTAssertEqual(HIDKey.name(of: 0x1E), "1")
        XCTAssertEqual(HIDKey.name(of: 0x26), "9")
        XCTAssertEqual(HIDKey.name(of: 0x27), "0")
        XCTAssertEqual(HIDKey.name(of: HIDKey.escape), "Esc")
        XCTAssertEqual(HIDKey.name(of: HIDKey.tab), "Tab")
        XCTAssertEqual(HIDKey.name(of: HIDKey.f1), "F1")
        XCTAssertEqual(HIDKey.name(of: HIDKey.f12), "F12")
        XCTAssertEqual(HIDKey.name(of: HIDKey.f13), "F13")
        XCTAssertEqual(HIDKey.name(of: HIDKey.f24), "F24")
        XCTAssertEqual([HIDKey.up, HIDKey.down, HIDKey.left, HIDKey.right].map(HIDKey.name(of:)), ["↑", "↓", "←", "→"])
        XCTAssertEqual(HIDKey.name(of: HIDKey.leftCommand), "Left ⌘")
        XCTAssertEqual(HIDKey.name(of: HIDKey.rightControl), "Right ⌃")
        XCTAssertEqual(HIDKey.name(of: 0xA5), "Key 0xA5", "an unknown usage is still shown, as a number")
    }
    func testEveryUsageOnTheKeyboardPageHasAName() {
        for code in HIDKey.valid { XCTAssertFalse(HIDKey.name(of: code).isEmpty, "\(code)") }
        XCTAssertEqual(Set((0x04...0x1D).map(HIDKey.name(of:))).count, 26, "A to Z are all different")
    }
    func testCharactersAndUsagesConvertBothWays() {
        for code in (0x04...0x27) + (0x2D...0x38).filter({ $0 != 0x32 }) {
            guard let character = HIDKey.character(for: code) else { XCTFail("no character for \(code)"); continue }
            XCTAssertEqual(HIDKey.code(forCharacter: character), code, character)
        }
        XCTAssertEqual(HIDKey.code(forCharacter: "E"), HIDKey.e, "case does not matter")
        XCTAssertNil(HIDKey.code(forCharacter: "ab"))
        XCTAssertNil(HIDKey.code(forCharacter: "é"))
        XCTAssertNil(HIDKey.character(for: HIDKey.up), "arrows have no character")
    }
    func testModifierKeysAreTheLeftAndRightOnes() {
        for code in 0xE0...0xE7 { XCTAssertTrue(HIDKey.isModifier(code)) }
        XCTAssertFalse(HIDKey.isModifier(0xDF)); XCTAssertFalse(HIDKey.isModifier(0xE8)); XCTAssertFalse(HIDKey.isModifier(0x04))
        XCTAssertEqual(HIDKey.modifier(of: HIDKey.leftControl), .control)
        XCTAssertEqual(HIDKey.modifier(of: HIDKey.rightCommand), .command)
        XCTAssertEqual(HIDKey.modifier(of: HIDKey.rightShift), .shift)
        XCTAssertEqual(HIDKey.modifier(of: HIDKey.leftAlt), .alt)
        XCTAssertNil(HIDKey.modifier(of: HIDKey.k))
    }

    // MARK: Chords

    func testModifiersOutsideTheFourAreIgnoredAndATapCarriesNone() {
        XCTAssertEqual(KeyChord(keyCode: HIDKey.k, modifiers: ChordModifiers(rawValue: 0xFF)).modifiers, .all)
        let tap = KeyChord(keyCode: HIDKey.leftCommand, modifiers: [.shift, .control])
        XCTAssertEqual(tap.modifiers, [], "a modifier key pressed alone is the whole chord")
        XCTAssertTrue(tap.isTap)
        XCTAssertEqual(tap, KeyChord(keyCode: HIDKey.leftCommand))
        XCTAssertFalse(KeyChord(keyCode: HIDKey.k, modifiers: .command).isTap)
    }
    func testTitlesUseTheMacOrderOfModifiers() {
        XCTAssertEqual(KeyChord(keyCode: HIDKey.k, modifiers: .command).title, "⌘K")
        XCTAssertEqual(KeyChord(keyCode: HIDKey.k, modifiers: .all).title, "⌃⌥⇧⌘K")
        XCTAssertEqual(KeyChord(keyCode: HIDKey.tab, modifiers: [.shift, .command]).title, "⇧⌘Tab")
        XCTAssertEqual(KeyChord(keyCode: (HIDKey.f1 + 4)).title, "F5")
        XCTAssertEqual(KeyChord(keyCode: HIDKey.leftControl).title, "Tap Left ⌃")
    }
    func testBareTypingKeysAreRefusedAndTheOthersAreNot() {
        for code in [HIDKey.a, HIDKey.k, 0x1E, HIDKey.space, HIDKey.returnKey, HIDKey.escape, HIDKey.tab, HIDKey.backspace, 0x38, 0x59] {
            XCTAssertThrowsError(try KeyChord(keyCode: code).validate(), "\(code)") { XCTAssertEqual($0 as? ChordError, .needsModifier) }
            XCTAssertThrowsError(try KeyChord(keyCode: code, modifiers: .shift).validate(), "shift alone only changes the letter") { XCTAssertEqual($0 as? ChordError, .needsModifier) }
            for modifier: ChordModifiers in [.control, .alt, .command] {
                let chord = KeyChord(keyCode: code, modifiers: modifier)
                if chord == .paletteDefault || chord == .helpDefault { continue }
                XCTAssertNoThrow(try chord.validate(), "\(chord.title)")
            }
        }
        XCTAssertNoThrow(try KeyChord(keyCode: (HIDKey.f1 + 4)).validate(), "function keys type nothing")
        XCTAssertNoThrow(try KeyChord(keyCode: HIDKey.pageUp).validate())
        XCTAssertNoThrow(try KeyChord(keyCode: HIDKey.leftControl).validate(), "a tap on a modifier")
    }
    func testUsagesAKeyboardCannotReportAreRefused() {
        for code in [0, 1, 3, 0xE8, 0x1000, -1] {
            XCTAssertThrowsError(try KeyChord(keyCode: code, modifiers: .command).validate(), "\(code)") { XCTAssertEqual($0 as? ChordError, .unknownKey) }
        }
    }
    func testTheMenuShortcutsAreReserved() {
        XCTAssertEqual(KeyChord.paletteDefault, KeyChord(keyCode: HIDKey.k, modifiers: .command))
        XCTAssertThrowsError(try KeyChord.paletteDefault.validate()) { XCTAssertEqual($0 as? ChordError, .reserved("⌘K")) }
        XCTAssertThrowsError(try KeyChord.settingsDefault.validate()) { XCTAssertEqual($0 as? ChordError, .reserved("⌘,")) }
        XCTAssertNoThrow(try KeyChord(keyCode: HIDKey.k, modifiers: [.command, .shift]).validate())
        XCTAssertFalse(KeyChord.paletteDefault.isValid)
    }
    func testTheHelpShortcutIsCommandSlashAndReservedLikeTheOthers() {
        XCTAssertEqual(KeyChord.helpDefault, KeyChord(keyCode: 0x38, modifiers: .command))
        XCTAssertEqual(KeyChord.helpDefault.title, "⌘/")
        XCTAssertEqual(HIDKey.character(for: KeyChord.helpDefault.keyCode), "/", "the usage a UIKeyCommand for ⌘/ comes back as")
        XCTAssertThrowsError(try KeyChord.helpDefault.validate()) { XCTAssertEqual($0 as? ChordError, .reserved("⌘/")) }
        XCTAssertFalse(KeyChord.helpDefault.isValid)
        XCTAssertThrowsError(try KeyChord(json: KeyChord.helpDefault.json), "and not accepted from storage as a chord either")
        for other: ChordModifiers in [.control, .alt, [.command, .shift], [.command, .alt]] {
            XCTAssertNoThrow(try KeyChord(keyCode: 0x38, modifiers: other).validate(), "only ⌘/ itself is taken, not every / chord")
        }
        let all: Set<KeyChord> = [.paletteDefault, .settingsDefault, .helpDefault]
        XCTAssertEqual(all.count, 3, "three different chords")
    }
    func testAChordRoundTripsThroughJSONAndStrictlyRejectsTheRest() throws {
        let chord = KeyChord(keyCode: HIDKey.e, modifiers: [.command, .shift])
        XCTAssertEqual(try KeyChord(json: chord.json), chord)
        let tap = KeyChord(keyCode: HIDKey.rightControl)
        XCTAssertEqual(try KeyChord(json: tap.json), tap)
        let bad: [JSONValue] = [
            .null, .string("x"), .object([:]),
            .object(["code": .number(4), "mods": .number(0)]),                // a bare letter
            .object(["code": .number(4.5), "mods": .number(8)]),
            .object(["code": .number(-4), "mods": .number(8)]),
            .object(["code": .number(4), "mods": .number(16)]),
            .object(["code": .number(4), "mods": .number(-1)]),
            .object(["code": .number(999), "mods": .number(8)]),
            .object(["code": .string("4"), "mods": .number(8)]),
            .object(["code": .number(4)])
        ]
        for value in bad { XCTAssertThrowsError(try KeyChord(json: value), "\(value)") }
    }

    func testPressingAKeyRecordsTheTerminalKeyItIs() {
        func key(_ code: Int, _ modifiers: ChordModifiers = []) -> TerminalKey? { TerminalKey(chord: KeyChord(keyCode: code, modifiers: modifiers)) }
        XCTAssertEqual(key(HIDKey.escape), .escape); XCTAssertEqual(key(HIDKey.tab), .tab); XCTAssertEqual(key(HIDKey.tab, .shift), .backTab)
        XCTAssertEqual(key(HIDKey.returnKey), .enter); XCTAssertEqual(key(HIDKey.backspace), .backspace); XCTAssertEqual(key(HIDKey.deleteForward), .delete)
        XCTAssertEqual([HIDKey.up, HIDKey.down, HIDKey.left, HIDKey.right].map { key($0) }, [.up, .down, .left, .right])
        XCTAssertEqual([HIDKey.home, HIDKey.end, HIDKey.pageUp, HIDKey.pageDown].map { key($0) }, [.home, .end, .pageUp, .pageDown])
        XCTAssertEqual(key(HIDKey.a, .control), .control("a")); XCTAssertEqual(key(HIDKey.z, .control), .control("z"))
        XCTAssertEqual(key(HIDKey.code(forCharacter: "c")!, .control), .control("c"))
        XCTAssertNil(key(HIDKey.a), "a plain letter is text, not a key")
        XCTAssertNil(key(HIDKey.a, [.control, .shift])); XCTAssertNil(key(HIDKey.up, .command)); XCTAssertNil(key(HIDKey.f1))
        XCTAssertNil(key(0x1E, .control), "Ctrl+1 is not on the contract's list")
        for letter in "abcdefghijklmnopqrstuvwxyz" {
            XCTAssertEqual(key(HIDKey.code(forCharacter: String(letter))!, .control)?.isValid, true, "\(letter)")
        }
    }

    // MARK: Tap detection

    func testAModifierPressedAndReleasedAloneIsATap() {
        var detector = ModifierTapDetector()
        detector.keyDown(HIDKey.leftControl)
        XCTAssertEqual(detector.keyUp(HIDKey.leftControl), KeyChord(keyCode: HIDKey.leftControl))
        XCTAssertNil(detector.keyUp(HIDKey.leftControl), "and only once")
    }
    func testAModifierUsedWithAnotherKeyIsNotATap() {
        var detector = ModifierTapDetector()
        detector.keyDown(HIDKey.leftCommand); detector.keyDown(HIDKey.k)
        XCTAssertNil(detector.keyUp(HIDKey.k)); XCTAssertNil(detector.keyUp(HIDKey.leftCommand))
        detector.keyDown(HIDKey.leftControl); detector.keyDown(HIDKey.leftShift)
        XCTAssertNil(detector.keyUp(HIDKey.leftShift)); XCTAssertNil(detector.keyUp(HIDKey.leftControl), "two modifiers are a chord, not a tap")
    }
    func testAKeyPressedBeforeTheModifierSpoilsItAndPlainKeysAreNeverTaps() {
        var detector = ModifierTapDetector()
        detector.keyDown(HIDKey.a); detector.keyDown(HIDKey.leftControl)
        XCTAssertNil(detector.keyUp(HIDKey.leftControl), "Ctrl pressed while A is held is not alone")
        XCTAssertNil(detector.keyUp(HIDKey.a))
        detector.keyDown(HIDKey.a)
        XCTAssertNil(detector.keyUp(HIDKey.a))
        detector.keyDown(HIDKey.leftControl)
        XCTAssertNotNil(detector.keyUp(HIDKey.leftControl), "and it works again once everything was let go")
    }
    func testAKeyUsedBehindOurBackIsSpoiledToo() {
        var detector = ModifierTapDetector()
        detector.keyDown(HIDKey.leftControl)
        detector.spoil()   // Ctrl-N went to a key command, which never shows us the N
        XCTAssertNil(detector.keyUp(HIDKey.leftControl))
        detector.keyDown(HIDKey.leftControl)
        XCTAssertNotNil(detector.keyUp(HIDKey.leftControl), "the next tap is fine")
    }
    func testResetForgetsWhatWasHeld() {
        var detector = ModifierTapDetector()
        detector.keyDown(HIDKey.a); detector.reset()
        detector.keyDown(HIDKey.leftControl)
        XCTAssertNotNil(detector.keyUp(HIDKey.leftControl))
    }

    // MARK: Shortcuts

    func testTheMapFindsTheFixedShortcutsTheMenuShortcutsAndTheHotkeys() {
        let esc = Hotkey(id: "e", label: "Esc", steps: [.key(.escape)], chord: KeyChord(keyCode: HIDKey.e, modifiers: .command))
        let plain = Hotkey(id: "p", label: "Plain", steps: [.text("x")])
        let tap = KeyChord(keyCode: HIDKey.leftControl)
        let map = ShortcutMap(hotkeys: [esc, plain], settings: ShortcutSettings(paletteChords: [tap]))
        XCTAssertEqual(map.action(for: .paletteDefault), .openPalette)
        XCTAssertEqual(map.action(for: .settingsDefault), .openSettings)
        XCTAssertEqual(map.action(for: tap), .openPalette)
        XCTAssertEqual(map.action(for: esc.chord!), .hotkey(esc))
        XCTAssertNil(map.action(for: KeyChord(keyCode: HIDKey.e, modifiers: .control)))
        XCTAssertTrue(map.hasTapChords)
        XCTAssertFalse(ShortcutMap(hotkeys: [esc]).hasTapChords)
        XCTAssertEqual(Set(ShortcutMap(hotkeys: [esc], settings: ShortcutSettings(paletteChords: [tap])).chords), [esc.chord!, tap])
    }
    func testTheHelpShortcutsAreFoundLikeTheMenuOnes() {
        let help = KeyChord(keyCode: HIDKey.code(forCharacter: "j")!, modifiers: .command)
        let tap = KeyChord(keyCode: HIDKey.rightControl)
        let map = ShortcutMap(hotkeys: [], settings: ShortcutSettings(helpChords: [help, tap]))
        XCTAssertEqual(map.action(for: .helpDefault), .openHelp, "⌘/ is always there")
        XCTAssertEqual(map.action(for: help), .openHelp)
        XCTAssertEqual(map.action(for: tap), .openHelp)
        XCTAssertEqual(map.action(for: .paletteDefault), .openPalette, "and the menu is still ⌘K")
        XCTAssertEqual(ShortcutMap(hotkeys: []).action(for: .helpDefault), .openHelp, "with no settings at all")
        XCTAssertTrue(map.hasTapChords, "a tap on a modifier that opens the help is worth watching for")
        XCTAssertFalse(ShortcutMap(hotkeys: [], settings: ShortcutSettings(helpChords: [help])).hasTapChords)
        XCTAssertEqual(Set(map.chords), [help, tap], "the extra ones get key commands; the fixed ones have their own")
    }
    func testAHelpShortcutIsValidatedAndNeverSharedWithTheMenuOrAHotkey() throws {
        var settings = ShortcutSettings()
        let chord = KeyChord(keyCode: HIDKey.e, modifiers: .command)
        let esc = Hotkey(id: "e", label: "Esc", steps: [.key(.escape)], chord: chord)
        XCTAssertThrowsError(try settings.addHelpChord(chord, library: HotkeyLibrary(hotkeys: [esc]))) { XCTAssertEqual($0 as? HotkeyError, .chordInUse("Esc")) }
        XCTAssertThrowsError(try settings.addHelpChord(KeyChord(keyCode: HIDKey.a)), "a bare letter")
        XCTAssertThrowsError(try settings.addHelpChord(.helpDefault), "⌘/ is always there")
        XCTAssertThrowsError(try settings.addHelpChord(.paletteDefault), "⌘K is the menu's")
        XCTAssertThrowsError(try settings.addHelpChord(.settingsDefault), "⌘, is the settings'")
        try settings.addHelpChord(KeyChord(keyCode: HIDKey.rightControl))
        try settings.addHelpChord(KeyChord(keyCode: HIDKey.rightControl))
        XCTAssertEqual(settings.helpChords, [KeyChord(keyCode: HIDKey.rightControl)], "no repeats")
        XCTAssertThrowsError(try settings.addPaletteChord(KeyChord(keyCode: HIDKey.rightControl))) { XCTAssertEqual($0 as? HotkeyError, .chordInUse("the hotkey help")) }
        try settings.addPaletteChord(KeyChord(keyCode: HIDKey.leftControl))
        XCTAssertThrowsError(try settings.addHelpChord(KeyChord(keyCode: HIDKey.leftControl))) { XCTAssertEqual($0 as? HotkeyError, .chordInUse("the hotkey menu")) }
        XCTAssertEqual(settings.paletteChords, [KeyChord(keyCode: HIDKey.leftControl)])
        settings.removeHelpChord(KeyChord(keyCode: HIDKey.rightControl))
        XCTAssertEqual(settings.helpChords, [])
    }
    func testAtMostAFewHelpShortcutsAreKeptAndTheyAreStoredStrictly() throws {
        var settings = ShortcutSettings()
        for code in 0xE0...0xE7 {
            do { try settings.addHelpChord(KeyChord(keyCode: code)) } catch { XCTAssertEqual(settings.helpChords.count, ShortcutSettings.maxHelpChords) }
        }
        XCTAssertEqual(settings.helpChords.count, ShortcutSettings.maxHelpChords)
        XCTAssertEqual(ShortcutSettings(helpChords: (0xE0...0xE7).map { KeyChord(keyCode: $0) }).helpChords.count, ShortcutSettings.maxHelpChords)
        XCTAssertEqual(ShortcutSettings(encoded: settings.encoded), settings, "round trip")
        let both = ShortcutSettings(paletteChords: [KeyChord(keyCode: HIDKey.leftControl)], helpChords: [KeyChord(keyCode: HIDKey.leftControl), KeyChord(keyCode: HIDKey.rightControl), .helpDefault, KeyChord(keyCode: HIDKey.a)])
        XCTAssertEqual(both.paletteChords, [KeyChord(keyCode: HIDKey.leftControl)])
        XCTAssertEqual(both.helpChords, [KeyChord(keyCode: HIDKey.rightControl)], "one chord opens one thing; reserved and unusable chords are dropped")
        XCTAssertEqual(ShortcutSettings(encoded: both.encoded), both)
    }
    func testWhatWasStoredBeforeTheHelpExistedReadsAndWritesAsBefore() {
        let old = "{\"v\":1,\"palette\":[{\"code\":224,\"mods\":0}]}"
        let loaded = ShortcutSettings(encoded: old)
        XCTAssertEqual(loaded.paletteChords, [KeyChord(keyCode: HIDKey.leftControl)])
        XCTAssertEqual(loaded.helpChords, [])
        XCTAssertFalse(loaded.encoded.contains("help"), "nothing new is written until there is something to write")
        XCTAssertEqual(ShortcutSettings(encoded: "{\"v\":1,\"palette\":[],\"help\":[{\"code\":4,\"mods\":0},{\"code\":229,\"mods\":0}]}").helpChords,
                       [KeyChord(keyCode: HIDKey.rightShift)], "a bad help entry is dropped like a bad menu one")
    }
    func testAMenuShortcutCannotShadowAHotkeyAndIsStoredStrictly() throws {
        var settings = ShortcutSettings()
        let chord = KeyChord(keyCode: HIDKey.e, modifiers: .command)
        let esc = Hotkey(id: "e", label: "Esc", steps: [.key(.escape)], chord: chord)
        XCTAssertThrowsError(try settings.addPaletteChord(chord, library: HotkeyLibrary(hotkeys: [esc]))) { XCTAssertEqual($0 as? HotkeyError, .chordInUse("Esc")) }
        XCTAssertThrowsError(try settings.addPaletteChord(KeyChord(keyCode: HIDKey.a)))
        XCTAssertThrowsError(try settings.addPaletteChord(.paletteDefault), "⌘K is always there")
        try settings.addPaletteChord(KeyChord(keyCode: HIDKey.leftControl))
        try settings.addPaletteChord(KeyChord(keyCode: HIDKey.leftControl))
        XCTAssertEqual(settings.paletteChords.count, 1, "no repeats")
        XCTAssertEqual(ShortcutSettings(encoded: settings.encoded), settings)
        for bad in [nil, "", "nope", "{}", "{\"v\":2,\"palette\":[]}", "{\"v\":1,\"palette\":[{\"code\":4,\"mods\":0}]}"] as [String?] {
            XCTAssertEqual(ShortcutSettings(encoded: bad), ShortcutSettings(), "\(String(describing: bad))")
        }
        settings.removePaletteChord(KeyChord(keyCode: HIDKey.leftControl))
        XCTAssertEqual(settings.paletteChords, [])
    }
    func testAtMostAFewMenuShortcutsAreKept() throws {
        var settings = ShortcutSettings()
        for code in 0xE0...0xE7 {
            do { try settings.addPaletteChord(KeyChord(keyCode: code)) } catch { XCTAssertEqual(settings.paletteChords.count, ShortcutSettings.maxPaletteChords) }
        }
        XCTAssertEqual(settings.paletteChords.count, ShortcutSettings.maxPaletteChords)
        XCTAssertEqual(ShortcutSettings(paletteChords: (0xE0...0xE7).map { KeyChord(keyCode: $0) }).paletteChords.count, ShortcutSettings.maxPaletteChords)
    }

    // MARK: Key events

    func testAKeyEventIsDescribedWithEverythingNeededToIdentifyAKey() {
        let event = KeyEventRecord(phase: .down, keyCode: 0xE3, modifiers: [], rawModifiers: 0x100000, characters: "", charactersIgnoringModifiers: "")
        XCTAssertEqual(event.lines[0], "down  keyCode 227 · usage 0x07/0xE3")
        XCTAssertEqual(event.lines[1], "Left ⌘")
        XCTAssertEqual(event.lines[2], "mods none  raw 0x100000")
        XCTAssertEqual(event.lines[3], "chars ∅  plain ∅")
        let typed = KeyEventRecord(phase: .down, keyCode: HIDKey.e, modifiers: [.command, .shift], rawModifiers: 0x120000, characters: "E", charactersIgnoringModifiers: "e")
        XCTAssertEqual(typed.chord, KeyChord(keyCode: HIDKey.e, modifiers: [.command, .shift]))
        XCTAssertTrue(typed.summary.contains("mods ⇧⌘"))
        let command = KeyEventRecord(phase: .command, keyCode: nil, modifiers: .command, characters: "k", charactersIgnoringModifiers: "k")
        XCTAssertNil(command.chord)
        XCTAssertEqual(command.lines[0], "command  (no keyCode)")
    }
    func testControlCharactersInAKeyEventAreMadeVisible() {
        XCTAssertEqual(KeyEventRecord.visible("\u{1b}"), "⎋")
        XCTAssertEqual(KeyEventRecord.visible("\t \r"), "⇥␠⏎")
        XCTAssertEqual(KeyEventRecord.visible("\u{3}"), "^C")
        XCTAssertEqual(KeyEventRecord.visible(""), "∅")
        XCTAssertEqual(KeyEventRecord.visible("é"), "é")
    }
}
