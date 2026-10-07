import XCTest
@testable import RiWorkCore

@MainActor final class ChatDraftsTests: XCTestCase {
    private var suites: [String] = []
    override func tearDown() async throws { for name in suites { UserDefaults().removePersistentDomain(forName: name) }; suites = [] }
    private func defaults() -> UserDefaults {
        let name = "com.riwork.tests.drafts.\(UUID().uuidString)"
        suites.append(name)
        return UserDefaults(suiteName: name)!
    }

    func testADraftIsKeptPerChatAndSurvivesARelaunch() {
        let defaults = defaults()
        let store = ChatDraftStore(defaults: defaults)
        store.setText("first chat", for: "a")
        store.setText("second chat", for: "b")
        let again = ChatDraftStore(defaults: defaults)
        XCTAssertEqual(again.draft("a")?.text, "first chat")
        XCTAssertEqual(again.draft("b")?.text, "second chat")
        XCTAssertEqual(again.draft("a")?.restored.uncertain, false)
    }
    func testClearingTheTextForgetsTheDraft() {
        let defaults = defaults()
        let store = ChatDraftStore(defaults: defaults)
        store.setText("typed", for: "a")
        store.setText("", for: "a")
        XCTAssertNil(store.draft("a"))
        XCTAssertNil(defaults.data(forKey: ChatDraftStore.key), "nothing left to save")
    }
    func testAMessageOnItsWayIsHeldUntilAnsweredAndComesBackUncertainAfterARelaunch() {
        let defaults = defaults()
        let store = ChatDraftStore(defaults: defaults)
        store.setText("run the tests", for: "a")
        store.beginSending("run the tests", for: "a")
        store.setText("", for: "a")
        XCTAssertEqual(store.draft("a")?.sending, "run the tests", "held while the desktop has not answered")
        // The app ends here.
        let relaunched = ChatDraftStore(defaults: defaults)
        XCTAssertEqual(relaunched.draft("a")?.restored.text, "run the tests")
        XCTAssertEqual(relaunched.draft("a")?.restored.uncertain, true, "never sent again by itself")
        // Answered: gone.
        store.endSending(for: "a")
        XCTAssertNil(store.draft("a"))
        XCTAssertNil(ChatDraftStore(defaults: defaults).draft("a"))
    }
    func testAnUncertainMessageGoesBeforeWhatWasTypedSince() {
        XCTAssertEqual(ChatDraft(text: "and then this", sending: "first this").restored.text, "first this\nand then this")
        XCTAssertEqual(ChatDraft(text: "only", sending: "").restored.text, "only")
    }
    func testOldDraftsAreLetGoAndTheCountIsBounded() {
        let defaults = defaults()
        var clock = Date(timeIntervalSince1970: 1_800_000_000)
        let store = ChatDraftStore(defaults: defaults, now: { clock })
        store.setText("old", for: "old")
        clock.addTimeInterval(ChatDraftStore.maximumAge + 60)
        store.setText("new", for: "new")
        let later = ChatDraftStore(defaults: defaults, now: { clock })
        XCTAssertNil(later.draft("old"), "a month untouched: its chat is most likely gone")
        XCTAssertEqual(later.draft("new")?.text, "new")
        for index in 0..<(ChatDraftStore.maximumDrafts + 5) { clock.addTimeInterval(1); later.setText("draft \(index)", for: "chat-\(index)") }
        XCTAssertEqual(later.chatIDs.count, ChatDraftStore.maximumDrafts)
        XCTAssertNil(later.draft("new"), "the oldest goes first")
        XCTAssertNotNil(later.draft("chat-\(ChatDraftStore.maximumDrafts + 4)"))
    }
    func testAHugeDraftIsCutBetweenCharacters() {
        let text = String(repeating: "é🦀", count: ChatDraftStore.maximumBytes)
        let bounded = ChatDraftStore.bounded(text)
        XCTAssertLessThanOrEqual(bounded.utf8.count, ChatDraftStore.maximumBytes)
        XCTAssertGreaterThan(bounded.utf8.count, ChatDraftStore.maximumBytes - 5)
        XCTAssertTrue(text.hasPrefix(bounded))
        XCTAssertEqual(ChatDraftStore.bounded("short"), "short")
    }
    func testAnUnreadableStoreStartsEmpty() {
        let defaults = defaults()
        defaults.set(Data("not json".utf8), forKey: ChatDraftStore.key)
        XCTAssertTrue(ChatDraftStore(defaults: defaults).chatIDs.isEmpty)
    }
}
