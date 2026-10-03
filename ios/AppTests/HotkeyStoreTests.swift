import XCTest
import RiWorkCore
@testable import RiWorkRemote

@MainActor final class HotkeyStoreTests: XCTestCase {
    private func scratchDefaults() -> UserDefaults {
        let name = "com.riwork.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }
    private func hotkey(_ label: String, _ steps: [KeyItem] = [.text("ls"), .key(.enter)]) -> Hotkey { Hotkey(label: label, steps: steps) }

    func testHotkeysArePersistedInOrderAndComeBackOnTheNextLaunch() throws {
        let defaults = scratchDefaults()
        let store = HotkeyStore(defaults: defaults)
        XCTAssertEqual(store.custom, [])
        let one = hotkey("One"), two = hotkey("Two", [.key(.control("c")), .text("exit"), .key(.enter)]), three = hotkey("Three")
        try store.add(one); try store.add(two); try store.add(three)
        let launched = HotkeyStore(defaults: defaults)
        XCTAssertEqual(launched.custom, [one, two, three])
        XCTAssertEqual(launched.custom[1].steps, [.key(.control("c")), .text("exit"), .key(.enter)])
    }
    func testEditingDeletingAndReorderingAreAllPersisted() throws {
        let defaults = scratchDefaults()
        let store = HotkeyStore(defaults: defaults)
        var a = hotkey("A"), b = hotkey("B"), c = hotkey("C")
        try store.add(a); try store.add(b); try store.add(c)
        b.label = "Bee"; b.steps = [.text("x")]
        try store.update(b)
        XCTAssertEqual(HotkeyStore(defaults: defaults).custom.map(\.label), ["A", "Bee", "C"])
        store.move(fromOffsets: IndexSet(integer: 2), toOffset: 0)
        XCTAssertEqual(HotkeyStore(defaults: defaults).custom.map(\.label), ["C", "A", "Bee"])
        store.remove(atOffsets: IndexSet(integer: 1))
        XCTAssertEqual(HotkeyStore(defaults: defaults).custom.map(\.label), ["C", "Bee"])
        store.remove(id: c.id)
        a = hotkey("Only")
        XCTAssertEqual(HotkeyStore(defaults: defaults).custom.map(\.label), ["Bee"])
        XCTAssertEqual(HotkeyStore(defaults: defaults).custom[0].steps, [.text("x")])
    }
    func testAnInvalidHotkeyIsRefusedAndNothingIsStored() throws {
        let defaults = scratchDefaults()
        let store = HotkeyStore(defaults: defaults)
        XCTAssertThrowsError(try store.add(hotkey("")))
        XCTAssertThrowsError(try store.add(hotkey("Newline", [.text("a\nb")])))
        XCTAssertThrowsError(try store.add(hotkey("No keys", [])))
        XCTAssertEqual(store.custom, [])
        XCTAssertNil(defaults.string(forKey: HotkeyStore.key), "nothing was written")
        let good = hotkey("Good")
        try store.add(good)
        var broken = good; broken.steps = [.text("a\u{1b}")]
        XCTAssertThrowsError(try store.update(broken))
        XCTAssertEqual(HotkeyStore(defaults: defaults).custom, [good], "a refused edit changes nothing on disk either")
    }
    func testUnreadableStorageStartsEmptyAndKeepsWorking() throws {
        let defaults = scratchDefaults()
        defaults.set("this is not json", forKey: HotkeyStore.key)
        let store = HotkeyStore(defaults: defaults)
        XCTAssertEqual(store.custom, [])
        try store.add(hotkey("Fresh"))
        XCTAssertEqual(HotkeyStore(defaults: defaults).custom.map(\.label), ["Fresh"])
    }
    func testStoredEntriesThatBreakTheContractAreDroppedOnLoad() {
        let defaults = scratchDefaults()
        defaults.set("""
        {"v":1,"hotkeys":[{"id":"a","label":"Ok","steps":[{"key":"Enter"}]},{"id":"b","label":"Bad","steps":[{"text":"a\\nb"}]},{"id":"c","label":"Nope","steps":[{"key":"F5"}]}]}
        """, forKey: HotkeyStore.key)
        XCTAssertEqual(HotkeyStore(defaults: defaults).custom.map(\.label), ["Ok"])
    }
    func testTheModelSharesTheStoreWithTheKeyBar() async throws {
        let defaults = scratchDefaults()
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let model = RemoteModel(client: FixtureTransport(), keychain: keychain, defaults: defaults)
        try model.hotkeys.add(hotkey("Mine"))
        let again = RemoteModel(client: FixtureTransport(), keychain: keychain, defaults: defaults)
        XCTAssertEqual(again.hotkeys.custom.map(\.label), ["Mine"])
    }

    // MARK: Shortcuts and templates

    private func chord(_ letter: String, _ modifiers: ChordModifiers = .command) -> KeyChord { KeyChord(keyCode: HIDKey.code(forCharacter: letter)!, modifiers: modifiers) }

    func testAShortcutOnAHotkeyIsPersisted() throws {
        let defaults = scratchDefaults()
        let store = HotkeyStore(defaults: defaults)
        let esc = Hotkey(id: "esc", label: "Esc", steps: [.key(.escape)], chord: chord("e"), showsOnBar: false)
        try store.add(esc)
        let launched = HotkeyStore(defaults: defaults)
        XCTAssertEqual(launched.custom, [esc])
        XCTAssertEqual(launched.custom[0].chord, chord("e"))
        XCTAssertEqual(launched.onBar, [], "a shortcut-only hotkey stays off the key bar")
        XCTAssertEqual(launched.shortcutMap.action(for: chord("e")), .hotkey(esc))
    }
    func testTheSameShortcutCannotBeGivenTwiceEvenToTheMenu() throws {
        let store = HotkeyStore(defaults: scratchDefaults())
        try store.add(Hotkey(id: "a", label: "A", steps: [.text("a")], chord: chord("e")))
        XCTAssertThrowsError(try store.add(Hotkey(id: "b", label: "B", steps: [.text("b")], chord: chord("e")))) { XCTAssertEqual($0 as? HotkeyError, .chordInUse("A")) }
        let tap = KeyChord(keyCode: HIDKey.leftControl)
        try store.addPaletteChord(tap)
        XCTAssertThrowsError(try store.addPaletteChord(chord("e"))) { XCTAssertEqual($0 as? HotkeyError, .chordInUse("A")) }
        XCTAssertThrowsError(try store.add(Hotkey(id: "c", label: "C", steps: [.text("c")], chord: tap)), "a tap on Control opens the menu")
        XCTAssertEqual(store.custom.map(\.label), ["A"])
    }
    func testMenuShortcutsArePersistedAndRemovable() throws {
        let defaults = scratchDefaults()
        let store = HotkeyStore(defaults: defaults)
        try store.addPaletteChord(KeyChord(keyCode: HIDKey.leftControl))
        XCTAssertEqual(HotkeyStore(defaults: defaults).shortcuts.paletteChords, [KeyChord(keyCode: HIDKey.leftControl)])
        store.removePaletteChord(KeyChord(keyCode: HIDKey.leftControl))
        XCTAssertEqual(HotkeyStore(defaults: defaults).shortcuts.paletteChords, [])
        defaults.set("garbage", forKey: HotkeyStore.shortcutsKey)
        XCTAssertEqual(HotkeyStore(defaults: defaults).shortcuts.paletteChords, [], "unreadable shortcuts are none")
    }
    func testInstallingTheTemplateLeavesOutWhatWouldBeShadowedByTheMenuOrTheHelp() throws {
        let store = HotkeyStore(defaults: scratchDefaults())
        try store.addHelpChord(chord("e"))
        try store.addPaletteChord(chord("t"))
        let result = store.install(.clicks)
        XCTAssertEqual(result.added.count, HotkeyTemplate.clicks.hotkeys.count - 2)
        XCTAssertEqual(result.skipped.map(\.reason), [.shortcutTaken(by: "the hotkey help"), .shortcutTaken(by: "the hotkey menu")])
        XCTAssertEqual(store.shortcutMap.action(for: chord("e")), .openHelp)
        XCTAssertEqual(store.shortcutMap.action(for: chord("t")), .openPalette)
        XCTAssertFalse(store.custom.contains { $0.chord == chord("e") || $0.chord == chord("t") })
    }
    func testInstallingTheClicksTemplateKeepsWhatThePersonHadAndPersists() throws {
        let defaults = scratchDefaults()
        let store = HotkeyStore(defaults: defaults)
        let mine = hotkey("Mine", [.text("/clear"), .key(.enter)])
        try store.add(mine)
        let result = store.install(.clicks)
        XCTAssertEqual(result.added.count, HotkeyTemplate.clicks.hotkeys.count)
        XCTAssertEqual(result.paletteChordsAdded, HotkeyTemplate.clicks.paletteChords)
        XCTAssertEqual(store.custom.first, mine, "theirs stays first and unchanged")
        XCTAssertEqual(store.custom.count, 1 + HotkeyTemplate.clicks.hotkeys.count)
        XCTAssertEqual(store.onBar, [mine], "the template's shortcuts stay off the key bar")
        let launched = HotkeyStore(defaults: defaults)
        XCTAssertEqual(launched.custom, store.custom)
        XCTAssertEqual(launched.shortcuts, store.shortcuts)
        XCTAssertEqual(launched.shortcutMap.action(for: chord("e")), .hotkey(HotkeyTemplate.clicks.hotkeys[0]))
        XCTAssertEqual(launched.shortcutMap.action(for: KeyChord(keyCode: HIDKey.leftControl)), .openPalette)
    }
    func testInstallingTwiceChangesNothingAndASecondInstallWritesNothing() throws {
        let defaults = scratchDefaults()
        let store = HotkeyStore(defaults: defaults)
        store.install(.clicks)
        let before = store.custom, shortcuts = store.shortcuts
        let stored = defaults.string(forKey: HotkeyStore.key)
        let again = store.install(.clicks)
        XCTAssertFalse(again.changedAnything)
        XCTAssertEqual(store.custom, before); XCTAssertEqual(store.shortcuts, shortcuts)
        XCTAssertEqual(defaults.string(forKey: HotkeyStore.key), stored)
    }
    func testATemplateNeverTakesAShortcutThatAlreadyBelongsToThePersonOrTheMenu() throws {
        let store = HotkeyStore(defaults: scratchDefaults())
        try store.add(Hotkey(id: "mine", label: "Mine", steps: [.text("hello")], chord: chord("e")))
        try store.addPaletteChord(KeyChord(keyCode: HIDKey.leftControl))
        let result = store.install(.clicks)
        XCTAssertEqual(result.skipped.map(\.hotkey.id), ["template.clicks.esc"])
        XCTAssertEqual(store.shortcutMap.action(for: chord("e")), .hotkey(store.custom[0]), "theirs")
        XCTAssertEqual(result.paletteChordsAdded, [KeyChord(keyCode: HIDKey.rightControl)], "the Control tap they had already is not added twice")
    }

    // MARK: The editor's draft

    func testADraftKeepsTheShortcutAndWhetherTheBarShowsIt() {
        let original = Hotkey(id: "x", label: "Esc", steps: [.key(.escape)], chord: chord("e"), showsOnBar: false)
        var draft = HotkeyDraft(original)
        XCTAssertEqual(draft.chord, chord("e")); XCTAssertFalse(draft.showsOnBar)
        XCTAssertEqual(draft.hotkey, original)
        draft.chord = KeyChord(keyCode: HIDKey.e)   // a bare letter
        XCTAssertNotNil(draft.problem, "an unusable shortcut stops the save")
        draft.chord = nil; draft.showsOnBar = true
        XCTAssertNil(draft.problem)
        XCTAssertNil(draft.hotkey.chord)
        XCTAssertNil(HotkeyDraft().chord); XCTAssertTrue(HotkeyDraft().showsOnBar)
    }
    func testADraftRoundTripsAHotkey() {
        let original = hotkey("Bail", [.key(.control("c")), .text("exit"), .key(.enter)])
        let draft = HotkeyDraft(original)
        XCTAssertFalse(draft.isNew)
        XCTAssertEqual(draft.steps.map(\.kind), [.key, .text, .key])
        XCTAssertEqual(draft.hotkey, original)
        XCTAssertNil(draft.problem)
    }
    func testANewDraftIsEmptyAndNotValidYetAndBecomesValidStepByStep() {
        var draft = HotkeyDraft()
        XCTAssertTrue(draft.isNew)
        XCTAssertEqual(draft.problem, .emptyLabel)
        draft.label = "  Clear  "
        XCTAssertEqual(draft.problem, .noSteps)
        draft.steps.append(.init(kind: .text, text: "/clear"))
        draft.steps.append(.init(kind: .key, key: .enter))
        XCTAssertNil(draft.problem)
        XCTAssertEqual(draft.hotkey.label, "Clear", "the name is trimmed")
        XCTAssertEqual(draft.hotkey.steps, [.text("/clear"), .key(.enter)])
        draft.steps[0].text = "/cl\near"
        XCTAssertEqual(draft.problem, .invalidStep(index: 0, reason: "text cannot contain line breaks or control characters; use the Enter key instead."))
        draft.steps.move(fromOffsets: IndexSet(integer: 1), toOffset: 0)
        XCTAssertEqual(draft.steps.map(\.kind), [.key, .text], "steps reorder")
    }
}
