import XCTest
@testable import RiWorkCore

/// These are the engine's files, not Swift copies. The source-root path also
/// works from an iOS Simulator XCTest bundle built by Xcode on this checkout.
final class ChatProjectionTests: XCTestCase {
    private struct FixtureFile: Decodable {
        var spec_version: Int
        var cases: [Fixture]?
        var shell_cases: [ShellCase]?
    }
    private struct ShellCase: Decodable {
        var command: String
        var allowed: Bool
    }
    private struct Catalog {
        var cases: [Fixture]
        var shellCases: [ShellCase]
    }
    private struct Fixture: Decodable {
        var name: String
        var items: [ChatItem]
        var item_sequences: [String: UInt64]?
        var requests: [ChatRequestRecord]?
        var options: ChatProjectionOptions?
        // The wire Row decoder deliberately tolerates missing/future fields.
        // Fixture expectations must instead compare every serialized JSON key.
        var expected_rows: JSONValue
        var anchors: [String: String]?
        var expected_selection: [String]?
        var page: PageFixture?
        var actions: [Action]?
    }
    private struct PageFixture: Decodable {
        var start: Int
        var end: Int
        var max_items: Int?
        var expected: ChatPageWindow
    }
    private struct Action: Decodable {
        var action: String
        var id: String?
        var open: Bool?
        var mode: ChatDisplayMode?
        var items: [ChatItem]?
        var expected_open_ids: [String]
        var anchors: [String: String]?
        var expected_rows: JSONValue?
    }

    private static func keys(_ value: JSONValue, _ allowed: Set<String>) throws {
        guard case .object(let fields) = value else { return }
        if let unknown = fields.keys.sorted().first(where: { !allowed.contains($0) }) {
            throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "Unknown fixture field `\(unknown)`"))
        }
    }

    /// Runtime wire decoders tolerate future keys; this independent schema gate
    /// matches the strict Rust harness while retaining malformed optional hints.
    private static func validateItem(_ item: JSONValue) throws {
        try keys(item, ["id", "turn_id", "status", "body", "presentation", "started_at", "ended_at", "elided"])
        try keys(item["elided"], ["event", "of", "kind", "tool_kind", "item_id", "turn_id", "parent_id", "status", "started_at", "ended_at", "bytes", "reason", "elided"])
        let presentation = item["presentation"]
        try keys(presentation, ["phase", "images", "attachments", "semantics", "parent_id", "elided"])
        let semantics = presentation["semantics"]
        try keys(semantics, ["kind", "target", "range", "match_count", "excerpt", "produced", "worker", "elided"])
        try keys(semantics["range"], ["start", "end", "elided"])
        try keys(semantics["worker"], ["role", "objective", "child_count", "elided"])
        for path in semantics["produced"].array { try keys(path, ["path", "line", "elided"]) }
        for image in presentation["images"].array {
            try keys(image, ["label", "source"])
            try keys(image["source"], ["kind", "path", "mime", "base64", "url", "reason", "elided"])
        }
        for attachment in presentation["attachments"].array {
            try keys(attachment, ["id", "name", "path", "kind", "bytes", "fingerprint", "preview"])
            try keys(attachment["kind"], ["kind", "mime"])
            try keys(attachment["preview"], ["kind", "excerpt", "path"])
        }
        let body = item["body"]
        let bodyKeys: Set<String>
        switch body["type"].string {
        case "user_message", "agent_message", "reasoning": bodyKeys = ["type", "text"]
        case "plan": bodyKeys = ["type", "explanation", "steps"]
        case "command": bodyKeys = ["type", "command", "cwd", "output", "exit_code"]
        case "file_change": bodyKeys = ["type", "changes"]
        case "tool_call": bodyKeys = ["type", "server", "tool", "input", "output"]
        case "web_search": bodyKeys = ["type", "query"]
        case "todo": bodyKeys = ["type", "items"]
        case "compaction": bodyKeys = ["type"]
        case "notice": bodyKeys = ["type", "level", "text", "kind", "resolved", "dismissed", "resets_at"]
        default: bodyKeys = ["type"]
        }
        try keys(body, bodyKeys)
        for field in ["steps", "items"] {
            for step in body[field].array { try keys(step, ["text", "status"]) }
        }
        for change in body["changes"].array { try keys(change, ["path", "kind", "diff"]) }
        // body.input is provider-owned arbitrary JSON and intentionally unchecked.
    }

    private static func validateFile(_ value: JSONValue) throws {
        try keys(value, ["spec_version", "cases", "shell_cases"])
        for shell in value["shell_cases"].array { try keys(shell, ["command", "allowed"]) }
        for fixture in value["cases"].array {
            try keys(fixture, ["name", "items", "item_sequences", "requests", "options", "expected_rows", "anchors", "expected_selection", "page", "actions"])
            for item in fixture["items"].array { try validateItem(item) }
            for request in fixture["requests"].array {
                try keys(request, ["request_id", "kind", "title", "turn_id", "item_id", "requested_seq", "resolved_seq", "resolution", "answer_summary"])
            }
            let options = fixture["options"]
            try keys(options, ["mode", "now_ms", "running_turn_id", "turn_timing", "disclosure", "provisional", "elided_items"])
            try keys(options["disclosure"], ["overrides", "bulk"])
            if case .object(let timings) = options["turn_timing"] {
                for timing in timings.values { try keys(timing, ["started_at", "ended_at"]) }
            }
            try keys(fixture["page"], ["start", "end", "max_items", "expected"])
            try keys(fixture["page"]["expected"], ["start", "end", "provisional"])
            for action in fixture["actions"].array {
                try keys(action, ["action", "id", "open", "mode", "items", "expected_open_ids", "anchors", "expected_rows"])
                for item in action["items"].array { try validateItem(item) }
            }
        }
    }

    private static func decodeFile(_ data: Data) throws -> FixtureFile {
        let raw = try JSONDecoder().decode(JSONValue.self, from: data)
        try validateFile(raw)
        let file = try JSONDecoder().decode(FixtureFile.self, from: data)
        guard !(file.cases ?? []).isEmpty || !(file.shell_cases ?? []).isEmpty else {
            throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "Fixture file has no executable cases"))
        }
        return file
    }

    private func fixtures() throws -> Catalog {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let folder = root.appendingPathComponent("spec/transcript-grouping/v2", isDirectory: true)
        let paths = try FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }.sorted { $0.path < $1.path }
        XCTAssertEqual(paths.count, 7, "Every shared v2 JSON file must be available from the checkout")
        var catalog = Catalog(cases: [], shellCases: [])
        for path in paths {
            let file = try Self.decodeFile(Data(contentsOf: path))
            XCTAssertEqual(file.spec_version, ChatProjection.specVersion, path.lastPathComponent)
            catalog.cases += file.cases ?? []
            catalog.shellCases += file.shell_cases ?? []
        }
        XCTAssertEqual(catalog.cases.count, 59, "Every projection fixture needs a separate XCTest entry")
        XCTAssertEqual(catalog.shellCases.count, 44, "Every shared shell fixture needs a separate XCTest entry")
        XCTAssertEqual(Set(catalog.cases.map(\.name)).count, catalog.cases.count, "Fixture names must be unique")
        XCTAssertEqual(Set(catalog.shellCases.map(\.command)).count, catalog.shellCases.count, "Shell cases must be unique")
        return catalog
    }

    private func find(_ rows: [ChatProjectionRow], _ id: String) -> ChatProjectionRow? {
        for row in rows {
            if row.id == id { return row }
            if let child = find(row.children, id) ?? find(row.comments, id) { return child }
        }
        return nil
    }
    private func openIDs(_ rows: [ChatProjectionRow]) -> [String] {
        rows.flatMap { ($0.open ? [$0.id] : []) + openIDs($0.children) }.sorted()
    }

    private func check(_ index: Int) throws {
        let fixtures = try fixtures().cases
        var fixture = try XCTUnwrap(fixtures.indices.contains(index) ? fixtures[index] : nil)
        var options = fixture.options ?? .init()
        if let page = fixture.page {
            let window = ChatProjection.turnAlignedPage(items: fixture.items, start: page.start, end: page.end, maxItems: page.max_items)
            XCTAssertEqual(window, page.expected, "\(fixture.name) page")
            fixture.items = Array(fixture.items[window.start..<window.end])
            options.provisional = options.provisional || window.provisional
        }
        func project() -> [ChatProjectionRow] {
            ChatProjection.project(items: fixture.items, itemSequences: fixture.item_sequences ?? [:], requests: fixture.requests ?? [], options: options)
        }
        var rows = project()
        XCTAssertEqual(try json(rows), fixture.expected_rows, fixture.name)
        for (item, row) in fixture.anchors ?? [:] {
            XCTAssertEqual(ChatProjection.visibleAnchor(rows: rows, itemID: item), row, "\(fixture.name) anchor \(item)")
        }
        if let expected = fixture.expected_selection {
            XCTAssertEqual(ChatProjection.selectionLeaves(rows: rows).map(\.text), expected, "\(fixture.name) selection")
        }
        for action in fixture.actions ?? [] {
            switch action.action {
            case "toggle":
                let id = try XCTUnwrap(action.id)
                options.disclosure.toggle(id, currentlyOpen: try XCTUnwrap(find(rows, id)).open)
            case "bulk": options.disclosure.resetBulk(open: try XCTUnwrap(action.open))
            case "mode":
                options.mode = try XCTUnwrap(action.mode)
                options.disclosure.resetMode()
            case "backfill":
                fixture.items = try XCTUnwrap(action.items)
                options.provisional = false
                let after = project()
                options.disclosure = ChatProjection.remapDisclosures(state: options.disclosure, before: rows, after: after)
            default: XCTFail("Unknown action \(action.action)")
            }
            rows = project()
            XCTAssertEqual(openIDs(rows), action.expected_open_ids.sorted(), "\(fixture.name) \(action.action)")
            if let expected = action.expected_rows { XCTAssertEqual(try json(rows), expected, "\(fixture.name) \(action.action) rows") }
            for (item, row) in action.anchors ?? [:] {
                XCTAssertEqual(ChatProjection.visibleAnchor(rows: rows, itemID: item), row, "\(fixture.name) \(action.action) anchor \(item)")
            }
        }
    }

    private func json<T: Encodable>(_ value: T) throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(value))
    }

    private func checkShell(_ index: Int) throws {
        let shells = try fixtures().shellCases
        let fixture = try XCTUnwrap(shells.indices.contains(index) ? shells[index] : nil)
        XCTAssertEqual(ChatProjection.isReadOnlyShell(fixture.command), fixture.allowed, fixture.command)
    }

    func testFixtureSchemaRejectsUnknownContractKeys() throws {
        let item = #"{"id":"a","body":{"type":"tool_call","tool":"Read"}}"#
        let invalidFields: [(String, String)] = [
            ("case", #""item_sequence":{}"#),
            ("item", #""items":[{"id":"a","started":1,"body":{"type":"reasoning","text":"x"}}]"#),
            ("body", #""items":[{"id":"a","body":{"type":"tool_call","tool":"Read","outpt":"x"}}]"#),
            ("presentation", #""items":[{"id":"a","body":{"type":"reasoning","text":"x"},"presentation":{"semantic":{}}}]"#),
            ("semantics", #""items":[{"id":"a","body":{"type":"reasoning","text":"x"},"presentation":{"semantics":{"targt":"a"}}}]"#),
            ("range", #""items":[{"id":"a","body":{"type":"reasoning","text":"x"},"presentation":{"semantics":{"range":{"strt":1}}}}]"#),
            ("worker", #""items":[{"id":"a","body":{"type":"reasoning","text":"x"},"presentation":{"semantics":{"worker":{"child_cout":2}}}}]"#),
            ("produced", #""items":[{"id":"a","body":{"type":"reasoning","text":"x"},"presentation":{"semantics":{"produced":[{"paht":"x"}]}}}]"#),
            ("image", #""items":[{"id":"a","body":{"type":"reasoning","text":"x"},"presentation":{"images":[{"lable":"x"}]}}]"#),
            ("image source", #""items":[{"id":"a","body":{"type":"reasoning","text":"x"},"presentation":{"images":[{"source":{"kind":"local","paht":"x"}}]}}]"#),
            ("attachment", #""items":[{"id":"a","body":{"type":"reasoning","text":"x"},"presentation":{"attachments":[{"naem":"x"}]}}]"#),
            ("attachment kind", #""items":[{"id":"a","body":{"type":"reasoning","text":"x"},"presentation":{"attachments":[{"kind":{"mimee":"x"}}]}}]"#),
            ("attachment preview", #""items":[{"id":"a","body":{"type":"reasoning","text":"x"},"presentation":{"attachments":[{"preview":{"exerpt":"x"}}]}}]"#),
            ("elision", #""items":[{"id":"a","body":{"type":"reasoning","text":"x"},"elided":{"reasn":"x"}}]"#),
            ("plan step", #""items":[{"id":"a","body":{"type":"plan","steps":[{"tetx":"x"}]}}]"#),
            ("todo item", #""items":[{"id":"a","body":{"type":"todo","items":[{"tetx":"x"}]}}]"#),
            ("file change", #""items":[{"id":"a","body":{"type":"file_change","changes":[{"paht":"x"}]}}]"#),
            ("options", #""options":{"running_turn":"t"}"#),
            ("timing", #""options":{"turn_timing":{"t":{"started":1}}}"#),
            ("disclosure", #""options":{"disclosure":{"override":{}}}"#),
            ("action", #""actions":[{"action":"bulk","open":true,"expected_open_ids":[],"opened":true}]"#),
            ("action item", #""actions":[{"action":"backfill","items":[{"id":"a","started":1,"body":{"type":"reasoning","text":"x"}}],"expected_open_ids":[]}]"#),
            ("page", #""page":{"start":0,"end":0,"expected":{"start":0,"end":0,"provisional":false},"limit":1}"#),
            ("window", #""page":{"start":0,"end":0,"expected":{"start":0,"end":0,"provisional":false,"limt":1}}"#),
            ("request", #""requests":[{"request_id":"q","kind":"question","title":"Question","requested_seq":1,"requested_sequnce":1}]"#)
        ]
        for (boundary, field) in invalidFields {
            // Replacing items avoids duplicate JSON keys even in schema tests.
            let items = field.hasPrefix("\"items\"") ? "" : "\"items\":[\(item)],"
            let data = Data("{\"spec_version\":2,\"cases\":[{\"name\":\"strict\",\(items)\"expected_rows\":[],\(field)}]}".utf8)
            XCTAssertThrowsError(try Self.decodeFile(data), "\(boundary) typo accepted")
        }
        XCTAssertThrowsError(try Self.decodeFile(Data(#"{"spec_version":2,"spec_verison":2,"cases":[{"name":"strict","items":[],"expected_rows":[]}]}"#.utf8)))
        XCTAssertThrowsError(try Self.decodeFile(Data(#"{"spec_version":2,"shell_cases":[{"command":"ls","allowed":true,"allowd":true}]}"#.utf8)))
        XCTAssertThrowsError(try Self.decodeFile(Data(#"{"spec_version":2}"#.utf8)))
        _ = try Self.decodeFile(Data(#"{"spec_version":2,"cases":[{"name":"strict","items":[],"expected_rows":[]}]}"#.utf8))
    }

    func testFixtureSchemaAllowsArbitraryToolInputAndMalformedOptionalMetadata() throws {
        let data = Data(#"{"spec_version":2,"cases":[{"name":"optional hints","items":[{"id":"a","body":{"type":"tool_call","tool":"Read","input":{"unknown_future_key":{"running_turn":true}}},"presentation":{"semantics":{"kind":"future_kind","target":7,"range":"bad","worker":{"role":4},"produced":[null,7,{"path":3},{"path":"valid.swift"}]}}},{"id":"b","body":{"type":"reasoning","text":"x"},"presentation":{"semantics":{"elided":true}}}],"expected_rows":[]}]}"#.utf8)
        let fixture = try XCTUnwrap(try Self.decodeFile(data).cases?.first)
        XCTAssertEqual(fixture.items.count, 2)
        XCTAssertEqual(fixture.items[0].presentation.semantics?.produced.map(\.path), ["valid.swift"])
        XCTAssertNil(fixture.items[1].presentation.semantics)
    }

    func testFixtureExpectedRowsRemainExactRawJSON() throws {
        let data = Data(#"{"spec_version":2,"cases":[{"name":"raw expected","items":[],"expected_rows":[{"id":"a","failed_cout":1}],"actions":[{"action":"bulk","open":true,"expected_open_ids":[],"expected_rows":[{"future_output_key":true}]}]}]}"#.utf8)
        let fixture = try XCTUnwrap(try Self.decodeFile(data).cases?.first)
        XCTAssertEqual(fixture.expected_rows.array.first?["failed_cout"], .number(1))
        XCTAssertEqual(fixture.actions?.first?.expected_rows?.array.first?["future_output_key"], .bool(true))
        XCTAssertNotEqual(try json([ChatProjectionRow(id: "a")]), fixture.expected_rows)
    }

    // Separate XCTest entries preserve per-case pass/fail counts in xcresult.
    func testSharedV2Case01() throws { try check(0) }
    func testSharedV2Case02() throws { try check(1) }
    func testSharedV2Case03() throws { try check(2) }
    func testSharedV2Case04() throws { try check(3) }
    func testSharedV2Case05() throws { try check(4) }
    func testSharedV2Case06() throws { try check(5) }
    func testSharedV2Case07() throws { try check(6) }
    func testSharedV2Case08() throws { try check(7) }
    func testSharedV2Case09() throws { try check(8) }
    func testSharedV2Case10() throws { try check(9) }
    func testSharedV2Case11() throws { try check(10) }
    func testSharedV2Case12() throws { try check(11) }
    func testSharedV2Case13() throws { try check(12) }
    func testSharedV2Case14() throws { try check(13) }
    func testSharedV2Case15() throws { try check(14) }
    func testSharedV2Case16() throws { try check(15) }
    func testSharedV2Case17() throws { try check(16) }
    func testSharedV2Case18() throws { try check(17) }
    func testSharedV2Case19() throws { try check(18) }
    func testSharedV2Case20() throws { try check(19) }
    func testSharedV2Case21() throws { try check(20) }
    func testSharedV2Case22() throws { try check(21) }
    func testSharedV2Case23() throws { try check(22) }
    func testSharedV2Case24() throws { try check(23) }
    func testSharedV2Case25() throws { try check(24) }
    func testSharedV2Case26() throws { try check(25) }
    func testSharedV2Case27() throws { try check(26) }
    func testSharedV2Case28() throws { try check(27) }
    func testSharedV2Case29() throws { try check(28) }
    func testSharedV2Case30() throws { try check(29) }
    func testSharedV2Case31() throws { try check(30) }
    func testSharedV2Case32() throws { try check(31) }
    func testSharedV2Case33() throws { try check(32) }
    func testSharedV2Case34() throws { try check(33) }
    func testSharedV2Case35() throws { try check(34) }
    func testSharedV2Case36() throws { try check(35) }
    func testSharedV2Case37() throws { try check(36) }
    func testSharedV2Case38() throws { try check(37) }
    func testSharedV2Case39() throws { try check(38) }
    func testSharedV2Case40() throws { try check(39) }
    func testSharedV2Case41() throws { try check(40) }
    func testSharedV2Case42() throws { try check(41) }
    func testSharedV2Case43() throws { try check(42) }
    func testSharedV2Case44() throws { try check(43) }
    func testSharedV2Case45() throws { try check(44) }
    func testSharedV2Case46() throws { try check(45) }
    func testSharedV2Case47() throws { try check(46) }
    func testSharedV2Case48() throws { try check(47) }
    func testSharedV2Case49() throws { try check(48) }
    func testSharedV2Case50() throws { try check(49) }
    func testSharedV2Case51() throws { try check(50) }
    func testSharedV2Case52() throws { try check(51) }
    func testSharedV2Case53() throws { try check(52) }
    func testSharedV2Case54() throws { try check(53) }
    func testSharedV2Case55() throws { try check(54) }
    func testSharedV2Case56() throws { try check(55) }
    func testSharedV2Case57() throws { try check(56) }
    func testSharedV2Case58() throws { try check(57) }
    func testSharedV2Case59() throws { try check(58) }

    func testSharedShellCase01() throws { try checkShell(0) }
    func testSharedShellCase02() throws { try checkShell(1) }
    func testSharedShellCase03() throws { try checkShell(2) }
    func testSharedShellCase04() throws { try checkShell(3) }
    func testSharedShellCase05() throws { try checkShell(4) }
    func testSharedShellCase06() throws { try checkShell(5) }
    func testSharedShellCase07() throws { try checkShell(6) }
    func testSharedShellCase08() throws { try checkShell(7) }
    func testSharedShellCase09() throws { try checkShell(8) }
    func testSharedShellCase10() throws { try checkShell(9) }
    func testSharedShellCase11() throws { try checkShell(10) }
    func testSharedShellCase12() throws { try checkShell(11) }
    func testSharedShellCase13() throws { try checkShell(12) }
    func testSharedShellCase14() throws { try checkShell(13) }
    func testSharedShellCase15() throws { try checkShell(14) }
    func testSharedShellCase16() throws { try checkShell(15) }
    func testSharedShellCase17() throws { try checkShell(16) }
    func testSharedShellCase18() throws { try checkShell(17) }
    func testSharedShellCase19() throws { try checkShell(18) }
    func testSharedShellCase20() throws { try checkShell(19) }
    func testSharedShellCase21() throws { try checkShell(20) }
    func testSharedShellCase22() throws { try checkShell(21) }
    func testSharedShellCase23() throws { try checkShell(22) }
    func testSharedShellCase24() throws { try checkShell(23) }
    func testSharedShellCase25() throws { try checkShell(24) }
    func testSharedShellCase26() throws { try checkShell(25) }
    func testSharedShellCase27() throws { try checkShell(26) }
    func testSharedShellCase28() throws { try checkShell(27) }
    func testSharedShellCase29() throws { try checkShell(28) }
    func testSharedShellCase30() throws { try checkShell(29) }
    func testSharedShellCase31() throws { try checkShell(30) }
    func testSharedShellCase32() throws { try checkShell(31) }
    func testSharedShellCase33() throws { try checkShell(32) }
    func testSharedShellCase34() throws { try checkShell(33) }
    func testSharedShellCase35() throws { try checkShell(34) }
    func testSharedShellCase36() throws { try checkShell(35) }
    func testSharedShellCase37() throws { try checkShell(36) }
    func testSharedShellCase38() throws { try checkShell(37) }
    func testSharedShellCase39() throws { try checkShell(38) }
    func testSharedShellCase40() throws { try checkShell(39) }
    func testSharedShellCase41() throws { try checkShell(40) }
    func testSharedShellCase42() throws { try checkShell(41) }
    func testSharedShellCase43() throws { try checkShell(42) }
    func testSharedShellCase44() throws { try checkShell(43) }

    func testTranscriptAdapterRetainsFirstArrivalRequestBoundaryAcrossUpdates() {
        let a = ChatItem(id: "a", turnID: "t", status: .completed, body: .toolCall(server: nil, tool: "Read", input: .object(["path": .string("a.swift")]), output: nil))
        let b = ChatItem(id: "b", turnID: "t", status: .completed, body: .toolCall(server: nil, tool: "Read", input: .object(["path": .string("b.swift")]), output: nil))
        var transcript = ChatTranscript()
        transcript.apply(.itemStarted(a), seq: 1)
        transcript.apply(.questionRequested(.init(requestID: "q", questions: [])), seq: 2)
        transcript.apply(.itemStarted(b), seq: 3)
        transcript.apply(.itemCompleted(a), seq: 4)
        XCTAssertEqual(transcript.itemSequences, ["a": 1, "b": 3])
        let rows = ChatProjection.project(transcript: transcript, options: .init(mode: .verbose))
        XCTAssertEqual(rows.map(\.id), ["a", "request:q", "b"])
    }

    func testShellClassifierMatchesEngineConservativeQuotingRules() {
        for command in ["rg needle src", "grep 'a > b' file", "sed -n '1,120p' model.rs", "cat 'a; b' | head -n 5", "ls && git status --short", "git log -3", "git diff -- src", "git show HEAD", "find . -name '*.rs'", "tail -n 20 log"] {
            XCTAssertTrue(ChatProjection.isReadOnlyShell(command), command)
        }
        for command in ["", "npm test", "cat a > b", "rg foo && rm file", "find . -delete", "find . -exec rm '{}' ';'", "sed -i 's/a/b/' f", "sed -n '1p;w output' f", "rg --pre=evil file", "git diff --output=result", "git -c alias.x='!rm f' x", "cat $(touch file)", "cat \"`touch file`\"", "cat a &", "cat 'unterminated", "ls ||", "ls\nrm file"] {
            XCTAssertFalse(ChatProjection.isReadOnlyShell(command), command)
        }
    }
}

extension ChatProjectionTests {
    private func projectionPage(_ order: UInt64, turn: String = "t", before: UInt64? = nil,
                                more: Bool = true, provisional: Bool = false) -> ChatSnapshotReply {
        .init(chatID: "c", cursor: "projection", next: 1000, before: before ?? order, more: more,
              items: [.init(order: order, item: .init(id: "i\(order)", turnID: turn, status: .completed,
                                                     body: .reasoning("work")))], controls: [], provisional: provisional)
    }

    func testProvisionalBoundarySurvivesHydrationAndClearsAfterWholeTurnBackfill() {
        var feed = ChatFeed()
        feed.install(projectionPage(90, provisional: true))
        XCTAssertTrue(feed.provisional)
        feed.hydrate(projectionPage(50))
        XCTAssertTrue(feed.provisional, "hydration supplies bodies, not the oldest page boundary")
        feed.prepend(projectionPage(60, provisional: true), requestedBefore: 90)
        XCTAssertTrue(feed.provisional, "another partial page still needs older members")
        feed.prepend(projectionPage(30, before: 20, provisional: true), requestedBefore: 60)
        XCTAssertFalse(feed.provisional, "the history page's partial newest edge joins existing rows; its oldest turn is intact")
        feed.prepend(projectionPage(10, more: false, provisional: true), requestedBefore: 20)
        XCTAssertFalse(feed.provisional, "no older history means the merged transcript has its whole prefix")
    }

    func testProvisionalFlagIgnoresStalePagesAndResetsForBoundedReplay() {
        var feed = ChatFeed()
        feed.install(projectionPage(90, provisional: true))
        feed.prepend(projectionPage(20), requestedBefore: 80)
        XCTAssertTrue(feed.provisional, "a rejected stale page cannot change the boundary")
        feed.beginDegradedReplay()
        XCTAssertFalse(feed.provisional, "sequential replay reconstructs from the beginning")
    }
}

private actor ProjectionSnapshotTransport: RemoteTransport {
    let reply: JSONValue
    init(_ reply: ChatSnapshotReply) throws { self.reply = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(reply)) }
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { pairing }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue { reply }
    func disconnect() async {}
    func isConnected() async -> Bool { true }
}

extension ChatProjectionTests {
    private var snapshotChatID: String { "cccccccc-1111-4111-8111-111111111111" }
    private func snapshotReply(orders: [UInt64], before: UInt64 = 10) -> ChatSnapshotReply {
        .init(chatID: snapshotChatID, cursor: "projection", next: 1000, before: before, more: true,
              items: orders.map { .init(order: $0, item: .init(id: "i\($0)", turnID: "t", status: .completed, body: .reasoning("work"))) }, controls: [])
    }

    func testSnapshotTransportAcceptsTurnBoundaryBeforeFirstItemAndMoreThanPreferredCount() async throws {
        let expected = snapshotReply(orders: Array(20...80))
        let transport = try ProjectionSnapshotTransport(expected)
        let decoded = try await transport.chatSnapshot(chatID: snapshotChatID)
        XCTAssertEqual(decoded.items.count, 61, "the 50-item request preference extends to a whole turn")
        XCTAssertEqual(decoded.before, 10, "turn-start cursor can precede the first item and its request records")
    }

    func testSnapshotTransportStillRejectsInvalidOrderingAndHydrationOverflow() async throws {
        for page in [snapshotReply(orders: [20, 19]), snapshotReply(orders: [20, 20]), snapshotReply(orders: [20], before: 21), snapshotReply(orders: [1001])] {
            let transport = try ProjectionSnapshotTransport(page)
            do { _ = try await transport.chatSnapshot(chatID: snapshotChatID); XCTFail("invalid snapshot accepted: \(page)") }
            catch { XCTAssertEqual(error as? ChatControlError, .unreadableReply) }
        }
        let transport = try ProjectionSnapshotTransport(snapshotReply(orders: Array(20...120)))
        do { _ = try await transport.chatSnapshot(chatID: snapshotChatID, cursor: "projection", itemIDs: ["i20"]); XCTFail("hydration overflow accepted") }
        catch { XCTAssertEqual(error as? ChatControlError, .unreadableReply) }
    }
}


extension ChatProjectionTests {
    func testLargeToolTranscriptStreamingAndDisclosureBudget() {
        var transcript = ToolUIPerformanceFixture.transcript()
        let cache = ChatProjectionCache()
        let initial = cache.project(transcript: transcript)
        XCTAssertEqual(initial.first { $0.id == "activity:perf-dense" }?.children.first { $0.kind == .group }?.children.count, 1200,
                       "the CI fixture must exercise a dense group, not only many short turns")
        XCTAssertEqual(transcript.items.reduce(0) { $0 + $1.presentation.images.count }, 100)
        let events = 100
        let baselineStart = ContinuousClock.now
        for _ in 0..<20 { _ = ChatProjection.project(transcript: transcript, options: .init(nowMS: 10_000)) }
        let baseline = baselineStart.duration(to: .now).milliseconds / 20
        let revision = transcript.projectionRevision
        let start = ContinuousClock.now
        for _ in 0..<events {
            transcript.apply(.itemDelta(itemID: "perf-live", delta: .text(" token")))
            _ = cache.project(transcript: transcript)
        }
        let perEvent = start.duration(to: .now).milliseconds / Double(events)
        XCTAssertEqual(transcript.projectionRevision, revision)
        XCTAssertEqual(cache.builds, 1, "body deltas must never regroup historical calls")
        XCTAssertLessThan(perEvent, 8, "large-fixture streaming budget in a debug build")
        _ = cache.project(transcript: transcript, mode: .verbose)
        let builds = cache.builds
        let toggles = 60
        let toggleStart = ContinuousClock.now
        for index in 0..<toggles {
            let disclosure = ChatDisclosureState(bulk: index % 2 == 0)
            let mode: ChatDisplayMode = index % 3 == 0 ? .verbose : .normal
            let rows = cache.project(transcript: transcript, mode: mode, disclosure: disclosure)
            if index == 0 {
                XCTAssertEqual(rows, ChatProjection.project(transcript: transcript, options: .init(mode: mode, nowMS: cache.clockMS, disclosure: disclosure)))
            }
        }
        let perToggle = toggleStart.duration(to: .now).milliseconds / Double(toggles)
        XCTAssertEqual(cache.builds, builds, "disclosure and revisiting a display mode must reuse grouping")
        XCTAssertLessThan(perToggle, 25, "large-fixture toggle budget in a debug build")
        transcript.apply(.itemCompleted(.init(id: "new-call", turnID: "perf-live", status: .completed, body: .command(command: "cat next", cwd: nil, output: "", exitCode: 0))))
        _ = cache.project(transcript: transcript)
        XCTAssertEqual(cache.builds, builds + 1, "structural updates invalidate grouping")
        print(String(format: "PERF tool-ui 2200 items + 100 images: uncached projection %.3f ms/event; cached stream %.3f ms/event; disclosure/mode %.3f ms/toggle", baseline, perEvent, perToggle))
    }
}

private extension Duration {
    var milliseconds: Double { Double(components.seconds) * 1000 + Double(components.attoseconds) / 1e15 }
}

extension ChatProjectionTests {
    func testCachedProjectionPreservesDisclosureRulesInBothModes() {
        var transcript = ChatTranscript()
        for item in ToolUIPerformanceFixture.items().prefix(44) { transcript.apply(.itemCompleted(item)) }
        let cache = ChatProjectionCache()
        for mode in ChatDisplayMode.allCases {
            for disclosure in [ChatDisclosureState(), .init(bulk: true), .init(bulk: false),
                               .init(overrides: ["activity:perf-dense": true, "g:perf-dense:perf-item-0": true, "perf-item-1": true])] {
                let cached = cache.project(transcript: transcript, mode: mode, disclosure: disclosure, nowMS: 10_000)
                let expected = ChatProjection.project(transcript: transcript, options: .init(mode: mode, nowMS: 10_000, disclosure: disclosure))
                XCTAssertEqual(cached, expected, "\(mode), \(disclosure)")
            }
        }
        XCTAssertEqual(cache.builds, 2)
    }
}
