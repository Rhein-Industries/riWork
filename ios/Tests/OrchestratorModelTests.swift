import XCTest
@testable import RiWorkCore

/// The model a chat orchestrator runs on (`model` on an `orchestrators.list` entry) and the desktop's fallback note when it created one
/// (`fallback` in the `orchestrator.create` reply), both additive and read leniently: shown when the desktop says, never made up.
final class OrchestratorModelTests: XCTestCase {
    private let global = "aaaaaaaa-0002-4000-8000-000000000002"
    private let chat = "cccccccc-0002-4000-8000-000000000002"

    private func entry(_ extra: String, chat mode: Bool = true) throws -> RemoteSession {
        let chatFields = mode ? ",\"mode\":\"chat\",\"chat_id\":\"\(chat)\",\"provider\":\"muse\"" : ""
        let json = "{\"id\":\"\(global)\",\"project_id\":null,\"worktree_id\":null,\"kind\":\"orchestrator\",\"cwd\":\"/\",\"harness\":null,\"alive\":true,\"created_at_unix\":5\(chatFields)\(extra)}"
        return try JSONDecoder().decode(RemoteSession.self, from: Data(json.utf8))
    }

    func testAChatOrchestratorsModelIsReadWhenItIsAShortWordAndOtherwiseLeftOut() throws {
        XCTAssertEqual(try entry(",\"model\":\"muse-wonder-full-1.0\"").model, "muse-wonder-full-1.0")
        XCTAssertNil(try entry("").model, "a desktop that does not say")
        XCTAssertNil(try entry(",\"model\":null").model, "the provider's default: no model is invented")
        for wrong in ["\"\"", "\"two words\"", "4", "{\"id\":\"m\"}", "\"\(String(repeating: "x", count: 121))\"", "\"line\\nbreak\""] {
            let read = try entry(",\"model\":\(wrong)")
            XCTAssertNil(read.model, wrong)
            XCTAssertEqual(read.chat_id, chat, "the rest of the entry stands: \(wrong)")
        }
        XCTAssertNil(try entry(",\"model\":\"gpt-6.1-sol\"", chat: false).model, "a terminal has no model, whatever it carries")
    }

    func testTheGlobalRowNamesTheModelOnlyWhenTheDesktopDoes() throws {
        let named = GlobalOrchestratorStatus.decide(entry: try entry(",\"model\":\"muse-wonder-full-1.0\",\"activity\":\"working\""), dashboard: nil, canStart: true)
        XCTAssertEqual(named.model, "muse-wonder-full-1.0")
        XCTAssertEqual(named.summary, "Muse chat · muse-wonder-full-1.0 · Working")
        let unnamed = GlobalOrchestratorStatus.decide(entry: try entry(",\"activity\":\"working\""), dashboard: nil, canStart: true)
        XCTAssertNil(unnamed.model)
        XCTAssertEqual(unnamed.summary, "Muse chat · Working")
        let none = GlobalOrchestratorStatus.decide(entry: nil, dashboard: .none(exitedTerminal: false), canStart: true)
        XCTAssertNil(none.model)
        XCTAssertEqual(none.actionTitle, "Start global orchestrator", "the Mac's runs-as setting is not on the wire: the wording stays its own")
    }

    func testTheFallbackNoteIsReadForOneMadeNowOnly() throws {
        let request = try NewOrchestratorRequest(projectID: nil)
        func reply(_ created: Bool, _ fallback: String) throws -> NewOrchestratorReply {
            let body = try JSONEncoder().encode(try entry(",\"model\":\"gpt-6.1-sol\""))
            var object = try JSONDecoder().decode(JSONValue.self, from: body)
            if case .object(var fields) = object { fields["mode"] = .string("chat"); fields["chat_id"] = .string(chat); fields["provider"] = .string("codex"); object = .object(fields) }
            let fallbackValue = try JSONDecoder().decode(JSONValue.self, from: Data(fallback.utf8))
            return try NewOrchestratorReply.parse(.object(["orchestrator": object, "created": .bool(created), "fallback": fallbackValue]), for: request)
        }
        let made = try reply(true, "\"Muse could not start. It runs as a Codex chat on gpt-6.1-sol instead.\\u0007\"")
        XCTAssertEqual(made.fallback, "Muse could not start. It runs as a Codex chat on gpt-6.1-sol instead.")
        XCTAssertEqual(made.session.model, "gpt-6.1-sol")
        XCTAssertNil(try reply(false, "\"stale\"").fallback, "one that was there already has no fallback of this call")
        XCTAssertNil(try reply(true, "\"   \"").fallback)
        XCTAssertNil(try reply(true, "7").fallback)
        XCTAssertEqual(try reply(true, "\"\(String(repeating: "é", count: 600))\"").fallback?.count, 500)
    }
}
