import XCTest
import SwiftUI
import RiWorkCore
@testable import RiWorkRemote

/// Scrolling: deep history paging, the lines held under stable indexes, the sticky bottom, the alternate screen. Against the fake
/// desktop with a scripted scrollback (`ScriptedScrollback`), so what the phone holds can be checked against what the desktop has.
@MainActor final class ScrollingTests: ScrollTestCase {
    private func loadPage(_ model: RemoteModel, file: StaticString = #filePath, line: UInt = #line) async {
        model.loadOlderHistory()
        let end = Date().addingTimeInterval(4)
        while model.historyTask != nil, Date() < end { try? await Task.sleep(for: .milliseconds(5)) }
        XCTAssertNil(model.historyTask, "the fetch finished", file: file, line: line)
    }
    private func ends(_ transport: FixtureTransport) async -> [Int] {
        await transport.historyRequests().compactMap { if case .number(let n)? = $0["end"] { Int(n) } else { nil } }
    }
    private func lineCounts(_ transport: FixtureTransport) async -> [Int] {
        await transport.historyRequests().compactMap { if case .number(let n)? = $0["lines"] { Int(n) } else { nil } }
    }

    // MARK: lines under stable indexes

    func testTheLiveAnswerFillsTheBufferAndNewOutputKeepsEveryIndex() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000)); defer { try? keychain.delete() }
        XCTAssertEqual(model.terminal.screenTop, 2000)
        XCTAssertEqual(model.terminal.heldHistory, 500, "the latest 500 lines of scrollback")
        XCTAssertEqual(model.terminal.historySize, 2000)
        assertConsistent(model)
        let anchor = model.terminal[1900]
        let epoch = model.terminal.epoch
        await transport.write(5)
        await eventually("the new lines arrive") { model.terminal.screenTop == 2005 }
        XCTAssertEqual(model.terminal[1900], anchor, "a line keeps its index while lines are added below it")
        XCTAssertEqual(model.terminal.epoch, epoch)
        assertConsistent(model)
        XCTAssertEqual(model.output.split(separator: "\n").last, "L2016", "output and styledOutput stay the latest live answer")
        await model.disconnect()
    }

    // MARK: paging

    func testPagingAsksForEachPageJustAboveWhatIsHeld() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 1300)); defer { try? keychain.delete() }
        XCTAssertFalse(model.terminal.atTop)
        for held in [800, 1100, 1300] {
            await loadPage(model)
            XCTAssertEqual(model.terminal.heldHistory, held)
            assertConsistent(model)
        }
        let requests = await transport.historyRequests()
        let askedEnds = await ends(transport), askedLines = await lineCounts(transport)
        XCTAssertEqual(askedEnds, [500, 800, 1100], "end = the lines already held above the screen")
        XCTAssertEqual(askedLines, [300, 300, 300])
        XCTAssertTrue(requests.allSatisfy { $0["styled"] == .bool(true) && $0["shell_id"]?.string == shell })
        XCTAssertTrue(model.terminal.atTop, "the last page was short: nothing older exists")
        XCTAssertEqual(model.terminal.start, 0)
        model.loadOlderHistory()
        try? await Task.sleep(for: .milliseconds(100))
        let after = await transport.historyRequests().count
        XCTAssertEqual(after, 3, "nothing more is asked for once the beginning is loaded")
        XCTAssertFalse(model.historyLoading)
        XCTAssertFalse(model.historyFailed)
        await model.disconnect()
    }
    func testAShortHistoryNeverAsksForAPage() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 300)); defer { try? keychain.delete() }
        XCTAssertTrue(model.terminal.atTop)
        model.loadOlderHistory()
        try? await Task.sleep(for: .milliseconds(100))
        let requests = await transport.historyRequests()
        XCTAssertTrue(requests.isEmpty)
        await model.disconnect()
    }
    func testACompletePageEndsPaging() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 700)); defer { try? keychain.delete() }
        await loadPage(model)
        XCTAssertEqual(model.terminal.heldHistory, 700)
        XCTAssertTrue(model.terminal.atTop)
        model.loadOlderHistory()
        try? await Task.sleep(for: .milliseconds(80))
        let count = await transport.historyRequests().count
        XCTAssertEqual(count, 1)
        assertConsistent(model)
        await model.disconnect()
    }
    func testOneHistoryRequestAtATimeWhileTheLivePollAndTypingGoOn() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000)); defer { try? keychain.delete() }
        await transport.gateHistory(true)
        for _ in 0..<5 { model.loadOlderHistory() }
        await eventually("the request is out") { await transport.historyRequests().count == 1 }
        XCTAssertTrue(model.historyLoading)
        for _ in 0..<5 { model.loadOlderHistory() }
        try? await Task.sleep(for: .milliseconds(60))
        let asked = await transport.historyRequests().count
        XCTAssertEqual(asked, 1, "one at a time")
        // The long poll keeps delivering while the page is on its way...
        await transport.write(4)
        await eventually("live output arrives") { model.terminal.screenTop == 2004 }
        // ... and typing is not held up by it.
        model.type(KeyMapper.items(for: "ls"))
        await eventually("keys reach the shell") { await transport.delivered(shell: self.shell) == "ls" }
        await transport.gateHistory(false)
        await eventually("the page is in") { !model.historyLoading && model.terminal.heldHistory > 504 }
        let peak = await transport.peakInFlight("shell.history")
        XCTAssertEqual(peak, 1)
        assertConsistent(model)
        await model.disconnect()
    }
    func testOutputArrivingWhileAPageIsOnItsWayLosesNothingAndRepeatsNothing() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000)); defer { try? keychain.delete() }
        await transport.gateHistory(true)
        model.loadOlderHistory()
        await eventually("the request is out") { await transport.historyRequests().count == 1 }
        await transport.write(12)
        await eventually("the live answer moved the screen") { model.terminal.screenTop == 2012 }
        await transport.gateHistory(false)
        await eventually("the page is in") { !model.historyLoading }
        XCTAssertEqual(model.terminal.start, 1212, "the page covered the lines 12 further down than it was asked for, and 12 of them were held already")
        XCTAssertEqual(model.terminal.heldHistory, 800)
        assertConsistent(model)
        // the next page continues right above
        await loadPage(model)
        XCTAssertEqual(model.terminal.start, 912)
        let later = await ends(transport)
        XCTAssertEqual(later.last, 800, "end counts the lines held above the screen, 512 + 288")
        assertConsistent(model)
        await model.disconnect()
    }
    func testHistoryThatGrowsBetweenPagesKeepsEachLineUnderItsIndex() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 3000)); defer { try? keychain.delete() }
        await loadPage(model)
        let remembered = model.terminal[2400]
        await transport.write(25)
        await eventually("grown") { model.terminal.screenTop == 3025 }
        await loadPage(model)
        await transport.write(3)
        await eventually("grown again") { model.terminal.screenTop == 3028 }
        await loadPage(model)
        XCTAssertEqual(model.terminal[2400], remembered)
        XCTAssertEqual(model.terminal.heldHistory, 500 + 25 + 3 + 3 * 300, "each page added its 300 new lines")
        assertConsistent(model)
        await model.disconnect()
    }
    func testResponseTooLargeHalvesTheLinesAndRemembersIt() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000), mode: .tooLarge(above: 100)); defer { try? keychain.delete() }
        await loadPage(model)
        let first = await lineCounts(transport)
        XCTAssertEqual(first, [300, 150, 75], "halved until it fits")
        XCTAssertEqual(model.terminal.heldHistory, 575)
        XCTAssertFalse(model.historyFailed)
        await loadPage(model)
        let second = await lineCounts(transport)
        XCTAssertEqual(second.last, 75, "the next page starts from the size that fit")
        XCTAssertEqual(model.terminal.heldHistory, 650)
        assertConsistent(model)
        await model.disconnect()
    }
    func testAPageThatNeverFitsFailsQuietlyAndIsRetriedLater() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000), mode: .tooLarge(above: 1)); defer { try? keychain.delete() }
        await loadPage(model)
        XCTAssertTrue(model.historyFailed)
        XCTAssertNil(model.error, "no banner for a page that could not be loaded")
        let tries = await lineCounts(transport)
        XCTAssertEqual(tries.last, HistoryLimits.minimumPageLines)
        model.loadOlderHistory()
        let asked = await transport.historyRequests().count
        XCTAssertEqual(asked, tries.count, "not again at once")
        await transport.setHistoryMode(.ok)
        model.retryHistory()
        await eventually("tapping retry works") { !model.historyLoading && !model.historyFailed && model.terminal.heldHistory > 500 }
        await model.disconnect()
    }
    func testADesktopThatDoesNotKnowShellHistoryKeepsTodaysBehaviour() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000), mode: .unsupported); defer { try? keychain.delete() }
        XCTAssertEqual(model.historySupport, .unknown)
        await loadPage(model)
        XCTAssertEqual(model.historySupport, .unsupported)
        XCTAssertFalse(model.historyPaging)
        XCTAssertNil(model.error)
        XCTAssertFalse(model.historyFailed)
        model.loadOlderHistory()
        try? await Task.sleep(for: .milliseconds(80))
        let count = await transport.historyRequests().count
        XCTAssertEqual(count, 1, "asked once, never again on this connection")
        XCTAssertEqual(model.terminal.heldHistory, 500, "the screen and the latest 500 lines of scrollback, as before")
        assertConsistent(model)
        // A new connection asks again: the desktop may have been upgraded.
        await model.connect()
        XCTAssertEqual(model.historySupport, .unknown)
        await model.disconnect()
    }
    func testADesktopWithoutHistorySizeNeverPagesButStillKeepsIndexes() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000), reportsHistorySize: false); defer { try? keychain.delete() }
        XCTAssertNil(model.terminal.historySize)
        XCTAssertFalse(model.terminal.canPage)
        XCTAssertFalse(model.historyPaging)
        model.loadOlderHistory()
        try? await Task.sleep(for: .milliseconds(80))
        let count = await transport.historyRequests().count
        XCTAssertEqual(count, 0)
        let epoch = model.terminal.epoch
        let top = model.terminal.screenTop
        let anchor = model.terminal[top - 30]
        await transport.write(7)
        await eventually("new lines") { model.terminal.screenTop == top + 7 }
        XCTAssertEqual(model.terminal.epoch, epoch, "the lines were matched up without a history size")
        XCTAssertEqual(model.terminal[top - 30], anchor, "and the line kept its index")
        assertConsistent(model)
        await model.disconnect()
    }
    func testAStrictDesktopThatRejectsStyledIsAskedOnceMoreWithoutIt() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000), mode: .rejectsStyled); defer { try? keychain.delete() }
        await loadPage(model)
        let requests = await transport.historyRequests()
        XCTAssertEqual(requests.count, 2)
        XCTAssertEqual(requests[0]["styled"], .bool(true))
        XCTAssertNil(requests[1]["styled"])
        XCTAssertEqual(model.terminal.heldHistory, 800)
        XCTAssertEqual(model.historySupport, .supported)
        await loadPage(model)
        let third = await transport.historyRequests().last
        XCTAssertNil(third?["styled"], "and plain for the rest of the connection")
        await model.disconnect()
    }
    func testARealInvalidRequestIsAQuietFailureAndDoesNotTurnPagingOff() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000), mode: .failing(code: "invalid_request")); defer { try? keychain.delete() }
        await loadPage(model)
        XCTAssertTrue(model.historyFailed)
        XCTAssertEqual(model.historySupport, .unknown, "an error about the request is not the desktop lacking the method")
        let requests = await transport.historyRequests()
        XCTAssertEqual(requests.count, 1, "and `styled` was not dropped for it")
        XCTAssertEqual(requests[0]["styled"], .bool(true))
        await transport.setHistoryMode(.ok)
        model.retryHistory()
        await eventually("works again") { !model.historyFailed && model.terminal.heldHistory == 800 }
        await model.disconnect()
    }
    func testAPageWhoseLineCountDoesNotMatchItsLinesIsNotPlaced() async throws {
        let (model, _, keychain) = try await rig(ScriptedScrollback(history: 2000), mode: .wrongCount); defer { try? keychain.delete() }
        await loadPage(model)
        XCTAssertTrue(model.historyFailed)
        XCTAssertEqual(model.terminal.heldHistory, 500)
        await model.disconnect()
    }
    func testAMissingSessionBacksOffInsteadOfAskingAtEveryScrollStep() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000)); defer { try? keychain.delete() }
        await transport.setMissing(shell)
        model.loadOlderHistory()
        await eventually("asked") { await transport.historyRequests().count == 1 }
        await eventually("done") { model.historyTask == nil }
        for _ in 0..<5 { model.loadOlderHistory() }
        try? await Task.sleep(for: .milliseconds(80))
        let asked = await transport.historyRequests().count
        XCTAssertEqual(asked, 1)
        XCTAssertFalse(model.historyFailed, "no retry row for a session that is gone")
        await model.disconnect()
    }
    func testNoWordOfAFingerSurvivesTheScrollViewThatHadIt() async throws {
        let defaults = scratchDefaults()
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000), alternate: false, defaults: defaults); defer { try? keychain.delete() }
        model.setScrollBusy(true)
        await transport.setAlternate(true)
        await eventually("the program's screen") { model.alternateScreen }
        XCTAssertFalse(model.scrollBusy, "the scroll view is gone, and nothing will say its finger lifted")
        await transport.setAlternate(false)
        await eventually("back") { !model.alternateScreen }
        model.setScrollBusy(true)
        await transport.setSessions([try session(shell), try session(other)])
        await model.refresh()
        await model.chooseSession(try session(other))
        XCTAssertFalse(model.scrollBusy)
        await model.disconnect()
    }
    func testAnOtherErrorIsAQuietFailureWithARetryRow() async throws {
        let (model, _, keychain) = try await rig(ScriptedScrollback(history: 2000), mode: .failing(code: "cli_error")); defer { try? keychain.delete() }
        await loadPage(model)
        XCTAssertTrue(model.historyFailed)
        XCTAssertNil(model.error)
        XCTAssertEqual(model.historySupport, .unknown)
        XCTAssertEqual(model.terminal.heldHistory, 500)
        await model.disconnect()
    }
    func testHistoryStopsAtTheCapOfTwentyThousandLines() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 30_000)); defer { try? keychain.delete() }
        var guardCount = 0
        while !model.terminal.limitReached, guardCount < 100 { await loadPage(model); guardCount += 1 }
        XCTAssertEqual(model.terminal.heldHistory, HistoryLimits.heldLines)
        XCTAssertFalse(model.terminal.atTop)
        let asked = await transport.historyRequests().count
        model.loadOlderHistory()
        try? await Task.sleep(for: .milliseconds(80))
        let askedAfter = await transport.historyRequests().count
        XCTAssertEqual(askedAfter, asked, "no more pages past the cap")
        XCTAssertEqual(model.terminal.start, 10_000)
        assertConsistent(model)
        // new output pushes the oldest lines out
        await transport.write(50)
        await eventually("held at the cap") { model.terminal.screenTop == 30_050 && model.terminal.heldHistory == HistoryLimits.heldLines }
        XCTAssertEqual(model.terminal.start, 10_050)
        assertConsistent(model)
        await model.disconnect()
    }
    func testAFullDesktopHistoryIsFollowedAndItsPagesOverlapSoTheSeamIsChecked() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 1500, cap: 1500)); defer { try? keychain.delete() }
        let epoch = model.terminal.epoch
        await transport.write(9)
        await eventually("the screen moved though history_size did not") { model.terminal.screenTop == 1509 }
        XCTAssertEqual(model.terminal.historySize, 1500)
        XCTAssertEqual(model.terminal.epoch, epoch, "re-anchored, not renumbered")
        XCTAssertTrue(model.terminal.drifting)
        assertConsistent(model)
        await loadPage(model)
        let requests = await transport.historyRequests()
        XCTAssertEqual(requests.count, 1)
        if case .number(let lines)? = requests[0]["lines"], case .number(let end)? = requests[0]["end"] {
            XCTAssertEqual(Int(lines), 300 + HistoryLimits.verifyLines)
            XCTAssertEqual(Int(end), 509 - HistoryLimits.verifyLines)
        }
        XCTAssertEqual(model.terminal.heldHistory, 809)
        assertConsistent(model)
        // A line arrives while a page is on its way: history_size cannot say so, the overlap can. Nothing is stitched on wrongly.
        await transport.gateHistory(true)
        model.loadOlderHistory()
        await eventually("request out") { await transport.historyRequests().count == 2 }
        await transport.write(2)
        await transport.gateHistory(false)
        await eventually("settled") { !model.historyLoading }
        assertConsistent(model)
        await eventually("and the next fetch works") { model.terminal.screenTop == 1511 }
        await loadPage(model)
        assertConsistent(model)
        await model.disconnect()
    }
    func testAPageThatWaitsForAMovingFingerIsPutInWhenTheScrollStops() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000)); defer { try? keychain.delete() }
        model.setScrollBusy(true)
        model.loadOlderHistory()
        await eventually("the page has arrived") { await transport.historyRequests().count == 1 }
        try? await Task.sleep(for: .milliseconds(200))
        XCTAssertEqual(model.terminal.heldHistory, 500, "nothing moves under the finger")
        XCTAssertTrue(model.historyLoading)
        model.setScrollBusy(false)
        await eventually("now it is in") { model.terminal.heldHistory == 800 && !model.historyLoading }
        assertConsistent(model)
        await model.disconnect()
    }
    func testASwitchToAnotherShellThrowsTheOldPageAway() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000)); defer { try? keychain.delete() }
        await transport.setSessions([try session(shell), try session(other)])
        await model.refresh()
        await transport.gateHistory(true)
        model.loadOlderHistory()
        await eventually("request out") { await transport.historyRequests().count == 1 }
        await model.chooseSession(try session(other))
        XCTAssertNil(model.historyTask, "the fetch for the first shell is over")
        XCTAssertFalse(model.historyLoading)
        await transport.gateHistory(false)
        try? await Task.sleep(for: .milliseconds(150))
        await eventually("the other shell's own screen") { model.outputSessionID == self.other && !model.terminal.isEmpty }
        XCTAssertEqual(model.terminal.heldHistory, 500, "nothing of the old page is in it")
        assertConsistent(model)
        await model.disconnect()
    }

    // MARK: the sticky bottom, in the model

    private func metrics(away: Double, content: Double = 10_000) -> ScrollMetrics {
        ScrollMetrics(offset: content - 600 - away, contentHeight: content, viewportHeight: 600)
    }
    func testScrollingUpStopsFollowingAndNewLinesAreCounted() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000)); defer { try? keychain.delete() }
        XCTAssertTrue(model.scrollFollow.following)
        XCTAssertNil(model.scrollFollow.pill)
        _ = model.scrollMetricsChanged(from: metrics(away: 0), to: metrics(away: 400), lineHeight: 14, userDriven: true)
        XCTAssertFalse(model.scrollFollow.following)
        await transport.write(6)
        await eventually("six new lines counted") { model.scrollFollow.newLines == 6 }
        XCTAssertEqual(model.scrollFollow.pill?.label, "↓ Live · 6 new")
        await transport.write(1)
        await eventually("one more") { model.scrollFollow.newLines == 7 }
        // back at the bottom by hand
        _ = model.scrollMetricsChanged(from: metrics(away: 400), to: metrics(away: 3), lineHeight: 14, userDriven: true)
        XCTAssertTrue(model.scrollFollow.following)
        XCTAssertEqual(model.scrollFollow.newLines, 0)
        XCTAssertNil(model.scrollFollow.pill)
        await model.disconnect()
    }
    func testTypingSendingAndTheMenuAllJumpBackToTheBottom() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000)); defer { try? keychain.delete() }
        func scrollUp() async {
            _ = model.scrollMetricsChanged(from: metrics(away: 0), to: metrics(away: 900), lineHeight: 14, userDriven: true)
            await transport.write(3)
            await eventually("counted") { model.scrollFollow.newLines > 0 }
            XCTAssertFalse(model.scrollFollow.following)
        }
        await scrollUp()
        let typed = model.typedCount
        model.type(KeyMapper.items(for: "x"))
        XCTAssertTrue(model.scrollFollow.following, "typing resumes following")
        XCTAssertEqual(model.scrollFollow.newLines, 0)
        XCTAssertEqual(model.typedCount, typed + 1, "and tells the view to scroll")
        await scrollUp()
        let jumps = model.jumpRequests
        model.jumpToLatest()
        XCTAssertTrue(model.scrollFollow.following)
        XCTAssertEqual(model.jumpRequests, jumps + 1)
        await scrollUp()
        model.setPreferLineComposer(true)
        model.reportTerminalArea(CGSize(width: 393, height: 600))
        await eventually("ready to send") { model.canSend }
        let sent = model.jumpRequests
        await model.submit(line: "echo hi")
        XCTAssertTrue(model.scrollFollow.following, "sending a line does too")
        XCTAssertGreaterThan(model.jumpRequests, sent)
        await model.disconnect()
    }
    func testOpeningOrSwitchingAShellStartsAtTheBottom() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000)); defer { try? keychain.delete() }
        await transport.setSessions([try session(shell), try session(other)])
        await model.refresh()
        _ = model.scrollMetricsChanged(from: metrics(away: 0), to: metrics(away: 900), lineHeight: 14, userDriven: true)
        XCTAssertFalse(model.scrollFollow.following)
        await model.chooseSession(try session(other))
        XCTAssertTrue(model.scrollFollow.following)
        XCTAssertEqual(model.scrollFollow.newLines, 0)
        await model.disconnect()
    }
    func testABottomThatMovesAwayFromAFollowingViewIsChased() async throws {
        let (model, _, keychain) = try await rig(ScriptedScrollback(history: 100)); defer { try? keychain.delete() }
        let grown = ScrollMetrics(offset: 9400, contentHeight: 10_140, viewportHeight: 600)
        XCTAssertEqual(model.scrollMetricsChanged(from: metrics(away: 0), to: grown, lineHeight: 14, userDriven: false), .scrollToBottom)
        _ = model.scrollMetricsChanged(from: metrics(away: 0), to: metrics(away: 700), lineHeight: 14, userDriven: true)
        XCTAssertEqual(model.scrollMetricsChanged(from: metrics(away: 700), to: grown, lineHeight: 14, userDriven: false), .none, "not when the user is reading")
        await model.disconnect()
    }

    // MARK: the alternate screen

    func testAFullScreenProgramHasNoScrollbackAndTheNormalBufferWaits() async throws {
        let defaults = scratchDefaults()
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 2000), alternate: false, defaults: defaults); defer { try? keychain.delete() }
        XCTAssertFalse(model.alternateScreen)
        let before = model.terminal
        await transport.setAlternate(true)
        await eventually("the program's screen") { model.alternateScreen }
        XCTAssertEqual(model.alternateLines.count, 12, "just its screen, without any scrollback")
        XCTAssertEqual(model.alternateLines.first?.text, "ALT 0")
        XCTAssertEqual(model.terminal, before, "the normal screen's lines wait unchanged")
        XCTAssertTrue(model.alternateHintLoud, "the first program")
        model.loadOlderHistory()
        try? await Task.sleep(for: .milliseconds(80))
        let asked = await transport.historyRequests().count
        XCTAssertEqual(asked, 0, "nothing to page on the alternate screen")
        await transport.write(4)   // a program that redraws; nothing is added to the list
        try? await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(model.terminal, before)
        // and back
        await transport.setAlternate(false)
        await eventually("normal scrolling again") { !model.alternateScreen && model.terminal.screenTop == 2004 }
        XCTAssertTrue(model.scrollFollow.following)
        assertConsistent(model)
        await loadPage(model)
        XCTAssertEqual(model.terminal.heldHistory, 804, "paging works again")
        assertConsistent(model)
        await model.disconnect()
    }
    func testTheAlternateHintIsLoudForTheFirstFewProgramsOnly() async throws {
        let defaults = scratchDefaults()
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 100), alternate: false, defaults: defaults); defer { try? keychain.delete() }
        for entry in 1...5 {
            await transport.setAlternate(true)
            await eventually("in \(entry)") { model.alternateScreen }
            XCTAssertEqual(model.alternateHintLoud, entry <= RemoteModel.loudAlternateHints, "program \(entry)")
            await transport.setAlternate(false)
            await eventually("out \(entry)") { !model.alternateScreen }
        }
        await model.disconnect()
    }
    func testAnUnchangedAnswerCanAlsoCarryTheAlternateFlag() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 100), alternate: false); defer { try? keychain.delete() }
        let version = model.outputVersion
        // The flag flips without the screen changing: the long poll that is waiting is answered `unchanged`, with the flag.
        await transport.setAlternateQuietly(true)
        await eventually("alternate, from an unchanged answer") { model.alternateScreen }
        XCTAssertEqual(model.outputVersion, version, "the screen itself was not touched")
        await transport.setAlternateQuietly(false)
        await eventually("and back") { !model.alternateScreen }
        await model.disconnect()
    }
    func testASwipeOnTheAlternateScreenSendsPageKeysWithoutTyping() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 100), alternate: true); defer { try? keychain.delete() }
        await eventually("alternate") { model.alternateScreen }
        let typed = model.typedCount
        XCTAssertEqual(model.sendPageKey(.pageUp), .accepted)
        XCTAssertEqual(model.sendPageKey(.pageDown), .accepted)
        await eventually("both reach the shell in order") { await transport.delivered(shell: self.shell) == "⇞⇟" }
        XCTAssertEqual(model.typedCount, typed, "a swipe is not typing")
        let items = await transport.calls().flatMap(\.items)
        XCTAssertEqual(items, [.object(["key": .string("PageUp")]), .object(["key": .string("PageDown")])])
        await model.disconnect()
    }
    func testPageKeysAlsoWorkWithTheLineComposerAndAreDroppedOffline() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 100), alternate: true); defer { try? keychain.delete() }
        model.setPreferLineComposer(true)
        XCTAssertFalse(model.directTyping)
        XCTAssertEqual(model.type(KeyMapper.items(for: "x")), .unavailable, "typing goes through the composer")
        XCTAssertEqual(model.sendPageKey(.pageDown), .accepted, "paging a program does not need direct typing")
        await eventually("delivered") { await transport.delivered(shell: self.shell) == "⇟" }
        await model.disconnect()
        XCTAssertEqual(model.sendPageKey(.pageUp), .unavailable, "not held for a reconnect")
        XCTAssertNil(model.pendingKeyBuffer)
    }
    func testADesktopThatCannotTakeKeysCannotPageAProgram() async throws {
        let (model, transport, keychain) = try await rig(ScriptedScrollback(history: 100), alternate: true); defer { try? keychain.delete() }
        await transport.setKeysMode(.unsupported)
        XCTAssertEqual(model.sendPageKey(.pageUp), .accepted)
        await eventually("learned") { model.keysSupport == .unsupported }
        XCTAssertEqual(model.sendPageKey(.pageUp), .unavailable)
        await model.disconnect()
    }
}
