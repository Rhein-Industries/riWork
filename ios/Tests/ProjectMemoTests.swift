import XCTest
@testable import RiWorkCore

/// The project memo on the phone: the text rules the Mac uses (the same vectors as `riwork_core::project_memo`'s tests), the requests
/// and replies of `project.memo.get` / `project.memo.set`, the editor's honest save states, the text kept on the phone while the Mac
/// does not have it, and what `/memo` offers in a chat.
final class ProjectMemoTests: XCTestCase {
    private let project = "00000000-0000-4000-8000-000000000002"
    private let requestID = "00000000-0000-4000-8000-000000000001"

    // MARK: Text rules

    func testNormalizeUnifiesLineBreaksAndDropsControlsOnly() {
        XCTAssertEqual(ProjectMemoText.normalize("# Notes\n\n- a\tb"), "# Notes\n\n- a\tb")
        XCTAssertEqual(ProjectMemoText.normalize("a\r\nb\rc\n"), "a\nb\nc\n")
        XCTAssertEqual(ProjectMemoText.normalize("x\u{1b}[31my\u{0}z\u{7f}"), "x[31myz")
        XCTAssertEqual(ProjectMemoText.normalize("ünï 😀 \u{2028}"), "ünï 😀 \u{2028}")
        let once = ProjectMemoText.normalize("a\r\r\nb\u{85}")
        XCTAssertEqual(once, "a\n\nb")
        XCTAssertEqual(ProjectMemoText.normalize(once), once)
    }

    func testTheCapCountsUTF8Bytes() {
        let full = String(repeating: "x", count: ProjectMemoText.maximumBytes)
        XCTAssertNil(ProjectMemoText.bytesOver(full))
        XCTAssertNil(ProjectMemoText.tooLargeMessage(full))
        XCTAssertEqual(ProjectMemoText.bytesOver(full + "y"), 1)
        let heavy = String(repeating: "\u{65e5}", count: ProjectMemoText.maximumBytes / 3 + 1)
        XCTAssertLessThan(heavy.count, ProjectMemoText.maximumBytes)
        XCTAssertEqual(ProjectMemoText.bytesOver(heavy), 1)
        XCTAssertTrue(ProjectMemoText.tooLargeMessage(heavy)!.contains("32768 bytes"))
        XCTAssertEqual(ProjectMemoText.maximumBytes, 32_768, "the Mac's riwork_core::project_memo::MAX_BYTES")
    }

    func testTheCommandIsTheDraftsLastWord() {
        XCTAssertEqual(ProjectMemoText.commandRest("/memo"), "")
        XCTAssertEqual(ProjectMemoText.commandRest("  /memo \n"), "")
        XCTAssertEqual(ProjectMemoText.commandRest("Review this:\n/memo"), "Review this:")
        XCTAssertEqual(ProjectMemoText.commandRest("see /memo"), "see")
        for draft in ["", "/memos", "x/memo", "/memo please", "/Memo", "memo"] {
            XCTAssertNil(ProjectMemoText.commandRest(draft), draft)
        }
    }

    func testInsertAppendsAfterABlankLineAndNeverReplaces() {
        XCTAssertEqual(ProjectMemoText.appended(to: "", memo: "# Plan\n- ship\n\n"), "# Plan\n- ship")
        XCTAssertEqual(ProjectMemoText.appended(to: "  \n", memo: "memo"), "memo")
        XCTAssertEqual(ProjectMemoText.appended(to: "Use these notes:\n", memo: "- a\n- b"), "Use these notes:\n\n- a\n- b")
        XCTAssertEqual(ProjectMemoText.appended(to: "draft", memo: "  indented"), "draft\n\n  indented")
    }

    // MARK: Wire

    func testRepliesAreTheMemoOfTheProjectAskedAbout() throws {
        let memo = try ProjectMemo.parse(.object(["project_id": .string(project), "text": .string("# Notes"), "updated_at": .number(1_790_000_000)]), projectID: project)
        XCTAssertEqual(memo, ProjectMemo(projectID: project, text: "# Notes", updatedAt: 1_790_000_000))
        let never = try ProjectMemo.parse(.object(["project_id": .string(project), "text": .string("")]), projectID: project)
        XCTAssertNil(never.updatedAt)
        XCTAssertTrue(never.isEmpty)
        for bad: JSONValue in [
            .object(["project_id": .string("other"), "text": .string("")]),
            .object(["project_id": .string(project)]),
            .object(["project_id": .string(project), "text": .number(1)]),
            .object(["project_id": .string(project), "text": .string(""), "updated_at": .number(0)]),
            .object(["project_id": .string(project), "text": .string(""), "updated_at": .number(1.5)]),
            .object(["project_id": .string(project), "text": .string(String(repeating: "x", count: ProjectMemoText.maximumBytes + 1))]),
            .array([]),
        ] {
            XCTAssertThrowsError(try ProjectMemo.parse(bad, projectID: project)) { XCTAssertEqual($0 as? ProjectMemoError, .unreadableReply) }
        }
    }

    func testRequestsAreNormalizedCappedAndChecked() throws {
        XCTAssertEqual(ProjectMemoRequests.get(projectID: project), ["project_id": .string(project)])
        XCTAssertEqual(try ProjectMemoRequests.set(projectID: project, text: "a\r\nb"), ["project_id": .string(project), "text": .string("a\nb")])
        XCTAssertThrowsError(try ProjectMemoRequests.set(projectID: project, text: String(repeating: "x", count: ProjectMemoText.maximumBytes + 1)))
        func check(_ method: String, _ params: [String: JSONValue]) throws { try RequestValidation.validate(method: method, params: params, id: requestID) }
        try check("project.memo.get", ProjectMemoRequests.get(projectID: project))
        try check("project.memo.set", try ProjectMemoRequests.set(projectID: project, text: ""))
        XCTAssertThrowsError(try check("project.memo.get", [:]))
        XCTAssertThrowsError(try check("project.memo.get", ["project_id": .string("Main")]))
        XCTAssertThrowsError(try check("project.memo.get", ["project_id": .string(project), "text": .string("")]))
        XCTAssertThrowsError(try check("project.memo.set", ["project_id": .string(project)]))
        XCTAssertThrowsError(try check("project.memo.set", ["project_id": .string(project), "text": .number(1)]))
        XCTAssertThrowsError(try check("project.memo.set", ["project_id": .string(project), "text": .string(String(repeating: "x", count: ProjectMemoText.maximumBytes + 1))]))
        XCTAssertThrowsError(try check("project.memo.set", ["project_id": .string(project), "text": .string("x"), "updated_at": .number(1)]))
        XCTAssertEqual(RelayClient.timeout(for: "project.memo.set", default: .seconds(15)), .seconds(30))
        XCTAssertEqual(RelayClient.timeout(for: "project.memo.get", default: .seconds(15)), .seconds(15))
    }

    func testTheFeatureIsReadFromReady() {
        XCTAssertTrue(DesktopFeatures(ready: .object(["features": .object(["project_memo": .bool(true)])])).projectMemo)
        XCTAssertFalse(DesktopFeatures(ready: .object(["features": .object(["project_memo": .string("true")])])).projectMemo)
        XCTAssertFalse(DesktopFeatures(ready: .object(["features": .object([:])])).projectMemo)
    }

    func testErrorsSayWhetherTheTextIsMerelyNotThereYet() {
        XCTAssertEqual(ProjectMemoError.from(RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")), .unsupported)
        XCTAssertEqual(ProjectMemoError.from(RemoteError.timeout), .connection)
        XCTAssertEqual(ProjectMemoError.from(RemoteError.disconnected), .connection)
        XCTAssertEqual(ProjectMemoError.from(CancellationError()), .connection)
        guard case .notFound = ProjectMemoError.from(RemoteError.rpc(code: "not_found", message: "No project")) else { return XCTFail() }
        guard case .rejected = ProjectMemoError.from(RemoteError.rpc(code: "invalid_request", message: "too long")) else { return XCTFail() }
        XCTAssertTrue(ProjectMemoError.connection.isConnectionProblem)
        XCTAssertTrue(ProjectMemoError.notConnected.isConnectionProblem)
        XCTAssertFalse(ProjectMemoError.rejected("x").isConnectionProblem)
    }

    private actor Scripted: RemoteTransport {
        var replies: [Result<JSONValue, any Error>]
        var calls: [(method: String, params: [String: JSONValue])] = []
        init(_ replies: [Result<JSONValue, any Error>]) { self.replies = replies }
        func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { pairing }
        func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
            calls.append((method, params))
            return try replies.removeFirst().get()
        }
        func disconnect() async {}
        func isConnected() async -> Bool { true }
        func sent() -> [(method: String, params: [String: JSONValue])] { calls }
    }

    func testTheTransportHelpersSendOneRequestEach() async throws {
        let saved = JSONValue.object(["project_id": .string(project), "text": .string("a\nb"), "updated_at": .number(7)])
        let transport = Scripted([.success(saved), .success(saved), .failure(RemoteError.timeout)])
        let read = try await transport.readProjectMemo(projectID: project)
        XCTAssertEqual(read.text, "a\nb")
        let written = try await transport.saveProjectMemo(projectID: project, text: "a\r\nb")
        XCTAssertEqual(written.updatedAt, 7)
        do { _ = try await transport.saveProjectMemo(projectID: project, text: "x"); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? ProjectMemoError, .connection) }
        let sent = await transport.sent()
        XCTAssertEqual(sent.map(\.method), ["project.memo.get", "project.memo.set", "project.memo.set"])
        XCTAssertEqual(sent[1].params["text"], .string("a\nb"), "normalized before it leaves")
        // Too long never leaves the phone.
        do { _ = try await transport.saveProjectMemo(projectID: project, text: String(repeating: "x", count: ProjectMemoText.maximumBytes + 1)); XCTFail("should throw") }
        catch { guard case .tooLarge = error as? ProjectMemoError else { return XCTFail("\(error)") } }
        let after = await transport.sent()
        XCTAssertEqual(after.count, 3)
    }

    // MARK: The editor's saving

    private func memo(_ text: String, at: UInt64? = 1) -> ProjectMemo { ProjectMemo(projectID: project, text: text, updatedAt: at) }

    func testLoadEditSaveAndNothingClaimedBeforeTheMacConfirms() {
        var editor = ProjectMemoEditor(projectID: project, projectName: "Demo")
        XCTAssertEqual(editor.phase, .loading)
        XCTAssertFalse(editor.isEditable, "nothing is typed over a memo not yet read")
        editor.edit("ignored")
        XCTAssertEqual(editor.text, "")
        XCTAssertNil(editor.pendingSave)

        editor.loaded(memo("# Plan"))
        XCTAssertEqual(editor.phase, .saved)
        XCTAssertEqual(editor.text, "# Plan")
        XCTAssertEqual(editor.status, "Saved automatically")
        XCTAssertNil(editor.textToKeep)

        editor.edit("# Plan\n- ship")
        XCTAssertEqual(editor.phase, .unsaved)
        XCTAssertTrue(editor.status.hasPrefix("Not saved yet"))
        XCTAssertEqual(editor.textToKeep, "# Plan\n- ship")
        XCTAssertEqual(editor.pendingSave, "# Plan\n- ship")
        editor.beginSave()
        XCTAssertEqual(editor.phase, .saving)
        XCTAssertNil(editor.pendingSave, "one save at a time")
        editor.edit("# Plan\n- ship\n- test")
        XCTAssertEqual(editor.phase, .saving, "typing during a save waits for its answer")
        editor.saveSucceeded(memo("# Plan\n- ship", at: 2))
        XCTAssertEqual(editor.phase, .unsaved, "what was typed meanwhile is not claimed saved")
        XCTAssertEqual(editor.pendingSave, "# Plan\n- ship\n- test")
        editor.beginSave()
        editor.saveSucceeded(memo("# Plan\n- ship\n- test", at: 3))
        XCTAssertEqual(editor.phase, .saved)
        XCTAssertNil(editor.textToKeep)
        // Typing back to what the Mac has is saved, with nothing to send.
        editor.edit("x")
        editor.edit("# Plan\n- ship\n- test")
        XCTAssertEqual(editor.phase, .saved)
        XCTAssertNil(editor.pendingSave)
    }

    func testAnUnreachableMacQueuesAndARefusalFailsButTheTextStays() {
        var editor = ProjectMemoEditor(projectID: project, projectName: "Demo")
        editor.loaded(memo(""))
        editor.edit("offline notes")
        editor.beginSave()
        editor.saveFailed(.connection)
        XCTAssertEqual(editor.phase, .queued)
        XCTAssertTrue(editor.status.contains("kept on this iPhone"))
        XCTAssertTrue(editor.statusIsWarning)
        XCTAssertEqual(editor.textToKeep, "offline notes")
        XCTAssertEqual(editor.pendingSave, "offline notes", "saved again once connected")

        editor.beginSave()
        editor.saveFailed(.rejected("too long"))
        guard case .failed = editor.phase else { return XCTFail("\(editor.phase)") }
        XCTAssertTrue(editor.status.contains("Your text is kept on this iPhone"))
        XCTAssertEqual(editor.text, "offline notes")
        XCTAssertEqual(editor.pendingSave, "offline notes", "Retry sends it again")

        editor.beginSave()
        editor.saveFailed(.unsupported)
        XCTAssertEqual(editor.phase, .unsupported)
        XCTAssertEqual(editor.textToKeep, "offline notes")
    }

    func testTooLongIsSaidAndNeverSent() {
        var editor = ProjectMemoEditor(projectID: project, projectName: "Demo")
        editor.loaded(memo("kept"))
        editor.edit(String(repeating: "x", count: ProjectMemoText.maximumBytes + 1))
        guard case .tooLarge(let message) = editor.phase else { return XCTFail("\(editor.phase)") }
        XCTAssertTrue(message.contains("32768"))
        XCTAssertNil(editor.pendingSave)
        XCTAssertNotNil(editor.textToKeep, "kept on the phone all the same")
        editor.edit("short")
        XCTAssertEqual(editor.pendingSave, "short")
    }

    func testTextKeptFromBeforeSurvivesTheReadAndSavesLastWins() {
        var editor = ProjectMemoEditor(projectID: project, projectName: "Demo", restoredText: "from the train", restoredBase: 1)
        XCTAssertTrue(editor.isEditable)
        XCTAssertNil(editor.pendingSave, "a read comes first")
        editor.loadFailed(.connection)
        XCTAssertEqual(editor.phase, .queued)
        XCTAssertTrue(editor.canRetry)
        // Typing before a read claims nothing: it still waits for the Mac.
        editor.edit("from the train, more")
        XCTAssertEqual(editor.phase, .queued)
        XCTAssertNil(editor.pendingSave)
        // The Mac's memo is the one the text was written over: the kept text saves, the last save wins.
        editor.loaded(memo("from the Mac", at: 1))
        XCTAssertEqual(editor.text, "from the train, more")
        XCTAssertEqual(editor.phase, .unsaved)
        XCTAssertEqual(editor.pendingSave, "from the train, more")

        // Kept text that the Mac already has is simply saved.
        var same = ProjectMemoEditor(projectID: project, projectName: "Demo", restoredText: "same")
        same.loaded(memo("same"))
        XCTAssertEqual(same.phase, .saved)
        XCTAssertNil(same.textToKeep)
    }

    func testKeptTextOverAMemoChangedOnTheMacWaitsForAChoice() {
        var mine = ProjectMemoEditor(projectID: project, projectName: "Demo", restoredText: "old phone text", restoredBase: 1)
        mine.loaded(memo("newer Mac text", at: 5))
        XCTAssertEqual(mine.phase, .conflict)
        XCTAssertEqual(mine.text, "old phone text", "nothing is lost")
        XCTAssertNil(mine.pendingSave, "nothing is sent without a choice")
        XCTAssertTrue(mine.statusIsWarning)
        XCTAssertEqual(mine.textToKeep, "old phone text")
        mine.edit("old phone text, edited")
        XCTAssertEqual(mine.phase, .conflict)
        mine.keepMine()
        XCTAssertEqual(mine.pendingSave, "old phone text, edited")

        var theirs = ProjectMemoEditor(projectID: project, projectName: "Demo", restoredText: "old phone text", restoredBase: 1)
        theirs.loaded(memo("newer Mac text", at: 5))
        theirs.useMacVersion()
        XCTAssertEqual(theirs.text, "newer Mac text")
        XCTAssertEqual(theirs.phase, .saved)
        XCTAssertNil(theirs.textToKeep)

        // Each editor is its own session.
        XCTAssertNotEqual(mine.session, theirs.session)
    }

    func testAnUnresolvedConflictKeepsTheBaseItsTextWasWrittenOver() {
        var editor = ProjectMemoEditor(projectID: project, projectName: "Demo", restoredText: "old phone text", restoredBase: 1)
        editor.loaded(memo("newer Mac text", at: 5))
        XCTAssertEqual(editor.phase, .conflict)
        XCTAssertEqual(editor.base, 1, "kept with the memo it was written over, not the Mac's newer one")
        // Closed without a choice and opened again: it still asks, and sends nothing by itself.
        var reopened = ProjectMemoEditor(projectID: project, projectName: "Demo", restoredText: editor.textToKeep, restoredBase: editor.base)
        reopened.loaded(memo("newer Mac text", at: 5))
        XCTAssertEqual(reopened.phase, .conflict)
        XCTAssertNil(reopened.pendingSave)
        // Keeping it is writing it over the Mac's memo.
        reopened.keepMine()
        XCTAssertEqual(reopened.base, 5)
        XCTAssertEqual(reopened.pendingSave, "old phone text")
    }

    func testGoingBackDuringASaveKeepsTheTextUntilTheMacHasIt() {
        var editor = ProjectMemoEditor(projectID: project, projectName: "Demo")
        editor.loaded(memo("A", at: 1))
        editor.edit("B")
        editor.beginSave()
        editor.edit("A")
        // The Mac may be about to hold B: A is the text to keep until the Mac has it again.
        XCTAssertEqual(editor.textToKeep, "A")
        XCTAssertEqual(editor.base, 1)
        editor.saveSucceeded(memo("B", at: 2))
        XCTAssertEqual(editor.phase, .unsaved)
        XCTAssertEqual(editor.pendingSave, "A")
        editor.beginSave()
        editor.saveSucceeded(memo("A", at: 3))
        XCTAssertEqual(editor.phase, .saved)
        XCTAssertNil(editor.textToKeep)

        // A save that may or may not have arrived: A is saved again once it can be.
        var lost = ProjectMemoEditor(projectID: project, projectName: "Demo")
        lost.loaded(memo("A", at: 1))
        lost.edit("B")
        lost.beginSave()
        lost.edit("A")
        lost.saveFailed(.connection)
        XCTAssertEqual(lost.phase, .queued)
        XCTAssertEqual(lost.textToKeep, "A")
        XCTAssertEqual(lost.pendingSave, "A")
        lost.loaded(memo("B", at: 2))
        XCTAssertEqual(lost.text, "A", "the Mac's B is not taken over the text typed after it")
        XCTAssertEqual(lost.pendingSave, "A")
    }

    func testAnEarlierEditorsSaveThatLandsLaterIsBuiltOnNotAConflict() {
        // Reopened with text kept over the memo of 1 while the earlier editor's save was on its way.
        var waiting = ProjectMemoEditor(projectID: project, projectName: "Demo", restoredText: "typed during it", restoredBase: 1)
        waiting.earlierSaveLanded(memo("the earlier save", at: 2))
        XCTAssertEqual(waiting.base, 2)
        waiting.loaded(memo("the earlier save", at: 2))
        XCTAssertEqual(waiting.phase, .unsaved)
        XCTAssertEqual(waiting.pendingSave, "typed during it")

        // Read after it landed, before its answer came: not a conflict once it does.
        var read = ProjectMemoEditor(projectID: project, projectName: "Demo", restoredText: "typed during it", restoredBase: 1)
        read.loaded(memo("the earlier save", at: 2))
        XCTAssertEqual(read.phase, .conflict)
        read.earlierSaveLanded(memo("the earlier save", at: 2))
        XCTAssertEqual(read.phase, .unsaved)
        XCTAssertEqual(read.pendingSave, "typed during it")
        // A Mac edit after it still asks.
        var later = ProjectMemoEditor(projectID: project, projectName: "Demo", restoredText: "typed during it", restoredBase: 1)
        later.loaded(memo("a Mac edit after it", at: 3))
        later.earlierSaveLanded(memo("the earlier save", at: 2))
        XCTAssertEqual(later.phase, .conflict)
        XCTAssertNil(later.pendingSave)

        // Read before it landed, showing the memo it replaced: the text on screen is saved over it.
        var before = ProjectMemoEditor(projectID: project, projectName: "Demo", restoredText: "A", restoredBase: 1)
        before.loaded(memo("A", at: 1))
        XCTAssertEqual(before.phase, .saved)
        before.earlierSaveLanded(memo("B", at: 2))
        XCTAssertEqual(before.text, "A")
        XCTAssertEqual(before.pendingSave, "A")
        before.earlierSaveLanded(memo("older", at: 1))
        XCTAssertEqual(before.saved?.updatedAt, 2, "an older answer changes nothing")
    }

    func testANewerMemoReplacesOnlyAnEditorWithNothingUnsaved() {
        var editor = ProjectMemoEditor(projectID: project, projectName: "Demo")
        editor.loaded(memo("v1"))
        editor.loaded(memo("v2 from the Mac", at: 2))
        XCTAssertEqual(editor.text, "v2 from the Mac")
        editor.edit("mine")
        editor.loaded(memo("v3 from the Mac", at: 3))
        XCTAssertEqual(editor.text, "mine")
        XCTAssertEqual(editor.phase, .unsaved)
    }

    func testAReadThatFailsSaysSoAndAnOldMacIsNamed() {
        var editor = ProjectMemoEditor(projectID: project, projectName: "Demo")
        editor.loadFailed(.notFound("No project matches it"))
        guard case .loadFailed = editor.phase else { return XCTFail() }
        XCTAssertFalse(editor.isEditable)
        var old = ProjectMemoEditor(projectID: project, projectName: "Demo")
        old.loadFailed(.unsupported)
        XCTAssertEqual(old.phase, .unsupported)
        XCTAssertEqual(old.status, ProjectMemoError.unsupportedMessage)
    }

    // MARK: Kept text

    @MainActor func testKeptTextIsPerDesktopAndProjectAndBounded() {
        let defaults = UserDefaults(suiteName: "memo-\(UUID().uuidString)")!
        let store = ProjectMemoDraftStore(defaults: defaults)
        store.keep("a", desktop: "d1", project: "p1")
        store.keep("b", desktop: "d2", project: "p1")
        XCTAssertEqual(store.text(desktop: "d1", project: "p1"), "a")
        XCTAssertEqual(store.text(desktop: "d2", project: "p1"), "b")
        XCTAssertNil(store.text(desktop: "d1", project: "p2"))
        store.keep(nil, desktop: "d1", project: "p1")
        XCTAssertNil(store.text(desktop: "d1", project: "p1"))
        XCTAssertEqual(ProjectMemoDraftStore(defaults: defaults).text(desktop: "d2", project: "p1"), "b", "survives a relaunch")
        store.keep("c", base: 7, desktop: "d2", project: "p2")
        XCTAssertEqual(store.keptProjects(desktop: "d2"), ["p1", "p2"])
        XCTAssertEqual(store.entry(desktop: "d2", project: "p2")?.base, 7)
        XCTAssertNil(store.entry(desktop: "d2", project: "p1")?.base)
        XCTAssertTrue(store.keptProjects(desktop: "d1").isEmpty)
        store.keep(nil, desktop: "d2", project: "p2")
        let start = Date(timeIntervalSince1970: 0)
        for index in 0..<(ProjectMemoDraftStore.maximumDrafts + 2) {
            store.keep("t\(index)", desktop: "d", project: "p\(index)", now: start.addingTimeInterval(Double(index)))
        }
        XCTAssertNil(store.text(desktop: "d", project: "p0"), "the oldest go first")
        XCTAssertEqual(store.text(desktop: "d", project: "p\(ProjectMemoDraftStore.maximumDrafts + 1)"), "t\(ProjectMemoDraftStore.maximumDrafts + 1)")
    }

    // MARK: `/memo`

    func testThePreviewOffersInsertOnlyForAMemoWithText() {
        XCTAssertNil(ProjectMemoPreview.projectID(of: nil))
        XCTAssertNil(ProjectMemoPreview.projectID(of: ChatInfo(id: "c", provider: .codex)))
        XCTAssertEqual(ProjectMemoPreview.projectID(of: ChatInfo(id: "c", provider: .codex, projectID: project)), project)

        XCTAssertFalse(ProjectMemoPreview.noProject.canInsert)
        XCTAssertFalse(ProjectMemoPreview.noProject.canEdit)
        XCTAssertTrue(ProjectMemoPreview.noProject.message!.contains("no project memo here"))
        XCTAssertFalse(ProjectMemoPreview.memo(memo("  \n")).canInsert)
        XCTAssertTrue(ProjectMemoPreview.memo(memo("  \n")).canEdit)
        XCTAssertTrue(ProjectMemoPreview.memo(memo("")).message!.contains("empty"))
        XCTAssertTrue(ProjectMemoPreview.memo(memo("# Notes")).canInsert)
        XCTAssertNil(ProjectMemoPreview.memo(memo("# Notes")).message)
        XCTAssertFalse(ProjectMemoPreview.unsupported.canEdit)
        XCTAssertTrue(ProjectMemoPreview.failed(projectID: project, message: "x").canEdit)
        XCTAssertFalse(ProjectMemoPreview.loading(projectID: project).canInsert)
    }
}
