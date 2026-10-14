import XCTest
import RiWorkCore
@testable import RiWorkRemote

/// A desktop with project memos, as the connector answers `project.memo.get` and `project.memo.set`, recording what it was asked.
actor MemoTransport: RemoteTransport {
    enum SaveMode { case ok, refuse, timeout }
    var connected = false
    var announces = true
    var saveMode = SaveMode.ok
    /// Saves wait while this is on, as a slow link holds them.
    var gated = false
    var memos: [String: (text: String, at: UInt64)] = [:]
    var methods: [String] = []
    var saved: [String] = []
    private var clock: UInt64 = 1_790_000_000
    private let project = try! JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"11111111-1111-4111-8111-111111111111\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))

    func setAnnounces(_ on: Bool) { announces = on }
    func setSaveMode(_ mode: SaveMode) { saveMode = mode }
    func setGated(_ on: Bool) { gated = on }
    func setMemo(_ project: String, _ text: String) { clock += 1; memos[project] = (text, clock) }
    func memo(_ project: String) -> String? { memos[project]?.text }
    func count(_ method: String) -> Int { methods.filter { $0 == method }.count }
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { connected = true; return pairing }
    func disconnect() async { connected = false }
    func isConnected() async -> Bool { connected }
    func desktopFeatures() async -> DesktopFeatures {
        DesktopFeatures(ready: .object(["features": .object(announces ? ["project_memo": .bool(true)] : [:])]))
    }
    private func reply(_ project: String) -> JSONValue {
        guard let memo = memos[project] else { return .object(["project_id": .string(project), "text": .string("")]) }
        return .object(["project_id": .string(project), "text": .string(memo.text), "updated_at": .number(Double(memo.at))])
    }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        guard connected else { throw RemoteError.disconnected }
        try RequestValidation.validate(method: method, params: params, id: id)
        methods.append(method)
        switch method {
        case "projects.list": return .object(["projects": .array([try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(project))])])
        case "worktrees.list": return .object(["worktrees": .array([])])
        case "orchestrators.list": return .object(["orchestrators": .array([])])
        case "shells.list": return .object(["shells": .array([])])
        case "appearance.get": throw RemoteError.rpc(code: "not_found", message: "appearance not published")
        case "project.memo.get":
            guard announces else { throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method") }
            return reply(params["project_id"]?.string ?? "")
        case "project.memo.set":
            while gated { try await Task.sleep(for: .milliseconds(3)) }
            let project = params["project_id"]?.string ?? ""
            switch saveMode {
            case .refuse: throw RemoteError.rpc(code: "invalid_request", message: "the Mac refused this one")
            case .timeout: throw RemoteError.timeout
            case .ok:
                let text = params["text"]?.string ?? ""
                saved.append(text)
                if memos[project]?.text != text { clock += 1; memos[project] = (text, clock) }
                return reply(project)
            }
        default: throw RemoteError.protocolViolation("Unknown method \(method)")
        }
    }
}

@MainActor final class ProjectMemoModelTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private var defaultsNames: [String] = []

    private func makeStore() throws -> KeychainStore {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        return keychain
    }
    private func defaults() -> UserDefaults {
        let name = "com.riwork.tests.memo.\(UUID().uuidString)"
        defaultsNames.append(name)
        return UserDefaults(suiteName: name)!
    }
    override func tearDown() async throws {
        for name in defaultsNames { UserDefaults().removePersistentDomain(forName: name) }
        defaultsNames = []
    }
    private struct Rig { let model: RemoteModel; let transport: MemoTransport }
    private func connected(_ setup: (MemoTransport) async -> Void = { _ in }) async throws -> Rig {
        let transport = MemoTransport()
        await setup(transport)
        let model = RemoteModel(client: transport, keychain: try makeStore(), defaults: defaults())
        model.memoSaveDelay = .milliseconds(20)
        await model.connect()
        XCTAssertEqual(model.state, .connected)
        return Rig(model: model, transport: transport)
    }
    private func settle(_ condition: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async {
        let deadline = ContinuousClock.now + .seconds(5)
        while !condition(), ContinuousClock.now < deadline { try? await Task.sleep(for: .milliseconds(5)) }
        XCTAssertTrue(condition(), file: file, line: line)
    }
    private func kept(_ model: RemoteModel) -> String? { model.desktop.flatMap { model.memoDrafts.text(desktop: $0.id, project: project) } }

    // MARK: The editor

    func testTheEditorReadsSavesAfterAPauseAndSaysSavedOnlyThen() async throws {
        let rig = try await connected { await $0.setMemo("11111111-1111-4111-8111-111111111111", "# Plan") }
        let model = rig.model
        model.openMemoEditor(projectID: project, name: "Fixture")
        await settle { model.memoEditor?.phase == .saved }
        XCTAssertEqual(model.memoEditor?.text, "# Plan")

        model.editMemo("# Plan\n- ship")
        XCTAssertEqual(model.memoEditor?.phase, .unsaved)
        XCTAssertEqual(kept(model), "# Plan\n- ship", "kept on the phone before the Mac has it")
        await settle { model.memoEditor?.phase == .saved }
        let onMac = await rig.transport.memo(project)
        XCTAssertEqual(onMac, "# Plan\n- ship")
        XCTAssertNil(kept(model), "let go once the Mac confirmed it")
        XCTAssertEqual(model.memoEditor?.status, "Saved automatically")
        model.closeMemoEditor()
        XCTAssertNil(model.memoEditor)
        await model.disconnect()
    }

    func testOfflineTypingIsQueuedKeptAndSavedOnReconnect() async throws {
        let rig = try await connected()
        let model = rig.model
        model.openMemoEditor(projectID: project, name: "Fixture")
        await settle { model.memoEditor?.phase == .saved }
        await model.disconnect()
        model.editMemo("written on the train")
        await settle { model.memoEditor?.phase == .queued }
        XCTAssertTrue(model.memoEditor?.status.contains("kept on this iPhone") ?? false)
        XCTAssertEqual(kept(model), "written on the train")
        let before = await rig.transport.memo(project)
        XCTAssertNil(before)

        await model.connect()
        await settle { model.memoEditor?.phase == .saved }
        let after = await rig.transport.memo(project)
        XCTAssertEqual(after, "written on the train")
        XCTAssertNil(kept(model))
        await model.disconnect()
    }

    func testARefusalSaysNotSavedKeepsTheTextAndRetrySaves() async throws {
        let rig = try await connected { await $0.setSaveMode(.refuse) }
        let model = rig.model
        model.openMemoEditor(projectID: project, name: "Fixture")
        await settle { model.memoEditor?.phase == .saved }
        model.editMemo("precious")
        await settle { if case .failed = model.memoEditor?.phase { true } else { false } }
        XCTAssertTrue(model.memoEditor?.status.hasPrefix("Not saved") ?? false)
        XCTAssertEqual(model.memoEditor?.text, "precious")
        XCTAssertEqual(kept(model), "precious")

        await rig.transport.setSaveMode(.ok)
        await model.retryMemo()
        XCTAssertEqual(model.memoEditor?.phase, .saved)
        let onMac = await rig.transport.memo(project)
        XCTAssertEqual(onMac, "precious")
        await model.disconnect()
    }

    func testClosingWithUnsavedTextSavesItOrKeepsIt() async throws {
        let rig = try await connected()
        let model = rig.model
        model.openMemoEditor(projectID: project, name: "Fixture")
        await settle { model.memoEditor?.phase == .saved }
        model.memoSaveDelay = .seconds(60)
        model.editMemo("closed at once")
        model.closeMemoEditor()
        await settle { kept(model) == nil }
        let onMac = await rig.transport.memo(project)
        XCTAssertEqual(onMac, "closed at once")

        // Closed while the Mac is away: kept, and back in the editor next time, queued.
        model.openMemoEditor(projectID: project, name: "Fixture")
        await settle { model.memoEditor?.phase == .saved }
        await rig.transport.setSaveMode(.timeout)
        model.editMemo("lost answer")
        model.closeMemoEditor()
        try? await Task.sleep(for: .milliseconds(50))
        XCTAssertEqual(kept(model), "lost answer")
        await rig.transport.setSaveMode(.ok)
        model.openMemoEditor(projectID: project, name: "Fixture")
        XCTAssertEqual(model.memoEditor?.text, "lost answer")
        await settle { model.memoEditor?.phase == .saved }
        let saved = await rig.transport.memo(project)
        XCTAssertEqual(saved, "lost answer")
        await model.disconnect()
    }

    func testAnOlderMacIsNeverAskedAndTheEditorSaysWhy() async throws {
        let rig = try await connected { await $0.setAnnounces(false) }
        let model = rig.model
        XCTAssertFalse(model.memoSupported)
        model.openMemoEditor(projectID: project, name: "Fixture")
        await settle { model.memoEditor?.phase == .unsupported }
        XCTAssertEqual(model.memoEditor?.status, ProjectMemoError.unsupportedMessage)
        let asked = await rig.transport.count("project.memo.get")
        XCTAssertEqual(asked, 0)
        await model.disconnect()
    }

    func testTypingDuringASaveNeitherCancelsItNorClaimsTheMacIsAway() async throws {
        let rig = try await connected()
        let model = rig.model
        model.openMemoEditor(projectID: project, name: "Fixture")
        await settle { model.memoEditor?.phase == .saved }
        await rig.transport.setGated(true)
        model.editMemo("a")
        await settle { model.memoEditor?.phase == .saving }
        model.editMemo("ab")
        XCTAssertEqual(model.memoEditor?.phase, .saving)
        await rig.transport.setGated(false)
        await settle { model.memoEditor?.phase == .saved }
        let onMac = await rig.transport.memo(project)
        XCTAssertEqual(onMac, "ab", "the text typed during the save followed it")
        let saves = await rig.transport.saved
        XCTAssertEqual(saves, ["a", "ab"])
        await model.disconnect()
    }

    func testClosingDuringASaveLetsItLandAndNeverBringsItBackOverALaterMacEdit() async throws {
        let rig = try await connected()
        let model = rig.model
        model.openMemoEditor(projectID: project, name: "Fixture")
        await settle { model.memoEditor?.phase == .saved }
        await rig.transport.setGated(true)
        model.editMemo("T1")
        await settle { model.memoEditor?.phase == .saving }
        model.closeMemoEditor()
        await rig.transport.setGated(false)
        await settle { kept(model) == nil }
        let landed = await rig.transport.memo(project)
        XCTAssertEqual(landed, "T1")

        // The Mac edits it later; a reconnect sends nothing old over it.
        await rig.transport.setMemo(project, "T2 from the Mac")
        await model.disconnect()
        await model.connect()
        try? await Task.sleep(for: .milliseconds(100))
        let after = await rig.transport.memo(project)
        XCTAssertEqual(after, "T2 from the Mac")
        await model.disconnect()
    }

    func testKeptTextOverAMemoChangedOnTheMacWaitsForTheChoice() async throws {
        let rig = try await connected { await $0.setMemo("11111111-1111-4111-8111-111111111111", "v1") }
        let model = rig.model
        model.openMemoEditor(projectID: project, name: "Fixture")
        await settle { model.memoEditor?.phase == .saved }
        await model.disconnect()
        model.editMemo("phone edit")
        await settle { model.memoEditor?.phase == .queued }
        model.closeMemoEditor()
        // Meanwhile the Mac changed it.
        await rig.transport.setMemo(project, "Mac edit")
        await model.connect()
        try? await Task.sleep(for: .milliseconds(100))
        let untouched = await rig.transport.memo(project)
        XCTAssertEqual(untouched, "Mac edit", "kept text is not replayed over a newer Mac memo")
        XCTAssertEqual(kept(model), "phone edit", "nor dropped")

        model.openMemoEditor(projectID: project, name: "Fixture")
        await settle { model.memoEditor?.phase == .conflict }
        XCTAssertEqual(model.memoEditor?.text, "phone edit")
        model.resolveMemoConflict(keepMine: true)
        await settle { model.memoEditor?.phase == .saved }
        let mine = await rig.transport.memo(project)
        XCTAssertEqual(mine, "phone edit")
        XCTAssertNil(kept(model))
        await model.disconnect()
    }

    // MARK: `/memo`

    func testMemoInAProjectChatPreviewsAndInsertAppendsWithoutSending() async throws {
        let rig = try await connected { await $0.setMemo("11111111-1111-4111-8111-111111111111", "# Notes\n- a\n") }
        let model = rig.model
        let chat = ChatInfo(id: "44444444-4444-4444-8444-444444444444", provider: .codex, projectID: project)
        model.conversation(chat.id).draft = "Use these:\n/memo"
        XCTAssertTrue(model.openMemoPreview(chat: chat))
        XCTAssertEqual(model.conversation(chat.id).draft, "Use these:")
        await settle { if case .memo = model.memoPreview?.preview { true } else { false } }
        XCTAssertTrue(model.memoPreview?.preview.canInsert ?? false)
        model.insertMemoPreview()
        XCTAssertEqual(model.conversation(chat.id).draft, "Use these:\n\n# Notes\n- a")
        XCTAssertNil(model.memoPreview)
        let methods = await rig.transport.methods
        XCTAssertFalse(methods.contains("chat.command"), "nothing is sent")

        // Anything else is not the command.
        model.conversation(chat.id).draft = "/memo please"
        XCTAssertFalse(model.openMemoPreview(chat: chat))
        XCTAssertEqual(model.conversation(chat.id).draft, "/memo please")
        await model.disconnect()
    }

    func testAnEmptyMemoOffersEditAndAProjectlessChatHasNone() async throws {
        let rig = try await connected()
        let model = rig.model
        let chat = ChatInfo(id: "44444444-4444-4444-8444-444444444444", provider: .codex, projectID: project)
        model.conversation(chat.id).draft = "/memo"
        XCTAssertTrue(model.openMemoPreview(chat: chat))
        await settle { if case .memo = model.memoPreview?.preview { true } else { false } }
        XCTAssertFalse(model.memoPreview?.preview.canInsert ?? true)
        XCTAssertTrue(model.memoPreview?.preview.canEdit ?? false)
        model.insertMemoPreview()
        XCTAssertEqual(model.conversation(chat.id).draft, "", "nothing is inserted")

        let global = ChatInfo(id: "55555555-5555-4555-8555-555555555555", provider: .codex)
        model.conversation(global.id).draft = "/memo"
        XCTAssertTrue(model.openMemoPreview(chat: global))
        XCTAssertEqual(model.memoPreview?.preview, .noProject)
        await model.disconnect()
    }
}
