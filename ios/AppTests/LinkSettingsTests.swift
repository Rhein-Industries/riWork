import XCTest
import SwiftUI
import RiWorkCore
@testable import RiWorkRemote

/// The history download setting, the link meter as the model feeds it, flags that flap, and what the desktop announces: against the scripted
/// desktop over a simulated link.
@MainActor final class LinkSettingsTests: ScrollTestCase {
    private func historyCount(_ transport: FixtureTransport) async -> Int { await transport.historyRequests().count }
    private func compressing() -> DesktopFeatures {
        var features = DesktopFeatures()
        features.deflate = true; features.maximumInflatedBytes = LinkFrame.maximumInflatedBytes; features.historyMaximumLines = 5000
        return features
    }

    // MARK: the setting

    func testTheModeIsAutomaticByDefaultAndIsRemembered() throws {
        let defaults = scratchDefaults()
        let model = makeModel(FixtureTransport(), try makeStore(), defaults: defaults)
        XCTAssertEqual(model.historyMode, .automatic)
        XCTAssertTrue(model.compressTraffic)
        model.setHistoryMode(.ahead)
        model.setCompressTraffic(false)
        let again = makeModel(FixtureTransport(), try makeStore(), defaults: defaults)
        XCTAssertEqual(again.historyMode, .ahead)
        XCTAssertFalse(again.compressTraffic)
        // Something that is not a mode is automatic.
        defaults.set("everything, please", forKey: RemoteModel.historyModeKey)
        XCTAssertEqual(makeModel(FixtureTransport(), try makeStore(), defaults: defaults).historyMode, .automatic)
        for mode in HistoryMode.allCases { XCTAssertFalse(mode.title.isEmpty); XCTAssertFalse(mode.detail.isEmpty) }
    }
    func testAlwaysEverythingFetchesTheWholeHistoryOnAMeteredLowDataLowPowerLink() async throws {
        let watcher = StaticLinkWatcher(conditions: LinkConditions(constrained: true, expensive: true, lowPower: true))
        let defaults = scratchDefaults(); defaults.set(HistoryMode.everything.rawValue, forKey: RemoteModel.historyModeKey)
        let (model, _, keychain) = try await rig(ScriptedScrollback(history: 3000, weight: 4), defaults: defaults, prefetch: true, watcher: watcher); defer { try? keychain.delete() }
        await eventually("all of it, although the path says no", timeout: 10) { model.terminal.atTop }
        XCTAssertEqual(model.terminal.heldHistory, 3000)
        XCTAssertEqual(model.historyPolicy.reason, .chosenEverything)
        await model.disconnect()
    }
    func testAheadOnlyStopsAtAFewScreensEvenOnAFastFreeLink() async throws {
        let defaults = scratchDefaults(); defaults.set(HistoryMode.ahead.rawValue, forKey: RemoteModel.historyModeKey)
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 6000, weight: 4), defaults: defaults, prefetch: true); defer { try? keychain.delete() }
        model.noteReader(top: model.terminal.screenTop - 100, rows: 100)
        await eventually("ten screens are loaded", timeout: 10) { model.terminal.heldHistory >= 1000 && model.historyTask == nil }
        try? await Task.sleep(for: .milliseconds(400))
        XCTAssertLessThan(model.terminal.heldHistory, 2200, "not the whole 6,000")
        XCTAssertFalse(model.terminal.atTop)
        let asked = await historyCount(transport)
        try? await Task.sleep(for: .milliseconds(400))
        let later = await historyCount(transport)
        XCTAssertEqual(later, asked, "and nothing more while he is far from the top")
        XCTAssertEqual(model.linkMeter.tier, .fast, "the link was fast; the setting is what held it back")
        XCTAssertEqual(model.historyPolicy.appetite, .screens(10))
        await model.disconnect()
    }
    func testWithTheDownloadOffNothingIsFetchedUntilTheReaderComesNearTheTop() async throws {
        let defaults = scratchDefaults(); defaults.set(HistoryMode.off.rawValue, forKey: RemoteModel.historyModeKey)
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 5000, weight: 4), defaults: defaults, prefetch: true); defer { try? keychain.delete() }
        // The first live answer brought 500 lines; the reader is at the bottom, far from the top of what is loaded.
        model.noteReader(top: model.terminal.screenTop - 40, rows: 40)
        try? await Task.sleep(for: .milliseconds(500))
        let none = await historyCount(transport)
        XCTAssertEqual(none, 0, "nothing in the background")
        XCTAssertFalse(model.terminal.atTop)
        // He scrolls up until he is one screen from the top of what is loaded: now a page comes.
        model.noteReader(top: model.terminal.start + 30, rows: 40)
        await eventually("a page comes", timeout: 5) { await self.historyCount(transport) > 0 }
        await eventually("and then it is quiet again", timeout: 5) { model.historyTask == nil }
        XCTAssertFalse(model.terminal.atTop, "a few screens, not everything")
        XCTAssertLessThan(model.terminal.heldHistory, 3000)
        await model.disconnect()
    }
    func testChangingTheModeTakesEffectAtOnce() async throws {
        let watcher = StaticLinkWatcher(conditions: LinkConditions(expensive: true))
        let (model, _, keychain) = try await rig(ScriptedScrollback(history: 6000, weight: 24), prefetch: true, watcher: watcher); defer { try? keychain.delete() }
        model.noteReader(top: model.terminal.screenTop - 100, rows: 100)
        await eventually("ten screens", timeout: 10) { model.terminal.heldHistory >= 1000 && model.historyTask == nil }
        try? await Task.sleep(for: .milliseconds(300))
        XCTAssertFalse(model.terminal.atTop, "metered: looking ahead only")
        XCTAssertEqual(model.historyPolicy.reason, .metered)
        model.setHistoryMode(.everything)
        await eventually("and with 'everything' it goes on to the top", timeout: 10) { model.terminal.atTop }
        model.setHistoryMode(.automatic)
        await model.disconnect()
    }

    // MARK: flags that flap

    func testARestrictionThatFlapsOffIsHeldAndThenLifted() async throws {
        let watcher = StaticLinkWatcher(conditions: LinkConditions())
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.setHashMode(true, cap: .milliseconds(120))
        // Weighty lines: what is left must weigh more than the 512 KB below which a metered link fetches the rest whole.
        await transport.setScrollback(ScriptedScrollback(history: 6000, weight: 24))
        let model = makeModel(transport, keychain, prefetch: true, watcher: watcher)
        model.conditionHold = ConditionHold(calm: 0.6)
        model.setTerminalVisible(true)
        watcher.conditions = LinkConditions(expensive: true)
        XCTAssertTrue(model.linkConditions.expensive, "at once")
        await model.connect()
        model.noteReader(top: model.terminal.screenTop - 100, rows: 100)
        // It flaps: off, on, off, on, off, within a fraction of the calm time.
        for on in [false, true, false, true, false] {
            watcher.conditions = LinkConditions(expensive: on)
            try? await Task.sleep(for: .milliseconds(60))
        }
        XCTAssertTrue(model.linkConditions.expensive, "the path says no restriction, but it said so a moment ago: still held")
        XCTAssertFalse(model.terminal.atTop)
        // Past the calm time the restriction is gone and the fetch looks again by itself.
        await eventually("lifted, and the rest comes", timeout: 10) { model.terminal.atTop }
        XCTAssertFalse(model.linkConditions.expensive)
        await model.disconnect()
    }
    func testAnInterfaceThatSettlesResetsTheMeterAndALostPathDoesNot() async throws {
        let watcher = StaticLinkWatcher(interface: "wifi")
        let (model, _, keychain) = try await rig(ScriptedScrollback(history: 3000, weight: 4), prefetch: true, watcher: watcher); defer { try? keychain.delete() }
        await eventually("measured", timeout: 8) { model.linkMeter.tier != .unknown }
        let bytesPerLine = model.linkMeter.bytesPerLine
        watcher.interface = "none"
        XCTAssertNotEqual(model.linkMeter.tier, .unknown, "a moment without a link is not a different link")
        watcher.interface = "wifi"
        XCTAssertNotEqual(model.linkMeter.tier, .unknown, "and the same link coming back is the same link")
        watcher.interface = "cellular"
        XCTAssertEqual(model.linkMeter.tier, .unknown)
        XCTAssertEqual(model.linkMeter.pages, 0)
        XCTAssertEqual(model.linkMeter.bytesPerLine, bytesPerLine, "what a line weighs is not about the link")
        await model.disconnect()
    }

    // MARK: what the desktop reports

    func testAFastLinkBehindASlowDesktopIsFastWhenTheDesktopReportsItsTimeAndSlowWhenItDoesNot() async throws {
        // 3 MB/s, and a desktop that spends 30 ms and 0.35 ms a line on a page: 300 lines cost it 135 ms. Divided by the whole time that
        // is a slow link (170 KB/s); with the desktop's share taken out it is fast.
        for reports in [true, false] {
            let keychain = try makeStore(); defer { try? keychain.delete() }
            let transport = FixtureTransport()
            await transport.setHashMode(true, cap: .milliseconds(120))
            await transport.setScrollback(ScriptedScrollback(history: 4000, weight: 6))
            await transport.setLink(fixed: .milliseconds(30), bytesPerSecond: 3_000_000, perLine: .microseconds(350))
            if reports { await transport.announce(DesktopFeatures(), timing: true) }
            let model = makeModel(transport, keychain, prefetch: true)
            model.setTerminalVisible(true)
            await model.connect()
            await eventually("pages have been measured", timeout: 10) { model.linkMeter.pages >= 4 }
            XCTAssertEqual(model.linkMeter.tier, reports ? .fast : .slow, "reports: \(reports), \(model.linkMeter.bytesPerSecond ?? 0) B/s")
            if reports {
                XCTAssertNotNil(model.linkMeter.roundTripSeconds, "the first replies told the round trip: no probe")
                XCTAssertGreaterThan(model.linkMeter.desktopSeconds ?? 0, 0.1)
                let first = await transport.historyRequests().first
                XCTAssertGreaterThan(first.flatMap { if case .number(let n)? = $0["lines"] { Int(n) } else { nil } } ?? 0, HistoryPrefetch.probeLines, "a real page at once, not the ten-line probe")
            }
            await model.disconnect()
        }
    }
    func testCompressionIsAskedForAtConnectAndTheSettingTellsTheTransport() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.announce(compressing(), timing: true)
        let model = makeModel(transport, keychain)
        await model.connect()
        let asked = await transport.compressionAsked()
        XCTAssertEqual(asked, [true], "on by default, asked before the first request")
        XCTAssertTrue(model.compressionAgreed)
        XCTAssertEqual(model.historyLineLimit, 5000, "pages of up to 5,000 lines on a desktop that says so")
        model.setCompressTraffic(false)
        await eventually("the transport is told, and the desktop stops") { await transport.compressionAsked() == [true, false] && !model.compressionAgreed }
        await model.disconnect()
        // A desktop that announces nothing: no compression, pages of 1,000 lines at most.
        let old = FixtureTransport()
        let other = makeModel(old, try makeStore())
        await other.connect()
        XCTAssertFalse(other.compressionAgreed)
        XCTAssertEqual(other.historyLineLimit, 1000)
        await other.disconnect()
    }
    func testPagesGrowBeyondAThousandLinesOnlyWhereTheDesktopAllowsIt() async throws {
        for (limit, expectedMax) in [(5000, 5000), (1000, 1000)] {
            let keychain = try makeStore(); defer { try? keychain.delete() }
            let transport = FixtureTransport()
            var features = compressing(); features.historyMaximumLines = limit
            await transport.announce(features, timing: true)
            await transport.setHashMode(true, cap: .milliseconds(120))
            await transport.setScrollback(ScriptedScrollback(history: 30_000, weight: 0))
            let model = makeModel(transport, keychain, prefetch: true)
            model.setTerminalVisible(true)
            await model.connect()
            await eventually("all in", timeout: 20) { model.terminal.atTop }
            let sizes = await transport.historyRequests().compactMap { request -> Int? in if case .number(let n)? = request["lines"] { Int(n) } else { nil } }
            XCTAssertLessThanOrEqual(sizes.max() ?? 0, expectedMax, "\(sizes)")
            if limit == 5000 { XCTAssertGreaterThan(sizes.max() ?? 0, 1000, "\(sizes)") }
            await model.disconnect()
        }
    }
    func testACliThatTakesFewerLinesThanTheConnectorAnnouncedIsLearnedFromItsRefusal() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.announce(compressing(), timing: true)
        await transport.setCliLineLimit(1000)
        await transport.setHashMode(true, cap: .milliseconds(120))
        await transport.setScrollback(ScriptedScrollback(history: 20_000, weight: 0))
        let model = makeModel(transport, keychain, prefetch: true)
        model.setTerminalVisible(true)
        await model.connect()
        await eventually("all in", timeout: 20) { model.terminal.atTop }
        XCTAssertEqual(model.historyLineLimit, 1000, "learned")
        XCTAssertFalse(model.historyFailed, "silently: it is not an error")
        let sizes = await transport.historyRequests().compactMap { request -> Int? in if case .number(let n)? = request["lines"] { Int(n) } else { nil } }
        XCTAssertLessThanOrEqual(sizes.max() ?? 0, 5000)
        XCTAssertEqual(sizes.filter { $0 > 1000 }.count, 1, "one refusal, and never again: \(sizes)")
        await model.disconnect()
    }
    func testTurningCompressionOnOrOffForgetsWhatALineWeighedAndTheCapsThatWereLearnedWithIt() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.announce(compressing(), timing: true)
        await transport.setHashMode(true, cap: .milliseconds(120))
        await transport.setScrollback(ScriptedScrollback(history: 3000, weight: 6))
        let model = makeModel(transport, keychain, prefetch: true)
        model.setTerminalVisible(true)
        await model.connect()
        await eventually("a line has been weighed", timeout: 8) { model.linkMeter.bytesPerLine != nil }
        model.historyPageLines[shell] = 50
        model.historyCapStreak[shell] = 3
        model.setCompressTraffic(false)
        XCTAssertNil(model.linkMeter.bytesPerLine, "learned under the other setting")
        XCTAssertNil(model.linkMeter.compressionRatio)
        XCTAssertTrue(model.historyPageLines.isEmpty)
        XCTAssertTrue(model.historyCapStreak.isEmpty)
        XCTAssertEqual(LinkReadout(meter: model.linkMeter, conditions: LinkConditions(), interface: "wifi", compression: false).ratio, "off")
        await eventually("the desktop is told") { await transport.compressionAsked() == [true, false] }
        model.setCompressTraffic(false)   // no change: nothing is forgotten again
        await model.disconnect()
    }
    func testAPageCapIsRaisedByHalfAfterSixPagesUnderItAndDroppedOnceItNoLongerBinds() async throws {
        let (model, _, keychain) = try await rig(ScriptedScrollback(history: 100, weight: 0)); defer { try? keychain.delete() }
        model.historyLineLimit = 1000
        model.historyPageLines[shell] = 300
        for _ in 0..<5 { model.liftPageCap(id: shell) }
        XCTAssertEqual(model.historyPageLines[shell], 300, "five pages are not enough")
        model.liftPageCap(id: shell)
        XCTAssertEqual(model.historyPageLines[shell], 450)
        for _ in 0..<6 { model.liftPageCap(id: shell) }
        XCTAssertEqual(model.historyPageLines[shell], 675)
        for _ in 0..<6 { model.liftPageCap(id: shell) }
        XCTAssertNil(model.historyPageLines[shell], "1,012 is past the 1,000 the desktop takes: the cap no longer binds")
        model.liftPageCap(id: shell)   // no cap: nothing to do
        XCTAssertNil(model.historyPageLines[shell])
        await model.disconnect()
    }
    func testAHeavyReplyThatSetACapDoesNotKeepTheSessionAtSmallPagesForever() async throws {
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.setHashMode(true, cap: .milliseconds(120))
        await transport.setScrollback(ScriptedScrollback(history: 6000, weight: 0))
        await transport.setHistoryMode(.tooLarge(above: 400))
        let model = makeModel(transport, keychain, prefetch: true)
        model.setTerminalVisible(true)
        await model.connect()
        await eventually("all in, in pages the desktop takes", timeout: 20) { model.terminal.atTop }
        let sizes = await transport.historyRequests().compactMap { request -> Int? in if case .number(let n)? = request["lines"] { Int(n) } else { nil } }
        let big = sizes.filter { $0 > 400 }.count
        XCTAssertLessThanOrEqual(big, 6, "the pages that were too long are a few: \(sizes)")
        XCTAssertGreaterThan(sizes.filter { $0 <= 400 }.count, 6)
        await model.disconnect()
    }

    func testWhatAnErrorSaysAboutTheLinesAPageMayHave() {
        let reject = RemoteModel.rejectedPageLines
        XCTAssertEqual(reject("cli_error", "RiWork CLI failed: riwork: --lines needs an integer from 1 to 1000", 3000), 1000)
        XCTAssertEqual(reject("cli_error", "RiWork CLI failed: riwork: --lines needs an integer from 1 to 2000", 4000), 2000)
        XCTAssertEqual(reject("invalid_request", "lines must be 1..=1000", 2000), 1000, "a connector from before the larger pages")
        XCTAssertEqual(reject("cli_error", "RiWork CLI failed: riwork: --lines needs an integer", 2000), 1000, "no number: the old limit")
        XCTAssertNil(reject("cli_error", "RiWork CLI failed: riwork: --lines needs an integer from 1 to 1000", 800), "a page that small was not too long")
        XCTAssertNil(reject("cli_error", "tmux said no", 3000))
        XCTAssertNil(reject("response_too_large", "lines must be 1..=1000", 3000))
        XCTAssertNil(reject("not_found", "--lines", 3000))
    }

    // MARK: what the person sees

    func testTheOverlayAndTheSettingsSayWhatTheLinkIs() async throws {
        let watcher = StaticLinkWatcher(conditions: LinkConditions(expensive: true), interface: "cellular")
        let keychain = try makeStore(); defer { try? keychain.delete() }
        let transport = FixtureTransport()
        await transport.announce(compressing(), timing: true)
        await transport.setHashMode(true, cap: .milliseconds(120))
        await transport.setScrollback(ScriptedScrollback(history: 3000, weight: 6))
        await transport.setLink(fixed: .milliseconds(20), bytesPerSecond: 2_000_000)
        let model = makeModel(transport, keychain, prefetch: true, watcher: watcher)
        model.setTerminalVisible(true)
        await model.connect()
        await eventually("measured", timeout: 8) { model.linkMeter.tier != .unknown }
        let lines = model.linkLines
        XCTAssertEqual(lines.count, 3)
        XCTAssertTrue(lines[0].hasPrefix("link "), lines[0])
        XCTAssertTrue(lines[0].contains("MB/s") || lines[0].contains("KB/s"), lines[0])
        XCTAssertTrue(lines[1].hasPrefix("desk "), lines[1])
        XCTAssertTrue(lines[2].hasPrefix("hist "), lines[2])
        let readout = model.linkReadout
        XCTAssertEqual(readout.path, "cellular")
        XCTAssertEqual(readout.restrictions, "metered")
        XCTAssertNotEqual(readout.tier, "measuring")
        XCTAssertFalse(model.historyPolicy.reason.sentence.isEmpty)
        XCTAssertEqual(LinkReadout.milliseconds(0.0244), "24 ms")
        XCTAssertEqual(LinkReadout.milliseconds(0.0042), "4.2 ms")
        XCTAssertEqual(LinkReadout.rate(1_840_000), "1.8 MB/s")
        XCTAssertEqual(LinkReadout.rate(230_000), "230 KB/s")
        XCTAssertEqual(HistoryAppetite.everything.summary, "the whole history")
        XCTAssertEqual(HistoryAppetite.screens(10).summary, "10 screens ahead")
        await model.disconnect()
    }
}
