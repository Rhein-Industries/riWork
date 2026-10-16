import XCTest
@testable import RiWorkCore

/// The relay's elision shapes (`docs/chat-events.md`), as its own tests emit them, against the phone's decoder: a placeholder is read
/// as `.elided`, never dropped as unreadable and never as the event it stands for.
final class ChatElisionCompatibilityTests: XCTestCase {
    func testElisionEventIsReadAndStillAdvancesTheCursor() throws {
        let raw = #"{"chat_id":"11111111-2222-4333-8444-555555555555","events":[{"seq":1,"event":{"event":"item_elided","of":"item_completed","status":"completed","item_id":"tool","kind":"tool_call","reason":"too_large","bytes":2097301}},{"seq":2,"event":{"event":"state","state":{"state":"idle"}}}],"next":2,"more":false}"#
        let value = try JSONDecoder().decode(JSONValue.self, from: Data(raw.utf8))
        let request = try ChatEventsRequest(chatID: "11111111-2222-4333-8444-555555555555", since: 0, waitMilliseconds: 0)
        let page = try request.parse(value)
        XCTAssertEqual(page.next, 2)
        XCTAssertEqual(page.events.count, 2)
        XCTAssertEqual(page.events[0].seq, 1)
        XCTAssertEqual(page.events[0].event, .elided(ChatElidedEvent(event: "item_elided", of: "item_completed", itemID: "tool", kind: "tool_call", bytes: 2097301, status: .completed)))
        XCTAssertEqual(page.skipped, 0)
        var feed = ChatFeed()
        _ = feed.accept(page, since: 0)
        XCTAssertEqual(feed.next, 2)
        XCTAssertEqual(feed.skipped, 0)
        XCTAssertEqual(feed.transcript.item("tool")?.body, .elided(kind: "tool_call", bytes: 2097301))
        XCTAssertEqual(feed.transcript.state, .idle, "the event after the placeholder still arrives")
    }

    func testSnapshotElisionRetainsTheLegacyItemSchema() throws {
        let raw = #"{"v":1,"chat_id":"11111111-2222-4333-8444-555555555555","cursor":"1234-99-abcdef","next":99,"before":7,"more":false,"items":[{"order":7,"item":{"id":"tool","turn_id":"turn","status":"completed","body":{"type":"agent_message","text":"This message is too long to show here. Full text is on your Mac."},"elided":{"event":"item_elided","of":"item_completed","status":"completed","item_id":"tool","kind":"tool_call","reason":"too_large","bytes":2097301}}}],"controls":[]}"#
        let page = try JSONDecoder().decode(ChatSnapshotReply.self, from: Data(raw.utf8))
        XCTAssertEqual(page.next, 99)
        XCTAssertEqual(page.items.count, 1)
        XCTAssertEqual(page.items[0].item.id, "tool")
        XCTAssertEqual(page.items[0].item.turnID, "turn")
        XCTAssertEqual(page.skipped, 0)
    }
}

extension ChatElisionCompatibilityTests {
    func testElidedControlsCannotBecomeAnswerableApprovalsOrQuestions() throws {
        for original in ["approval_requested", "question_requested"] {
            let control = "{\"event\":\"control_elided\",\"of\":\"\(original)\",\"reason\":\"too_large\",\"bytes\":2097301,\"elided\":true,\"approval\":{\"request_id\":\"r\",\"item_id\":\"tool\",\"elided\":true},\"question\":{\"request_id\":\"q\",\"elided\":true}}"
            let raw = "{\"chat_id\":\"11111111-2222-4333-8444-555555555555\",\"events\":[{\"seq\":1,\"event\":\(control)}],\"next\":1,\"more\":false}"
            let value = try JSONDecoder().decode(JSONValue.self, from: Data(raw.utf8))
            let request = try ChatEventsRequest(chatID: "11111111-2222-4333-8444-555555555555", since: 0, waitMilliseconds: 0)
            let reply = try request.parse(value)
            // Read as the placeholder, never as the request it stands for.
            guard case .elided(let elided)? = reply.events[0].event else { XCTFail("\(original): \(String(describing: reply.events[0].event))"); continue }
            XCTAssertEqual(elided.of, original)
            XCTAssertTrue(elided.isRequest)
            XCTAssertEqual(reply.next, 1)
            var feed = ChatFeed()
            _ = feed.accept(reply, since: 0)
            XCTAssertEqual(feed.next, 1)
            XCTAssertTrue(feed.transcript.approvals.isEmpty)
            XCTAssertTrue(feed.transcript.questions.isEmpty)
            // It waits on the Mac, with nothing to answer here.
            XCTAssertEqual(feed.transcript.elidedRequests.map(\.question), [original == "question_requested"])
            let snapshot = "{\"v\":1,\"chat_id\":\"11111111-2222-4333-8444-555555555555\",\"cursor\":\"1234-99-abcdef\",\"next\":99,\"before\":0,\"more\":false,\"items\":[],\"controls\":[\(control)]}"
            let decoded = try JSONDecoder().decode(ChatSnapshotReply.self, from: Data(snapshot.utf8))
            XCTAssertEqual(decoded.skipped, 0)
            guard decoded.controls.count == 1, case .elided(let kept) = decoded.controls[0] else { XCTFail("\(original): \(decoded.controls)"); continue }
            XCTAssertEqual(kept.of, original)
            var installed = ChatFeed()
            installed.install(decoded)
            XCTAssertTrue(installed.transcript.approvals.isEmpty)
            XCTAssertTrue(installed.transcript.questions.isEmpty)
            XCTAssertEqual(installed.transcript.elidedRequests.map(\.question), [original == "question_requested"])
        }
    }
}


extension ChatElisionCompatibilityTests {
    func testLiveElidedToolKeepsGroupingHierarchyAndLifecycleHints() throws {
        let raw = #"{"event":"item_elided","of":"item_completed","item_id":"read","turn_id":"turn","parent_id":"worker","tool_kind":"read","started_at":100,"ended_at":250,"status":"completed","kind":"tool_call","bytes":10000}"#
        let event = try JSONDecoder().decode(ChatEvent.self, from: Data(raw.utf8))
        XCTAssertEqual(try JSONDecoder().decode(ChatEvent.self, from: JSONEncoder().encode(event)), event)
        var feed = ChatFeed()
        feed.accept(ChatEventsReply(chatID: "c", events: [ChatEnvelope(seq: 5, event: event)], next: 5, more: false), since: 0)
        let item = try XCTUnwrap(feed.transcript.item("read"))
        XCTAssertEqual(item.turnID, "turn")
        XCTAssertEqual(item.presentation.parentID, "worker")
        XCTAssertEqual(item.presentation.semantics?.kind, .read)
        XCTAssertEqual(item.startedAt, 100); XCTAssertEqual(item.endedAt, 250)
        XCTAssertEqual(item.status, .completed)
        XCTAssertEqual(item.body, .elided(kind: "tool_call", bytes: 10000))
        XCTAssertEqual(feed.skipped, 0)
    }
    func testMalformedElidedHintsDoNotDropTheItemAndUnknownKindsBecomeOther() throws {
        let raw = #"{"event":"item_elided","of":"item_started","item_id":"future","turn_id":17,"parent_id":false,"tool_kind":"future_kind","started_at":"elided","ended_at":-1,"status":"in_progress","kind":"tool_call"}"#
        let event = try JSONDecoder().decode(ChatEvent.self, from: Data(raw.utf8))
        guard case .itemCompleted(let item) = event.resolved(seq: 6) else { return XCTFail("elided item") }
        XCTAssertEqual(item.id, "future"); XCTAssertEqual(item.status, .inProgress)
        XCTAssertEqual(item.presentation.semantics?.kind, .other)
        XCTAssertNil(item.turnID); XCTAssertNil(item.presentation.parentID)
        XCTAssertNil(item.startedAt); XCTAssertNil(item.endedAt)
    }
}
