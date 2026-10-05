import XCTest
@testable import RiWorkCore

/// The model picker's decisions: which model a chat runs, what the chip says, which efforts and Fast are on offer, what each choice sends,
/// and what a keyboard does in the sheet.
final class ChatModelsTests: XCTestCase {
    /// A Codex-like list: the default has efforts and Fast, the second has efforts only, the third has nothing to choose.
    private let gpt = ChatModelOption(id: "gpt-5.5", name: "GPT-5.5", description: "Frontier", efforts: ["low", "medium", "high", "xhigh"], defaultEffort: "medium", supportsFast: true, isDefault: true)
    private let mini = ChatModelOption(id: "gpt-5.4-mini", name: "GPT-5.4 mini", description: "Faster", efforts: ["low", "medium"], defaultEffort: "low")
    private let bare = ChatModelOption(id: "bare", name: "Bare")
    private var list: [ChatModelOption] { [gpt, mini, bare] }
    private func choices(model: String? = nil, effort: String? = nil, fast: Bool = false, models: [ChatModelOption]? = nil) -> ChatModelChoices {
        ChatModelChoices(models: models ?? list, model: model, effort: effort, fast: fast)
    }
    private func transcript(_ events: [ChatEvent]) -> ChatTranscript {
        var t = ChatTranscript()
        for event in events { t.apply(event) }
        return t
    }

    // MARK: Words

    func testAModelsShortNameDropsWhatTheChipHasNoRoomFor() {
        XCTAssertEqual(ChatModelOption(id: "opus", name: "Claude Opus 4.1").shortName, "Opus 4.1")
        XCTAssertEqual(ChatModelOption(id: "default", name: "Default (recommended)").shortName, "Default")
        XCTAssertEqual(ChatModelOption(id: "opus[1m]", name: "Opus (1M context)").shortName, "Opus (1M context)", "a parenthesis that says what the model is stays")
        XCTAssertEqual(ChatModelOption(id: "gpt-5.5", name: "GPT-5.5").shortName, "GPT-5.5")
        XCTAssertEqual(ChatModelOption(id: "claude", name: "Claude").shortName, "Claude", "nothing left to keep otherwise")
        XCTAssertEqual(ChatModelOption(id: "x", name: " claude  Sonnet 4 ").shortName, "Sonnet 4", "spaces around it go too")
    }
    func testEffortsAreNamedForPeopleAndSpokenInFull() {
        XCTAssertEqual(["minimal", "low", "medium", "high", "xhigh", "max"].map(ChatEffort.title), ["Minimal", "Low", "Medium", "High", "X-High", "Max"])
        XCTAssertEqual(ChatEffort.title("HIGH"), "High")
        XCTAssertEqual(ChatEffort.title("ultra_think"), "Ultra think")
        XCTAssertEqual(ChatEffort.title(""), "")
        XCTAssertEqual(ChatEffort.spoken("xhigh"), "Extra high")
        XCTAssertEqual(ChatEffort.spoken("low"), "Low")
        XCTAssertEqual(gpt.effort(matching: "HIGH"), "high")
        XCTAssertNil(gpt.effort(matching: "max"))
        XCTAssertEqual(gpt.defaultEffortChoice, "medium")
        XCTAssertNil(ChatModelOption(id: "m", name: "M", efforts: ["low"], defaultEffort: "high").defaultEffortChoice, "a default that is not one of the efforts is not a segment")
    }

    // MARK: Which model

    func testTheModelTheChatRunsIsFoundInTheList() {
        XCTAssertEqual(choices(model: "gpt-5.4-mini").current, mini)
        XCTAssertEqual(choices(model: "gpt-5.4-mini").currentIndex, 1)
        XCTAssertEqual(choices(model: "GPT-5.4-MINI").current, mini, "ignoring case")
        XCTAssertEqual(choices().current, gpt, "no model named: the provider's default")
        XCTAssertEqual(choices(model: "  ").current, gpt)
        XCTAssertNil(choices(model: "o9").current, "a model the list does not know")
        XCTAssertNil(choices(models: [mini, bare]).current, "no model named and none marked default")
        XCTAssertNil(choices(models: []).current)
    }
    func testAFullModelNameFindsItsAliasInTheListLongestFirst() {
        let claude = [ChatModelOption(id: "default", name: "Default (recommended)", isDefault: true), ChatModelOption(id: "opus", name: "Opus"),
                      ChatModelOption(id: "sonnet", name: "Sonnet"), ChatModelOption(id: "opus-plan", name: "Opus plan")]
        XCTAssertEqual(choices(model: "claude-opus-4-1-20250805", models: claude).current?.id, "opus")
        XCTAssertEqual(choices(model: "claude-sonnet-4-5", models: claude).current?.id, "sonnet")
        XCTAssertEqual(choices(model: "claude-opus-plan-4", models: claude).current?.id, "opus-plan", "the longest id that is inside")
        XCTAssertEqual(choices(model: "default", models: claude).current?.id, "default")
        XCTAssertNil(choices(model: "claude-haiku-4", models: claude).current, "“default” is not a word to find inside another name")
    }
    func testDuplicatesInAListAreOneModel() {
        XCTAssertEqual(choices(models: [gpt, gpt, mini]).models.map(\.id), ["gpt-5.5", "gpt-5.4-mini"])
    }
    func testWithoutAListThereIsNoPickerAtAll() {
        XCTAssertFalse(choices(models: []).isAvailable)
        XCTAssertTrue(choices().isAvailable)
        let none = ChatModelChoices(transcript: ChatTranscript(), fallback: ChatInfo(id: "c", provider: .codex, model: "gpt-5", fast: true))
        XCTAssertFalse(none.isAvailable, "an older desktop: the toolbar keeps what it had")
    }
    func testTheChoicesComeFromTheTranscriptWithAnUnconfirmedChoiceLaidOver() {
        var info = ChatInfo(id: "c", provider: .codex, model: "gpt-5.4-mini", effort: "low", fast: false, state: .idle)
        let t = transcript([.models(list), .info(info)])
        let plain = ChatModelChoices(transcript: t)
        XCTAssertEqual([plain.modelID, plain.effort], ["gpt-5.4-mini", "low"]); XCTAssertFalse(plain.fast)
        let over = ChatModelChoices(transcript: t, pending: .init(model: "gpt-5.5", fast: true))
        XCTAssertEqual([over.modelID, over.effort], ["gpt-5.5", "low"], "what was not chosen stays what the chat says")
        XCTAssertTrue(over.fast)
        // Before the chat's own info is there, the tab list's entry stands in.
        info.model = "bare"
        XCTAssertEqual(ChatModelChoices(transcript: transcript([.models(list)]), fallback: info).modelID, "bare")
    }

    // MARK: Efforts

    func testOnlyTheChosenModelsEffortsAreOfferedWithItsDefaultPreselected() {
        XCTAssertEqual(choices().efforts, ["low", "medium", "high", "xhigh"])
        XCTAssertEqual(choices().selectedEffort, "medium", "no effort chosen: the model's default")
        XCTAssertEqual(choices().selectedEffortIndex, 1)
        XCTAssertEqual(choices(model: "gpt-5.4-mini").efforts, ["low", "medium"], "only this model's")
        XCTAssertEqual(choices(model: "gpt-5.4-mini").selectedEffort, "low")
        XCTAssertEqual(choices(effort: "xhigh").selectedEffort, "xhigh")
        XCTAssertEqual(choices(effort: "HIGH").selectedEffort, "high")
        XCTAssertEqual(choices(model: "gpt-5.4-mini", effort: "xhigh").selectedEffort, "low", "an effort the model does not take falls back to its default")
        XCTAssertEqual(choices(model: "bare").efforts, [])
        XCTAssertNil(choices(model: "bare", effort: "high").selectedEffort, "nothing to choose, nothing selected")
        XCTAssertEqual(choices(model: "o9").efforts, [], "a model the list does not know has none to show")
        XCTAssertNil(choices(model: "o9", effort: "high").selectedEffort)
    }

    // MARK: Fast

    func testFastIsShownOnlyForAModelThatHasIt() {
        XCTAssertTrue(choices().showsFast)
        XCTAssertFalse(choices(model: "gpt-5.4-mini").showsFast)
        XCTAssertFalse(choices(model: "bare").showsFast)
        XCTAssertFalse(choices(model: "o9").showsFast)
        XCTAssertTrue(choices(fast: true).fastIsOn)
        XCTAssertFalse(choices().fastIsOn)
        XCTAssertFalse(choices(model: "gpt-5.4-mini", fast: true).fastIsOn, "on in the chat, but this model has none")
    }

    // MARK: The chip

    func testTheChipSaysTheModelAndABoltWhenFastIsOn() {
        XCTAssertEqual(choices().chipText, "GPT-5.5", "the default model while none is named")
        XCTAssertEqual(choices(fast: true).chipText, "GPT-5.5 ⚡")
        XCTAssertTrue(choices(fast: true).chipShowsFast)
        XCTAssertEqual(choices(model: "gpt-5.4-mini", fast: true).chipText, "GPT-5.4 mini", "no bolt for a model without Fast, even if the chat says so")
        XCTAssertEqual(choices(model: "o9").chipText, "o9", "a model the list does not know is shown as the chat names it")
        XCTAssertEqual(choices(model: "o9", fast: true).chipText, "o9 ⚡", "and its Fast is believed")
        XCTAssertEqual(choices(models: [mini]).chipText, "Model", "nothing known at all")
        XCTAssertEqual(ChatModelChoices(models: [ChatModelOption(id: "opus", name: "Claude Opus 4.1")], model: "opus", effort: nil, fast: false).chipText, "Opus 4.1")
    }
    func testTheChipIsSpokenWithItsEffortAndFast() {
        XCTAssertEqual(choices(effort: "xhigh", fast: true).spoken, "GPT-5.5, Extra high effort, Fast on")
        XCTAssertEqual(choices(model: "gpt-5.4-mini").spoken, "GPT-5.4 mini, Low effort")
        XCTAssertEqual(choices(model: "bare").spoken, "Bare")
    }

    // MARK: Choosing: one Configure per change

    func testChoosingAModelSendsItAlone() {
        let change = choices().configuration(for: .model("gpt-5.4-mini"))
        XCTAssertEqual(change, .init(model: "gpt-5.4-mini"))
        XCTAssertEqual(change?.command, .configure(model: "gpt-5.4-mini"))
        XCTAssertNil(choices(model: "gpt-5.4-mini").configuration(for: .model("gpt-5.4-mini")), "already running: nothing to send")
        XCTAssertNil(choices().configuration(for: .model("gpt-5.5")), "the default, while none is named, is already running")
        XCTAssertNil(choices().configuration(for: .model("nope")), "not on offer")
        XCTAssertEqual(choices(model: "o9").configuration(for: .model("gpt-5.5")), .init(model: "gpt-5.5"), "from a model the list does not know")
    }
    func testChoosingAModelCarriesWhatTheNewModelCannotKeepInTheSameCommand() {
        // An effort the new model does not take becomes its default; one it takes stays the person's.
        XCTAssertEqual(choices(effort: "xhigh").configuration(for: .model("gpt-5.4-mini")), .init(model: "gpt-5.4-mini", effort: "low"))
        XCTAssertEqual(choices(effort: "medium").configuration(for: .model("gpt-5.4-mini")), .init(model: "gpt-5.4-mini"))
        XCTAssertEqual(choices(effort: "xhigh").configuration(for: .model("bare")), .init(model: "bare"), "no efforts to choose: nothing to name")
        XCTAssertEqual(choices().configuration(for: .model("gpt-5.4-mini")), .init(model: "gpt-5.4-mini"), "no effort chosen: the new model's default applies by itself")
        // Fast goes off with a model that has none.
        XCTAssertEqual(choices(fast: true).configuration(for: .model("gpt-5.4-mini")), .init(model: "gpt-5.4-mini", fast: false))
        XCTAssertEqual(choices(effort: "xhigh", fast: true).configuration(for: .model("gpt-5.4-mini")), .init(model: "gpt-5.4-mini", effort: "low", fast: false))
        XCTAssertEqual(choices(model: "bare", fast: true).configuration(for: .model("gpt-5.5")), .init(model: "gpt-5.5", fast: nil), "a model with Fast keeps it as it is")
    }
    func testChoosingAnEffortOrFastSendsThatAlone() {
        XCTAssertEqual(choices().configuration(for: .effort("high"))?.command, .configure(effort: "high"))
        XCTAssertEqual(choices().configuration(for: .effort("HIGH")), .init(effort: "high"), "sent as the model spells it")
        XCTAssertNil(choices(effort: "high").configuration(for: .effort("high")), "already chosen")
        XCTAssertEqual(choices().configuration(for: .effort("medium")), .init(effort: "medium"), "the preselected default is pinned when asked for")
        XCTAssertNil(choices().configuration(for: .effort("max")), "the model does not take it")
        XCTAssertNil(choices(model: "bare").configuration(for: .effort("high")))
        XCTAssertEqual(choices().configuration(for: .fast(true))?.command, .configure(fast: true))
        XCTAssertEqual(choices(fast: true).configuration(for: .fast(false))?.command, .configure(fast: false))
        XCTAssertNil(choices(fast: true).configuration(for: .fast(true)))
        XCTAssertNil(choices().configuration(for: .fast(false)))
        XCTAssertNil(choices(model: "gpt-5.4-mini").configuration(for: .fast(true)), "no Fast on this model")
    }
    func testAChoiceIsShownAtOnceBeforeTheDesktopConfirmsIt() throws {
        let start = choices(effort: "xhigh", fast: true)
        let configuration = try XCTUnwrap(start.configuration(for: .model("gpt-5.4-mini")))
        let after = start.applying(configuration)
        XCTAssertEqual([after.modelID, after.effort], ["gpt-5.4-mini", "low"])
        XCTAssertFalse(after.fast)
        XCTAssertEqual(after.selectedEffort, "low")
        XCTAssertFalse(after.showsFast)
        // Two choices before the desktop answers: the later wins where they overlap, the earlier stays where they do not.
        let first = ChatModelChoices.Configuration(model: "gpt-5.5", effort: "low")
        let merged = ChatModelChoices.Configuration(effort: "high", fast: true).merged(over: first)
        XCTAssertEqual(merged, .init(model: "gpt-5.5", effort: "high", fast: true))
        XCTAssertEqual(ChatModelChoices.Configuration(fast: true).merged(over: nil), .init(fast: true))
    }
    func testAChoiceIsSettledWhenTheChatsOwnInfoHasIt() {
        let info = ChatInfo(id: "c", provider: .codex, model: "gpt-5.4-mini", effort: "low", fast: true)
        XCTAssertTrue(ChatModelChoices.Configuration(model: "gpt-5.4-mini", effort: "low", fast: true).isSettled(by: info, models: list))
        XCTAssertTrue(ChatModelChoices.Configuration(effort: "LOW").isSettled(by: info, models: list))
        XCTAssertFalse(ChatModelChoices.Configuration(model: "gpt-5.5").isSettled(by: info, models: list))
        XCTAssertFalse(ChatModelChoices.Configuration(fast: false).isSettled(by: info, models: list))
        XCTAssertFalse(ChatModelChoices.Configuration(effort: "high").isSettled(by: info, models: list))
        // Claude may name the model in full where the phone sent the alias.
        let claude = [ChatModelOption(id: "opus", name: "Opus")]
        XCTAssertTrue(ChatModelChoices.Configuration(model: "opus").isSettled(by: ChatInfo(id: "c", provider: .claude, model: "claude-opus-4-1-20250805"), models: claude))
        XCTAssertFalse(ChatModelChoices.Configuration(model: "opus").isSettled(by: ChatInfo(id: "c", provider: .claude, model: nil), models: claude))
    }

    // MARK: The keyboard

    private func press(_ keys: [ChatModelCursor.Key], on choices: ChatModelChoices, from start: ChatModelCursor? = nil) -> (ChatModelCursor, [ChatModelChoices.Change]) {
        var cursor = start ?? ChatModelCursor(for: choices)
        var made: [ChatModelChoices.Change] = []
        for key in keys { if let change = cursor.handle(key, in: choices) { made.append(change) } }
        return (cursor, made)
    }

    func testTheRingStartsOnTheModelTheChatRuns() {
        XCTAssertEqual(ChatModelCursor(for: choices()).stop, .model(0))
        XCTAssertEqual(ChatModelCursor(for: choices(model: "gpt-5.4-mini")).stop, .model(1))
        XCTAssertEqual(ChatModelCursor(for: choices(model: "o9")).stop, .model(0), "or the first")
        XCTAssertEqual(ChatModelCursor(for: choices()).effortIndex, 1, "and, on the efforts, the one chosen")
    }
    func testTheStopsAreTheModelsThenTheEffortsThenFastWhereTheyExist() {
        let cursor = ChatModelCursor(for: choices())
        XCTAssertEqual(cursor.stops(in: choices()), [.model(0), .model(1), .model(2), .effort, .fast])
        XCTAssertEqual(cursor.stops(in: choices(model: "gpt-5.4-mini")), [.model(0), .model(1), .model(2), .effort], "no Fast on this model")
        XCTAssertEqual(cursor.stops(in: choices(model: "bare")), [.model(0), .model(1), .model(2)])
        XCTAssertEqual(cursor.stops(in: choices(models: [])), [])
    }
    func testDownAndTabWalkTheRowsAndWrapUpAndShiftTabWalkBack() {
        let c = choices()
        XCTAssertEqual(press([.down], on: c).0.stop, .model(1))
        XCTAssertEqual(press([.tab, .tab, .tab], on: c).0.stop, .effort)
        XCTAssertEqual(press([.down, .down, .down, .down], on: c).0.stop, .fast)
        XCTAssertEqual(press([.down, .down, .down, .down, .down], on: c).0.stop, .model(0), "wraps")
        XCTAssertEqual(press([.up], on: c).0.stop, .fast, "up from the first wraps to the last")
        XCTAssertEqual(press([.backTab, .backTab], on: c).0.stop, .effort)
        XCTAssertTrue(press([.down, .up, .left, .right, .tab], on: c).1.isEmpty, "moving chooses nothing")
    }
    func testLeftAndRightWalkTheEffortsAndStopAtTheEnds() {
        let c = choices()
        var (cursor, _) = press([.down, .down, .down], on: c)
        XCTAssertEqual(cursor.stop, .effort)
        XCTAssertEqual(cursor.effortIndex, 1, "on arriving, the effort that is chosen")
        _ = cursor.handle(.right, in: c); _ = cursor.handle(.right, in: c); _ = cursor.handle(.right, in: c)
        XCTAssertEqual(cursor.effortIndex, 3, "the last, not past it")
        for _ in 0..<5 { _ = cursor.handle(.left, in: c) }
        XCTAssertEqual(cursor.effortIndex, 0)
        // Left and right mean nothing on a model row.
        var onModel = ChatModelCursor(for: c)
        XCTAssertNil(onModel.handle(.right, in: c)); XCTAssertNil(onModel.handle(.left, in: c))
        XCTAssertEqual(onModel.stop, .model(0))
    }
    func testReturnAndSpaceChooseWhatTheRingIsOn() {
        let c = choices()
        XCTAssertEqual(press([.down, .return], on: c).1, [.model("gpt-5.4-mini")])
        XCTAssertEqual(press([.down, .space], on: c).1, [.model("gpt-5.4-mini")])
        XCTAssertEqual(press([.tab, .tab, .tab, .right, .return], on: c).1, [.effort("high")])
        XCTAssertEqual(press([.up, .return], on: c).1, [.fast(true)], "Fast flips")
        XCTAssertEqual(press([.up, .return], on: choices(fast: true)).1, [.fast(false)])
        XCTAssertEqual(press([.return], on: c).1, [.model("gpt-5.5")], "Return on the model that runs is a choice the screen then finds already made")
        XCTAssertNil(c.configuration(for: .model("gpt-5.5")))
    }
    func testTheRingIsPutBackWhenTheRowsChange() {
        // The ring is on Fast; the model changes to one without it.
        let withFast = choices()
        let (onFast, _) = press([.up], on: withFast)
        XCTAssertEqual(onFast.stop, .fast)
        var cursor = onFast
        cursor.reconcile(with: choices(model: "gpt-5.4-mini"))
        XCTAssertEqual(cursor.stop, .model(0))
        // The effort index is kept inside the efforts there now.
        var onEfforts = ChatModelCursor(for: withFast)
        onEfforts.place(.effort, effort: 3, in: withFast)
        onEfforts.reconcile(with: choices(model: "gpt-5.4-mini"))
        XCTAssertEqual(onEfforts.stop, .effort)
        XCTAssertEqual(onEfforts.effortIndex, 1)
        // A list that went away leaves no key to do anything.
        var empty = ChatModelCursor(for: withFast)
        XCTAssertNil(empty.handle(.return, in: choices(models: [])))
        // A tap puts the ring where the finger is.
        var tapped = ChatModelCursor(for: withFast)
        tapped.place(.model(2), in: withFast)
        XCTAssertEqual(tapped.stop, .model(2))
        tapped.place(.fast, in: choices(model: "bare"))
        XCTAssertEqual(tapped.stop, .model(0), "Fast is not there for this model")
    }
}
