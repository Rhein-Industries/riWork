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
        try keys(presentation, ["phase", "images", "attachments", "semantics", "parent_id", "subagent", "worker", "elided"])
        try keys(presentation["subagent"], ["name", "purpose", "status", "summary", "children", "omitted"])
        for child in presentation["subagent"]["children"].array { try keys(child, ["id", "status", "summary"]) }
        try keys(presentation["worker"], ["session_id", "kind", "title", "harness", "model", "worktree_id", "assignment", "noted_at", "after", "late", "follow_up", "final"])
        try keys(presentation["worker"]["final"], ["status", "ended_at", "tail", "turn", "turn_end", "done_since"])
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
            try keys(options, ["mode", "provider", "now_ms", "running_turn_id", "turn_timing", "disclosure", "provisional", "elided_items"])
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
        XCTAssertEqual(paths.count, 8, "Every shared v2 JSON file must be available from the checkout")
        var catalog = Catalog(cases: [], shellCases: [])
        for path in paths {
            let file = try Self.decodeFile(Data(contentsOf: path))
            XCTAssertEqual(file.spec_version, ChatProjection.specVersion, path.lastPathComponent)
            catalog.cases += file.cases ?? []
            catalog.shellCases += file.shell_cases ?? []
        }
        XCTAssertEqual(catalog.cases.count, 66, "Every shared projection fixture must be checked")
        XCTAssertEqual(catalog.shellCases.count, 44, "Every shared shell fixture must be checked")
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
            case "bulk":
                options.disclosure.resetBulkForRows(open: try XCTUnwrap(action.open), mode: options.mode, rows: rows)
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

    func testSharedProjectionContract() throws {
        for index in try fixtures().cases.indices { try check(index) }
    }

    func testSharedShellContract() throws {
        for index in try fixtures().shellCases.indices { try checkShell(index) }
    }

}
