import XCTest
@testable import RiWorkCore

/// `ChatTranscript.apply` against `Transcript::apply` of `src/chat/model.rs`: the first three tests are the Rust tests, ported.
final class ChatTranscriptTests: XCTestCase {
    private func agent(_ id: String, _ text: String, _ status: ChatItemStatus, turn: String? = "t1") -> ChatItem {
        ChatItem(id: id, turnID: turn, status: status, body: .agentMessage(text))
    }
    private func command(_ id: String, output: String = "", status: ChatItemStatus = .inProgress, turn: String? = "t1") -> ChatItem {
        ChatItem(id: id, turnID: turn, status: status, body: .command(command: "ls", cwd: nil, output: output, exitCode: nil))
    }
    private func approval(_ id: String, _ choices: [ChatDecision] = [.accept, .decline]) -> ChatApproval {
        ChatApproval(requestID: id, kind: .command, title: "rm -rf build", choices: choices)
    }
    private func chat(state: ChatState = .idle) -> ChatInfo { ChatInfo(id: "c", provider: .codex, title: "t", state: state) }
    private func folded(_ events: [ChatEvent]) -> ChatTranscript {
        var transcript = ChatTranscript()
        for event in events { transcript.apply(event) }
        return transcript
    }

    // MARK: The Rust tests

    func testDeltasBuildAnItemAndItsCompletionReplacesIt() {
        var t = ChatTranscript()
        t.apply(.turnStarted(turnID: "t1"))
        t.apply(.itemStarted(agent("a", "", .inProgress)))
        for part in ["Hel", "lo"] { t.apply(.itemDelta(itemID: "a", delta: .text(part))) }
        XCTAssertEqual(t.items[0].body, .agentMessage("Hello"))
        t.apply(.itemCompleted(agent("a", "Hello!", .completed)))
        XCTAssertEqual(t.items.count, 1)
        XCTAssertEqual(t.items[0], agent("a", "Hello!", .completed))
    }
    func testAFinishedTurnClosesWhatItLeftOpenAndItsRequests() {
        var t = ChatTranscript()
        t.apply(.turnStarted(turnID: "t1"))
        t.apply(.itemStarted(agent("a", "…", .inProgress)))
        t.apply(.approvalRequested(approval("r1", [.accept, .decline])))
        XCTAssertEqual(t.approvals.count, 1)
        t.apply(.turnCompleted(turnID: "t1", outcome: .interrupted))
        XCTAssertEqual(t.items[0].status, .interrupted)
        XCTAssertTrue(t.approvals.isEmpty)
        XCTAssertNil(t.turnID)
    }
    func testEventsRoundTripThroughJSON() throws {
        let events: [ChatEvent] = [
            .itemStarted(command("c")),
            .itemDelta(itemID: "c", delta: .output("a\n")),
            .state(.failed("gone"))
        ]
        for event in events {
            let line = try JSONEncoder().encode(event)
            XCTAssertEqual(try JSONDecoder().decode(ChatEvent.self, from: line), event, String(decoding: line, as: UTF8.self))
        }
        let approve = ChatCommand.approve(requestID: "r", decision: .acceptForSession)
        XCTAssertEqual(try JSONDecoder().decode(ChatCommand.self, from: JSONEncoder().encode(approve)), approve)
    }

    // MARK: Items

    func testAnItemStartedAgainIsReplacedInPlaceNotAppended() {
        var t = ChatTranscript()
        t.apply(.itemStarted(agent("a", "one", .inProgress)))
        t.apply(.itemStarted(agent("b", "two", .inProgress)))
        t.apply(.itemStarted(agent("a", "uno", .inProgress)))
        t.apply(.itemCompleted(agent("b", "dos", .completed)))
        XCTAssertEqual(t.items.map(\.id), ["a", "b"])
        XCTAssertEqual(t.items.map(\.body), [.agentMessage("uno"), .agentMessage("dos")])
        XCTAssertEqual(t.item("b")?.status, .completed)
        XCTAssertNil(t.item("zzz"))
    }
    func testACommandsOutputGrowsAndReasoningStreamsLikeAMessage() {
        var t = ChatTranscript()
        t.apply(.itemStarted(command("c")))
        t.apply(.itemStarted(ChatItem(id: "r", body: .reasoning("Th"))))
        t.apply(.itemDelta(itemID: "c", delta: .output("a\n")))
        t.apply(.itemDelta(itemID: "c", delta: .output("b\n")))
        t.apply(.itemDelta(itemID: "r", delta: .text("ink")))
        XCTAssertEqual(t.items[0].body, .command(command: "ls", cwd: nil, output: "a\nb\n", exitCode: nil))
        XCTAssertEqual(t.items[1].body, .reasoning("Think"))
    }
    func testADeltaThatDoesNotFitItsItemOrFindsNoItemChangesNothing() {
        var t = ChatTranscript()
        t.apply(.itemStarted(command("c")))
        t.apply(.itemStarted(agent("a", "x", .inProgress)))
        t.apply(.itemStarted(ChatItem(id: "u", body: .userMessage("hi"))))
        let before = t
        t.apply(.itemDelta(itemID: "c", delta: .text("text for a command")))
        t.apply(.itemDelta(itemID: "a", delta: .output("output for a message")))
        t.apply(.itemDelta(itemID: "u", delta: .text("more user")))
        t.apply(.itemDelta(itemID: "missing", delta: .text("nobody")))
        XCTAssertEqual(t, before)
    }
    func testADeltaBeforeItsItemIsLostNotKept() {
        var t = ChatTranscript()
        t.apply(.itemDelta(itemID: "a", delta: .text("early")))
        t.apply(.itemStarted(agent("a", "", .inProgress)))
        XCTAssertEqual(t.items[0].body, .agentMessage(""))
    }

    // MARK: Turns

    func testATurnThatEndsClosesItsOwnItemsAndThoseWithoutATurnButNotAnotherTurnsOrFinishedOnes() {
        var t = ChatTranscript()
        t.apply(.itemStarted(agent("mine", "", .inProgress, turn: "t1")))
        t.apply(.itemStarted(agent("loose", "", .inProgress, turn: nil)))
        t.apply(.itemStarted(agent("theirs", "", .inProgress, turn: "t2")))
        t.apply(.itemStarted(agent("done", "ok", .completed, turn: "t1")))
        t.apply(.itemStarted(command("declined", status: .declined, turn: "t1")))
        t.apply(.turnCompleted(turnID: "t1", outcome: .failed("boom")))
        XCTAssertEqual(t.items.map(\.status), [.failed, .failed, .inProgress, .completed, .declined])
    }
    func testEachOutcomeClosesOpenItemsItsOwnWay() {
        for (outcome, status) in [(ChatTurnOutcome.completed, ChatItemStatus.completed), (.interrupted, .interrupted), (.failed("x"), .failed)] {
            let t = folded([.itemStarted(agent("a", "", .inProgress)), .turnCompleted(turnID: "t1", outcome: outcome)])
            XCTAssertEqual(t.items[0].status, status, "\(outcome)")
        }
    }
    func testTheTurnIDIsClearedOnlyByTheTurnThatSetIt() {
        var t = ChatTranscript()
        t.apply(.turnStarted(turnID: "t2"))
        t.apply(.turnCompleted(turnID: "t1", outcome: .completed))
        XCTAssertEqual(t.turnID, "t2", "an old turn finishing does not end the new one")
        t.apply(.turnCompleted(turnID: "t2", outcome: .completed))
        XCTAssertNil(t.turnID)
    }
    func testEveryEndOfATurnClearsTheRequestsEvenForAnotherTurn() {
        var t = ChatTranscript()
        t.apply(.approvalRequested(approval("r1")))
        t.apply(.questionRequested(ChatQuestion(requestID: "q1", questions: [ChatQuestionPrompt(question: "?")])))
        t.apply(.turnCompleted(turnID: "other", outcome: .completed))
        XCTAssertTrue(t.approvals.isEmpty); XCTAssertTrue(t.questions.isEmpty)
    }

    // MARK: Requests

    func testRequestsKeepTheirArrivalOrderAndARepeatedRequestReplacesItself() {
        var t = ChatTranscript()
        t.apply(.approvalRequested(approval("r1")))
        t.apply(.approvalRequested(approval("r2")))
        t.apply(.approvalRequested(ChatApproval(requestID: "r1", kind: .tool, title: "again", choices: [.accept])))
        XCTAssertEqual(t.approvals.map(\.requestID), ["r2", "r1"], "the repeat goes to the back, as `retain` then `push` does")
        XCTAssertEqual(t.approvals.last?.title, "again")
        t.apply(.approvalResolved(requestID: "r2", decision: .decline))
        XCTAssertEqual(t.approvals.map(\.requestID), ["r1"])
        t.apply(.approvalResolved(requestID: "nobody", decision: .accept))
        XCTAssertEqual(t.approvals.count, 1)
    }
    func testQuestionsAreAskedAndAnsweredTheSameWay() {
        var t = ChatTranscript()
        let first = ChatQuestion(requestID: "q1", questions: [ChatQuestionPrompt(question: "one?")])
        t.apply(.questionRequested(first))
        t.apply(.questionRequested(ChatQuestion(requestID: "q2", questions: [])))
        t.apply(.questionRequested(first))
        XCTAssertEqual(t.questions.map(\.requestID), ["q2", "q1"])
        t.apply(.questionResolved(requestID: "q2"))
        XCTAssertEqual(t.questions, [first])
    }

    // MARK: Models

    private func option(_ id: String, fast: Bool = false) -> ChatModelOption { ChatModelOption(id: id, name: id.uppercased(), efforts: ["low", "high"], defaultEffort: "low", supportsFast: fast) }

    func testEachModelsEventReplacesTheListAndAnEmptyOneClearsIt() {
        var t = ChatTranscript()
        XCTAssertTrue(t.models.isEmpty, "an older driver, or an older desktop, never says")
        t.apply(.models([option("a"), option("b", fast: true)]))
        XCTAssertEqual(t.models.map(\.id), ["a", "b"])
        t.apply(.models([option("c")]))
        XCTAssertEqual(t.models.map(\.id), ["c"], "replaced, not added to")
        t.apply(.models([]))
        XCTAssertTrue(t.models.isEmpty)
    }
    func testTheModelsSurviveTheRestOfTheConversation() {
        var t = ChatTranscript()
        t.apply(.models([option("a")]))
        for event: ChatEvent in [.info(chat()), .state(.running), .turnStarted(turnID: "t1"), .itemStarted(agent("a", "x", .inProgress)), .turnCompleted(turnID: "t1", outcome: .completed), .usage(ChatUsage())] { t.apply(event) }
        XCTAssertEqual(t.models.map(\.id), ["a"])
        // An info that says Fast is on is the chat's word, and it replaces the info as before.
        var fast = chat(); fast.fast = true
        t.apply(.info(fast))
        XCTAssertEqual(t.info?.fast, true)
        XCTAssertEqual(t.models.map(\.id), ["a"])
    }
    func testAFeedFoldsAModelsEventLikeTheOthers() {
        var feed = ChatFeed()
        let reply = ChatEventsReply(chatID: "c", events: [ChatEnvelope(seq: 1, event: .models([option("a")])), ChatEnvelope(seq: 2, event: nil), ChatEnvelope(seq: 3, event: .models([option("b")]))], next: 3, more: false)
        feed.accept(reply, since: 0)
        XCTAssertEqual(feed.transcript.models.map(\.id), ["b"])
        feed.accept(reply, since: 0)
        XCTAssertEqual(feed.transcript.models.map(\.id), ["b"], "the same answer again changes nothing")
    }

    // MARK: State, info and usage

    func testStateAndInfoKeepEachOtherInStep() {
        var t = ChatTranscript()
        XCTAssertEqual(t.state, .starting)
        t.apply(.state(.running))
        XCTAssertEqual(t.state, .running)
        XCTAssertNil(t.info, "a state before any info has nothing to update")
        t.apply(.info(chat(state: .idle)))
        XCTAssertEqual(t.state, .idle, "an info carries the chat's state and replaces ours")
        t.apply(.state(.waiting))
        XCTAssertEqual(t.state, .waiting)
        XCTAssertEqual(t.info?.state, .waiting)
        var renamed = chat(state: .idle); renamed.title = "renamed"; renamed.approvalMode = .plan
        t.apply(.info(renamed))
        XCTAssertEqual(t.info?.title, "renamed"); XCTAssertEqual(t.info?.approvalMode, .plan); XCTAssertEqual(t.state, .idle)
    }
    func testUsageIsTheLatestOne() {
        var t = ChatTranscript()
        XCTAssertNil(t.usage)
        t.apply(.usage(ChatUsage(inputTokens: 1)))
        t.apply(.usage(ChatUsage(inputTokens: 5, costUSD: 0.5)))
        XCTAssertEqual(t.usage, ChatUsage(inputTokens: 5, costUSD: 0.5))
    }
    func testTheLastUserMessageIsWhatRetrySends() {
        var t = ChatTranscript()
        XCTAssertNil(t.lastUserMessage)
        t.apply(.itemStarted(ChatItem(id: "u1", status: .completed, body: .userMessage("first"))))
        t.apply(.itemStarted(agent("a", "answer", .completed)))
        t.apply(.itemStarted(ChatItem(id: "u2", status: .completed, body: .userMessage("second"))))
        t.apply(.itemStarted(agent("b", "answer", .completed)))
        XCTAssertEqual(t.lastUserMessage, "second")
    }

    // MARK: The whole fixture

    func testTheDesktopsOwnEventsFoldIntoTheTranscriptItDescribes() throws {
        #if SWIFT_PACKAGE
        let bundle = Bundle.module
        #else
        let bundle = Bundle(for: Self.self)
        #endif
        let url = try XCTUnwrap(bundle.url(forResource: "chat-serde", withExtension: "json", subdirectory: "Fixtures") ?? bundle.url(forResource: "chat-serde", withExtension: "json"))
        let events = try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: url))["events"].array.map { try $0.decode(ChatEvent.self) }
        let t = folded(events)
        // The last info (the minimal chat) is the one that stands, with its state, and its numbers are those of the last usage event.
        XCTAssertEqual(t.info?.provider, .claude)
        XCTAssertEqual(t.state, .starting)
        XCTAssertEqual(t.usage, ChatUsage())
        // The turn t1 started and ended three times before any item existed, so nothing was closed by it.
        XCTAssertNil(t.turnID)
        XCTAssertEqual(t.item("a")?.status, .inProgress)
        XCTAssertEqual(t.items.map(\.id), ["u", "a", "r", "p", "p2", "c", "c2", "f", "t", "t2", "w", "d", "k", "n1", "n2", "n3"])
        XCTAssertEqual(t.item("a")?.body, .agentMessage("Hel"), "the delta built the empty message")
        // The delta for “c” came before “c” itself: it is lost, as it is on the desktop, and the item starts empty.
        XCTAssertEqual(t.item("c")?.body, .command(command: "ls -la", cwd: "/tmp", output: "", exitCode: nil))
        // Approvals r1..r4 arrived after the turns ended, r1 was resolved, no turn ended since.
        XCTAssertEqual(t.approvals.map(\.requestID), ["r2", "r3", "r4"])
        XCTAssertTrue(t.questions.isEmpty, "asked and then resolved")
        // The fixture ends with three models events, each replacing the last: three models, then one, then none.
        XCTAssertTrue(t.models.isEmpty)
        XCTAssertEqual(folded(Array(events.prefix(40))).models.map(\.id), ["gpt-5.5", "gpt-5.4-mini", "bare"])
        XCTAssertEqual(folded(Array(events.prefix(41))).models.map(\.id), ["bare"])
    }
}

/// The cursor that keeps the phone's transcript in step with `chat.events`.
final class ChatFeedTests: XCTestCase {
    private func envelope(_ seq: UInt64, _ event: ChatEvent? = nil) -> ChatEnvelope {
        ChatEnvelope(seq: seq, event: event ?? .itemStarted(ChatItem(id: "i\(seq)", body: .agentMessage("m\(seq)"))))
    }
    private func reply(_ envelopes: [ChatEnvelope], next: UInt64? = nil, more: Bool = false) -> ChatEventsReply {
        ChatEventsReply(chatID: "c", events: envelopes, next: next ?? envelopes.last?.seq ?? 0, more: more)
    }

    func testAnswersAreFoldedInOrderAndMoveTheCursorOn() {
        var feed = ChatFeed()
        XCTAssertFalse(feed.loaded); XCTAssertEqual(feed.next, 0)
        XCTAssertEqual(feed.accept(reply([envelope(1), envelope(2)]), since: 0), .applied(2))
        XCTAssertEqual(feed.next, 2); XCTAssertTrue(feed.loaded)
        XCTAssertEqual(feed.accept(reply([envelope(3)]), since: 2), .applied(1))
        XCTAssertEqual(feed.transcript.items.map(\.id), ["i1", "i2", "i3"])
        XCTAssertEqual(feed.next, 3)
    }
    func testAnEmptyAnswerLoadsTheFeedAndKeepsTheCursor() {
        var feed = ChatFeed()
        XCTAssertEqual(feed.accept(reply([], next: 0), since: 0), .applied(0))
        XCTAssertTrue(feed.loaded); XCTAssertEqual(feed.next, 0)
        _ = feed.accept(reply([envelope(1)]), since: 0)
        XCTAssertEqual(feed.accept(reply([], next: 1), since: 1), .applied(0))
        XCTAssertEqual(feed.next, 1)
    }
    func testTheSameAnswerTakenTwiceChangesNothing() {
        var feed = ChatFeed()
        let page = reply([envelope(1), envelope(2)])
        feed.accept(page, since: 0)
        let once = feed
        XCTAssertEqual(feed.accept(page, since: 0), .applied(0), "a late answer to a request that was asked twice")
        XCTAssertEqual(feed, once)
    }
    func testAnOverlappingAnswerIsFoldedFromWhereItNewlyBegins() {
        var feed = ChatFeed()
        feed.accept(reply([envelope(1), envelope(2)]), since: 0)
        XCTAssertEqual(feed.accept(reply([envelope(2), envelope(3), envelope(4)]), since: 1), .applied(2))
        XCTAssertEqual(feed.transcript.items.map(\.id), ["i1", "i2", "i3", "i4"])
        XCTAssertEqual(feed.next, 4)
    }
    func testWhatThePhoneCannotReadIsCountedAndStillMovesTheCursor() {
        var feed = ChatFeed()
        XCTAssertEqual(feed.accept(reply([envelope(1), ChatEnvelope(seq: 2, event: nil), envelope(3)]), since: 0), .applied(2))
        XCTAssertEqual(feed.skipped, 1); XCTAssertEqual(feed.next, 3)
        XCTAssertEqual(feed.accept(reply([ChatEnvelope(seq: 4, event: nil)]), since: 3), .applied(0))
        XCTAssertEqual(feed.next, 4, "an unknown event is not asked for again")
    }
    func testACursorTheDesktopCutShortIsFollowedAndAPagedAnswerIsFlagged() {
        var feed = ChatFeed()
        let page = reply([envelope(1), envelope(2)], next: 2, more: true)
        XCTAssertTrue(page.more)
        feed.accept(page, since: 0)
        XCTAssertEqual(feed.next, 2)
        // The desktop says to continue from further on than the last event it sent.
        feed.accept(reply([], next: 9), since: 2)
        XCTAssertEqual(feed.next, 9)
    }
    func testNumbersThatGoBackwardsStartTheFeedOver() {
        var feed = ChatFeed()
        feed.accept(reply([envelope(1), envelope(2), envelope(3)]), since: 0)
        XCTAssertEqual(feed.accept(reply([], next: 0), since: 3), .restarted)
        XCTAssertEqual(feed, ChatFeed(), "the log was replaced: nothing held is trusted, and the next request asks from 0")
        XCTAssertEqual(feed.accept(reply([envelope(1)]), since: 0), .applied(1))
        XCTAssertEqual(feed.transcript.items.map(\.id), ["i1"])
    }
}


final class LatestFirstFeedTests: XCTestCase {
    private func row(_ n: UInt64, _ text: String = "base") -> ChatSnapshotRow {
        ChatSnapshotRow(order: n, item: ChatItem(id: "r\(n)", status: .inProgress, body: .agentMessage(text)))
    }
    private func page(_ rows: [ChatSnapshotRow], more: Bool = true, controls: [ChatEvent] = []) -> ChatSnapshotReply {
        ChatSnapshotReply(chatID: "c", cursor: "fixture", next: 10_000, before: rows.first?.order ?? 0, more: more, items: rows, controls: controls)
    }
    func testRecentBootstrapAndRepeatedHistoryNeverReplayControlsOrOverwriteLiveRows() {
        var feed = ChatFeed()
        feed.install(page([row(9999), row(10000)], controls: [.state(.waiting), .models([]), .usage(ChatUsage(inputTokens: 42))]))
        XCTAssertEqual(feed.next, 10_000)
        XCTAssertEqual(feed.transcript.items.count, 2)
        let live = ChatEventsReply(chatID: "c", events: [ChatEnvelope(seq: 10001, event: .itemDelta(itemID: "r9999", delta: .text("LIVE"))), ChatEnvelope(seq: 10002, event: .state(.running))], next: 10002, more: false)
        feed.accept(live, since: 10000)
        feed.accept(live, since: 10000)
        let older = page([row(9998), row(9999, "STALE")])
        feed.prepend(older, requestedBefore: 9999)
        feed.prepend(older, requestedBefore: 9999)
        XCTAssertEqual(feed.transcript.items.map(\.id), ["r9998", "r9999", "r10000"])
        XCTAssertEqual(feed.transcript.item("r9999")?.body, .agentMessage("baseLIVE"))
        XCTAssertEqual(feed.transcript.state, .running)
        XCTAssertEqual(feed.transcript.usage?.inputTokens, 42)
        XCTAssertEqual(feed.next, 10002)
        XCTAssertEqual(feed.itemArrivals, 2, "older rows and duplicate live pages are not unread arrivals")
    }
    func testUnknownOlderDeltaAndTurnCompletionFoldOnFullBaseWithoutDuplicates() {
        var feed = ChatFeed(); feed.install(page([row(10000)]))
        feed.accept(ChatEventsReply(chatID: "c", events: [ChatEnvelope(seq: 10001, event: .itemDelta(itemID: "r1", delta: .text("STREAM"))), ChatEnvelope(seq: 10002, event: .turnCompleted(turnID: "t", outcome: .completed))], next: 10002, more: false), since: 10000)
        feed.prepend(page([row(1)], more: false), requestedBefore: 10000)
        XCTAssertEqual(feed.transcript.item("r1")?.body, .agentMessage("baseSTREAM"))
        XCTAssertEqual(feed.transcript.item("r1")?.status, .completed)
        XCTAssertEqual(feed.transcript.items.count, 2)
    }
    func testHydrationOfAnOldItemUsesOriginalOrderAndLiveCompletionWinsHistory() {
        var feed = ChatFeed(); feed.install(page([row(10000)]))
        feed.hydrate(page([row(5)], more: false))
        feed.accept(ChatEventsReply(chatID: "c", events: [ChatEnvelope(seq: 10001, event: .itemCompleted(ChatItem(id: "r5", status: .completed, body: .agentMessage("final"))))], next: 10001, more: false), since: 10000)
        feed.prepend(page([row(5), row(6)], more: false), requestedBefore: 10000)
        XCTAssertEqual(feed.transcript.items.map(\.id), ["r5", "r6", "r10000"])
        XCTAssertEqual(feed.transcript.item("r5")?.body, .agentMessage("final"))
    }
}

extension LatestFirstFeedTests {
    func testInterleavedHistoryHydrationAndLiveArrivalsCountOnlyNewIDs() {
        var feed = ChatFeed(); feed.install(page([row(10000)]))
        feed.hydrate(page([row(5)], more: false))
        feed.accept(ChatEventsReply(chatID: "c", events: [ChatEnvelope(seq: 10001, event: .itemCompleted(row(5).item)), ChatEnvelope(seq: 10002, event: .itemStarted(row(10002).item)), ChatEnvelope(seq: 10003, event: .itemCompleted(row(10002).item))], next: 10003, more: false), since: 10000)
        XCTAssertEqual(feed.itemArrivals, 2)
        feed.prepend(page([row(5), row(6)], more: false), requestedBefore: 10000)
        XCTAssertEqual(feed.itemArrivals, 2)
        var sticky = StickyBottom(); sticky.contentChanged(end: 1, epoch: 0); sticky.stopFollowing(); sticky.contentChanged(end: feed.itemArrivals, epoch: 0)
        XCTAssertEqual(sticky.pill?.newLines, 1)
    }
    func testShortenedReplayRetainsBoundedRecoveryPolicy() {
        var feed = ChatFeed(); feed.install(page([row(10000)])); feed.beginDegradedReplay()
        feed.accept(ChatEventsReply(chatID: "c", events: [], next: 100, more: false), since: 0)
        XCTAssertEqual(feed.accept(ChatEventsReply(chatID: "c", events: [], next: 1, more: false), since: 100), .restarted)
        XCTAssertTrue(feed.degradedReplay)
        XCTAssertEqual(feed.next, 0)
        XCTAssertTrue(feed.transcript.items.isEmpty)
    }
    func testExceptionalReplayKeepsCurrentControlsThroughItsCheckpoint() {
        var feed = ChatFeed(); feed.install(page([row(10000)], controls: [.state(.waiting), .approvalRequested(ChatApproval(requestID: "current", kind: .command, title: "current", choices: [.accept])), .models([])]))
        feed.beginDegradedReplay()
        feed.accept(ChatEventsReply(chatID: "c", events: [ChatEnvelope(seq: 1, event: .state(.idle)), ChatEnvelope(seq: 2, event: .approvalResolved(requestID: "current", decision: .accept)), ChatEnvelope(seq: 3, event: .itemCompleted(row(5).item))], next: 3, more: true), since: 0)
        XCTAssertEqual(feed.transcript.state, .waiting)
        XCTAssertEqual(feed.transcript.approvals.first?.requestID, "current")
        XCTAssertEqual(feed.itemArrivals, 1)
        XCTAssertTrue(feed.degradedReplay)
        feed.accept(ChatEventsReply(chatID: "c", events: [ChatEnvelope(seq: 10001, event: .approvalResolved(requestID: "current", decision: .accept)), ChatEnvelope(seq: 10002, event: .state(.running))], next: 10002, more: false), since: 3)
        XCTAssertTrue(feed.transcript.approvals.isEmpty)
        XCTAssertEqual(feed.transcript.state, .running)
    }

    // MARK: What the relay leaves out as too large

    func testAnElidedItemIsARowThatSaysWhereItIsAndAnotherElidedEventANoteOfItsOwn() throws {
        let reply = try ChatEventsReply.parse(JSONDecoder().decode(JSONValue.self, from: Data("""
        {"chat_id":"c","next":4,"more":false,"events":[
          {"seq":1,"event":{"event":"item_started","item":{"id":"big","status":"in_progress","body":{"type":"command","command":"cat log"}}}},
          {"seq":2,"event":{"event":"item_elided","item_id":"big","kind":"command","reason":"too_large","bytes":1992294}},
          {"seq":3,"event":{"event":"approval_requested","elided":true,"reason":"too_large","bytes":300000}},
          {"seq":4,"event":{"event":"item_completed","item":{"id":"after","status":"completed","body":{"type":"agent_message","text":"Done"}}}}]}
        """.utf8)), chatID: "c")
        XCTAssertEqual(reply.skipped, 0, "nothing elided is dropped as unreadable")
        XCTAssertEqual(reply.events[1].event, .elided(event: "item_elided", itemID: "big", kind: "command", bytes: 1992294))
        XCTAssertEqual(reply.events[2].event, .elided(event: "approval_requested", itemID: nil, kind: nil, bytes: 300000))
        var feed = ChatFeed()
        XCTAssertEqual(feed.accept(reply, since: 0), .applied(4))
        XCTAssertEqual(feed.next, 4)
        XCTAssertEqual(feed.transcript.items.map(\.id), ["big", "elided-3", "after"], "the item keeps its place; the note takes the event's")
        XCTAssertEqual(feed.transcript.item("big")?.body, .elided(kind: "command", bytes: 1992294))
        XCTAssertEqual(feed.transcript.approvals, [], "an elided request is not shown as one that can be answered here")
        XCTAssertEqual(ChatElision(feed.transcript.item("big")!.body)?.line, "Shown on your Mac · command · 2 MB")
        XCTAssertEqual(ChatElision(feed.transcript.item("elided-3")!.body), ChatElision(.elided(kind: nil, bytes: 300000, event: "approval_requested")))
        XCTAssertEqual(ChatElision(.elided(kind: nil, bytes: 300000, event: "approval_requested"))?.line, "Too large to show here · approval requested · 300 KB")
        XCTAssertEqual(ChatElision(.elided(kind: "agent_message", bytes: nil))?.line, "Shown on your Mac · message")
        XCTAssertEqual(ChatElision(.elided(kind: "agent_message", bytes: nil))?.isNote, false)
        XCTAssertEqual(try JSONDecoder().decode(ChatEvent.self, from: Data(#"{"event":"item_elided","item_id":"x","bytes":1e100}"#.utf8)),
                       .elided(event: "item_elided", itemID: "x", kind: nil, bytes: nil), "a size out of range is no size, never a crash")
        // The same reply again changes nothing, and the elided body survives the snapshot's JSON.
        XCTAssertEqual(feed.accept(reply, since: 0), .applied(0))
        let item = try XCTUnwrap(feed.transcript.item("big"))
        XCTAssertEqual(try JSONDecoder().decode(ChatItem.self, from: JSONEncoder().encode(item)), item)
        for event in reply.events.compactMap(\.event) { XCTAssertEqual(try JSONDecoder().decode(ChatEvent.self, from: JSONEncoder().encode(event)), event) }
    }

    func testAnEventAnOlderRelayCannotSendIsPassedOverWithANote() {
        var feed = ChatFeed()
        feed.accept(ChatEventsReply(chatID: "c", events: [ChatEnvelope(seq: 1, event: .state(.running))], next: 1, more: false), since: 0)
        feed.skipOversized()
        XCTAssertEqual(feed.next, 2, "the next request asks after the event that could not be sent")
        XCTAssertEqual(ChatElision(feed.transcript.item("elided-2")!.body)?.line, "Too large to show here")
        feed.accept(ChatEventsReply(chatID: "c", events: [ChatEnvelope(seq: 3, event: .state(.idle))], next: 3, more: false), since: 2)
        XCTAssertEqual(feed.transcript.state, .idle, "what comes after it still arrives")
    }
}
