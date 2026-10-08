import XCTest
@testable import RiWorkCore

@MainActor final class SharedTabsTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let session = "22222222-2222-4222-8222-222222222222"
    private func value(_ json: String) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: Data(json.utf8)) }
    func testBothCallsValidateAndAllUpdatesHaveTheirOwnShape() throws {
        let id = UUID().uuidString.lowercased(), key = "chat:\(session)"
        try RequestValidation.validate(method: "tabs.list", params: ["project_id": .string(project)], id: id)
        for update in [TabUpdate.pin(key), .unpin(key), .hide(key), .unhide(key), .move(key, before: nil), .rename(key, title: "Name")] {
            try RequestValidation.validate(method: "tabs.update", params: ["project_id": .string(project), "update": update.json], id: id)
        }
        XCTAssertThrowsError(try RequestValidation.validate(method: "tabs.update", params: ["project_id": .string(project), "update": TabUpdate.hide("../x").json], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "tabs.update", params: ["project_id": .string(project), "update": TabUpdate.rename(key, title: "\n").json], id: id))
        XCTAssertThrowsError(try RequestValidation.validate(method: "tabs.list", params: ["project_id": .string(project), "stop": .bool(true)], id: id))
    }
    func testDecodeKeepsChildrenStatusAndFiltersHiddenAndParentedEntries() throws {
        let base: [String: JSONValue] = ["key": .string("chat:\(session)"), "kind": .string("chat"), "title": .string("Shared"), "status": .string("waiting"), "pinned": .bool(true), "hidden": .bool(false), "order": .number(0), "parent": .null, "children": .array([]), "child_count": .number(0)]
        var child = base; child["key"] = .string("shell:\(project)"); child["kind"] = .string("shell"); child["parent"] = base["key"]; child["pinned"] = .bool(false)
        var parent = base; parent["children"] = .array([.object(child)]); parent["child_count"] = .number(1)
        var hidden = base; hidden["hidden"] = .bool(true)
        var dead = base; dead["kind"] = .string("shell"); dead["status"] = .string("stopped")
        let reply = try JSONValue.object(["entries": .array([.object(parent), .object(hidden), .object(dead)])]).decode(SharedTabsReply.self)
        XCTAssertEqual(reply.allEntries.count, 4); XCTAssertEqual(reply.visible.count, 2); XCTAssertEqual(reply.visible[0].children[0].status, .waiting)
        XCTAssertEqual(reply.visible[0].childCount, 1); XCTAssertEqual(reply.visible[0].sessionID, session)
    }
    func testUnknownEnumsKeepTheReplyAndRenameCanBeClearedButCannotSpoofDirection() throws {
        let id = UUID().uuidString.lowercased()
        let reply = try value("{\"entries\":[{\"key\":\"future:\(session)\",\"kind\":\"future\",\"status\":\"future\",\"title\":\"future\",\"pinned\":false,\"hidden\":false,\"order\":0,\"parent\":null,\"children\":[],\"child_count\":0}]}").decode(SharedTabsReply.self)
        XCTAssertEqual(reply.entries[0].kind, .unknown); XCTAssertEqual(reply.entries[0].status, .unknown); XCTAssertEqual(reply.entries[0].sessionID, session)
        try RequestValidation.validate(method: "tabs.update", params: ["project_id": .string(project), "update": TabUpdate.rename("chat:\(session)", title: "").json], id: id)
        for title in ["👩‍💻", "soft\u{00ad}hyphen"] { try RequestValidation.validate(method: "tabs.update", params: ["project_id": .string(project), "update": TabUpdate.rename("chat:\(session)", title: title).json], id: id) }
        for title in ["bad\u{202e}title", "bad\u{2067}title", "bad\u{0000}"] { XCTAssertThrowsError(try RequestValidation.validate(method: "tabs.update", params: ["project_id": .string(project), "update": TabUpdate.rename("chat:\(session)", title: title).json], id: id)) }
    }
    func testParentIDsAreBackwardCompatibleOnChatsAndShells() throws {
        let chat = try value("{\"id\":\"\(session)\",\"provider\":\"codex\",\"parent_id\":\"\(project)\"}").decode(ChatInfo.self)
        XCTAssertEqual(chat.parentID, project)
        XCTAssertNil(try value("{\"id\":\"\(session)\",\"provider\":\"claude\"}").decode(ChatInfo.self).parentID)
        let shell = try value("{\"id\":\"\(session)\",\"parent_id\":\"\(project)\",\"kind\":\"project\",\"cwd\":\"/p\",\"alive\":true,\"created_at_unix\":0}").decode(RemoteSession.self)
        XCTAssertEqual(shell.parent_id, project)
    }
    func testClosePreferenceAlwaysDetachesWorkersEvenAfterParentExit() throws {
        let base: [String: JSONValue] = ["key": .string("shell:\(session)"), "kind": .string("shell"), "title": .string("Worker"), "status": .string("working"), "pinned": .bool(false), "hidden": .bool(false), "worker": .bool(true), "order": .number(0), "parent": .null, "children": .array([]), "child_count": .number(0)]
        let worker = try JSONValue.object(base).decode(SharedTab.self)
        for preference in TabCloseBehavior.allCases { XCTAssertEqual(preference.effectiveChoice(for: worker), .detach) }
        var user = base; user["worker"] = .bool(false)
        let userTab = try JSONValue.object(user).decode(SharedTab.self)
        XCTAssertEqual(TabCloseBehavior.ask.effectiveChoice(for: userTab), .ask)
        XCTAssertEqual(TabCloseBehavior.exit.effectiveChoice(for: userTab), .exit)
        XCTAssertEqual(TabCloseBehavior.settingKey, "tab_close_behavior")
        try RequestValidation.validate(method: "tabs.open", params: ["project_id": .string(project), "key": .string(worker.key)], id: UUID().uuidString.lowercased())
        XCTAssertThrowsError(try RequestValidation.validate(method: "tabs.open", params: ["project_id": .string(project), "key": .string("invalid")], id: UUID().uuidString.lowercased()))
    }
    func testStoreEpochAcceptsResetButRejectsStaleRevisionWithinInstance() throws {
        func reply(_ epoch: String?, _ revision: UInt64) throws -> SharedTabsReply {
            var object: [String: JSONValue] = ["revision": .number(Double(revision)), "entries": .array([])]
            if let epoch { object["epoch"] = .string(epoch) }
            return try JSONValue.object(object).decode(SharedTabsReply.self)
        }
        let current = try reply("old", 100)
        XCTAssertFalse(try reply("old", 2).supersedes(current))
        XCTAssertTrue(try reply("new", 1).supersedes(current))
        XCTAssertTrue(try reply("old", 100).supersedes(current))
        XCTAssertFalse(try reply(nil, 1).supersedes(reply(nil, 2)))
    }
    func testParentFallbackCannotWeakenExplicitWorkerProtection() throws {
        let tab = try value("{\"key\":\"shell:\(session)\",\"kind\":\"shell\",\"title\":\"worker\",\"status\":\"working\",\"pinned\":false,\"hidden\":false,\"worker\":false,\"order\":0,\"parent\":\"chat:\(project)\",\"children\":[],\"child_count\":0}").decode(SharedTab.self)
        XCTAssertEqual(TabCloseBehavior.exit.effectiveChoice(for: tab), .detach)
    }
    func testCapabilityDefaultsOffForOlderHosts() throws {
        XCTAssertFalse(DesktopFeatures(ready: .object([:])).tabs)
        XCTAssertTrue(DesktopFeatures(ready: try value("{\"features\":{\"tabs\":true}}")).tabs)
    }
    func testTransportMethodsSendBothCallsAndDecodeTheirLists() async throws {
        let transport = TabsTransport()
        let list = try await transport.listTabs(projectID: project)
        XCTAssertTrue(list.entries.isEmpty)
        let updated = try await transport.updateTabs(projectID: project, update: .hide("shell:\(session)"))
        XCTAssertTrue(updated.entries.isEmpty)
        let opened = try await transport.openTab(projectID: project, key: "shell:\(session)")
        XCTAssertTrue(opened.entries.isEmpty)
        let methods = await transport.methods
        XCTAssertEqual(methods, ["tabs.list", "tabs.update", "tabs.open"])
    }
}
private actor TabsTransport: RemoteTransport {
    var methods: [String] = []
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { pairing }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue { methods.append(method); return .object(["entries": .array([])]) }
    func disconnect() async {}
    func isConnected() async -> Bool { true }
}
