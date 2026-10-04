import XCTest
@testable import RiWorkCore

/// What the chat screen decides without drawing: the keyboard, the answers to questions, the usage meter and the tab order.
final class ChatPresentationTests: XCTestCase {
    private func approval(_ choices: [ChatDecision]) -> ChatApproval { ChatApproval(requestID: "r1", kind: .command, title: "rm -rf build", choices: choices) }
    private let everything: [ChatDecision] = [.accept, .acceptForSession, .decline, .cancel]
    private func action(_ key: ChatKey, empty: Bool = true, approval: ChatApproval? = nil, busy: Bool = false, canSend: Bool = true) -> ChatKeyAction {
        ChatKeyRouter.action(for: key, in: ChatKeyContext(composerIsEmpty: empty, approval: approval, busy: busy, canSend: canSend))
    }

    // MARK: The keyboard

    func testReturnSendsAndShiftReturnMakesANewLine() {
        XCTAssertEqual(action(.return, empty: false), .send)
        XCTAssertEqual(action(.shiftReturn, empty: false), .insertNewline)
        XCTAssertEqual(action(.shiftReturn, empty: true), .insertNewline)
        XCTAssertEqual(action(.return, empty: true), .none, "nothing to send, nothing pending: the key is swallowed")
        XCTAssertEqual(action(.return, empty: false, canSend: false), .none, "a send is on its way, or the link is down")
        XCTAssertEqual(action(.return, empty: false, approval: approval(everything)), .send, "typing something takes Return back from the request")
    }
    func testWithARequestWaitingAndNothingTypedTheKeysAnswerIt() {
        let pending = approval(everything)
        XCTAssertEqual(action(.return, approval: pending), .decide(.accept))
        XCTAssertEqual(action(.shiftReturn, approval: pending), .decide(.acceptForSession))
        XCTAssertEqual(action(.escape, approval: pending), .decide(.decline))
        XCTAssertEqual(action(.commandDelete, approval: pending), .decide(.decline), "Escape for the Clicks keyboard, which has none")
    }
    func testTypedTextKeepsEveryKeyForTheText() {
        let pending = approval(everything)
        XCTAssertEqual(action(.shiftReturn, empty: false, approval: pending), .insertNewline)
        XCTAssertEqual(action(.escape, empty: false, approval: pending), .none, "Escape does not deny while you are writing")
        XCTAssertEqual(action(.commandDelete, empty: false, approval: pending), .none, "and ⌘⌫ stays the text view's")
    }
    func testADecisionTheProviderDoesNotOfferIsNeverMadeOnAKey() {
        XCTAssertEqual(action(.return, approval: approval([.decline, .cancel])), .none, "Return does not pick Deny for you")
        XCTAssertEqual(action(.shiftReturn, approval: approval([.accept, .decline])), .none, "no session-wide allow here")
        XCTAssertEqual(action(.escape, approval: approval([.accept])), .none)
        XCTAssertEqual(action(.escape, approval: approval([.accept, .cancel])), .decide(.cancel), "Stop is the way out when Deny is not offered")
        // A request with no readable choices offers Allow and Deny.
        XCTAssertEqual(action(.return, approval: approval([])), .decide(.accept))
        XCTAssertEqual(action(.escape, approval: approval([])), .decide(.decline))
    }
    func testWithNoRequestEscapeDoesNothing() {
        XCTAssertEqual(action(.escape), .none)
        XCTAssertEqual(action(.escape, empty: false), .none)
        XCTAssertEqual(action(.commandDelete), .none)
    }
    func testCommandPeriodInterruptsOnlyAWorkingChat() {
        XCTAssertEqual(action(.commandPeriod, busy: true), .interrupt)
        XCTAssertEqual(action(.commandPeriod, empty: false, busy: true), .interrupt, "even while writing the next message")
        XCTAssertEqual(action(.commandPeriod, busy: false), .none)
    }
    func testTheChatKeysAreNotTheTerminalsShortcuts() {
        // A chat screen has no KeyCapture, but a hardware keyboard is the same one on every screen: the chords the chat uses must not be
        // ones the terminal's key view has taken (⌘K ⌘, ⌘/, the hotkey menu's, a person's hotkeys' and the Clicks template's), so that
        // the two can never both want a key.
        let template = HotkeyTemplate.clicks
        let map = ShortcutMap(hotkeys: template.hotkeys, settings: ShortcutSettings(paletteChords: template.paletteChords))
        for key in ChatKey.allCases {
            let chord = ChatKeyBindings.chord(key)
            XCTAssertNil(map.action(for: chord), "\(chord.title)")
            for reserved in [KeyChord.paletteDefault, .settingsDefault, .helpDefault] { XCTAssertNotEqual(chord, reserved, chord.title) }
        }
        // The app's own SwiftUI shortcuts are letters: ⌘N (new terminal), ⌘⇧N (new project) and ⌘O (project order). None is a chat chord.
        let letters = [HIDKey.code(forCharacter: "n"), HIDKey.code(forCharacter: "o")].compactMap { $0 }
        XCTAssertEqual(letters.count, 2)
        let swiftUI = letters.flatMap { [KeyChord(keyCode: $0, modifiers: .command), KeyChord(keyCode: $0, modifiers: [.command, .shift])] }
        for chord in swiftUI { XCTAssertFalse(ChatKey.allCases.map(ChatKeyBindings.chord).contains(chord), chord.title) }
        XCTAssertEqual(ChatKeyBindings.period, HIDKey.code(forCharacter: ".") ?? -1)
    }

    // MARK: Answering questions

    private func question(_ prompts: [ChatQuestionPrompt]) -> ChatQuestion { ChatQuestion(requestID: "q1", questions: prompts) }
    private let single = ChatQuestionPrompt(header: "Pick", question: "Which?", options: [ChatQuestionOption(label: "A"), ChatQuestionOption(label: "B"), ChatQuestionOption(label: "C")])
    private var multi: ChatQuestionPrompt {
        ChatQuestionPrompt(question: "Which ones?", options: [ChatQuestionOption(label: "A"), ChatQuestionOption(label: "B"), ChatQuestionOption(label: "C")], multiSelect: true)
    }

    func testASingleChoiceQuestionTakesOneOptionOrSomeText() {
        var form = ChatAnswerForm(question: question([single]))
        XCTAssertFalse(form.isComplete)
        form.toggle(prompt: 0, option: 1)
        XCTAssertTrue(form.isComplete); XCTAssertEqual(form.answers, [["B"]])
        form.toggle(prompt: 0, option: 2)
        XCTAssertEqual(form.answers, [["C"]], "a second choice replaces the first")
        form.toggle(prompt: 0, option: 2)
        XCTAssertFalse(form.isComplete, "tapping the chosen option again clears it")
        form.toggle(prompt: 0, option: 0)
        form.setText(prompt: 0, "something else")
        XCTAssertEqual(form.answers, [["something else"]], "typing lets go of the option")
        form.toggle(prompt: 0, option: 0)
        XCTAssertEqual(form.answers, [["A"]], "and choosing lets go of the text")
        XCTAssertEqual(form.text, [""])
    }
    func testAMultipleChoiceQuestionTakesAnyOptionsAndTextBesides() {
        var form = ChatAnswerForm(question: question([multi]))
        form.toggle(prompt: 0, option: 2)
        form.toggle(prompt: 0, option: 0)
        XCTAssertEqual(form.answers, [["A", "C"]], "in the order the options are listed, not the order they were tapped")
        form.setText(prompt: 0, "  and D  ")
        XCTAssertEqual(form.answers, [["A", "C", "and D"]])
        form.toggle(prompt: 0, option: 0)
        XCTAssertEqual(form.answers, [["C", "and D"]])
        XCTAssertTrue(form.isChosen(prompt: 0, option: 2)); XCTAssertFalse(form.isChosen(prompt: 0, option: 0))
    }
    func testEveryQuestionNeedsAnAnswerAndTheAnswersKeepTheirOrder() {
        var form = ChatAnswerForm(question: question([single, multi, ChatQuestionPrompt(question: "Name?")]))
        form.toggle(prompt: 1, option: 1)
        XCTAssertFalse(form.isComplete)
        form.toggle(prompt: 0, option: 0)
        XCTAssertFalse(form.isComplete, "the free-text question is still empty")
        form.setText(prompt: 2, "   ")
        XCTAssertFalse(form.isComplete, "blank is not an answer")
        form.setText(prompt: 2, "Ada")
        XCTAssertTrue(form.isComplete)
        XCTAssertEqual(form.answers, [["A"], ["B"], ["Ada"]])
    }
    func testNothingOutOfRangeAndNoQuestionsIsEverComplete() {
        var form = ChatAnswerForm(question: question([single]))
        form.toggle(prompt: 5, option: 0); form.toggle(prompt: 0, option: 9); form.toggle(prompt: -1, option: 0); form.setText(prompt: 3, "x")
        XCTAssertEqual(form, ChatAnswerForm(question: question([single])))
        XCTAssertFalse(ChatAnswerForm(question: question([])).isComplete)
        XCTAssertEqual(ChatAnswerForm(question: question([])).answers, [])
    }

    // MARK: Usage

    func testTokensAreShortened() {
        XCTAssertEqual([0, 7, 999, 1000, 1234, 9999, 10_000, 84_000, 99_999, 100_000, 200_000, 999_999].map(ChatUsageMeter.tokens),
                       ["0", "7", "999", "1k", "1.2k", "10k", "10k", "84k", "100k", "100k", "200k", "1M"])
        XCTAssertEqual([1_000_000, 1_234_567, 20_000_000].map(ChatUsageMeter.tokens), ["1M", "1.2M", "20M"])
    }
    func testTheMeterSaysHowFullTheContextIsAndWhatItCost() {
        let usage = ChatUsage(inputTokens: 1200, outputTokens: 300, cachedInputTokens: 100, contextWindow: 200_000, contextUsed: 84_000, costUSD: 0.4234)
        let meter = ChatUsageMeter(usage)
        XCTAssertEqual(meter.contextFraction, 0.42)
        XCTAssertEqual(meter.contextText, "42% of 200k")
        XCTAssertEqual(meter.costText, "≈ $0.42 (estimate)")
        XCTAssertEqual(meter.text, "42% of 200k · ≈ $0.42 (estimate)")
        XCTAssertEqual(meter.spoken, "Context 42 percent full, of 200k tokens. about $0.42 (estimate)")
    }
    func testWithoutAWindowTheTokensSpentAreShownAndCostIsOnlyEverAnEstimate() {
        let codex = ChatUsageMeter(ChatUsage(inputTokens: 12_345, outputTokens: 1_000))
        XCTAssertNil(codex.contextFraction); XCTAssertNil(codex.costText)
        XCTAssertEqual(codex.text, "13.3k tokens")
        XCTAssertNil(ChatUsageMeter(ChatUsage()).text, "nothing to say yet")
        XCTAssertNil(ChatUsageMeter(ChatUsage()).spoken)
        XCTAssertEqual(ChatUsageMeter(ChatUsage(costUSD: 0.004)).costText, "≈ $0.004 (estimate)")
        XCTAssertEqual(ChatUsageMeter(ChatUsage(costUSD: 0)).costText, "≈ $0.00 (estimate)")
        XCTAssertEqual(ChatUsageMeter(ChatUsage(costUSD: 12.5)).costText, "≈ $12.50 (estimate)")
        XCTAssertNil(ChatUsageMeter(ChatUsage(costUSD: -1)).costText)
        XCTAssertNil(ChatUsageMeter(ChatUsage(costUSD: .nan)).costText)
    }
    func testAFullOrOverfullContextIsCappedAtOneHundredPercent() {
        XCTAssertEqual(ChatUsageMeter(ChatUsage(contextWindow: 1000, contextUsed: 5000)).contextFraction, 1)
        XCTAssertEqual(ChatUsageMeter(ChatUsage(contextWindow: 1000, contextUsed: 5000)).contextText, "100% of 1k")
        XCTAssertNil(ChatUsageMeter(ChatUsage(contextWindow: 0, contextUsed: 5)).contextFraction)
        XCTAssertNil(ChatUsageMeter(ChatUsage(contextWindow: 1000)).contextFraction, "the used part is not known")
    }

    // MARK: Tabs

    func testChatsAreOrderedNewestFirstAndKeepTheirOrderWhenTheyTie() {
        func chat(_ id: String, _ at: UInt64) -> ChatInfo { ChatInfo(id: id, provider: .codex, createdAtUnix: at) }
        XCTAssertEqual(ChatTabs.ordered([chat("old", 1), chat("new", 9), chat("mid", 5), chat("tie-a", 5), chat("tie-b", 5)]).map(\.id), ["new", "mid", "tie-a", "tie-b", "old"])
        XCTAssertEqual(ChatTabs.ordered([]), [])
        let id = "44444444-4444-4444-8444-444444444444"
        let named = ChatInfo(id: id, provider: .claude, title: "Fix the build")
        XCTAssertEqual(ChatTabs.title(named), "Fix the build")
        XCTAssertEqual(ChatTabs.detail(named, branch: "main"), "Claude chat · main")
        XCTAssertEqual(ChatTabs.detail(named, branch: nil), "Claude chat")
        let plain = ChatInfo(id: id, provider: .codex, title: "Codex chat")
        XCTAssertEqual(ChatTabs.title(plain), "Codex chat")
        XCTAssertEqual(ChatTabs.detail(plain, branch: "main"), "main", "the first line already says what it is")
        XCTAssertEqual(ChatTabs.detail(plain, branch: nil), "44444444")
        let unnamed = ChatInfo(id: id, provider: .claude, title: "  ")
        XCTAssertEqual(ChatTabs.title(unnamed), "Claude chat")
        XCTAssertEqual(ChatTabs.detail(unnamed, branch: nil), "44444444")
    }
}

/// The text of cards: the end of long output, and a line about a tool call.
final class ChatCardTextTests: XCTestCase {
    func testTheTailKeepsTheLastLinesAndCountsTheOthers() {
        let text = (1...10).map { "line \($0)" }.joined(separator: "\n") + "\n"
        let tail = ChatOutput.tail(text, lines: 3)
        XCTAssertEqual(tail.text, "line 8\nline 9\nline 10")
        XCTAssertEqual(tail.hidden, 7)
        XCTAssertEqual(ChatOutput.tail(text, lines: 10).hidden, 0)
        XCTAssertEqual(ChatOutput.tail(text, lines: 10).text, text.dropLast().description)
        XCTAssertEqual(ChatOutput.tail(text, lines: 99).hidden, 0)
    }
    func testTheTailOfShortOrOddTextIsTheTextItself() {
        XCTAssertEqual(ChatOutput.tail("").text, ""); XCTAssertEqual(ChatOutput.tail("").hidden, 0)
        XCTAssertEqual(ChatOutput.tail("one").text, "one")
        XCTAssertEqual(ChatOutput.tail("a\nb").text, "a\nb")
        XCTAssertEqual(ChatOutput.tail("a\n\nb\n", lines: 3).text, "a\n\nb")
        XCTAssertEqual(ChatOutput.tail("é\nü\nñ", lines: 2).text, "ü\nñ")
        XCTAssertEqual(ChatOutput.tail("é\nü\nñ", lines: 2).hidden, 1)
        let none = ChatOutput.tail("a\nb\nc", lines: 0)
        XCTAssertEqual(none.text, ""); XCTAssertEqual(none.hidden, 3)
    }
    func testAHugeOutputIsCutToTheCharacterBudgetAtALineBoundary() {
        let line = String(repeating: "x", count: 99) + "\n"
        let text = String(repeating: line, count: 1000)
        let tail = ChatOutput.tail(text, lines: 500, maxCharacters: 1000)
        XCTAssertLessThanOrEqual(tail.text.count, 1000)
        XCTAssertTrue(tail.text.hasPrefix("x"))
        XCTAssertEqual(tail.text.split(separator: "\n").count + tail.hidden, 1000, "every line is either shown or counted")
        XCTAssertTrue(tail.text.split(separator: "\n").allSatisfy { $0.count == 99 }, "no half lines when there is a whole one that fits")
        // One endless line is cut in the middle, because there is nothing better.
        let endless = ChatOutput.tail(String(repeating: "y", count: 5000), lines: 5, maxCharacters: 100)
        XCTAssertEqual(endless.text.count, 100)
    }
    func testLinesAreCountedWithOrWithoutAFinalBreak() {
        XCTAssertEqual(ChatOutput.lineCount(""), 0)
        XCTAssertEqual(ChatOutput.lineCount("a"), 1)
        XCTAssertEqual(ChatOutput.lineCount("a\n"), 1)
        XCTAssertEqual(ChatOutput.lineCount("a\nb"), 2)
        XCTAssertEqual(ChatOutput.lineCount("\n\n"), 2)
    }
    func testAToolCallIsSummarisedByWhatItWasAskedToDo() throws {
        func input(_ json: String) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: Data(json.utf8)) }
        XCTAssertEqual(ChatToolSummary.line(for: try input(#"{"file_path":"/a/b.swift","limit":10}"#)), "/a/b.swift")
        XCTAssertEqual(ChatToolSummary.line(for: try input(#"{"zeta":"last","alpha":"first","n":1}"#)), "first", "no usual key: the first string, in key order")
        XCTAssertEqual(ChatToolSummary.line(for: try input(#"{"command":"ls\n  -la","path":"/x"}"#)), "ls -la", "the usual keys have a rank, and a line break is a space")
        XCTAssertEqual(ChatToolSummary.line(for: try input(#"{"x":1,"y":[2,3]}"#)), #"{"x":1,"y":[2,3]}"#)
        XCTAssertEqual(ChatToolSummary.line(for: try input(#""just text""#)), "just text")
        XCTAssertEqual(ChatToolSummary.line(for: try input("[1,2]")), "[1,2]")
        XCTAssertEqual(ChatToolSummary.line(for: .null), "")
        XCTAssertEqual(ChatToolSummary.line(for: try input(#"{"query":"\#(String(repeating: "q", count: 300))"}"#), limit: 20), String(repeating: "q", count: 19) + "…")
        XCTAssertEqual(ChatToolSummary.pretty(try input(#"{"b":[1],"a":"x/y"}"#)), "{\n  \"a\" : \"x/y\",\n  \"b\" : [\n    1\n  ]\n}")
        XCTAssertTrue(ChatToolSummary.pretty(try input(#"{"a":"\#(String(repeating: "z", count: 9000))"}"#)).hasSuffix("\n…"))
    }
    func testEveryItemHasALineForVoiceOver() {
        let step = { (s: ChatStepStatus) in ChatStep(text: "x", status: s) }
        let bodies: [ChatItemBody] = [
            .userMessage("hi"), .agentMessage("hello"), .reasoning("..."), .plan(explanation: nil, steps: [step(.completed), step(.pending)]),
            .command(command: "ls", cwd: nil, output: "", exitCode: 2), .fileChange([ChatFileChange(path: "a", kind: .add), ChatFileChange(path: "b", kind: .modify)]),
            .toolCall(server: "cua", tool: "click", input: .null, output: nil), .webSearch("q"), .todo([step(.completed)]), .compaction, .notice(level: .error, text: "bad")
        ]
        XCTAssertEqual(bodies.map(\.summary), ["You: hi", "hello", "Thinking", "Plan, 1 of 2 steps done", "Command: ls, exit 2", "Edit: 2 files", "Tool: cua click", "Web search: q", "To-do, 1 of 1 done", "Context compacted", "bad"])
        XCTAssertEqual(ChatItemBody.command(command: "ls", cwd: nil, output: "", exitCode: 0).summary, "Command: ls")
        XCTAssertEqual(ChatItemBody.fileChange([ChatFileChange(path: "a.rs", kind: .add)]).summary, "Edit: a.rs")
        XCTAssertEqual([ChatItemStatus.inProgress, .completed, .failed, .declined, .interrupted].map(\.spoken), ["running", "done", "failed", "declined", "interrupted"])
    }
}
