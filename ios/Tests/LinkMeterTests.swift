import XCTest
@testable import RiWorkCore

/// The link meter on traces that look like the real thing: a link with a round trip, a rate and jitter, a desktop that takes time of its
/// own per request and per line, replies that may be compressed, live replies that share the socket.
final class LinkMeterTests: XCTestCase {
    /// A synthetic link and desktop. `page` and `small` are the replies the phone would time, on a clock that runs on by itself.
    private struct Link {
        /// The network's round trip, and how much each reply's can be worse (uniform).
        var rtt = 0.05, jitter = 0.0
        /// Wire bytes per second.
        var rate = 2_000_000.0
        /// The desktop's own time: per request, and per line of a page (a loaded machine spawning its CLI and asking tmux).
        var desktopBase = 0.12, desktopPerLine = 0.0002
        /// JSON bytes per line, and how much compression shrinks them (nil: not compressed).
        var jsonPerLine = 60.0
        var ratio: Double?
        var reportsServerTime = true
        var seed: UInt64 = 7
        var clock = 100.0

        mutating func random() -> Double {
            seed = seed &* 6364136223846793005 &+ 1442695040888963407
            return Double(seed >> 11) / Double(1 << 53)
        }
        func wire(json: Double) -> Int { Int(json / (ratio ?? 1) * 4 / 3 + 200) }
        /// A page of `lines` lines. `concurrent` bytes of other replies share the socket while it is out: they delay it by their own
        /// transfer time, which is in `elapsed` but not in the page's bytes.
        mutating func page(lines: Int, concurrent: Int = 0) -> (sample: LinkSample, at: Double) {
            let json = Double(lines) * jsonPerLine
            let wireBytes = wire(json: json)
            let server = desktopBase + desktopPerLine * Double(lines)
            let elapsed = rtt + jitter * random() + server + Double(wireBytes + concurrent) / rate
            clock += elapsed
            let sample = LinkSample(wireBytes: wireBytes, jsonBytes: Int(json), sealedBytes: Int(json / (ratio ?? 1)), lines: lines, elapsed: elapsed,
                                    serverSeconds: reportsServerTime ? server : nil, concurrentBytes: concurrent, compressed: ratio != nil)
            defer { clock += 0.1 }
            return (sample, clock)
        }
        /// A key batch's acknowledgement: 400 bytes, a little desktop time.
        mutating func small() -> (timing: ReplyTiming, at: Double) {
            let server = 0.01
            let elapsed = rtt + jitter * random() + server + 400 / rate
            clock += elapsed + 0.3
            return (ReplyTiming(elapsed: elapsed, serverSeconds: reportsServerTime ? server : nil, wireBytes: 400), clock)
        }
    }
    private func warmedUp(_ link: inout Link, rounds: Int = 5) -> LinkMeter {
        var meter = LinkMeter()
        for _ in 0..<rounds { let reply = link.small(); meter.observe(reply.timing, at: reply.at) }
        return meter
    }
    private func feed(_ meter: inout LinkMeter, _ link: inout Link, pages: Int, lines: Int, concurrent: Int = 0) {
        for _ in 0..<pages { let page = link.page(lines: lines, concurrent: concurrent); meter.record(page.sample, at: page.at) }
    }

    // MARK: The desktop's time is not the link's

    func testAFastLinkBehindASlowDesktopIsFastNotGood() {
        // 3 MB/s and 40 ms away, but the desktop spends 120 ms and 0.2 ms a line: a 1,000-line page costs it 320 ms.
        var link = Link(rtt: 0.04, rate: 3_000_000)
        var meter = warmedUp(&link)
        feed(&meter, &link, pages: 5, lines: 1000)
        XCTAssertEqual(meter.tier, .fast)
        XCTAssertEqual(meter.bytesPerSecond ?? 0, 3_000_000, accuracy: 300_000)
        XCTAssertEqual(meter.desktopSeconds ?? 0, 0.32, accuracy: 0.01)
        XCTAssertEqual(meter.roundTripSeconds ?? 0, 0.04, accuracy: 0.005)

        // The same trace as an older desktop would show it: no `server_ms`, one ten-line probe for the fixed cost. The probe costs the
        // desktop 120 ms, a thousand lines 320: the 200 ms the page spent on the desktop is counted as transfer.
        var old = Link(rtt: 0.04, rate: 3_000_000); old.reportsServerTime = false
        var legacy = LinkMeter()
        let probe = old.page(lines: 10); legacy.record(probe.sample, at: probe.at)
        feed(&legacy, &old, pages: 5, lines: 1000)
        XCTAssertLessThan(legacy.tier, .fast, "this is what the phone used to conclude about the same link: \(legacy.bytesPerSecond ?? 0) B/s")
        XCTAssertEqual(legacy.tier, .good)
    }
    func testASlowLinkIsSlowWhateverTheDesktopDoes() {
        var link = Link(rtt: 0.08, rate: 100_000, desktopBase: 0.03, desktopPerLine: 0.00001)
        var meter = warmedUp(&link)
        feed(&meter, &link, pages: 5, lines: 200)   // 12 KB of text
        XCTAssertEqual(meter.tier, .slow)
        XCTAssertEqual(meter.bytesPerSecond ?? 0, 100_000, accuracy: 15_000)
    }
    func testSmallRepliesGiveTheRoundTripSoNoProbeIsNeeded() {
        var link = Link(rtt: 0.12)
        var meter = LinkMeter()
        XCTAssertFalse(meter.knowsRequestCost)
        let reply = link.small()
        meter.observe(reply.timing, at: reply.at)
        XCTAssertTrue(meter.knowsRequestCost, "one acknowledgement is a clean sample of the round trip")
        XCTAssertEqual(meter.roundTripSeconds ?? 0, 0.12 + 400 / 2_000_000, accuracy: 0.001)
        // A big reply, or one from a desktop that does not time itself, says nothing about the round trip.
        var other = LinkMeter()
        other.observe(ReplyTiming(elapsed: 0.5, serverSeconds: 0.1, wireBytes: 60_000), at: 1)
        other.observe(ReplyTiming(elapsed: 0.2, serverSeconds: nil, wireBytes: 300), at: 2)
        XCTAssertFalse(other.knowsRequestCost)
    }
    func testWithoutServerTimeTheProbeStillTeachesTheFixedCost() {
        var link = Link(rtt: 0.05, rate: 1_000_000); link.reportsServerTime = false
        var meter = LinkMeter()
        let probe = link.page(lines: 10)
        meter.record(probe.sample, at: probe.at)
        XCTAssertTrue(meter.knowsRequestCost)
        XCTAssertNotNil(meter.fixedSeconds)
        XCTAssertNil(meter.roundTripSeconds)
        feed(&meter, &link, pages: 3, lines: 1000)
        XCTAssertNotEqual(meter.tier, .unknown)
    }
    func testAGapIsAShareOfWhatTheLinkSpentNotOfWhatTheDesktopDid() {
        var link = Link(rtt: 0.04, rate: 3_000_000, desktopBase: 0.4, desktopPerLine: 0)
        var meter = warmedUp(&link)
        feed(&meter, &link, pages: 3, lines: 1000)
        XCTAssertEqual(meter.tier, .fast)
        XCTAssertEqual(meter.lastSeconds ?? 0, 0.46, accuracy: 0.02)
        XCTAssertEqual(meter.lastTransferSeconds ?? 1, 0.02, accuracy: 0.01)
        XCTAssertEqual(HistoryPrefetch.gapAfterPage(meter: meter, liveBusy: false), 0.03, accuracy: 0.001, "fast links are used nearly back to back, whatever the desktop took")
    }

    // MARK: Jitter, and a link that sits on a threshold

    func testJitterOnASteadyFastLinkDoesNotMoveTheTier() {
        var link = Link(rtt: 0.03, jitter: 0.06, rate: 2_500_000)
        var meter = warmedUp(&link, rounds: 8)
        feed(&meter, &link, pages: 80, lines: 1400)   // 84 KB
        XCTAssertEqual(meter.tier, .fast)
        XCTAssertLessThanOrEqual(meter.tierChanges, 2, "unknown to fast, and at most one wobble")
    }
    func testALinkSittingOnAThresholdChangesTierFarLessOftenThanItsRateCrossesIt() {
        var link = Link(rtt: 0.04, jitter: 0.05, rate: 1_000_000, desktopBase: 0.05, desktopPerLine: 0)
        var meter = warmedUp(&link, rounds: 8)
        var crossings = 0
        var previous: Bool?
        for _ in 0..<150 {
            let page = link.page(lines: 1300)
            meter.record(page.sample, at: page.at)
            guard let rate = meter.bytesPerSecond else { continue }
            let above = rate >= LinkMeter.fastBytesPerSecond
            if let previous, previous != above { crossings += 1 }
            previous = above
        }
        XCTAssertGreaterThanOrEqual(crossings, 6, "the rate really does cross the threshold again and again: \(crossings)")
        XCTAssertLessThan(meter.tierChanges, crossings / 3, "tier changes: \(meter.tierChanges), crossings \(crossings)")
    }
    func testBetweenTheThresholdsATierIsKeptInBothDirections() {
        var meter = LinkMeter()
        func pages(_ rate: Double, count: Int) {
            // 0.1 s of round trip (as `observe` below) and 0.1 s of the desktop's, then the transfer.
            for _ in 0..<count { meter.record(LinkSample(wireBytes: 80_000, lines: 1000, elapsed: 0.2 + 80_000 / rate, serverSeconds: 0.1), at: Double(meter.pages) * 0.5) }
        }
        meter.observe(ReplyTiming(elapsed: 0.1, serverSeconds: 0, wireBytes: 300), at: 0)
        pages(1_500_000, count: 4)
        XCTAssertEqual(meter.tier, .fast)
        pages(850_000, count: 30)
        XCTAssertEqual(meter.tier, .fast, "850 KB/s is under the entry threshold but over the band that leaves fast")
        pages(500_000, count: 5)
        XCTAssertEqual(meter.tier, .good)
        pages(900_000, count: 30)
        XCTAssertEqual(meter.tier, .good, "900 KB/s does not climb to fast: that takes 1 MB/s")
        pages(200_000, count: 5)
        XCTAssertEqual(meter.tier, .good, "200 KB/s is under 250 but over the band that leaves good")
        pages(100_000, count: 5)
        XCTAssertEqual(meter.tier, .slow)
        pages(300_000, count: 3)
        XCTAssertEqual(meter.tier, .good)
        XCTAssertEqual(meter.tierChanges, 4, "unknown to fast, to good, to slow, to good")
    }
    func testALinkThatTurnsSlowIsNoticedAfterThreePagesNotOne() {
        var link = Link(rtt: 0.04, rate: 3_000_000, desktopPerLine: 0)
        var meter = warmedUp(&link)
        feed(&meter, &link, pages: 4, lines: 1000)
        XCTAssertEqual(meter.tier, .fast)
        link.rate = 80_000
        feed(&meter, &link, pages: 1, lines: 150)
        XCTAssertEqual(meter.tier, .fast, "one stalled page proves little")
        feed(&meter, &link, pages: 2, lines: 150)
        XCTAssertEqual(meter.tier, .slow)
    }
    func testRatesFromBeforeTheLastFewSecondsAreForgotten() {
        var meter = LinkMeter()
        meter.observe(ReplyTiming(elapsed: 0.05, serverSeconds: 0, wireBytes: 300), at: 0)
        meter.record(LinkSample(wireBytes: 80_000, lines: 1000, elapsed: 0.1, serverSeconds: 0.04), at: 1)   // 80 KB in 0.01 s
        XCTAssertEqual(meter.tier, .fast)
        // A minute later the link is slow; the fast sample has aged out and cannot hold the rate up.
        meter.record(LinkSample(wireBytes: 20_000, lines: 300, elapsed: 0.45, serverSeconds: 0.04), at: 70)
        XCTAssertEqual(meter.bytesPerSecond ?? 0, 20_000 / 0.36, accuracy: 3_000)
        XCTAssertEqual(meter.tier, .slow)
    }

    func testAProbeWhoseLinesAreTooWideToBeSmallStillTeachesTheFixedCost() {
        // A desktop that does not report its time, and lines of 600 bytes: the ten-line probe weighs 6 KB, over the 4 KB that is "small".
        var meter = LinkMeter()
        meter.record(LinkSample(wireBytes: 6_000, lines: HistoryPrefetch.probeLines, elapsed: 0.2), at: 1)
        XCTAssertEqual(meter.fixedSeconds ?? 0, 0.2, accuracy: 0.0001)
        XCTAssertTrue(meter.knowsRequestCost, "so the next decision is a page, not another probe")
        guard case .fetch(let lines, _) = HistoryPrefetch.decide({ var input = HistoryPrefetch.Input(); input.meter = meter; input.aboveReader = 50_000; return input }()) else { return XCTFail() }
        XCTAssertGreaterThan(lines, HistoryPrefetch.probeLines)
        // A bigger page is not a probe.
        var other = LinkMeter()
        other.record(LinkSample(wireBytes: 60_000, lines: 100, elapsed: 0.5), at: 1)
        XCTAssertNil(other.fixedSeconds)
    }
    func testWhatALiveAnswerSaysAboutALineIsReplacedByTheFirstPageNotBlendedWithIt() {
        var meter = LinkMeter()
        meter.noteLines(wireBytes: 50_000, jsonBytes: 60_000, lines: 500)
        XCTAssertEqual(meter.bytesPerLine ?? 0, 100, accuracy: 0.001)
        meter.noteLines(wireBytes: 90_000, jsonBytes: 90_000, lines: 500)
        XCTAssertEqual(meter.bytesPerLine ?? 0, 180, accuracy: 0.001, "until a page says, the latest live answer is the idea")
        meter.record(LinkSample(wireBytes: 20_000, lines: 1000, elapsed: 0.2, serverSeconds: 0.05), at: 1)
        XCTAssertEqual(meter.bytesPerLine ?? 0, 20, accuracy: 0.001, "a page replaces it outright")
        meter.noteLines(wireBytes: 500_000, jsonBytes: 500_000, lines: 500)
        XCTAssertEqual(meter.bytesPerLine ?? 0, 20, accuracy: 0.001, "and a live answer no longer counts")
        meter.record(LinkSample(wireBytes: 40_000, lines: 1000, elapsed: 0.2, serverSeconds: 0.05), at: 2)
        XCTAssertEqual(meter.bytesPerLine ?? 0, 20 + (40 - 20) * 0.4, accuracy: 0.001, "after that it is smoothed")
        // A change of the setting that decides what a line weighs starts it over.
        meter.forgetContent()
        XCTAssertNil(meter.bytesPerLine); XCTAssertNil(meter.jsonBytesPerLine); XCTAssertNil(meter.compressionRatio)
        meter.noteLines(wireBytes: 5_000, jsonBytes: 50_000, lines: 500)
        XCTAssertEqual(meter.bytesPerLine ?? 0, 10, accuracy: 0.001)
    }

    // MARK: Traffic that shares the socket

    func testAPageThatSharedTheSocketWithABigLiveReplyIsLeftOutNotCountedAsSlow() {
        // 1.2 MB/s: fast. An 80 KB page takes 67 ms. A 500-line live answer (60 KB) arrives meanwhile and costs the page 50 ms more.
        var link = Link(rtt: 0.04, rate: 1_200_000, desktopBase: 0.05, desktopPerLine: 0)
        var meter = warmedUp(&link)
        feed(&meter, &link, pages: 3, lines: 1330)
        XCTAssertEqual(meter.tier, .fast)
        feed(&meter, &link, pages: 6, lines: 1330, concurrent: 60_000)
        XCTAssertEqual(meter.tier, .fast, "taking the 50 ms for transfer it would be 80 KB over 117 ms: 680 KB/s, good")
        XCTAssertEqual(meter.contended, 6)
        XCTAssertEqual(meter.bytesPerSecond ?? 0, 1_200_000, accuracy: 150_000)
    }
    func testAnOverlapThatIsSmallNextToThePageIsAddedBackToItsBytes() {
        var link = Link(rtt: 0.04, rate: 600_000, desktopBase: 0.05, desktopPerLine: 0)
        var meter = warmedUp(&link)
        // 10 KB of other replies on an 80 KB page: 12 %, under a fifth.
        feed(&meter, &link, pages: 4, lines: 1330, concurrent: 10_000)
        XCTAssertEqual(meter.contended, 0)
        XCTAssertEqual(meter.bytesPerSecond ?? 0, 600_000, accuracy: 60_000, "the whole 90 KB moved in that time")
        XCTAssertEqual(meter.tier, .good)
    }
    func testAPageThatAlwaysSharesTheSocketLeavesTheLastKnownTierStanding() {
        var link = Link(rtt: 0.04, rate: 2_000_000, desktopPerLine: 0)
        var meter = warmedUp(&link)
        feed(&meter, &link, pages: 3, lines: 1000)
        XCTAssertEqual(meter.tier, .fast)
        feed(&meter, &link, pages: 20, lines: 1000, concurrent: 100_000)
        XCTAssertEqual(meter.tier, .fast)
        XCTAssertEqual(meter.contended, 20)
    }

    // MARK: Path changes

    func testAPathChangeForgetsTheLinkAndKeepsWhatIsKnownAboutTheContent() {
        var wifi = Link(rtt: 0.02, rate: 4_000_000, ratio: 8)
        var meter = warmedUp(&wifi)
        feed(&meter, &wifi, pages: 4, lines: 2000)
        XCTAssertEqual(meter.tier, .fast)
        let bytesPerLine = meter.bytesPerLine, ratio = meter.compressionRatio, desktop = meter.desktopSeconds
        XCTAssertNotNil(bytesPerLine); XCTAssertEqual(ratio ?? 0, 8, accuracy: 0.1)
        meter.pathChanged()
        XCTAssertEqual(meter.tier, .unknown)
        XCTAssertEqual(meter.pages, 0)
        XCTAssertNil(meter.roundTripSeconds)
        XCTAssertNil(meter.bytesPerSecond)
        XCTAssertFalse(meter.knowsRequestCost)
        XCTAssertEqual(meter.bytesPerLine, bytesPerLine, "a line of this history weighs what it weighed")
        XCTAssertEqual(meter.compressionRatio, ratio)
        XCTAssertEqual(meter.desktopSeconds, desktop)
        // Cellular: much slower, measured again from the first replies, no probe.
        var cellular = Link(rtt: 0.09, rate: 150_000, ratio: 8); cellular.clock = wifi.clock + 5
        let reply = cellular.small(); meter.observe(reply.timing, at: reply.at)
        XCTAssertTrue(meter.knowsRequestCost)
        feed(&meter, &cellular, pages: 4, lines: 2000)
        XCTAssertEqual(meter.tier, .slow)
        XCTAssertEqual(meter.tierChanges, 1)
    }
    func testAResetForgetsEverything() {
        var link = Link()
        var meter = warmedUp(&link)
        feed(&meter, &link, pages: 3, lines: 1000)
        meter.reset()
        XCTAssertEqual(meter, LinkMeter())
    }
    func testNonsenseSamplesAreIgnored() {
        var link = Link()
        var meter = warmedUp(&link)
        feed(&meter, &link, pages: 3, lines: 1000)
        let before = meter
        meter.record(LinkSample(wireBytes: 100, lines: 0, elapsed: 1), at: 1)
        meter.record(LinkSample(wireBytes: 100, lines: 5, elapsed: 0), at: 1)
        meter.record(LinkSample(wireBytes: 100, lines: 5, elapsed: .nan), at: 1)
        meter.record(LinkSample(wireBytes: -1, lines: 5, elapsed: 1), at: 1)
        meter.observe(ReplyTiming(elapsed: .nan, serverSeconds: 0.1, wireBytes: 100), at: 1)
        XCTAssertEqual(meter, before)
    }

    // MARK: Compression on and off

    func testCompressionDoesNotChangeWhatTheLinkIsOnlyHowManyLinesAPageCarries() {
        var plain = Link(rtt: 0.05, rate: 500_000, desktopPerLine: 0.00002)
        var packed = Link(rtt: 0.05, rate: 500_000, desktopPerLine: 0.00002, ratio: 8)
        var plainMeter = warmedUp(&plain), packedMeter = warmedUp(&packed)
        let plainFirst = HistoryPrefetch.pageLines(meter: plainMeter, maximumLines: 5000), packedFirst = HistoryPrefetch.pageLines(meter: packedMeter, maximumLines: 5000)
        XCTAssertEqual(plainFirst, packedFirst, "nothing is known yet: the same first page")
        // Pages as the loop would ask for them, until each has the 50,000 lines.
        func download(_ link: inout Link, _ meter: inout LinkMeter, maximumLines: Int) -> (requests: Int, seconds: Double, wireBytes: Int) {
            var lines = 0, requests = 0, seconds = 0.0, bytes = 0
            while lines < 50_000 {
                let next = min(50_000 - lines, HistoryPrefetch.pageLines(meter: meter, maximumLines: maximumLines))
                let page = link.page(lines: next)
                meter.record(page.sample, at: page.at)
                lines += next; requests += 1; seconds += page.sample.elapsed; bytes += page.sample.wireBytes
            }
            return (requests, seconds, bytes)
        }
        let before = download(&plain, &plainMeter, maximumLines: 1000)   // the desktop of 2026-09-30: 1,000 lines a page, no compression
        let after = download(&packed, &packedMeter, maximumLines: 5000)
        XCTAssertEqual(plainMeter.tier, packedMeter.tier, "the same link either way")
        XCTAssertEqual(packedMeter.compressionRatio ?? 0, 8, accuracy: 0.2)
        XCTAssertNil(plainMeter.compressionRatio)
        XCTAssertGreaterThanOrEqual(before.requests, 50)
        XCTAssertLessThanOrEqual(after.requests, 16, "a handful: \(after.requests)")
        XCTAssertLessThan(Double(after.wireBytes) * 5, Double(before.wireBytes), "wire bytes: \(after.wireBytes) against \(before.wireBytes)")
        XCTAssertLessThan(after.seconds * 4, before.seconds, "\(after.seconds) s against \(before.seconds) s")
    }
    func testCompressedPagesGrowToWhatTheWireAndThePhoneCanTakeAndNoFurther() {
        var link = Link(rtt: 0.03, rate: 5_000_000, desktopPerLine: 0.00002, ratio: 12)
        var meter = warmedUp(&link)
        var sizes: [Int] = [], measurable: [Bool] = []
        for _ in 0..<10 {
            let lines = HistoryPrefetch.pageLines(meter: meter, maximumLines: 5000)
            sizes.append(lines)
            let page = link.page(lines: lines)
            measurable.append(page.sample.wireBytes >= LinkMeter.measurableBytes)
            meter.record(page.sample, at: page.at)
            XCTAssertLessThanOrEqual(Double(page.sample.wireBytes), HistoryPrefetch.maximumPageWireBytes * 1.3, "\(lines) lines: \(page.sample.wireBytes) bytes")
            XCTAssertLessThanOrEqual(Double(page.sample.jsonBytes), HistoryPrefetch.maximumPageJSONBytes * 1.3)
        }
        XCTAssertEqual(meter.tier, .fast)
        XCTAssertEqual(sizes, sizes.sorted(), "pages only grow while the link keeps up: \(sizes)")
        XCTAssertGreaterThanOrEqual(sizes.last ?? 0, 4000, "pages reach thousands of lines: \(sizes)")
        XCTAssertLessThanOrEqual(sizes.max() ?? 0, 5000)
        // Until a page is big enough to measure, the next is sized to be (so compressed pages do not crawl up from 300 lines).
        for (index, pair) in zip(sizes, sizes.dropFirst()).enumerated() where measurable[index] {
            XCTAssertLessThanOrEqual(pair.1, max(2 * pair.0, HistoryLimits.pageLines), "at most double once there is a rate: \(sizes)")
        }
        XCTAssertLessThanOrEqual(HistoryPrefetch.pageLines(meter: meter, maximumLines: 1000), 1000, "a desktop that takes 1,000 lines at most is asked for no more")
    }
}
