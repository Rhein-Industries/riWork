import XCTest
@testable import RiWorkCore

final class ACPCompatibilityTests: XCTestCase {
    func testAnACPChatDoesNotBreakMixedProviderLists() throws {
        let data = Data(#"[{"id":"aaaaaaaa-1111-4111-8111-111111111111","provider":"acp","cwd":"/work","title":"ACP chat","created_at_unix":1,"state":{"state":"waiting"}},{"id":"bbbbbbbb-1111-4111-8111-111111111111","provider":"codex","cwd":"/work","title":"Codex chat","created_at_unix":1,"state":{"state":"idle"}}]"#.utf8)
        let chats = try JSONDecoder().decode([ChatInfo].self, from: data)
        XCTAssertEqual(chats.map(\.provider), [.acp, .codex])
        XCTAssertEqual(chats.first?.provider.chatTitle, "ACP chat")
        XCTAssertTrue(ChatBundledModels.models(for: .acp).isEmpty)
        let request = try ChatCreateRequest(provider: .acp, target: .project("aaaaaaaa-1111-4111-8111-111111111111"))
        XCTAssertEqual(request.params["provider"], .string("acp"))
        XCTAssertNil(request.params["fast"])
        XCTAssertNil(request.params["effort"])
        XCTAssertNil(request.params["contributor_opt_out"])
    }
}
