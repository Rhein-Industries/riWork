import XCTest
import SwiftUI
import RiWorkCore
@testable import RiWorkRemote

/// Live sync (a long poll that the desktop answers on change), the styled screen, the latency numbers and the display settings,
/// against the fake desktop transport.
/// The terminal screen under the same root the app has, which restyles it when the interface size changes.
private struct ThemedTerminal: View {
    @Bindable var model: RemoteModel
    let project: RemoteProject
    var body: some View { TerminalTabsView(model: model, project: project, onBack: {}).desktopThemed(model.theme.style) }
}

@MainActor final class LiveSyncTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let shell = "44444444-4444-4444-8444-444444444444"
    private let other = "55555555-5555-4555-8555-555555555555"
    private let esc = "\u{1B}"

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
    private func makeModel(_ transport: FixtureTransport, _ keychain: KeychainStore, defaults: UserDefaults? = nil, poll: Duration = .milliseconds(40), liveWait: Int = LiveSync.waitMilliseconds) -> RemoteModel {
        RemoteModel(client: transport, keychain: keychain, pollInterval: poll, keyFlushInterval: .milliseconds(5), previewDelay: .milliseconds(80), reconnectBackoff: .milliseconds(10),
                    defaults: defaults ?? scratchDefaults(), cellMetrics: { TerminalLayout.approximateCell(fontSize: $0) }, keepAwake: { _ in }, liveWaitMilliseconds: liveWait)
    }
    private func session(_ id: String) throws -> RemoteSession {
        try JSONDecoder().decode(RemoteSession.self, from: Data("""
        {"id":"\(id)","project_id":"\(project)","kind":"project","cwd":"/fixture","alive":true,"created_at_unix":2}
        """.utf8))
    }
    private func eventually(_ what: String, timeout: Double = 4, _ condition: () async -> Bool) async {
        let end = Date().addingTimeInterval(timeout)
        while await !condition(), Date() < end { try? await Task.sleep(for: .milliseconds(5)) }
        let met = await condition()
        XCTAssertTrue(met, what)
    }
    /// A connected model with the terminal on screen, against a desktop that can (or cannot) wait for changes.
    private func live(hash: Bool = true, cap: Duration = .milliseconds(120), transport: FixtureTransport = FixtureTransport(), defaults: UserDefaults? = nil, liveWait: Int = LiveSync.waitMilliseconds,
                      configure: (FixtureTransport) async -> Void = { _ in }) async throws -> (RemoteModel, FixtureTransport, KeychainStore) {
        let keychain = try makeStore()
        await transport.setHashMode(hash, cap: cap)
        await configure(transport)
        let model = makeModel(transport, keychain, defaults: defaults, liveWait: liveWait)
        model.setTerminalVisible(true)
        await model.connect()
        XCTAssertEqual(model.state, .connected)
        // Showing the terminal schedules one immediate read after a short debounce; let it pass so counts below are steady.
        try? await Task.sleep(for: .milliseconds(300))
        return (model, transport, keychain)
    }
    private func isLongPoll(_ params: [String: JSONValue]) -> Bool { params["if_changed"] != nil && params["wait_ms"] == .number(8000) }

    // MARK: the loop

    func testADesktopWithAHashIsFollowedByALongPoll() async throws {
        let (model, transport, keychain) = try await live(); defer { try? keychain.delete() }
        await eventually("live mode learned from the first hash") { model.syncMode == .live }
        await eventually("a long poll is waiting") { await transport.outputRequests().contains(where: self.isLongPoll) }
        let requests = await transport.outputRequests()
        XCTAssertNil(requests[0]["if_changed"], "the first read has no hash to ask from")
        XCTAssertEqual(requests[0]["styled"], .bool(true))
        let poll = try XCTUnwrap(requests.first(where: isLongPoll))
        let current = await transport.currentHash()
        XCTAssertEqual(poll["if_changed"]?.string, current)
        XCTAssertEqual(poll["wait_ms"], .number(8000))
        XCTAssertEqual(poll["styled"], .bool(true))
        XCTAssertEqual(poll["shell_id"]?.string, shell)
        await model.disconnect()
    }
    func testAnUnchangedAnswerIsReissuedAtOnceWithoutRedrawingAnything() async throws {
        let (model, transport, keychain) = try await live(cap: .milliseconds(30)); defer { try? keychain.delete() }
        await eventually("several unchanged rounds") { await transport.outputRequests().filter(self.isLongPoll).count >= 6 }
        let version = model.outputVersion
        let text = model.output
        let hashes = Set(await transport.outputRequests().filter(isLongPoll).compactMap { $0["if_changed"]?.string })
        XCTAssertEqual(hashes.count, 1, "an unchanged screen keeps asking from the same hash")
        try? await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(model.outputVersion, version, "unchanged answers do not touch the screen")
        XCTAssertEqual(model.output, text)
        XCTAssertFalse(model.snapshotStale)
        // "At once": rounds follow each other as fast as the desktop answers (30 ms each here), with no interval sleep between.
        let times = await transport.outputTimes()
        let gaps = zip(times.dropFirst(3), times.dropFirst(4)).map { ($1 - $0).timeInterval }
        XCTAssertLessThan(gaps.max() ?? 0, 0.12, "\(gaps)")
        await model.disconnect()
    }
    func testAChangedAnswerUpdatesTheScreenAndTheNextRequestAsksFromTheNewHash() async throws {
        let (model, transport, keychain) = try await live(cap: .seconds(3)); defer { try? keychain.delete() }
        await eventually("waiting") { await transport.outputRequests().contains(where: self.isLongPoll) }
        let before = model.outputVersion
        let oldHash = await transport.currentHash()
        await transport.setOutput("fresh output")
        await eventually("the pending long poll returned on the change, well before its wait ran out") { model.output == "fresh output" }
        XCTAssertEqual(model.outputVersion, before + 1)
        let newHash = await transport.currentHash()
        XCTAssertNotEqual(oldHash, newHash)
        await eventually("asking from the new hash") { await transport.outputRequests().last?["if_changed"]?.string == newHash }
        await model.disconnect()
    }
    func testExactlyOneOutputRequestIsEverInFlight() async throws {
        let (model, transport, keychain) = try await live(cap: .milliseconds(60)); defer { try? keychain.delete() }
        await eventually("live") { model.syncMode == .live }
        // Callers that want the screen now, typing, resizes and a session switch while the long poll is running.
        for i in 0..<6 {
            Task { await model.readOutput() }
            model.type(KeyMapper.items(for: "k\(i)"))
            model.reportTerminalArea(CGSize(width: 300 + Double(i) * 20, height: 500))
            await transport.setOutput("screen \(i)")
            try? await Task.sleep(for: .milliseconds(40))
        }
        try? await Task.sleep(for: .milliseconds(300))
        let peak = await transport.peakInFlight("shell.output")
        XCTAssertEqual(peak, 1)
        await model.disconnect()
    }
    func testTypingDoesNotStartExtraPollsAndTheEchoArrivesOnThePendingOne() async throws {
        // The desktop holds the long poll for up to 3 s; the echo (a screen change) must come back long before that.
        let (model, transport, keychain) = try await live(cap: .seconds(3)); defer { try? keychain.delete() }
        await eventually("waiting") { await transport.outputRequests().contains(where: self.isLongPoll) }
        let requestsBefore = await transport.outputRequests().count
        model.type(KeyMapper.items(for: "l"))
        await eventually("keys reached the desktop while the poll is still out") { await transport.calls().count == 1 }
        let requestsAfterKeys = await transport.outputRequests().count
        XCTAssertEqual(requestsAfterKeys, requestsBefore, "typing does not start a poll of its own")
        let started = Date()
        await transport.setOutput("l")
        await eventually("the echo shows") { model.output == "l" }
        XCTAssertLessThan(Date().timeIntervalSince(started), 1, "on the pending poll, not on a 3 s cycle")
        XCTAssertEqual(model.latency.echo.count, 1, "the keys-to-screen time was measured")
        XCTAssertNotNil(model.latency.echo.last)
        await model.disconnect()
    }
    func testKeysAreNotBlockedWhileALongPollWaits() async throws {
        let (model, transport, keychain) = try await live(cap: .seconds(5)); defer { try? keychain.delete() }
        await eventually("waiting") { await transport.outputRequests().contains(where: self.isLongPoll) }
        let started = Date()
        model.type(KeyMapper.items(for: "echo hi\n"))
        await eventually("delivered") { await transport.delivered(shell: self.shell) == "echo hi⏎" }
        XCTAssertLessThan(Date().timeIntervalSince(started), 1.5)
        let output = await transport.peakInFlight("shell.output"), keys = await transport.peakInFlight("shell.keys")
        XCTAssertEqual(output, 1)
        XCTAssertEqual(keys, 1)
        await model.disconnect()
    }

    // MARK: pause and resume

    func testTheLoopPausesWhenTheTerminalLeavesTheScreenAndResumesWithItsHash() async throws {
        let (model, transport, keychain) = try await live(cap: .milliseconds(150)); defer { try? keychain.delete() }
        await eventually("waiting") { await transport.outputRequests().contains(where: self.isLongPoll) }
        model.setTerminalVisible(false)
        await eventually("the wait out at that moment ran its course and nothing replaced it") { await transport.inFlightCount("shell.output") == 0 }
        let hash = await transport.currentHash()
        let paused = await transport.outputRequests().count
        try? await Task.sleep(for: .milliseconds(400))
        let stillPaused = await transport.outputRequests().count
        XCTAssertEqual(stillPaused, paused, "nothing is asked while the terminal is off screen")
        await transport.setOutput("changed while away")
        model.setTerminalVisible(true)
        await eventually("resumed and caught up") { model.output == "changed while away" }
        let requests = await transport.outputRequests()
        XCTAssertEqual(requests[paused]["if_changed"]?.string, hash, "it resumed from the hash it had, so the desktop answered with the change at once")
        XCTAssertEqual(requests[paused]["wait_ms"], .number(8000))
        await model.disconnect()
    }
    func testTheLoopPausesWhileTheAppIsNotActive() async throws {
        let (model, transport, keychain) = try await live(cap: .milliseconds(150)); defer { try? keychain.delete() }
        await eventually("waiting") { await transport.outputRequests().contains(where: self.isLongPoll) }
        model.setAppActive(false)
        await eventually("the wait out at that moment ran its course") { await transport.inFlightCount("shell.output") == 0 }
        let paused = await transport.outputRequests().count
        try? await Task.sleep(for: .milliseconds(400))
        let stillPaused = await transport.outputRequests().count
        XCTAssertEqual(stillPaused, paused)
        model.setAppActive(true)
        await eventually("waiting again") { await transport.inFlightCount("shell.output") == 1 }
        let hash = await transport.currentHash()
        let last = await transport.outputRequests().last
        XCTAssertEqual(last?["if_changed"]?.string, hash)
        await model.disconnect()
    }
    func testPausingAndResumingQuicklyDoesNotStartASecondRequestOrLoseTheWait() async throws {
        let (model, transport, keychain) = try await live(cap: .seconds(5)); defer { try? keychain.delete() }
        await eventually("waiting") { await transport.inFlightCount("shell.output") == 1 }
        let before = await transport.outputRequests().count
        for _ in 0..<5 { model.setTerminalVisible(false); model.setAppActive(false); model.setAppActive(true); model.setTerminalVisible(true) }
        try? await Task.sleep(for: .milliseconds(200))
        let after = await transport.outputRequests().count
        XCTAssertEqual(after, before, "the wait already out was kept: nothing was cancelled and nothing new was asked")
        let peak = await transport.peakInFlight("shell.output")
        XCTAssertEqual(peak, 1)
        XCTAssertEqual(model.waitSlots.stillWaiting(at: ProcessInfo.processInfo.systemUptime), 0, "pausing abandons nothing")
        await model.disconnect()
    }
    func testLeavingForTheBackgroundEndsTheWaitAndComingBackStartsLiveSyncAgain() async throws {
        let (model, transport, keychain) = try await live(cap: .seconds(5)); defer { try? keychain.delete() }
        await eventually("waiting") { await transport.outputRequests().contains(where: self.isLongPoll) }
        model.setAppActive(false)
        await model.disconnect(background: true)
        XCTAssertEqual(model.state, .suspended)
        let inFlight = await transport.inFlightCount("shell.output")
        XCTAssertEqual(inFlight, 0)
        await model.appDidBecomeActive()
        XCTAssertEqual(model.state, .connected)
        await eventually("live again on the new connection") { model.syncMode == .live }
        await eventually("waiting") { await transport.inFlightCount("shell.output") == 1 }
        await model.disconnect()
    }

    // MARK: fallback

    func testAnOlderDesktopKeepsIntervalPollingWithPlainRequests() async throws {
        let (model, transport, keychain) = try await live(hash: false); defer { try? keychain.delete() }
        XCTAssertEqual(model.syncMode, .poll)
        await eventually("several interval polls") { await transport.outputRequests().count >= 4 }
        for params in await transport.outputRequests() {
            XCTAssertNil(params["if_changed"])
            XCTAssertNil(params["wait_ms"])
            XCTAssertEqual(params["styled"], .bool(true), "colors are asked for; an older desktop just ignores it")
        }
        XCTAssertEqual(model.output, "existing session output")
        let peak = await transport.peakInFlight("shell.output")
        XCTAssertEqual(peak, 1)
        await model.disconnect()
    }
    func testADesktopThatRefusesTheNewParametersIsReadPlainAndPolled() async throws {
        for kind in [FixtureTransport.OldDesktop.strictFields, .oldCLI] {
            let (model, transport, keychain) = try await live(hash: false) { await $0.setOldDesktop(kind) }; defer { try? keychain.delete() }
            XCTAssertEqual(model.output, "existing session output", "\(kind): the first screen arrives, read again without the new parameters")
            XCTAssertNil(model.error, "\(kind): a refused optional parameter is not an error to show")
            XCTAssertEqual(model.syncMode, .poll)
            XCTAssertFalse(model.outputExtensions)
            await eventually("plain polling goes on") { await transport.outputRequests().count >= 5 }
            let requests = await transport.outputRequests()
            XCTAssertNotNil(requests[0]["styled"], "the first attempt asked")
            for params in requests.dropFirst(2) { XCTAssertEqual(Set(params.keys), ["shell_id", "lines"], "\(kind): only what an older desktop knows: \(params)") }
            let lines = await transport.lineRequests()
            XCTAssertTrue(lines.allSatisfy { $0 == 500 }, "\(kind): the refusal is not mistaken for a reply that is too large: \(lines)")
            // A new connection asks again (the desktop may have been upgraded).
            await transport.setOldDesktop(.no); await transport.setHashMode(true)
            await model.connect()
            await eventually("upgraded desktop is live") { model.syncMode == .live }
            XCTAssertTrue(model.outputExtensions)
            await model.disconnect()
        }
    }
    func testATransientCliErrorDuringAWaitDoesNotShrinkTheScrollback() async throws {
        let (model, transport, keychain) = try await live(cap: .milliseconds(40)); defer { try? keychain.delete() }
        await eventually("waiting") { await transport.outputRequests().contains(where: self.isLongPoll) }
        await transport.failNextOutputs(1, code: "cli_error")
        await eventually("spent") { await transport.pendingOutputFailures() == 0 }
        await transport.setOutput("still fine")
        await eventually("reads went on") { model.output == "still fine" }
        let lines = await transport.lineRequests()
        XCTAssertTrue(lines.allSatisfy { $0 == 500 }, "\(lines)")
        let requests = await transport.outputRequests()
        let failedAt = try XCTUnwrap(requests.indices.last(where: { self.isLongPoll(requests[$0]) && $0 + 1 < requests.count && requests[$0 + 1]["wait_ms"] == nil }), "the retry was a read that does not wait")
        XCTAssertEqual(requests[failedAt + 1]["if_changed"], requests[failedAt]["if_changed"])
        await model.disconnect()
    }
    func testADesktopThatLosesTheHashFallsBackToPolling() async throws {
        let (model, transport, keychain) = try await live(cap: .milliseconds(30)); defer { try? keychain.delete() }
        await eventually("live") { model.syncMode == .live }
        await transport.setHashMode(false)
        await eventually("polling again") { model.syncMode == .poll }
        await model.readOutput()
        XCTAssertNil(model.outputHash)
        await model.disconnect()
    }
    func testOversizeRepliesStillHalveTheLinesWhileLive() async throws {
        let (model, transport, keychain) = try await live(cap: .milliseconds(30)) { await $0.setOversize(above: 130) }; defer { try? keychain.delete() }
        await eventually("live and settled on a size that fits") { model.syncMode == .live && model.snapshotStale == false }
        await eventually("long polls carry the reduced size") {
            let polls = await transport.outputRequests().filter(self.isLongPoll)
            return polls.last?["lines"] == .number(125)
        }
        let lines = await transport.lineRequests()
        XCTAssertEqual(Array(lines.prefix(3)), [500, 250, 125])
        await model.disconnect()
    }

    // MARK: errors

    func testErrorsBackOffFromQuarterSecondAndRecover() async throws {
        let (model, transport, keychain) = try await live(cap: .milliseconds(30)); defer { try? keychain.delete() }
        await eventually("live") { model.syncMode == .live }
        await eventually("waiting") { await transport.outputRequests().contains(where: self.isLongPoll) }
        await transport.failNextOutputs(3)
        await eventually("the failures were spent") { await transport.pendingOutputFailures() == 0 }
        XCTAssertNotNil(model.error, "the failure is reported")
        await transport.setOutput("after the errors")
        await eventually("recovered", timeout: 8) { model.output == "after the errors" }
        XCTAssertNil(model.error, "and the message goes away once reads work again")
        // The three failed requests, the one that recovered and the long poll after it: each wait was longer than the one before, from 250 ms up.
        let times = Array(await transport.outputTimes().suffix(5))
        let gaps = Array(zip(times, times.dropFirst()).map { ($1 - $0).timeInterval }.prefix(3))
        XCTAssertEqual(gaps.count, 3)
        XCTAssertGreaterThanOrEqual(gaps[0], 0.2, "\(gaps)")
        XCTAssertGreaterThanOrEqual(gaps[1], 0.4, "\(gaps)")
        XCTAssertGreaterThanOrEqual(gaps[2], 0.9, "\(gaps)")
        XCTAssertEqual(model.state, .connected)
        await model.disconnect()
    }

    func testCancelledLongPollsCountAgainstTheDesktopsTwoWaitingSlots() async throws {
        // A 900 ms wait keeps the test short; the accounting is the same as for 8 s.
        let (model, transport, keychain) = try await live(cap: .seconds(5), liveWait: 900); defer { try? keychain.delete() }
        func waiting(_ params: [String: JSONValue]) -> Bool { params["wait_ms"] == .number(900) }
        await eventually("waiting") { await transport.inFlightCount("shell.output") == 1 }
        model.interruptLongPoll()
        await eventually("the replacement is out: two waits on the desktop, one of them abandoned") { await transport.inFlightCount("shell.output") == 1 && model.waitSlots.stillWaiting(at: ProcessInfo.processInfo.systemUptime) == 1 }
        let afterOne = await transport.outputRequests().count
        model.interruptLongPoll()
        await eventually("the second cancellation was recorded") { model.waitSlots.stillWaiting(at: ProcessInfo.processInfo.systemUptime) == 2 }
        // Both slots are held by cancelled waits: no third wait may be asked for; the screen is followed by short reads meanwhile.
        try? await Task.sleep(for: .milliseconds(500))
        let during = Array(await transport.outputRequests().dropFirst(afterOne))
        XCTAssertFalse(during.isEmpty)
        XCTAssertTrue(during.allSatisfy { $0["wait_ms"] == nil }, "no request waits while both slots are taken: \(during)")
        XCTAssertTrue(during.allSatisfy { $0["if_changed"] != nil }, "short reads still ask from the hash")
        let peak = await transport.peakInFlight("shell.output")
        XCTAssertEqual(peak, 1, "still one request on the wire from here")
        await eventually("waiting again once a slot frees", timeout: 5) { await transport.outputRequests().last.map(waiting) == true }
        await model.disconnect()
    }

    // MARK: sessions

    func testSwitchingSessionsNeverAsksOneShellFromAnotherShellsHash() async throws {
        let transport = FixtureTransport()
        await transport.setSessions([try session(shell), try session(other)])
        let (model, _, keychain) = try await live(cap: .milliseconds(200), transport: transport); defer { try? keychain.delete() }
        await eventually("waiting on the first shell") { await transport.outputRequests().contains(where: self.isLongPoll) }
        await model.chooseSession(try session(other))
        await eventually("waiting on the second shell") { await transport.outputRequests().contains { self.isLongPoll($0) && $0["shell_id"]?.string == self.other } }
        let requests = await transport.outputRequests()
        let firstForOther = try XCTUnwrap(requests.first(where: { $0["shell_id"]?.string == other }))
        XCTAssertNil(firstForOther["if_changed"], "the first read of a shell brings its whole screen")
        let peak = await transport.peakInFlight("shell.output")
        XCTAssertEqual(peak, 1)
        await model.disconnect()
    }

    // MARK: styled screens

    func testColorsSymbolsAndTheCursorReachTheModel() async throws {
        let styled = "\(esc)[1;32m⏺\(esc)[0m Done \(esc)[38;5;208mwarn\(esc)[0m ⚠\n\(esc)[7mbar\(esc)[0m\n$ "
        let (model, _, keychain) = try await live(hash: false) { await $0.setOutput(styled, extras: ["cursor": .object(["x": .number(2), "y": .number(2)]), "rows": .number(3), "cols": .number(40)]) }; defer { try? keychain.delete() }
        await model.readOutput()
        XCTAssertEqual(model.output, "⏺\u{FE0E} Done warn ⚠\u{FE0E}\nbar\n$  ", "symbols carry the text selector; the last blank is the cell under the cursor")
        XCTAssertFalse(model.styledOutput.isPlain)
        XCTAssertEqual(model.styledOutput.text, model.output)
        XCTAssertEqual(model.styledOutput.runs.reduce(0) { $0 + $1.length }, model.output.count)
        XCTAssertTrue(model.styledOutput.runs.contains { $0.style == CellStyle(foreground: .indexed(2), attributes: .bold) })
        XCTAssertTrue(model.styledOutput.runs.contains { $0.style == CellStyle(foreground: .indexed(208)) })
        XCTAssertTrue(model.styledOutput.runs.contains { $0.style.attributes.contains(.inverse) })
        XCTAssertEqual(model.outputCursorOffset, model.styledOutput.cursorOffset)
        XCTAssertEqual(Array(model.output)[try XCTUnwrap(model.outputCursorOffset)], " ", "the cursor sits on the blank after the prompt")
        await model.disconnect()
    }
    func testAnIdenticalScreenIsNotParsedOrPublishedAgain() async throws {
        let (model, transport, keychain) = try await live(hash: false); defer { try? keychain.delete() }
        await transport.setOutput("\(esc)[31mred\(esc)[0m")
        await model.readOutput()
        let version = model.outputVersion
        for _ in 0..<3 { await model.readOutput() }
        XCTAssertEqual(model.outputVersion, version)
        await transport.setOutput("\(esc)[32mgreen\(esc)[0m")
        await model.readOutput()
        XCTAssertEqual(model.outputVersion, version + 1)
        XCTAssertEqual(model.output, "green")
        await model.disconnect()
    }
    func testMalformedSequencesFromTheDesktopNeverBreakTheScreen() async throws {
        let hostile = "ok\(esc)[38;5;999m\(esc)[38;2;1;2mstill\(esc)[?1049h\(esc)]0;x\u{07}\(esc)[999999999;1m \(esc)[1"
        let (model, _, keychain) = try await live(hash: false) { await $0.setOutput(hostile) }; defer { try? keychain.delete() }
        await model.readOutput()
        XCTAssertEqual(model.output, "okstill ")
        XCTAssertEqual(model.state, .connected)
        await model.disconnect()
    }

    // MARK: latency numbers

    func testTheModelKeepsRoundTripsPayloadAndMode() async throws {
        let (model, transport, keychain) = try await live(hash: false) { await $0.setKeysDelay(.milliseconds(30)); await $0.setOutput("payload of some size") }; defer { try? keychain.delete() }
        await model.readOutput()
        XCTAssertEqual(model.syncMode, .poll)
        XCTAssertNotNil(model.latency.output.last, "a read that did not wait is a round trip")
        XCTAssertEqual(model.latency.payloadBytes, "payload of some size".utf8.count)
        model.type(KeyMapper.items(for: "x"))
        await eventually("keys answered") { model.latency.keys.count == 1 }
        XCTAssertGreaterThanOrEqual(model.latency.keys.last ?? 0, 25, "the delay the fake desktop added shows in the round trip")
        await transport.setOutput("x")
        await model.readOutput()
        XCTAssertEqual(model.latency.echo.count, 1)
        XCTAssertNotNil(model.latency.age(at: ProcessInfo.processInfo.systemUptime))
        await model.connect()
        XCTAssertEqual(model.latency.keys.count, 0, "a new connection starts new numbers")
        await model.disconnect()
    }
    func testLongPollAnswersThatCameBeforeTheirWaitWasOutAreNotRoundTrips() async throws {
        let (model, _, keychain) = try await live(cap: .milliseconds(50)); defer { try? keychain.delete() }
        // The fake desktop waits at most 50 ms of the 8 s asked for, so those answers are earlier than the wait and count for nothing:
        // only reads that did not wait (the first one, the one after the terminal appeared) are round trips.
        await eventually("live") { model.syncMode == .live }
        let settled = model.latency.output.count
        XCTAssertGreaterThanOrEqual(settled, 1)
        try? await Task.sleep(for: .milliseconds(400))
        XCTAssertEqual(model.latency.output.count, settled, "many long polls later, still the same samples")
        await model.disconnect()
    }

    // MARK: display settings

    func testInterfaceScalePersistsClampsAndReachesTheStyle() async throws {
        let defaults = scratchDefaults()
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let model = makeModel(FixtureTransport(), keychain, defaults: defaults)
        XCTAssertEqual(model.interfaceScale, 1.0)
        XCTAssertEqual(model.theme.style.scale, 1.0)
        model.setInterfaceScale(1.2)
        XCTAssertEqual(model.interfaceScale, 1.2)
        XCTAssertEqual(model.theme.style.scale, 1.2)
        model.stepInterfaceScale(1)
        XCTAssertEqual(model.interfaceScale, 1.25)
        model.setInterfaceScale(5)
        XCTAssertEqual(model.interfaceScale, 1.30)
        model.setInterfaceScale(0.1)
        XCTAssertEqual(model.interfaceScale, 0.80)
        model.setInterfaceScale(1.07)
        XCTAssertEqual(model.interfaceScale, 1.05)
        let restored = makeModel(FixtureTransport(), keychain, defaults: defaults)
        XCTAssertEqual(restored.interfaceScale, 1.05, "survives a restart")
        XCTAssertEqual(restored.theme.style.scale, 1.05, "and is applied before anything is drawn")
        defaults.set(99.0, forKey: RemoteModel.interfaceScaleKey)
        XCTAssertEqual(makeModel(FixtureTransport(), keychain, defaults: defaults).interfaceScale, 1.30, "a stored value out of range is clamped")
        defaults.set(Double.nan, forKey: RemoteModel.interfaceScaleKey)
        XCTAssertEqual(makeModel(FixtureTransport(), keychain, defaults: defaults).interfaceScale, 1.0)
    }
    func testAScaledStyleScalesFontsAndTargetsAndStaysEqualOnlyToItself() {
        let normal = DesktopStyle(.builtIn), large = DesktopStyle(.builtIn, scale: 1.3), small = DesktopStyle(.builtIn, scale: 0.8)
        XCTAssertNotEqual(normal, large)
        XCTAssertEqual(normal, DesktopStyle(.builtIn, scale: 1.0))
        XCTAssertEqual(normal.pt(44), 44)
        XCTAssertEqual(large.pt(44), 57)
        XCTAssertEqual(small.pt(44), 35)
        XCTAssertEqual(large.uiFont("Menlo", size: 13).pointSize, 16.9, accuracy: 0.01)
        XCTAssertEqual(small.uiFont("Menlo", size: 10).pointSize, 8, accuracy: 0.01)
        XCTAssertEqual(DesktopStyle(.builtIn, scale: 9).scale, 1.3, "clamped")
    }
    func testShowLatencyPersists() async throws {
        let defaults = scratchDefaults()
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let model = makeModel(FixtureTransport(), keychain, defaults: defaults)
        XCTAssertFalse(model.showLatency)
        model.setShowLatency(true)
        XCTAssertTrue(makeModel(FixtureTransport(), keychain, defaults: defaults).showLatency)
        model.setShowLatency(false)
        XCTAssertFalse(makeModel(FixtureTransport(), keychain, defaults: defaults).showLatency)
    }
    func testTextSizeFromTheDisplaySettingsRecomputesTheGridAndResizesTheDesktopOnce() async throws {
        let (model, transport, keychain) = try await live(hash: false); defer { try? keychain.delete() }
        model.reportTerminalArea(CGSize(width: 393, height: 560))
        await eventually("initial fit applied") { model.viewportReady && model.appliedViewport != nil }
        try? await Task.sleep(for: .milliseconds(300))
        let before = try XCTUnwrap(model.terminalViewport)
        let resizes = await transport.resizeRequests().count
        model.setTerminalFontSize(18)
        let after = try XCTUnwrap(model.terminalViewport)
        XCTAssertLessThan(after.columns, before.columns)
        XCTAssertLessThan(after.rows, before.rows)
        await eventually("the desktop pane follows") { model.appliedViewport == after }
        try? await Task.sleep(for: .milliseconds(300))
        let requests = await transport.resizeRequests()
        XCTAssertEqual(requests.count, resizes + 1)
        XCTAssertEqual(requests.last, after)
        await model.disconnect()
    }
    /// The real screen, laid out at two interface sizes: the bigger chrome leaves the terminal less room, so the grid shrinks and
    /// the desktop pane follows.
    func testALargerInterfaceLeavesTheTerminalAHeightThatShrinksTheGrid() async throws {
        let (model, transport, keychain) = try await live(hash: false); defer { try? keychain.delete() }
        let project = try JSONDecoder().decode(RemoteProject.self, from: Data(#"{"id":"\#(self.project)","name":"Fixture","root":"/fixture","created_at":1}"#.utf8))
        let host = UIHostingController(rootView: ThemedTerminal(model: model, project: project))
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 440, height: 956))
        window.rootViewController = host
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        func settle() async { for _ in 0..<20 { host.view.layoutIfNeeded(); try? await Task.sleep(for: .milliseconds(30)) } }
        await settle()
        let normal = try XCTUnwrap(model.terminalViewport, "the pane reported its size")
        await eventually("the desktop pane is fitted") { model.appliedViewport == normal }
        model.setInterfaceScale(1.3)
        await settle()
        let large = try XCTUnwrap(model.terminalViewport)
        XCTAssertLessThan(large.rows, normal.rows, "taller chrome, fewer rows: \(normal) -> \(large)")
        XCTAssertEqual(large.columns, normal.columns, "the width is the screen's")
        await eventually("the desktop pane follows") { model.appliedViewport == large }
        let resized = await transport.resizeRequests()
        XCTAssertEqual(resized.last, large)
        model.setInterfaceScale(0.8)
        await settle()
        let small = try XCTUnwrap(model.terminalViewport)
        XCTAssertGreaterThan(small.rows, normal.rows, "smaller chrome, more rows: \(normal) -> \(small)")
        await model.disconnect()
    }
}
