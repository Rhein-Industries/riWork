import XCTest
import Foundation
#if canImport(Darwin)
import Darwin
#endif
@testable import RiWorkCore

/// Benchmarks of the work one live answer causes, on a phone-sized terminal (50 columns, 40 rows) with realistic content
/// (`SyntheticTranscript`). They print `PERF …` lines and assert only what is deterministic (the work done, the bytes held); the
/// times are for reading. Run optimized, as the app ships: `swift test -c release --filter PerformanceTests`.
final class PerformanceTests: XCTestCase {
    private let rows = 40

    /// Median wall time of `body`, in milliseconds, over `runs` runs of `inner` calls each (divided back to one call).
    @discardableResult
    private func bench(_ name: String, runs: Int = 9, inner: Int = 20, _ body: () -> Void) -> Double {
        for _ in 0..<3 { body() }
        var samples: [Double] = []
        let clock = ContinuousClock()
        for _ in 0..<runs {
            let start = clock.now
            for _ in 0..<inner { body() }
            let d = (clock.now - start).components
            samples.append((Double(d.seconds) * 1000 + Double(d.attoseconds) / 1e15) / Double(inner))
        }
        samples.sort()
        let median = samples[samples.count / 2]
        print(String(format: "PERF %-46@ %9.4f ms  (min %.4f)", name as NSString, median, samples[0]))
        return median
    }

    /// The screen text of a live answer that reaches back `scrollback` lines: the lines joined by line breaks, a break after the last.
    private func answerText(scrollback: Int, first: Int = 100_000, edit: Int = 0) -> String {
        var lines = SyntheticTranscript.lines(first, scrollback + rows)
        if edit > 0 { lines[lines.count - 1] += String(repeating: "x", count: edit) }
        return lines.joined(separator: "\n") + "\n"
    }

    // MARK: Parsing a live answer

    func testParseOfALiveAnswer() {
        let short = answerText(scrollback: 120)
        let long = answerText(scrollback: 500)
        var parsed = StyledScreen.empty
        bench("parse 120+40 lines (live answer)") { parsed = TerminalText.styledScreen(short, cursor: (3, rows - 1), rows: rows) }
        XCTAssertEqual(parsed.lines.count, 160)
        bench("parse 500+40 lines (first answer / full history)", inner: 5) { parsed = TerminalText.styledScreen(long, cursor: (3, rows - 1), rows: rows) }
        XCTAssertEqual(parsed.lines.count, 540)
        print("PERF bytes per answer: 120+40 lines \(short.utf8.count), 500+40 lines \(long.utf8.count)")
    }

    /// Where the parse spends its time: scanning the characters, or building lines and the flat form.
    func testWhereTheParseSpendsItsTime() {
        let text = answerText(scrollback: 120)
        var scanned = TerminalText.scanStyled(text, keepCells: true, textPresentation: true)
        bench("  scanStyled alone (160 lines)") { scanned = TerminalText.scanStyled(text, keepCells: true, textPresentation: true) }
        XCTAssertGreaterThan(scanned.rows.count, 100)
        bench("  scanStyled without text presentation") { scanned = TerminalText.scanStyled(text, keepCells: true, textPresentation: false) }
        var scalars: [Unicode.Scalar] = []
        bench("  Array(unicodeScalars) alone") { scalars = Array(text.unicodeScalars) }
        XCTAssertGreaterThan(scalars.count, 1000)
    }

    func testParseOfAHistoryPage() {
        let page = SyntheticTranscript.lines(0, 1000).joined(separator: "\n")
        var lines: [StyledLine] = []
        bench("parse 1000-line history page", inner: 3) { lines = TerminalText.styledLines(page: page, expecting: 1000) }
        XCTAssertEqual(lines.count, 1000)
    }

    // MARK: Comparing screens

    /// The model assigns a new `StyledScreen` and its text to observed properties; the macro compares old and new first (to wake
    /// observers only for a real change). This is what that costs when the new screen differs from the old only in its last line.
    func testComparingTwoScreens() {
        let a = TerminalText.styledScreen(answerText(scrollback: 120), cursor: (3, rows - 1), rows: rows)
        let b = TerminalText.styledScreen(answerText(scrollback: 120, edit: 1), cursor: (3, rows - 1), rows: rows)
        let c = TerminalText.styledScreen(answerText(scrollback: 120, edit: 1), cursor: (3, rows - 1), rows: rows)
        var different = false
        bench("StyledScreen != (last line differs)", inner: 40) { different = a != b }
        XCTAssertTrue(different)
        var same = false
        bench("StyledScreen == (equal, separate storage)", inner: 40) { same = b == c }
        XCTAssertTrue(same)
        bench("flat text != (last line differs)", inner: 40) { different = a.text != b.text }
        bench("flat text == (equal, separate storage)", inner: 40) { same = b.text == c.text }
        bench("lines == (equal, separate storage)", inner: 40) { same = b.lines == c.lines }
        let asciiA = String(repeating: "abcdefghij", count: 800), asciiB = String(repeating: "abcdefghij", count: 799) + "abcdefghik"
        bench("8 KB ASCII != (differs at the end)", inner: 40) { different = asciiA != asciiB }
        print("PERF flat text is \(a.text.utf8.count) bytes, \(a.text.unicodeScalars.filter { !$0.isASCII }.count) non-ASCII scalars")
    }

    // MARK: A buffer of 50,000 lines

    /// A buffer holding `history` lines of scrollback above a 40-row screen, built the way the app builds it: a live answer, then pages.
    private func filledBuffer(history: Int) -> TerminalBuffer {
        var buffer = TerminalBuffer()
        let size = history
        let tail = tailLines(historySize: size, scrollback: 120)
        buffer.applyLive(LiveTail(lines: tail.lines, historyLines: 120, historySize: size, cursorLine: tail.lines.count - 1, cursorColumn: 0))
        while let fetch = buffer.nextFetch(pageLines: 1000) {
            let top = size - fetch.end - fetch.lines
            let text = SyntheticTranscript.lines(top, fetch.lines).joined(separator: "\n")
            let page = TerminalText.styledLines(page: text, expecting: fetch.lines)
            buffer.merge(page: page, historySize: size, complete: fetch.end + fetch.lines >= size, for: fetch)
        }
        return buffer
    }
    /// The 120 scrollback lines above a screen of `rows` lines whose top row is `historySize` (serial numbers are the absolute indexes).
    private func tailLines(historySize: Int, scrollback: Int, edit: Int = 0) -> (lines: [StyledLine], text: String) {
        var raw = SyntheticTranscript.lines(historySize - scrollback, scrollback + rows)
        if edit > 0 { raw[raw.count - 1] += String(repeating: "x", count: edit) }
        let text = raw.joined(separator: "\n") + "\n"
        return (TerminalText.styledScreen(text, cursor: (0, rows - 1), rows: rows).lines, text)
    }

    /// Times `count` answers applied one after the other (each is a different answer, so the run is timed once), in ms per answer.
    private func perAnswer(_ name: String, buffer start: TerminalBuffer, historySize base: Int, step: Int, scrollback: Int, count: Int = 300) -> Double {
        let answers = (1...count).map { tailLines(historySize: base + step * $0, scrollback: scrollback).lines }
        var best = Double.infinity
        for _ in 0..<5 {
            var buffer = start
            let clock = ContinuousClock()
            let began = clock.now
            for (i, lines) in answers.enumerated() {
                buffer.applyLive(LiveTail(lines: lines, historyLines: scrollback, historySize: base + step * (i + 1), cursorLine: lines.count - 1, cursorColumn: 0))
            }
            let d = (clock.now - began).components
            best = min(best, (Double(d.seconds) * 1000 + Double(d.attoseconds) / 1e15) / Double(count))
        }
        print(String(format: "PERF %-46@ %9.4f ms", name as NSString, best))
        return best
    }

    func testApplyingALiveAnswerToAFullBuffer() {
        var buffer = filledBuffer(history: 50_000)
        XCTAssertGreaterThanOrEqual(buffer.lines.count, 50_000)
        // Typing: the screen is the same lines, the last one grows. History does not move.
        var step = 0
        var answers: [[StyledLine]] = []
        for k in 1...40 { answers.append(tailLines(historySize: 50_000, scrollback: 120, edit: k).lines) }
        bench("applyLive, 50k held, echo (last line edited)", inner: 40) {
            let lines = answers[step % answers.count]
            step += 1
            buffer.applyLive(LiveTail(lines: lines, historyLines: 120, historySize: 50_000, cursorLine: lines.count - 1, cursorColumn: 0))
        }
        // Output: two lines scroll in per answer (the answers are parsed beforehand: this is the buffer alone).
        _ = perAnswer("applyLive, 50k held, +2 lines of output", buffer: buffer, historySize: 50_000, step: 2, scrollback: 120)
        XCTAssertEqual(buffer.holes.count, 0)
        XCTAssertTrue(buffer.lines.count >= 50_000)
    }

    func testApplyingAnAnswerOfAFullDesktopHistory() {
        // The desktop's history is full: `history_size` stops growing and the shift is found by comparing lines.
        var buffer = filledBuffer(history: 50_000)
        let tail0 = tailLines(historySize: 50_000, scrollback: 500)
        buffer.applyLive(LiveTail(lines: tail0.lines, historyLines: 500, historySize: 50_000, cursorLine: tail0.lines.count - 1, cursorColumn: 0))
        let answers = (1...200).map { k -> [StyledLine] in
            TerminalText.styledScreen(SyntheticTranscript.lines(50_000 + 2 * k - 500, 500 + rows).joined(separator: "\n") + "\n", cursor: (0, rows - 1), rows: rows).lines
        }
        var best = Double.infinity
        for _ in 0..<5 {
            var copy = buffer
            let clock = ContinuousClock()
            let began = clock.now
            for lines in answers { copy.applyLive(LiveTail(lines: lines, historyLines: 500, historySize: 50_000, cursorLine: lines.count - 1, cursorColumn: 0)) }
            let d = (clock.now - began).components
            best = min(best, (Double(d.seconds) * 1000 + Double(d.attoseconds) / 1e15) / Double(answers.count))
            XCTAssertTrue(copy.drifting)
        }
        print(String(format: "PERF %-46@ %9.4f ms", "applyLive, history full, 500 back, +2 lines" as NSString, best))
    }

    /// What a held line costs in memory. Measured with the allocator, after the temporaries are gone.
    func testWhatAHeldLineCosts() {
        func inUse() -> Int {
            var stats = malloc_statistics_t()
            malloc_zone_statistics(nil, &stats)
            return Int(stats.size_in_use)
        }
        let before = inUse()
        var buffer: TerminalBuffer? = filledBuffer(history: 50_000)
        let after = inUse()
        let count = buffer!.lines.count
        let perLine = Double(after - before) / Double(count)
        print(String(format: "PERF held line: %.1f bytes/line over %d lines (%.2f MB)", perLine, count, Double(after - before) / 1e6))
        XCTAssertGreaterThan(count, 50_000)
        buffer = nil
    }

    // MARK: The wire

    func testDecodeOfALiveAnswerReply() throws {
        let text = answerText(scrollback: 120)
        let reply: JSONValue = .object(["shell_id": .string("44444444-4444-4444-8444-444444444444"), "output": .string(text), "rows": .number(40), "cols": .number(50),
                                        "in_mode": .bool(false), "cursor": .object(["x": .number(3), "y": .number(39)]), "history_size": .number(50_000),
                                        "hash": .string("0123456789abcdef"), "alternate": .bool(false)])
        let data = try JSONEncoder().encode(reply)
        print("PERF reply is \(data.count) bytes of JSON")
        var decoded = JSONValue.null
        bench("JSONDecoder → JSONValue (one reply)") { decoded = (try? JSONDecoder().decode(JSONValue.self, from: data)) ?? .null }
        var output: ShellOutput?
        bench("ShellOutput(result:)") { output = try? ShellOutput(result: decoded) }
        XCTAssertEqual(output?.text.count, text.count)
        var same = false
        let copy = output
        bench("ShellOutput == (unchanged screen)") { same = output == copy }
        XCTAssertTrue(same)
    }

    /// The whole of one answer off the main actor and on it, as the app does it (decode, parse, compare, apply).
    func testOneAnswerEndToEnd() throws {
        var buffer = filledBuffer(history: 50_000)
        var size = 50_000
        bench("answer pipeline: decode+parse+apply (+2 lines)", inner: 20) {
            size += 2
            let raw = SyntheticTranscript.lines(size - 120, 120 + rows).joined(separator: "\n") + "\n"
            let screen = TerminalText.styledScreen(raw, cursor: (0, rows - 1), rows: rows)
            buffer.applyLive(LiveTail(screen: screen, historySize: size))
        }
    }
}
