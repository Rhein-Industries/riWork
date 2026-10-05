import XCTest
@testable import RiWorkCore

/// The five chat requests: what leaves the phone, what is refused before it does, and what an answer must look like to be believed.
final class ChatRequestsTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let tree = "33333333-3333-4333-8333-333333333333"
    private let chat = "44444444-4444-4444-8444-444444444444"
    private let requestID = "55555555-5555-4555-8555-555555555555"
    private let letters = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"

    private func value(_ json: String) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: Data(json.utf8)) }
    private func info(provider: String = "codex", id: String? = nil, project projectID: String? = nil, worktree: String? = nil) -> String {
        let fields = ["\"id\":\"\(id ?? chat)\"", "\"provider\":\"\(provider)\"", "\"cwd\":\"/p\"", "\"title\":\"t\"", "\"created_at_unix\":9"]
            + [projectID.map { "\"project_id\":\"\($0)\"" }, worktree.map { "\"worktree_id\":\"\($0)\"" }].compactMap { $0 }
        return "{" + fields.joined(separator: ",") + "}"
    }
    private func create(_ provider: ChatProvider = .codex, _ target: ChatCreateRequest.Target? = nil, mode: ChatApprovalMode? = nil, model: String? = nil, effort: String? = nil, title: String? = nil) throws -> ChatCreateRequest {
        try ChatCreateRequest(provider: provider, target: target ?? .project(project), approvalMode: mode, model: model, effort: effort, title: title)
    }

    // MARK: chats.list

    func testTheListIsAskedForOneProjectOrForAll() throws {
        XCTAssertEqual(try ChatListRequest(projectID: project).params, ["project_id": .string(project)])
        XCTAssertEqual(try ChatListRequest().params, [:])
        XCTAssertThrowsError(try ChatListRequest(projectID: "nope")) { XCTAssertEqual($0 as? ChatValidationError, .invalidID) }
        XCTAssertThrowsError(try ChatListRequest(projectID: letters.uppercased()))
        XCTAssertEqual(try ChatListRequest(params: ["project_id": .string(project)]).projectID, project)
        XCTAssertThrowsError(try ChatListRequest(params: ["project_id": .number(1)]))
        XCTAssertThrowsError(try ChatListRequest(params: ["x": .null]))
    }

    // MARK: chat.create

    func testACreateRequestCarriesOnlyWhatWasChosen() throws {
        XCTAssertEqual(try create().params, ["provider": .string("codex"), "project_id": .string(project)])
        XCTAssertEqual(try create(.claude, .worktree(tree)).params, ["provider": .string("claude"), "worktree_id": .string(tree)])
        XCTAssertEqual(try create(.claude, mode: .full).params["approval_mode"], .string("full"))
        XCTAssertEqual(try create(mode: .autoEdit).params["approval_mode"], .string("auto_edit"))
        XCTAssertNil(try create().params["approval_mode"], "the desktop's default is not spelled out")
        let all = try create(.codex, mode: .plan, model: "gpt-5", effort: "high", title: "Fix the build")
        XCTAssertEqual(all.params, ["provider": .string("codex"), "project_id": .string(project), "approval_mode": .string("plan"),
                                    "model": .string("gpt-5"), "effort": .string("high"), "title": .string("Fix the build")])
    }
    func testFastIsSentOnlyWhenItIsOn() throws {
        // A desktop that predates Fast denies a field it does not know, so off is not spelled out.
        XCTAssertNil(try create().params["fast"])
        XCTAssertNil(try ChatCreateRequest(provider: .claude, target: .project(project), model: "opus", fast: false).params["fast"])
        let on = try ChatCreateRequest(provider: .claude, target: .project(project), model: "opus", effort: "high", fast: true)
        XCTAssertEqual(on.params, ["provider": .string("claude"), "project_id": .string(project), "model": .string("opus"), "effort": .string("high"), "fast": .bool(true)])
        XCTAssertTrue(on.fast)
        XCTAssertEqual(try ChatCreateRequest(params: on.params), on)
        XCTAssertFalse(try ChatCreateRequest(params: ["provider": .string("codex"), "project_id": .string(project), "fast": .bool(false)]).fast, "a spelled-out off is read")
        for bad in [JSONValue.string("true"), .number(1), .null] {
            XCTAssertThrowsError(try ChatCreateRequest(params: ["provider": .string("codex"), "project_id": .string(project), "fast": bad]), "\(bad)") { XCTAssertEqual($0 as? ChatValidationError, .malformed) }
        }
        XCTAssertNoThrow(try RequestValidation.validate(method: "chat.create", params: on.params, id: requestID))
        XCTAssertThrowsError(try RequestValidation.validate(method: "chat.create", params: on.params.merging(["fast": .string("yes")]) { $1 }, id: requestID))
    }
    func testTheTargetIsExactlyOneFullLowercaseUUID() throws {
        for bad in ["", "nope", String(project.prefix(8)), letters.uppercased(), project.replacingOccurrences(of: "-", with: ""), project + " "] {
            XCTAssertThrowsError(try create(.codex, .project(bad)), bad) { XCTAssertEqual($0 as? ChatValidationError, .invalidID) }
            XCTAssertThrowsError(try create(.codex, .worktree(bad)), bad) { XCTAssertEqual($0 as? ChatValidationError, .invalidID) }
        }
        let provider = JSONValue.string("codex")
        XCTAssertThrowsError(try ChatCreateRequest(params: ["provider": provider])) { XCTAssertEqual($0 as? ChatValidationError, .needsOneTarget) }
        XCTAssertThrowsError(try ChatCreateRequest(params: ["provider": provider, "project_id": .string(project), "worktree_id": .string(tree)])) {
            XCTAssertEqual($0 as? ChatValidationError, .needsOneTarget)
        }
        XCTAssertThrowsError(try ChatCreateRequest(params: ["provider": provider, "project_id": .number(1)])) { XCTAssertEqual($0 as? ChatValidationError, .invalidID) }
    }
    func testTextIsBoundedLikeTheDesktopBoundsIt() throws {
        _ = try create(model: String(repeating: "m", count: 100), effort: String(repeating: "e", count: 32), title: String(repeating: "t", count: 200))
        XCTAssertThrowsError(try create(model: String(repeating: "m", count: 101))) { XCTAssertEqual($0 as? ChatValidationError, .textTooLong(field: "model", limit: 100)) }
        XCTAssertThrowsError(try create(effort: String(repeating: "e", count: 33))) { XCTAssertEqual($0 as? ChatValidationError, .textTooLong(field: "effort", limit: 32)) }
        XCTAssertThrowsError(try create(title: String(repeating: "t", count: 201))) { XCTAssertEqual($0 as? ChatValidationError, .textTooLong(field: "title", limit: 200)) }
        // Bytes, the stricter reading: a title of 100 two-byte letters is too long.
        XCTAssertThrowsError(try create(title: String(repeating: "é", count: 101)))
    }
    func testACreateRequestReadsBackThroughTheSameRules() throws {
        let all = try create(.claude, .worktree(tree), mode: .autoEdit, model: "opus", effort: "low", title: "x")
        XCTAssertEqual(try ChatCreateRequest(params: all.params), all)
        for params: [String: JSONValue] in [
            ["provider": .string("gemini"), "project_id": .string(project)], ["provider": .null, "project_id": .string(project)], ["project_id": .string(project)],
            ["provider": .string("codex"), "project_id": .string(project), "approval_mode": .string("yolo")],
            ["provider": .string("codex"), "project_id": .string(project), "model": .number(1)],
            ["provider": .string("codex"), "project_id": .string(project), "unrestricted": .bool(true)]
        ] { XCTAssertThrowsError(try ChatCreateRequest(params: params), "\(params)") }
    }
    func testACreateAnswerMustBeTheChatThatWasAskedFor() throws {
        let request = try create(.codex, .project(project))
        let good = try request.parse(try value("{\"chat\":\(info(project: project))}"))
        XCTAssertEqual(good.id, chat); XCTAssertEqual(good.provider, .codex)
        XCTAssertEqual(try request.parse(try value("{\"chat\":\(info())}")).id, chat, "a desktop that leaves the project out is believed")
        for bad in ["{}", "{\"chat\":null}", "{\"chat\":{\"id\":\"x\"}}", "{\"chat\":\(info(id: "short"))}", "{\"chat\":\(info(provider: "claude"))}",
                    "{\"chat\":\(info(project: tree))}", "{\"chat\":\(info(provider: "gemini"))}", "[]", "{\"status\":\"created\"}"] {
            XCTAssertThrowsError(try request.parse(try value(bad)), bad) { XCTAssertEqual($0 as? ChatControlError, .unreadableReply) }
        }
        let inTree = try create(.claude, .worktree(tree))
        XCTAssertNoThrow(try inTree.parse(try value("{\"chat\":\(info(provider: "claude", worktree: tree))}")))
        XCTAssertThrowsError(try inTree.parse(try value("{\"chat\":\(info(provider: "claude", worktree: project))}")))
    }

    // MARK: chat.events

    func testAnEventsRequestNamesTheCursorAndTheWait() throws {
        let request = try ChatEventsRequest(chatID: chat, since: 42, waitMilliseconds: 20_000)
        XCTAssertEqual(request.params, ["chat_id": .string(chat), "since": .number(42), "wait_ms": .number(20_000)])
        XCTAssertTrue(request.isLongPoll)
        XCTAssertEqual(try ChatEventsRequest(chatID: chat, since: 0, waitMilliseconds: 0, maxEvents: 50).params["max_events"], .number(50))
        XCTAssertFalse(try ChatEventsRequest(chatID: chat, since: 0, waitMilliseconds: 0).isLongPoll)
        XCTAssertNil(try ChatEventsRequest(chatID: chat, since: 0, waitMilliseconds: 0).params["max_events"])
    }
    func testWaitAndCountStayInsideTheDesktopsBounds() throws {
        _ = try ChatEventsRequest(chatID: chat, since: 0, waitMilliseconds: 25_000, maxEvents: 2000)
        _ = try ChatEventsRequest(chatID: chat, since: 0, waitMilliseconds: 0, maxEvents: 1)
        XCTAssertThrowsError(try ChatEventsRequest(chatID: chat, since: 0, waitMilliseconds: 25_001)) { XCTAssertEqual($0 as? ChatValidationError, .invalidWait) }
        XCTAssertThrowsError(try ChatEventsRequest(chatID: chat, since: 0, waitMilliseconds: -1)) { XCTAssertEqual($0 as? ChatValidationError, .invalidWait) }
        XCTAssertThrowsError(try ChatEventsRequest(chatID: chat, since: 0, waitMilliseconds: 0, maxEvents: 0)) { XCTAssertEqual($0 as? ChatValidationError, .invalidCount) }
        XCTAssertThrowsError(try ChatEventsRequest(chatID: chat, since: 0, waitMilliseconds: 0, maxEvents: 2001)) { XCTAssertEqual($0 as? ChatValidationError, .invalidCount) }
        XCTAssertThrowsError(try ChatEventsRequest(chatID: "nope", since: 0, waitMilliseconds: 0)) { XCTAssertEqual($0 as? ChatValidationError, .invalidID) }
        XCTAssertLessThan(ChatLimits.waitMilliseconds, ChatLimits.maximumWaitMilliseconds)
    }
    func testAnEventsRequestReadsBackAndRefusesWhatIsNotOne() throws {
        let request = try ChatEventsRequest(chatID: chat, since: 7, waitMilliseconds: 1000, maxEvents: 10)
        XCTAssertEqual(try ChatEventsRequest(params: request.params), request)
        for params: [String: JSONValue] in [
            ["chat_id": .string(chat), "since": .number(1)],
            ["chat_id": .string(chat), "since": .number(-1), "wait_ms": .number(0)],
            ["chat_id": .string(chat), "since": .number(1.5), "wait_ms": .number(0)],
            ["chat_id": .string(chat), "since": .string("1"), "wait_ms": .number(0)],
            ["chat_id": .string(chat), "since": .number(1), "wait_ms": .number(99_999)],
            ["chat_id": .string(chat), "since": .number(1), "wait_ms": .number(0), "max_events": .number(0)],
            ["chat_id": .string(chat), "since": .number(1), "wait_ms": .number(0), "extra": .null]
        ] { XCTAssertThrowsError(try ChatEventsRequest(params: params), "\(params)") }
    }
    func testAnEventsAnswerMustBeForTheChatAndHaveACursor() throws {
        let request = try ChatEventsRequest(chatID: chat, since: 0, waitMilliseconds: 0)
        let reply = try request.parse(try value("{\"chat_id\":\"\(chat)\",\"events\":[{\"seq\":1,\"event\":{\"event\":\"state\",\"state\":{\"state\":\"idle\"}}}],\"next\":1,\"more\":true}"))
        XCTAssertEqual(reply.events, [ChatEnvelope(seq: 1, event: .state(.idle))])
        XCTAssertEqual(reply.next, 1); XCTAssertTrue(reply.more)
        XCTAssertFalse(try request.parse(try value("{\"chat_id\":\"\(chat)\",\"events\":[],\"next\":0}")).more, "a missing `more` is false")
        for bad in ["{\"chat_id\":\"\(requestID)\",\"events\":[],\"next\":0}", "{\"chat_id\":\"\(chat)\",\"events\":[]}", "{\"chat_id\":\"\(chat)\",\"next\":0}",
                    "{\"chat_id\":\"\(chat)\",\"events\":{},\"next\":0}", "{\"chat_id\":\"\(chat)\",\"events\":[],\"next\":-1}", "{\"chat_id\":\"\(chat)\",\"events\":[],\"next\":1.5}",
                    "{\"chat_id\":\"\(chat)\",\"events\":[],\"next\":\"1\"}", "[]", "null"] {
            XCTAssertThrowsError(try request.parse(try value(bad)), bad) { XCTAssertEqual($0 as? ChatControlError, .unreadableReply) }
        }
    }

    // MARK: chat.command and chat.stop

    func testACommandRequestIsTheChatAndTheCommandInItsWireForm() throws {
        let send = try ChatCommandRequest(chatID: chat, command: .send(text: "hello"))
        XCTAssertEqual(send.params, ["chat_id": .string(chat), "command": try value(#"{"command":"send","text":"hello"}"#)])
        let approve = try ChatCommandRequest(chatID: chat, command: .approve(requestID: "r1", decision: .acceptForSession))
        XCTAssertEqual(approve.params["command"], try value(#"{"command":"approve","request_id":"r1","decision":"accept_for_session"}"#))
        XCTAssertEqual(try ChatCommandRequest(params: approve.params), approve)
        XCTAssertEqual(try ChatCommandRequest(chatID: chat, command: .interrupt).params["command"], try value(#"{"command":"interrupt"}"#))
        XCTAssertThrowsError(try ChatCommandRequest(chatID: "nope", command: .interrupt)) { XCTAssertEqual($0 as? ChatValidationError, .invalidID) }
    }
    func testAConfigureCarriesOneChangeAndIsRefusedWhenItChangesNothing() throws {
        let model = try ChatCommandRequest(chatID: chat, command: .configure(model: "opus"))
        XCTAssertEqual(model.params["command"], try value(#"{"command":"configure","model":"opus"}"#))
        XCTAssertEqual(try ChatCommandRequest(chatID: chat, command: .configure(effort: "high")).params["command"], try value(#"{"command":"configure","effort":"high"}"#))
        XCTAssertEqual(try ChatCommandRequest(chatID: chat, command: .configure(fast: true)).params["command"], try value(#"{"command":"configure","fast":true}"#))
        XCTAssertEqual(try ChatCommandRequest(chatID: chat, command: .configure(fast: false)).params["command"], try value(#"{"command":"configure","fast":false}"#), "turning it off is a change")
        let all = try ChatCommandRequest(chatID: chat, command: .configure(model: "gpt-5.5", effort: "xhigh", approvalMode: .plan, fast: true))
        XCTAssertEqual(try ChatCommandRequest(params: all.params), all)
        // The desktop refuses a configure with nothing in it, and a blank or overlong model or effort.
        XCTAssertThrowsError(try ChatCommandRequest(chatID: chat, command: .configure())) { XCTAssertEqual($0 as? ChatValidationError, .emptyConfigure) }
        XCTAssertThrowsError(try ChatCommandRequest(chatID: chat, command: .configure(model: "  "))) { XCTAssertEqual($0 as? ChatValidationError, .blankSetting(field: "model")) }
        XCTAssertThrowsError(try ChatCommandRequest(chatID: chat, command: .configure(effort: ""))) { XCTAssertEqual($0 as? ChatValidationError, .blankSetting(field: "effort")) }
        XCTAssertThrowsError(try ChatCommandRequest(chatID: chat, command: .configure(model: String(repeating: "m", count: 101)))) { XCTAssertEqual($0 as? ChatValidationError, .textTooLong(field: "model", limit: 100)) }
        XCTAssertThrowsError(try ChatCommandRequest(chatID: chat, command: .configure(effort: String(repeating: "e", count: 33)))) { XCTAssertEqual($0 as? ChatValidationError, .textTooLong(field: "effort", limit: 32)) }
        _ = try ChatCommandRequest(chatID: chat, command: .configure(model: String(repeating: "m", count: 100), effort: String(repeating: "e", count: 32)))
    }
    func testAMessageIsNotBlankAndAtMost64KiB() throws {
        for blank in ["", "   ", "\n\t \n"] {
            XCTAssertThrowsError(try ChatCommandRequest(chatID: chat, command: .send(text: blank))) { XCTAssertEqual($0 as? ChatValidationError, .blankMessage) }
        }
        _ = try ChatCommandRequest(chatID: chat, command: .send(text: String(repeating: "a", count: 65_536)))
        XCTAssertThrowsError(try ChatCommandRequest(chatID: chat, command: .send(text: String(repeating: "a", count: 65_537)))) { XCTAssertEqual($0 as? ChatValidationError, .messageTooLong) }
        XCTAssertThrowsError(try ChatCommandRequest(chatID: chat, command: .send(text: String(repeating: "é", count: 32_769))), "bytes, not characters")
        // Text is sent as written: no trimming of the message itself.
        XCTAssertEqual(try ChatCommandRequest(chatID: chat, command: .send(text: "  keep\n")).params["command"], try value("{\"command\":\"send\",\"text\":\"  keep\\n\"}"))
    }
    func testACommandRequestReadsBackAndRefusesWhatIsNotOne() throws {
        for params: [String: JSONValue] in [
            ["chat_id": .string(chat)], ["command": .object(["command": .string("stop")])],
            ["chat_id": .string(chat), "command": .string("stop")], ["chat_id": .string(chat), "command": .object(["command": .string("explode")])],
            ["chat_id": .string(chat), "command": .object(["command": .string("send")])],
            ["chat_id": .string(chat), "command": .object(["command": .string("stop")]), "extra": .null]
        ] { XCTAssertThrowsError(try ChatCommandRequest(params: params), "\(params)") }
        let request = try ChatCommandRequest(chatID: chat, command: .stop)
        XCTAssertNoThrow(try request.parse(try value(#"{"status":"ok"}"#)))
        for bad in [#"{"status":"stopped"}"#, #"{}"#, #"[]"#] { XCTAssertThrowsError(try request.parse(try value(bad)), bad) }
    }
    func testAStopRequestExpectsStopped() throws {
        let request = try ChatStopRequest(chatID: chat)
        XCTAssertEqual(request.params, ["chat_id": .string(chat)])
        XCTAssertNoThrow(try request.parse(try value(#"{"status":"stopped"}"#)))
        XCTAssertThrowsError(try request.parse(try value(#"{"status":"ok"}"#)))
        XCTAssertThrowsError(try ChatStopRequest(chatID: "x"))
    }

    // MARK: The transport's own check

    func testTheTransportAcceptsExactlyTheseRequestsAndNoOthers() throws {
        let id = requestID
        XCTAssertNoThrow(try RequestValidation.validate(method: "chats.list", params: [:], id: id))
        XCTAssertNoThrow(try RequestValidation.validate(method: "chats.list", params: try ChatListRequest(projectID: project).params, id: id))
        XCTAssertNoThrow(try RequestValidation.validate(method: "chat.create", params: try create(mode: .full).params, id: id))
        XCTAssertNoThrow(try RequestValidation.validate(method: "chat.events", params: try ChatEventsRequest(chatID: chat, since: 3, waitMilliseconds: 20_000).params, id: id))
        XCTAssertNoThrow(try RequestValidation.validate(method: "chat.command", params: try ChatCommandRequest(chatID: chat, command: .compact).params, id: id))
        XCTAssertNoThrow(try RequestValidation.validate(method: "chat.stop", params: try ChatStopRequest(chatID: chat).params, id: id))
        // Each is checked by the rules above, not just by its parameter names.
        XCTAssertThrowsError(try RequestValidation.validate(method: "chat.events", params: ["chat_id": .string(chat), "since": .number(0)], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "chat.events", params: ["chat_id": .string("nope"), "since": .number(0), "wait_ms": .number(0)], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "chat.events", params: ["chat_id": .string(chat), "since": .number(0), "wait_ms": .number(30_000)], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "chat.create", params: ["provider": .string("codex")], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "chat.create", params: ["provider": .string("codex"), "project_id": .string(project), "kind": .string("codex")], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "chat.command", params: ["chat_id": .string(chat), "command": .object(["command": .string("send"), "text": .string(" ")])], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "chat.stop", params: ["chat_id": .string(chat), "extra": .null], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "chat.delete", params: ["chat_id": .string(chat)], id: id))
        // A terminal is not a chat: shell.create refuses the chat kinds.
        XCTAssertThrowsError(try RequestValidation.validate(method: "shell.create", params: ["kind": .string("codex_chat"), "project_id": .string(project)], id: id))
    }
    func testTheRequestTimeoutsLeaveRoomForTheDesktop() {
        let base = Duration.seconds(15)
        XCTAssertEqual(RelayClient.timeout(for: "chat.events", params: ["wait_ms": .number(20_000)], default: base), .seconds(40), "the wait plus 20 s")
        XCTAssertEqual(RelayClient.timeout(for: "chat.events", params: ["wait_ms": .number(0)], default: base), base)
        XCTAssertEqual(RelayClient.timeout(for: "chat.events", params: ["wait_ms": .number(99_999)], default: base), .seconds(45), "never more than the bound plus the slack")
        XCTAssertEqual(RelayClient.timeout(for: "chat.create", params: [:], default: base), .seconds(90))
        XCTAssertGreaterThanOrEqual(RelayClient.timeout(for: "chat.command", params: [:], default: base), .seconds(60))
        XCTAssertGreaterThanOrEqual(RelayClient.timeout(for: "chat.stop", params: [:], default: base), .seconds(30))
        XCTAssertEqual(RelayClient.timeout(for: "chats.list", params: [:], default: base), base)
    }

    // MARK: Features and errors

    func testChatIsOfferedOnlyByADesktopThatSaysSo() throws {
        XCTAssertFalse(DesktopFeatures().chat)
        XCTAssertTrue(DesktopFeatures(ready: try value(#"{"features":{"chat":true}}"#)).chat)
        for json in [#"{"features":{"chat":false}}"#, #"{"features":{"chat":"yes"}}"#, #"{"features":{"chat":1}}"#, #"{"features":{}}"#, #"{}"#, #"{"features":"chat"}"#] {
            XCTAssertFalse(DesktopFeatures(ready: try value(json)).chat, json)
        }
        let both = DesktopFeatures(ready: try value(#"{"features":{"chat":true,"deflate":{"min_bytes":2048,"max_inflated":1048576},"history_max_lines":5000}}"#))
        XCTAssertTrue(both.chat); XCTAssertTrue(both.deflate); XCTAssertEqual(both.historyMaximumLines, 5000)
    }
    func testWhatTheTransportThrowsBecomesSomethingToSay() {
        let unsupported = RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")
        XCTAssertEqual(ChatControlError.from(unsupported, operation: .list), .unsupported)
        XCTAssertEqual(ChatControlError.from(RemoteError.rpc(code: "not_found", message: "no chat"), operation: .events), .notFound(.events))
        XCTAssertEqual(ChatControlError.from(RemoteError.rpc(code: "harness_unavailable", message: "x"), operation: .create(.claude)), .harnessUnavailable(.claude))
        XCTAssertEqual(ChatControlError.harnessUnavailable(.claude).message, "Claude isn’t installed on the Mac (or isn’t on its PATH).")
        XCTAssertEqual(ChatControlError.from(RemoteError.rpc(code: "invalid_request", message: "title too long"), operation: .create(.codex)), .invalid("title too long"))
        XCTAssertEqual(ChatControlError.from(RemoteError.rpc(code: "cli_error", message: "boom\nline"), operation: .command), .failed("boom line"))
        XCTAssertEqual(ChatControlError.from(ChatValidationError.blankMessage, operation: .command), .invalid("Write a message first."))
        // Only the desktop's own answer says nothing happened. Whatever the connection did leaves it unknown.
        for error: any Error in [RemoteError.timeout, RemoteError.disconnected, RemoteError.relayClosed(code: 1006, reason: nil), RemoteError.uncertainDelivery, CancellationError(), URLError(.timedOut)] {
            XCTAssertEqual(ChatControlError.from(error, operation: .create(.codex)), .outcomeUnknown(.create(.codex)), "\(error)")
            XCTAssertTrue(ChatControlError.outcomeUnknown(.command).outcomeIsUncertain)
        }
        XCTAssertTrue(ChatControlError.unreadableReply.outcomeIsUncertain)
        XCTAssertFalse(ChatControlError.notFound(.command).outcomeIsUncertain)
        XCTAssertTrue(ChatControlError.outcomeUnknown(.create(.codex)).message.contains("may or may not have been created"))
        XCTAssertTrue(ChatControlError.outcomeUnknown(.command).message.contains("may or may not have gone through"))
        XCTAssertEqual(ChatControlError.notFound(.create(.codex)).message, "That project or worktree no longer exists on the Mac. Refresh and try again.")
    }
}

final class LatestFirstRequestTests: XCTestCase {
    private let id = "cccccccc-1111-4111-8111-111111111111"
    func testSnapshotValidationAndLosslessLiveRequests() throws {
        try RequestValidation.validate(method: "chat.snapshot", params: ["chat_id": .string(id), "limit": .number(50)], id: id)
        for extra in [["cursor": JSONValue.null], ["before": .number(2)], ["limit": .number(101)], ["cursor": .string("../path")], ["item_ids": .array([.string("a")])]] {
            XCTAssertThrowsError(try RequestValidation.validate(method: "chat.snapshot", params: ["chat_id": .string(id)].merging(extra) { _, v in v }, id: id))
        }
        let request = try ChatEventsRequest(chatID: id, since: 30_000, waitMilliseconds: 0, complete: true)
        XCTAssertEqual(request.params["complete"], .bool(true))
        XCTAssertEqual(try ChatEventsRequest(params: request.params), request)
        try RequestValidation.validate(method: "chat.events", params: request.params, id: id)
        XCTAssertThrowsError(try ChatEventsRequest(params: request.params.merging(["complete": .string("true")]) { _, v in v }))
    }
}

extension LatestFirstRequestTests {
    func testLosslessPagesRejectGapsDuplicatesAndUnrepresentedCursorAdvances() throws {
        let request = try ChatEventsRequest(chatID: id, since: 100, waitMilliseconds: 0, complete: true)
        func page(_ seqs: [Int], next: Int) -> JSONValue {
            .object(["chat_id": .string(id), "next": .number(Double(next)), "more": .bool(false),
                     "events": .array(seqs.map { .object(["seq": .number(Double($0)), "event": .object(["event": .string("future_event")])]) })])
        }
        let contiguous = try request.parse(page([101, 102], next: 102))
        XCTAssertEqual(contiguous.next, 102)
        XCTAssertEqual(contiguous.skipped, 2, "unknown tagged events still account for their sequence")
        for invalid in [page([102], next: 102), page([101, 101], next: 101), page([101], next: 103), page([], next: 101)] {
            XCTAssertThrowsError(try request.parse(invalid), "never acknowledge an event that was not received")
        }
        XCTAssertEqual(try request.parse(page([], next: 100)).next, 100)
    }
}

extension LatestFirstRequestTests {
    func testResourceFailuresStayStructuredAndBoundedRecoveryIsGapless() throws {
        XCTAssertEqual(ChatControlError.from(RemoteError.rpc(code: "snapshot_limit", message: "limit"), operation: .events), .resourceLimit(.snapshot, "limit"))
        XCTAssertEqual(ChatControlError.from(RemoteError.rpc(code: "response_too_large", message: "large"), operation: .events), .resourceLimit(.response, "large"))
        let request = try ChatEventsRequest(chatID: id, since: 100, waitMilliseconds: 0, bounded: true)
        try RequestValidation.validate(method: "chat.events", params: request.params, id: id)
        XCTAssertEqual(try ChatEventsRequest(params: request.params), request)
        XCTAssertThrowsError(try ChatEventsRequest(chatID: id, since: 0, waitMilliseconds: 0, complete: true, bounded: true))
        XCTAssertThrowsError(try request.parse(.object(["chat_id":.string(id),"events":.array([]),"next":.number(101),"more":.bool(false)])))
    }
}
