import XCTest
@testable import RiWorkCore

/// The chat vocabulary against the desktop's own serde output. `Fixtures/chat-serde.json` is written by `scripts/gen-chat-fixtures.sh` from
/// the real types in `src/chat/model.rs`, so what is decoded here is what the host sends and what is encoded here is what it accepts.
final class ChatTests: XCTestCase {
    private func fixture() throws -> JSONValue {
        #if SWIFT_PACKAGE
        let bundle = Bundle.module
        #else
        let bundle = Bundle(for: Self.self)
        #endif
        let url = try XCTUnwrap(bundle.url(forResource: "chat-serde", withExtension: "json", subdirectory: "Fixtures") ?? bundle.url(forResource: "chat-serde", withExtension: "json"))
        return try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: url))
    }
    private func decode<T: Decodable>(_ type: T.Type = T.self, _ json: String) throws -> T { try JSONDecoder().decode(type, from: Data(json.utf8)) }
    private func wire<T: Encodable>(_ value: T) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(value)) }
    private func value(_ json: String) throws -> JSONValue { try decode(JSONValue.self, json) }

    // MARK: The desktop's serde forms

    func testEveryEventTheDesktopWritesDecodesAndEncodesBackToTheSameJSON() throws {
        let events = try fixture()["events"].array
        XCTAssertGreaterThan(events.count, 30, "the fixture covers every event, item, delta and outcome")
        for original in events {
            let event = try original.decode(ChatEvent.self)
            XCTAssertEqual(try wire(event), original, "\(original)")
        }
    }
    func testTheFixtureCoversEveryKindOfEventAndItem() throws {
        let events = try fixture()["events"].array
        XCTAssertEqual(Set(events.compactMap { $0["event"].string }),
                       ["info", "state", "turn_started", "turn_completed", "item_started", "item_delta", "item_completed", "approval_requested",
                        "approval_resolved", "question_requested", "question_resolved", "usage"])
        let bodies = events.compactMap { event -> String? in event["item"]["body"]["type"].string }
        XCTAssertEqual(Set(bodies), ["user_message", "agent_message", "reasoning", "plan", "command", "file_change", "tool_call", "web_search", "todo", "compaction", "notice"])
        XCTAssertEqual(Set(events.compactMap { $0["delta"]["kind"].string }), ["text", "output"])
    }
    func testEveryCommandWeSendEncodesToTheFormTheDesktopWrites() throws {
        let commands = try fixture()["commands"].array
        XCTAssertEqual(commands.count, 10)
        for original in commands {
            let command = try original.decode(ChatCommand.self)
            XCTAssertEqual(try wire(command), original, "\(original)")
        }
        // Spelled out, because the desktop denies unknown fields: nothing extra, and an unset field is absent rather than null.
        XCTAssertEqual(try wire(ChatCommand.configure(approvalMode: .plan)), try value(#"{"command":"configure","approval_mode":"plan"}"#))
        XCTAssertEqual(try wire(ChatCommand.configure()), try value(#"{"command":"configure"}"#))
        XCTAssertEqual(try wire(ChatCommand.approve(requestID: "r", decision: .acceptForSession)), try value(#"{"command":"approve","request_id":"r","decision":"accept_for_session"}"#))
        XCTAssertEqual(try wire(ChatCommand.answer(requestID: "q", answers: [["A"], []])), try value(#"{"command":"answer","request_id":"q","answers":[["A"],[]]}"#))
        XCTAssertEqual(try wire(ChatCommand.send(text: "hi")), try value(#"{"command":"send","text":"hi"}"#))
        XCTAssertEqual(try wire(ChatCommand.interrupt), try value(#"{"command":"interrupt"}"#))
    }
    func testChatInfosRoundTripWithOptionalFieldsAbsent() throws {
        let chats = try fixture()["chats"].array
        for original in chats { XCTAssertEqual(try wire(try original.decode(ChatInfo.self)), original) }
        let full = try chats[0].decode(ChatInfo.self)
        XCTAssertEqual(full.provider, .codex)
        XCTAssertEqual(full.projectID, "22222222-2222-4222-8222-222222222222")
        XCTAssertNil(full.worktreeID)
        XCTAssertEqual(full.approvalMode, .autoEdit)
        XCTAssertEqual(full.state, .failed("gone"))
        XCTAssertEqual(full.createdAtUnix, 1_790_000_000)
        XCTAssertEqual(full.model, "gpt-5"); XCTAssertEqual(full.effort, "high"); XCTAssertEqual(full.codexAccountID, "acct")
        let minimal = try chats[1].decode(ChatInfo.self)
        XCTAssertEqual(minimal.provider, .claude)
        XCTAssertNil(minimal.projectID)
        XCTAssertEqual(minimal.worktreeID, "44444444-4444-4444-8444-444444444444")
        XCTAssertEqual(minimal.approvalMode, .supervised)
        XCTAssertEqual(minimal.state, .starting)
        XCTAssertNil(minimal.model); XCTAssertNil(minimal.providerThreadID)
    }
    func testTheWordsOfTheEnumsAreTheDesktops() throws {
        let doc = try fixture()
        XCTAssertEqual(ChatApprovalMode.allCases.map(\.rawValue), doc["modes"].array.compactMap(\.string))
        XCTAssertEqual(doc["modes"].array.compactMap(\.string), ["supervised", "auto_edit", "full", "plan"])
        XCTAssertEqual(ChatProvider.allCases.map(\.rawValue), doc["providers"].array.compactMap(\.string))
        XCTAssertEqual(ChatApprovalMode.allCases.map(\.title), ["Supervised", "Auto-edit", "Full", "Plan"])
        XCTAssertEqual(ChatProvider.codex.chatTitle, "Codex chat")
        XCTAssertEqual(ChatProvider.claude.chatTitle, "Claude chat", "never “Claude Code”")
    }
    func testWhatEachEventCarriesIsReadExactly() throws {
        let events = try fixture()["events"].array.map { try $0.decode(ChatEvent.self) }
        XCTAssertEqual(events[1], .state(.starting))
        XCTAssertEqual(events[6], .state(.failed("gone")))
        XCTAssertEqual(events[8], .turnCompleted(turnID: "t1", outcome: .completed))
        XCTAssertEqual(events[10], .turnCompleted(turnID: "t1", outcome: .failed("boom")))
        XCTAssertEqual(events[13], .itemDelta(itemID: "a", delta: .text("Hel")))
        XCTAssertEqual(events[14], .itemDelta(itemID: "c", delta: .output("a\n")))
        guard case .itemCompleted(let plan) = events[16], case .plan(let explanation, let steps) = plan.body else { return XCTFail("a plan") }
        XCTAssertNil(explanation, "serde writes null for the missing explanation")
        XCTAssertEqual(steps, [ChatStep(text: "one", status: .inProgress), ChatStep(text: "two", status: .pending), ChatStep(text: "three", status: .completed)])
        guard case .itemCompleted(let failed) = events[19], case .command(let command, let cwd, let output, let exitCode) = failed.body else { return XCTFail("a command") }
        XCTAssertEqual([command, output], ["false", "x"]); XCTAssertNil(cwd); XCTAssertEqual(exitCode, 1); XCTAssertEqual(failed.status, .failed)
        guard case .itemCompleted(let files) = events[20], case .fileChange(let changes) = files.body else { return XCTFail("a file change") }
        XCTAssertEqual(changes.map(\.kind), [.modify, .add, .delete, .rename])
        XCTAssertEqual(changes[0].diff, "@@ -1 +1 @@\n-a\n+b\n"); XCTAssertNil(changes[1].diff)
        guard case .itemCompleted(let tool) = events[21], case .toolCall(let server, let name, let input, let toolOutput) = tool.body else { return XCTFail("a tool") }
        XCTAssertEqual(server, "cua"); XCTAssertEqual(name, "click"); XCTAssertEqual(toolOutput, "done")
        XCTAssertEqual(input["y"].array, [.number(2), .number(3)])
        guard case .itemCompleted(let bare) = events[22], case .toolCall(let noServer, _, let noInput, let noOutput) = bare.body else { return XCTFail("a bare tool") }
        XCTAssertNil(noServer); XCTAssertEqual(noInput, .null); XCTAssertNil(noOutput); XCTAssertEqual(bare.status, .declined)
        guard case .approvalRequested(let approval) = events[29] else { return XCTFail("an approval") }
        XCTAssertEqual(approval.choices, [.accept, .acceptForSession, .decline, .cancel])
        XCTAssertEqual([approval.requestID, approval.itemID, approval.title, approval.detail, approval.kind.rawValue], ["r1", "c", "rm -rf build", "why", "command"])
        guard case .questionRequested(let question) = events[34] else { return XCTFail("a question") }
        XCTAssertEqual(question.questions.count, 2)
        XCTAssertEqual(question.questions[0].options, [ChatQuestionOption(label: "A", description: "first"), ChatQuestionOption(label: "B")])
        XCTAssertTrue(question.questions[0].multiSelect); XCTAssertEqual(question.questions[0].header, "Pick")
        XCTAssertNil(question.questions[1].header); XCTAssertFalse(question.questions[1].multiSelect)
        guard case .usage(let usage) = events[36] else { return XCTFail("usage") }
        XCTAssertEqual(usage, ChatUsage(inputTokens: 1200, outputTokens: 300, cachedInputTokens: 100, contextWindow: 200_000, contextUsed: 84_000, costUSD: 0.4234))
        guard case .usage(let empty) = events[37] else { return XCTFail("empty usage") }
        XCTAssertEqual(empty, ChatUsage())
    }

    // MARK: Leniency

    func testAnUnknownEventItemOrDeltaIsNotReadAndTheOthersAre() throws {
        for json in [
            #"{"event":"hologram","x":1}"#,
            #"{"event":"item_started","item":{"id":"z","status":"completed","body":{"type":"hologram"}}}"#,
            #"{"event":"item_delta","item_id":"a","delta":{"kind":"smell","text":"x"}}"#,
            #"{"event":"state","state":"idle"}"#,
            #"{"nothing":true}"#
        ] { XCTAssertThrowsError(try decode(ChatEvent.self, json), json) }

        let result = try value("""
        {"chat_id":"c","next":7,"more":false,"events":[
          {"seq":1,"event":{"event":"state","state":{"state":"idle"}}},
          {"seq":2,"event":{"event":"hologram"}},
          {"seq":3,"event":{"event":"item_started","item":{"id":"z","status":"completed","body":{"type":"hologram"}}}},
          {"seq":4,"event":{"event":"item_delta","item_id":"a","delta":{"kind":"smell","text":"x"}}},
          {"seq":5,"event":"not even an object"},
          {"seq":6,"event":{"event":"turn_started","turn_id":"t"}},
          {"event":{"event":"turn_started","turn_id":"no seq"}},
          "junk",
          {"seq":7,"event":{"event":"usage","usage":{"input_tokens":1,"output_tokens":2}}}
        ]}
        """)
        let reply = try ChatEventsReply.parse(result, chatID: "c")
        XCTAssertEqual(reply.events.map(\.seq), [1, 2, 3, 4, 5, 6, 7], "an entry that is no {seq, event} at all is dropped; the rest keep their numbers")
        XCTAssertEqual(reply.events.map { $0.event != nil }, [true, false, false, false, false, true, true])
        XCTAssertEqual(reply.skipped, 4)
        XCTAssertEqual(reply.next, 7)
    }
    func testAWordThePhoneDoesNotKnowInsideAKnownThingIsReadAsItsNearestHarmlessMeaning() throws {
        let info = try decode(ChatInfo.self, #"{"id":"i","provider":"codex","cwd":"/","title":"t","created_at_unix":1,"approval_mode":"yolo","state":{"state":"hibernating"}}"#)
        XCTAssertEqual(info.approvalMode, .supervised)
        XCTAssertEqual(info.state, .unknown("hibernating"))
        XCTAssertEqual(info.state.activity, .unknown)
        // A decision it has no word for must not keep a request on the screen: the event is read.
        XCTAssertEqual(try decode(ChatEvent.self, #"{"event":"approval_resolved","request_id":"r","decision":"maybe"}"#), .approvalResolved(requestID: "r", decision: .decline))
        let approval = try decode(ChatApproval.self, #"{"request_id":"r","kind":"quantum","title":"t","choices":["accept","maybe","cancel"]}"#)
        XCTAssertEqual(approval.choices, [.accept, .cancel])
        XCTAssertEqual(approval.kind, .tool)
        XCTAssertEqual(approval.detail, "")
        let item = try decode(ChatItem.self, #"{"id":"i","status":"vanished","body":{"type":"plan","steps":[{"text":"a","status":"maybe"},{"nope":1},{"text":"b"}]}}"#)
        XCTAssertEqual(item.status, .completed, "an item the phone cannot name is not one that spins forever")
        XCTAssertEqual(item.body, .plan(explanation: nil, steps: [ChatStep(text: "a", status: .pending), ChatStep(text: "b", status: .pending)]))
        let notice = try decode(ChatItem.self, #"{"id":"n","body":{"type":"notice","level":"catastrophe","text":"x"}}"#)
        XCTAssertEqual(notice.body, .notice(level: .info, text: "x"))
        XCTAssertEqual(notice.status, .completed, "no status at all")
        XCTAssertEqual(try decode(FileChangeBox.self, #"{"c":{"path":"p","kind":"teleport"}}"#).c.kind, .modify)
        XCTAssertEqual(try decode(ChatEvent.self, #"{"event":"turn_completed","turn_id":"t","outcome":{"outcome":"exploded"}}"#), .turnCompleted(turnID: "t", outcome: .completed))
    }
    private struct FileChangeBox: Decodable { let c: ChatFileChange }

    func testMissingOptionalFieldsTakeTheirDefaults() throws {
        let command = try decode(ChatItemBody.self, #"{"type":"command","command":"ls"}"#)
        XCTAssertEqual(command, .command(command: "ls", cwd: nil, output: "", exitCode: nil))
        let tool = try decode(ChatItemBody.self, #"{"type":"tool_call","tool":"Read"}"#)
        XCTAssertEqual(tool, .toolCall(server: nil, tool: "Read", input: .null, output: nil))
        XCTAssertEqual(try decode(ChatItemBody.self, #"{"type":"file_change","changes":[{"nope":1},{"path":"a","kind":"add"}]}"#), .fileChange([ChatFileChange(path: "a", kind: .add)]))
        let prompt = try decode(ChatQuestionPrompt.self, #"{"question":"q"}"#)
        XCTAssertEqual(prompt, ChatQuestionPrompt(question: "q"))
        XCTAssertEqual(try decode(ChatState.self, #"{"state":"failed"}"#), .failed(""))
        // A number of the wrong kind in usage is a zero, not a lost event.
        XCTAssertEqual(try decode(ChatUsage.self, #"{"input_tokens":"many","output_tokens":-3,"cost_usd":"free"}"#), ChatUsage())
    }
    func testAnUnreadableChatIsLeftOutOfTheList() throws {
        let result = try value("""
        {"chats":[
          {"id":"a","provider":"codex","cwd":"/","title":"one","created_at_unix":1},
          {"id":"b","provider":"gemini","cwd":"/","title":"two","created_at_unix":2},
          {"title":"no id"},
          {"id":"c","provider":"claude","cwd":"/","title":"three","created_at_unix":3,"state":{"state":"running"}}
        ]}
        """)
        let chats = try ChatListRequest().parse(result)
        XCTAssertEqual(chats.map(\.id), ["a", "c"])
        XCTAssertEqual(chats[1].state, .running)
        XCTAssertThrowsError(try ChatListRequest().parse(try value(#"{"chats":"nope"}"#)))
        XCTAssertThrowsError(try ChatListRequest().parse(try value(#"[]"#)))
        XCTAssertEqual(try ChatListRequest().parse(try value(#"{"chats":[]}"#)), [])
    }

    // MARK: Activity, banner and words

    func testAChatStateDrawsLikeATerminalAgent() {
        XCTAssertEqual(ChatState.starting.activity, .working)
        XCTAssertEqual(ChatState.running.activity, .working)
        XCTAssertEqual(ChatState.waiting.activity, .waiting)
        for state in [ChatState.idle, .stopped, .failed("x"), .unknown("y")] { XCTAssertEqual(state.activity, .unknown, "\(state)") }
        XCTAssertEqual(ChatState.waiting.spokenActivity, "Waiting for input")
        XCTAssertEqual(ChatState.running.spokenActivity, "Working")
        XCTAssertNil(ChatState.idle.spokenActivity)
        XCTAssertTrue(ChatState.running.isBusy); XCTAssertTrue(ChatState.waiting.isBusy)
        for state in [ChatState.starting, .idle, .stopped, .failed("x")] { XCTAssertFalse(state.isBusy, "\(state)") }
        XCTAssertEqual(ChatState.unknown("hibernating").title, "Hibernating")
    }
    func testTheBannerSaysStartingStoppedAndFailedAndOffersRetryOnlyWithAMessageToSend() {
        XCTAssertEqual(ChatBanner(state: .starting, provider: .claude, lastMessage: nil)?.text, "Starting Claude…")
        XCTAssertEqual(ChatBanner(state: .stopped, provider: .codex, lastMessage: "hi"), .stopped)
        XCTAssertEqual(ChatBanner(state: .failed("process exited"), provider: .codex, lastMessage: "do it"), .failed(message: "process exited", retry: "do it"))
        XCTAssertEqual(ChatBanner(state: .failed("x"), provider: .codex, lastMessage: nil)?.text, "Failed: x Send a message to try again.")
        for state in [ChatState.idle, .running, .waiting, .unknown("z")] { XCTAssertNil(ChatBanner(state: state, provider: .codex, lastMessage: "hi"), "\(state)") }
        // The desktop's text is shown as one bounded printable line.
        let long = String(repeating: "x", count: 400) + "\n\u{7}evil"
        guard case .failed(let message, _)? = ChatBanner(state: .failed(long), provider: .codex, lastMessage: nil) else { return XCTFail("failed") }
        XCTAssertLessThanOrEqual(message.count, 240)
        XCTAssertFalse(message.contains("\n"))
    }
}
