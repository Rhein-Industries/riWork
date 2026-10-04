import XCTest
@testable import RiWorkCore

private func line(_ text: String) -> StyledLine {
    StyledLine(text: text, runs: text.isEmpty ? [] : [StyleRun(length: text.count, style: .plain)], columns: text.reduce(0) { $0 + TerminalText.cellWidth($1) })
}
/// The text of each link found on a line, in order.
private func found(_ text: String) -> [String] {
    let characters = Array(text)
    return TerminalLinks.spans(in: text).map { String(characters[$0.range]) }
}

/// Web and mail links in terminal text: which characters are a link, where it goes, and how rows that wrap are read.
final class TerminalLinksTests: XCTestCase {
    // MARK: one line

    func testPlainURLsAreFoundWithTheirPlaceOnTheLine() {
        let spans = TerminalLinks.spans(in: "see https://example.com/a?b=1#c and http://x.dev")
        XCTAssertEqual(spans.map(\.range), [4..<31, 36..<48])
        XCTAssertEqual(spans.map(\.url.absoluteString), ["https://example.com/a?b=1#c", "http://x.dev"])
        XCTAssertEqual(found("HTTPS://Example.com/Path"), ["HTTPS://Example.com/Path"], "the scheme is matched without case")
    }

    func testMailtoIsALinkAndOtherSchemesAreNot() {
        XCTAssertEqual(found("write to mailto:dev@example.com."), ["mailto:dev@example.com"])
        XCTAssertEqual(TerminalLinks.spans(in: "mailto:dev@example.com").first?.url.scheme, "mailto")
        XCTAssertEqual(found("ftp://example.com file:///tmp/a ssh://host"), [])
    }

    func testSentencePunctuationAndUnbalancedBracketsAreTrimmed() {
        XCTAssertEqual(found("Go to https://example.com."), ["https://example.com"])
        XCTAssertEqual(found("(see https://example.com/x)."), ["https://example.com/x"])
        XCTAssertEqual(found("[https://example.com/y]"), ["https://example.com/y"])
        XCTAssertEqual(found("https://example.com/a, https://example.com/b; done"), ["https://example.com/a", "https://example.com/b"])
        XCTAssertEqual(found("'https://example.com/q?' ok!"), ["https://example.com/q"])
        XCTAssertEqual(found("https://en.wikipedia.org/wiki/Rust_(video_game)"), ["https://en.wikipedia.org/wiki/Rust_(video_game)"], "balanced brackets stay")
        XCTAssertEqual(found("<https://example.com/z>"), ["https://example.com/z"])
        XCTAssertEqual(found("\"https://example.com/q\""), ["https://example.com/q"])
    }

    func testWhatIsNotALink() {
        XCTAssertEqual(found("https:// alone"), [], "a scheme alone")
        XCTAssertEqual(found("xhttps://example.com"), [], "a scheme glued to a word")
        XCTAssertEqual(found("https://example.com/very/long…"), [], "cut short on screen")
        XCTAssertEqual(found("no links here: none"), [])
        XCTAssertEqual(found(""), [])
    }

    func testTheEarliestStartWinsAndBoxDrawingEndsAURL() {
        XCTAssertEqual(found("https://a.example/?next=https://b.example"), ["https://a.example/?next=https://b.example"])
        XCTAssertEqual(found("│https://example.com│"), ["https://example.com"])
        XCTAssertEqual(found("url=https://example.com"), ["https://example.com"], "a scheme after punctuation")
    }

    func testRangesCountCharactersNotBytes() {
        let spans = TerminalLinks.spans(in: "✔\u{FE0E} 日本 https://example.com/ü")
        XCTAssertEqual(spans.map(\.range), [5..<26])
        XCTAssertEqual(spans.first?.url.host, "example.com")
    }

    // MARK: wrapped rows

    func testAURLWrappedAcrossFullRowsIsOneLinkWithASpanOnEachRow() {
        // A pane 20 cells wide: the first two rows are full, so they continue on the next.
        let rows = ["see https://example.", "com/a/very/long/path", "/end. done"].map(line)
        let range = TerminalLinks.wrappedRows(around: 1, wrapColumns: 20) { rows.indices.contains($0) ? rows[$0] : nil }
        XCTAssertEqual(range, 0..<3)
        let spans = TerminalLinks.spans(inWrapped: rows)
        let target = URL(string: "https://example.com/a/very/long/path/end")
        XCTAssertEqual(spans[0], [TerminalLinkSpan(range: 4..<20, url: target!)])
        XCTAssertEqual(spans[1], [TerminalLinkSpan(range: 0..<20, url: target!)])
        XCTAssertEqual(spans[2], [TerminalLinkSpan(range: 0..<4, url: target!)], "the full stop after it is not part of it")
    }

    func testRowsThatDoNotFillThePaneAreNotJoined() {
        let rows = ["see https://example.", "com/next"].map(line)
        let source: (Int) -> StyledLine? = { rows.indices.contains($0) ? rows[$0] : nil }
        XCTAssertEqual(TerminalLinks.wrappedRows(around: 0, wrapColumns: 30, line: source), 0..<1)
        XCTAssertEqual(TerminalLinks.wrappedRows(around: 1, wrapColumns: 30, line: source), 1..<2)
        XCTAssertEqual(TerminalLinks.wrappedRows(around: 0, wrapColumns: nil, line: source), 0..<1, "without the pane width nothing is joined")
        XCTAssertEqual(TerminalLinks.wrappedRows(around: 0, wrapColumns: 12, line: source), 0..<1, "a row wider than the pane is from before a resize")
        XCTAssertEqual(TerminalLinks.spans(in: rows[0].text).first?.url.absoluteString, "https://example")
    }

    func testAMissingRowEndsAWrappedLine() {
        let rows = [line("https://example.com/"), .missing, line("rest")]
        XCTAssertEqual(TerminalLinks.wrappedRows(around: 0, wrapColumns: 20) { rows.indices.contains($0) ? rows[$0] : nil }, 0..<2)
        XCTAssertEqual(TerminalLinks.wrappedRows(around: 2, wrapColumns: 20) { rows.indices.contains($0) ? rows[$0] : nil }, 2..<3)
    }

    func testTheWrappedRowsAroundAnyRowAreTheSame() {
        let rows = ["aaaaaaaaaa", "bbbbbbbbbb", "cc", "dddddddddd", "e"].map(line)
        let source: (Int) -> StyledLine? = { rows.indices.contains($0) ? rows[$0] : nil }
        XCTAssertEqual((0..<5).map { TerminalLinks.wrappedRows(around: $0, wrapColumns: 10, line: source) }, [0..<3, 0..<3, 0..<3, 3..<5, 3..<5])
    }

    func testStyledOutputIsReadAsItsText() {
        // SGR around and inside a link changes nothing; an OSC 8 hyperlink is dropped by the parser and its words are read as text.
        let input = "\u{1B}[4;34mhttps://exa\u{1B}[1mmple.com\u{1B}[0m and \u{1B}]8;;https://hidden.example\u{07}words\u{1B}]8;;\u{07}\n"
        let screen = TerminalText.styledScreen(input, cursor: (0, 1), rows: 2)
        XCTAssertEqual(screen.lines[0].text, "https://example.com and words")
        XCTAssertEqual(TerminalLinks.spans(in: screen.lines[0].text).map(\.url.absoluteString), ["https://example.com"])
    }
}
