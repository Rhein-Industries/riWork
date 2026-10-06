import XCTest
@testable import RiWorkCore

private let shell = "44444444-4444-4444-8444-444444444444"
private let requestID = "55555555-5555-4555-8555-555555555555"
private func parse(_ text: String) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: Data(text.utf8)) }

final class LiveOutputTests: XCTestCase {
    // MARK: replies

    func testAChangedScreenCarriesItsHash() throws {
        let reply = try OutputReply(result: parse(#"{"shell_id":"\#(shell)","output":"hi","hash":"9f2c-77","rows":24,"cols":80,"cursor":{"x":2,"y":0}}"#))
        guard case .screen(let screen) = reply else { return XCTFail("\(reply)") }
        XCTAssertEqual(screen.hash, "9f2c-77")
        XCTAssertEqual(screen.text, "hi")
        XCTAssertEqual(screen.cursor, ShellOutput.Cursor(x: 2, y: 0))
    }
    func testAnUnchangedReplyHasNoOutput() throws {
        let reply = try OutputReply(result: parse(#"{"shell_id":"\#(shell)","unchanged":true,"hash":"abc123"}"#))
        XCTAssertEqual(reply, .unchanged(shellID: shell, hash: "abc123"))
        XCTAssertThrowsError(try OutputReply(result: parse(#"{"shell_id":"\#(shell)","unchanged":true}"#)), "unchanged without the hash it is unchanged from")
        XCTAssertThrowsError(try OutputReply(result: parse(#"{"unchanged":true,"hash":"abc"}"#)))
        // `unchanged: false` (or a non-boolean) is an ordinary screen, and needs its output.
        XCTAssertThrowsError(try OutputReply(result: parse(#"{"shell_id":"\#(shell)","unchanged":false,"hash":"abc"}"#)))
        XCTAssertNoThrow(try OutputReply(result: parse(#"{"shell_id":"\#(shell)","unchanged":false,"output":"x"}"#)))
    }
    func testOlderDesktopsHaveNoHash() throws {
        let reply = try OutputReply(result: parse(#"{"shell_id":"\#(shell)","output":"plain"}"#))
        guard case .screen(let screen) = reply else { return XCTFail() }
        XCTAssertNil(screen.hash)
    }
    func testAnUnusableHashIsIgnoredAndTheScreenSurvives() throws {
        let bad: [String] = [#""""#, #""has space""#, #""tab\there""#, #"123"#, #"null"#, #"true"#, #"{"a":1}"#, #""café""#, "\"" + String(repeating: "a", count: 65) + "\""]
        for hash in bad {
            let screen = try ShellOutput(result: parse(#"{"shell_id":"\#(shell)","output":"keep","hash":\#(hash)}"#))
            XCTAssertNil(screen.hash, hash)
            XCTAssertEqual(screen.text, "keep")
        }
        XCTAssertEqual(try ShellOutput(result: parse(#"{"shell_id":"\#(shell)","output":"x","hash":"\#(String(repeating: "a", count: 64))"}"#)).hash?.count, 64)
    }

    // MARK: requests

    func testRequestParameters() {
        XCTAssertEqual(OutputRequest(shellID: shell, lines: 500).params, ["shell_id": .string(shell), "lines": .number(500), "styled": .bool(true)],
                       "no hash yet: colors are asked for, nothing to wait on")
        let poll = OutputRequest(shellID: shell, lines: 250, ifChanged: "h1", waitMilliseconds: 8000)
        XCTAssertEqual(poll.params, ["shell_id": .string(shell), "lines": .number(250), "styled": .bool(true), "if_changed": .string("h1"), "wait_ms": .number(8000)])
        XCTAssertTrue(poll.isLongPoll)
        let immediate = OutputRequest(shellID: shell, lines: 250, ifChanged: "h1", waitMilliseconds: 0)
        XCTAssertNil(immediate.params["wait_ms"])
        XCTAssertFalse(immediate.isLongPoll)
        XCTAssertNil(OutputRequest(shellID: shell, lines: 1, ifChanged: nil, waitMilliseconds: 8000).params["wait_ms"], "waiting needs a hash to wait from")
        XCTAssertEqual(OutputRequest(shellID: shell, lines: 7, styled: false).params, ["shell_id": .string(shell), "lines": .number(7)], "the plain form an older desktop understands")
        XCTAssertEqual(OutputRequest(shellID: shell, lines: 1, ifChanged: "h", waitMilliseconds: 99_999).waitMilliseconds, 10_000)
        XCTAssertEqual(OutputRequest(shellID: shell, lines: 1, ifChanged: "h", waitMilliseconds: -5).waitMilliseconds, 0)
    }
    func testEveryRequestTheAppBuildsPassesValidation() throws {
        for built in [OutputRequest(shellID: shell, lines: 500), OutputRequest(shellID: shell, lines: 20, ifChanged: "abc-123", waitMilliseconds: LiveSync.waitMilliseconds)] {
            XCTAssertNoThrow(try RequestValidation.validate(method: "shell.output", params: built.params, id: requestID))
        }
    }
    func testValidationRejectsWhatTheContractDoesNot() {
        func rejects(_ extra: [String: JSONValue], _ why: String) {
            var params: [String: JSONValue] = ["shell_id": .string(shell)]
            params.merge(extra) { $1 }
            XCTAssertThrowsError(try RequestValidation.validate(method: "shell.output", params: params, id: requestID), why) { error in
                guard case RemoteError.protocolViolation = error else { return XCTFail("\(why): \(error)") }
            }
        }
        rejects(["styled": .string("yes")], "styled must be a boolean")
        rejects(["styled": .number(1)], "styled must be a boolean")
        rejects(["if_changed": .number(1)], "if_changed must be a string")
        rejects(["if_changed": .string("")], "empty hash")
        rejects(["if_changed": .string("has space")], "hash with a space")
        rejects(["if_changed": .string("line\nbreak")], "hash with a control character")
        rejects(["if_changed": .string(String(repeating: "a", count: 65))], "hash too long")
        rejects(["wait_ms": .number(-1)], "negative wait")
        rejects(["wait_ms": .number(10_001)], "wait beyond 10 s")
        rejects(["wait_ms": .number(1.5)], "fractional wait")
        rejects(["wait_ms": .string("8000")], "wait as text")
        rejects(["wait_ms": .number(.nan)], "wait NaN")
        rejects(["unknown": .bool(true)], "unknown parameter")
        XCTAssertNoThrow(try RequestValidation.validate(method: "shell.output", params: ["shell_id": .string(shell), "if_changed": .string("h"), "wait_ms": .number(0), "styled": .bool(false)], id: requestID))
        XCTAssertNoThrow(try RequestValidation.validate(method: "shell.output", params: ["shell_id": .string(shell), "wait_ms": .number(10_000)], id: requestID))
    }

    // MARK: timeout

    func testALongPollGetsItsWaitPlusTwentySeconds() {
        let long: [String: JSONValue] = ["shell_id": .string(shell), "if_changed": .string("h"), "wait_ms": .number(8000)]
        XCTAssertEqual(RelayClient.timeout(for: "shell.output", params: long, default: .seconds(5)), .seconds(28))
        XCTAssertEqual(RelayClient.timeout(for: "shell.output", params: long, default: .seconds(15)), .seconds(28))
        XCTAssertEqual(RelayClient.timeout(for: "shell.output", params: long, default: .seconds(30)), .seconds(30), "a longer configured timeout is kept")
        XCTAssertEqual(RelayClient.timeout(for: "shell.output", params: ["shell_id": .string(shell), "wait_ms": .number(10_000)], default: .seconds(1)), .seconds(30))
        XCTAssertGreaterThanOrEqual(LiveSync.timeout(waitMilliseconds: 8000), .seconds(8 + 8), "never below the connector's own limit, the wait plus 8 s")
        // Reads that do not wait, and every other method, keep the configured timeout.
        XCTAssertEqual(RelayClient.timeout(for: "shell.output", params: ["shell_id": .string(shell)], default: .seconds(5)), .seconds(5))
        XCTAssertEqual(RelayClient.timeout(for: "shell.output", params: ["shell_id": .string(shell), "wait_ms": .number(0)], default: .seconds(5)), .seconds(5))
        XCTAssertEqual(RelayClient.timeout(for: "projects.list", params: long, default: .seconds(5)), .seconds(5))
        XCTAssertEqual(RelayClient.timeout(for: "shell.keys", params: [:], default: .seconds(1)), .seconds(10))
    }

    func testWhatCountsAsADesktopThatDoesNotKnowTheNewParameters() {
        XCTAssertTrue(LiveSync.rejectsNewParameters(code: "invalid_request", message: "unknown field `styled`, expected `shell_id` or `lines`"))
        XCTAssertTrue(LiveSync.rejectsNewParameters(code: "cli_error", message: "the installed riwork CLI does not support styled output or waiting for changes; update RiWork"))
        XCTAssertFalse(LiveSync.rejectsNewParameters(code: "cli_error", message: "reply too large"))
        XCTAssertFalse(LiveSync.rejectsNewParameters(code: "not_found", message: "Selected session is unavailable."))
        XCTAssertFalse(LiveSync.rejectsNewParameters(code: "response_too_large", message: "styled"))
    }

    // MARK: backoff

    func testBackoffGrowsFromQuarterSecondToTwoSecondsAndResets() {
        var backoff = LongPollBackoff()
        XCTAssertEqual((0..<6).map { _ in backoff.failure() }, [.milliseconds(250), .milliseconds(500), .seconds(1), .seconds(2), .seconds(2), .seconds(2)])
        backoff.success()
        XCTAssertEqual(backoff.failure(), .milliseconds(250))
        var many = LongPollBackoff()
        for _ in 0..<10_000 { _ = many.failure() }
        XCTAssertEqual(many.failure(), .seconds(2), "no overflow however long it fails")
    }

    // MARK: waiting slots

    func testCancelledWaitsKeepHoldingTheirSlotUntilTheyRunOut() {
        var slots = WaitSlots()
        XCTAssertTrue(slots.canWait(at: 0))
        slots.abandon(startedAt: 0, wait: 8)          // still waiting on the desktop until 8.5
        XCTAssertTrue(slots.canWait(at: 1), "one cancelled wait and one new long poll are the two the desktop allows")
        slots.abandon(startedAt: 1, wait: 8)          // until 9.5
        XCTAssertFalse(slots.canWait(at: 2), "a third would be refused")
        XCTAssertEqual(slots.stillWaiting(at: 2), 2)
        XCTAssertFalse(slots.canWait(at: 8.4))
        XCTAssertTrue(slots.canWait(at: 8.6), "the first one has run out")
        XCTAssertEqual(slots.stillWaiting(at: 8.6), 1)
        XCTAssertTrue(slots.canWait(at: 30))
        XCTAssertEqual(slots.stillWaiting(at: 30), 0)
        slots.abandon(startedAt: 30, wait: 8); slots.abandon(startedAt: 30, wait: 8)
        slots.reset()
        XCTAssertTrue(slots.canWait(at: 31), "a new connection starts clean")
    }

    // MARK: latency

    func testRollingAverageKeepsTheLastTwentySamples() {
        var series = RollingAverage(capacity: LatencyBook.window)
        XCTAssertNil(series.average)
        XCTAssertNil(series.last)
        for value in 1...20 { series.add(Double(value)) }
        XCTAssertEqual(series.count, 20)
        XCTAssertEqual(series.average, 10.5)
        series.add(100)
        XCTAssertEqual(series.count, 20)
        XCTAssertEqual(series.last, 100)
        XCTAssertEqual(series.average, (Double((2...20).reduce(0, +)) + 100) / 20, "the oldest sample dropped out")
        for _ in 0..<40 { series.add(10) }
        XCTAssertEqual(series.average, 10)
        series.add(.nan); series.add(-1); series.add(.infinity)
        XCTAssertEqual(series.last, 10, "unusable samples are ignored")
        series.reset()
        XCTAssertNil(series.average)
    }
    func testKeysAndOutputRoundTrips() {
        var book = LatencyBook()
        book.keysAnswered(seconds: 0.040)
        book.keysAnswered(seconds: 0.060)
        XCTAssertEqual(book.keys.last ?? 0, 60, accuracy: 0.001)
        XCTAssertEqual(book.keys.average ?? 0, 50, accuracy: 0.001)
        // A read that did not wait is a round trip as it is.
        book.outputAnswered(seconds: 0.030, waited: 0, unchanged: false, bytes: 2048)
        XCTAssertEqual(book.output.last ?? 0, 30, accuracy: 0.001)
        XCTAssertEqual(book.payloadBytes, 2048)
        // A long poll that came back changed says nothing about the round trip: it was waiting for the screen.
        book.outputAnswered(seconds: 3.2, waited: 8, unchanged: false, bytes: 4096)
        XCTAssertEqual(book.output.count, 1)
        XCTAssertEqual(book.payloadBytes, 4096)
        // One that ran out its wait: the time past the wait is the link and the desktop's own delay.
        book.outputAnswered(seconds: 8.052, waited: 8, unchanged: true, bytes: nil)
        XCTAssertEqual(book.output.last ?? 0, 52, accuracy: 0.001)
        XCTAssertEqual(book.payloadBytes, 4096, "an unchanged answer carries no screen")
        book.outputAnswered(seconds: 7.5, waited: 8, unchanged: true, bytes: nil)
        XCTAssertEqual(book.output.count, 2, "an answer earlier than the wait cannot be a round trip")
    }
    func testEchoLatencyRunsFromTheKeysToTheFirstChangedScreenAfterThem() {
        var book = LatencyBook()
        book.screenChanged(at: 10)
        XCTAssertEqual(book.echo.count, 0, "a change nobody typed for is not an echo")
        XCTAssertEqual(book.lastChangeAt, 10)
        book.keysSent(at: 20.000)
        book.keysSent(at: 20.050)
        book.screenChanged(at: 20.120)
        XCTAssertEqual(book.echo.last ?? 0, 120, accuracy: 0.001, "measured from the first batch that had not been echoed")
        book.screenChanged(at: 20.300)
        XCTAssertEqual(book.echo.count, 1, "one echo per typing burst")
        book.keysSent(at: 30)
        book.screenChanged(at: 30.080)
        XCTAssertEqual(book.echo.count, 2)
        XCTAssertEqual(book.echo.average ?? 0, 100, accuracy: 0.001)
        // A change that comes long after is unrelated; the next batch starts a fresh clock.
        book.keysSent(at: 40)
        book.screenChanged(at: 47)
        XCTAssertEqual(book.echo.count, 2)
        book.keysSent(at: 50)
        book.screenChanged(at: 50.2)
        XCTAssertEqual(book.echo.last ?? 0, 200, accuracy: 0.001)
        // An echo that never came must not swallow the next one.
        book.keysSent(at: 60)
        book.keysSent(at: 70)
        book.screenChanged(at: 70.1)
        XCTAssertEqual(book.echo.last ?? 0, 100, accuracy: 0.001)
    }
    func testAgeAndFormatting() {
        var book = LatencyBook()
        XCTAssertNil(book.age(at: 5))
        book.screenChanged(at: 100)
        XCTAssertEqual(book.age(at: 100.4) ?? 0, 0.4, accuracy: 0.0001)
        XCTAssertEqual(book.age(at: 99) ?? -1, 0, "never negative")
        XCTAssertEqual(LatencyBook.format(milliseconds: nil), "–")
        XCTAssertEqual(LatencyBook.format(milliseconds: 4.26), "4.3")
        XCTAssertEqual(LatencyBook.format(milliseconds: 45.4), "45")
        XCTAssertEqual(LatencyBook.format(age: 0.44), "0.4s")
        XCTAssertEqual(LatencyBook.format(age: 42), "42s")
        XCTAssertEqual(LatencyBook.format(age: 300), "5m")
        XCTAssertEqual(LatencyBook.format(bytes: 900), "900 B")
        XCTAssertEqual(LatencyBook.format(bytes: 3072), "3.0 KB")
        XCTAssertEqual(LatencyBook.format(bytes: nil), "–")
        var series = RollingAverage(capacity: 20)
        XCTAssertEqual(LatencyBook.format(series), "–")
        series.add(40); series.add(60)
        XCTAssertEqual(LatencyBook.format(series), "60 ms (avg 50)")
        book.reset()
        XCTAssertNil(book.lastChangeAt)
        XCTAssertEqual(SyncMode.live.label, "live")
        XCTAssertEqual(SyncMode.poll.label, "poll")
    }

    // MARK: interface scale

    func testInterfaceScaleIsEightyToOneThirtyPercentInFivePercentSteps() {
        XCTAssertEqual(InterfaceScale.range, 0.80...1.30)
        XCTAssertEqual(InterfaceScale.standard, 1.0)
        XCTAssertEqual(InterfaceScale.clamped(1.0), 1.0)
        XCTAssertEqual(InterfaceScale.clamped(0.5), 0.80)
        XCTAssertEqual(InterfaceScale.clamped(2), 1.30)
        XCTAssertEqual(InterfaceScale.clamped(1.07), 1.05)
        XCTAssertEqual(InterfaceScale.clamped(1.08), 1.10)
        XCTAssertEqual(InterfaceScale.clamped(.nan), 1.0)
        XCTAssertEqual(InterfaceScale.clamped(.infinity), 1.0)
        XCTAssertEqual(InterfaceScale.stepped(1.0, by: 1), 1.05)
        XCTAssertEqual(InterfaceScale.stepped(1.0, by: -4), 0.80)
        XCTAssertEqual(InterfaceScale.stepped(1.30, by: 1), 1.30)
        XCTAssertEqual(InterfaceScale.stepped(0.80, by: -1), 0.80)
        XCTAssertEqual(InterfaceScale.percent(0.85), 85)
        XCTAssertEqual(InterfaceScale.percent(1.3), 130)
        let steps = stride(from: 80, through: 130, by: 5).map { Double($0) / 100 }
        XCTAssertEqual(steps.count, 11)
        for step in steps { XCTAssertEqual(InterfaceScale.clamped(step), step) }
    }
    func testScaledLengthsAreWholePoints() {
        XCTAssertEqual(InterfaceScale.scaled(44, by: 1.0), 44)
        XCTAssertEqual(InterfaceScale.scaled(44, by: 1.3), 57)
        XCTAssertEqual(InterfaceScale.scaled(44, by: 0.8), 35)
        XCTAssertEqual(InterfaceScale.scaled(44, by: 9), 57, "an out-of-range scale is clamped first")
        XCTAssertEqual(KeyBarGeometry.height(scale: 1.0), KeyBarGeometry.height)
        XCTAssertEqual(KeyBarGeometry.height(scale: 1.3), 57)
        XCTAssertEqual(KeyBarGeometry.height(scale: 0.8), 44, "the bar's keys stay 44-point targets at the smallest size")
    }
}
