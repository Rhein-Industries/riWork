import XCTest
@testable import RiWorkCore

private let shellID = "44444444-4444-4444-8444-444444444444"
private let batchID = "55555555-5555-4555-8555-555555555555"
private let requestID = "66666666-6666-4666-8666-666666666666"
/// A UUID with hex letters, so upper-casing it changes it.
private let letteredID = "abcdefab-cdef-4abc-8def-abcdefabcdef"
private let t0 = Date(timeIntervalSince1970: 1_000)

private let namedKeys: [(String, TerminalKey)] = [
    ("Enter", .enter), ("Tab", .tab), ("BTab", .backTab), ("Escape", .escape), ("Backspace", .backspace), ("Delete", .delete),
    ("Up", .up), ("Down", .down), ("Left", .left), ("Right", .right), ("Home", .home), ("End", .end),
    ("PageUp", .pageUp), ("PageDown", .pageDown)
]
private let lowercaseLetters: [Character] = Array("abcdefghijklmnopqrstuvwxyz")

private func parse(_ text: String) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: Data(text.utf8)) }
private func scalarString(_ value: UInt32) -> String { String(Unicode.Scalar(value)!) }
private func assertViolation<T>(_ expression: @autoclosure () throws -> T, _ message: String = "", file: StaticString = #filePath, line: UInt = #line) {
    XCTAssertThrowsError(try expression(), message, file: file, line: line) { error in
        guard case RemoteError.protocolViolation = error else { return XCTFail("expected protocolViolation, got \(error). \(message)", file: file, line: line) }
    }
}
private func keysParams(items: [KeyItem] = [.text("ls"), .key(.enter)], shell: String = shellID, batch: String = batchID) -> [String: JSONValue] {
    ["shell_id": .string(shell), "batch": .string(batch), "items": .array(items.map(\.json))]
}

/// Records every request and answers from `reply`; by default it echoes shell_id and batch with the given status.
private final class FakeTransport: RemoteTransport, @unchecked Sendable {
    struct Call: Sendable { let method: String; let params: [String: JSONValue]; let id: String }
    typealias Reply = @Sendable ([String: JSONValue]) throws -> JSONValue
    static func echo(status: String = "sent") -> Reply {
        { params in .object(["shell_id": params["shell_id"] ?? .null, "batch": params["batch"] ?? .null, "status": .string(status)]) }
    }
    private let lock = NSLock()
    private var recorded: [Call] = []
    private let reply: Reply
    init(reply: @escaping Reply = FakeTransport.echo()) { self.reply = reply }
    var calls: [Call] { lock.withLock { recorded } }
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { pairing }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        lock.withLock { recorded.append(Call(method: method, params: params, id: id)) }
        return try reply(params)
    }
    func disconnect() async {}
    func isConnected() async -> Bool { true }
}

@MainActor final class KeyInputTests: XCTestCase {
    // MARK: - a. Item JSON

    func testEveryNamedKeyAndControlLetterHasItsWireName() throws {
        for (name, key) in namedKeys {
            XCTAssertEqual(key.name, name)
            XCTAssertEqual(TerminalKey(name: name), key, name)
            XCTAssertEqual(KeyItem.key(key).json, .object(["key": .string(name)]))
        }
        for letter in lowercaseLetters {
            let key = TerminalKey.control(letter)
            XCTAssertEqual(key.name, "C-\(letter)")
            XCTAssertEqual(TerminalKey(name: "C-\(letter)"), key)
            XCTAssertTrue(key.isValid)
            XCTAssertTrue(TerminalKey.isControlLetter(letter))
        }
        XCTAssertEqual(KeyItem.text("ls -la").json, .object(["text": .string("ls -la")]))
    }

    func testItemsRoundTripThroughRealJSONText() throws {
        var items: [KeyItem] = [.text("ls -la"), .text("héllo 你好 🙂"), .text("a b"), .text("👨‍👩‍👧‍👦"), .text("e\u{301}")]
        items += namedKeys.map { .key($0.1) }
        items += lowercaseLetters.map { .key(.control($0)) }
        XCTAssertEqual(items.count, 5 + 14 + 26)
        for item in items {
            let text = String(data: try JSONEncoder().encode(item.json), encoding: .utf8)!
            XCTAssertEqual(try KeyItem(json: try parse(text)), item, text)
        }
        XCTAssertEqual(try KeyItem(json: try parse(#"{"key":"C-c"}"#)), .key(.control("c")))
        XCTAssertEqual(try KeyItem(json: try parse(#"{"text":"café 🙂"}"#)), .text("café 🙂"))
    }

    func testDecodingIsStrict() throws {
        let rejected: [JSONValue] = [
            .object([:]),
            .object(["text": .string("a"), "extra": .bool(true)]),
            .object(["text": .string("a"), "key": .string("Enter")]),
            .object(["key": .string("Enter"), "extra": .null]),
            .object(["other": .string("x")]),
            .object(["key": .string("Unknown")]),
            .object(["key": .string("enter")]),
            .object(["key": .string("ENTER")]),
            .object(["key": .string("C-A")]),
            .object(["key": .string("C-1")]),
            .object(["key": .string("C-")]),
            .object(["key": .string("C-ab")]),
            .object(["key": .string("c-a")]),
            .object(["key": .string("C-é")]),
            .object(["key": .string("C-\u{FF41}")]),
            .object(["key": .string("")]),
            .object(["key": .number(1)]),
            .object(["key": .null]),
            .object(["text": .string("")]),
            .object(["text": .number(5)]),
            .object(["text": .null]),
            .object(["text": .string("a\nb")]),
            .object(["text": .string(String(repeating: "a", count: 4097))]),
            .string("Enter"), .array([]), .null, .number(1), .bool(true)
        ]
        for value in rejected { assertViolation(try KeyItem(json: value), "\(value)") }
        // The name parser is strict on its own, not only through item validation.
        for name in ["", "enter", "ENTER", "Return", "C-A", "C-Z", "C-1", "C-", "C-ab", "c-a", "C_a", "C-é", "C-\u{FF41}", "Ctrl-a", " Enter", "Enter ", "F1", "Space", "Insert"] {
            XCTAssertNil(TerminalKey(name: name), "\"\(name)\"")
        }
    }

    func testControlLettersAcceptBothCasesOnlyWhenMapping() {
        XCTAssertEqual(TerminalKey.control(forLetter: "a"), .control("a"))
        XCTAssertEqual(TerminalKey.control(forLetter: "Z"), .control("z"))
        for other: Character in ["1", " ", "é", "-", "\n", "你"] { XCTAssertNil(TerminalKey.control(forLetter: other), "\(other)") }
        XCTAssertFalse(TerminalKey.isControlLetter("A"))
        XCTAssertFalse(TerminalKey.isControlLetter("1"))
        XCTAssertFalse(TerminalKey.isControlLetter("é"))
        XCTAssertFalse(TerminalKey.control("A").isValid)
        XCTAssertEqual(TerminalKey.control("A").name, "C-A", "an invalid letter still renders")
        assertViolation(try KeyItem.key(.control("A")).validate())
        assertViolation(try KeyItem.key(.control("1")).validate())
    }

    func testKeySymbols() {
        let expected: [(TerminalKey, String)] = [
            (.enter, "⏎"), (.tab, "⇥"), (.backTab, "⇤"), (.escape, "⎋"), (.backspace, "⌫"), (.delete, "⌦"),
            (.up, "↑"), (.down, "↓"), (.left, "←"), (.right, "→"), (.home, "↖"), (.end, "↘"), (.pageUp, "⇞"), (.pageDown, "⇟"),
            (.control("c"), "^C"), (.control("a"), "^A")
        ]
        for (key, symbol) in expected { XCTAssertEqual(key.symbol, symbol); XCTAssertEqual(KeyItem.key(key).symbol, symbol) }
        XCTAssertEqual(KeyItem.text("hello").symbol, "hello")
        XCTAssertEqual(Set(expected.map(\.1)).count, expected.count)
    }

    // MARK: - b. Validation limits

    func testTextByteLimits() throws {
        XCTAssertEqual(KeyItem.maxTextBytes, 4096)
        XCTAssertEqual(KeyItem.maxItems, 64)
        try KeyItem.text("a").validate()
        try KeyItem.text(String(repeating: "a", count: 4096)).validate()
        try KeyItem.text(String(repeating: "é", count: 2048)).validate()
        try KeyItem.text(String(repeating: "🙂", count: 1024)).validate()
        assertViolation(try KeyItem.text("").validate())
        assertViolation(try KeyItem.text(String(repeating: "a", count: 4097)).validate())
        assertViolation(try KeyItem.text(String(repeating: "é", count: 2049)).validate(), "4098 bytes")
        assertViolation(try KeyItem.text(String(repeating: "é", count: 2048) + "a").validate(), "4097 bytes")
        assertViolation(try KeyItem.text(String(repeating: "🙂", count: 1025)).validate())
    }

    func testControlCharactersAndSeparatorsAreRejectedInText() throws {
        let forbidden: [UInt32] = [0x00, 0x01, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x1B, 0x1F, 0x7F, 0x80, 0x85, 0x9F, 0x2028, 0x2029]
        for value in forbidden {
            let bad = "a" + scalarString(value) + "b"
            assertViolation(try KeyItem.text(bad).validate(), "U+\(String(value, radix: 16))")
            assertViolation(try KeyItem.text(scalarString(value)).validate(), "alone U+\(String(value, radix: 16))")
            XCTAssertTrue(KeyItem.isForbidden(Unicode.Scalar(value)!), "U+\(String(value, radix: 16))")
        }
        for value: UInt32 in [0x20, 0x21, 0x7E, 0xA0, 0xAD, 0xE9, 0x200B, 0x200D, 0xFEFF, 0xE000, 0x1F642] {
            XCTAssertFalse(KeyItem.isForbidden(Unicode.Scalar(value)!), "U+\(String(value, radix: 16))")
            try KeyItem.text("a" + scalarString(value) + "b").validate()
        }
    }

    func testZeroWidthJoinerSequencesAndCombiningMarksAreAllowed() throws {
        // U+200D is a format character (Cf), not a control character.
        try KeyItem.text("👨‍👩‍👧‍👦").validate()
        try KeyItem.text("🏳️‍🌈 and 🧑🏽‍💻").validate()
        try KeyItem.text("e\u{301} n\u{303} a\u{308}\u{323}").validate()
        try KeyItem.text("नमस्ते").validate()
        XCTAssertEqual(try KeyItem(json: KeyItem.text("👨‍👩‍👧‍👦").json), .text("👨‍👩‍👧‍👦"))
    }

    func testBatchItemCountLimits() throws {
        assertViolation(try KeyItem.validate(batch: []))
        try KeyItem.validate(batch: [.key(.enter)])
        try KeyItem.validate(batch: Array(repeating: .key(.enter), count: 64))
        try KeyItem.validate(batch: Array(repeating: .text("a"), count: 64))
        assertViolation(try KeyItem.validate(batch: Array(repeating: .key(.enter), count: 65)))
        assertViolation(try KeyItem.validate(batch: Array(repeating: .text("a"), count: 65)))
        assertViolation(try KeyItem.validate(batch: [.text("ok"), .key(.control("A"))]), "one invalid item spoils the batch")
        assertViolation(try KeyItem.validate(batch: [.key(.enter), .text("")]))
        assertViolation(try KeyItem.validate(batch: [.text("a\u{7}")]))
    }

    func testBatchTextByteBudgetIsTheSumOfAllTextItems() throws {
        let kilo = String(repeating: "a", count: 1024)
        try KeyItem.validate(batch: [.text(kilo), .text(kilo), .text(kilo), .text(kilo)])
        try KeyItem.validate(batch: [.text(kilo), .key(.enter), .text(kilo), .key(.tab), .text(kilo), .text(kilo)])
        assertViolation(try KeyItem.validate(batch: [.text(kilo), .text(kilo), .text(kilo), .text(kilo), .text("a")]), "4097 bytes over five items")
        assertViolation(try KeyItem.validate(batch: [.text(kilo), .key(.enter), .text(kilo), .text(kilo), .text(kilo + "é")]), "4098 bytes")
        // Keys carry no text bytes.
        try KeyItem.validate(batch: [.text(String(repeating: "a", count: 4096)), .key(.enter), .key(.control("c"))])
    }

    // MARK: - c. RequestValidation

    func testRequestValidationAcceptsAValidShellKeysRequest() throws {
        try RequestValidation.validate(method: "shell.keys", params: keysParams(), id: requestID)
        try RequestValidation.validate(method: "shell.keys", params: keysParams(items: [.key(.control("c"))]), id: requestID)
        try RequestValidation.validate(method: "shell.keys", params: keysParams(items: Array(repeating: .key(.enter), count: 64)), id: requestID)
        try RequestValidation.validate(method: "shell.keys", params: keysParams(shell: letteredID, batch: letteredID), id: requestID)
    }

    func testRequestValidationRejectsMalformedShellKeysRequests() {
        func reject(_ params: [String: JSONValue], _ why: String, id: String = requestID) {
            assertViolation(try RequestValidation.validate(method: "shell.keys", params: params, id: id), why)
        }
        var missingBatch = keysParams(); missingBatch["batch"] = nil
        reject(missingBatch, "missing batch")
        var missingItems = keysParams(); missingItems["items"] = nil
        reject(missingItems, "missing items")
        var missingShell = keysParams(); missingShell["shell_id"] = nil
        reject(missingShell, "missing shell_id")
        reject([:], "no params")
        reject(keysParams(batch: letteredID.uppercased()), "upper-case batch")
        reject(keysParams(batch: "55555555555545558555555555555555"), "batch without hyphens")
        reject(keysParams(batch: "{55555555-5555-4555-8555-555555555555}"), "braced batch")
        reject(keysParams(batch: "55555555"), "short batch")
        reject(keysParams(batch: ""), "empty batch")
        reject(keysParams(shell: letteredID.uppercased()), "upper-case shell id")
        reject(keysParams(shell: "44444444"), "short shell id")
        var nullBatch = keysParams(); nullBatch["batch"] = .null
        reject(nullBatch, "null batch")
        var numberBatch = keysParams(); numberBatch["batch"] = .number(5)
        reject(numberBatch, "numeric batch")
        for extra in ["line", "lines", "worktree_id", "project_id", "owner"] {
            var params = keysParams(); params[extra] = .string(batchID)
            reject(params, "unknown extra param \(extra)")
        }
        reject(keysParams(), "upper-case request id", id: letteredID.uppercased())
        reject(keysParams(), "request id not a uuid", id: "not-a-uuid")

        var notArray = keysParams(); notArray["items"] = .string("Enter")
        reject(notArray, "items is a string")
        var objectItems = keysParams(); objectItems["items"] = .object(["key": .string("Enter")])
        reject(objectItems, "items is an object")
        var nullItems = keysParams(); nullItems["items"] = .null
        reject(nullItems, "items is null")
        for bad: JSONValue in [
            .object(["text": .string("a"), "key": .string("Enter")]), .object(["key": .string("Nope")]), .object(["key": .string("C-A")]),
            .object([:]), .string("x"), .null, .object(["text": .string("a\nb")]), .object(["text": .string("")]), .object(["text": .string("a"), "extra": .null])
        ] {
            var params = keysParams(); params["items"] = .array([.object(["text": .string("ok")]), bad])
            reject(params, "malformed item \(bad)")
        }
        reject(keysParams(items: []), "no items")
        reject(keysParams(items: Array(repeating: .key(.enter), count: 65)), "65 items")
        reject(keysParams(items: Array(repeating: .text("a"), count: 65)), "65 text items")
        reject(keysParams(items: [.text(String(repeating: "a", count: 4096)), .text("a")]), "4097 text bytes")
        reject(keysParams(items: [.text(String(repeating: "é", count: 2049))]), "4098 byte item")
    }

    func testShellKeysAndShellInputRemainDifferentMethods() {
        assertViolation(try RequestValidation.validate(method: "shell.input", params: keysParams(), id: requestID))
        assertViolation(try RequestValidation.validate(method: "shell.keys", params: ["shell_id": .string(shellID), "line": .string("ls")], id: requestID))
        assertViolation(try RequestValidation.validate(method: "shell.key", params: keysParams(), id: requestID))
    }

    // MARK: - d. RemoteTransport.keys

    func testKeysSendsTheContractRequest() async throws {
        let transport = FakeTransport()
        let items: [KeyItem] = [.text("ls -la"), .key(.enter), .key(.control("c")), .key(.backTab)]
        let status = try await transport.keys(shellID: shellID, batch: batchID, items: items, id: requestID)
        XCTAssertEqual(status, .sent)
        let call = try XCTUnwrap(transport.calls.first)
        XCTAssertEqual(transport.calls.count, 1)
        XCTAssertEqual(call.method, "shell.keys")
        XCTAssertEqual(call.id, requestID)
        XCTAssertEqual(Set(call.params.keys), ["shell_id", "batch", "items"])
        XCTAssertEqual(call.params["shell_id"], .string(shellID))
        XCTAssertEqual(call.params["batch"], .string(batchID))
        XCTAssertEqual(call.params["items"], .array([
            .object(["text": .string("ls -la")]), .object(["key": .string("Enter")]), .object(["key": .string("C-c")]), .object(["key": .string("BTab")])
        ]))
        try RequestValidation.validate(method: call.method, params: call.params, id: call.id)
        // What the desktop parses is exactly the JSON text.
        let wire = String(data: try JSONEncoder().encode(JSONValue.object(call.params)), encoding: .utf8)!
        let parsed = try parse(wire)
        XCTAssertEqual(parsed["items"].array.count, 4)
        XCTAssertEqual(try parsed["items"].array.map { try KeyItem(json: $0) }, items)
    }

    func testEveryStatusParses() async throws {
        for (raw, expected) in [("sent", KeysStatus.sent), ("duplicate", .duplicate), ("uncertain", .uncertain)] {
            let transport = FakeTransport(reply: FakeTransport.echo(status: raw))
            let status = try await transport.keys(shellID: shellID, batch: batchID, items: [.key(.enter)])
            XCTAssertEqual(status, expected, raw)
            XCTAssertEqual(expected.rawValue, raw)
        }
    }

    func testMismatchedOrUnknownResultsThrow() async throws {
        func attempt(_ result: JSONValue, _ why: String) async {
            let transport = FakeTransport(reply: { _ in result })
            do { _ = try await transport.keys(shellID: shellID, batch: batchID, items: [.key(.enter)]); XCTFail("accepted: \(why)") }
            catch { guard case RemoteError.protocolViolation = error else { return XCTFail("\(why): \(error)") } }
        }
        await attempt(.object(["shell_id": .string(letteredID), "batch": .string(batchID), "status": .string("sent")]), "other shell")
        await attempt(.object(["batch": .string(batchID), "status": .string("sent")]), "shell_id absent")
        await attempt(.object(["shell_id": .null, "batch": .string(batchID), "status": .string("sent")]), "shell_id null")
        await attempt(.object(["shell_id": .string(shellID), "batch": .string(letteredID), "status": .string("sent")]), "other batch")
        await attempt(.object(["shell_id": .string(shellID), "batch": .string(batchID), "status": .string("delivered")]), "unknown status")
        await attempt(.object(["shell_id": .string(shellID), "batch": .string(batchID), "status": .string("SENT")]), "status is case-sensitive")
        await attempt(.object(["shell_id": .string(shellID), "batch": .string(batchID), "status": .string("")]), "empty status")
        await attempt(.object(["shell_id": .string(shellID), "batch": .string(batchID)]), "status absent")
        await attempt(.object(["shell_id": .string(shellID), "batch": .string(batchID), "status": .bool(true)]), "status not a string")
        await attempt(.null, "null result")
        await attempt(.string("sent"), "string result")
    }

    func testStatusParsingComparesIdentitiesExactly() throws {
        let ok = JSONValue.object(["shell_id": .string(letteredID), "batch": .string(letteredID), "status": .string("uncertain")])
        XCTAssertEqual(try KeysStatus.parse(ok, shellID: letteredID, batch: letteredID), .uncertain)
        assertViolation(try KeysStatus.parse(ok, shellID: letteredID.uppercased(), batch: letteredID))
        assertViolation(try KeysStatus.parse(ok, shellID: letteredID, batch: letteredID.uppercased()))
        assertViolation(try KeysStatus.parse(.object(["shell_id": .string(letteredID.uppercased()), "status": .string("sent")]), shellID: letteredID, batch: letteredID))
    }

    func testAnAbsentBatchEchoIsTolerated() async throws {
        let transport = FakeTransport(reply: { _ in .object(["shell_id": .string(shellID), "status": .string("duplicate")]) })
        let status = try await transport.keys(shellID: shellID, batch: batchID, items: [.text("x")])
        XCTAssertEqual(status, .duplicate)
    }

    func testInvalidItemsThrowBeforeAnyRequestIsMade() async throws {
        let transport = FakeTransport()
        let invalid: [[KeyItem]] = [
            [], Array(repeating: .key(.enter), count: 65), [.text("")], [.text("a\nb")], [.text("a\u{1b}")], [.text("a\u{2028}")], [.key(.control("A"))],
            [.text(String(repeating: "é", count: 2049))], [.text(String(repeating: "a", count: 4096)), .text("a")]
        ]
        for items in invalid {
            do { _ = try await transport.keys(shellID: shellID, batch: batchID, items: items); XCTFail("accepted \(items.count) items") }
            catch { guard case RemoteError.protocolViolation = error else { return XCTFail("\(error)") } }
        }
        XCTAssertTrue(transport.calls.isEmpty)
    }

    func testEachCallUsesTheIDItWasGivenOrAFreshOne() async throws {
        let transport = FakeTransport()
        _ = try await transport.keys(shellID: shellID, batch: batchID, items: [.key(.enter)], id: requestID)
        _ = try await transport.keys(shellID: shellID, batch: batchID, items: [.key(.enter)], id: letteredID)
        XCTAssertEqual(transport.calls.map(\.id), [requestID, letteredID])

        let defaults = FakeTransport()
        for _ in 0..<5 { _ = try await defaults.keys(shellID: shellID, batch: batchID, items: [.text("retry")]) }
        let ids = defaults.calls.map(\.id)
        XCTAssertEqual(Set(ids).count, 5, "a retry of the same batch is a new request")
        XCTAssertTrue(defaults.calls.allSatisfy { $0.params["batch"] == .string(batchID) })
        for id in ids { XCTAssertEqual(UUID(uuidString: id)?.uuidString.lowercased(), id, "request ids are canonical lowercase UUIDs") }
        for call in defaults.calls { try RequestValidation.validate(method: call.method, params: call.params, id: call.id) }
    }

    func testTransportErrorsPropagateUnchanged() async throws {
        let transport = FakeTransport(reply: { _ in throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method") })
        do { _ = try await transport.keys(shellID: shellID, batch: batchID, items: [.key(.enter)]); XCTFail() }
        catch {
            guard case RemoteError.rpc(let code, let message) = error else { return XCTFail("\(error)") }
            XCTAssertEqual(code, "invalid_request"); XCTAssertEqual(message, "unsupported RPC method")
            XCTAssertTrue(RemoteError.isUnsupportedMethod(error))
        }
    }

    // MARK: - e. isUnsupportedMethod

    func testOnlyTheOldDesktopAnswerCountsAsUnsupported() {
        for message in ["unsupported RPC method", "Unsupported RPC method: shell.keys", "UNSUPPORTED rpc METHOD", "error: unsupported rpc method 'shell.keys'"] {
            XCTAssertTrue(RemoteError.isUnsupportedMethod(RemoteError.rpc(code: "invalid_request", message: message)), message)
        }
        for (code, message) in [("invalid_request", "invalid params"), ("invalid_request", "unsupported"), ("invalid_request", "method"), ("invalid_request", ""),
                                ("not_found", "unsupported RPC method"), ("outcome_unknown", "unsupported RPC method"), ("", "unsupported RPC method"), ("internal", "unsupported RPC method")] {
            XCTAssertFalse(RemoteError.isUnsupportedMethod(RemoteError.rpc(code: code, message: message)), "\(code): \(message)")
        }
        let others: [any Error] = [
            RemoteError.remote("unsupported RPC method"), RemoteError.protocolViolation("unsupported RPC method"), RemoteError.disconnected, RemoteError.timeout,
            RemoteError.uncertainDelivery, RemoteError.relayClosed(code: 1005, reason: "unsupported RPC method"), RemoteError.invalidPairing("unsupported RPC method"),
            CancellationError(), URLError(.cancelled)
        ]
        for error in others { XCTAssertFalse(RemoteError.isUnsupportedMethod(error), "\(error)") }
    }

    // MARK: - f. KeyBuffer

    func testAppendMergesAdjacentTextAndKeepsKeysSeparate() {
        var buffer = KeyBuffer()
        XCTAssertTrue(buffer.isEmpty)
        XCTAssertTrue(buffer.append([.text("ab")]))
        XCTAssertTrue(buffer.append([.text("cd"), .text("ef")]))
        XCTAssertEqual(buffer.items, [.text("abcdef")])
        XCTAssertTrue(buffer.append([.key(.enter), .key(.enter), .text("x")]))
        XCTAssertTrue(buffer.append([.text("y")]))
        XCTAssertTrue(buffer.append([.key(.control("c"))]))
        XCTAssertTrue(buffer.append([.text("z")]))
        XCTAssertEqual(buffer.items, [.text("abcdef"), .key(.enter), .key(.enter), .text("xy"), .key(.control("c")), .text("z")])
        XCTAssertEqual(buffer.itemCount, 6)
        XCTAssertEqual(buffer.characterCount, 6 + 2 + 2 + 1 + 1)
        XCTAssertFalse(buffer.isEmpty)
        XCTAssertNil(buffer.frozen)
    }

    func testAppendingNothingIsANoOp() {
        var buffer = KeyBuffer()
        XCTAssertTrue(buffer.append([], now: t0))
        XCTAssertTrue(buffer.isEmpty)
        XCTAssertNil(buffer.oldestPendingAt)
        XCTAssertEqual(buffer, KeyBuffer())
    }

    func testCharacterCapIsAllOrNothing() {
        var buffer = KeyBuffer()
        XCTAssertEqual(KeyBuffer.maxCharacters, 4096)
        XCTAssertTrue(buffer.append([.text(String(repeating: "a", count: 4090))], now: t0))
        let before = buffer
        XCTAssertFalse(buffer.append([.text("abc"), .key(.enter), .key(.enter), .key(.enter), .key(.enter)], now: t0 + 1), "4090 + 3 + 4 keys = 4097")
        XCTAssertEqual(buffer, before, "a rejected append changes nothing, not even part of it")
        XCTAssertEqual(buffer.oldestPendingAt, t0)
        XCTAssertTrue(buffer.append([.text("abc"), .key(.enter), .key(.enter), .key(.enter)]), "exactly 4096")
        XCTAssertEqual(buffer.characterCount, 4096)
        let full = buffer
        XCTAssertFalse(buffer.append([.text("z")]))
        XCTAssertFalse(buffer.append([.key(.tab)]))
        XCTAssertEqual(buffer, full)
    }

    func testCharacterCapCountsCharactersNotBytes() {
        var emoji = KeyBuffer()
        XCTAssertTrue(emoji.append([.text(String(repeating: "🙂", count: 4096))]), "16 KiB of UTF-8 is still 4096 characters")
        XCTAssertFalse(emoji.append([.text("🙂")]))
        var over = KeyBuffer()
        XCTAssertFalse(over.append([.text(String(repeating: "🙂", count: 4097))], now: t0))
        XCTAssertTrue(over.isEmpty)
        XCTAssertNil(over.oldestPendingAt, "a rejected append into an empty buffer records no time")
        XCTAssertEqual(over, KeyBuffer())
    }

    func testItemCapIsAllOrNothing() {
        var buffer = KeyBuffer()
        XCTAssertEqual(KeyBuffer.maxItems, 512)
        for i in 0..<511 { XCTAssertTrue(buffer.append([.key(.tab)]), "\(i)") }
        let before = buffer
        XCTAssertFalse(buffer.append([.key(.enter), .key(.enter)]), "only one slot is left")
        XCTAssertEqual(buffer, before)
        XCTAssertTrue(buffer.append([.key(.enter)]))
        XCTAssertEqual(buffer.itemCount, 512)
        let full = buffer
        XCTAssertFalse(buffer.append([.key(.up)]))
        XCTAssertFalse(buffer.append([.text("x")]))
        XCTAssertEqual(buffer, full)
        var texts = KeyBuffer()
        for i in 0..<256 { XCTAssertTrue(texts.append([.text("a"), .key(.enter)]), "\(i)") }
        XCTAssertEqual(texts.itemCount, 512)
        XCTAssertFalse(texts.append([.key(.enter)]))
    }

    func testTextThatMergesDoesNotUseAnotherItemSlot() {
        var buffer = KeyBuffer()
        for _ in 0..<255 { buffer.append([.text("a"), .key(.enter)]) }
        XCTAssertTrue(buffer.append([.text("a")]))
        XCTAssertEqual(buffer.itemCount, 511)
        XCTAssertTrue(buffer.append([.text("b"), .text("c"), .text("d")]), "merges into the last text item")
        XCTAssertEqual(buffer.itemCount, 511)
        XCTAssertEqual(buffer.items.last, .text("abcd"))
    }

    func testItemCapCountsTheFrozenBatchToo() throws {
        var buffer = KeyBuffer()
        for _ in 0..<64 { XCTAssertTrue(buffer.append([.key(.enter)])) }
        let frozen = try XCTUnwrap(buffer.nextBatch(id: "b1"))
        XCTAssertEqual(frozen.items.count, 64)
        XCTAssertTrue(buffer.queued.isEmpty)
        XCTAssertEqual(buffer.itemCount, 64)
        for i in 0..<448 { XCTAssertTrue(buffer.append([.key(.tab)]), "\(i)") }
        XCTAssertEqual(buffer.itemCount, 512)
        XCTAssertEqual(buffer.queued.count, 448)
        let before = buffer
        XCTAssertFalse(buffer.append([.key(.tab)]), "448 queued would fit alone, but 64 more are in flight")
        XCTAssertFalse(buffer.append([.text("x")]))
        XCTAssertEqual(buffer, before)
        XCTAssertTrue(buffer.finish("b1"))
        XCTAssertEqual(buffer.itemCount, 448)
        XCTAssertFalse(buffer.append(Array(repeating: .key(.tab), count: 65)), "448 + 65 = 513")
        XCTAssertTrue(buffer.append(Array(repeating: .key(.tab), count: 64)))
        XCTAssertEqual(buffer.itemCount, 512)
    }

    func testCharacterCapCountsTheFrozenBatchToo() throws {
        var buffer = KeyBuffer()
        XCTAssertTrue(buffer.append([.text(String(repeating: "a", count: 4000))]))
        let batch = try XCTUnwrap(buffer.nextBatch(id: "b1"))
        XCTAssertEqual(batch.items, [.text(String(repeating: "a", count: 4000))])
        XCTAssertEqual(buffer.characterCount, 4000)
        XCTAssertTrue(buffer.append([.text(String(repeating: "b", count: 96))]))
        XCTAssertFalse(buffer.append([.key(.enter)]), "4096 characters are pending, in flight or queued")
        XCTAssertTrue(buffer.finish("b1"))
        XCTAssertEqual(buffer.characterCount, 96)
        XCTAssertTrue(buffer.append([.key(.enter)]))
    }

    func testNextBatchOnAnEmptyBufferIsNil() {
        var buffer = KeyBuffer()
        XCTAssertNil(buffer.nextBatch())
        XCTAssertNil(buffer.frozen)
        XCTAssertEqual(buffer, KeyBuffer())
    }

    func testBatchesHoldAtMost64ItemsAndKeepOrder() throws {
        var buffer = KeyBuffer()
        var expected: [KeyItem] = []
        for i in 0..<150 {
            let item: KeyItem = i.isMultiple(of: 2) ? .text("t\(i)") : .key(.up)
            expected.append(item)
            XCTAssertTrue(buffer.append([item]), "\(i)")
        }
        XCTAssertEqual(buffer.itemCount, 150)
        var sent: [KeyItem] = [], sizes: [Int] = [], ids: [String] = []
        while let batch = buffer.nextBatch() {
            sizes.append(batch.items.count); sent += batch.items; ids.append(batch.id)
            try KeyItem.validate(batch: batch.items)
            XCTAssertEqual(UUID(uuidString: batch.id)?.uuidString.lowercased(), batch.id, "default batch ids are canonical lowercase UUIDs")
            XCTAssertTrue(buffer.finish(batch.id))
        }
        XCTAssertEqual(sizes, [64, 64, 22])
        XCTAssertEqual(sent, expected)
        XCTAssertEqual(Set(ids).count, 3)
        XCTAssertTrue(buffer.isEmpty)
        XCTAssertEqual(buffer.characterCount, 0)
    }

    func testLongTextIsSplitAcrossBatchesAt4096Bytes() throws {
        var buffer = KeyBuffer()
        // "é" is two bytes: 3000 characters are 6000 bytes.
        XCTAssertTrue(buffer.append([.text(String(repeating: "é", count: 3000))]))
        let first = try XCTUnwrap(buffer.nextBatch(id: "b1"))
        XCTAssertEqual(first.items, [.text(String(repeating: "é", count: 2048))])
        XCTAssertEqual(buffer.queued, [.text(String(repeating: "é", count: 952))])
        XCTAssertTrue(buffer.finish("b1"))
        XCTAssertEqual(buffer.characterCount, 952)
        let second = try XCTUnwrap(buffer.nextBatch(id: "b2"))
        XCTAssertEqual(second.items, [.text(String(repeating: "é", count: 952))])
        XCTAssertTrue(buffer.finish("b2"))
        XCTAssertTrue(buffer.isEmpty)
    }

    func testASplitNeverCutsAScalarInHalf() throws {
        var buffer = KeyBuffer()
        let original = String(repeating: "a", count: 4090) + "🙂🙂"   // 4098 bytes, 4092 characters
        XCTAssertTrue(buffer.append([.text(original)]))
        let first = try XCTUnwrap(buffer.nextBatch(id: "b1"))
        XCTAssertEqual(first.items, [.text(String(repeating: "a", count: 4090) + "🙂")], "the second emoji does not fit the last 2 bytes")
        if case .text(let text)? = first.items.first { XCTAssertEqual(text.utf8.count, 4094) }
        try KeyItem.validate(batch: first.items)
        XCTAssertTrue(buffer.finish("b1"))
        XCTAssertEqual(try XCTUnwrap(buffer.nextBatch(id: "b2")).items, [.text("🙂")])
    }

    func testATextItemWhoseFirstScalarDoesNotFitWaitsForTheNextBatch() throws {
        var buffer = KeyBuffer()
        XCTAssertTrue(buffer.append([.text(String(repeating: "a", count: 4093)), .key(.enter), .text("🙂🙂")]))
        let first = try XCTUnwrap(buffer.nextBatch(id: "b1"))
        XCTAssertEqual(first.items, [.text(String(repeating: "a", count: 4093)), .key(.enter)], "3 bytes are left and an emoji needs 4")
        XCTAssertEqual(buffer.queued, [.text("🙂🙂")], "the text stays whole")
        XCTAssertTrue(buffer.finish("b1"))
        XCTAssertEqual(try XCTUnwrap(buffer.nextBatch(id: "b2")).items, [.text("🙂🙂")])
    }

    func testAHugeEmojiRunSplitsIntoFullBatchesAndReassembles() throws {
        var buffer = KeyBuffer()
        let original = String(repeating: "🙂", count: 4096)
        XCTAssertTrue(buffer.append([.text(original)]))
        var pieces: [String] = []
        while let batch = buffer.nextBatch() {
            try KeyItem.validate(batch: batch.items)
            guard case .text(let text)? = batch.items.first, batch.items.count == 1 else { return XCTFail("\(batch)") }
            XCTAssertEqual(text.utf8.count, 4096)
            pieces.append(text)
            XCTAssertTrue(buffer.finish(batch.id))
        }
        XCTAssertEqual(pieces.count, 4)
        XCTAssertEqual(pieces.joined(), original)
        XCTAssertTrue(buffer.isEmpty)
    }

    func testTextSharesTheByteBudgetAcrossItemsAndKeepsOrderAcrossBatches() throws {
        var buffer = KeyBuffer()
        let mixed: [KeyItem] = [.text(String(repeating: "é", count: 1500)), .key(.tab), .text(String(repeating: "ü", count: 1500)), .key(.enter), .text("end")]
        XCTAssertTrue(buffer.append(mixed))
        var text = "", order: [String] = []
        while let batch = buffer.nextBatch() {
            var bytes = 0
            for item in batch.items {
                switch item {
                case .text(let value): bytes += value.utf8.count; text += value; order.append("t")
                case .key(let key): order.append(key.name)
                }
            }
            XCTAssertLessThanOrEqual(bytes, 4096)
            try KeyItem.validate(batch: batch.items)
            XCTAssertTrue(buffer.finish(batch.id))
        }
        XCTAssertEqual(text, String(repeating: "é", count: 1500) + String(repeating: "ü", count: 1500) + "end")
        // 3000 + 3000 bytes of text: batch one ends inside the second run, so the run shows up in two batches.
        XCTAssertEqual(order, ["t", "Tab", "t", "t", "Enter", "t"])
    }

    func testTheFrozenBatchIsReturnedUnchangedUntilItIsFinished() throws {
        var buffer = KeyBuffer()
        XCTAssertTrue(buffer.append([.key(.tab), .text("ls")], now: t0))
        let first = try XCTUnwrap(buffer.nextBatch(id: "first"))
        XCTAssertEqual(first, KeyBatch(id: "first", items: [.key(.tab), .text("ls")]))
        XCTAssertEqual(buffer.frozen, first)
        XCTAssertTrue(buffer.queued.isEmpty)
        // Keystrokes typed after freezing queue behind it and never join it, even text directly after text.
        XCTAssertTrue(buffer.append([.text("x")], now: t0 + 1))
        XCTAssertTrue(buffer.append([.key(.enter)], now: t0 + 2))
        XCTAssertEqual(buffer.frozen, first)
        XCTAssertEqual(buffer.queued, [.text("x"), .key(.enter)])
        XCTAssertEqual(buffer.items, first.items + [.text("x"), .key(.enter)])
        for _ in 0..<3 { XCTAssertEqual(buffer.nextBatch(id: "ignored"), first, "a retry carries the same id and content") }
        XCTAssertEqual(buffer.queued, [.text("x"), .key(.enter)])
        XCTAssertTrue(buffer.finish("first"))
        XCTAssertNil(buffer.frozen)
        XCTAssertFalse(buffer.finish("first"), "already finished")
        let second = try XCTUnwrap(buffer.nextBatch(id: "second"))
        XCTAssertEqual(second, KeyBatch(id: "second", items: [.text("x"), .key(.enter)]))
    }

    /// The desktop pauses when a key follows text, so a batch ends at an Enter: typical batches are `[text, Enter]`.
    func testABatchEndsAtAnEnterAndAbsorbsEntersRightBehindIt() throws {
        var buffer = KeyBuffer()
        XCTAssertTrue(buffer.append(KeyMapper.items(for: "ls\ngit status\n\n\npwd"), now: t0))
        let first = try XCTUnwrap(buffer.nextBatch(id: "1"))
        XCTAssertEqual(first.items, [.text("ls"), .key(.enter)])
        XCTAssertTrue(buffer.finish("1"))
        let second = try XCTUnwrap(buffer.nextBatch(id: "2"))
        XCTAssertEqual(second.items, [.text("git status"), .key(.enter), .key(.enter), .key(.enter)], "a run of Enters stays together")
        XCTAssertTrue(buffer.finish("2"))
        let third = try XCTUnwrap(buffer.nextBatch(id: "3"))
        XCTAssertEqual(third.items, [.text("pwd")])
        XCTAssertTrue(buffer.finish("3"))
        XCTAssertTrue(buffer.isEmpty)
    }
    func testOtherKeysDoNotEndABatch() throws {
        var buffer = KeyBuffer()
        XCTAssertTrue(buffer.append([.text("a"), .key(.tab), .text("b"), .key(.up), .key(.control("c"))], now: t0))
        XCTAssertEqual(buffer.nextBatch(id: "1")?.items, [.text("a"), .key(.tab), .text("b"), .key(.up), .key(.control("c"))])
    }
    func testAnEnterBatchStillRespectsTheItemLimit() throws {
        var buffer = KeyBuffer()
        XCTAssertTrue(buffer.append(Array(repeating: .key(.enter), count: 70), now: t0))
        XCTAssertEqual(buffer.nextBatch(id: "1")?.items.count, 64)
        XCTAssertTrue(buffer.finish("1"))
        XCTAssertEqual(buffer.nextBatch(id: "2")?.items.count, 6)
    }

    func testFinishWithTheWrongIDChangesNothing() throws {
        var buffer = KeyBuffer()
        XCTAssertFalse(buffer.finish("anything"), "nothing is frozen")
        buffer.append([.text("hi")], now: t0)
        let batch = try XCTUnwrap(buffer.nextBatch(id: "real"))
        let before = buffer
        XCTAssertFalse(buffer.finish("other"))
        XCTAssertFalse(buffer.finish(""))
        XCTAssertFalse(buffer.finish("REAL"))
        XCTAssertEqual(buffer, before)
        XCTAssertEqual(buffer.frozen, batch)
        XCTAssertEqual(buffer.characterCount, 2)
    }

    func testFinishFreesCapacity() throws {
        var buffer = KeyBuffer()
        XCTAssertTrue(buffer.append([.text(String(repeating: "a", count: 4096))]))
        _ = try XCTUnwrap(buffer.nextBatch(id: "b1"))
        XCTAssertFalse(buffer.append([.text("z")]), "the frozen batch still counts")
        XCTAssertEqual(buffer.characterCount, 4096)
        XCTAssertTrue(buffer.finish("b1"))
        XCTAssertEqual(buffer.characterCount, 0)
        XCTAssertTrue(buffer.isEmpty)
        XCTAssertTrue(buffer.append([.text("z")]))
        XCTAssertEqual(buffer.characterCount, 1)
    }

    func testOldestPendingAtTracksTheOldestUnsentOrInFlightItem() throws {
        var buffer = KeyBuffer()
        XCTAssertNil(buffer.oldestPendingAt)
        buffer.append([.text("a")], now: t0)
        buffer.append([.key(.enter)], now: t0 + 5)
        XCTAssertEqual(buffer.oldestPendingAt, t0, "later appends do not move it")
        let first = try XCTUnwrap(buffer.nextBatch(id: "b1"))
        XCTAssertEqual(buffer.oldestPendingAt, t0, "in flight still counts")
        buffer.append([.text("b")], now: t0 + 10)
        XCTAssertEqual(buffer.oldestPendingAt, t0, "the in-flight batch is older than the queue")
        XCTAssertTrue(buffer.finish(first.id))
        XCTAssertEqual(buffer.oldestPendingAt, t0 + 10)
        let second = try XCTUnwrap(buffer.nextBatch(id: "b2"))
        XCTAssertEqual(buffer.oldestPendingAt, t0 + 10)
        XCTAssertTrue(buffer.finish(second.id))
        XCTAssertNil(buffer.oldestPendingAt, "drained")
        buffer.append([.text("c")], now: t0 + 20)
        XCTAssertEqual(buffer.oldestPendingAt, t0 + 20)
        buffer.removeAll()
        XCTAssertNil(buffer.oldestPendingAt)
    }

    func testRemoveAllForgetsEverythingAndRestoresCapacity() throws {
        var buffer = KeyBuffer()
        buffer.append([.text("abc"), .key(.enter)], now: t0)
        _ = buffer.nextBatch(id: "b1")
        buffer.append([.text("def")], now: t0 + 1)
        buffer.block = KeyBuffer.Block(reason: "Input is disabled on the desktop.", at: t0)
        buffer.removeAll()
        XCTAssertTrue(buffer.isEmpty)
        XCTAssertEqual(buffer.itemCount, 0)
        XCTAssertEqual(buffer.characterCount, 0)
        XCTAssertNil(buffer.frozen)
        XCTAssertNil(buffer.block)
        XCTAssertNil(buffer.oldestPendingAt)
        XCTAssertNil(buffer.nextBatch())
        XCTAssertEqual(buffer, KeyBuffer())
        XCTAssertTrue(buffer.append([.text(String(repeating: "a", count: 4096))]))
    }

    func testABlockIsKeptWhileTheQueueChanges() throws {
        var buffer = KeyBuffer()
        let block = KeyBuffer.Block(reason: "Input is disabled on the desktop.", at: t0)
        buffer.append([.text("kept")])
        buffer.block = block
        buffer.append([.key(.enter)])
        let batch = try XCTUnwrap(buffer.nextBatch())
        XCTAssertTrue(buffer.finish(batch.id))
        XCTAssertEqual(buffer.block, block)
        buffer.block = nil
        XCTAssertNil(buffer.block)
    }

    func testPlainTextKeepsOnlyTypedCharactersAndAppliesBackspace() throws {
        var buffer = KeyBuffer()
        XCTAssertEqual(buffer.plainText, "")
        buffer.append([.text("helo"), .key(.backspace), .text("lo"), .key(.tab), .key(.enter), .key(.control("c")), .key(.up)])
        XCTAssertEqual(buffer.plainText, "hello")
        var edits = KeyBuffer()
        edits.append([.key(.backspace), .key(.backspace), .text("ab"), .key(.backspace), .key(.backspace), .key(.backspace), .text("c")])
        XCTAssertEqual(edits.plainText, "c", "backspace on nothing does nothing")
        var characters = KeyBuffer()
        characters.append([.text("a🙂e\u{301}"), .key(.backspace)])
        XCTAssertEqual(characters.plainText, "a🙂")
        characters.append([.key(.backspace)])
        XCTAssertEqual(characters.plainText, "a")
    }

    func testPlainTextSpansTheFrozenBatchAndTheQueue() throws {
        var buffer = KeyBuffer()
        buffer.append([.text("ls -l"), .key(.enter), .text("cd /tm")])
        _ = try XCTUnwrap(buffer.nextBatch(id: "b1"))
        buffer.append([.key(.backspace), .text("p")])
        XCTAssertEqual(buffer.plainText, "ls -lcd /tp")
    }

    func testPreviewUsesGlyphsForKeys() {
        var buffer = KeyBuffer()
        XCTAssertEqual(buffer.preview(), "")
        buffer.append([.text("ls"), .key(.enter), .key(.tab), .key(.backspace), .key(.escape), .key(.up), .key(.down), .key(.left), .key(.right), .key(.control("c"))])
        XCTAssertEqual(buffer.preview(), "ls⏎⇥⌫⎋↑↓←→^C")
        _ = buffer.nextBatch(id: "b1")
        buffer.append([.text("x")])
        XCTAssertEqual(buffer.preview(), "ls⏎⇥⌫⎋↑↓←→^Cx", "the frozen batch and the queue both show")
    }

    func testPreviewTruncatesAtTheFrontToExactlyTheLimit() {
        var buffer = KeyBuffer()
        buffer.append([.text("abcdefghij")])
        XCTAssertEqual(buffer.preview(limit: 10), "abcdefghij", "fits exactly")
        XCTAssertEqual(buffer.preview(limit: 11), "abcdefghij")
        XCTAssertEqual(buffer.preview(limit: 5), "…ghij")
        XCTAssertEqual(buffer.preview(limit: 5).count, 5)
        XCTAssertEqual(buffer.preview(limit: 2), "…j")
        for limit in 2...9 { XCTAssertEqual(buffer.preview(limit: limit).count, limit, "limit \(limit)") }
        buffer.append([.key(.enter), .key(.control("c"))])
        XCTAssertEqual(buffer.preview(limit: 6), "…ij⏎^C", "the newest input stays visible")
        var long = KeyBuffer()
        long.append([.text(String(repeating: "x", count: 100) + "tail")])
        XCTAssertEqual(long.preview().count, 48, "the default limit is 48")
        XCTAssertTrue(long.preview().hasPrefix("…x"))
        XCTAssertTrue(long.preview().hasSuffix("xtail"))
        var emoji = KeyBuffer()
        emoji.append([.text(String(repeating: "🙂", count: 30))])
        XCTAssertEqual(emoji.preview(limit: 8), "…" + String(repeating: "🙂", count: 7))
    }

    // MARK: - g. KeyMapper

    func testLineEndingsBecomeEnter() {
        XCTAssertEqual(KeyMapper.items(for: "\n"), [.key(.enter)])
        XCTAssertEqual(KeyMapper.items(for: "\r\n"), [.key(.enter)], "CRLF is one Enter")
        XCTAssertEqual(KeyMapper.items(for: "\r"), [.key(.enter)])
        XCTAssertEqual(KeyMapper.items(for: "\r\n\r\n"), [.key(.enter), .key(.enter)])
        XCTAssertEqual(KeyMapper.items(for: "\n\n"), [.key(.enter), .key(.enter)])
        XCTAssertEqual(KeyMapper.items(for: "\r\r"), [.key(.enter), .key(.enter)])
        XCTAssertEqual(KeyMapper.items(for: "\n\r"), [.key(.enter), .key(.enter)])
        XCTAssertEqual(KeyMapper.items(for: "\r\r\n"), [.key(.enter), .key(.enter)])
        XCTAssertEqual(KeyMapper.items(for: "a\r\nb"), [.text("a"), .key(.enter), .text("b")])
        XCTAssertEqual(KeyMapper.items(for: ""), [])
    }

    func testTabBecomesTab() {
        XCTAssertEqual(KeyMapper.items(for: "\t"), [.key(.tab)])
        XCTAssertEqual(KeyMapper.items(for: "ls\tfoo"), [.text("ls"), .key(.tab), .text("foo")])
    }

    func testOtherControlCharactersAndSeparatorsAreDroppedWhileTextStays() throws {
        for value in [0x00, 0x01, 0x07, 0x08, 0x0B, 0x0C, 0x1B, 0x7F, 0x80, 0x85, 0x9F, 0x2028, 0x2029] as [UInt32] {
            let dropped = scalarString(value)
            XCTAssertEqual(KeyMapper.items(for: dropped), [], "U+\(String(value, radix: 16)) alone")
            XCTAssertEqual(KeyMapper.items(for: "a" + dropped + "b"), [.text("ab")], "U+\(String(value, radix: 16)) between text")
            XCTAssertEqual(KeyMapper.items(for: "a" + dropped + "\n" + dropped + "b"), [.text("a"), .key(.enter), .text("b")], "U+\(String(value, radix: 16)) next to Enter")
        }
        XCTAssertEqual(KeyMapper.items(for: "\u{1b}[31mred\u{1b}[0m"), [.text("[31mred[0m")], "only the ESC byte goes; this is typed text, not a terminal stream")
    }

    func testCommonLinesAndPastes() throws {
        XCTAssertEqual(KeyMapper.items(for: "ls -la\n"), [.text("ls -la"), .key(.enter)])
        XCTAssertEqual(KeyMapper.items(for: "a\nb\n\nc"), [.text("a"), .key(.enter), .text("b"), .key(.enter), .key(.enter), .text("c")])
        XCTAssertEqual(KeyMapper.items(for: "\nx"), [.key(.enter), .text("x")])
        XCTAssertEqual(KeyMapper.items(for: "  indented  "), [.text("  indented  ")], "spaces are text")
        let paste = "if true; then\n\techo \"hi\"\nfi\n"
        let items = KeyMapper.items(for: paste)
        XCTAssertEqual(items, [.text("if true; then"), .key(.enter), .key(.tab), .text("echo \"hi\""), .key(.enter), .text("fi"), .key(.enter)])
        try KeyItem.validate(batch: items)
    }

    func testDictationAndNonASCIITextStayOneItem() throws {
        var mapper = KeyMapper()
        let sentence = "Please summarise the failing tests, then open a pull request."
        XCTAssertEqual(mapper.insert(sentence), [.text(sentence)])
        let international = "héllo 你好 🙂"
        XCTAssertEqual(mapper.insert(international), [.text(international)])
        XCTAssertEqual(mapper.insert("👨‍👩‍👧‍👦 e\u{301}"), [.text("👨‍👩‍👧‍👦 e\u{301}")])
        XCTAssertEqual(mapper.insert("x"), [.text("x")])
        try KeyItem.validate(batch: mapper.insert(international))
        XCTAssertFalse(mapper.controlArmed)
    }

    func testDeleteBackwardIsBackspace() {
        var mapper = KeyMapper()
        XCTAssertEqual(mapper.deleteBackward(), [.key(.backspace)])
        mapper.tap(.alt, at: t0)
        XCTAssertEqual(mapper.deleteBackward(), [.key(.escape), .key(.backspace)], "Alt+Backspace deletes a word in readline")
        XCTAssertFalse(mapper.altArmed, "any key uses up what was armed for it")
        mapper.tap(.control, at: t0)
        XCTAssertEqual(mapper.deleteBackward(), [.key(.control("h"))], "Ctrl+Backspace is ^H, as xterm sends it")
        XCTAssertEqual(mapper.insert("c"), [.text("c")])
    }

    // MARK: Arming

    func testATapArmsForOneKeyADoubleTapLocksAndASlowSecondTapReleases() {
        var mapper = KeyMapper()
        XCTAssertEqual(mapper.control, .off)
        mapper.tap(.control, at: t0)
        XCTAssertEqual(mapper.control, .once)
        XCTAssertTrue(mapper.controlArmed)
        XCTAssertEqual(mapper.insert("c"), [.key(.control("c"))])
        XCTAssertEqual(mapper.control, .off, "one key only")
        XCTAssertEqual(mapper.insert("c"), [.text("c")])

        mapper.tap(.control, at: t0)
        mapper.tap(.control, at: t0 + 1)
        XCTAssertEqual(mapper.control, .off, "a slow second tap lets it go")

        mapper.tap(.control, at: t0)
        mapper.tap(.control, at: t0 + 0.3)
        XCTAssertEqual(mapper.control, .locked, "a double tap locks")
        XCTAssertEqual(mapper.insert("a"), [.key(.control("a"))])
        XCTAssertEqual(mapper.press(.left), [.key(.escape), .text("[1;5D")])
        XCTAssertEqual(mapper.insert("e"), [.key(.control("e"))])
        XCTAssertEqual(mapper.control, .locked, "a lock outlasts any number of keys")
        mapper.tap(.control, at: t0 + 0.4)
        XCTAssertEqual(mapper.control, .off, "a tap on a locked modifier releases it, however soon")
        mapper.tap(.control, at: t0 + 0.5)
        XCTAssertEqual(mapper.control, .once, "and the next tap starts over")
    }
    func testADoubleTapCountsOnlyOnTheSameModifierAndOnlyAfterNothingWasSent() {
        var mapper = KeyMapper()
        mapper.tap(.control, at: t0)
        mapper.tap(.alt, at: t0 + 0.1)
        mapper.tap(.control, at: t0 + 0.2)
        XCTAssertEqual(mapper.control, .off, "Alt came between: a second tap, not a double tap")
        XCTAssertEqual(mapper.alt, .once)
        mapper.disarmModifiers()
        mapper.tap(.shift, at: t0)
        _ = mapper.insert("a")
        mapper.tap(.shift, at: t0 + 0.1)
        XCTAssertEqual(mapper.shift, .once, "a tap after the key used it arms again")
    }
    func testLatchesAreSetOutrightAndDisarmModifiersClearsLocksToo() {
        var mapper = KeyMapper()
        mapper.setLatch(.alt, .locked)
        mapper.setLatch(.shift, .once)
        XCTAssertEqual(mapper.armed, [.alt, .shift])
        XCTAssertEqual(mapper.latch(.alt), .locked)
        XCTAssertEqual(mapper.latch(.command), .off, "Command is not a bar modifier")
        mapper.disarmModifiers()
        XCTAssertEqual(mapper.armed, [])
        XCTAssertEqual(mapper.alt, .off)
    }
    func testOnlyWhatWasArmedForOneKeyIsUsedUpALockedOneStays() {
        var mapper = KeyMapper()
        mapper.setLatch(.control, .locked)
        mapper.tap(.alt, at: t0)
        XCTAssertEqual(mapper.insert("x"), [.key(.escape), .key(.control("x"))])
        XCTAssertEqual(mapper.alt, .off)
        XCTAssertEqual(mapper.control, .locked)
        XCTAssertEqual(mapper.insert("x"), [.key(.control("x"))])
    }
    func testAHotkeyIsSentAsDefinedAndUsesUpOnlyOneKeyModifiers() {
        var mapper = KeyMapper()
        mapper.tap(.alt, at: t0); mapper.setLatch(.control, .locked)
        XCTAssertEqual(mapper.run(Hotkey(label: "Clear", steps: [.text("/clear"), .key(.enter)])), [.text("/clear"), .key(.enter)])
        XCTAssertFalse(mapper.altArmed)
        XCTAssertEqual(mapper.control, .locked)
    }

    // MARK: Multi-character input

    func testDictationPastesAndInputMethodWordsPassThroughAndKeepTheArming() {
        var mapper = KeyMapper()
        mapper.tap(.control, at: t0); mapper.tap(.alt, at: t0 + 1); mapper.tap(.shift, at: t0 + 2)
        for text in ["cd", "hello world", "ab\ncd", "你好", "Please open the file."] {
            XCTAssertEqual(mapper.insert(text), KeyMapper.items(for: text), text)
            XCTAssertEqual(mapper.armed, [.control, .alt, .shift], "\(text): no key was pressed, so nothing is used up")
        }
        XCTAssertEqual(mapper.insert("c"), [.key(.escape), .key(.control("c"))], "the next single key gets them")
        XCTAssertEqual(mapper.armed, [])
    }
    func testOneGraphemeIsOneKeyEvenWhenItIsSeveralScalars() {
        var mapper = KeyMapper()
        mapper.tap(.alt, at: t0)
        XCTAssertEqual(mapper.insert("e\u{301}"), [.key(.escape), .text("e\u{301}")])
        mapper.tap(.control, at: t0)
        XCTAssertEqual(mapper.insert("\r\n"), [.key(.enter)], "CRLF is one character")
        XCTAssertFalse(mapper.controlArmed)
        mapper.tap(.alt, at: t0)
        XCTAssertEqual(mapper.insert("🙂"), [.key(.escape), .text("🙂")])
    }
    func testACharacterThatCannotBeSentKeepsTheArming() {
        var mapper = KeyMapper()
        mapper.tap(.alt, at: t0)
        XCTAssertEqual(mapper.insert("\u{7}"), [], "a bare control character is dropped")
        XCTAssertTrue(mapper.altArmed)
        XCTAssertEqual(mapper.insert(""), [])
        XCTAssertTrue(mapper.altArmed)
        XCTAssertEqual(mapper.insert("f"), [.key(.escape), .text("f")])
    }

    // MARK: Encoding: software keyboard characters

    func testCtrlWithEveryLetterOfEitherCase() {
        for letter in lowercaseLetters {
            XCTAssertEqual(KeyMapper.encode(letter, modifiers: .control), [.key(.control(letter))])
            XCTAssertEqual(KeyMapper.encode(Character(letter.uppercased()), modifiers: .control), [.key(.control(letter))], "Ctrl+Shift+letter is Ctrl+letter without CSI u")
            XCTAssertEqual(KeyMapper.encode(letter, modifiers: [.control, .shift]), [.key(.control(letter))])
        }
    }
    func testCtrlWithDigitsPunctuationAndSpace() {
        XCTAssertEqual(KeyMapper.encode("[", modifiers: .control), [.key(.escape)], "Ctrl+[ is Escape")
        XCTAssertEqual(KeyMapper.encode("3", modifiers: .control), [.key(.escape)], "so is Ctrl+3 in xterm")
        XCTAssertEqual(KeyMapper.encode("?", modifiers: .control), [.key(.backspace)], "Ctrl+? is DEL")
        XCTAssertEqual(KeyMapper.encode("8", modifiers: .control), [.key(.backspace)], "so is Ctrl+8")
        // No key the desktop takes is NUL, FS, GS, RS or US: these go as themselves, as Ctrl+1 does in xterm.
        for character: Character in ["1", "2", " ", "@", "\\", "]", "^", "_", "/", "-", ".", ",", "é"] {
            XCTAssertEqual(KeyMapper.encode(character, modifiers: .control), [.text(String(character))], "\(character)")
        }
    }
    func testAltPutsEscapeInFrontOfAnyCharacter() {
        for character: Character in ["b", "f", ".", "B", "1", " ", "<", "é"] {
            XCTAssertEqual(KeyMapper.encode(character, modifiers: .alt), [.key(.escape), .text(String(character))], "\(character)")
        }
        XCTAssertEqual(KeyMapper.encode("b", modifiers: [.alt, .shift]), [.key(.escape), .text("B")], "Alt+Shift+b is Escape B")
    }
    func testCtrlAltWithALetterIsEscapeThenTheControlKey() {
        XCTAssertEqual(KeyMapper.encode("h", modifiers: [.control, .alt]), [.key(.escape), .key(.control("h"))])
        XCTAssertEqual(KeyMapper.encode("X", modifiers: [.control, .alt, .shift]), [.key(.escape), .key(.control("x"))])
        XCTAssertEqual(KeyMapper.encode("[", modifiers: [.control, .alt]), [.key(.escape), .key(.escape)])
    }
    func testShiftAloneMakesALetterUpperCaseAndLeavesTheRest() {
        XCTAssertEqual(KeyMapper.encode("a", modifiers: .shift), [.text("A")])
        XCTAssertEqual(KeyMapper.encode("1", modifiers: .shift), [.text("1")], "the software keyboard already shifted what it sends")
        XCTAssertEqual(KeyMapper.encode("é", modifiers: .shift), [.text("é")])
    }
    func testCommandIsNotATerminalModifier() {
        XCTAssertEqual(KeyMapper.encode("a", modifiers: .command), [.text("a")])
        XCTAssertEqual(KeyMapper.encode(.up, modifiers: .command), [.key(.up)])
    }
    func testReturnTabAndBackspaceFromTheSoftwareKeyboardTakeTheModifiers() {
        var mapper = KeyMapper()
        mapper.tap(.alt, at: t0)
        XCTAssertEqual(mapper.insert("\n"), [.key(.escape), .key(.enter)], "Alt+Return")
        mapper.tap(.shift, at: t0)
        XCTAssertEqual(mapper.insert("\t"), [.key(.backTab)], "Shift+Tab")
        mapper.tap(.control, at: t0); mapper.tap(.alt, at: t0 + 1)
        XCTAssertEqual(mapper.deleteBackward(), [.key(.escape), .key(.control("h"))])
        mapper.tap(.control, at: t0)
        XCTAssertEqual(mapper.insert("\n"), [.key(.enter)], "Ctrl+Return has no legacy form")
        XCTAssertFalse(mapper.controlArmed)
    }

    // MARK: Encoding: named keys

    func testModifiedCursorAndEditingKeysAreXtermSequences() {
        let finals: [(TerminalKey, String)] = [(.up, "1;%A"), (.down, "1;%B"), (.right, "1;%C"), (.left, "1;%D"), (.home, "1;%H"), (.end, "1;%F"),
                                               (.pageUp, "5;%~"), (.pageDown, "6;%~"), (.delete, "3;%~")]
        let parameters: [(ChordModifiers, Int)] = [(.shift, 2), (.alt, 3), ([.shift, .alt], 4), (.control, 5), ([.control, .shift], 6), ([.control, .alt], 7), ([.control, .alt, .shift], 8)]
        for (key, pattern) in finals {
            XCTAssertEqual(KeyMapper.encode(key, modifiers: []), [.key(key)], "\(key) alone is the named key")
            for (modifiers, parameter) in parameters {
                XCTAssertEqual(KeyMapper.xtermParameter(modifiers), parameter)
                let items = KeyMapper.encode(key, modifiers: modifiers)
                XCTAssertEqual(items, [.key(.escape), .text("[" + pattern.replacingOccurrences(of: "%", with: "\(parameter)"))], "\(key) \(modifiers.glyphs)")
                XCTAssertNoThrow(try KeyItem.validate(batch: items))
            }
        }
    }
    func testKeysWithoutAModifiedFormKeepAltAndShiftTabIsBackTab() {
        XCTAssertEqual(KeyMapper.encode(.tab, modifiers: .shift), [.key(.backTab)])
        XCTAssertEqual(KeyMapper.encode(.tab, modifiers: [.shift, .alt]), [.key(.escape), .key(.backTab)])
        XCTAssertEqual(KeyMapper.encode(.tab, modifiers: .control), [.key(.tab)])
        XCTAssertEqual(KeyMapper.encode(.backTab, modifiers: .alt), [.key(.escape), .key(.backTab)])
        XCTAssertEqual(KeyMapper.encode(.enter, modifiers: .alt), [.key(.escape), .key(.enter)])
        XCTAssertEqual(KeyMapper.encode(.enter, modifiers: .shift), [.key(.enter)])
        XCTAssertEqual(KeyMapper.encode(.escape, modifiers: .alt), [.key(.escape), .key(.escape)])
        XCTAssertEqual(KeyMapper.encode(.escape, modifiers: .control), [.key(.escape)])
        XCTAssertEqual(KeyMapper.encode(.backspace, modifiers: .shift), [.key(.backspace)])
        XCTAssertEqual(KeyMapper.encode(.backspace, modifiers: .control), [.key(.control("h"))])
        XCTAssertEqual(KeyMapper.encode(.control("c"), modifiers: .alt), [.key(.escape), .key(.control("c"))])
        XCTAssertEqual(KeyMapper.encode(.control("c"), modifiers: .control), [.key(.control("c"))])
    }
    func testAPressAddsTheHeldModifiersToTheArmedOnes() {
        var mapper = KeyMapper()
        mapper.tap(.control, at: t0)
        XCTAssertEqual(mapper.press(.right, modifiers: .shift), [.key(.escape), .text("[1;6C")], "bar Ctrl and a held Shift")
        XCTAssertEqual(mapper.press(.right, modifiers: .alt), [.key(.escape), .text("[1;3C")], "Ctrl was used up")
        mapper.tap(.alt, at: t0)
        XCTAssertEqual(mapper.press("b", modifiers: .control), [.key(.escape), .key(.control("b"))])
        XCTAssertEqual(mapper.press(.escape), [.key(.escape)])
    }

    func testMapperOutputFeedsTheBufferIntoValidBatches() throws {
        var mapper = KeyMapper(), buffer = KeyBuffer()
        var typed: [KeyItem] = []
        for step in ["git status", "\n", "ls\t-la\r\n", "ünï 🙂 ok"] { typed += mapper.insert(step) }
        mapper.tap(.control, at: t0); typed += mapper.insert("c")
        typed += mapper.deleteBackward()
        typed += mapper.press(.up)
        mapper.tap(.control, at: t0); mapper.tap(.alt, at: t0 + 1); typed += mapper.press(.left)
        XCTAssertTrue(buffer.append(typed))
        var sent: [KeyItem] = []
        while let batch = buffer.nextBatch() { try KeyItem.validate(batch: batch.items); sent += batch.items; XCTAssertTrue(buffer.finish(batch.id)) }
        XCTAssertEqual(sent, [
            .text("git status"), .key(.enter), .text("ls"), .key(.tab), .text("-la"), .key(.enter), .text("ünï 🙂 ok"),
            .key(.control("c")), .key(.backspace), .key(.up), .key(.escape), .text("[1;7D")
        ])
    }

    // MARK: A sequence stays in one batch

    func testABatchFullOfKeysDoesNotEndOnTheEscapeOfASequence() throws {
        var buffer = KeyBuffer()
        let ups = Array(repeating: KeyItem.key(.up), count: KeyItem.maxItems - 1)
        XCTAssertTrue(buffer.append(ups + KeyMapper.encode(.up, modifiers: .control)))
        let first = try XCTUnwrap(buffer.nextBatch())
        XCTAssertEqual(first.items, ups, "the Escape waits for the rest of Ctrl+Up")
        XCTAssertTrue(buffer.finish(first.id))
        XCTAssertEqual(try XCTUnwrap(buffer.nextBatch()).items, [.key(.escape), .text("[1;5A")])
    }
    func testTextThatDoesNotFitTakesItsEscapeWithIt() throws {
        var buffer = KeyBuffer()
        let long = String(repeating: "é", count: KeyItem.maxTextBytes / 2 - 1)
        XCTAssertTrue(buffer.append([.text(long), .key(.up)] + KeyMapper.encode(.left, modifiers: .alt)))
        let first = try XCTUnwrap(buffer.nextBatch())
        XCTAssertEqual(first.items, [.text(long), .key(.up)], "`[1;3D` does not fit, and the Escape goes with it")
        XCTAssertTrue(buffer.finish(first.id))
        XCTAssertEqual(try XCTUnwrap(buffer.nextBatch()).items, [.key(.escape), .text("[1;3D")])
    }
    func testALoneEscapeStillGoesAndOneAtTheEndOfTheQueueToo() throws {
        var buffer = KeyBuffer()
        XCTAssertTrue(buffer.append([.key(.escape)]))
        XCTAssertEqual(try XCTUnwrap(buffer.nextBatch()).items, [.key(.escape)])
        var more = KeyBuffer()
        XCTAssertTrue(more.append([.text("a"), .key(.escape)]))
        XCTAssertEqual(try XCTUnwrap(more.nextBatch()).items, [.text("a"), .key(.escape)], "nothing behind it: it ends the batch")
    }
    func testTheLineComposerGetsTypedTextButNotTheRestOfAnEscapeSequence() {
        var buffer = KeyBuffer()
        buffer.append([.text("ls")] + KeyMapper.encode(.left, modifiers: .control) + KeyMapper.encode("b", modifiers: .alt) + [.key(.enter), .text(" -la")])
        XCTAssertEqual(buffer.plainText, "ls -la")
    }
}
