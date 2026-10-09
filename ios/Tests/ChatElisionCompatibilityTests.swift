import XCTest
@testable import RiWorkCore

/// Verify the existing decoder against relay recovery without adding client behavior.
final class ChatElisionCompatibilityTests: XCTestCase {
    func testUnknownElisionEventStillAdvancesTheLegacyCursor() throws {
        let raw = #"{"chat_id":"11111111-2222-4333-8444-555555555555","events":[{"seq":1,"event":{"event":"item_elided","item_id":"tool","kind":"tool_call","reason":"too_large","bytes":2097301}},{"seq":2,"event":{"event":"state","state":{"state":"idle"}}}],"next":2,"more":false}"#
        let value = try JSONDecoder().decode(JSONValue.self, from: Data(raw.utf8))
        let request = try ChatEventsRequest(chatID: "11111111-2222-4333-8444-555555555555", since: 0, waitMilliseconds: 0)
        let page = try request.parse(value)
        XCTAssertEqual(page.next, 2)
        XCTAssertEqual(page.events.count, 2)
        XCTAssertEqual(page.events[0].seq, 1)
        XCTAssertNil(page.events[0].event)
        XCTAssertEqual(page.skipped, 1)
        var feed = ChatFeed()
        _ = feed.accept(page, since: 0)
        XCTAssertEqual(feed.next, 2)
    }

    func testSnapshotElisionRetainsTheLegacyItemSchema() throws {
        let raw = #"{"v":1,"chat_id":"11111111-2222-4333-8444-555555555555","cursor":"1234-99-abcdef","next":99,"before":7,"more":false,"items":[{"order":7,"item":{"id":"tool","turn_id":"turn","status":"completed","body":{"type":"agent_message","text":"This message is too long to show here. Full text is on your Mac."},"elided":{"event":"item_elided","item_id":"tool","kind":"tool_call","reason":"too_large","bytes":2097301}}}],"controls":[]}"#
        let page = try JSONDecoder().decode(ChatSnapshotReply.self, from: Data(raw.utf8))
        XCTAssertEqual(page.next, 99)
        XCTAssertEqual(page.items.count, 1)
        XCTAssertEqual(page.items[0].item.id, "tool")
        XCTAssertEqual(page.items[0].item.turnID, "turn")
        XCTAssertEqual(page.skipped, 0)
    }
}
