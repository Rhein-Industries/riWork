import XCTest
@testable import RiWorkCore

final class NewTerminalTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let other = "22222222-2222-4222-8222-222222222222"
    private let tree = "33333333-3333-4333-8333-333333333333"
    private let shell = "44444444-4444-4444-8444-444444444444"
    private let requestID = "55555555-5555-4555-8555-555555555555"
    private let letters = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"

    private func request(_ target: NewTerminalRequest.Target? = nil, _ kind: NewTerminalKind = .shell, unrestricted: Bool = false, command: String? = nil) throws -> NewTerminalRequest {
        try NewTerminalRequest(target: target ?? .project(project), kind: kind, unrestricted: unrestricted, command: command)
    }
    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T { try JSONDecoder().decode(type, from: Data(json.utf8)) }
    private func session(alive: Bool = true, id: String? = nil, kind: String = "project", harness: String = "null") -> String {
        "{\"id\":\"\(id ?? shell)\",\"project_id\":\"\(project)\",\"worktree_id\":null,\"kind\":\"\(kind)\",\"cwd\":\"/fixture\",\"harness\":\(harness),\"alive\":\(alive),\"created_at_unix\":5}"
    }
    private func reply(shellID: String? = nil, shell body: String? = nil) throws -> JSONValue {
        let json = "{\"shell_id\":\"\(shellID ?? shell)\",\"shell\":\(body ?? session())}"
        return try decode(JSONValue.self, json)
    }

    // MARK: Request building

    func testParametersForEveryKindAndTarget() throws {
        XCTAssertEqual(try request().params, ["kind": .string("shell"), "project_id": .string(project)])
        XCTAssertEqual(try request(.worktree(tree), .claude).params, ["kind": .string("claude"), "worktree_id": .string(tree)])
        for kind in NewTerminalKind.terminalKinds {
            XCTAssertEqual(try request(.project(project), kind).params["kind"], .string(kind.rawValue))
        }
        // The default (restricted) is never spelled out.
        XCTAssertNil(try request(.project(project), .codex).params["unrestricted"])
        XCTAssertEqual(try request(.project(project), .codex, unrestricted: true).params["unrestricted"], .bool(true))
        XCTAssertEqual(try request(command: "npm run dev").params["command"], .string("npm run dev"))
    }
    func testTheTargetIsExactlyOneFullLowercaseUUID() throws {
        for bad in ["", "nope", String(project.prefix(8)), letters.uppercased(), project.replacingOccurrences(of: "-", with: ""), project + " "] {
            XCTAssertThrowsError(try request(.project(bad)), bad) { XCTAssertEqual($0 as? NewTerminalValidationError, .invalidID) }
            XCTAssertThrowsError(try request(.worktree(bad)), bad) { XCTAssertEqual($0 as? NewTerminalValidationError, .invalidID) }
        }
        // From the wire: both, neither, or the wrong type.
        let kind = JSONValue.string("shell")
        XCTAssertThrowsError(try NewTerminalRequest(params: ["kind": kind])) { XCTAssertEqual($0 as? NewTerminalValidationError, .needsOneTarget) }
        XCTAssertThrowsError(try NewTerminalRequest(params: ["kind": kind, "project_id": .string(project), "worktree_id": .string(tree)])) {
            XCTAssertEqual($0 as? NewTerminalValidationError, .needsOneTarget)
        }
        XCTAssertThrowsError(try NewTerminalRequest(params: ["kind": kind, "project_id": .number(1)])) { XCTAssertEqual($0 as? NewTerminalValidationError, .invalidID) }
        XCTAssertThrowsError(try NewTerminalRequest(params: ["kind": kind, "worktree_id": .null]))
    }
    func testKindIsRequiredAndKnown() throws {
        for kind: JSONValue in [.string("Shell"), .string("bash"), .string(""), .string("codex "), .number(1), .null, .bool(true)] {
            XCTAssertThrowsError(try NewTerminalRequest(params: ["kind": kind, "project_id": .string(project)])) { XCTAssertEqual($0 as? NewTerminalValidationError, .unknownKind) }
        }
        XCTAssertThrowsError(try NewTerminalRequest(params: ["project_id": .string(project)])) { XCTAssertEqual($0 as? NewTerminalValidationError, .unknownKind) }
    }
    func testUnrestrictedNeedsAnAgentAndABoolean() throws {
        XCTAssertThrowsError(try request(.project(project), .shell, unrestricted: true)) { XCTAssertEqual($0 as? NewTerminalValidationError, .unrestrictedNeedsAgent) }
        for kind in [NewTerminalKind.codex, .claude, .grok] { _ = try request(.project(project), kind, unrestricted: true) }
        let base: [String: JSONValue] = ["kind": .string("codex"), "project_id": .string(project)]
        for bad: JSONValue in [.string("true"), .number(1), .null, .array([])] {
            var params = base; params["unrestricted"] = bad
            XCTAssertThrowsError(try NewTerminalRequest(params: params), "\(bad)") { XCTAssertEqual($0 as? NewTerminalValidationError, .malformed) }
        }
        // An explicit false is fine, for a shell too.
        XCTAssertNoThrow(try NewTerminalRequest(params: ["kind": .string("shell"), "project_id": .string(project), "unrestricted": .bool(false)]))
    }
    func testCommandRulesMatchTheDesktop() throws {
        for good in ["ls", "npm run dev", "echo 'hi there'", "exec sleep 300", "é ü 日本", String(repeating: "a", count: 4096), "a;b && c | d"] {
            XCTAssertNoThrow(try request(command: good), good)
        }
        let cases: [(String, NewTerminalValidationError)] = [
            ("", .commandBlank), (" ", .commandBlank), ("  \t ", .commandBlank),
            (String(repeating: "a", count: 4097), .commandTooLong),
            (String(repeating: "é", count: 2049), .commandTooLong),
            ("one\ntwo", .commandHasControlCharacters), ("a\rb", .commandHasControlCharacters), ("a\u{0}b", .commandHasControlCharacters),
            ("a\u{1b}[31m", .commandHasControlCharacters), ("a\tb", .commandHasControlCharacters), ("a\u{7f}", .commandHasControlCharacters),
            ("a\u{85}b", .commandHasControlCharacters), ("a\u{2028}b", .commandHasControlCharacters), ("a\u{2029}b", .commandHasControlCharacters),
            ("-x", .commandStartsWithDash), ("--json", .commandStartsWithDash), ("--project", .commandStartsWithDash), ("-", .commandStartsWithDash)
        ]
        for (command, expected) in cases {
            XCTAssertThrowsError(try request(command: command), command.debugDescription) { XCTAssertEqual($0 as? NewTerminalValidationError, expected, command.debugDescription) }
        }
        // The limit is bytes, not characters: 2048 two-byte characters fit exactly.
        XCTAssertNoThrow(try request(command: String(repeating: "é", count: 2048)))
        // A command starts a plain shell only.
        for kind in [NewTerminalKind.codex, .claude, .grok] {
            XCTAssertThrowsError(try request(.project(project), kind, command: "ls")) { XCTAssertEqual($0 as? NewTerminalValidationError, .commandNeedsShell) }
        }
        // Not trimmed: the desktop gets exactly what was typed.
        XCTAssertEqual(try request(command: "  ls  ").params["command"], .string("  ls  "))
    }
    func testUnknownParametersAreRefused() throws {
        for extra in ["shell_id", "line", "cwd", "env", "harness", "args", "Kind"] {
            XCTAssertThrowsError(try NewTerminalRequest(params: ["kind": .string("shell"), "project_id": .string(project), extra: .string("x")]), extra) {
                XCTAssertEqual($0 as? NewTerminalValidationError, .malformed)
            }
        }
    }
    func testTheTransportChecksEveryCreateAndCloseRequest() throws {
        func check(_ method: String, _ params: [String: JSONValue]) throws { try RequestValidation.validate(method: method, params: params, id: requestID) }
        try check("shell.create", try request(.worktree(tree), .grok, unrestricted: true).params)
        try check("shell.close", try CloseTerminalRequest(shellID: shell).params)
        XCTAssertThrowsError(try check("shell.create", [:]))
        XCTAssertThrowsError(try check("shell.create", ["kind": .string("shell")]))
        XCTAssertThrowsError(try check("shell.create", ["kind": .string("shell"), "project_id": .string(project), "worktree_id": .string(tree)]))
        XCTAssertThrowsError(try check("shell.create", ["kind": .string("shell"), "project_id": .string("short")]))
        XCTAssertThrowsError(try check("shell.create", ["kind": .string("shell"), "project_id": .string(project), "unrestricted": .bool(true)]))
        XCTAssertThrowsError(try check("shell.create", ["kind": .string("codex"), "project_id": .string(project), "command": .string("ls")]))
        XCTAssertThrowsError(try check("shell.create", ["kind": .string("shell"), "project_id": .string(project), "command": .string("--help")]))
        XCTAssertThrowsError(try check("shell.close", [:]))
        XCTAssertThrowsError(try check("shell.close", ["shell_id": .string(String(shell.prefix(8)))]))
        XCTAssertThrowsError(try check("shell.close", ["shell_id": .string(shell), "force": .bool(true)]))
    }
    func testRequestTimeoutsAllowAgentStartup() {
        XCTAssertEqual(RelayClient.timeout(for: "shell.create", default: .seconds(15)), .seconds(90))
        XCTAssertEqual(RelayClient.timeout(for: "shell.close", default: .seconds(15)), .seconds(30))
        XCTAssertEqual(RelayClient.timeout(for: "shell.create", default: .seconds(120)), .seconds(120))
        XCTAssertEqual(RelayClient.timeout(for: "shell.output", default: .seconds(15)), .seconds(15))
    }

    // MARK: Replies

    func testAGoodReplyGivesTheSessionToOpen() throws {
        let parsed = try NewTerminalReply.parse(try reply(shell: session(harness: "\"claude\"")))
        XCTAssertEqual(parsed.shellID, shell)
        XCTAssertEqual(parsed.session.id, shell)
        XCTAssertEqual(parsed.session.harness, "claude")
        XCTAssertEqual(parsed.session.project_id, project)
        XCTAssertTrue(parsed.session.alive)
        XCTAssertEqual(parsed.session.title, "Claude worker")
        // Additive fields are tolerated.
        let extra = "{\"id\":\"\(shell)\",\"project_id\":\"\(project)\",\"worktree_id\":\"\(tree)\",\"kind\":\"project\",\"cwd\":\"/x\",\"harness\":null,\"alive\":true,\"created_at_unix\":9,\"future\":1}"
        XCTAssertEqual(try NewTerminalReply.parse(try reply(shell: extra)).session.worktree_id, tree)
    }
    func testUnusableRepliesAreNeverTrusted() throws {
        let unreadable: [JSONValue] = [
            try decode(JSONValue.self, "{}"),
            try decode(JSONValue.self, "{\"shell_id\":\"\(shell)\"}"),
            try decode(JSONValue.self, "{\"shell\":\(session())}"),
            try reply(shellID: "55555555-5555-4555-8555-555555555555"),            // the entry is another shell
            try reply(shellID: letters.uppercased(), shell: session(id: letters.uppercased())),
            try reply(shellID: "nope", shell: session(id: "nope")),
            try reply(shell: session(kind: "orchestrator")),
            try reply(shell: "{\"id\":\"\(shell)\"}"),
            try reply(shell: "\"text\""),
            try reply(shell: "null"),
            .string("created"), .null
        ]
        for result in unreadable {
            XCTAssertThrowsError(try NewTerminalReply.parse(result), "\(result)") { XCTAssertEqual($0 as? TerminalControlError, .unreadableReply) }
        }
    }
    func testATerminalThatAlreadyExitedIsReportedNotOpened() throws {
        XCTAssertThrowsError(try NewTerminalReply.parse(try reply(shell: session(alive: false)))) { XCTAssertEqual($0 as? TerminalControlError, .exitedRightAway) }
    }
    func testCloseRequestAndReply() throws {
        let close = try CloseTerminalRequest(shellID: shell)
        XCTAssertEqual(close.params, ["shell_id": .string(shell)])
        XCTAssertNoThrow(try close.parse(try decode(JSONValue.self, "{\"shell_id\":\"\(shell)\",\"status\":\"closed\"}")))
        for bad in ["{\"shell_id\":\"\(other)\",\"status\":\"closed\"}", "{\"shell_id\":\"\(shell)\",\"status\":\"sent\"}", "{\"shell_id\":\"\(shell)\"}", "{}", "null"] {
            XCTAssertThrowsError(try close.parse(try decode(JSONValue.self, bad)), bad) { XCTAssertEqual($0 as? TerminalControlError, .unreadableReply) }
        }
        for bad in ["", "x", String(shell.prefix(8)), letters.uppercased(), shell + " ", " " + shell] { XCTAssertThrowsError(try CloseTerminalRequest(shellID: bad), bad) }
        XCTAssertNoThrow(try CloseTerminalRequest(shellID: letters))
    }

    // MARK: Errors

    func testDesktopErrorsBecomeSpecificMessages() {
        func map(_ code: String, _ message: String = "m", _ op: TerminalControlError.Operation = .create(.codex)) -> TerminalControlError {
            TerminalControlError.from(RemoteError.rpc(code: code, message: message), operation: op)
        }
        XCTAssertEqual(map("invalid_request", "unsupported RPC method"), .unsupported)
        XCTAssertEqual(map("invalid_request", "Unsupported RPC Method"), .unsupported)
        XCTAssertEqual(map("not_found", "project not found"), .notFound(.create(.codex)))
        XCTAssertEqual(map("not_found", "unknown shell", .close), .notFound(.close))
        XCTAssertEqual(map("harness_unavailable", "codex is not installed or is not on PATH"), .harnessUnavailable(.codex))
        XCTAssertEqual(map("harness_unavailable", "x", .create(.grok)), .harnessUnavailable(.grok))
        XCTAssertEqual(map("invalid_request", "kind must be one of"), .invalid("kind must be one of"))
        XCTAssertEqual(map("cli_error", "tmux: no server running"), .failed("tmux: no server running"))
        XCTAssertEqual(map("response_too_large", "too big"), .failed("too big"))
        XCTAssertEqual(map("outcome_unknown"), .outcomeUnknown(.create(.codex)))
        XCTAssertEqual(TerminalControlError.harnessUnavailable(.codex).message, "Codex isn’t installed on the Mac (or isn’t on its PATH).")
        XCTAssertEqual(TerminalControlError.harnessUnavailable(.claude).message, "Claude isn’t installed on the Mac (or isn’t on its PATH).")
        XCTAssertEqual(TerminalControlError.unsupported.message, "Update RiWork on your Mac to open terminals from the phone.")
        XCTAssertEqual(TerminalControlError.notFound(.create(.shell)).message, "That project or worktree no longer exists on the Mac. Refresh and try again.")
    }
    func testAConnectionThatBreaksLeavesTheOutcomeUnknown() {
        let create = TerminalControlError.Operation.create(.shell)
        for error: any Error in [RemoteError.timeout, RemoteError.disconnected, RemoteError.uncertainDelivery, RemoteError.relayClosed(code: 1006, reason: nil),
                                 CancellationError(), URLError(.networkConnectionLost)] {
            let mapped = TerminalControlError.from(error, operation: create)
            XCTAssertEqual(mapped, .outcomeUnknown(create), "\(error)")
            XCTAssertTrue(mapped.outcomeIsUncertain)
        }
        XCTAssertTrue(TerminalControlError.outcomeUnknown(create).message.contains("Check the terminal list before trying again"))
        XCTAssertFalse(TerminalControlError.notFound(create).outcomeIsUncertain)
        XCTAssertFalse(TerminalControlError.unsupported.outcomeIsUncertain)
        XCTAssertTrue(TerminalControlError.unreadableReply.outcomeIsUncertain)
    }
    func testDesktopTextIsBoundedAndPrintable() {
        let noisy = "bad\u{1b}[31m\n\nthing\t" + String(repeating: "x", count: 1000)
        guard case .failed(let text) = TerminalControlError.from(RemoteError.rpc(code: "cli_error", message: noisy), operation: .close) else { return XCTFail() }
        XCTAssertLessThanOrEqual(text.count, 240)
        XCTAssertFalse(text.unicodeScalars.contains { $0.properties.generalCategory == .control })
        XCTAssertTrue(text.hasPrefix("bad [31m thing"))
        guard case .failed(let empty) = TerminalControlError.from(RemoteError.rpc(code: "cli_error", message: " \n "), operation: .close) else { return XCTFail() }
        XCTAssertEqual(empty, "The Mac could not do that.")
    }

    // MARK: Transport helpers

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
    func testTheTransportHelpersSendOnceAndMapErrors() async throws {
        let ok = Scripted(.success(try reply()))
        let created = try await ok.createTerminal(try request(.worktree(tree), .claude))
        XCTAssertEqual(created.shellID, shell)
        let sent = await ok.sent()
        XCTAssertEqual(sent.count, 1)
        XCTAssertEqual(sent[0].method, "shell.create")
        XCTAssertEqual(sent[0].params, ["kind": .string("claude"), "worktree_id": .string(tree)])

        let old = Scripted(.failure(RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")))
        do { _ = try await old.createTerminal(try request()); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? TerminalControlError, .unsupported) }
        do { try await old.closeTerminal(try CloseTerminalRequest(shellID: shell)); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? TerminalControlError, .unsupported) }

        let lost = Scripted(.failure(RemoteError.timeout))
        do { _ = try await lost.createTerminal(try request(.project(project), .codex)); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? TerminalControlError, .outcomeUnknown(.create(.codex))) }
        let attempts = await lost.sent()
        XCTAssertEqual(attempts.count, 1, "never retried")

        let closed = Scripted(.success(try decode(JSONValue.self, "{\"shell_id\":\"\(shell)\",\"status\":\"closed\"}")))
        try await closed.closeTerminal(try CloseTerminalRequest(shellID: shell))
        let closeCalls = await closed.sent()
        XCTAssertEqual(closeCalls.map(\.method), ["shell.close"])
    }

    // MARK: Targets

    private func projects() throws -> [RemoteProject] {
        [try decode(RemoteProject.self, "{\"id\":\"\(project)\",\"name\":\"Alpha\",\"root\":\"/a\",\"created_at\":1}"),
         try decode(RemoteProject.self, "{\"id\":\"\(other)\",\"name\":\"Beta\",\"root\":\"/b\",\"created_at\":2}")]
    }
    private func worktree(_ id: String, _ branch: String, primary: Bool, in projectID: String? = nil) throws -> RemoteWorktree {
        try decode(RemoteWorktree.self, "{\"id\":\"\(id)\",\"project_id\":\"\(projectID ?? project)\",\"branch\":\"\(branch)\",\"path\":\"/a/\(branch)\",\"is_primary\":\(primary),\"created_at\":1}")
    }
    func testTargetsListTheMainWorktreeFirstAndOnlyOfThisProject() throws {
        let feature = try worktree(tree, "feature", primary: false)
        let main = try worktree("66666666-6666-4666-8666-666666666666", "main", primary: true)
        let foreign = try worktree("77777777-7777-4777-8777-777777777777", "elsewhere", primary: true, in: other)
        let options = NewTerminalTargets.options(projectID: project, projects: try projects(), worktrees: [feature, main, foreign])
        XCTAssertEqual(options.map(\.id), ["worktree:\(main.id)", "worktree:\(feature.id)"])
        XCTAssertEqual(options.map(\.title), ["Alpha · main", "Alpha · feature"])
        XCTAssertEqual(options[0].branchLabel, "main · root")
        XCTAssertEqual(options[1].branchLabel, "feature")
        XCTAssertEqual(options.map(\.projectID), [project, project])
        // Without worktrees the project itself is the only choice.
        let bare = NewTerminalTargets.options(projectID: project, projects: try projects(), worktrees: [])
        XCTAssertEqual(bare.map(\.id), ["project:\(project)"])
        XCTAssertNil(bare[0].branchLabel)
        // A project the phone has not listed yet still gets a row.
        XCTAssertEqual(NewTerminalTargets.options(projectID: project, projects: [], worktrees: []).map(\.title), ["Project"])
    }
    func testPreselectionPrefersTheViewedWorktreeThenTheMainOne() throws {
        let feature = try worktree(tree, "feature", primary: false)
        let main = try worktree("66666666-6666-4666-8666-666666666666", "main", primary: true)
        let options = NewTerminalTargets.options(projectID: project, projects: try projects(), worktrees: [feature, main])
        XCTAssertEqual(NewTerminalTargets.preselected(in: options, selectedWorktreeID: feature.id), 1)
        XCTAssertEqual(NewTerminalTargets.preselected(in: options, selectedWorktreeID: main.id), 0)
        XCTAssertEqual(NewTerminalTargets.preselected(in: options, selectedWorktreeID: nil), 0)
        XCTAssertEqual(NewTerminalTargets.preselected(in: options, selectedWorktreeID: "99999999-9999-4999-8999-999999999999"), 0)
        // No primary worktree known: the first row.
        let noPrimary = NewTerminalTargets.options(projectID: project, projects: try projects(), worktrees: [feature])
        XCTAssertEqual(NewTerminalTargets.preselected(in: noPrimary, selectedWorktreeID: nil), 0)
        XCTAssertEqual(NewTerminalTargets.preselected(in: [], selectedWorktreeID: nil), 0)
    }
    func testTargetsMakeWorktreeOrProjectRequests() throws {
        let feature = try worktree(tree, "feature", primary: false)
        let options = NewTerminalTargets.options(projectID: project, projects: try projects(), worktrees: [feature])
        XCTAssertEqual(try NewTerminalRequest(target: options[0].requestTarget, kind: .shell).params["worktree_id"], .string(tree))
        let bare = NewTerminalTargets.options(projectID: other, projects: try projects(), worktrees: [])
        XCTAssertEqual(try NewTerminalRequest(target: bare[0].requestTarget, kind: .shell).params["project_id"], .string(other))
    }

    // MARK: Form and keyboard

    private func form(kind: NewTerminalKind = .shell) throws -> NewTerminalForm {
        let options = NewTerminalTargets.options(projectID: project, projects: try projects(),
                                                 worktrees: [try worktree(tree, "main", primary: true), try worktree("66666666-6666-4666-8666-666666666666", "dev", primary: false)])
        return NewTerminalForm(targets: options, targetIndex: 0, kind: kind)
    }
    func testUpAndDownMoveTheKindAndWrap() throws {
        var f = try form()
        XCTAssertEqual(f.focus, .kind)
        f.handle(.down); XCTAssertEqual(f.kind, .codex)
        f.handle(.down); XCTAssertEqual(f.kind, .claude)
        f.handle(.down); XCTAssertEqual(f.kind, .grok)
        f.handle(.down); XCTAssertEqual(f.kind, .shell, "wraps")
        f.handle(.up); XCTAssertEqual(f.kind, .grok, "wraps backwards")
        f.handle(.up); XCTAssertEqual(f.kind, .claude)
    }
    func testTabAndArrowsMoveTheFocusRingAndSkipAHiddenToggle() throws {
        var f = try form()
        XCTAssertEqual(f.fields, [.target, .kind, .create])
        f.handle(.tab); XCTAssertEqual(f.focus, .create)
        f.handle(.tab); XCTAssertEqual(f.focus, .target, "wraps")
        f.handle(.backTab); XCTAssertEqual(f.focus, .create)
        f.handle(.left); XCTAssertEqual(f.focus, .kind)
        f.handle(.right); XCTAssertEqual(f.focus, .create)
        f.select(kind: .codex)
        XCTAssertEqual(f.fields, [.target, .kind, .unrestricted, .create])
        f.focus = .kind
        f.handle(.tab); XCTAssertEqual(f.focus, .unrestricted)
        f.handle(.tab); XCTAssertEqual(f.focus, .create)
    }
    func testUpAndDownFromAnotherControlChangeTheKindAndReturnFocusToIt() throws {
        var f = try form()
        f.focus = .create
        f.handle(.down)
        XCTAssertEqual(f.kind, .codex)
        XCTAssertEqual(f.focus, .kind)
        // On the target row they choose the target instead.
        f.focus = .target
        let before = f.targetIndex
        f.handle(.down)
        XCTAssertEqual(f.targetIndex, before + 1)
        XCTAssertEqual(f.kind, .codex)
        XCTAssertEqual(f.focus, .target)
        f.handle(.up); f.handle(.up)
        XCTAssertEqual(f.targetIndex, f.targets.count - 1, "wraps")
    }
    func testSpaceTogglesUnrestrictedOnlyWhereItIsFocusedAndNeverCarriesOver() throws {
        var f = try form(kind: .claude)
        f.handle(.space)
        XCTAssertFalse(f.unrestricted, "space does nothing on another control")
        f.focus = .unrestricted
        f.handle(.space); XCTAssertTrue(f.unrestricted)
        XCTAssertEqual(try f.request().params["unrestricted"], .bool(true))
        f.handle(.space); XCTAssertFalse(f.unrestricted)
        f.handle(.space)
        // Changing the kind switches it off, and a shell cannot have it at all.
        f.select(kind: .grok)
        XCTAssertFalse(f.unrestricted)
        f.setUnrestricted(true); XCTAssertTrue(f.unrestricted)
        f.select(kind: .shell)
        XCTAssertFalse(f.unrestricted)
        f.setUnrestricted(true); XCTAssertFalse(f.unrestricted)
        XCTAssertEqual(f.focus, .kind, "focus leaves the toggle when it disappears")
        XCTAssertNil(try f.request().params["unrestricted"])
    }
    func testTheFormBuildsTheRequestForWhatIsChosen() throws {
        var f = try form()
        XCTAssertEqual(try f.request().params, ["kind": .string("shell"), "worktree_id": .string(tree)])
        f.handle(.down); f.handle(.down)
        XCTAssertEqual(try f.request().kind, .claude)
        f.select(targetAt: 1)
        XCTAssertEqual(try f.request().params["worktree_id"], .string("66666666-6666-4666-8666-666666666666"))
        XCTAssertNil(try f.request().params["project_id"])
        let bare = NewTerminalForm(targets: [.project(id: project, name: "Alpha")])
        XCTAssertEqual(try bare.request().params["project_id"], .string(project))
        XCTAssertThrowsError(try NewTerminalForm(targets: []).request()) { XCTAssertEqual($0 as? NewTerminalValidationError, .needsOneTarget) }
        // Out-of-range indexes are clamped.
        XCTAssertEqual(NewTerminalForm(targets: f.targets, targetIndex: 99).targetIndex, f.targets.count - 1)
        XCTAssertEqual(NewTerminalForm(targets: f.targets, targetIndex: -4).targetIndex, 0)
    }
    func testKindsAreOrderedAndNamed() {
        XCTAssertEqual(NewTerminalKind.terminalKinds.map(\.title), ["Shell", "Codex", "Claude", "Grok"])
        XCTAssertEqual(NewTerminalKind.terminalKinds.map(\.isAgent), [false, true, true, true])
        // One chat for both providers comes after the terminals, only where the desktop has chats.
        XCTAssertEqual(NewTerminalKind.allCases.map(\.title), ["Shell", "Codex", "Claude", "Grok", "Chat"])
        XCTAssertEqual(NewTerminalKind.offered(chats: false), NewTerminalKind.terminalKinds)
        XCTAssertEqual(NewTerminalKind.offered(chats: true), NewTerminalKind.allCases)
        XCTAssertEqual(NewTerminalKind.allCases.map(\.isChat), [false, false, false, false, true])
        XCTAssertTrue(NewTerminalKind.chat.isAgent, "its Unrestricted switch is the Full mode")
        // What an older version remembered per provider is the chat, and says which provider.
        XCTAssertEqual(NewTerminalKind.remembered("codex_chat"), .chat); XCTAssertEqual(NewTerminalKind.remembered("claude_chat"), .chat)
        XCTAssertEqual(NewTerminalKind.legacyChatProvider("claude_chat"), .claude); XCTAssertNil(NewTerminalKind.legacyChatProvider("chat"))
        XCTAssertEqual(NewTerminalKind.remembered("grok"), .grok); XCTAssertNil(NewTerminalKind.remembered("bash"))
        XCTAssertEqual(NewTerminalKind.standard, .shell)
        XCTAssertEqual(NewTerminalKind.shell.moved(by: -1), .grok)
        XCTAssertEqual(NewTerminalKind.grok.moved(by: 5), .shell)
        XCTAssertEqual(NewTerminalKind.codex.moved(by: -5), .shell)
        XCTAssertNil(NewTerminalKind(rawValue: "bash"))
    }

    // MARK: Chats in the same sheet

    func testChatKindsAreOpenedByChatCreateAndNeverByShellCreate() throws {
        XCTAssertThrowsError(try request(.project(project), .chat)) { XCTAssertEqual($0 as? NewTerminalValidationError, .unknownKind) }
        for word in ["chat", "codex_chat", "claude_chat"] {
            XCTAssertThrowsError(try NewTerminalRequest(params: ["kind": .string(word), "project_id": .string(project)])) { XCTAssertEqual($0 as? NewTerminalValidationError, .unknownKind) }
        }
        let targets = [NewTerminalTarget.project(id: project, name: "Alpha")]
        var form = NewTerminalForm(targets: targets, kind: .chat, kinds: NewTerminalKind.allCases)
        XCTAssertEqual(form.kind, .chat); XCTAssertEqual(form.chatProvider, .codex, "Codex unless said otherwise")
        XCTAssertEqual(try form.submission(), .chat(try ChatCreateRequest(provider: .codex, target: .project(project))))
        XCTAssertNil(try ChatCreateRequest(provider: .codex, target: .project(project)).params["approval_mode"], "restricted: the desktop's default mode")
        XCTAssertThrowsError(try form.request(), "the terminal request is for terminals")
        form.setUnrestricted(true)
        XCTAssertEqual(try form.submission(), .chat(try ChatCreateRequest(provider: .codex, target: .project(project), approvalMode: .full)))
        // The provider is the model's: choosing a Claude row keeps the chat and its mode.
        form.chooseChatRow(.providerDefault(.claude))
        XCTAssertEqual(form.kind, .chat)
        XCTAssertEqual(try form.submission(), .chat(try ChatCreateRequest(provider: .claude, target: .project(project), approvalMode: .full)))
        form.selectChat(.codex)
        XCTAssertEqual(try form.submission(), .chat(try ChatCreateRequest(provider: .codex, target: .project(project), approvalMode: .full)))
        form.select(kind: .grok)
        XCTAssertFalse(form.unrestricted, "chosen on purpose, each time")
        XCTAssertEqual(try form.submission(), .terminal(try NewTerminalRequest(target: .project(project), kind: .grok)))
    }
    func testAChatInAWorktreeIsAskedForThere() throws {
        let tree = NewTerminalTarget.worktree(id: self.tree, projectID: project, projectName: "Alpha", branch: "main", isPrimary: true)
        let form = NewTerminalForm(targets: [tree], kind: .chat, kinds: NewTerminalKind.allCases, chatProvider: .claude)
        XCTAssertEqual(try form.submission(), .chat(try ChatCreateRequest(provider: .claude, target: .worktree(self.tree))))
        XCTAssertThrowsError(try NewTerminalForm(targets: [], kind: .chat, kinds: NewTerminalKind.allCases).submission()) { XCTAssertEqual($0 as? NewTerminalValidationError, .needsOneTarget) }
    }
    func testWithoutChatsTheFormOffersTerminalsOnlyAndForgetsARememberedChat() {
        let targets = [NewTerminalTarget.project(id: project, name: "Alpha")]
        var form = NewTerminalForm(targets: targets, kind: .chat)
        XCTAssertEqual(form.kind, .shell, "remembered on a desktop that had chats")
        XCTAssertEqual(form.kinds, NewTerminalKind.terminalKinds)
        form.select(kind: .chat)
        XCTAssertEqual(form.kind, .shell, "not on offer")
        form.selectChat(.claude)
        XCTAssertEqual(form.kind, .shell, "not on offer")
        form.select(kind: .grok)
        form.handle(.down)
        XCTAssertEqual(form.kind, .shell, "Grok is the last row")
    }
    func testTheArrowsStepThroughTheChatRowsToo() {
        let targets = [NewTerminalTarget.project(id: project, name: "Alpha")]
        var form = NewTerminalForm(targets: targets, kind: .grok, kinds: NewTerminalKind.allCases)
        form.handle(.down); XCTAssertEqual(form.kind, .chat)
        form.handle(.down); XCTAssertEqual(form.kind, .shell, "wraps")
        form.handle(.up); XCTAssertEqual(form.kind, .chat)
        XCTAssertEqual(form.fields, [.target, .kind, .chatModel, .unrestricted, .create], "a chat has its model row between the kind and the toggle; a terminal has none")
        form.select(kind: .grok)
        XCTAssertEqual(form.fields, [.target, .kind, .unrestricted, .create])
    }

    // MARK: The desktop's Settings decide

    func testWhereTheDesktopDecidesAgentTerminalsHaveNoSwitchAndAskForNothing() throws {
        let targets = [NewTerminalTarget.project(id: project, name: "Alpha")]
        var form = NewTerminalForm(targets: targets, kind: .codex, kinds: NewTerminalKind.allCases)
        form.agentsFollowDesktop = true
        for kind in [NewTerminalKind.codex, .claude, .grok] {
            form.select(kind: kind)
            XCTAssertFalse(form.offersUnrestricted, kind.title)
            XCTAssertEqual(form.fields, [.target, .kind, .create], "no switch: the Mac's Agent terminals run unrestricted decides")
            form.setUnrestricted(true)
            XCTAssertFalse(form.unrestricted)
            XCTAssertEqual(try form.request().params, ["kind": .string(kind.rawValue), "project_id": .string(project)], "unrestricted is left out")
        }
        // A chat keeps its own: its Full mode is a choice of the chat, not the terminals' setting.
        form.select(kind: .chat)
        XCTAssertTrue(form.offersUnrestricted)
        XCTAssertEqual(form.fields, [.target, .kind, .chatModel, .unrestricted, .create])
        form.setUnrestricted(true)
        XCTAssertEqual(try form.submission(), .chat(try ChatCreateRequest(provider: .codex, target: .project(project), approvalMode: .full)))
        // A desktop from before it: the switch is there, as it always was.
        var older = NewTerminalForm(targets: targets, kind: .claude, kinds: NewTerminalKind.allCases)
        XCTAssertTrue(older.offersUnrestricted)
        older.setUnrestricted(true)
        XCTAssertEqual(try older.request().params["unrestricted"], .bool(true))
    }
}
