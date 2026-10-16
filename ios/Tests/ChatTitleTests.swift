import XCTest
@testable import RiWorkCore

final class ChatTitleTests: XCTestCase {
    private func info(_ fields: [String: JSONValue]) throws -> ChatInfo {
        var wire: [String: JSONValue] = ["id": .string("11111111-1111-4111-8111-111111111111"), "provider": .string("codex"), "title": .string("Codex chat")]
        wire.merge(fields) { _, new in new }
        return try JSONValue.object(wire).decode(ChatInfo.self)
    }

    func testDesktopTitleMetadataRoundTripsAndResolvesInDesktopOrder() throws {
        var chat = try info(["user_title": .string("My name"), "provider_title": .string("Provider name"), "first_user_message": .string("First message")])
        XCTAssertEqual(ChatTabs.title(chat), "My name")
        let roundTrip = try JSONDecoder().decode(ChatInfo.self, from: JSONEncoder().encode(chat))
        XCTAssertEqual(roundTrip, chat)
        chat.userTitle = nil
        XCTAssertEqual(ChatTabs.title(chat), "Provider name")
        chat.providerTitle = nil
        XCTAssertEqual(ChatTabs.title(chat), "First message")
        chat.firstUserMessage = nil
        XCTAssertEqual(ChatTabs.title(chat), "Codex chat")
    }

    func testSharedTitleWinsAndStaysOutOfChatMetadata() throws {
        var chat = try info(["user_title": .string("User"), "provider_title": .string("Provider")])
        chat.sharedTitle = "Renamed on desktop"
        XCTAssertEqual(ChatTabs.title(chat), "Renamed on desktop")
        let wire = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(chat))
        XCTAssertEqual(wire["user_title"], .string("User"))
        XCTAssertNil(try wire.decode(ChatInfo.self).sharedTitle)
    }

    func testOrchestratorKeepsItsFixedNameUnlessExplicitlyRenamed() throws {
        var chat = try info(["orchestrator": .object(["scope": .string("project"), "project_id": .string("project")]),
                             "user_title": .string("P·ORCH · PROJECT"), "provider_title": .string("First task"), "first_user_message": .string("Hello")])
        XCTAssertEqual(ChatTabs.title(chat), "Project orchestrator")
        chat.userTitle = "Release coordinator"
        XCTAssertEqual(ChatTabs.title(chat), "Release coordinator")
        chat.orchestrator = .global; chat.userTitle = nil
        XCTAssertEqual(ChatTabs.title(chat), "Global orchestrator")
        XCTAssertEqual(try JSONDecoder().decode(ChatInfo.self, from: JSONEncoder().encode(chat)), chat)
    }

    func testBlankAndMalformedOptionalTitlesKeepTheChatReadable() throws {
        let chat = try info(["user_title": .string(" \n"), "provider_title": .number(7), "first_user_message": .string(""), "orchestrator": .object(["scope": .string("future")])])
        XCTAssertEqual(ChatTabs.title(chat), "Codex chat")
        XCTAssertNil(chat.providerTitle); XCTAssertNil(chat.orchestrator)
        var switched = chat; switched.provider = .claude
        XCTAssertEqual(ChatTabs.title(switched), "Claude chat", "a stale generic name from the old provider is not a user name")
    }
}
