import XCTest
@testable import RiWorkCore

final class ChatNoticesTests: XCTestCase {
    private func notice(_ id: String, _ text: String, _ level: ChatNoticeLevel = .warning, kind: String? = nil) -> ChatItem {
        ChatItem(id: id, status: .completed, body: .notice(level: level, text: text, kind: kind))
    }
    private func user(_ id: String) -> ChatItem { ChatItem(id: id, status: .completed, body: .userMessage("go")) }
    private func reply(_ id: String) -> ChatItem { ChatItem(id: id, status: .completed, body: .agentMessage("done")) }

    func testTheHostsKindGroupsAndANoticeWithoutOneIsItsOwnLine() {
        XCTAssertEqual(ChatProviderNotice.key(kind: "reconnecting", text: "Reconnecting… 2/5"), ChatProviderNotice.key(kind: "reconnecting", text: "Reconnecting… 3/5"))
        XCTAssertNotEqual(ChatProviderNotice.key(kind: nil, text: "Reconnecting… 2/5"), ChatProviderNotice.key(kind: nil, text: "Reconnecting… 3/5"), "no kind: by its text")
        XCTAssertEqual(ChatProviderNotice.key(kind: nil, text: "Rate limited "), ChatProviderNotice.key(kind: nil, text: "Rate limited"))
        XCTAssertNotEqual(ChatProviderNotice.key(kind: "rate_limit:five_hour", text: "x"), ChatProviderNotice.key(kind: "rate_limit:seven_day", text: "x"))
        XCTAssertNotEqual(ChatProviderNotice.key(kind: "api_retry", text: "api_retry"), ChatProviderNotice.key(kind: nil, text: "api_retry"), "a kind never meets a text")
    }
    func testTheKindIsReadWhenSentAndToleratedWhenNot() throws {
        func decode(_ json: String) throws -> ChatItemBody { try JSONDecoder().decode(ChatItemBody.self, from: Data(json.utf8)) }
        XCTAssertEqual(try decode(#"{"type":"notice","level":"warning","text":"Weekly limit","kind":"rate_limit:seven_day","extra":1}"#),
                       .notice(level: .warning, text: "Weekly limit", kind: "rate_limit:seven_day"))
        XCTAssertEqual(try decode(#"{"type":"notice","level":"error","text":"Failed"}"#), .notice(level: .error, text: "Failed", kind: nil), "older hosts send none")
        XCTAssertEqual(try decode(#"{"type":"notice","text":"x","kind":7}"#), .notice(level: .info, text: "x", kind: nil), "a kind that is not a string is none")
        XCTAssertEqual(try decode(#"{"type":"notice","text":"x","kind":" "}"#), .notice(level: .info, text: "x", kind: nil))
        let encoded = try JSONEncoder().encode(ChatItemBody.notice(level: .info, text: "x", kind: "silence"))
        XCTAssertEqual(try JSONDecoder().decode(ChatItemBody.self, from: encoded), .notice(level: .info, text: "x", kind: "silence"))
    }
    func testTheBannerShowsTheLatestOfEachKindInTheCurrentTurnOnly() {
        let items = [notice("n1", "This account is close to the weekly usage limit", kind: "rate_limit:seven_day"), user("u1"), reply("a1"),
                     notice("n2", "Reconnecting… 1/5", kind: "reconnecting"), notice("n3", "Reconnecting… 2/5", kind: "reconnecting"),
                     notice("n4", "This account is close to the weekly usage limit", kind: "rate_limit:seven_day"),
                     notice("n5", "Request failed", .error, kind: "turn_failed"), notice("n6", "Something new from the provider", kind: "brand_new_kind")]
        let current = ChatNotices.current(items, dismissed: [])
        XCTAssertEqual(current.map(\.id), ["n5", "n6", "n4", "n3"], "the error first, then the newest; one per kind, an unknown kind as it is")
        XCTAssertEqual(current.first { $0.id == "n4" }?.count, 2, "said twice in the chat")
        XCTAssertEqual(current.first { $0.id == "n3" }?.text, "Reconnecting… 2/5", "replaced in place, not stacked")
        // A new turn: what the provider said in the last one is history now.
        XCTAssertTrue(ChatNotices.current(items + [user("u2")], dismissed: []).isEmpty)
        XCTAssertEqual(ChatNotices.all(items + [user("u2")]).count, 6, "all of them stay in the history")
        // Without kinds (an older host) each text is its own line.
        let old = [user("u"), notice("o1", "Reconnecting… 1/5"), notice("o2", "Reconnecting… 2/5"), notice("o3", "Reconnecting… 2/5")]
        XCTAssertEqual(ChatNotices.current(old, dismissed: []).map(\.id), ["o3", "o1"])
    }
    func testADismissedNoticeComesBackOnlyWhenSaidAgain() {
        var items = [user("u1"), notice("n1", "Rate limited, retrying", kind: "api_retry")]
        XCTAssertTrue(ChatNotices.current(items, dismissed: ["n1"]).isEmpty)
        items.append(notice("n2", "Retrying in 4 s", kind: "api_retry"))
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
