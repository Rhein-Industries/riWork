import XCTest
@testable import RiWorkCore

/// A message with every kind of block, each separated from the next by a blank line so that its chunks parse on their own.
private let sampleMessage = """
# Plan

Intro with **bold** and `code`.
Second line.

- one
- [ ] two
- [x] three
  - nested

1. first
2. second

> quoted
> *more*

```swift
let x = 1
```

| a | b |
|---|:-:|
| 1 | 2 |

---

    indented

Done [link](https://example.com).
"""

final class ChatMarkdownTests: XCTestCase {
    // MARK: Helpers

    private func parse(_ lines: String...) -> [ChatMarkdownBlock] { ChatMarkdown.parse(lines.joined(separator: "\n")) }
    private func p(_ text: String) -> ChatMarkdownBlock { .paragraph(text) }
    private func h(_ level: Int, _ text: String) -> ChatMarkdownBlock { .heading(level: level, text: text) }
    private func code(_ text: String, _ language: String? = nil, closed: Bool = true) -> ChatMarkdownBlock { .code(language: language, text: text, closed: closed) }
    private func item(_ blocks: ChatMarkdownBlock..., checked: Bool? = nil) -> ChatMarkdownListItem { ChatMarkdownListItem(checked: checked, blocks: blocks) }
    private func line(_ text: String, checked: Bool? = nil) -> ChatMarkdownListItem { ChatMarkdownListItem(checked: checked, blocks: [.paragraph(text)]) }
    private func bullets(_ items: ChatMarkdownListItem...) -> ChatMarkdownBlock { .list(ordered: false, start: 1, items: items) }
    private func numbers(from start: Int = 1, _ items: ChatMarkdownListItem...) -> ChatMarkdownBlock { .list(ordered: true, start: start, items: items) }
    private func table(_ header: [String], _ rows: [[String]] = []) -> ChatMarkdownBlock { .table(header: header, rows: rows) }
    private func seconds(_ work: () -> Void) -> TimeInterval {
        let start = Date()
        work()
        return Date().timeIntervalSince(start)
    }

    /// What every result must satisfy whatever the input: no empty paragraph, a level of 1...6, tables as wide as their header.
    private func assertWellFormed(_ blocks: [ChatMarkdownBlock], _ source: @autoclosure () -> String = "", file: StaticString = #filePath, line: UInt = #line) {
        for block in blocks {
            switch block {
            case .heading(let level, let text):
                XCTAssertTrue((1...6).contains(level), source(), file: file, line: line)
                XCTAssertFalse(text.isEmpty, source(), file: file, line: line)
            case .paragraph(let text):
                XCTAssertFalse(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, source(), file: file, line: line)
            case .code, .rule:
                break
            case .list(_, _, let items):
                XCTAssertFalse(items.isEmpty, source(), file: file, line: line)
                for entry in items { assertWellFormed(entry.blocks, source(), file: file, line: line) }
            case .quote(let inner):
                XCTAssertFalse(inner.isEmpty, source(), file: file, line: line)
                assertWellFormed(inner, source(), file: file, line: line)
            case .table(let header, let rows):
                XCTAssertFalse(header.isEmpty, source(), file: file, line: line)
                for row in rows { XCTAssertEqual(row.count, header.count, source(), file: file, line: line) }
            }
        }
    }

    // MARK: Empty and paragraphs

    func testEmptyAndBlankSourcesHaveNoBlocks() {
        for source in ["", "\n", "\n\n\n", "   ", " \t \n  \n", "\r\n\r\n"] {
            XCTAssertEqual(ChatMarkdown.parse(source), [], "\(source.debugDescription)")
        }
    }
    func testParagraphsAreSeparatedByBlankLines() {
        XCTAssertEqual(parse("one", "", "two"), [p("one"), p("two")])
        XCTAssertEqual(parse("one", "   ", "two"), [p("one"), p("two")], "a line of blanks is blank")
        XCTAssertEqual(parse("one", "", "", "", "two"), [p("one"), p("two")])
    }
    func testParagraphLinesAreJoinedWithNewlinesAndTrimmed() {
        XCTAssertEqual(parse("first line", "second line", "third line"), [p("first line\nsecond line\nthird line")])
        XCTAssertEqual(parse("  indented a  ", "   b\t"), [p("indented a\nb")])
    }
    func testLeadingAndTrailingBlankLinesNeverMakeEmptyParagraphs() {
        XCTAssertEqual(ChatMarkdown.parse("\n\n  \nhello\n\n\n"), [p("hello")])
        XCTAssertEqual(ChatMarkdown.parse("hello\n"), [p("hello")])
    }
    func testInlineSyntaxIsKeptAsSource() {
        XCTAssertEqual(parse("A **bold** `code` [link](https://example.com) and *half"), [p("A **bold** `code` [link](https://example.com) and *half")])
    }
    func testSetextHeadingsHtmlAndFootnotesStayPlainParagraphs() {
        XCTAssertEqual(parse("Title", "====="), [p("Title\n=====")])
        XCTAssertEqual(parse("<div>", "hi", "</div>"), [p("<div>\nhi\n</div>")])
        XCTAssertEqual(parse("Text[^1]", "", "[^1]: the note"), [p("Text[^1]"), p("[^1]: the note")])
        XCTAssertEqual(parse("[ref]: https://example.com"), [p("[ref]: https://example.com")])
        // Without setext headings, dashes under a line are a rule.
        XCTAssertEqual(parse("Title", "-----"), [p("Title"), .rule])
    }
    func testUnicodeSurvives() {
        XCTAssertEqual(parse("# Héllo 👋", "", "日本語 e\u{301}"), [h(1, "Héllo 👋"), p("日本語 e\u{301}")])
        XCTAssertEqual(parse("```", "👨‍👩‍👧 \u{1F600}", "```"), [code("👨‍👩‍👧 \u{1F600}")])
    }

    // MARK: Headings

    func testHeadingsOneToSix() {
        XCTAssertEqual(parse("# One", "## Two", "### Three", "#### Four", "##### Five", "###### Six"),
                       [h(1, "One"), h(2, "Two"), h(3, "Three"), h(4, "Four"), h(5, "Five"), h(6, "Six")])
    }
    func testHeadingTextIsTrimmedAndLosesItsClosingHashes() {
        XCTAssertEqual(parse("#   Spaced   "), [h(1, "Spaced")])
        XCTAssertEqual(parse("## Title ##"), [h(2, "Title")])
        XCTAssertEqual(parse("## Title   ####   "), [h(2, "Title")])
        XCTAssertEqual(parse("# C#"), [h(1, "C#")], "a hash that is part of a word stays")
        XCTAssertEqual(parse("# a # b"), [h(1, "a # b")])
        XCTAssertEqual(parse("#\tTabbed"), [h(1, "Tabbed")])
        XCTAssertEqual(parse("# **Bold** `code`"), [h(1, "**Bold** `code`")], "inline source is kept")
    }
    func testUpToThreeSpacesMayIndentAHeading() {
        XCTAssertEqual(parse("   ### Three"), [h(3, "Three")])
        XCTAssertEqual(parse("    ### Four"), [code("### Four")])
    }
    func testHashtagsAndSevenHashesAreParagraphs() {
        XCTAssertEqual(parse("#hashtag"), [p("#hashtag")])
        XCTAssertEqual(parse("####### seven"), [p("####### seven")])
        XCTAssertEqual(parse("#5 is a number"), [p("#5 is a number")])
    }
    func testAHeadingWithNoTextYetIsAParagraph() {
        XCTAssertEqual(parse("#"), [p("#")])
        XCTAssertEqual(parse("# "), [p("#")])
        XCTAssertEqual(parse("###"), [p("###")])
        XCTAssertEqual(parse("# #"), [p("# #")])
    }
    func testAHeadingInterruptsAParagraphAndNeedsNoBlankLine() {
        XCTAssertEqual(parse("text", "# Head", "more"), [p("text"), h(1, "Head"), p("more")])
    }

    // MARK: Fenced code

    func testFencedCodeWithLanguageKeepsBlankAndIndentedLines() {
        XCTAssertEqual(parse("```swift", "let a = 1", "", "  let b = 2", "```"), [code("let a = 1\n\n  let b = 2", "swift")])
    }
    func testFenceWithoutInfoHasNoLanguageAndFirstWordIsTheLanguage() {
        XCTAssertEqual(parse("```", "plain", "```"), [code("plain")])
        XCTAssertEqual(parse("```   ", "plain", "```"), [code("plain")])
        XCTAssertEqual(parse("``` swift title=\"x\"", "code", "```"), [code("code", "swift")])
        XCTAssertEqual(parse("```c++\t{.numberLines}", "code", "```"), [code("code", "c++")])
    }
    func testTildeFenceAndFenceCharactersInsideAreContent() {
        XCTAssertEqual(parse("~~~python", "print(1)", "~~~"), [code("print(1)", "python")])
        XCTAssertEqual(parse("~~~", "```", "not a closer", "~~~"), [code("```\nnot a closer")])
        XCTAssertEqual(parse("```", "~~~", "```"), [code("~~~")])
    }
    func testAFenceOfNBackticksClosesOnlyOnAtLeastNBackticks() {
        XCTAssertEqual(parse("````", "```", "inner", "```", "````"), [code("```\ninner\n```")])
        XCTAssertEqual(parse("```", "code", "````"), [code("code")], "a longer closer closes")
        XCTAssertEqual(parse("````", "code", "```"), [code("code\n```", closed: false)], "a shorter one is content")
        XCTAssertEqual(parse("```", "code", "``` x", "more"), [code("code\n``` x\nmore", closed: false)], "text after a closer keeps it open")
        XCTAssertEqual(parse("```", "code", "```   "), [code("code")], "blanks after a closer are fine")
    }
    func testCodeContentIsNeverReadAsMarkdown() {
        let body = "# not a heading\n- not a list\n> not a quote\n---\n| a | b |\n|---|---|"
        XCTAssertEqual(ChatMarkdown.parse("```\n\(body)\n```"), [code(body)])
    }
    func testAFenceThatIsNotClosedYetIsOpenCode() {
        XCTAssertEqual(parse("```"), [code("", closed: false)])
        XCTAssertEqual(parse("```swift"), [code("", "swift", closed: false)])
        XCTAssertEqual(parse("```swift", "let x"), [code("let x", "swift", closed: false)])
        XCTAssertEqual(parse("```swift", "let x", "```"), [code("let x", "swift")])
        XCTAssertEqual(parse("```swift", "let x", ""), [code("let x\n", "swift", closed: false)], "a streamed newline is content until the fence closes")
    }
    func testAnIndentedFenceLosesItsIndentFromTheContent() {
        XCTAssertEqual(parse("  ```", "  code", "    deeper", "  ```"), [code("code\n  deeper")])
    }
    func testAFenceInterruptsAParagraph() {
        XCTAssertEqual(parse("Run:", "```", "ls", "```", "Done"), [p("Run:"), code("ls"), p("Done")])
    }
    func testBackticksInTheInfoStringMakeItInlineCodeNotAFence() {
        XCTAssertEqual(parse("```code``` here"), [p("```code``` here")])
        XCTAssertEqual(parse("``", "x"), [p("``\nx")])
        XCTAssertEqual(parse("~~~code~~~"), [code("", "code~~~", closed: false)], "tildes may appear in a tilde fence's info")
    }

    // MARK: Indented code

    func testIndentedCodeByFourSpacesOrATab() {
        XCTAssertEqual(parse("    let a = 1", "    let b = 2"), [code("let a = 1\nlet b = 2")])
        XCTAssertEqual(parse("\tone tab"), [code("one tab")])
        XCTAssertEqual(parse("\t\ttwo tabs"), [code("\ttwo tabs")])
        XCTAssertEqual(parse("    a", "      deeper"), [code("a\n  deeper")])
    }
    func testIndentedCodeKeepsBlankLinesInsideButNotTrailingOnes() {
        XCTAssertEqual(parse("    a", "", "    b", "", "", "after"), [code("a\n\nb"), p("after")])
    }
    func testAnIndentedLineContinuesAParagraphInsteadOfStartingCode() {
        XCTAssertEqual(parse("text", "    more"), [p("text\nmore")])
        XCTAssertEqual(parse("text", "", "    code"), [p("text"), code("code")])
    }

    // MARK: Rules

    func testThematicBreaks() {
        for source in ["---", "***", "___", "- - -", " * * * ", "_____", "-  -  -  -", "   ---"] {
            XCTAssertEqual(ChatMarkdown.parse(source), [.rule], source)
        }
        XCTAssertEqual(parse("    ---"), [code("---")], "four columns in is code")
        XCTAssertEqual(parse("a", "---", "b"), [p("a"), .rule, p("b")])
    }
    func testThingsThatAreNotRules() {
        XCTAssertEqual(parse("--"), [p("--")])
        XCTAssertEqual(parse("---x"), [p("---x")])
        XCTAssertEqual(parse("**bold**"), [p("**bold**")])
        XCTAssertEqual(parse("***bold italic***"), [p("***bold italic***")])
        XCTAssertEqual(parse("-*-"), [p("-*-")])
    }

    // MARK: Quotes

    func testBlockQuoteWithAndWithoutTheOptionalSpace() {
        XCTAssertEqual(parse("> one", "> two"), [.quote([p("one\ntwo")])])
        XCTAssertEqual(parse(">tight"), [.quote([p("tight")])])
        XCTAssertEqual(parse("   > indented"), [.quote([p("indented")])])
    }
    func testQuoteContentIsParsedAsBlocks() {
        XCTAssertEqual(parse("> # Title", "> - a", "> - b", ">", "> ```", "> code", "> ```", ">", "> last"),
                       [.quote([h(1, "Title"), bullets(line("a"), line("b")), code("code"), p("last")])])
    }
    func testNestedQuotes() {
        XCTAssertEqual(parse("> outer", "> > inner"), [.quote([p("outer"), .quote([p("inner")])])])
        XCTAssertEqual(parse(">> deep"), [.quote([.quote([p("deep")])])])
    }
    func testAQuoteEndsAtABlankLineOrALineWithoutTheMarker() {
        XCTAssertEqual(parse("> a", "", "> b"), [.quote([p("a")]), .quote([p("b")])])
        XCTAssertEqual(parse("> a", "lazy"), [.quote([p("a")]), p("lazy")], "no lazy continuation")
    }
    func testAQuoteInterruptsAParagraph() {
        XCTAssertEqual(parse("text", "> quote"), [p("text"), .quote([p("quote")])])
    }
    func testAQuoteWithNothingInItYetIsAParagraph() {
        XCTAssertEqual(parse(">"), [p(">")])
        XCTAssertEqual(parse("> "), [p(">")])
        XCTAssertEqual(parse(">", ">"), [p(">\n>")])
        XCTAssertEqual(parse(">", "> hi"), [.quote([p("hi")])])
    }

    // MARK: Lists

    func testBulletListsWithEachMarker() {
        XCTAssertEqual(parse("- a", "- b", "- c"), [bullets(line("a"), line("b"), line("c"))])
        XCTAssertEqual(parse("* a", "* b"), [bullets(line("a"), line("b"))])
        XCTAssertEqual(parse("+ a", "+ b"), [bullets(line("a"), line("b"))])
        XCTAssertEqual(parse("- a", "* b", "+ c"), [bullets(line("a"), line("b"), line("c"))], "the bullet character does not start a new list")
    }
    func testOrderedListsKeepTheFirstNumberAsStart() {
        XCTAssertEqual(parse("1. a", "2. b"), [numbers(line("a"), line("b"))])
        XCTAssertEqual(parse("3) a", "4) b"), [numbers(from: 3, line("a"), line("b"))])
        XCTAssertEqual(parse("0. zero", "1. one"), [numbers(from: 0, line("zero"), line("one"))])
        XCTAssertEqual(parse("1. a", "1. b", "1. c"), [numbers(line("a"), line("b"), line("c"))], "later numbers are ignored")
        XCTAssertEqual(parse("10. ten", "11. eleven"), [numbers(from: 10, line("ten"), line("eleven"))])
    }
    func testBlankLinesBetweenItemsKeepOneList() {
        XCTAssertEqual(parse("- a", "", "- b", "", "", "- c"), [bullets(line("a"), line("b"), line("c"))])
        XCTAssertEqual(parse("1. a", "", "2. b"), [numbers(line("a"), line("b"))])
    }
    func testADifferentListKindStartsANewList() {
        XCTAssertEqual(parse("- a", "1. b"), [bullets(line("a")), numbers(line("b"))])
        XCTAssertEqual(parse("1. a", "- b"), [numbers(line("a")), bullets(line("b"))])
        XCTAssertEqual(parse("- a", "", "1. b", "", "- c"), [bullets(line("a")), numbers(line("b")), bullets(line("c"))])
    }
    func testBulletsAndOrderedOneInterruptAParagraphButOtherNumbersDoNot() {
        XCTAssertEqual(parse("Intro", "- a"), [p("Intro"), bullets(line("a"))])
        XCTAssertEqual(parse("Intro", "* a"), [p("Intro"), bullets(line("a"))])
        XCTAssertEqual(parse("Intro", "1. a", "2. b"), [p("Intro"), numbers(line("a"), line("b"))])
        XCTAssertEqual(parse("Intro", "2. a"), [p("Intro\n2. a")])
        XCTAssertEqual(parse("It was", "2024. a year"), [p("It was\n2024. a year")])
        XCTAssertEqual(parse("2024. a year"), [numbers(from: 2024, line("a year"))], "at the start of a block any number starts a list")
    }
    func testMarkersWithoutTextYetAreParagraphs() {
        XCTAssertEqual(parse("-"), [p("-")])
        XCTAssertEqual(parse("- "), [p("-")])
        XCTAssertEqual(parse("*"), [p("*")])
        XCTAssertEqual(parse("+"), [p("+")])
        XCTAssertEqual(parse("1."), [p("1.")])
        XCTAssertEqual(parse("1) "), [p("1)")])
        XCTAssertEqual(parse("- a", "-"), [bullets(line("a")), p("-")])
        XCTAssertEqual(parse("1. a", "2."), [numbers(line("a")), p("2.")])
    }
    func testThingsThatLookLikeMarkersButAreNot() {
        for source in ["-a", "-5 degrees", "+1", "*emph* text", "**bold** text", "1.5 liters", "12345678901. x", "1.a", "1 . a", "a. b"] {
            XCTAssertEqual(ChatMarkdown.parse(source), [p(source)], source)
        }
    }
    func testTaskListCheckboxes() {
        XCTAssertEqual(parse("- [ ] todo", "- [x] done", "- [X] caps", "- plain"),
                       [bullets(line("todo", checked: false), line("done", checked: true), line("caps", checked: true), line("plain"))])
        XCTAssertEqual(parse("1. [x] numbered"), [numbers(line("numbered", checked: true))])
    }
    func testCheckboxEdgeCases() {
        XCTAssertEqual(parse("- [ ]"), [bullets(item(checked: false))], "a checkbox with no text yet")
        XCTAssertEqual(parse("- [x]no space"), [bullets(line("[x]no space"))])
        XCTAssertEqual(parse("- [y] other"), [bullets(line("[y] other"))])
        XCTAssertEqual(parse("- ["), [bullets(line("["))])
        XCTAssertEqual(parse("- [ ] t", "  - [x] sub"), [bullets(item(p("t"), bullets(line("sub", checked: true)), checked: false))])
        XCTAssertEqual(parse("* [ ]  spaced  "), [bullets(line("spaced", checked: false))])
    }

    // MARK: Nesting

    func testNestingByTwoSpaces() {
        XCTAssertEqual(parse("- a", "  - b", "  - c", "- d"), [bullets(item(p("a"), bullets(line("b"), line("c"))), line("d"))])
    }
    func testNestingByFourSpaces() {
        XCTAssertEqual(parse("- a", "    - b", "- c"), [bullets(item(p("a"), bullets(line("b"))), line("c"))])
    }
    func testNestingUnderAnOrderedItemByThreeOrTwoSpaces() {
        let expected = [numbers(item(p("a"), bullets(line("b"))), line("c"))]
        XCTAssertEqual(parse("1. a", "   - b", "2. c"), expected)
        XCTAssertEqual(parse("1. a", "  - b", "2. c"), expected, "two spaces under `1. ` still nests")
        XCTAssertEqual(parse("1. a", "    - b", "2. c"), expected)
        XCTAssertEqual(parse("10. a", "    - b"), [numbers(from: 10, item(p("a"), bullets(line("b"))))])
    }
    func testOneSpaceDeeperIsASiblingNotANestedItem() {
        XCTAssertEqual(parse("- a", " - b"), [bullets(line("a"), line("b"))])
    }
    func testThreeLevels() {
        XCTAssertEqual(parse("- a", "  - b", "    - c", "  - d", "- e"),
                       [bullets(item(p("a"), bullets(item(p("b"), bullets(line("c"))), line("d"))), line("e"))])
        XCTAssertEqual(parse("- a", "    - b", "        - c"), [bullets(item(p("a"), bullets(item(p("b"), bullets(line("c"))))))], "four spaces per level")
    }
    func testAnOrderedListNestedUnderABulletAndBack() {
        XCTAssertEqual(parse("- a", "  1. x", "  2. y", "- b"), [bullets(item(p("a"), numbers(line("x"), line("y"))), line("b"))])
        XCTAssertEqual(parse("1. a", "   1. x", "   2. y", "2. b"), [numbers(item(p("a"), numbers(line("x"), line("y"))), line("b"))])
    }
    func testNestedListAfterItsParentsParagraphContinuation() {
        XCTAssertEqual(parse("- a", "  more of a", "  - b"), [bullets(item(p("a\nmore of a"), bullets(line("b"))))])
    }
    func testNestingDeeperThanTheLimitIsTextAndDoesNotCrash() {
        let source = String(repeating: "- ", count: 60) + "deep"
        let blocks = ChatMarkdown.parse(source)
        assertWellFormed(blocks)
        var depth = 0
        var level = blocks
        while case .list(_, _, let items)? = level.first, let next = items.first?.blocks { depth += 1; level = next }
        XCTAssertLessThanOrEqual(depth, 13)
        XCTAssertGreaterThan(depth, 3)
    }

    // MARK: Items with more than a line

    func testContinuationLinesJoinTheItem() {
        XCTAssertEqual(parse("- first line", "  second line", "- next"), [bullets(line("first line\nsecond line"), line("next"))])
        XCTAssertEqual(parse("1. first", "  second", "2. next"), [numbers(line("first\nsecond"), line("next"))])
    }
    func testAnItemCanHaveSeveralParagraphs() {
        XCTAssertEqual(parse("- a", "", "  more", "- b"), [bullets(item(p("a"), p("more")), line("b"))])
    }
    func testFencedCodeInsideAnItem() {
        XCTAssertEqual(parse("1. Run:", "   ```bash", "   ls -la", "", "   pwd", "   ```", "2. Then"),
                       [numbers(item(p("Run:"), code("ls -la\n\npwd", "bash")), line("Then"))])
        XCTAssertEqual(parse("- Run:", "  ```", "  ls", "  ```", "- Next"), [bullets(item(p("Run:"), code("ls")), line("Next"))])
    }
    func testAFenceOpeningTheItemItself() {
        XCTAssertEqual(parse("- ```", "  code", "  ```"), [bullets(item(code("code")))])
    }
    func testAFenceInAnItemThatIsNotClosedYet() {
        XCTAssertEqual(parse("- Run:", "  ```bash", "  ls"), [bullets(item(p("Run:"), code("ls", "bash", closed: false)))])
    }
    func testIndentedCodeInsideAnItem() {
        XCTAssertEqual(parse("- a", "", "      code"), [bullets(item(p("a"), code("code")))])
    }
    func testOtherBlocksInsideAnItem() {
        XCTAssertEqual(parse("- > quote"), [bullets(item(.quote([p("quote")])))])
        XCTAssertEqual(parse("- # Head"), [bullets(item(h(1, "Head")))])
        XCTAssertEqual(parse("- t", "  | a | b |", "  |---|---|", "  | 1 | 2 |"), [bullets(item(p("t"), table(["a", "b"], [["1", "2"]])))])
    }
    func testUnindentedTextEndsTheList() {
        XCTAssertEqual(parse("- a", "text"), [bullets(line("a")), p("text")])
        XCTAssertEqual(parse("- a", "", "text"), [bullets(line("a")), p("text")])
        XCTAssertEqual(parse("- a", "# Head"), [bullets(line("a")), h(1, "Head")])
        XCTAssertEqual(parse("- a", "---"), [bullets(line("a")), .rule])
    }
    func testAListAfterAListWithAParagraphBetween() {
        XCTAssertEqual(parse("- a", "", "between", "", "- b"), [bullets(line("a")), p("between"), bullets(line("b"))])
    }
    func testAnUnindentedFenceAfterAnItemEndsTheListAndTheNextNumberKeepsCounting() {
        XCTAssertEqual(parse("1. Install:", "```bash", "npm i", "```", "2. Next"),
                       [numbers(line("Install:")), code("npm i", "bash"), numbers(from: 2, line("Next"))])
    }
    func testListInsideAQuote() {
        XCTAssertEqual(parse("> - a", ">   - b", "> - c"), [.quote([bullets(item(p("a"), bullets(line("b"))), line("c"))])])
    }

    // MARK: Tables

    func testTableWithAlignmentRow() {
        XCTAssertEqual(parse("| Name | Qty |", "|:-----|----:|", "| Apple | 3 |", "| Pear | 5 |"),
                       [table(["Name", "Qty"], [["Apple", "3"], ["Pear", "5"]])])
        XCTAssertEqual(parse("| a | b |", "|:-:|:-:|"), [table(["a", "b"])])
        XCTAssertEqual(parse("| a |", "| - |", "| 1 |"), [table(["a"], [["1"]])], "one dash is enough")
    }
    func testTableWithoutOuterPipes() {
        XCTAssertEqual(parse("a | b", "--- | ---", "1 | 2"), [table(["a", "b"], [["1", "2"]])])
    }
    func testTableCellsAreTrimmedAndMayBeEmpty() {
        XCTAssertEqual(parse("|  x  |  y  |", "|--|--|", "|  | z |", "| w |  |"), [table(["x", "y"], [["", "z"], ["w", ""]])])
    }
    func testAnEscapedPipeStaysInTheCell() {
        XCTAssertEqual(parse("| expr | note |", "|---|---|", "| a \\| b | `c \\| d` |", "| back\\\\slash | e |"),
                       [table(["expr", "note"], [["a | b", "`c | d`"], ["back\\\\slash", "e"]])])
        XCTAssertEqual(parse("| a \\| b | c |", "|---|---|"), [table(["a | b", "c"])])
    }
    func testRowsAreAsWideAsTheHeader() {
        XCTAssertEqual(parse("| a | b |", "|---|---|", "| 1 |", "| 1 | 2 | 3 |"), [table(["a", "b"], [["1", ""], ["1", "2"]])])
    }
    func testTableEndsAtANonPipeLineOrABlankLine() {
        XCTAssertEqual(parse("| a |", "|---|", "| 1 |", "after"), [table(["a"], [["1"]]), p("after")])
        XCTAssertEqual(parse("| a |", "|---|", "| 1 |", "", "| 2 |"), [table(["a"], [["1"]]), p("| 2 |")])
    }
    func testWithoutADelimiterRowItIsAParagraph() {
        XCTAssertEqual(parse("| a | b |", "| 1 | 2 |"), [p("| a | b |\n| 1 | 2 |")])
        XCTAssertEqual(parse("| a | b |", "|---|"), [p("| a | b |\n|---|")], "the delimiter row must be as wide as the header")
        XCTAssertEqual(parse("| a | b |", "| -x- | --- |"), [p("| a | b |\n| -x- | --- |")])
        XCTAssertEqual(parse("| a | b |", "|:|:|"), [p("| a | b |\n|:|:|")])
        XCTAssertEqual(parse("use a | b here"), [p("use a | b here")])
        XCTAssertEqual(parse("a | b", "---"), [p("a | b"), .rule], "a delimiter row needs a pipe")
    }
    func testATableInterruptsAParagraph() {
        XCTAssertEqual(parse("Here:", "| a | b |", "|---|---|", "| 1 | 2 |"), [p("Here:"), table(["a", "b"], [["1", "2"]])])
    }
    func testTableCellsKeepInlineSource() {
        XCTAssertEqual(parse("| **b** | `c` |", "|---|---|", "| [l](https://example.com) | *e* |"),
                       [table(["**b**", "`c`"], [["[l](https://example.com)", "*e*"]])])
    }
    func testTableInsideAQuote() {
        XCTAssertEqual(parse("> | a |", "> |---|", "> | 1 |"), [.quote([table(["a"], [["1"]])])])
    }
    func testAStreamedTableIsAParagraphUntilItsDelimiterRowIsWhole() {
        XCTAssertEqual(parse("| a | b |"), [p("| a | b |")])
        XCTAssertEqual(parse("| a | b |", "|"), [p("| a | b |\n|")])
        XCTAssertEqual(parse("| a | b |", "|---"), [p("| a | b |\n|---")])
        XCTAssertEqual(parse("| a | b |", "|---|---"), [table(["a", "b"])])
        XCTAssertEqual(parse("| a | b |", "|---|---|", "| 1"), [table(["a", "b"], [["1", ""]])])
    }

    // MARK: Line endings and tabs

    func testCRLFIsOneLineBreak() {
        let lf = "# T\n\npara\nline2\n\n- a\n- b\n\n```swift\ncode\n```\n"
        XCTAssertEqual(ChatMarkdown.parse(lf.replacingOccurrences(of: "\n", with: "\r\n")), ChatMarkdown.parse(lf))
        XCTAssertEqual(ChatMarkdown.parse(sampleMessage.replacingOccurrences(of: "\n", with: "\r\n")), ChatMarkdown.parse(sampleMessage))
        XCTAssertEqual(ChatMarkdown.parse("a\r\nb"), [p("a\nb")])
        XCTAssertEqual(ChatMarkdown.parse("a\rb"), [p("a\nb")], "a lone CR also ends a line")
        XCTAssertEqual(ChatMarkdown.parse("a\r"), [p("a")], "a stream that stops between CR and LF")
    }
    func testTabsCountAsFourColumnsForIndentationOnly() {
        XCTAssertEqual(parse("-\ta", "\t- b"), [bullets(item(p("a"), bullets(line("b"))))])
        XCTAssertEqual(parse("1.\ta"), [numbers(line("a"))])
        XCTAssertEqual(parse("\tcode\twith\ttabs"), [code("code\twith\ttabs")], "tabs inside the text stay")
        XCTAssertEqual(parse("```", "\tindented", "```"), [code("\tindented")])
        XCTAssertEqual(parse("a\tb"), [p("a\tb")])
    }

    // MARK: Streaming

    func testStreamedStatesOfEachBlock() {
        XCTAssertEqual(parse("**bo"), [p("**bo")])
        XCTAssertEqual(parse("`co"), [p("`co")])
        XCTAssertEqual(parse("[lin"), [p("[lin")])
        XCTAssertEqual(parse("[link](https://exa"), [p("[link](https://exa")])
        XCTAssertEqual(parse("##"), [p("##")])
        XCTAssertEqual(parse("## He"), [h(2, "He")])
        XCTAssertEqual(parse("--"), [p("--")])
        XCTAssertEqual(parse("---"), [.rule])
        XCTAssertEqual(parse("a", "```"), [p("a"), code("", closed: false)])
        XCTAssertEqual(parse("a", "``"), [p("a\n``")])
        XCTAssertEqual(parse("- a", "  -"), [bullets(line("a\n-"))], "an empty nested marker is text until it has some")
        XCTAssertEqual(parse("> a", ">"), [.quote([p("a")])])
    }
    func testEveryPrefixOfAMessageParsesWithoutTrapping() {
        let characters = Array(sampleMessage)
        for end in 0...characters.count {
            let prefix = String(characters[0..<end])
            assertWellFormed(ChatMarkdown.parse(prefix), prefix)
        }
    }
    func testEveryPrefixOfAnAwkwardMessageParsesWithoutTrapping() {
        let message = "1. Run:\r\n   ```bash\r\n   ls\r\n   ```\r\n  - sub\t- x\r\n\r\n> > deep\r\n>- a\r\n\t\ttab code\r\n#\ttab head\r\n~~~~\r\n```\r\n~~~~\r\n| h |\r\n|:-:|\r\n|\\|\r\n***\r\n_ _ _\r\n"
        let scalars = Array(message.unicodeScalars)
        for end in 0...scalars.count {
            var view = String.UnicodeScalarView()
            view.append(contentsOf: scalars[0..<end])
            let prefix = String(view)
            assertWellFormed(ChatMarkdown.parse(prefix), prefix)
        }
    }
    func testAClosedMessageParsesLikeTheSumOfItsChunks() {
        let chunks = sampleMessage.components(separatedBy: "\n\n")
        XCTAssertEqual(chunks.count, 10)
        let blocks = chunks.map { ChatMarkdown.parse($0) }
        XCTAssertEqual(ChatMarkdown.parse(sampleMessage), Array(blocks.joined()))
        // The message grows chunk by chunk; each time the blocks so far are exactly the blocks of the chunks so far.
        for count in 1...chunks.count {
            let text = chunks.prefix(count).joined(separator: "\n\n") + "\n"
            XCTAssertEqual(ChatMarkdown.parse(text), Array(blocks.prefix(count).joined()), "after \(count) chunks")
        }
    }
    func testTrailingNewlinesDoNotChangeAClosedMessage() {
        let full = ChatMarkdown.parse(sampleMessage)
        XCTAssertEqual(ChatMarkdown.parse(sampleMessage + "\n"), full)
        XCTAssertEqual(ChatMarkdown.parse(sampleMessage + "\n\n\n"), full)
        XCTAssertEqual(ChatMarkdown.parse("\n\n" + sampleMessage), full)
    }
    func testTheSampleMessageBlocks() {
        XCTAssertEqual(ChatMarkdown.parse(sampleMessage), [
            h(1, "Plan"),
            p("Intro with **bold** and `code`.\nSecond line."),
            bullets(line("one"), line("two", checked: false), item(p("three"), bullets(line("nested")), checked: true)),
            numbers(line("first"), line("second")),
            .quote([p("quoted\n*more*")]),
            code("let x = 1", "swift"),
            table(["a", "b"], [["1", "2"]]),
            .rule,
            code("indented"),
            p("Done [link](https://example.com)."),
        ])
    }
    func testGrowingAFenceKeepsEarlierBlocksStable() {
        let head = ChatMarkdown.parse("Intro\n\n- a\n- b\n\n")
        for tail in ["```", "```swift", "```swift\nlet", "```swift\nlet x = 1\n", "```swift\nlet x = 1\n``", "```swift\nlet x = 1\n```"] {
            let blocks = ChatMarkdown.parse("Intro\n\n- a\n- b\n\n" + tail)
            XCTAssertEqual(Array(blocks.prefix(head.count)), head, tail)
            XCTAssertEqual(blocks.count, head.count + 1, tail)
        }
    }

    // MARK: Fuzz

    func testRandomMarkdownLikeTextNeverTrapsAndIsWellFormed() {
        let tokens = ["#", "##", "#######", " ", "  ", "    ", "\t", "-", "*", "+", "1.", "2)", ">", "|", "\\|", "```", "~~~", "`", "\n", "\n", "\n",
                      "a", "word", "[ ]", "[x]", "---", "***", ":-:", "\\", "\r\n", "\r", "é", "👍", "**", "_"]
        var state: UInt64 = 0x9E37_79B9_7F4A_7C15
        func next() -> Int {
            state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            return Int(state >> 33)
        }
        for _ in 0..<600 {
            var text = ""
            for _ in 0..<(next() % 120) { text += tokens[next() % tokens.count] }
            assertWellFormed(ChatMarkdown.parse(text), text)
        }
    }

    // MARK: Size

    func testAHundredKilobyteMessageIsFastAndMatchesItsParts() {
        let unit = sampleMessage + "\n\n"
        let copies = 100_000 / unit.utf8.count + 1
        let big = String(repeating: unit, count: copies)
        XCTAssertGreaterThan(big.utf8.count, 100_000)
        var blocks: [ChatMarkdownBlock] = []
        let time = seconds { blocks = ChatMarkdown.parse(big) }
        XCTAssertLessThan(time, 2.0)
        XCTAssertEqual(blocks, Array(repeating: ChatMarkdown.parse(sampleMessage), count: copies).flatMap { $0 })
    }
    func testAHundredKilobyteShapesThatCouldBeSlow() {
        let shapes: [(String, String)] = [
            ("quote markers", String(repeating: ">", count: 100_000)),
            ("spaced quote markers", String(repeating: "> ", count: 50_000) + "x"),
            ("bullet markers on one line", String(repeating: "- ", count: 50_000) + "x"),
            ("ordered markers on one line", String(repeating: "1. ", count: 33_000) + "x"),
            ("one long line", String(repeating: "word ", count: 20_000)),
            ("one long table row", "|" + String(repeating: " a |", count: 25_000) + "\n|" + String(repeating: "---|", count: 25_000)),
            ("pipes without a delimiter row", String(repeating: "| a | b |\n", count: 10_000)),
            ("many open fences", String(repeating: "```\n", count: 25_000)),
            ("a long list", String(repeating: "- item\n", count: 15_000)),
            ("alternating list kinds", String(repeating: "- a\n1. b\n", count: 10_000)),
            ("nested list ladder", (0..<400).map { String(repeating: "  ", count: $0 % 30) + "- level" }.joined(separator: "\n")),
            ("many paragraphs", String(repeating: "para\n\n", count: 20_000)),
            ("long indented code", String(repeating: "    code line\n", count: 8_000)),
            ("headings", String(repeating: "## head\n", count: 12_000)),
        ]
        for (name, source) in shapes {
            var blocks: [ChatMarkdownBlock] = []
            let time = seconds { blocks = ChatMarkdown.parse(source) }
            XCTAssertLessThan(time, 2.0, name)
            XCTAssertFalse(blocks.isEmpty, name)
        }
    }

    // MARK: Inline

    private func links(_ text: AttributedString) -> [String] { text.runs.compactMap { $0.link?.absoluteString } }
    private func plain(_ text: AttributedString) -> String { String(text.characters) }

    func testInlineStylesEmphasisAndCode() {
        let text = ChatMarkdown.inline("a **bold** and *soft* and `code`")
        XCTAssertEqual(plain(text), "a bold and soft and code")
        let intents = text.runs.compactMap { run in run.inlinePresentationIntent.map { (String(text[run.range].characters), $0) } }
        XCTAssertTrue(intents.contains { $0.0 == "bold" && $0.1.contains(.stronglyEmphasized) })
        XCTAssertTrue(intents.contains { $0.0 == "soft" && $0.1.contains(.emphasized) })
        XCTAssertTrue(intents.contains { $0.0 == "code" && $0.1.contains(.code) })
    }
    func testInlineKeepsLineBreaksAndSpaces() {
        XCTAssertEqual(plain(ChatMarkdown.inline("one\ntwo")), "one\ntwo")
        XCTAssertEqual(plain(ChatMarkdown.inline("a  b   c")), "a  b   c")
        XCTAssertEqual(plain(ChatMarkdown.inline("")), "")
    }
    func testInlineKeepsWebLinks() {
        let text = ChatMarkdown.inline("See [docs](https://example.com/a?b=1) and [plain](http://example.com) or <https://auto.example>.")
        XCTAssertEqual(plain(text), "See docs and plain or https://auto.example.")
        XCTAssertEqual(links(text), ["https://example.com/a?b=1", "http://example.com", "https://auto.example"])
        XCTAssertEqual(links(ChatMarkdown.inline("[x](HTTPS://Example.com/A)")).count, 1, "the scheme is compared without case")
    }
    func testInlineStripsEveryOtherSchemeButKeepsTheText() {
        let sources = ["[x](javascript:alert(1))", "[x](tel:+15551234)", "[x](file:///etc/passwd)", "[x](riwork://open/abc)",
                       "[x](mailto:a@example.com)", "[x](ftp://example.com/f)", "[x](sms:+15551234)", "[x](data:text/html,hi)",
                       "[x](example.com/relative)", "[x](/absolute/path)", "[x](JavaScript:alert(1))", "<mailto:a@example.com>"]
        for source in sources {
            let text = ChatMarkdown.inline(source)
            XCTAssertEqual(links(text), [], source)
            XCTAssertFalse(plain(text).isEmpty, source)
        }
        XCTAssertEqual(plain(ChatMarkdown.inline("[open this](javascript:alert(1))")), "open this")
        XCTAssertEqual(plain(ChatMarkdown.inline("[call](tel:+15551234)")), "call")
    }
    func testInlineStripsOnlyTheUnsafeLinksOfAMixedText() {
        let text = ChatMarkdown.inline("[good](https://example.com) [bad](javascript:x) [also good](http://example.org) [bad2](riwork://x)")
        XCTAssertEqual(plain(text), "good bad also good bad2")
        XCTAssertEqual(links(text), ["https://example.com", "http://example.org"])
    }
    func testInlineNeverThrowsOnUnfinishedOrOddSyntax() {
        for source in ["**bo", "*", "`", "``", "[", "[a](", "[a](https://", "![img](", "\\", "\u{0}", "**a *b** c*", "[a]((b)", "<", "<https://", "~~x", "a\\"] {
            XCTAssertFalse(plain(ChatMarkdown.inline(source)).isEmpty, source.debugDescription)
        }
        XCTAssertTrue(plain(ChatMarkdown.inline("**bo")).contains("bo"))
        XCTAssertEqual(links(ChatMarkdown.inline("[a](https://")), [], "a half-typed link is not a link")
    }
    func testInlineOfATableCellOrHeadingFromTheBlockParser() {
        guard case .table(let header, let rows)? = parse("| **a** | [l](javascript:x) |", "|---|---|", "| `c` | [m](https://example.com) |").first else { return XCTFail("not a table") }
        XCTAssertEqual(plain(ChatMarkdown.inline(header[0])), "a")
        XCTAssertEqual(links(ChatMarkdown.inline(header[1])), [])
        XCTAssertEqual(links(ChatMarkdown.inline(rows[0][1])), ["https://example.com"])
    }
}
