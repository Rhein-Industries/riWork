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

    // MARK: The editor's draft

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
