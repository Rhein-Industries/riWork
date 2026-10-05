import XCTest
@testable import RiWorkCore

/// `orchestrator.create {project_id?}`: the request it is built with, the reply it is read from, what it says when it fails, the
/// `orchestrator_create` feature, and the two orchestrator rows of the New terminal sheet.
final class NewOrchestratorTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let other = "22222222-2222-4222-8222-222222222222"
    private let tree = "33333333-3333-4333-8333-333333333333"
    private let entryID = "aaaaaaaa-1111-4111-8111-111111111111"
    private let chatID = "cccccccc-1111-4111-8111-111111111111"

    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T { try JSONDecoder().decode(type, from: Data(json.utf8)) }
    private func entry(project scope: String?, alive: Bool = true, kind: String = "orchestrator", _ extra: String = "") -> String {
        "{\"id\":\"\(entryID)\",\"project_id\":\(scope.map { "\"\($0)\"" } ?? "null"),\"worktree_id\":null,\"kind\":\"\(kind)\",\"cwd\":\"/a\",\"harness\":null,\"alive\":\(alive),\"created_at_unix\":5\(extra.isEmpty ? "" : "," + extra)}"
    }
    private let asChat = "\"mode\":\"chat\",\"chat_id\":\"cccccccc-1111-4111-8111-111111111111\",\"provider\":\"codex\""
    private func reply(_ entry: String, created: String? = "true") throws -> JSONValue {
        try decode(JSONValue.self, "{\"orchestrator\":\(entry)\(created.map { ",\"created\":\($0)" } ?? "")}")
    }
    private func projectRequest(_ id: String? = nil) throws -> NewOrchestratorRequest { try NewOrchestratorRequest(projectID: id ?? project) }
    private func globalRequest() throws -> NewOrchestratorRequest { try NewOrchestratorRequest(projectID: nil) }

    // MARK: The feature

    func testTheFeatureIsOnlyWhatTheDesktopSaysIsTrue() throws {
        XCTAssertFalse(DesktopFeatures().orchestratorCreate)
        XCTAssertTrue(DesktopFeatures(ready: try decode(JSONValue.self, "{\"features\":{\"orchestrator_create\":true}}")).orchestratorCreate)
        for bad in ["false", "1", "\"true\"", "null", "{}", "[true]"] {
            XCTAssertFalse(DesktopFeatures(ready: try decode(JSONValue.self, "{\"features\":{\"orchestrator_create\":\(bad)}}")).orchestratorCreate, bad)
        }
        XCTAssertFalse(DesktopFeatures(ready: try decode(JSONValue.self, "{\"features\":{\"chat\":true}}")).orchestratorCreate, "chats do not imply it")
        let both = DesktopFeatures(ready: try decode(JSONValue.self, "{\"features\":{\"chat\":true,\"orchestrator_create\":true}}"))
        XCTAssertTrue(both.chat && both.orchestratorCreate)
    }

    // MARK: The request

    func testTheProjectsOrchestratorSendsItsProjectAndTheGlobalOneSendsNothing() throws {
        XCTAssertEqual(try projectRequest().params, ["project_id": .string(project)])
        XCTAssertEqual(try globalRequest().params, [:], "an absent project_id is the global orchestrator")
        XCTAssertTrue(try globalRequest().isGlobal)
        XCTAssertFalse(try projectRequest().isGlobal)
        XCTAssertEqual(try projectRequest().title, "Project orchestrator")
        XCTAssertEqual(try globalRequest().title, "Global orchestrator")
    }
    func testTheProjectIsOneFullLowercaseUUID() throws {
        for bad in ["", "nope", String(project.prefix(8)), entryID.uppercased(), project.replacingOccurrences(of: "-", with: ""), project + " "] {
            XCTAssertThrowsError(try NewOrchestratorRequest(projectID: bad), bad) { XCTAssertEqual($0 as? NewTerminalValidationError, .invalidID) }
        }
    }
    func testWireParametersAreReadBackThroughTheSameRules() throws {
        XCTAssertEqual(try NewOrchestratorRequest(params: [:]), try globalRequest())
        XCTAssertEqual(try NewOrchestratorRequest(params: ["project_id": .string(project)]), try projectRequest())
        XCTAssertThrowsError(try NewOrchestratorRequest(params: ["project_id": .number(1)])) { XCTAssertEqual($0 as? NewTerminalValidationError, .invalidID) }
        XCTAssertThrowsError(try NewOrchestratorRequest(params: ["project_id": .null]))
        XCTAssertThrowsError(try NewOrchestratorRequest(params: ["project_id": .string("x")]))
        XCTAssertThrowsError(try NewOrchestratorRequest(params: ["worktree_id": .string(tree)])) { XCTAssertEqual($0 as? NewTerminalValidationError, .malformed) }
        XCTAssertThrowsError(try NewOrchestratorRequest(params: ["project_id": .string(project), "kind": .string("shell")]))
    }
    func testTheTransportChecksTheRequestBeforeItLeaves() throws {
        XCTAssertNoThrow(try RequestValidation.validate(method: "orchestrator.create", params: [:], id: project))
        XCTAssertNoThrow(try RequestValidation.validate(method: "orchestrator.create", params: ["project_id": .string(project)], id: project))
        for bad: [String: JSONValue] in [["project_id": .string("nope")], ["worktree_id": .string(tree)], ["project_id": .string(project), "unrestricted": .bool(true)], ["project_id": .null]] {
            XCTAssertThrowsError(try RequestValidation.validate(method: "orchestrator.create", params: bad, id: project), "\(bad)")
        }
    }
    func testItHasTheTimeOfAnAgentStartingUp() {
        XCTAssertGreaterThanOrEqual(RelayClient.timeout(for: "orchestrator.create", default: .seconds(15)), .seconds(90))
    }

    // MARK: The reply

    func testTheReplyCarriesTheEntryAndWhetherItWasMade() throws {
        let made = try NewOrchestratorReply.parse(reply(entry(project: project)), for: projectRequest())
        XCTAssertEqual(made.session.id, entryID)
        XCTAssertEqual(made.session.title, "Project orchestrator")
        XCTAssertTrue(made.created)
        XCTAssertEqual(made.session.opening(chatsAvailable: true), .terminal, "no mode: a terminal")
        let there = try NewOrchestratorReply.parse(reply(entry(project: project), created: "false"), for: projectRequest())
        XCTAssertFalse(there.created, "it was there already")
        XCTAssertEqual(there.session, made.session)
    }
    func testAChatOrchestratorComesWithItsChat() throws {
        let value = try NewOrchestratorReply.parse(reply(entry(project: project, asChat)), for: projectRequest())
        XCTAssertEqual(value.session.mode, .chat)
        XCTAssertEqual(value.session.chat_id, chatID)
        XCTAssertEqual(value.session.provider, .codex)
        XCTAssertEqual(value.session.opening(chatsAvailable: true), .chat(chatID), "opened by the chat's id")
        // A chat is not held to `alive`: its next message starts the agent again.
        XCTAssertNoThrow(try NewOrchestratorReply.parse(reply(entry(project: project, alive: false, asChat)), for: projectRequest()))
    }
    func testTheGlobalOrchestratorHasNoProject() throws {
        let value = try NewOrchestratorReply.parse(reply(entry(project: nil)), for: globalRequest())
        XCTAssertNil(value.session.project_id)
        XCTAssertEqual(value.session.title, "Global orchestrator")
        // Absent altogether is the same.
        let bare = "{\"id\":\"\(entryID)\",\"kind\":\"orchestrator\",\"cwd\":\"/a\",\"alive\":true,\"created_at_unix\":5}"
        XCTAssertNoThrow(try NewOrchestratorReply.parse(reply(bare), for: globalRequest()))
    }
    func testWhetherItWasMadeIsTakenAsYesWhenNotSaid() throws {
        for odd in [nil, "1", "\"false\"", "null"] {
            XCTAssertTrue(try NewOrchestratorReply.parse(reply(entry(project: project), created: odd), for: projectRequest()).created, "\(odd ?? "absent")")
        }
    }
    func testAnAnswerForAnotherOrchestratorIsNotOpened() throws {
        func unreadable(_ result: JSONValue, _ request: NewOrchestratorRequest, _ what: String) {
            XCTAssertThrowsError(try NewOrchestratorReply.parse(result, for: request), what) { XCTAssertEqual($0 as? OrchestratorControlError, .unreadableReply, what) }
        }
        unreadable(try reply(entry(project: nil)), try projectRequest(), "the global one for a project's")
        unreadable(try reply(entry(project: project)), try globalRequest(), "a project's for the global one")
        unreadable(try reply(entry(project: other)), try projectRequest(), "another project's")
        unreadable(try reply(entry(project: project, kind: "project")), try projectRequest(), "a shell")
        unreadable(try reply("{\"id\":\"nope\",\"kind\":\"orchestrator\",\"cwd\":\"/a\",\"alive\":true,\"created_at_unix\":5}"), try globalRequest(), "an id that is no UUID")
        unreadable(try decode(JSONValue.self, "{\"created\":true}"), try projectRequest(), "no orchestrator")
        unreadable(try decode(JSONValue.self, "{\"orchestrator\":\"x\",\"created\":true}"), try projectRequest(), "an orchestrator that is no object")
        unreadable(try decode(JSONValue.self, "{\"orchestrator\":{\"id\":\"\(entryID)\"},\"created\":true}"), try projectRequest(), "an entry missing its fields")
    }
    func testATerminalThatIsNotRunningCannotBeOpened() throws {
        XCTAssertThrowsError(try NewOrchestratorReply.parse(reply(entry(project: project, alive: false)), for: projectRequest())) {
            XCTAssertEqual($0 as? OrchestratorControlError, .exitedRightAway)
        }
    }

    // MARK: Failures

    func testWhatTheDesktopRefusesAndWhatTheLinkLoses() {
        XCTAssertEqual(OrchestratorControlError.from(RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")), .unsupported)
        XCTAssertEqual(OrchestratorControlError.from(RemoteError.rpc(code: "not_found", message: "no such project")), .notFound)
        XCTAssertEqual(OrchestratorControlError.from(RemoteError.rpc(code: "invalid_request", message: "bad\nparams")), .invalid("bad params"))
        XCTAssertEqual(OrchestratorControlError.from(RemoteError.rpc(code: "cli_error", message: "the agent is not installed")), .failed("the agent is not installed"))
        XCTAssertEqual(OrchestratorControlError.from(RemoteError.rpc(code: "outcome_unknown", message: "")), .outcomeUnknown)
        for lost in [RemoteError.timeout, .disconnected, .relayClosed(code: 1006, reason: "gone"), .uncertainDelivery] {
            let error = OrchestratorControlError.from(lost)
            XCTAssertEqual(error, OrchestratorControlError.outcomeUnknown, "\(lost)")
            XCTAssertTrue(error.outcomeIsUncertain)
        }
        XCTAssertEqual(OrchestratorControlError.from(CancellationError()), .outcomeUnknown)
        XCTAssertEqual(OrchestratorControlError.from(NewTerminalValidationError.invalidID), .invalid("The project or worktree is not a full UUID."))
        XCTAssertTrue(OrchestratorControlError.unreadableReply.outcomeIsUncertain)
        for sure in [OrchestratorControlError.unsupported, .notFound, .invalid("x"), .exitedRightAway, .notConnected, .busy, .failed("x")] {
            XCTAssertFalse(sure.outcomeIsUncertain, "\(sure)")
        }
        XCTAssertEqual(OrchestratorControlError.unsupported.message, "Update RiWork on your Mac to open orchestrators from the phone.")
        XCTAssertTrue(OrchestratorControlError.outcomeUnknown.message.contains("Check the tab list before trying again"))
    }

    private actor Scripted: RemoteTransport {
        var reply: Result<JSONValue, any Error>
        var calls: [(method: String, params: [String: JSONValue])] = []
        init(_ reply: Result<JSONValue, any Error>) { self.reply = reply }
        func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { pairing }
        func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
            calls.append((method, params))
            return try reply.get()
        }
        func disconnect() async {}
        func isConnected() async -> Bool { true }
        func sent() -> [(method: String, params: [String: JSONValue])] { calls }
    }
    func testTheTransportHelperSendsOnceAndMapsErrors() async throws {
        let ok = Scripted(.success(try reply(entry(project: project, asChat), created: "false")))
        let opened = try await ok.createOrchestrator(try projectRequest())
        XCTAssertEqual(opened.session.chat_id, chatID)
        XCTAssertFalse(opened.created)
        let sent = await ok.sent()
        XCTAssertEqual(sent.map(\.method), ["orchestrator.create"])
        XCTAssertEqual(sent.first?.params, ["project_id": .string(project)])

        let global = Scripted(.success(try reply(entry(project: nil))))
        _ = try await global.createOrchestrator(try globalRequest())
        let globalCalls = await global.sent()
        XCTAssertEqual(globalCalls.first?.params, [:])

        let old = Scripted(.failure(RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")))
        do { _ = try await old.createOrchestrator(try projectRequest()); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? OrchestratorControlError, .unsupported) }

        let lost = Scripted(.failure(RemoteError.timeout))
        do { _ = try await lost.createOrchestrator(try projectRequest()); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? OrchestratorControlError, .outcomeUnknown) }
        let attempts = await lost.sent()
        XCTAssertEqual(attempts.count, 1, "never retried")

        let wrong = Scripted(.success(try reply(entry(project: other))))
        do { _ = try await wrong.createOrchestrator(try projectRequest()); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? OrchestratorControlError, .unreadableReply) }
    }

    // MARK: The sheet's rows

    private func target(branch: String = "main") -> NewTerminalTarget { .worktree(id: tree, projectID: project, projectName: "Alpha", branch: branch, isPrimary: true) }

    func testTheOrchestratorRowsAreOnlyOfferedWhereTheDesktopOpensThem() {
        XCTAssertEqual(NewTerminalKind.allCases.count, 6, "the kinds that make a tab of their own are what they were")
        XCTAssertEqual(NewTerminalKind.offered(chats: false), NewTerminalKind.terminalKinds)
        XCTAssertEqual(NewTerminalKind.offered(chats: true), NewTerminalKind.allCases)
        XCTAssertEqual(NewTerminalKind.offered(chats: false, orchestrators: true), NewTerminalKind.terminalKinds + [.projectOrchestrator, .globalOrchestrator])
        XCTAssertEqual(NewTerminalKind.offered(chats: true, orchestrators: true).map(\.title),
                       ["Shell", "Codex", "Claude", "Grok", "Codex chat", "Claude chat", "Project orchestrator", "Global orchestrator"])
        XCTAssertEqual(NewTerminalKind.orchestratorKinds.map(\.isOrchestrator), [true, true])
        XCTAssertEqual(NewTerminalKind.allCases.map(\.isOrchestrator), Array(repeating: false, count: 6))
    }
    func testAnOrchestratorHasNoUnrestrictedSwitchAndIsNeitherATerminalNorAChat() {
        for kind in NewTerminalKind.orchestratorKinds {
            XCTAssertFalse(kind.isAgent, kind.title)
            XCTAssertFalse(kind.isChat)
            XCTAssertThrowsError(try NewTerminalRequest(target: .project(project), kind: kind), "shell.create has no such kind") {
                XCTAssertEqual($0 as? NewTerminalValidationError, .unknownKind)
            }
        }
        XCTAssertTrue(NewTerminalKind.codexChat.isAgent, "and the others are as they were")
    }
    func testTheProjectOrchestratorIsOfTheProjectNotOfAWorktree() throws {
        var form = NewTerminalForm(targets: [target(), .worktree(id: other, projectID: project, projectName: "Alpha", branch: "feature", isPrimary: false)],
                                   kind: .projectOrchestrator, kinds: NewTerminalKind.offered(chats: true, orchestrators: true))
        guard case .orchestrator(let request) = try form.submission() else { return XCTFail("an orchestrator request") }
        XCTAssertEqual(request, try projectRequest())
        form.select(targetAt: 1)
        guard case .orchestrator(let again) = try form.submission() else { return XCTFail() }
        XCTAssertEqual(again, try projectRequest(), "another worktree of the project, the same orchestrator")
        XCTAssertEqual(form.target?.projectName, "Alpha")
        XCTAssertEqual(NewTerminalTarget.project(id: project, name: "Beta").projectName, "Beta")
    }
    func testTheGlobalOrchestratorNeedsNoProject() throws {
        let form = NewTerminalForm(targets: [], kind: .globalOrchestrator, kinds: NewTerminalKind.offered(chats: false, orchestrators: true))
        guard case .orchestrator(let request) = try form.submission() else { return XCTFail("an orchestrator request") }
        XCTAssertEqual(request, try globalRequest())
        XCTAssertEqual(request.params, [:])
        // The project's needs one.
        XCTAssertThrowsError(try NewTerminalForm(targets: [], kind: .projectOrchestrator, kinds: NewTerminalKind.offered(chats: false, orchestrators: true)).submission()) {
            XCTAssertEqual($0 as? NewTerminalValidationError, .needsOneTarget)
        }
    }
    func testTheKeyboardStepsThroughTheOrchestratorRowsAndSkipsTheWorktreeRow() {
        var form = NewTerminalForm(targets: [target()], kind: .claudeChat, kinds: NewTerminalKind.offered(chats: true, orchestrators: true))
        form.handle(.down)
        XCTAssertEqual(form.kind, .projectOrchestrator)
        XCTAssertEqual(form.fields, [.kind, .orchestratorMode, .create], "no worktree to choose, no switch")
        form.handle(.down)
        XCTAssertEqual(form.kind, .globalOrchestrator)
        form.handle(.down)
        XCTAssertEqual(form.kind, .shell, "and it wraps")
        XCTAssertEqual(form.fields, [.target, .kind, .create])
        form.handle(.up)
        XCTAssertEqual(form.kind, .globalOrchestrator)
        // The ring was on the worktree row: it goes to the kind when that row is gone.
        form.focus = .kind
        form.handle(.tab)
        XCTAssertEqual(form.focus, .orchestratorMode)
        form.handle(.tab)
        XCTAssertEqual(form.focus, .create)
        form.handle(.tab)
        XCTAssertEqual(form.focus, .kind, "the ring skips the row that is not there")
        form.select(kind: .projectOrchestrator)
        form.setUnrestricted(true)
        XCTAssertFalse(form.unrestricted)
    }
    func testAFocusOnTheWorktreeRowMovesWhenAnOrchestratorIsChosen() {
        var form = NewTerminalForm(targets: [target()], kind: .shell, kinds: NewTerminalKind.offered(chats: false, orchestrators: true))
        form.focus = .target
        form.select(kind: .globalOrchestrator)
        XCTAssertEqual(form.focus, .kind)
    }
    func testExplicitModeRoundTripsAndKeyboardSelectionReachesSubmission() throws {
        for mode in [NewOrchestratorMode.chat, .terminal] {
            let request = try NewOrchestratorRequest(projectID: project, mode: mode)
            XCTAssertEqual(request.params["mode"], .string(mode.rawValue))
            XCTAssertEqual(try NewOrchestratorRequest(params: request.params), request)
        }
        for bad in [JSONValue.null, .number(1), .string("desktop"), .string("Chat")] {
            XCTAssertThrowsError(try NewOrchestratorRequest(params: ["mode": bad]))
        }
        var form = NewTerminalForm(targets: [], kind: .globalOrchestrator, kinds: NewTerminalKind.offered(chats: true, orchestrators: true))
        form.handle(.tab)
        form.handle(.down)
        guard case .orchestrator(let request) = try form.submission() else { return XCTFail() }
        XCTAssertEqual(request.mode, .chat)
        XCTAssertNil(request.projectID)
    }

    func testNewChatCarriesTheChosenModel() throws {
        var form = NewTerminalForm(targets: [target()], kind: .codexChat, kinds: NewTerminalKind.offered(chats: true, orchestrators: true))
        // The model comes from the sheet's model choice (the picker), not typed text.
        form.chatChoices[.codex] = NewChatChoice(model: ChatModelOption(id: "custom-model", name: "Custom"), usesModel: true)
        guard case .chat(let request) = try form.submission() else { return XCTFail() }
        XCTAssertEqual(request.model, "custom-model")
        form.chatChoices[.codex] = NewChatChoice()
        guard case .chat(let defaultRequest) = try form.submission() else { return XCTFail() }
        XCTAssertNil(defaultRequest.model)
    }

}
