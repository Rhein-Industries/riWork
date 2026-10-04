import XCTest
@testable import RiWorkCore

final class ChatDiffTests: XCTestCase {
    // MARK: Helpers

    private func lines(_ rows: String...) -> String { rows.joined(separator: "\n") }
    private func parse(_ rows: String...) -> ChatDiff { ChatDiff.parse(rows.joined(separator: "\n")) }
    private func meta(_ text: String) -> ChatDiffLine { ChatDiffLine(kind: .meta, text: text) }
    private func hunk(_ text: String) -> ChatDiffLine { ChatDiffLine(kind: .hunk, text: text) }
    private func added(_ text: String, _ new: Int? = nil) -> ChatDiffLine { ChatDiffLine(kind: .added, text: text, oldLine: nil, newLine: new) }
    private func removed(_ text: String, _ old: Int? = nil) -> ChatDiffLine { ChatDiffLine(kind: .removed, text: text, oldLine: old, newLine: nil) }
    private func context(_ text: String, _ old: Int? = nil, _ new: Int? = nil) -> ChatDiffLine { ChatDiffLine(kind: .context, text: text, oldLine: old, newLine: new) }
    private func seconds(_ work: () -> Void) -> TimeInterval {
        let start = Date()
        work()
        return Date().timeIntervalSince(start)
    }

    private let gitDiff = [
        "diff --git a/src/a.swift b/src/a.swift",
        "index 83db48f..bf2a3d7 100644",
        "--- a/src/a.swift",
        "+++ b/src/a.swift",
        "@@ -1,4 +1,5 @@",
        " import Foundation",
        "-let a = 1",
        "+let a = 2",
        "+let b = 3",
        " ",
        " func f() {}",
        "@@ -20,3 +21,3 @@ func g() {",
        " x",
        "-y",
        "+z",
    ].joined(separator: "\n") + "\n"

    // MARK: Real git diff

    func testAGitDiffWithTwoHunksHasKindsTextAndLineNumbers() {
        let diff = ChatDiff.parse(gitDiff)
        XCTAssertEqual(diff.lines, [
            meta("diff --git a/src/a.swift b/src/a.swift"),
            meta("index 83db48f..bf2a3d7 100644"),
            meta("--- a/src/a.swift"),
            meta("+++ b/src/a.swift"),
            hunk("@@ -1,4 +1,5 @@"),
            context("import Foundation", 1, 1),
            removed("let a = 1", 2),
            added("let a = 2", 2),
            added("let b = 3", 3),
            context("", 3, 4),
            context("func f() {}", 4, 5),
            hunk("@@ -20,3 +21,3 @@ func g() {"),
            context("x", 20, 21),
            removed("y", 21),
            added("z", 22),
        ])
        XCTAssertEqual(diff.added, 3)
        XCTAssertEqual(diff.removed, 2)
        XCTAssertEqual(diff.hiddenLines, 0)
        XCTAssertFalse(diff.isEmpty)
    }
    func testAddedLinesHaveNoOldNumberAndRemovedLinesNoNewNumber() {
        for line in ChatDiff.parse(gitDiff).lines {
            switch line.kind {
            case .added: XCTAssertNil(line.oldLine); XCTAssertNotNil(line.newLine)
            case .removed: XCTAssertNil(line.newLine); XCTAssertNotNil(line.oldLine)
            case .context: XCTAssertNotNil(line.oldLine); XCTAssertNotNil(line.newLine)
            case .hunk, .meta: XCTAssertNil(line.oldLine); XCTAssertNil(line.newLine)
            }
        }
    }
    func testASecondFileInTheSameDiffRestartsTheNumbers() {
        let diff = parse("diff --git a/a b/a", "--- a/a", "+++ b/a", "@@ -7,2 +7,2 @@", " keep", "-old", "+new",
                         "diff --git a/b b/b", "new file mode 100644", "index 0000000..abc1234", "--- /dev/null", "+++ b/b", "@@ -0,0 +1,2 @@", "+one", "+two")
        XCTAssertEqual(diff.lines, [
            meta("diff --git a/a b/a"), meta("--- a/a"), meta("+++ b/a"), hunk("@@ -7,2 +7,2 @@"), context("keep", 7, 7), removed("old", 8), added("new", 8),
            meta("diff --git a/b b/b"), meta("new file mode 100644"), meta("index 0000000..abc1234"), meta("--- /dev/null"), meta("+++ b/b"),
            hunk("@@ -0,0 +1,2 @@"), added("one", 1), added("two", 2),
        ])
        XCTAssertEqual(diff.added, 3)
        XCTAssertEqual(diff.removed, 1)
    }
    func testGitHeaderLinesAreMeta() {
        let diff = parse("diff --git a/x b/y", "old mode 100644", "new mode 100755", "similarity index 90%", "rename from x", "rename to y",
                         "new file mode 100644", "deleted file mode 100644", "Binary files a/x and b/x differ", "index 1a2b3c..4d5e6f 100644")
        XCTAssertEqual(diff.lines.map(\.kind), Array(repeating: .meta, count: 10))
        XCTAssertEqual(diff.added + diff.removed, 0)
    }
    func testIndexIsMetaOnlyWhenItLooksLikeGit() {
        XCTAssertEqual(parse("index = find(x)", "index 12345").lines, [context("index = find(x)"), context("index 12345")])
        XCTAssertEqual(parse("index abc..def").lines, [meta("index abc..def")])
    }

    // MARK: Other flavours

    func testHunksWithoutAnyFileHeader() {
        let diff = parse("@@ -1,2 +1,2 @@", " a", "-b", "+c")
        XCTAssertEqual(diff.lines, [hunk("@@ -1,2 +1,2 @@"), context("a", 1, 1), removed("b", 2), added("c", 2)])
    }
    func testABareListOfAddedAndRemovedLines() {
        let diff = parse("-old one", "-old two", "+new one", "+new two", "+new three")
        XCTAssertEqual(diff.lines, [removed("old one"), removed("old two"), added("new one"), added("new two"), added("new three")])
        XCTAssertEqual(diff.added, 3)
        XCTAssertEqual(diff.removed, 2)
    }
    func testABareDiffWithAFileHeaderPair() {
        let diff = parse("--- a/p", "+++ b/p", "-old", "+new")
        XCTAssertEqual(diff.lines, [meta("--- a/p"), meta("+++ b/p"), removed("old"), added("new")])
        XCTAssertEqual(diff.added, 1)
        XCTAssertEqual(diff.removed, 1)
    }
    func testABareRemovedLineThatLooksLikeAFileHeaderIsRemoved() {
        let diff = parse("--- a comment", "+new")
        XCTAssertEqual(diff.lines, [removed("-- a comment"), added("new")])
        XCTAssertEqual(ChatDiff.parse("+++ more").lines, [added("++ more")])
    }
    func testNewFilePlainContentIsContextAsWritten() {
        let diff = ChatDiff.parse("fn main() {\n    println!(\"hi\");\n\n}\n")
        XCTAssertEqual(diff.lines, [context("fn main() {"), context("    println!(\"hi\");"), context(""), context("}")])
        XCTAssertEqual(diff.added, 0)
        XCTAssertEqual(diff.removed, 0)
    }
    func testALeadingBlankIsTheMarkerOnlyOnceAHeaderHasBeenSeen() {
        XCTAssertEqual(parse(" indented", "plain").lines, [context(" indented"), context("plain")])
        XCTAssertEqual(parse("@@ -1 +1 @@", "-a", "+b", " tail").lines.last, context("tail", 2, 2))
    }
    func testHunkHeaderCountsDefaultToOne() {
        let diff = parse("@@ -3 +4 @@", "-a", "+b", "+c")
        XCTAssertEqual(diff.lines, [hunk("@@ -3 +4 @@"), removed("a", 3), added("b", 4), added("c", 5)])
    }
    func testAHunkHeaderWeCannotReadStillShowsAsAHunk() {
        let diff = parse("@@ nonsense @@", "-a", "+b", "@@@ -1,2 -1,2 +1,3 @@@", " c")
        XCTAssertEqual(diff.lines.map(\.kind), [.hunk, .removed, .added, .hunk, .context])
        XCTAssertEqual(diff.lines[1].oldLine, nil)
        XCTAssertEqual(diff.lines[2].newLine, nil)
    }

    // MARK: Hunk counters

    func testNoNewlineMarkerIsMetaAndIsNotCounted() {
        let diff = parse("@@ -1 +1 @@", "-a", "\\ No newline at end of file", "+b", "\\ No newline at end of file", " after")
        XCTAssertEqual(diff.lines, [
            hunk("@@ -1 +1 @@"), removed("a", 1), meta("\\ No newline at end of file"), added("b", 1), meta("\\ No newline at end of file"), context("after", 2, 2),
        ])
        XCTAssertEqual(parse("+x", "\\ No newline at end of file").lines, [added("x"), meta("\\ No newline at end of file")])
    }
    func testOnlyTheNoNewlineMarkerIsMetaNotEveryLineWithABackslash() {
        XCTAssertEqual(ChatDiff.parse("\\begin{document}\n\\end{document}").lines, [context("\\begin{document}"), context("\\end{document}")])
    }
    func testARemovedLineThatLooksLikeAFileHeaderInsideAHunk() {
        let diff = parse("@@ -1,3 +1,2 @@", " keep", "--- a comment", "-- another", "+++ plus")
        XCTAssertEqual(diff.lines, [hunk("@@ -1,3 +1,2 @@"), context("keep", 1, 1), removed("-- a comment", 2), removed("- another", 3), added("++ plus", 2)])
        XCTAssertEqual(diff.removed, 2)
        XCTAssertEqual(diff.added, 1)
    }
    func testWhenTheCountersRunOutTheNextLineIsJudgedAfresh() {
        let diff = parse("diff --git a/a b/a", "--- a/a", "+++ b/a", "@@ -1 +1 @@", "-x", "+y",
                         "--- a/b", "+++ b/b", "@@ -5 +5 @@", "-p", "+q")
        XCTAssertEqual(diff.lines.map(\.kind), [.meta, .meta, .meta, .hunk, .removed, .added, .meta, .meta, .hunk, .removed, .added])
        XCTAssertEqual(diff.lines[9], removed("p", 5))
        XCTAssertEqual(diff.lines[10], added("q", 5))
    }
    func testCountersThatAreTooBigEndAtTheNextFileHeader() {
        let diff = parse("@@ -1,9 +1,9 @@", "-a", "+b", "diff --git a/c b/c", "--- a/c", "+++ b/c")
        XCTAssertEqual(diff.lines.map(\.kind), [.hunk, .removed, .added, .meta, .meta, .meta])
        XCTAssertEqual(parse("@@ -1,9 +1,9 @@", "-a", "+b", "@@ -4 +4 @@", "-c").lines.map(\.kind), [.hunk, .removed, .added, .hunk, .removed])
        XCTAssertEqual(parse("@@ -1,9 +1,9 @@", "-a", "+b", "@@ -4 +4 @@", "-c").lines.last, removed("c", 4))
    }
    func testCountersThatAreTooSmallStillMarkPlusAndMinusLines() {
        let diff = parse("@@ -1 +1 @@", "-a", "+b", "+c", "-d", "+e")
        XCTAssertEqual(diff.lines, [hunk("@@ -1 +1 @@"), removed("a", 1), added("b", 1), added("c", 2), removed("d", 2), added("e", 3)])
        XCTAssertEqual(diff.added, 3)
        XCTAssertEqual(diff.removed, 2)
    }
    func testAnEmptyLineInsideAHunkIsAContextLine() {
        // Editors and chat clients strip the trailing blank of an empty context line.
        let diff = parse("@@ -1,3 +1,3 @@", " a", "", " c")
        XCTAssertEqual(diff.lines, [hunk("@@ -1,3 +1,3 @@"), context("a", 1, 1), context("", 2, 2), context("c", 3, 3)])
    }
    func testAPlainLineInsideAHunkIsContextWithItsWholeText() {
        let diff = parse("@@ -1,2 +1,2 @@", "plain", " c")
        XCTAssertEqual(diff.lines, [hunk("@@ -1,2 +1,2 @@"), context("plain", 1, 1), context("c", 2, 2)])
    }
    func testAHunkThatIsCutShortJustEnds() {
        let diff = parse("@@ -1,50 +1,50 @@", " a", "-b")
        XCTAssertEqual(diff.lines, [hunk("@@ -1,50 +1,50 @@"), context("a", 1, 1), removed("b", 2)])
    }

    // MARK: Line endings and emptiness

    func testNoLinesForEmptyInputAndNoPhantomLineAfterTheFinalNewline() {
        XCTAssertTrue(ChatDiff.parse("").isEmpty)
        XCTAssertEqual(ChatDiff.parse("").lines, [])
        XCTAssertEqual(ChatDiff.parse("+a\n").lines, [added("a")])
        XCTAssertEqual(ChatDiff.parse("+a").lines, [added("a")])
        XCTAssertEqual(ChatDiff.parse("+a\n\n").lines, [added("a"), context("")], "an empty line before the final newline is a line")
        XCTAssertEqual(ChatDiff.parse("\n").lines, [context("")])
    }
    func testCRLFIsNotPartOfTheText() {
        let diff = ChatDiff.parse("@@ -1 +1 @@\r\n-a\r\n+b\r\n")
        XCTAssertEqual(diff.lines, [hunk("@@ -1 +1 @@"), removed("a", 1), added("b", 1)])
        XCTAssertEqual(ChatDiff.parse("+a\r").lines, [added("a")])
    }
    func testTabsAndUnicodeAreKept() {
        let diff = parse("@@ -1 +1 @@", "-\told", "+\tnew é 👍 日本語")
        XCTAssertEqual(diff.lines.suffix(2), [removed("\told", 1), added("\tnew é 👍 日本語", 1)])
    }
    func testInvalidUTF8DoesNotCrash() {
        let bytes: [UInt8] = [0xFF, 0x2B, 0x61, 0x0A, 0xC3, 0x0A, 0x2D, 0xE2, 0x82]
        let diff = ChatDiff.parse(String(decoding: bytes, as: UTF8.self))
        XCTAssertEqual(diff.lines.count, 3)
        XCTAssertEqual(diff.removed, 1)
    }

    // MARK: Caps

    func testTheCapKeepsTheFirstLinesAndCountsEverything() {
        let text = (0..<6).map { "+add \($0)" }.joined(separator: "\n") + "\n" + (0..<4).map { "-del \($0)" }.joined(separator: "\n") + "\n"
        let diff = ChatDiff.parse(text, maxLines: 3)
        XCTAssertEqual(diff.lines, [added("add 0"), added("add 1"), added("add 2")])
        XCTAssertEqual(diff.added, 6)
        XCTAssertEqual(diff.removed, 4)
        XCTAssertEqual(diff.hiddenLines, 7)
        XCTAssertFalse(diff.isEmpty)
        let exact = ChatDiff.parse(text, maxLines: 10)
        XCTAssertEqual(exact.lines.count, 10)
        XCTAssertEqual(exact.hiddenLines, 0)
    }
    func testClassificationKeepsRunningPastTheCap() {
        // The removed line after the cap is a removed line only because the hunk header before it said so.
        let text = lines("@@ -1,3 +1,2 @@", " keep", "--- looks like a header", "-- and another")
        let capped = ChatDiff.parse(text, maxLines: 1)
        XCTAssertEqual(capped.removed, 2)
        XCTAssertEqual(capped.added, 0)
        XCTAssertEqual(capped.hiddenLines, 3)
        XCTAssertEqual(capped.lines, [hunk("@@ -1,3 +1,2 @@")])
    }
    func testAZeroCapKeepsNothingButStillCounts() {
        let diff = ChatDiff.parse("+a\n-b\n c\n", maxLines: 0)
        XCTAssertEqual(diff.lines, [])
        XCTAssertEqual(diff.added, 1)
        XCTAssertEqual(diff.removed, 1)
        XCTAssertEqual(diff.hiddenLines, 3)
        XCTAssertFalse(diff.isEmpty, "there are lines, they are only hidden")
        XCTAssertTrue(ChatDiff.parse("", maxLines: 0).isEmpty)
    }
    func testTheDefaultCapIsTwoThousandLines() {
        let diff = ChatDiff.parse(String(repeating: "+x\n", count: 2500))
        XCTAssertEqual(diff.lines.count, 2000)
        XCTAssertEqual(diff.hiddenLines, 500)
        XCTAssertEqual(diff.added, 2500)
    }

    // MARK: Long lines

    func testALongLineIsCutWithAnEllipsis() {
        let diff = ChatDiff.parse("+" + String(repeating: "x", count: 5000))
        XCTAssertEqual(diff.lines.count, 1)
        XCTAssertEqual(diff.lines[0].text, String(repeating: "x", count: 4000) + "…")
        XCTAssertEqual(diff.lines[0].kind, .added)
    }
    func testALineExactlyAtTheLimitIsNotCut() {
        XCTAssertEqual(ChatDiff.parse("+" + String(repeating: "x", count: 10), maxLineLength: 10).lines[0].text, String(repeating: "x", count: 10))
        XCTAssertEqual(ChatDiff.parse("+" + String(repeating: "x", count: 11), maxLineLength: 10).lines[0].text, String(repeating: "x", count: 10) + "…")
    }
    func testTheCutIsOnCharacterBoundaries() {
        let family = "👨‍👩‍👧"
        XCTAssertEqual(ChatDiff.parse("+" + String(repeating: family, count: 10), maxLineLength: 4).lines[0].text, String(repeating: family, count: 4) + "…")
        let accent = "e\u{301}"
        XCTAssertEqual(ChatDiff.parse("-" + String(repeating: accent, count: 10), maxLineLength: 3).lines[0].text, String(repeating: accent, count: 3) + "…")
        // Many bytes but few characters: not cut.
        XCTAssertEqual(ChatDiff.parse("+" + String(repeating: family, count: 3), maxLineLength: 3).lines[0].text, String(repeating: family, count: 3))
        XCTAssertEqual(ChatDiff.parse("+" + String(repeating: "日", count: 5), maxLineLength: 2).lines[0].text, "日日…")
    }
    func testHunkAndMetaLinesAreCutToo() {
        let long = String(repeating: "y", count: 6000)
        let diff = parse("diff --git a/\(long) b/x", "@@ -1 +1 @@ \(long)")
        XCTAssertEqual(diff.lines[0].text.count, 4001)
        XCTAssertTrue(diff.lines[0].text.hasPrefix("diff --git a/yyy"))
        XCTAssertEqual(diff.lines[1].text.count, 4001)
        XCTAssertTrue(diff.lines[1].text.hasSuffix("…"))
    }
    func testACutLineStillCountsAsAddedAndKeepsItsNumber() {
        let diff = ChatDiff.parse(lines("@@ -1 +1 @@", "+" + String(repeating: "z", count: 100)), maxLineLength: 10)
        XCTAssertEqual(diff.lines[1], added(String(repeating: "z", count: 10) + "…", 1))
        XCTAssertEqual(diff.added, 1)
    }

    // MARK: Size and robustness

    func testAFiveMegabyteDiffIsFastAndCountedToTheEnd() {
        let hunkLines = 55_000
        let unit = "-old line of text that goes on for a while\n+new line of text that goes on for a while\n unchanged line of the file stays put\n"
        let text = "@@ -1,\(hunkLines * 2) +1,\(hunkLines * 2) @@\n" + String(repeating: unit, count: hunkLines)
        XCTAssertGreaterThan(text.utf8.count, 5_000_000)
        var diff = ChatDiff()
        let time = seconds { diff = ChatDiff.parse(text) }
        XCTAssertLessThan(time, 2.0)
        XCTAssertEqual(diff.lines.count, 2000)
        XCTAssertEqual(diff.added, hunkLines)
        XCTAssertEqual(diff.removed, hunkLines)
        XCTAssertEqual(diff.hiddenLines, 1 + hunkLines * 3 - 2000)
        XCTAssertEqual(diff.lines[0].kind, .hunk)
        XCTAssertEqual(diff.lines[1], removed("old line of text that goes on for a while", 1))
        XCTAssertEqual(diff.lines[2], added("new line of text that goes on for a while", 1))
    }
    func testAFiveMegabyteSingleLineAndAMillionBlankLines() {
        var diff = ChatDiff()
        let line = "+" + String(repeating: "m", count: 5_000_000)
        XCTAssertLessThan(seconds { diff = ChatDiff.parse(line) }, 2.0)
        XCTAssertEqual(diff.lines.count, 1)
        XCTAssertEqual(diff.lines[0].text.count, 4001)
        XCTAssertLessThan(seconds { diff = ChatDiff.parse(String(repeating: "\n", count: 1_000_000)) }, 2.0)
        XCTAssertEqual(diff.lines.count, 2000)
        XCTAssertEqual(diff.hiddenLines, 1_000_000 - 2000)
    }
    func testEveryPrefixOfADiffParsesAndCountsAreConsistent() {
        let characters = Array(gitDiff)
        for end in 0...characters.count {
            let text = String(characters[0..<end])
            let diff = ChatDiff.parse(text)
            XCTAssertEqual(diff.added, diff.lines.filter { $0.kind == .added }.count, text)
            XCTAssertEqual(diff.removed, diff.lines.filter { $0.kind == .removed }.count, text)
        }
    }
    func testRandomLineSoupNeverTrapsAndTheCapChangesNothingButTheKeptLines() {
        let rows = ["+", "-", " ", "+x", "-y", " z", "@@ -1,2 +1,2 @@", "@@", "@@ -3 +9 @@ f()", "diff --git a b", "index abc..def 100644", "--- a/f", "+++ b/f", "--- c", "+++ d",
                    "\\ No newline at end of file", "\\", "x", "", "\t", "Binary files a and b differ", "é", "👍", "new mode 100755", "@@ -0,0 +1 @@", "+++", "---"]
        var state: UInt64 = 0xDEAD_BEEF_CAFE_F00D
        func next() -> Int {
            state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            return Int(state >> 33)
        }
        for _ in 0..<400 {
            let count = next() % 60
            let text = (0..<count).map { _ in rows[next() % rows.count] }.joined(separator: next() % 5 == 0 ? "\r\n" : "\n") + (next() % 2 == 0 ? "\n" : "")
            let whole = ChatDiff.parse(text)
            let capped = ChatDiff.parse(text, maxLines: 7, maxLineLength: 3)
            XCTAssertEqual(capped.added, whole.added, text)
            XCTAssertEqual(capped.removed, whole.removed, text)
            XCTAssertEqual(capped.lines.count + capped.hiddenLines, whole.lines.count, text)
            XCTAssertEqual(capped.lines.map(\.kind), whole.lines.prefix(7).map(\.kind), text)
            XCTAssertEqual(capped.lines.map(\.oldLine), whole.lines.prefix(7).map(\.oldLine), text)
            XCTAssertEqual(whole.added, whole.lines.filter { $0.kind == .added }.count, text)
            XCTAssertEqual(whole.removed, whole.lines.filter { $0.kind == .removed }.count, text)
        }
    }
}
