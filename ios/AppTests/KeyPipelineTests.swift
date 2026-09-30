import XCTest
import RiWorkCore
@testable import RiWorkRemote

/// The direct-typing send pipeline, screen refresh, layout and focus mode, against the fake desktop transport.
@MainActor final class KeyPipelineTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let shell = "44444444-4444-4444-8444-444444444444"
    private let other = "55555555-5555-4555-8555-555555555555"
    private var awake: [Bool] = []

    private func makeStore() throws -> KeychainStore {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = shell
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        return keychain
    }
    private func scratchDefaults() -> UserDefaults {
        let name = "com.riwork.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }
    private func makeModel(_ transport: FixtureTransport, _ keychain: KeychainStore, defaults: UserDefaults? = nil, poll: Duration = .seconds(3),
                           flush: Duration = .milliseconds(5), preview: Duration = .milliseconds(80), backoff: Duration = .milliseconds(10)) -> RemoteModel {
        RemoteModel(client: transport, keychain: keychain, pollInterval: poll, keyFlushInterval: flush, previewDelay: preview, reconnectBackoff: backoff,
                    defaults: defaults ?? scratchDefaults(), cellMetrics: { TerminalLayout.approximateCell(fontSize: $0) }, keepAwake: { [weak self] in self?.awake.append($0) })
    }
    private func session(_ id: String) throws -> RemoteSession {
        try JSONDecoder().decode(RemoteSession.self, from: Data("""
        {"id":"\(id)","project_id":"\(project)","kind":"project","cwd":"/fixture","alive":true,"created_at_unix":2}
        """.utf8))
    }
    private func eventually(_ what: String, timeout: Double = 3, _ condition: () async -> Bool) async {
        let end = Date().addingTimeInterval(timeout)
        while await !condition(), Date() < end { try? await Task.sleep(for: .milliseconds(5)) }
        let met = await condition()
        XCTAssertTrue(met, what)
    }
    @discardableResult
    private func typeText(_ model: RemoteModel, _ text: String) -> KeyAcceptance { model.type(KeyMapper.items(for: text)) }
    private func connected(_ transport: FixtureTransport = FixtureTransport(), configure: (FixtureTransport) async -> Void = { _ in },
                           flush: Duration = .milliseconds(5), preview: Duration = .milliseconds(80), poll: Duration = .seconds(3), defaults: UserDefaults? = nil) async throws -> (RemoteModel, FixtureTransport, KeychainStore) {
        let keychain = try makeStore()
        await configure(transport)
        let model = makeModel(transport, keychain, defaults: defaults, poll: poll, flush: flush, preview: preview)
        await model.connect()
        XCTAssertEqual(model.state, .connected)
        return (model, transport, keychain)
    }

    // MARK: sending

    func testTypingGoesStraightToTheShellWithoutShowingAPreview() async throws {
        let (model, transport, keychain) = try await connected(); defer { try? keychain.delete() }
        XCTAssertEqual(typeText(model, "l"), .accepted)
        await eventually("the key reached the desktop") { await transport.calls().count == 1 }
        await eventually("buffer drained") { model.keyBuffers.isEmpty }
        let firstCall = await transport.calls().first
        let call = try XCTUnwrap(firstCall)
        XCTAssertEqual(call.shell, shell)
        XCTAssertNotNil(UUID(uuidString: call.batch))
        XCTAssertEqual(call.items, [.object(["text": .string("l")])])
        XCTAssertEqual(model.keysSupport, .supported)
        XCTAssertNil(model.keyPreview, "a shell that answers at once never shows the chip")
        await model.disconnect()
    }
    func testKeysTypedWhileABatchIsInFlightRideTogetherInTheNextBatch() async throws {
        let (model, transport, keychain) = try await connected(configure: { await $0.setKeysDelay(.milliseconds(120)) }); defer { try? keychain.delete() }
        typeText(model, "a")
        await eventually("first batch in flight") { await transport.calls().count == 1 }
        typeText(model, "b"); typeText(model, "c"); typeText(model, "\n")
        await eventually("second batch delivered") { await transport.calls().count == 2 }
        let calls = await transport.calls()
        XCTAssertEqual(calls[1].items, [.object(["text": .string("bc")]), .object(["key": .string("Enter")])], "adjacent text is coalesced, keys stay separate")
        await eventually("drained") { model.keyBuffers.isEmpty }
        let delivered = await transport.delivered(shell: shell)
        XCTAssertEqual(delivered, "abc⏎")
        let peak = await transport.peakInFlight("shell.keys")
        XCTAssertEqual(peak, 1)
        await model.disconnect()
    }
    func testExactlyOneBatchIsInFlightWhileTypingContinues() async throws {
        let (model, transport, keychain) = try await connected(configure: { await $0.setKeysDelay(.milliseconds(30)) }); defer { try? keychain.delete() }
        let expected = (0..<30).map { String($0 % 10) }.joined()
        for character in expected { typeText(model, String(character)); try? await Task.sleep(for: .milliseconds(3)) }
        await eventually("all delivered") { await transport.delivered(shell: self.shell) == expected }
        let peak = await transport.peakInFlight("shell.keys")
        let batches = await transport.calls().count
        XCTAssertEqual(peak, 1)
        XCTAssertLessThan(batches, 30, "keys typed while a batch is out are coalesced")
        await model.disconnect()
    }
    func testBatchesStayWithinTheWireLimitsAndKeepOrder() async throws {
        let (model, transport, keychain) = try await connected(); defer { try? keychain.delete() }
        await model.disconnect()
        var items: [KeyItem] = []
        for _ in 0..<200 { items += [.text("x"), .key(.enter)] }
        XCTAssertEqual(model.type(items), .accepted)
        XCTAssertEqual(model.pendingKeyBuffer?.itemCount, 400)
        await model.connect()
        await eventually("all sent") { await transport.delivered(shell: self.shell).count == 400 }
        for call in await transport.calls() { XCTAssertLessThanOrEqual(call.items.count, 64) }
        let delivered = await transport.delivered(shell: shell)
        XCTAssertEqual(delivered, String(repeating: "x⏎", count: 200))
        await model.disconnect()
    }

    func testAPastedScriptGoesOutAsTextPlusEnterBatchesInOrder() async throws {
        let (model, transport, keychain) = try await connected(); defer { try? keychain.delete() }
        await model.disconnect()
        typeText(model, "ls\ngit status\n")
        await model.connect()
        await eventually("both lines sent") { await transport.calls().count == 2 }
        let calls = await transport.calls()
        XCTAssertEqual(calls[0].items, [.object(["text": .string("ls")]), .object(["key": .string("Enter")])], "a batch ends at Enter: one pause on the desktop")
        XCTAssertEqual(calls[1].items, [.object(["text": .string("git status")]), .object(["key": .string("Enter")])])
        let peak = await transport.peakInFlight("shell.keys")
        XCTAssertEqual(peak, 1)
        await model.disconnect()
    }

    // MARK: retry semantics

    func testLostConnectionRetriesTheSameBatchWithANewRequestIDAfterReconnect() async throws {
        let (model, transport, keychain) = try await connected(configure: { await $0.setKeysMode(.dropLinkAfterDelivery) }); defer { try? keychain.delete() }
        typeText(model, "ls")
        await eventually("first attempt seen") { await transport.calls().count >= 1 }
        typeText(model, "\n")
        await eventually("retry and follow-up delivered") { await transport.calls().count >= 3 }
        let calls = await transport.calls()
        XCTAssertEqual(calls[0].batch, calls[1].batch, "a retry reuses the batch UUID")
        XCTAssertNotEqual(calls[0].request, calls[1].request, "and gets a new request id")
        XCTAssertNotEqual(calls[2].batch, calls[0].batch, "keys typed meanwhile form a new batch behind it")
        XCTAssertEqual(calls[1].items, calls[0].items)
        await eventually("drained") { model.keyBuffers.isEmpty }
        let delivered = await transport.delivered(shell: shell)
        XCTAssertEqual(delivered, "ls⏎", "delivered once, in order")
        XCTAssertEqual(model.state, .connected)
        XCTAssertNil(model.deliveryNotice, "a duplicate answer is a plain success")
        let connections = await transport.counts().0
        XCTAssertEqual(connections, 2)
        await model.disconnect()
    }
    func testSilentlyDeadLinkIsReconnectedAndBufferedKeysGoOutOnce() async throws {
        let (model, transport, keychain) = try await connected(); defer { try? keychain.delete() }
        await transport.setConnected(false)
        typeText(model, "hi")
        await eventually("reconnected and delivered") { await transport.delivered(shell: self.shell) == "hi" }
        await eventually("connected again") { model.state == .connected }
        let connections = await transport.counts().0
        XCTAssertEqual(connections, 2)
        XCTAssertTrue(model.keyBuffers.isEmpty)
        await model.disconnect()
    }
    func testManualDisconnectDoesNotReconnectByItself() async throws {
        let (model, transport, keychain) = try await connected(); defer { try? keychain.delete() }
        await model.disconnect()
        XCTAssertEqual(typeText(model, "x"), .accepted, "typing offline buffers")
        try? await Task.sleep(for: .milliseconds(150))
        let connections = await transport.counts().0
        XCTAssertEqual(connections, 1)
        XCTAssertEqual(model.state, .disconnected)
        XCTAssertEqual(model.pendingKeyBuffer?.plainText, "x")
    }
    func testUncertainIsANonBlockingNoteAndTypingContinues() async throws {
        let (model, transport, keychain) = try await connected(configure: { await $0.setKeysMode(.uncertain) }); defer { try? keychain.delete() }
        typeText(model, "x")
        await eventually("note shown") { model.deliveryNotice == "Some input may not have arrived." }
        await eventually("drained") { model.keyBuffers.isEmpty }
        XCTAssertEqual(model.state, .connected)
        await transport.setKeysMode(.ok)
        typeText(model, "y")
        await eventually("typing continues") { await transport.delivered(shell: self.shell) == "y" }
        await model.disconnect()
    }
    func testInputUnavailableStopsKeepsTheBufferAndShowsWhy() async throws {
        let (model, transport, keychain) = try await connected(configure: { await $0.setKeysMode(.inputUnavailable) }); defer { try? keychain.delete() }
        typeText(model, "abc")
        await eventually("blocked chip") { model.keyPreview?.tone == .blocked }
        XCTAssertEqual(model.keyPreview?.label, "Input is disabled for this terminal.")
        XCTAssertEqual(model.keyPreview?.text, "abc")
        try? await Task.sleep(for: .milliseconds(150))
        let attempts = await transport.calls()
        XCTAssertEqual(attempts.count, 1, "no retry loop against a refusing desktop")
        // Once the desktop accepts input again, a later keystroke retries the same batch first, then the new key.
        await transport.setKeysMode(.ok)
        try? await Task.sleep(for: .milliseconds(1100))
        typeText(model, "d")
        await eventually("buffer delivered in order") { await transport.delivered(shell: self.shell) == "abcd" }
        let calls = await transport.calls()
        XCTAssertEqual(calls[1].batch, calls[0].batch, "the refused batch is retried unchanged")
        await model.disconnect()
    }
    func testNotFoundKeepsTheBufferAndSaysTheTerminalIsGone() async throws {
        let (model, _, keychain) = try await connected(configure: { await $0.setKeysMode(.notFound) }); defer { try? keychain.delete() }
        typeText(model, "q")
        await eventually("blocked") { model.keyPreview?.tone == .blocked }
        XCTAssertEqual(model.keyPreview?.label, "This terminal is no longer available.")
        XCTAssertEqual(model.pendingKeyBuffer?.plainText, "q")
        await model.disconnect()
    }

    func testLeavingTheAppLetsQueuedKeysReachTheDesktopFirst() async throws {
        let (model, transport, keychain) = try await connected(configure: { await $0.setKeysDelay(.milliseconds(60)) }); defer { try? keychain.delete() }
        typeText(model, "exit\n")
        await model.disconnect(background: true)
        let delivered = await transport.delivered(shell: shell)
        XCTAssertEqual(delivered, "exit⏎", "flushed before the connection was paused")
        XCTAssertEqual(model.state, .suspended)
        XCTAssertTrue(model.keyBuffers.isEmpty)
    }
    func testTypedItemsAreSanitisedBeforeTheyCanPoisonABatch() async throws {
        let (model, transport, keychain) = try await connected(); defer { try? keychain.delete() }
        await model.disconnect()
        XCTAssertEqual(model.type([.text("a\u{7}b\nc"), .text(""), .key(.control("É"))]), .accepted)
        XCTAssertEqual(model.pendingKeyBuffer?.items, [.text("ab"), .key(.enter), .text("c")])
        await model.connect()
        await eventually("delivered") { await transport.delivered(shell: self.shell) == "ab⏎c" }
        await model.disconnect()
    }

    func testReconnectBackoffGrowsThenStaysCapped() {
        let delays = (0..<6).map { RemoteModel.reconnectDelay(base: .seconds(1), attempt: $0) }
        XCTAssertEqual(delays, [.seconds(1), .seconds(2), .seconds(4), .seconds(8), .seconds(15), .seconds(15)])
        for attempt in [62, 63, 64, 65, 1000, Int.max] { XCTAssertEqual(RemoteModel.reconnectDelay(base: .seconds(1), attempt: attempt), .seconds(15), "attempt \(attempt) must not wrap to zero") }
        XCTAssertEqual(RemoteModel.reconnectDelay(base: .seconds(1), attempt: -3), .seconds(1))
    }
    func testComingBackDuringTheBackgroundDrainStillReconnects() async throws {
        let (model, transport, keychain) = try await connected(configure: { await $0.setKeysDelay(.milliseconds(300)) }); defer { try? keychain.delete() }
        typeText(model, "abc")
        await eventually("batch in flight") { await transport.calls().count == 1 }
        let leaving = Task { await model.disconnect(background: true) }
        try? await Task.sleep(for: .milliseconds(30))
        await model.resume()   // the app is active again while the drain is still running
        await leaving.value
        XCTAssertEqual(model.state, .connected, "resumed after the drain instead of staying suspended")
        let connections = await transport.counts().0
        XCTAssertEqual(connections, 2)
        let delivered = await transport.delivered(shell: shell)
        XCTAssertEqual(delivered, "abc")
        await model.disconnect()
    }
    func testAClosedTerminalSwitchesSelectionWithoutSendingItsKeysToTheReplacement() async throws {
        let transport = FixtureTransport()
        await transport.setSessions([try session(shell), try session(other)])
        let (model, _, keychain) = try await connected(transport); defer { try? keychain.delete() }
        XCTAssertEqual(model.sessionAutoSwitches, 0)
        await model.disconnect()
        typeText(model, "abc")
        await transport.setSessions([try session(other)])
        await model.connect()
        XCTAssertEqual(model.sessionID, other)
        XCTAssertEqual(model.sessionAutoSwitches, 1, "the view drops the keyboard on this")
        await eventually("old buffer was offered to its own shell only") { await transport.calls().count >= 1 }
        for call in await transport.calls() { XCTAssertEqual(call.shell, shell) }
        let toReplacement = await transport.delivered(shell: other)
        XCTAssertEqual(toReplacement, "")
        await model.disconnect()
    }

    // MARK: buffer and preview

    func testBufferCapsRejectFurtherKeystrokesWithABufferFullHint() async throws {
        let (model, _, keychain) = try await connected(); defer { try? keychain.delete() }
        await model.disconnect()
        XCTAssertEqual(typeText(model, String(repeating: "a", count: 4096)), .accepted)
        XCTAssertEqual(typeText(model, "b"), .bufferFull)
        XCTAssertEqual(model.pendingKeyBuffer?.characterCount, 4096, "a refused keystroke changes nothing")
        XCTAssertEqual(model.keyPreview?.tone, .full)
        XCTAssertEqual(model.keyPreview?.label, "Buffer full · Offline")
        model.discardPendingKeys()
        XCTAssertNil(model.keyPreview)
        for _ in 0..<512 { XCTAssertEqual(model.type([.key(.enter)]), .accepted) }
        XCTAssertEqual(model.type([.key(.enter)]), .bufferFull, "512 items is the other cap")
    }
    func testOfflinePreviewUsesKeySymbolsAndDrainsOnReconnect() async throws {
        let (model, transport, keychain) = try await connected(); defer { try? keychain.delete() }
        await model.disconnect()
        typeText(model, "git status\n")
        model.type([.key(.control("c")), .key(.backspace), .key(.escape), .key(.tab), .key(.up)])
        let chip = try XCTUnwrap(model.keyPreview, "shown at once when the connection is down")
        XCTAssertEqual(chip.text, "git status⏎^C⌫⎋⇥↑")
        XCTAssertEqual(chip.label, "Offline — will send when reconnected")
        XCTAssertEqual(chip.tone, .offline)
        await model.connect()
        await eventually("delivered") { await transport.delivered(shell: self.shell) == "git status⏎^C⌫⎋⇥↑" }
        await eventually("chip gone once drained") { model.keyPreview == nil }
        await model.disconnect()
    }
    func testPreviewOnlyAppearsForInputOlderThanTheDelayAndDisappearsWhenDrained() async throws {
        let (model, _, keychain) = try await connected(configure: { await $0.setKeysDelay(.milliseconds(500)) }, preview: .milliseconds(80)); defer { try? keychain.delete() }
        typeText(model, "a")
        XCTAssertNil(model.keyPreview)
        await eventually("chip appears after the delay") { model.keyPreview?.tone == .sending }
        XCTAssertEqual(model.keyPreview?.label, "Sending…")
        await eventually("chip disappears when the queue drains", timeout: 4) { model.keyPreview == nil && model.keyBuffers.isEmpty }
        await model.disconnect()
    }

    // MARK: shells

    func testSwitchingShellsKeepsEachBufferWithItsOwnShell() async throws {
        let transport = FixtureTransport()
        await transport.setSessions([try session(shell), try session(other)])
        let (model, _, keychain) = try await connected(transport); defer { try? keychain.delete() }
        await model.disconnect()
        typeText(model, "aaa")
        await model.chooseSession(try session(other))
        XCTAssertEqual(model.sessionID, other)
        XCTAssertNil(model.keyPreview, "the other shell has nothing pending")
        typeText(model, "bb")
        XCTAssertEqual(model.keyPreview?.text, "bb")
        await model.chooseSession(try session(shell))
        XCTAssertEqual(model.keyPreview?.text, "aaa", "returning shows the shell's own buffer")
        await model.connect()
        await eventually("both delivered") { let a = await transport.delivered(shell: self.shell), b = await transport.delivered(shell: self.other); return a == "aaa" && b == "bb" }
        for call in await transport.calls() {
            let text = call.items.compactMap { $0["text"].string }.joined()
            XCTAssertEqual(call.shell, text.hasPrefix("a") ? shell : other, "keys never go to another shell")
        }
        await model.disconnect()
    }
    func testKeysTypedJustBeforeSwitchingAreStillSentToTheOriginalShell() async throws {
        let transport = FixtureTransport()
        await transport.setSessions([try session(shell), try session(other)])
        let (model, _, keychain) = try await connected(transport, configure: { await $0.setKeysDelay(.milliseconds(60)) }); defer { try? keychain.delete() }
        typeText(model, "x")
        await model.chooseSession(try session(other))
        typeText(model, "y")
        await eventually("each reached its own shell") { let a = await transport.delivered(shell: self.shell), b = await transport.delivered(shell: self.other); return a == "x" && b == "y" }
        await model.disconnect()
    }

    // MARK: fallback

    func testOlderDesktopFallsBackToTheLineComposerAndKeepsWhatWasTyped() async throws {
        let (model, transport, keychain) = try await connected(configure: { await $0.setKeysMode(.unsupported) }); defer { try? keychain.delete() }
        XCTAssertTrue(model.directTyping)
        typeText(model, "ec")
        await eventually("detected") { model.keysSupport == .unsupported }
        XCTAssertFalse(model.directTyping, "the composer takes over")
        XCTAssertEqual(model.draft, "ec", "what was typed is not lost")
        XCTAssertTrue(model.keyBuffers.isEmpty)
        XCTAssertEqual(typeText(model, "ho"), .unavailable)
        XCTAssertNotNil(model.deliveryNotice)
        let attempts = await transport.calls().count
        XCTAssertEqual(attempts, 1)
        // The composer still works, unchanged.
        model.draft = "echo hi"
        await model.readOutput()
        await model.submit()
        let lines = await transport.lines()
        XCTAssertEqual(lines, ["echo hi"])
        // A new connection detects again.
        await transport.setKeysMode(.ok)
        await model.connect()
        XCTAssertEqual(model.keysSupport, .unknown)
        XCTAssertTrue(model.directTyping)
        await model.disconnect()
    }
    func testUserCanChooseTheLineComposerAndTheChoiceIsRemembered() async throws {
        let defaults = scratchDefaults()
        let (model, _, keychain) = try await connected(defaults: defaults); defer { try? keychain.delete() }
        XCTAssertTrue(model.directTyping)
        model.setPreferLineComposer(true)
        XCTAssertFalse(model.directTyping)
        XCTAssertEqual(typeText(model, "x"), .unavailable)
        let restored = makeModel(FixtureTransport(), keychain, defaults: defaults)
        XCTAssertTrue(restored.preferLineComposer)
        await model.disconnect()
    }

    // MARK: screen refresh

    func testPollDelayFollowsKeyActivity() async throws {
        let (model, _, keychain) = try await connected(); defer { try? keychain.delete() }
        XCTAssertEqual(model.pollDelay(), .seconds(3), "resting interval before any typing")
        typeText(model, "a")
        let now = Date()
        XCTAssertEqual(model.pollDelay(now: now), .milliseconds(300))
        XCTAssertEqual(model.pollDelay(now: now.addingTimeInterval(1.9)), .milliseconds(300))
        XCTAssertEqual(model.pollDelay(now: now.addingTimeInterval(2.5)), .seconds(1))
        XCTAssertEqual(model.pollDelay(now: now.addingTimeInterval(11.5)), .seconds(1))
        XCTAssertEqual(model.pollDelay(now: now.addingTimeInterval(12.5)), .seconds(3))
        await model.disconnect()
    }
    func testTypingCutsAnIdleWaitShortSoTheEchoShowsUpQuickly() async throws {
        let (model, transport, keychain) = try await connected(poll: .seconds(30)); defer { try? keychain.delete() }
        try? await Task.sleep(for: .milliseconds(200))
        let before = await transport.operations().filter { $0.hasPrefix("shell.output") }.count
        typeText(model, "a")
        await eventually("a poll follows within a fraction of a second", timeout: 2) { await transport.operations().filter { $0.hasPrefix("shell.output") }.count > before }
        await model.disconnect()
    }
    func testOnlyOneScreenReadIsEverInFlight() async throws {
        let (model, transport, keychain) = try await connected(); defer { try? keychain.delete() }
        await transport.block("shell.output")
        let reads = (0..<3).map { _ in Task { await model.readOutput() } }
        await transport.waitUntilBlocked()
        try? await Task.sleep(for: .milliseconds(50))
        await transport.unblock()
        for read in reads { await read.value }
        let peak = await transport.peakInFlight("shell.output")
        XCTAssertEqual(peak, 1)
        await model.disconnect()
    }
    func testCursorAndCopyModeComeFromTheNewFieldsAndOldDesktopsShowNone() async throws {
        let transport = FixtureTransport()
        let (model, _, keychain) = try await connected(transport); defer { try? keychain.delete() }
        XCTAssertNil(model.outputCursorOffset, "an older desktop reports no cursor")
        XCTAssertFalse(model.outputInMode)
        await transport.setOutput("old1\nold2\n$ ls\n\n", extras: ["cursor": .object(["x": .number(4), "y": .number(1)]), "rows": .number(3), "cols": .number(40), "in_mode": .bool(true)])
        await model.readOutput()
        XCTAssertEqual(model.output, "old1\nold2\n$ ls ")
        XCTAssertEqual(model.outputCursorOffset, 14)
        XCTAssertEqual(Array(model.output)[14], " ", "the cursor cell sits after the typed text")
        XCTAssertTrue(model.outputInMode)
        await transport.setOutput("plain output")
        await model.readOutput()
        XCTAssertEqual(model.output, "plain output")
        XCTAssertNil(model.outputCursorOffset)
        XCTAssertFalse(model.outputInMode)
        await model.disconnect()
    }
    func testTypingCountsSoTheViewCanKeepTheScreenAtTheBottom() async throws {
        let (model, _, keychain) = try await connected(); defer { try? keychain.delete() }
        let before = model.typedCount
        typeText(model, "a"); typeText(model, "b")
        XCTAssertEqual(model.typedCount, before + 2)
        await model.disconnect()
    }

    // MARK: layout, font and focus

    func testFocusModeGivesTheShellALargerGridThanTheNormalLayout() async throws {
        let (model, _, keychain) = try await connected(); defer { try? keychain.delete() }
        model.reportTerminalArea(CGSize(width: 393, height: 560))
        let cell = TerminalLayout.approximateCell(fontSize: 12)
        XCTAssertEqual(model.terminalViewport, TerminalLayout.normal.viewport(width: 393, height: 560, cellWidth: cell.width, lineHeight: cell.height))
        let normal = try XCTUnwrap(model.terminalViewport)
        model.setFocusMode(true)
        XCTAssertTrue(model.focusMode)
        let focusSameArea = try XCTUnwrap(model.terminalViewport)
        XCTAssertGreaterThanOrEqual(focusSameArea.columns, normal.columns)
        XCTAssertGreaterThanOrEqual(focusSameArea.rows, normal.rows)
        // Chrome gone: the pane itself is taller, which the view reports after layout.
        model.reportTerminalArea(CGSize(width: 393, height: 700))
        let focus = try XCTUnwrap(model.terminalViewport)
        XCTAssertGreaterThan(focus.rows, normal.rows)
        model.setFocusMode(false)
        XCTAssertEqual(model.terminalLayout, .normal)
        await model.disconnect()
    }
    func testFontSizeChangesTheGridAndIsPersisted() async throws {
        let defaults = scratchDefaults()
        let (model, _, keychain) = try await connected(defaults: defaults); defer { try? keychain.delete() }
        XCTAssertEqual(model.terminalFontSize, TerminalFontSize.standard)
        model.reportTerminalArea(CGSize(width: 393, height: 600))
        let small = try XCTUnwrap(model.terminalViewport)
        model.setTerminalFontSize(16)
        let large = try XCTUnwrap(model.terminalViewport)
        XCTAssertLessThan(large.columns, small.columns)
        XCTAssertLessThan(large.rows, small.rows)
        model.stepTerminalFontSize(1)
        XCTAssertEqual(model.terminalFontSize, 17)
        model.setTerminalFontSize(400)
        XCTAssertEqual(model.terminalFontSize, TerminalFontSize.range.upperBound)
        model.setTerminalFontSize(16)
        let restored = makeModel(FixtureTransport(), keychain, defaults: defaults)
        XCTAssertEqual(restored.terminalFontSize, 16, "the size survives a restart")
        await model.disconnect()
    }
    func testGridChangesAreDebouncedIntoOneResizeRequest() async throws {
        let (model, transport, keychain) = try await connected(); defer { try? keychain.delete() }
        model.setTerminalVisible(true)
        model.reportTerminalArea(CGSize(width: 393, height: 560))
        await eventually("initial fit applied") { model.viewportReady && model.appliedViewport != nil }
        try? await Task.sleep(for: .milliseconds(300))
        let before = await transport.resizeRequests().count
        // Focus mode, two font steps and a bigger pane, all inside one debounce window.
        model.setFocusMode(true)
        model.stepTerminalFontSize(1)
        model.stepTerminalFontSize(1)
        model.reportTerminalArea(CGSize(width: 440, height: 800))
        let final = try XCTUnwrap(model.terminalViewport)
        await eventually("the final grid is applied") { model.appliedViewport == final }
        try? await Task.sleep(for: .milliseconds(300))
        let requests = await transport.resizeRequests()
        XCTAssertEqual(requests.count, before + 1, "one request for the whole burst")
        XCTAssertEqual(requests.last, final)
        await model.disconnect()
    }
    func testFocusModeIsRememberedPerSessionAndKeepsTheScreenAwakeOnlyWhileShown() async throws {
        let transport = FixtureTransport()
        await transport.setSessions([try session(shell), try session(other)])
        let (model, _, keychain) = try await connected(transport); defer { try? keychain.delete() }
        model.setTerminalVisible(true)
        model.setFocusMode(true)
        XCTAssertEqual(awake.last, true)
        await model.chooseSession(try session(other))
        XCTAssertFalse(model.focusMode, "another session has its own setting")
        model.updateKeepAwake()
        XCTAssertEqual(awake.last, false)
        await model.chooseSession(try session(shell))
        XCTAssertTrue(model.focusMode, "remembered for this app run")
        model.updateKeepAwake()
        XCTAssertEqual(awake.last, true)
        model.setTerminalVisible(false)
        XCTAssertEqual(awake.last, false, "leaving the terminal screen restores the idle timer")
        model.setTerminalVisible(true)
        model.setFocusMode(false)
        XCTAssertEqual(awake.last, false)
        await model.disconnect()
    }
}
