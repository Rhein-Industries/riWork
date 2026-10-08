import XCTest
@testable import RiWorkCore

final class ChatNoticesTests: XCTestCase {
    private func notice(_ id: String, _ text: String, _ level: ChatNoticeLevel = .warning, kind: String? = nil, resolved: Bool = false,
                        resetsAt: UInt64? = nil, dismissed: Bool = false) -> ChatItem {
        ChatItem(id: id, status: .completed, body: .notice(level: level, text: text, kind: kind, resolved: resolved, resetsAt: resetsAt, dismissed: dismissed))
    }
    private let now = Date(timeIntervalSince1970: 1_760_000_000)
    private func user(_ id: String) -> ChatItem { ChatItem(id: id, status: .completed, body: .userMessage("go")) }
    private func reply(_ id: String) -> ChatItem { ChatItem(id: id, status: .completed, body: .agentMessage("done")) }

    func testTheHostsKindGroupsAndANoticeWithoutOneIsItsOwnLine() {
        XCTAssertEqual(ChatProviderNotice.key(kind: "reconnecting", id: "a"), ChatProviderNotice.key(kind: "reconnecting", id: "b"))
        XCTAssertNotEqual(ChatProviderNotice.key(kind: nil, id: "a"), ChatProviderNotice.key(kind: nil, id: "b"), "no kind: each notice its own")
        XCTAssertNotEqual(ChatProviderNotice.key(kind: "rate_limit:five_hour", id: "x"), ChatProviderNotice.key(kind: "rate_limit:seven_day", id: "x"))
        XCTAssertNotEqual(ChatProviderNotice.key(kind: "api_retry", id: "api_retry"), ChatProviderNotice.key(kind: nil, id: "api_retry"), "a kind never meets an id")
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
        XCTAssertEqual(current.map(\.id), ["n5", "n6", "n3"], "the error first, then the newest; one per kind, an unknown kind as it is; a usage warning is the chip's")
        XCTAssertEqual(current.first { $0.id == "n3" }?.count, 2, "said twice in the chat")
        XCTAssertEqual(current.first { $0.id == "n3" }?.text, "Reconnecting… 2/5", "replaced in place, not stacked")
        // A new turn: what the provider said in the last one is history now.
        XCTAssertTrue(ChatNotices.current(items + [user("u2")], dismissed: []).isEmpty)
        XCTAssertEqual(ChatNotices.all(items + [user("u2")]).count, 6, "all of them stay in the history")
        // Without kinds (an older host) each notice is its own line.
        let old = [user("u"), notice("o1", "Reconnecting… 1/5"), notice("o2", "Reconnecting… 2/5"), notice("o3", "Reconnecting… 2/5")]
        XCTAssertEqual(ChatNotices.current(old, dismissed: []).map(\.id), ["o3", "o2", "o1"])
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

    func testTheNewFieldsAreReadAndToleratedAndOmittedWhenEmpty() throws {
        func decode(_ json: String) throws -> ChatItemBody { try JSONDecoder().decode(ChatItemBody.self, from: Data(json.utf8)) }
        XCTAssertEqual(try decode(#"{"type":"notice","level":"error","text":"Limit","kind":"rate_limit:five_hour","resolved":true,"dismissed":true,"resets_at":1767225600}"#),
                       .notice(level: .error, text: "Limit", kind: "rate_limit:five_hour", resolved: true, resetsAt: 1_767_225_600, dismissed: true))
        XCTAssertEqual(try decode(#"{"type":"notice","text":"x","resolved":"yes","resets_at":"soon","dismissed":1}"#), .notice(level: .info, text: "x"), "wrong types are absent")
        let encoded = try JSONEncoder().encode(ChatItemBody.notice(level: .info, text: "x"))
        let object = try JSONDecoder().decode(JSONValue.self, from: encoded)
        XCTAssertEqual(object["resolved"], .null); XCTAssertEqual(object["dismissed"], .null); XCTAssertEqual(object["resets_at"], .null)
    }
    func testAReachedLimitAndASignInAreStickyUntilResolvedResetOrDismissed() {
        let limit = notice("l1", "You've hit your 5-hour limit.", .error, kind: "rate_limit:five_hour", resetsAt: 1_760_003_600)
        let signIn = notice("s1", "Sign in again.", .error, kind: "auth_required")
        // Said turns ago: still shown, unlike an ordinary notice.
        let items = [user("u1"), limit, signIn, notice("r1", "Retrying", kind: "api_retry"), user("u2"), reply("a2")]
        XCTAssertEqual(Set(ChatNotices.current(items, dismissed: [], now: now).map(\.id)), ["l1", "s1"])
        XCTAssertEqual(ChatNotices.current(items, dismissed: [], now: now).first { $0.id == "l1" }?.isSticky, true)
        // Its reset has passed.
        XCTAssertEqual(ChatNotices.current(items, dismissed: [], now: now.addingTimeInterval(3600)).map(\.id), ["s1"])
        // Resolved (re-emitted with the same id).
        let resolved = items + [notice("l1", "Usage is available again.", .info, kind: "rate_limit:five_hour", resolved: true)]
        XCTAssertEqual(ChatNotices.current(resolved, dismissed: [], now: now).map(\.id), ["s1"])
        // Dismissed on another device: the item's flag, or the snapshot's key before the flag arrives.
        let flagged = items + [notice("l1", "You've hit your 5-hour limit.", .error, kind: "rate_limit:five_hour", resetsAt: 1_760_003_600, dismissed: true)]
        XCTAssertEqual(ChatNotices.current(flagged, dismissed: [], now: now).map(\.id), ["s1"])
        XCTAssertEqual(ChatNotices.current(items, dismissed: [], hostKeys: ["claude:default|rate_limit:five_hour@1760003600", "codex:a|auth_required#s1"], now: now).map(\.id), [])
        XCTAssertEqual(ChatNotices.current(items, dismissed: [], hostKeys: ["claude:default|rate_limit:five_hour@1760000001"], now: now).count, 2, "another reset is another occurrence")
        // A usage warning (an older log) is never a banner; an unknown window is still a usage limit.
        XCTAssertTrue(ChatNotices.current([notice("w", "Close to the limit", .warning, kind: "rate_limit:seven_day")], dismissed: [], now: now).isEmpty)
        let unknown = ChatNotices.current([user("u"), notice("x", "Limit", .error, kind: "rate_limit:new_window"), user("u2")], dismissed: [], now: now)
        XCTAssertEqual(unknown.map(\.id), ["x"])
    }
    func testAUsageLimitsBannerSaysWhenItResets() {
        var calendar = Calendar(identifier: .gregorian); calendar.timeZone = TimeZone(identifier: "Europe/Zurich")!
        let limit = ChatNotices.all([notice("l1", "You've hit your weekly limit.", .error, kind: "rate_limit:seven_day", resetsAt: 1_767_366_000)])[0]
        XCTAssertTrue(limit.isUsageLimit)
        XCTAssertEqual(limit.bannerText(calendar: calendar, locale: Locale(identifier: "en_GB")), "You've hit your weekly limit. · resets Fri 16:00")
        let other = ChatNotices.all([notice("n", "Signed out", .error, kind: "auth_required")])[0]
        XCTAssertEqual(other.bannerText(), "Signed out")
    }
}

final class ChatUsageLimitsTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_760_000_000)
    private var calendar: Calendar { var c = Calendar(identifier: .gregorian); c.timeZone = TimeZone(identifier: "Europe/Zurich")!; return c }
    private let locale = Locale(identifier: "en_GB")

    func testTheRateLimitsEventReplacesTheWindowsWholesale() throws {
        let json = #"{"event":"rate_limits","windows":[{"id":"five_hour","label":"5h","used_percent":30.0,"resets_at":1767225600,"warn_at":70.0},{"id":"seven_day","label":"weekly","used_percent":87,"resets_at":1767300000},{"id":"bad","used_percent":"x"}]}"#
        let event = try JSONDecoder().decode(ChatEvent.self, from: Data(json.utf8))
        guard case .rateLimits(let windows) = event else { return XCTFail("\(event)") }
        XCTAssertEqual(windows.map(\.id), ["five_hour", "seven_day"], "a window the phone cannot read is left out")
        XCTAssertEqual(windows[1].warnAt, 70, "Claude's threshold when none is sent")
        var transcript = ChatTranscript()
        transcript.apply(event)
        XCTAssertEqual(transcript.rateLimits.count, 2)
        transcript.apply(.rateLimits([]))
        XCTAssertTrue(transcript.rateLimits.isEmpty, "an empty list is no known windows")
        let encoded = try JSONEncoder().encode(ChatEvent.rateLimits(windows))
        XCTAssertEqual(try JSONDecoder().decode(ChatEvent.self, from: encoded), .rateLimits(windows))
    }
    func testTheChipIsTheFullestLiveWindowPastItsThresholdBoldFromNinety() {
        let five = ChatRateWindow(id: "five_hour", label: "5h", usedPercent: 72, resetsAt: 1_760_003_600, warnAt: 70)
        let weekly = ChatRateWindow(id: "seven_day", label: "weekly", usedPercent: 87, resetsAt: 1_760_259_600, warnAt: 70)
        let codex = ChatRateWindow(id: "primary", label: "5h", usedPercent: 74, warnAt: 75)
        let chip = ChatUsageLimits.chip([five, weekly, codex], now: now, calendar: calendar, locale: locale)
        XCTAssertEqual(chip?.text, "weekly 87%")
        XCTAssertEqual(chip?.resetText, "resets Sun 11:00")
        XCTAssertEqual(chip?.bold, false)
        XCTAssertNil(ChatUsageLimits.chip([codex, ChatRateWindow(id: "x", label: "weekly", usedPercent: 30)], now: now), "below each own threshold (Codex 75): no chip, even at 74")
        XCTAssertEqual(ChatUsageLimits.chip([ChatRateWindow(id: "s", label: "weekly", usedPercent: 90, warnAt: 70)], now: now)?.bold, true)
        let expired = ChatRateWindow(id: "old", label: "weekly", usedPercent: 99, resetsAt: 1_759_999_000)
        XCTAssertEqual(ChatUsageLimits.chip([expired, five], now: now)?.text, "5h 72%", "an expired window is hidden")
        XCTAssertEqual(ChatUsageLimits.live([expired, five, weekly, codex], now: now).map(\.id), ["seven_day", "primary", "five_hour"])
        XCTAssertNil(ChatUsageLimits.chip([ChatRateWindow(id: "u", label: "5h", usedPercent: 80)], now: now)?.resetText, "no reset known: none said")
    }
    func testDismissNoticeIsOneValidatedCommandAndTheSnapshotCarriesTheHostsDismissals() throws {
        let chat = "11111111-1111-4111-8111-111111111111"
        let request = try ChatCommandRequest(chatID: chat, command: .dismissNotice(itemID: "notice-1"))
        XCTAssertEqual(request.params["command"], .object(["command": .string("dismiss_notice"), "item_id": .string("notice-1")]))
        XCTAssertEqual(try ChatCommandRequest(params: request.params), request)
        for bad in ["", String(repeating: "x", count: 513), "a\nb"] { XCTAssertThrowsError(try ChatCommandRequest(chatID: chat, command: .dismissNotice(itemID: bad))) }
        let base = #"{"v":1,"chat_id":"\#(chat)","cursor":"c","next":3,"before":0,"more":false,"items":[],"controls":[]"#
        let old = try JSONDecoder().decode(ChatSnapshotReply.self, from: Data((base + "}").utf8))
        XCTAssertEqual(old.dismissedNotices, [], "an older host sends none")
        let new = try JSONDecoder().decode(ChatSnapshotReply.self, from: Data((base + #","dismissed_notices":["claude:default|rate_limit:seven_day@1760000000"]}"#).utf8))
        var feed = ChatFeed(); feed.install(new)
        XCTAssertEqual(feed.dismissedNotices, ["claude:default|rate_limit:seven_day@1760000000"])
    }
}

final class SnapshotToleranceTests: XCTestCase {
    /// A newer host's snapshot: a control event and an item type this phone has never heard of are skipped, not the snapshot.
    func testUnknownControlsAndItemsAreSkippedNotTheSnapshot() throws {
        let chat = "11111111-1111-4111-8111-111111111111"
        let json = #"""
        {"v":1,"chat_id":"\#(chat)","cursor":"c","next":9,"before":1,"more":false,"future_field":true,
         "items":[{"order":1,"item":{"id":"x","status":"completed","body":{"type":"hologram","text":"?"}}},
                  {"order":2,"item":{"id":"a","status":"completed","body":{"type":"agent_message","text":"hi"}}}],
         "controls":[{"event":"state","state":{"state":"idle"}},{"event":"brand_new_control","payload":[1,2]},
                     {"event":"rate_limits","windows":[{"id":"five_hour","label":"5h","used_percent":80}]}]}
        """#
        let reply = try JSONDecoder().decode(ChatSnapshotReply.self, from: Data(json.utf8))
        XCTAssertEqual(reply.items.map(\.item.id), ["a"])
        XCTAssertEqual(reply.controls.count, 2, "state and rate_limits; the unknown control is left out")
        XCTAssertEqual(reply.skipped, 2)
        var feed = ChatFeed(); feed.install(reply)
        XCTAssertEqual(feed.transcript.rateLimits.map(\.id), ["five_hour"])
        XCTAssertEqual(feed.transcript.items.map(\.id), ["a"])
        // A single unknown event in chat.events is skipped as before.
        let envelope = try JSONDecoder().decode(ChatEnvelope.self, from: Data(#"{"seq":3,"event":{"event":"brand_new_control"}}"#.utf8))
        XCTAssertNil(envelope.event)
    }
}

private actor SnapshotTransport: RemoteTransport {
    let reply: JSONValue
    var sent: [[String: JSONValue]] = []
    init(_ reply: JSONValue) { self.reply = reply }
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { pairing }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        try RequestValidation.validate(method: method, params: params, id: id)
        sent.append(params); return reply
    }
    func disconnect() async {}
    func isConnected() async -> Bool { true }
}
extension SnapshotToleranceTests {
    /// A history page of only newer item types passes the transport's checks and moves on to the host's `before`; both reads opt in to
    /// the usage windows.
    func testAnAllUnknownPageAdvancesThroughTheTransportAndBothReadsOptInToRateLimits() async throws {
        let chat = "11111111-1111-4111-8111-111111111111"
        let page = try JSONDecoder().decode(JSONValue.self, from: Data(#"""
        {"v":1,"chat_id":"\#(chat)","cursor":"c0ffee","next":90,"before":40,"more":true,"controls":[],
         "items":[{"order":40,"item":{"id":"x1","status":"completed","body":{"type":"hologram"}}},{"order":41,"item":{"id":"x2","status":"completed","body":{"type":"hologram"}}}]}
        """#.utf8))
        let transport = SnapshotTransport(page)
        let reply = try await transport.chatSnapshot(chatID: chat, cursor: "c0ffee", before: 60)
        XCTAssertTrue(reply.items.isEmpty); XCTAssertEqual(reply.skipped, 2); XCTAssertEqual(reply.before, 40); XCTAssertTrue(reply.more)
        var feed = ChatFeed()
        feed.install(ChatSnapshotReply(chatID: chat, cursor: "c0ffee", next: 90, before: 60, more: true, items: [], controls: []))
        feed.prepend(reply, requestedBefore: 60)
        XCTAssertEqual(feed.before, 40, "the next page is asked for before 40")
        XCTAssertTrue(feed.hasOlder)
        let sent = await transport.sent
        XCTAssertEqual(sent.first?["features"], .array([.string("rate_limits")]))
        XCTAssertEqual(try ChatEventsRequest(chatID: chat, since: 0, waitMilliseconds: 0).params["features"], .array([.string("rate_limits")]))
    }
}
