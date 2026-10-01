import XCTest
import SwiftUI
import RiWorkCore
@testable import RiWorkRemote

/// Fetching history in the background, against the scripted desktop over a simulated link: how it starts, how it paces itself, when it
/// holds back, and what it does when the desktop draws its history again.
@MainActor final class PrefetchTests: ScrollTestCase {
    private func requestsFor(_ transport: FixtureTransport, shell id: String) async -> [[String: JSONValue]] {
        await transport.historyRequests().filter { $0["shell_id"]?.string == id }
    }
    private func number(_ request: [String: JSONValue], _ key: String) -> Int {
        if case .number(let n)? = request[key] { return Int(n) } else { return -1 }
    }

    // MARK: on a good link: everything, in the background, after the live screen

    func testTheLiveScreenComesFirstThenHistoryFillsInOnePageAtATimeUntilTheBeginning() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 5000, weight: 4), prefetch: true); defer { try? keychain.delete() }
        XCTAssertFalse(model.terminal.isEmpty, "the live screen is there")
        await eventually("all of the history is in", timeout: 10) { model.terminal.atTop }
        XCTAssertEqual(model.terminal.start, 0)
        XCTAssertEqual(model.terminal.heldHistory, 5000)
        assertConsistent(model)
        let requests = await transport.historyRequests()
        let first = try XCTUnwrap(requests.first)
        XCTAssertEqual(number(first, "lines"), HistoryPrefetch.probeLines, "the first request is a probe that learns the fixed cost of a request")
        XCTAssertEqual(number(first, "end"), 500)
        let sizes = requests.map { number($0, "lines") }
        XCTAssertGreaterThan(sizes.count, 3)
        XCTAssertGreaterThan(sizes[2], sizes[1], "pages grow while the link keeps up")
        XCTAssertLessThanOrEqual(sizes.max() ?? 0, HistoryLimits.maximumPageLines)
        let peak = await transport.peakInFlight("shell.history")
        XCTAssertEqual(peak, 1, "one history request in flight, ever")
        XCTAssertEqual(model.linkMeter.tier, LinkTier.fast)
        XCTAssertNil(model.historyTask, "and it is over")
        XCTAssertEqual(model.historyHeader?.text, "Beginning of history")
        XCTAssertFalse(model.historyFailed)
        await model.disconnect()
    }

    func testOnceOlderLinesComeAsHistoryEveryLiveAnswerAsksForFewerLines() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 3000), prefetch: true); defer { try? keychain.delete() }
        await eventually("history is flowing") { model.historySupport == .supported }
        await eventually("all in", timeout: 8) { model.terminal.atTop }
        await transport.write(3)
        await eventually("new lines") { model.terminal.screenTop == 3003 }
        let lines = await transport.lineRequests()
        XCTAssertEqual(lines.first, 500, "the first answer brings the recent scrollback so the screen is not empty")
        XCTAssertEqual(lines.last, HistoryLimits.liveScrollbackLines, "afterwards 120 lines are enough to join one answer to the next")
        XCTAssertEqual(model.terminal.heldHistory, 3003, "and no line was lost by asking for fewer")
        assertConsistent(model)
        await model.disconnect()
    }

    func testADesktopThatCannotPageKeepsAskingForTheWholeFiveHundredLines() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000), mode: .unsupported, prefetch: true); defer { try? keychain.delete() }
        await eventually("learned") { model.historySupport == .unsupported }
        await transport.write(2)
        await eventually("new lines") { model.terminal.screenTop == 2002 }
        let lines = await transport.lineRequests()
        XCTAssertTrue(lines.allSatisfy { $0 == 500 }, "\(lines)")
        let asked = await transport.historyRequests().count
        XCTAssertEqual(asked, 1, "one try, never again on this connection")
        await model.disconnect()
    }

    // MARK: on a poor link or a restricted path: a few screens ahead

    func testASlowLinkLooksAheadOfTheReaderAndNoFurther() async throws {
        // 100 KB/s and 30 ms per request: about 1 MB/s is fast, a quarter of that good; this is slow.
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 12_000, weight: 6), prefetch: true, link: (fixed: .milliseconds(30), bytesPerSecond: 100_000)); defer { try? keychain.delete() }
        // The reader sees 120 rows: ten screens are 1,200 lines.
        model.noteReader(top: model.terminal.screenTop - 120, rows: 120)
        await eventually("the lookahead is loaded", timeout: 20) { model.terminal.heldHistory >= 1200 && model.historyTask == nil }
        XCTAssertEqual(model.linkMeter.tier, LinkTier.slow)
        let held = model.terminal.heldHistory
        XCTAssertLessThan(held, 1200 + 400, "it stopped a page or two past ten screens")
        let asked = await transport.historyRequests().count
        try? await Task.sleep(for: .milliseconds(600))
        let later = await transport.historyRequests().count
        XCTAssertEqual(later, asked, "nothing more while the reader is far from the top")
        // Pages were sized to what the link carries in a tenth of a second or so, not a thousand lines.
        let sizes = await transport.historyRequests().map { number($0, "lines") }
        XCTAssertLessThan(sizes.max() ?? 0, 500, "\(sizes)")
        // The reader climbs: within five screens of the top of what is loaded, fetching starts again, well before the top.
        model.noteReader(top: model.terminal.start + 4 * 120, rows: 120)
        await eventually("more", timeout: 20) { model.terminal.heldHistory > held && model.historyTask == nil }
        assertConsistent(model)
        await model.disconnect()
    }

    func testAReaderWhoReachesTheTopEndsThePauseBetweenBackgroundPages() async throws {
        // Everything is wanted but the link is good, not fast: pages are spaced by their own duration, here about a second.
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 6000, weight: 20), prefetch: true, link: (fixed: .milliseconds(700), bytesPerSecond: 400_000)); defer { try? keychain.delete() }
        await eventually("the loop is resting between two pages", timeout: 15) { model.historySleeper != nil && model.linkMeter.pages >= 3 }
        let before = await transport.historyRequests().count
        // The reader is at the very top of what is loaded while the loop sleeps between pages.
        model.noteReader(top: model.terminal.start + 5, rows: 50)
        let moment = ContinuousClock.now
        await eventually("a page comes at once", timeout: 3) { await transport.historyRequests().count > before }
        let times = await transport.historyRequestTimes()
        XCTAssertLessThan((times[before] - moment).timeInterval, 0.5, "not after the rest of the pause")
        await model.disconnect()
    }

    func testLowDataModeLooksLessFarAheadAndIsLiftedWhenTheModeIs() async throws {
        let watcher = StaticLinkWatcher(conditions: LinkConditions(constrained: true))
        let (model, _, keychain) = try await rig(ScriptedScrollback(history: 4000, weight: 12), prefetch: true, watcher: watcher); defer { try? keychain.delete() }
        model.noteReader(top: model.terminal.screenTop - 100, rows: 100)
        await eventually("five screens are loaded", timeout: 10) { model.terminal.heldHistory >= 500 && model.historyTask == nil }
        try? await Task.sleep(for: .milliseconds(400))
        XCTAssertLessThan(model.terminal.heldHistory, 1200, "Low Data Mode wants five screens, not the whole history")
        XCTAssertFalse(model.terminal.atTop)
        // The restriction is held for a while after the path stops saying so (ten seconds in life); a test does not wait that long.
        model.conditionHold.calm = 0.3
        watcher.conditions = LinkConditions()
        XCTAssertTrue(model.linkConditions.constrained, "held for a moment")
        await eventually("and with the mode off everything comes", timeout: 10) { model.terminal.atTop }
        assertConsistent(model)
        await model.disconnect()
    }

    func testOnARestrictedLinkAHoleIsFilledNearTheReaderAndTheRestWaitsForHim() async throws {
        let watcher = StaticLinkWatcher(conditions: LinkConditions(constrained: true))
        let (model, _, keychain) = try await rig(ScriptedScrollback(history: 2000, weight: 3), prefetch: true, watcher: watcher); defer { try? keychain.delete() }
        model.noteReader(top: model.terminal.screenTop - 100, rows: 100)
        await eventually("settled", timeout: 5) { model.historyTask == nil }
        // A flood: 5,000 lines in one go, far more than an answer reaches back.
        let epoch = model.terminal.epoch
        await model.chooseSession(try session(shell))   // no-op; keeps the reader's place
        await transportWrite(model, 5000)
        await eventually("a hole", timeout: 5) { model.terminal.missingLines > 0 }
        model.noteReader(top: model.terminal.screenTop - 100, rows: 100)
        await eventually("the part near the reader is filled", timeout: 10) { model.historyTask == nil && model.terminal.missingLines < 4900 }
        try? await Task.sleep(for: .milliseconds(400))
        XCTAssertGreaterThan(model.terminal.missingLines, 3500, "the rest of the 5,000 lines is not downloaded on a link that was asked to be light")
        XCTAssertEqual(model.terminal.epoch, epoch)
        // The reader scrolls up into it: now it is wanted.
        let hole = try XCTUnwrap(model.terminal.holes.first)
        model.noteReader(top: hole.lowerBound + 200, rows: 100)
        await eventually("and fetched around him", timeout: 10) { model.terminal.missingLines < 3000 }
        assertConsistent(model)
        await model.disconnect()
    }
    private func transportWrite(_ model: RemoteModel, _ count: Int) async {
        // The scripted desktop is the model's client.
        if let transport = model.client as? FixtureTransport { await transport.write(count) }
    }

    func testAMeteredLinkAndLowPowerModeAlsoLookAhead() async throws {
        for conditions in [LinkConditions(expensive: true), LinkConditions(lowPower: true)] {
            let watcher = StaticLinkWatcher(conditions: conditions)
            // Weighty lines: what is left must weigh more than the 512 KB below which a metered link fetches the rest whole.
            let (model, _, keychain) = try await rig(ScriptedScrollback(history: 4000, weight: 24), prefetch: true, watcher: watcher); defer { try? keychain.delete() }
            model.noteReader(top: model.terminal.screenTop - 100, rows: 100)
            await eventually("ten screens", timeout: 10) { model.terminal.heldHistory >= 1000 && model.historyTask == nil }
            try? await Task.sleep(for: .milliseconds(300))
            XCTAssertFalse(model.terminal.atTop, "\(conditions)")
            XCTAssertLessThan(model.terminal.heldHistory, 2000, "\(conditions)")
            await model.disconnect()
        }
    }

    func testAChangeOfLinkForgetsWhatWasMeasuredOfTheOne() async throws {
        let watcher = StaticLinkWatcher(interface: "wifi")
        let (model, _, keychain) = try await rig(ScriptedScrollback(history: 3000, weight: 4), prefetch: true, watcher: watcher); defer { try? keychain.delete() }
        await eventually("measured", timeout: 8) { model.linkMeter.tier != .unknown }
        watcher.interface = "cellular"
        XCTAssertEqual(model.linkMeter.tier, LinkTier.unknown, "cellular is not wifi")
        XCTAssertEqual(model.linkMeter.pages, 0)
        await model.disconnect()
    }

    // MARK: holding back

    func testNoPageStartsWhileKeysAreGoingOutAndFetchingResumesWhenTheyAreQuiet() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 6000, weight: 6), prefetch: true, link: (fixed: .milliseconds(25), bytesPerSecond: 400_000)); defer { try? keychain.delete() }
        await eventually("fetching", timeout: 5) { await transport.historyRequests().count >= 2 }
        // Type something and keep the keys slow to be sent: the desktop takes 300 ms per batch.
        await transport.setKeysDelay(.milliseconds(300))
        model.type(KeyMapper.items(for: "ls"))
        try? await Task.sleep(for: .milliseconds(120))   // a page that was already out may land
        let during = await transport.historyRequests().count
        try? await Task.sleep(for: .milliseconds(500))
        let still = await transport.historyRequests().count
        let delivered = await transport.delivered(shell: shell)
        XCTAssertEqual(delivered, "ls", "the keys went out first")
        XCTAssertLessThanOrEqual(still - during, 1, "at most the page that was already on its way; nothing starts while typing")
        await transport.setKeysDelay(Duration?.none)
        await eventually("and it carries on after", timeout: 15) { await transport.historyRequests().count > still + 2 }
        await model.disconnect()
    }

    func testTypingIsNotHeldUpByAPageThatIsOnItsWay() async throws {
        // A slow page is out when a key is typed: the key's batch has its own lane on the desktop and goes at once.
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 6000, weight: 6), prefetch: true, link: (fixed: .milliseconds(500), bytesPerSecond: nil)); defer { try? keychain.delete() }
        await eventually("a page is out", timeout: 5) { await transport.inFlightCount("shell.history") == 1 }
        let start = ContinuousClock.now
        model.type(KeyMapper.items(for: "x"))
        await eventually("the key reaches the shell") { await transport.delivered(shell: self.shell) == "x" }
        XCTAssertLessThan((ContinuousClock.now - start).timeInterval, 0.35, "not behind the 500 ms page")
        await model.disconnect()
    }

    func testTwoWaitsHoldingTheSharedSlotsKeepAPageFromTakingTheLast() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 6000, weight: 4), prefetch: true); defer { try? keychain.delete() }
        model.cancelHistory()
        let before = await transport.historyRequests().count
        // Two cancelled long polls still waiting on the desktop, for another 0.8 s.
        let now = ProcessInfo.processInfo.systemUptime
        model.waitSlots.abandon(startedAt: now - 7.7, wait: 8)
        model.waitSlots.abandon(startedAt: now - 7.7, wait: 8)
        model.kickPrefetch()
        try? await Task.sleep(for: .milliseconds(400))
        let held = await transport.historyRequests().count
        XCTAssertEqual(held, before, "a plain read must still find a slot")
        await eventually("and they go on when the waits are over", timeout: 6) { await transport.historyRequests().count > before }
        await model.disconnect()
    }

    // MARK: the desktop draws its history again

    func testAPageInFlightWhenTheHistoryIsWipedAndDrawnAgainIsNeverStitchedOn() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000), prefetch: false); defer { try? keychain.delete() }
        await transport.gateHistory(true)
        model.loadOlderHistory()
        await eventually("the page is out") { await transport.historyRequests().count == 1 }
        // An inline agent clears its scrollback and draws the transcript again at the new width: 1,300 lines saying something else.
        var drawn = ScriptedScrollback(history: 1300); drawn.label = "W"
        await transport.setScrollback(drawn)
        await eventually("the model saw the history shrink") { model.terminal.screenTop == 1300 }
        await transport.gateHistory(false)
        try? await Task.sleep(for: .milliseconds(200))
        XCTAssertFalse(model.historyFailed, "silent: this is the history being redrawn, not an error")
        for index in model.terminal.indices where model.terminal[index]?.text.hasPrefix("L") == true { return XCTFail("old line \(index) is in the new history") }
        XCTAssertEqual(model.terminal.heldHistory, 500, "just the new live answer's scrollback")
        await model.disconnect()
    }

    func testAfterARedrawTheFetchStartsFromTheLiveScreenAgainWithoutAnyError() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 3000, weight: 4), prefetch: true); defer { try? keychain.delete() }
        await eventually("some history", timeout: 5) { model.terminal.heldHistory > 900 }
        var drawn = ScriptedScrollback(history: 2400, weight: 4); drawn.label = "W"
        await transport.setScrollback(drawn)
        await eventually("noticed", timeout: 5) { model.terminal.screenTop == 2400 }
        await eventually("all of the new history is in", timeout: 12) { model.terminal.atTop }
        XCTAssertFalse(model.historyFailed)
        XCTAssertNil(model.error)
        XCTAssertEqual(model.terminal.heldHistory, 2400)
        for index in model.terminal.indices where model.terminal[index]?.text != "W\(index)" { return XCTFail("index \(index) is \(String(describing: model.terminal[index]?.text))") }
        await model.disconnect()
    }

    func testTheFetchWaitsOutTheRedrawAfterThePhonesOwnResize() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.setHashMode(true, cap: .milliseconds(120))
        await transport.setScrollback(ScriptedScrollback(history: 4000, weight: 4))
        let model = makeModel(transport, keychain, prefetch: true)
        model.settleScale = 1   // the real 0.8 s
        model.setTerminalVisible(true)
        model.reportTerminalArea(CGSize(width: 393, height: 600))
        await model.connect()
        await eventually("the pane was fitted") { await transport.resizeRequests().count >= 1 }
        let fitted = ContinuousClock.now
        await eventually("the first screen") { !model.terminal.isEmpty }
        // Nothing is asked of the history until it has settled.
        try? await Task.sleep(for: .milliseconds(450))
        let early = await transport.historyRequests().count
        XCTAssertEqual(early, 0, "the agents redraw for about half a second after a resize")
        await eventually("and then it starts", timeout: 5) { await transport.historyRequests().count > 0 }
        let times = await transport.historyRequestTimes()
        let firstTime = try XCTUnwrap(times.first)
        XCTAssertGreaterThanOrEqual((firstTime - fitted).timeInterval, 0.6)
        await model.disconnect()
    }

    // MARK: pages as they are on the wire

    func testPagesThatEndInBlankLinesLoadLikeAnyOther() async throws {
        // Every seventh line is blank, so pages end in blank lines (and a page is sometimes nothing but one) at all sorts of places.
        var scrollback = ScriptedScrollback(history: 4000, weight: 3)
        scrollback.blanks = Set(stride(from: 0, to: 4100, by: 7))
        let (model, _, keychain) = try await rig(scrollback, prefetch: true); defer { try? keychain.delete() }
        await eventually("everything is in", timeout: 12) { model.terminal.atTop }
        XCTAssertFalse(model.historyFailed, "a page ending in a blank line is a page, not an error")
        XCTAssertEqual(model.terminal.heldHistory, 4000)
        for index in model.terminal.start..<model.terminal.screenTop where model.terminal[index]?.text != (scrollback.blanks.contains(index) ? "" : "L\(index)") {
            return XCTFail("index \(index) is \(String(describing: model.terminal[index]?.text))")
        }
        await model.disconnect()
    }

    // MARK: holes

    func testOutputBurstingPastWhatAnAnswerReachesBackKeepsTheHistoryAndFetchesTheGap() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 3000, weight: 2), prefetch: true); defer { try? keychain.delete() }
        await eventually("all in", timeout: 8) { model.terminal.atTop }
        let start = model.terminal.start
        let epoch = model.terminal.epoch
        await transport.write(700)   // far more than the 120 lines an answer reaches back
        await eventually("the screen moved", timeout: 5) { model.terminal.screenTop == 3700 }
        XCTAssertEqual(model.terminal.epoch, epoch, "nothing was renumbered")
        await eventually("the gap is fetched", timeout: 8) { model.terminal.holes.isEmpty && model.terminal.screenTop == 3700 }
        XCTAssertEqual(model.terminal.start, start, "and the 3,000 lines of history were never thrown away")
        XCTAssertEqual(model.terminal.heldHistory, 3700)
        assertConsistent(model)
        XCTAssertFalse(model.historyFailed)
        await model.disconnect()
    }

    // MARK: shells that are not on screen

    func testGoingBackToAShellDoesNotFetchItsHistoryAgain() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2500, weight: 4), prefetch: true); defer { try? keychain.delete() }
        await transport.setSessions([try session(shell), try session(other)])
        await model.refresh()
        await eventually("the first shell is complete", timeout: 8) { model.terminal.atTop }
        let askedBefore = await requestsFor(transport, shell: shell).count
        await model.chooseSession(try session(other))
        await eventually("the other shell is shown") { model.outputSessionID == self.other && !model.terminal.isEmpty }
        await model.chooseSession(try session(shell))
        await eventually("the first again") { model.outputSessionID == self.shell && !model.terminal.isEmpty }
        XCTAssertTrue(model.terminal.atTop, "its history came back with it")
        XCTAssertEqual(model.terminal.start, 0)
        assertConsistent(model)
        try? await Task.sleep(for: .milliseconds(300))
        let askedAfter = await requestsFor(transport, shell: shell).count
        XCTAssertEqual(askedAfter, askedBefore, "not one request for lines the phone already had")
        await model.disconnect()
    }

    func testAShellThatWasWipedWhileAwayShowsNoneOfItsOldLines() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2500, weight: 4), prefetch: true); defer { try? keychain.delete() }
        await transport.setSessions([try session(shell), try session(other)])
        await model.refresh()
        await eventually("complete", timeout: 8) { model.terminal.atTop }
        await model.chooseSession(try session(other))
        await eventually("other") { model.outputSessionID == self.other && !model.terminal.isEmpty }
        var drawn = ScriptedScrollback(history: 2500, weight: 4); drawn.label = "W"
        await transport.setScrollback(drawn)
        await model.chooseSession(try session(shell))
        await eventually("the first again, with its new lines", timeout: 8) { model.outputSessionID == self.shell && model.terminal.atTop && model.terminal[model.terminal.screenTop - 1]?.text.hasPrefix("W") == true }
        for index in model.terminal.indices where model.terminal[index]?.text != "W\(index)" { return XCTFail("index \(index) is \(String(describing: model.terminal[index]?.text))") }
        await model.disconnect()
    }

    func testAReconnectKeepsWhatIsHeldAndOnlyContinues() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 4000, weight: 4), prefetch: true, link: (fixed: .milliseconds(40), bytesPerSecond: 300_000)); defer { try? keychain.delete() }
        await eventually("part of it", timeout: 8) { model.terminal.heldHistory > 1500 }
        await model.disconnect()
        let held = model.terminal.heldHistory
        let start = model.terminal.start
        XCTAssertGreaterThan(held, 1500, "the lines are still there while disconnected")
        let asked = await transport.historyRequests().count
        await model.connect()
        await eventually("connected and carrying on", timeout: 15) { model.terminal.atTop }
        XCTAssertEqual(model.terminal.heldHistory, 4000)
        assertConsistent(model)
        let after = await transport.historyRequests().dropFirst(asked)
        let lowestEnd = after.map { number($0, "end") }.min() ?? 0
        XCTAssertGreaterThanOrEqual(lowestEnd, held - 40, "nothing below what was held is asked for again (start was \(start))")
        await model.disconnect()
    }
}
