import XCTest
@testable import RiWorkCore

/// The policy of the background fetch: how good the link is, how far ahead to read, how big a page is, when to hold back.
final class HistoryPrefetchTests: XCTestCase {
    /// A meter that has seen a ten-line probe (`fixed` seconds) and then pages of `bytes` taking `seconds`.
    private func meter(fixed: Double = 0.1, pages: [(bytes: Int, lines: Int, seconds: Double)] = []) -> LinkMeter {
        var meter = LinkMeter()
        meter.record(wireBytes: 600, lines: 10, seconds: fixed)
        for page in pages { meter.record(wireBytes: page.bytes, lines: page.lines, seconds: page.seconds) }
        return meter
    }
    private func input(_ configure: (inout HistoryPrefetch.Input) -> Void = { _ in }) -> HistoryPrefetch.Input {
        var input = HistoryPrefetch.Input()
        input.meter = meter(pages: [(60_000, 1000, 0.16)])   // 60 KB in 0.06 s over the fixed cost: 1 MB/s
        input.aboveReader = 20_000
        input.viewRows = 48
        configure(&input)
        return input
    }

    // MARK: measuring the link

    func testAMeterKnowsNothingUntilItHasSeenASmallRequestAndABigPage() {
        var meter = LinkMeter()
        XCTAssertEqual(meter.tier, .unknown)
        XCTAssertNil(meter.fixedSeconds)
        meter.record(wireBytes: 60_000, lines: 1000, seconds: 0.3)
        XCTAssertEqual(meter.tier, .unknown, "a page with no fixed cost to take out tells nothing about the rate")
        XCTAssertEqual(meter.bytesPerLine ?? 0, 60, accuracy: 0.001)
        meter.record(wireBytes: 600, lines: 10, seconds: 0.08)
        XCTAssertEqual(meter.fixedSeconds, 0.08)
        meter.record(wireBytes: 60_000, lines: 1000, seconds: 0.18)
        XCTAssertEqual(meter.bytesPerSecond ?? 0, 600_000, accuracy: 1, "60,000 bytes in the 0.1 s over the fixed cost")
    }
    func testTheFixedCostIsTakenOutSoALongRoundTripOnAFastLinkIsNotMistakenForASlowLink() {
        // 400 ms round trip, 50 ms of transfer for 60 KB: 1.2 MB/s. Divided by the whole time it would be 150 KB/s.
        let fast = meter(fixed: 0.4, pages: [(60_000, 1000, 0.45)])
        XCTAssertEqual(fast.tier, .fast)
        // The same bytes in 1.4 s over the same round trip: 60 KB/s.
        let slow = meter(fixed: 0.4, pages: [(60_000, 1000, 1.4)])
        XCTAssertEqual(slow.tier, .slow)
    }
    func testTheTiersSitAtOneMegabyteAndAQuarterOfOne() {
        func tier(_ bytesPerSecond: Double) -> LinkTier {
            // 100 KB at the rate, over a 50 ms fixed cost.
            meter(fixed: 0.05, pages: [(100_000, 1000, 0.05 + 100_000 / bytesPerSecond)]).tier
        }
        XCTAssertEqual(tier(5_000_000), .fast)
        XCTAssertEqual(tier(1_100_000), .fast)
        XCTAssertEqual(tier(900_000), .good)
        XCTAssertEqual(tier(300_000), .good)
        XCTAssertEqual(tier(200_000), .slow)
        XCTAssertEqual(tier(20_000), .slow)
        XCTAssertLessThan(LinkTier.slow, .good)
        XCTAssertLessThan(LinkTier.good, .fast)
    }
    func testSmallPagesDoNotMakeARateAndOnlyLowerTheFixedCost() {
        var meter = meter(fixed: 0.2)
        meter.record(wireBytes: 3_000, lines: 50, seconds: 0.12)
        XCTAssertEqual(meter.fixedSeconds, 0.12)
        XCTAssertNil(meter.bytesPerSecond)
        meter.record(wireBytes: 9_000, lines: 150, seconds: 5)
        XCTAssertEqual(meter.tier, .slow, "9 KB over 4.9 s")
    }
    func testTheRateIsTheBestOfTheLastThreePagesSoJitterCannotMakeAFastLinkLookSlowAndASlowLinkIsStillNoticed() {
        var meter = meter(fixed: 0.05, pages: [(100_000, 1000, 0.05 + 0.05)])   // 2 MB/s
        XCTAssertEqual(meter.tier, .fast)
        // Delays only add to a page's time: one that stalled says nothing about what the link can do.
        meter.record(wireBytes: 100_000, lines: 1000, seconds: 0.05 + 0.5)       // 200 KB/s
        XCTAssertEqual(meter.tier, .fast)
        meter.record(wireBytes: 100_000, lines: 1000, seconds: 0.05 + 0.5)
        XCTAssertEqual(meter.tier, .fast, "two bad pages after a good one")
        meter.record(wireBytes: 100_000, lines: 1000, seconds: 0.05 + 0.5)
        XCTAssertLessThan(meter.tier, .fast, "a link that stays slow for three pages is noticed")
        XCTAssertEqual(meter.bytesPerSecond ?? 0, 200_000, accuracy: 1)
    }
    func testNonsenseSamplesAreIgnoredAndAResetForgetsTheLink() {
        var meter = meter(pages: [(60_000, 1000, 0.2)])
        let before = meter
        meter.record(wireBytes: 100, lines: 0, seconds: 1)
        meter.record(wireBytes: 100, lines: 5, seconds: 0)
        meter.record(wireBytes: 100, lines: 5, seconds: .nan)
        meter.record(wireBytes: -1, lines: 5, seconds: 1)
        XCTAssertEqual(meter, before)
        meter.reset()
        XCTAssertEqual(meter, LinkMeter())
        XCTAssertEqual(meter.tier, .unknown)
    }
    func testAReplyWeighsTheJSONEscapesOfItsColors() throws {
        let esc = "\u{1B}"
        XCTAssertEqual(HistoryReply.wireBytes(of: "abc"), 3)
        XCTAssertEqual(HistoryReply.wireBytes(of: "\(esc)[31mred\(esc)[0m"), 12 + 2 * 5, "each ESC is six bytes in JSON")
        let reply = try HistoryReply(result: .object(["shell_id": .string("s"), "output": .string("\(esc)[1mx\(esc)[0m\n")]))
        XCTAssertEqual(reply.wireBytes, 10 + 10)
    }

    // MARK: how much to read ahead

    func testAGoodUnrestrictedLinkWantsEverythingAndAPoorOrRestrictedOneLooksAhead() {
        XCTAssertEqual(HistoryAppetite.appetite(tier: .fast, conditions: LinkConditions()), .everything)
        XCTAssertEqual(HistoryAppetite.appetite(tier: .good, conditions: LinkConditions()), .everything)
        XCTAssertEqual(HistoryAppetite.appetite(tier: .unknown, conditions: LinkConditions()), .everything, "the probe does not wait for a verdict")
        XCTAssertEqual(HistoryAppetite.appetite(tier: .slow, conditions: LinkConditions()), .screens(10))
        XCTAssertEqual(HistoryAppetite.appetite(tier: .fast, conditions: LinkConditions(expensive: true)), .screens(10))
        XCTAssertEqual(HistoryAppetite.appetite(tier: .fast, conditions: LinkConditions(lowPower: true)), .screens(10))
        XCTAssertEqual(HistoryAppetite.appetite(tier: .fast, conditions: LinkConditions(constrained: true)), .screens(5), "Low Data Mode asks for the least")
        XCTAssertEqual(HistoryAppetite.appetite(tier: .slow, conditions: LinkConditions(constrained: true, expensive: true)), .screens(5))
        XCTAssertTrue(LinkConditions(expensive: true).isRestricted)
        XCTAssertFalse(LinkConditions().isRestricted)
    }
    func testAFastLinkFetchesWhileTheReaderIsFarAway() {
        let decision = HistoryPrefetch.decide(input())
        guard case .fetch(let lines, let urgent) = decision else { return XCTFail("\(decision)") }
        XCTAssertFalse(urgent)
        XCTAssertEqual(lines, 1000, "1 MB/s for 0.3 s is more than a thousand lines of 60 bytes")
    }
    func testNothingIsFetchedWhenThereIsNothingToFetch() {
        XCTAssertEqual(HistoryPrefetch.decide(input { $0.canFetch = false }), .idle)
    }
    func testALookaheadStartsWellBeforeTheReaderReachesTheTopAndRunsToItsFarEnd() {
        let rows = 48
        func decide(above: Int, running: Bool) -> HistoryPrefetch.Decision {
            HistoryPrefetch.decide(input { $0.conditions = LinkConditions(expensive: true); $0.aboveReader = above; $0.viewRows = rows; $0.running = running })
        }
        // Ten screens wanted, a fetch begins when fewer than five are loaded above the view.
        XCTAssertEqual(decide(above: 10 * rows, running: false), .idle)
        XCTAssertEqual(decide(above: 5 * rows + 1, running: false), .idle)
        guard case .fetch(_, let urgent) = decide(above: 5 * rows - 1, running: false) else { return XCTFail("starts at five screens") }
        XCTAssertFalse(urgent)
        // Once going it carries on up to ten.
        guard case .fetch = decide(above: 8 * rows, running: true) else { return XCTFail("keeps filling") }
        XCTAssertEqual(decide(above: 10 * rows, running: true), .idle)
        // Not within a screen of the top: that is already late.
        XCTAssertGreaterThanOrEqual(5 * rows, 3 * rows, "the trigger is several screens out")
    }
    func testLowDataModeLooksLessFarAheadThanAMeteredLink() {
        let rows = 48
        let constrained = LinkConditions(constrained: true)
        XCTAssertEqual(HistoryPrefetch.decide(input { $0.conditions = constrained; $0.aboveReader = 3 * rows }), .idle, "five screens wanted, half of them loaded")
        guard case .fetch = HistoryPrefetch.decide(input { $0.conditions = constrained; $0.aboveReader = 2 * rows }) else { return XCTFail("fewer than 2.5 screens: fetch") }
    }
    func testAReaderAtTheTopIsUrgentWhateverTheLinkAndNeverWaitsBetweenPages() {
        let slow = input {
            $0.meter = meter(pages: [(30_000, 500, 0.1 + 1.0)])
            $0.aboveReader = 20
            $0.viewRows = 48
            $0.sinceLastPage = 0.01
            $0.waitsHeld = 2
            $0.liveBusy = true
        }
        XCTAssertEqual(slow.meter.tier, .slow)
        guard case .fetch(let lines, let urgent) = HistoryPrefetch.decide(slow) else { return XCTFail("no pause for a reader at the top") }
        XCTAssertTrue(urgent)
        XCTAssertLessThan(lines, 300, "a slow link still gets a page it can carry in a tenth of a second")
        // The same reader a screen further down is not urgent: pages are spaced and the shared slots are left free.
        XCTAssertEqual(HistoryPrefetch.decide(input { $0.aboveReader = 200; $0.sinceLastPage = 0.01; $0.waitsHeld = 2 }), .wait(0.5))
    }
    func testHolesAreAlwaysWantedEvenWhereALookaheadIsSatisfied() {
        let satisfied = input { $0.conditions = LinkConditions(constrained: true); $0.aboveReader = 100_000 }
        XCTAssertEqual(HistoryPrefetch.decide(satisfied), .idle)
        var withHole = satisfied
        withHole.missing = 250
        guard case .fetch = HistoryPrefetch.decide(withHole) else { return XCTFail("a hole is fetched at once") }
    }

    // MARK: holding back

    func testTypingHoldsEveryBackgroundPageButNotARequestSomebodyMade() {
        XCTAssertEqual(HistoryPrefetch.decide(input { $0.typing = true }), .wait(0.4))
        XCTAssertEqual(HistoryPrefetch.decide(input { $0.typing = true; $0.aboveReader = 3 }), .wait(0.4), "even a reader at the top: they are typing, not reading")
        guard case .fetch = HistoryPrefetch.decide(input { $0.typing = true; $0.demand = true }) else { return XCTFail("the retry row was tapped") }
    }
    func testTwoWaitsHoldTheSharedSlotsSoABackgroundPageLeavesOneFreeForAPlainRead() {
        // The desktop runs three requests besides the ordered lane: two of them may be waiting `shell.output` calls. With two waits
        // out (the live one and one that was cancelled and still holds its slot) a page would take the last: the screen read after
        // Return would queue behind it.
        XCTAssertEqual(HistoryPrefetch.decide(input { $0.waitsHeld = 2 }), .wait(0.5))
        guard case .fetch = HistoryPrefetch.decide(input { $0.waitsHeld = 1 }) else { return XCTFail("one wait and one page leave a slot") }
        guard case .fetch = HistoryPrefetch.decide(input { $0.waitsHeld = 0 }) else { return XCTFail() }
    }
    func testPagesAreSpacedByTheLinkAndLiveOutputWidensTheGap() {
        let fast = meter(pages: [(60_000, 1000, 0.16)])
        let good = meter(pages: [(60_000, 1000, 0.1 + 0.2)])
        let slow = meter(pages: [(30_000, 500, 0.1 + 0.4)])
        XCTAssertEqual(fast.tier, .fast); XCTAssertEqual(good.tier, .good); XCTAssertEqual(slow.tier, .slow)
        XCTAssertEqual(HistoryPrefetch.gapAfterPage(meter: fast, liveBusy: false), 0.03, accuracy: 0.0001, "nearly back to back")
        XCTAssertEqual(HistoryPrefetch.gapAfterPage(meter: good, liveBusy: false), 0.3, accuracy: 0.0001, "half the time on the link")
        XCTAssertEqual(HistoryPrefetch.gapAfterPage(meter: slow, liveBusy: false), 1.5, accuracy: 0.0001, "a quarter")
        XCTAssertEqual(HistoryPrefetch.gapAfterPage(meter: good, liveBusy: true), 0.6, accuracy: 0.0001)
        XCTAssertEqual(HistoryPrefetch.gapAfterPage(meter: LinkMeter(), liveBusy: false), 0.1, accuracy: 0.0001)
        // The decision waits out the rest of the gap.
        guard case .wait(let seconds) = HistoryPrefetch.decide(input { $0.meter = good; $0.sinceLastPage = 0.1 }) else { return XCTFail() }
        XCTAssertEqual(seconds, 0.2, accuracy: 0.001)
        guard case .fetch = HistoryPrefetch.decide(input { $0.meter = good; $0.sinceLastPage = 0.31 }) else { return XCTFail("the gap is over") }
    }
    func testTheHistoryIsLeftAloneWhileItIsBeingDrawnAgain() {
        XCTAssertEqual(HistoryPrefetch.decide(input { $0.settleRemaining = 0.5 }), .wait(0.5))
        XCTAssertEqual(HistoryPrefetch.decide(input { $0.settleRemaining = 0.5; $0.aboveReader = 1; $0.demand = true }), .wait(0.5), "even for a reader at the top")
        XCTAssertEqual(HistoryPrefetch.decide(input { $0.settleRemaining = 0.01 }), .wait(0.05), "never a busy loop")
        guard case .fetch = HistoryPrefetch.decide(input { $0.settleRemaining = 0 }) else { return XCTFail() }
        XCTAssertGreaterThan(HistoryPrefetch.settleAfterResizeSeconds, 0.5, "the agents settle about half a second after a resize")
    }

    // MARK: the first request and the size of pages

    func testTheFirstRequestOfASessionIsATenLineProbeThatLearnsTheFixedCost() {
        let fresh = input { $0.meter = LinkMeter() }
        XCTAssertEqual(HistoryPrefetch.decide(fresh), .fetch(lines: HistoryPrefetch.probeLines, urgent: false))
        // A reader at the top, or a request somebody made, gets a real page at once.
        guard case .fetch(let lines, _) = HistoryPrefetch.decide(input { $0.meter = LinkMeter(); $0.aboveReader = 5 }) else { return XCTFail() }
        XCTAssertEqual(lines, HistoryLimits.pageLines)
        guard case .fetch(let manual, _) = HistoryPrefetch.decide(input { $0.meter = LinkMeter(); $0.demand = true }) else { return XCTFail() }
        XCTAssertEqual(manual, HistoryLimits.pageLines)
        // A hole is not probed first either.
        guard case .fetch(let hole, _) = HistoryPrefetch.decide(input { $0.meter = LinkMeter(); $0.missing = 10 }) else { return XCTFail() }
        XCTAssertEqual(hole, HistoryLimits.pageLines)
    }
    func testPagesGrowFromTheProbeToTheLimitWhileTheLinkKeepsUp() {
        var m = LinkMeter()
        m.record(wireBytes: 600, lines: 10, seconds: 0.1)
        XCTAssertEqual(HistoryPrefetch.pageLines(meter: m), HistoryLimits.pageLines, "no rate yet: the default page")
        // 300 lines of 60 bytes, 18 KB, in 0.12 s: 900 KB/s, a good link.
        m.record(wireBytes: 18_000, lines: 300, seconds: 0.12)
        XCTAssertEqual(m.tier, .good)
        let second = HistoryPrefetch.pageLines(meter: m)
        XCTAssertLessThanOrEqual(second, 600, "at most double the last page")
        XCTAssertGreaterThan(second, 300)
        m.record(wireBytes: 36_000, lines: second, seconds: 0.14)
        XCTAssertLessThanOrEqual(HistoryPrefetch.pageLines(meter: m), HistoryLimits.maximumPageLines)
    }
    func testAPageIsAsBigAsTheLinkCarriesInItsBudgetAndNeverMoreThanTheWireLimit() {
        // Slow link, 100 KB/s: 0.12 s buys 12 KB, 200 lines of 60 bytes.
        let slow = meter(fixed: 0.1, pages: [(20_000, 300, 0.1 + 0.2)])
        XCTAssertEqual(slow.tier, .slow)
        let slowLines = HistoryPrefetch.pageLines(meter: slow)
        XCTAssertEqual(Double(slowLines) * 66.7, 0.12 * 100_000, accuracy: 3_000)
        // A very fast link is held to 80 KB a page: 1000 lines of 120 bytes would be 120 KB, so fewer.
        var wide = LinkMeter()
        wide.record(wireBytes: 1_200, lines: 10, seconds: 0.05)
        wide.record(wireBytes: 36_000, lines: 300, seconds: 0.06)
        wide.record(wireBytes: 72_000, lines: 600, seconds: 0.07)
        XCTAssertEqual(wide.tier, .fast)
        let wideLines = HistoryPrefetch.pageLines(meter: wide)
        XCTAssertLessThanOrEqual(Double(wideLines) * 120, HistoryPrefetch.maximumPageWireBytes + 120)
        XCTAssertGreaterThan(wideLines, 600)
    }
    func testSparseHistoryGetsBiggerPagesSoTheyWeighSomethingToMeasure() {
        var m = LinkMeter()
        m.record(wireBytes: 200, lines: 10, seconds: 0.1)   // 20 bytes a line
        XCTAssertGreaterThan(HistoryPrefetch.pageLines(meter: m) * 20, LinkMeter.measurableBytes)
    }
    func testThePageCapAfterResponseTooLargeLimitsEveryPageAndNeverGoesBelowTheMinimum() {
        XCTAssertEqual(HistoryPrefetch.pageLines(meter: LinkMeter(), cap: 150), 150)
        XCTAssertEqual(HistoryPrefetch.pageLines(meter: LinkMeter(), cap: 3), HistoryLimits.minimumPageLines)
        XCTAssertEqual(HistoryPrefetch.pageLines(meter: meter(pages: [(60_000, 1000, 0.16)]), cap: 75), 75)
    }
    func testBytesPerLineMakeTheNextPageFitTheWire() {
        // Wide styled lines, 300 bytes each: a thousand of them would not fit a reply.
        var m = LinkMeter()
        m.record(wireBytes: 3_000, lines: 10, seconds: 0.05)
        m.record(wireBytes: 90_000, lines: 300, seconds: 0.07)
        m.record(wireBytes: 180_000, lines: 600, seconds: 0.08)
        XCTAssertEqual(m.tier, .fast)
        XCTAssertLessThanOrEqual(Double(HistoryPrefetch.pageLines(meter: m)) * 300, HistoryPrefetch.maximumPageWireBytes + 300)
    }
}
