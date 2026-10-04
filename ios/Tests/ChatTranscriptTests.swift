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
