import XCTest
@testable import RiWorkCore

final class ChatNoticesTests: XCTestCase {
    private func notice(_ id: String, _ text: String, _ level: ChatNoticeLevel = .warning) -> ChatItem {
        ChatItem(id: id, status: .completed, body: .notice(level: level, text: text))
    }
    private func user(_ id: String) -> ChatItem { ChatItem(id: id, status: .completed, body: .userMessage("go")) }
    private func reply(_ id: String) -> ChatItem { ChatItem(id: id, status: .completed, body: .agentMessage("done")) }

    func testNumbersDoNotMakeANewKind() {
        XCTAssertEqual(ChatProviderNotice.kind(of: "Reconnecting… 2/5"), ChatProviderNotice.kind(of: "Reconnecting… 3/5"))
        XCTAssertEqual(ChatProviderNotice.kind(of: "API retry 1 of 10 in 532 ms"), ChatProviderNotice.kind(of: "API retry 2 of 10 in 1204 ms"))
        XCTAssertNotEqual(ChatProviderNotice.kind(of: "Rate limited"), ChatProviderNotice.kind(of: "Context compacted"))
    }
    func testTheBannerShowsTheLatestOfEachKindInTheCurrentTurnOnly() {
        let items = [notice("n1", "This account is close to the weekly usage limit"), user("u1"), reply("a1"),
                     notice("n2", "Reconnecting… 1/5"), notice("n3", "Reconnecting… 2/5"),
                     notice("n4", "This account is close to the weekly usage limit"), notice("n5", "Request failed", .error)]
        let current = ChatNotices.current(items, dismissed: [])
        XCTAssertEqual(current.map(\.id), ["n5", "n4", "n3"], "the error first, then the newest; one per kind")
        XCTAssertEqual(current.first { $0.id == "n4" }?.count, 2, "said twice in the chat")
        XCTAssertEqual(current.first { $0.id == "n3" }?.text, "Reconnecting… 2/5", "replaced in place, not stacked")
        // A new turn: what the provider said in the last one is history now.
        XCTAssertTrue(ChatNotices.current(items + [user("u2")], dismissed: []).isEmpty)
        XCTAssertEqual(ChatNotices.all(items + [user("u2")]).count, 5, "all of them stay in the history")
    }
    func testADismissedNoticeComesBackOnlyWhenSaidAgain() {
        var items = [user("u1"), notice("n1", "Rate limited, retrying")]
        XCTAssertTrue(ChatNotices.current(items, dismissed: ["n1"]).isEmpty)
        items.append(notice("n2", "Rate limited, retrying"))
        XCTAssertEqual(ChatNotices.current(items, dismissed: ["n1"]).map(\.id), ["n2"])
    }
    func testBeforeAnythingIsSentEveryNoticeIsCurrentAndNoticesAreNotTranscriptRows() {
        let items = [notice("n1", "Config warning: unknown key", .info)]
        XCTAssertEqual(ChatNotices.current(items, dismissed: []).map(\.id), ["n1"])
        XCTAssertFalse(ChatNotices.isTranscriptRow(items[0]))
        XCTAssertTrue(ChatNotices.isTranscriptRow(reply("a")))
    }
    func testThePhonesOwnMessagesAreOnePerSourceReplacedInPlace() {
        var alerts = ChatAlerts()
        alerts.show(.read, "Could not read the conversation")
        alerts.show(.read, "Could not read the conversation")
        alerts.show(.action, "Couldn’t send", level: .error)
        XCTAssertEqual(alerts.items.count, 2)
        XCTAssertEqual(alerts.items.first { $0.source == .read }?.repeats, 2, "a recurring error counts up in one banner")
        XCTAssertEqual(alerts.ordered.first?.source, .action, "the error first")
        alerts.show(.read, "Timed out reading")
        XCTAssertEqual(alerts.items.first { $0.source == .read }?.repeats, 1, "a different text starts over")
        alerts.clear(.read)
        XCTAssertNil(alerts.text(.read))
        XCTAssertEqual(alerts.items.count, 1)
    }
}
