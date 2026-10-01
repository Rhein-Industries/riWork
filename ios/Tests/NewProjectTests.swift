import XCTest
@testable import RiWorkCore

final class NewProjectTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let other = "22222222-2222-4222-8222-222222222222"
    private let requestID = "55555555-5555-4555-8555-555555555555"
    private let letters = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"

    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T { try JSONDecoder().decode(type, from: Data(json.utf8)) }
    private func reply(id: String? = nil, name: String = "My App", project body: String? = nil) throws -> JSONValue {
        let entry = body ?? "{\"id\":\"\(id ?? project)\",\"name\":\"\(name)\",\"root\":\"/Users/me/Documents/riwork/\(name)\",\"created_at\":1790000000}"
        return try decode(JSONValue.self, "{\"project_id\":\"\(id ?? project)\",\"project\":\(entry)}")
    }
    private func problem(_ name: String) -> NewProjectValidationError? { NewProjectName.problem(with: name) }

    // MARK: The name

    func testOrdinaryNamesAreAccepted() {
        for name in ["app", "My App", "my-project", "my_project", "a", "x.y", "v1.2", "node-app 2", "日本語のプロジェクト", "café", "e\u{301}", "😀 launch",
                     "a-b", "a-", "a.", "a b c", "A+B", "100%", "it's", "(draft)", "a:b", "tilde~", "name with  two spaces"] {
            XCTAssertNil(problem(name), name)
        }
        // A dot or a dash anywhere but first is fine.
        XCTAssertNil(problem("a.b-c.d"))
    }
    func testTheNameRulesMatchTheDesktop() {
        let cases: [(String, NewProjectValidationError)] = [
            ("", .empty),
            (" ", .surroundingWhitespace), (" a", .surroundingWhitespace), ("a ", .surroundingWhitespace),
            ("\u{a0}a", .surroundingWhitespace), ("a\u{a0}", .surroundingWhitespace),            // no-break space
            ("\u{3000}a", .surroundingWhitespace), ("a\u{3000}", .surroundingWhitespace),        // ideographic space
            ("\u{2003}a", .surroundingWhitespace), ("a\u{202f}", .surroundingWhitespace),         // em space, narrow no-break space
            ("a\u{2028}", .surroundingWhitespace), ("\u{2029}a", .surroundingWhitespace),
            ("\ta", .surroundingWhitespace), ("a\n", .surroundingWhitespace), ("a\r\n", .surroundingWhitespace),
            ("a\nb", .controlCharacters), ("a\rb", .controlCharacters), ("a\tb", .controlCharacters), ("a\u{0}b", .controlCharacters),
            ("a\u{1b}[2Jb", .controlCharacters), ("a\u{7f}b", .controlCharacters), ("a\u{85}b", .controlCharacters),
            ("a\u{9f}b", .controlCharacters), ("a\u{2028}b", .controlCharacters), ("a\u{2029}b", .controlCharacters),
            ("a/b", .separator), ("/a", .separator), ("a/", .separator), ("../x", .separator), ("a\\b", .separator), ("\\a", .separator), ("C:\\x", .separator),
            (".", .leadingDot), ("..", .leadingDot), (".git", .leadingDot), (".GIT", .leadingDot), (".hidden", .leadingDot), ("...", .leadingDot),
            ("-", .leadingDash), ("-x", .leadingDash), ("--json", .leadingDash), ("--name", .leadingDash), ("-rf", .leadingDash)
        ]
        for (name, expected) in cases { XCTAssertEqual(problem(name), expected, name.debugDescription) }
    }
    func testTheLengthLimitCountsScalarsAndBytes() {
        XCTAssertNil(problem(String(repeating: "a", count: 100)))
        XCTAssertEqual(problem(String(repeating: "a", count: 101)), .tooLong)
        // Scalars, not what a person counts: "e" plus a combining accent is two, and an emoji with a skin tone is two.
        XCTAssertEqual("e\u{301}".count, 1)
        XCTAssertNil(problem(String(repeating: "e\u{301}", count: 50)), "100 scalars in 50 characters")
        XCTAssertEqual(problem(String(repeating: "e\u{301}", count: 51)), .tooLong, "102 scalars in 51 characters")
        XCTAssertEqual("👍🏽".count, 1)
        XCTAssertEqual("👍🏽".unicodeScalars.count, 2)
        XCTAssertEqual(problem(String(repeating: "👍🏽", count: 51)), .tooLong)
        // 255 bytes at most: 63 four-byte scalars is 252, 64 is 256; 100 two-byte scalars is 200.
        XCTAssertNil(problem(String(repeating: "é", count: 100)))
        XCTAssertNil(problem(String(repeating: "😀", count: 63)))
        XCTAssertEqual(problem(String(repeating: "😀", count: 64)), .tooManyBytes)
        XCTAssertEqual(problem(String(repeating: "😀", count: 101)), .tooLong, "the scalar count is looked at first")
        XCTAssertNil(problem(String(repeating: "日", count: 85)), "85 x 3 bytes is exactly 255")
        XCTAssertEqual(problem(String(repeating: "日", count: 86)), .tooManyBytes)
    }
    func testTrimmingMatchesRustsTrim() {
        XCTAssertEqual(NewProjectName.trimmed("  my app \n"), "my app")
        XCTAssertEqual(NewProjectName.trimmed("\u{a0}\u{3000}x\u{2003}\u{202f}"), "x")
        XCTAssertEqual(NewProjectName.trimmed("a  b"), "a  b")
        XCTAssertEqual(NewProjectName.trimmed(" \t "), "")
        XCTAssertEqual(NewProjectName.trimmed(""), "")
        // An inner control character is not white space and stays (it is then refused).
        XCTAssertEqual(NewProjectName.trimmed(" a\u{1b}b "), "a\u{1b}b")
        // Zero width characters are not white space.
        XCTAssertEqual(NewProjectName.trimmed("\u{200b}x"), "\u{200b}x")
    }
    func testLiveCheckSeparatesNothingTypedYetFromAMistake() {
        XCTAssertEqual(NewProjectName.check(typed: ""), .empty)
        XCTAssertEqual(NewProjectName.check(typed: "   "), .empty, "only spaces is still nothing")
        XCTAssertEqual(NewProjectName.check(typed: "  my app  "), .valid("my app"), "what is typed is trimmed")
        XCTAssertEqual(NewProjectName.check(typed: ".git"), .invalid(.leadingDot))
        XCTAssertEqual(NewProjectName.check(typed: " -x"), .invalid(.leadingDash))
        XCTAssertEqual(NewProjectName.check(typed: "a/b"), .invalid(.separator))
        XCTAssertEqual(NewProjectName.check(typed: String(repeating: "a", count: 101)), .invalid(.tooLong))
        XCTAssertNil(NewProjectName.check(typed: "").problem)
        XCTAssertNil(NewProjectName.check(typed: "ok").problem)
        XCTAssertEqual(NewProjectName.check(typed: "ok").name, "ok")
        XCTAssertNil(NewProjectName.check(typed: "a/b").name)
        for problem in [NewProjectValidationError.empty, .surroundingWhitespace, .tooLong, .tooManyBytes, .controlCharacters, .separator, .leadingDot, .leadingDash, .malformed] {
            XCTAssertFalse(problem.message.isEmpty)
            XCTAssertEqual(problem.errorDescription, problem.message)
        }
        XCTAssertTrue(NewProjectValidationError.tooLong.message.contains("100"))
    }

    // MARK: Request building

    func testParametersAreTheNameAndGitOnlyWhenItIsOff() throws {
        XCTAssertEqual(try NewProjectRequest(name: "My App").params, ["name": .string("My App")])
        XCTAssertEqual(try NewProjectRequest(name: "My App", git: true).params, ["name": .string("My App")], "the default is never spelled out")
        XCTAssertEqual(try NewProjectRequest(name: "My App", git: false).params, ["name": .string("My App"), "git": .bool(false)])
        XCTAssertTrue(try NewProjectRequest(name: "x").git)
    }
    func testTheRequestRefusesAnUntrimmedOrBadName() {
        for (name, expected) in [(" app", NewProjectValidationError.surroundingWhitespace), ("app\n", .surroundingWhitespace), ("", .empty), (".x", .leadingDot), ("-x", .leadingDash), ("a/b", .separator)] {
            XCTAssertThrowsError(try NewProjectRequest(name: name), name.debugDescription) { XCTAssertEqual($0 as? NewProjectValidationError, expected) }
        }
    }
    func testWireParametersAreReadBackThroughTheSameRules() throws {
        let request = try NewProjectRequest(params: ["name": .string("a b"), "git": .bool(false)])
        XCTAssertEqual(request.name, "a b")
        XCTAssertFalse(request.git)
        XCTAssertTrue(try NewProjectRequest(params: ["name": .string("a")]).git)
        XCTAssertTrue(try NewProjectRequest(params: ["name": .string("a"), "git": .bool(true)]).git)
        let malformed: [[String: JSONValue]] = [
            [:], ["git": .bool(true)],
            ["name": .null], ["name": .number(1)], ["name": .bool(true)], ["name": .array([.string("a")])], ["name": .object([:])],
            ["name": .string("a"), "git": .null], ["name": .string("a"), "git": .string("false")], ["name": .string("a"), "git": .number(0)],
            ["name": .string("a"), "path": .string("/tmp/x")], ["name": .string("a"), "root": .string("/tmp")], ["name": .string("a"), "cwd": .string("/")],
            ["name": .string("a"), "folder": .string("x")], ["name": .string("a"), "Git": .bool(true)], ["name": .string("a"), "kind": .string("shell")]
        ]
        for params in malformed {
            XCTAssertThrowsError(try NewProjectRequest(params: params), "\(params)") { XCTAssertEqual($0 as? NewProjectValidationError, .malformed, "\(params)") }
        }
        XCTAssertThrowsError(try NewProjectRequest(params: ["name": .string(" a")])) { XCTAssertEqual($0 as? NewProjectValidationError, .surroundingWhitespace) }
        XCTAssertThrowsError(try NewProjectRequest(params: ["name": .string(".a")])) { XCTAssertEqual($0 as? NewProjectValidationError, .leadingDot) }
    }
    func testTheTransportChecksEveryProjectRequest() throws {
        func check(_ params: [String: JSONValue]) throws { try RequestValidation.validate(method: "project.create", params: params, id: requestID) }
        try check(try NewProjectRequest(name: "My App").params)
        try check(try NewProjectRequest(name: "My App", git: false).params)
        XCTAssertThrowsError(try check([:]))
        XCTAssertThrowsError(try check(["name": .string("")]))
        XCTAssertThrowsError(try check(["name": .string(" x")]))
        XCTAssertThrowsError(try check(["name": .string("a/b")]))
        XCTAssertThrowsError(try check(["name": .string(".hidden")]))
        XCTAssertThrowsError(try check(["name": .string("-x")]))
        XCTAssertThrowsError(try check(["name": .string("a"), "git": .string("no")]))
        XCTAssertThrowsError(try check(["name": .string("a"), "path": .string("/tmp")]))
        XCTAssertThrowsError(try RequestValidation.validate(method: "project.create", params: ["name": .string("a")], id: "not-a-uuid"))
        // The other methods are as they were.
        XCTAssertThrowsError(try RequestValidation.validate(method: "project.delete", params: ["name": .string("a")], id: requestID))
        XCTAssertThrowsError(try RequestValidation.validate(method: "projects.create", params: ["name": .string("a")], id: requestID))
    }
    func testTheRequestTimeoutAllowsASlowGitInit() {
        XCTAssertEqual(RelayClient.timeout(for: "project.create", default: .seconds(15)), .seconds(90))
        XCTAssertEqual(RelayClient.timeout(for: "project.create", default: .seconds(120)), .seconds(120))
        XCTAssertEqual(RelayClient.timeout(for: "projects.list", default: .seconds(15)), .seconds(15))
    }

    // MARK: Replies

    func testAGoodReplyGivesTheProjectToOpen() throws {
        let parsed = try NewProjectReply.parse(try reply(name: "My App"), requestedName: "My App")
        XCTAssertEqual(parsed.project.id, project)
        XCTAssertEqual(parsed.project.name, "My App")
        XCTAssertEqual(parsed.project.created_at, 1_790_000_000)
        // Additive fields are tolerated, at both levels.
        let extra = try decode(JSONValue.self, "{\"project_id\":\"\(project)\",\"project\":{\"id\":\"\(project)\",\"name\":\"x\",\"root\":\"/r/x\",\"created_at\":5,\"git\":true},\"warning\":\"none\"}")
        XCTAssertEqual(try NewProjectReply.parse(extra, requestedName: "x").project.root, "/r/x")
    }
    func testUnusableRepliesAreNeverTrusted() throws {
        let unreadable: [JSONValue] = [
            try decode(JSONValue.self, "{}"),
            try decode(JSONValue.self, "{\"project_id\":\"\(project)\"}"),
            try decode(JSONValue.self, "{\"project\":{\"id\":\"\(project)\",\"name\":\"My App\",\"root\":\"/r\",\"created_at\":1}}"),
            try reply(id: project, project: "{\"id\":\"\(other)\",\"name\":\"My App\",\"root\":\"/r\",\"created_at\":1}"),   // the entry is another project
            try reply(id: "nope", name: "My App"),
            try reply(id: letters.uppercased(), name: "My App"),
            try reply(id: String(project.prefix(8)), name: "My App"),
            try reply(name: "Other Name"),                                                                                 // not the name that was asked for
            try reply(project: "{\"id\":\"\(project)\"}"),
            try reply(project: "{\"id\":\"\(project)\",\"name\":\"My App\",\"root\":7,\"created_at\":1}"),
            try reply(project: "{\"id\":\"\(project)\",\"name\":\"My App\",\"root\":\"/r\",\"created_at\":\"now\"}"),
            try reply(project: "\"text\""),
            try reply(project: "null"),
            .string("created"), .null, .array([])
        ]
        for result in unreadable {
            XCTAssertThrowsError(try NewProjectReply.parse(result, requestedName: "My App"), "\(result)") { XCTAssertEqual($0 as? ProjectCreateError, .unreadableReply) }
        }
    }

    // MARK: Errors

    func testDesktopErrorsBecomeSpecificMessages() {
        func map(_ code: String, _ message: String = "m") -> ProjectCreateError { ProjectCreateError.from(RemoteError.rpc(code: code, message: message)) }
        XCTAssertEqual(map("invalid_request", "unsupported RPC method"), .unsupported)
        XCTAssertEqual(map("invalid_request", "Unsupported RPC Method"), .unsupported)
        XCTAssertEqual(map("already_exists", "A project named “foo” already exists on the Mac."), .alreadyExists("A project named “foo” already exists on the Mac."))
        XCTAssertEqual(ProjectCreateError.alreadyExists("A folder named “foo” is already in the Mac’s projects folder.").message, "A folder named “foo” is already in the Mac’s projects folder.")
        XCTAssertEqual(map("invalid_request", "name must not start with a dot"), .invalid("name must not start with a dot"))
        XCTAssertEqual(map("cli_error", "git init failed: no space left"), .failed("git init failed: no space left"))
        XCTAssertEqual(map("cli_error", "the installed riwork CLI cannot create projects from the phone; update RiWork"), .failed("the installed riwork CLI cannot create projects from the phone; update RiWork"))
        XCTAssertEqual(map("not_found", "device revoked"), .failed("device revoked"))
        XCTAssertEqual(map("response_too_large", "too big"), .failed("too big"))
        XCTAssertEqual(map("outcome_unknown"), .outcomeUnknown)
        XCTAssertEqual(ProjectCreateError.unsupported.message, "Update RiWork on your Mac to create projects from the phone.")
        XCTAssertEqual(ProjectCreateError.invalid("x").message, "The Mac refused the request: x")
        XCTAssertFalse(ProjectCreateError.alreadyExists("x").outcomeIsUncertain)
        XCTAssertFalse(ProjectCreateError.failed("x").outcomeIsUncertain)
        XCTAssertEqual(ProjectCreateError.busy.errorDescription, ProjectCreateError.busy.message)
    }
    func testAConnectionThatBreaksLeavesTheOutcomeUnknown() {
        for error: any Error in [RemoteError.timeout, RemoteError.disconnected, RemoteError.uncertainDelivery, RemoteError.relayClosed(code: 1006, reason: nil),
                                 CancellationError(), URLError(.networkConnectionLost)] {
            let mapped = ProjectCreateError.from(error)
            XCTAssertEqual(mapped, .outcomeUnknown, "\(error)")
            XCTAssertTrue(mapped.outcomeIsUncertain)
        }
        let message = ProjectCreateError.outcomeUnknown.message
        XCTAssertTrue(message.contains("Check the project list before trying again"))
        XCTAssertTrue(message.contains("creating it again says so"), "a second try is itself informative")
        XCTAssertTrue(ProjectCreateError.unreadableReply.outcomeIsUncertain)
        XCTAssertFalse(ProjectCreateError.unsupported.outcomeIsUncertain)
    }
    func testDesktopTextIsBoundedAndPrintable() {
        let noisy = "bad\u{1b}[31m\n\nthing\t" + String(repeating: "x", count: 1000)
        guard case .failed(let text) = ProjectCreateError.from(RemoteError.rpc(code: "cli_error", message: noisy)) else { return XCTFail() }
        XCTAssertLessThanOrEqual(text.count, 240)
        XCTAssertFalse(text.unicodeScalars.contains { $0.properties.generalCategory == .control })
        guard case .alreadyExists(let exists) = ProjectCreateError.from(RemoteError.rpc(code: "already_exists", message: "a\nb\u{1b}c")) else { return XCTFail() }
        XCTAssertEqual(exists, "a b c")
        guard case .failed(let empty) = ProjectCreateError.from(RemoteError.rpc(code: "cli_error", message: " \n ")) else { return XCTFail() }
        XCTAssertEqual(empty, "The Mac could not do that.")
    }

    // MARK: Transport

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
        let ok = Scripted(.success(try reply(name: "My App")))
        let created = try await ok.createProject(try NewProjectRequest(name: "My App", git: false))
        XCTAssertEqual(created.project.id, project)
        let sent = await ok.sent()
        XCTAssertEqual(sent.count, 1)
        XCTAssertEqual(sent[0].method, "project.create")
        XCTAssertEqual(sent[0].params, ["name": .string("My App"), "git": .bool(false)])

        let old = Scripted(.failure(RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")))
        do { _ = try await old.createProject(try NewProjectRequest(name: "x")); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? ProjectCreateError, .unsupported) }

        let taken = Scripted(.failure(RemoteError.rpc(code: "already_exists", message: "A project named “x” already exists on the Mac.")))
        do { _ = try await taken.createProject(try NewProjectRequest(name: "x")); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? ProjectCreateError, .alreadyExists("A project named “x” already exists on the Mac.")) }

        let lost = Scripted(.failure(RemoteError.timeout))
        do { _ = try await lost.createProject(try NewProjectRequest(name: "x")); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? ProjectCreateError, .outcomeUnknown) }
        let attempts = await lost.sent()
        XCTAssertEqual(attempts.count, 1, "never retried")

        // An answer for another project is not this request's answer.
        let wrong = Scripted(.success(try reply(name: "Other")))
        do { _ = try await wrong.createProject(try NewProjectRequest(name: "My App")); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? ProjectCreateError, .unreadableReply) }
    }

    // MARK: Form and keyboard

    func testTheFormStartsOnTheNameWithGitOn() {
        let form = NewProjectForm()
        XCTAssertEqual(form.focus, .name)
        XCTAssertTrue(form.git)
        XCTAssertEqual(form.name, "")
        XCTAssertEqual(form.check, .empty)
        XCTAssertFalse(form.isReady)
    }
    func testTabAndArrowsMoveTheRingRoundTheThreeControls() {
        var form = NewProjectForm()
        form.handle(.tab); XCTAssertEqual(form.focus, .git)
        form.handle(.tab); XCTAssertEqual(form.focus, .create)
        form.handle(.tab); XCTAssertEqual(form.focus, .name, "wraps")
        form.handle(.backTab); XCTAssertEqual(form.focus, .create, "wraps backwards")
        form.handle(.backTab); XCTAssertEqual(form.focus, .git)
        form.handle(.down); XCTAssertEqual(form.focus, .create)
        form.handle(.up); XCTAssertEqual(form.focus, .git)
        form.handle(.right); XCTAssertEqual(form.focus, .create)
        form.handle(.left); XCTAssertEqual(form.focus, .git)
        form.moveFocus(by: 4); XCTAssertEqual(form.focus, .create)
        form.moveFocus(by: -7); XCTAssertEqual(form.focus, .git)
    }
    func testSpaceFlipsTheSwitchOnlyWhenItHasTheRing() {
        var form = NewProjectForm()
        form.handle(.space)
        XCTAssertTrue(form.git, "the name has the ring: a space there is typing")
        form.focus = .git
        form.handle(.space); XCTAssertFalse(form.git)
        form.handle(.space); XCTAssertTrue(form.git)
        form.focus = .create
        form.handle(.space)
        XCTAssertTrue(form.git, "on Create a space presses the button (the sheet does that), it flips nothing")
        form.setGit(false)
        XCTAssertFalse(form.git)
    }
    func testTheFormMakesTheRequestFromWhatIsTyped() throws {
        var form = NewProjectForm(name: "  My App ")
        XCTAssertTrue(form.isReady)
        XCTAssertEqual(form.trimmedName, "My App")
        XCTAssertEqual(try form.request().params, ["name": .string("My App")])
        form.setGit(false)
        XCTAssertEqual(try form.request().params, ["name": .string("My App"), "git": .bool(false)])
        form.name = ""
        XCTAssertThrowsError(try form.request()) { XCTAssertEqual($0 as? NewProjectValidationError, .empty) }
        form.name = "   "
        XCTAssertThrowsError(try form.request()) { XCTAssertEqual($0 as? NewProjectValidationError, .empty) }
        form.name = "../etc"
        XCTAssertFalse(form.isReady)
        XCTAssertThrowsError(try form.request()) { XCTAssertEqual($0 as? NewProjectValidationError, .separator) }
        form.name = ".hidden"
        XCTAssertThrowsError(try form.request()) { XCTAssertEqual($0 as? NewProjectValidationError, .leadingDot) }
    }
}
