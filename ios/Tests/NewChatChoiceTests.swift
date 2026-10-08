import XCTest
@testable import RiWorkCore

/// The model, effort and Fast of a new chat: what the phone can offer before a chat exists (the default, and the model last used), what it
/// sends, what it remembers, and how the New terminal sheet's keyboard moves through it.
final class NewChatChoiceTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let opus = ChatModelOption(id: "opus", name: "Claude Opus 4.1", description: "Most capable", efforts: ["low", "medium", "high"], defaultEffort: "medium", supportsFast: true)
    private let haiku = ChatModelOption(id: "haiku", name: "Claude Haiku", efforts: [], supportsFast: false)
    private var targets: [NewTerminalTarget] { [.project(id: project, name: "Alpha")] }
    private func form(_ kind: NewTerminalKind = .chat, _ provider: ChatProvider = .claude) -> NewTerminalForm {
        NewTerminalForm(targets: targets, kind: kind, kinds: NewTerminalKind.allCases, chatProvider: provider)
    }
    private func request(_ form: NewTerminalForm) throws -> ChatCreateRequest {
        guard case .chat(let request) = try form.submission() else { throw XCTSkip("not a chat") }
        return request
    }

    // MARK: What is offered and what is sent

    func testWithNothingChosenTheMacsDefaultStaysItsOwn() throws {
        let choice = NewChatChoice()
        XCTAssertNil(choice.chosen); XCTAssertNil(choice.requestedModel); XCTAssertNil(choice.requestedEffort); XCTAssertFalse(choice.requestedFast)
        XCTAssertNil(choice.summary)
        XCTAssertEqual(try request(form()).params, ["provider": .string("claude"), "project_id": .string(project)], "no model, no effort, no fast, no mode")
    }
    func testTheLastModelIsOfferedWithItsEffortsAndFast() {
        var choice = NewChatChoice(model: opus)
        XCTAssertFalse(choice.usesModel, "a model that was used is offered, not chosen for you")
        XCTAssertTrue(choice.efforts.isEmpty); XCTAssertFalse(choice.showsFast)
        choice.useLastModel()
        XCTAssertEqual(choice.chosen, opus)
        XCTAssertEqual(choice.efforts, ["low", "medium", "high"])
        XCTAssertEqual(choice.selectedEffort, "medium", "its default is preselected")
        XCTAssertTrue(choice.showsFast)
        choice.useDefault()
        XCTAssertNil(choice.chosen); XCTAssertEqual(choice.model, opus, "the model stays offered")
        XCTAssertFalse(NewChatChoice(usesModel: true).usesModel, "there is no second row without a model")
        var none = NewChatChoice(); none.useLastModel()
        XCTAssertNil(none.chosen)
    }
    func testAModelEffortAndFastAreSentOnlyWhereTheyWereChosen() throws {
        var choice = NewChatChoice(model: opus, usesModel: true)
        XCTAssertEqual(choice.requestedModel, "opus")
        XCTAssertNil(choice.requestedEffort, "the default effort is the provider's, not spelled out")
        XCTAssertFalse(choice.requestedFast)
        choice.choose(effort: "HIGH"); choice.setFast(true)
        XCTAssertEqual(choice.selectedEffort, "high")
        XCTAssertEqual(choice.requestedEffort, "high")
        XCTAssertTrue(choice.requestedFast)
        XCTAssertEqual(choice.summary, "Opus 4.1 · High · Fast")
        let request = try ChatCreateRequest(provider: .claude, target: .project(project), choice: choice)
        XCTAssertEqual(request.params, ["provider": .string("claude"), "project_id": .string(project), "model": .string("opus"), "effort": .string("high"), "fast": .bool(true)])
        // Not a model's effort: not taken.
        choice.choose(effort: "max")
        XCTAssertEqual(choice.selectedEffort, "high")
        // The default has none of it, whatever was set for the model.
        choice.useDefault()
        XCTAssertNil(choice.requestedModel); XCTAssertNil(choice.requestedEffort); XCTAssertFalse(choice.requestedFast, "Fast belongs to the model that was chosen")
        XCTAssertEqual(try ChatCreateRequest(provider: .claude, target: .project(project), choice: choice).params, ["provider": .string("claude"), "project_id": .string(project)])
    }
    func testFastNeedsAModelThatHasIt() {
        var choice = NewChatChoice(model: haiku, usesModel: true, fast: true)
        XCTAssertFalse(choice.showsFast); XCTAssertFalse(choice.requestedFast, "a leftover on from another model is not sent")
        XCTAssertEqual(choice.summary, "Haiku")
        choice = NewChatChoice(model: opus, usesModel: true, effort: "xhigh")
        XCTAssertEqual(choice.selectedEffort, "medium", "an effort the model does not take falls back to its default")
        XCTAssertNil(choice.requestedEffort)
    }

    // MARK: Remembering

    func testWhatIsChosenInsideAChatIsWhatTheNextChatStartsWith() {
        let list = [opus, haiku]
        let inChat = ChatModelChoices(models: list, model: "opus", effort: "high", fast: true)
        let remembered = NewChatChoice().remembering(inChat)
        XCTAssertEqual(remembered, NewChatChoice(model: opus, usesModel: true, effort: "high", fast: true))
        // With no effort chosen it stays the model's default; a model that has no Fast remembers none.
        XCTAssertEqual(NewChatChoice().remembering(ChatModelChoices(models: list, model: "opus", effort: nil, fast: false)).effort, "medium")
        XCTAssertFalse(NewChatChoice().remembering(ChatModelChoices(models: list, model: "haiku", effort: nil, fast: true)).fast)
        // A chat whose model the list does not know changes nothing.
        let before = NewChatChoice(model: haiku, usesModel: false)
        XCTAssertEqual(before.remembering(ChatModelChoices(models: list, model: "o9", effort: nil, fast: false)), before)
    }
    func testAChoiceSurvivesTheStoreAndADamagedOneIsTheDefault() throws {
        let choice = NewChatChoice(model: opus, usesModel: true, effort: "low", fast: true)
        XCTAssertEqual(try JSONDecoder().decode(NewChatChoice.self, from: JSONEncoder().encode(choice)), choice)
        for damaged in ["{}", #"{"model":7,"uses_model":"yes","effort":3,"fast":"on"}"#, #"{"model":{"name":"no id"},"uses_model":true}"#] {
            XCTAssertEqual(try JSONDecoder().decode(NewChatChoice.self, from: Data(damaged.utf8)), NewChatChoice(), damaged)
        }
        // Chosen, but its model did not survive: the default.
        XCTAssertFalse(try JSONDecoder().decode(NewChatChoice.self, from: Data(#"{"uses_model":true}"#.utf8)).usesModel)
        // A model stored by an older phone, with fields it did not know yet.
        let old = try JSONDecoder().decode(NewChatChoice.self, from: Data(#"{"model":{"id":"opus","name":"Opus"},"uses_model":true}"#.utf8))
        XCTAssertEqual(old.chosen, ChatModelOption(id: "opus", name: "Opus"))
    }

    // MARK: In the sheet

    func testOnlyAChatHasModelControlsAndTheyAppearWithWhatTheChoiceNeeds() {
        XCTAssertEqual(form(.shell).fields, [.target, .kind, .create])
        XCTAssertEqual(form(.claude).fields, [.target, .kind, .unrestricted, .create], "a terminal agent has no model row")
        XCTAssertNil(form(.shell).chatChoice)
        var chat = form()
        XCTAssertEqual(chat.fields, [.target, .kind, .chatModel, .unrestricted, .create], "only the model row until a model is chosen")
        chat.chatChoices[.claude] = NewChatChoice(model: opus)
        XCTAssertEqual(chat.fields, [.target, .kind, .chatModel, .unrestricted, .create], "offered, not chosen")
        chat.selectChatModel(last: true)
        XCTAssertEqual(chat.fields, [.target, .kind, .chatModel, .chatEffort, .chatFast, .unrestricted, .create])
        chat.chatChoices[.claude] = NewChatChoice(model: haiku, usesModel: true)
        XCTAssertEqual(chat.fields, [.target, .kind, .chatModel, .unrestricted, .create], "Haiku has neither")
    }
    func testUpAndDownChooseWithinTheModelRowsAndTheEffortsAndSpaceFlipsFast() {
        var chat = form()
        chat.chatChoices[.claude] = NewChatChoice(model: opus)
        chat.focus = .chatModel
        // The rows: Codex's default, Claude's default, Claude's last model (no list is known in this test).
        XCTAssertEqual(chat.chatRows, [.providerDefault(.codex), .providerDefault(.claude), .last(.claude, opus)])
        XCTAssertEqual(chat.chosenChatRow, .providerDefault(.claude))
        chat.handle(.down)
        XCTAssertEqual(chat.chatChoice?.usesModel, true); XCTAssertEqual(chat.kind, .chat, "the arrows did not change the kind")
        XCTAssertEqual(chat.chosenChatRow, .last(.claude, opus))
        XCTAssertEqual(chat.focus, .chatModel)
        chat.handle(.down); XCTAssertEqual(chat.chatProvider, .codex, "past the last row is the first, which is Codex's")
        XCTAssertEqual(chat.chosenChatRow, .providerDefault(.codex))
        chat.handle(.up); XCTAssertEqual(chat.chatProvider, .claude); XCTAssertEqual(chat.chatChoice?.usesModel, true)
        chat.handle(.up); XCTAssertEqual(chat.chatChoice?.usesModel, false)
        chat.handle(.down)
        // Efforts: medium is chosen; down goes to high and stops there.
        chat.handle(.right); XCTAssertEqual(chat.focus, .chatEffort)
        XCTAssertEqual(chat.chatChoice?.selectedEffort, "medium")
        chat.handle(.down); XCTAssertEqual(chat.chatChoice?.selectedEffort, "high")
        chat.handle(.down); XCTAssertEqual(chat.chatChoice?.selectedEffort, "high")
        chat.handle(.up); chat.handle(.up); chat.handle(.up)
        XCTAssertEqual(chat.chatChoice?.selectedEffort, "low")
        XCTAssertEqual(chat.chatChoice?.effort, "low")
        // Fast: space flips it, only there.
        chat.handle(.right); XCTAssertEqual(chat.focus, .chatFast)
        chat.handle(.space); XCTAssertEqual(chat.chatChoice?.fastIsOn, true)
        chat.handle(.space); XCTAssertEqual(chat.chatChoice?.fastIsOn, false)
        // Up and down on Fast choose the kind, as on the other toggle.
        chat.handle(.down)
        XCTAssertEqual(chat.kind, .shell); XCTAssertEqual(chat.focus, .kind)
        // Space elsewhere does nothing to the choice.
        var other = form()
        other.chatChoices[.claude] = NewChatChoice(model: opus, usesModel: true)
        other.focus = .kind; other.handle(.space)
        XCTAssertEqual(other.chatChoice?.fastIsOn, false)
    }
    func testTabWalksTheChatControlsAndSkipsTheOnesThatAreNotThere() {
        var chat = form()
        chat.chatChoices[.claude] = NewChatChoice(model: opus, usesModel: true)
        var seen: [NewTerminalForm.Field] = [chat.focus]
        for _ in 0..<6 { chat.handle(.tab); seen.append(chat.focus) }
        XCTAssertEqual(seen, [.kind, .chatModel, .chatEffort, .chatFast, .unrestricted, .create, .target])
        chat.focus = .chatFast
        chat.selectChatModel(last: false)
        XCTAssertEqual(chat.focus, .kind, "the ring leaves a control that went away")
    }
    func testEachProviderKeepsItsOwnChoiceAndTheRequestIsTheSelectedProviders() throws {
        var chat = form(.chat, .codex)
        let gpt = ChatModelOption(id: "gpt-5.5", name: "GPT-5.5", efforts: ["low", "high"], defaultEffort: "low", supportsFast: true)
        chat.chatChoices = [.codex: NewChatChoice(model: gpt, usesModel: true, effort: "high", fast: true), .claude: NewChatChoice(model: opus, usesModel: true)]
        XCTAssertEqual(try request(chat).params, ["provider": .string("codex"), "project_id": .string(project), "model": .string("gpt-5.5"), "effort": .string("high"), "fast": .bool(true)])
        chat.selectChat(.claude)
        XCTAssertEqual(try request(chat).params, ["provider": .string("claude"), "project_id": .string(project), "model": .string("opus")])
        chat.setUnrestricted(true)
        XCTAssertEqual(try request(chat).approvalMode, .full, "the mode and the model are separate choices")
        chat.selectChat(.codex)
        chat.selectChatEffort("low")
        XCTAssertEqual(chat.chatChoices[.codex]?.effort, "low"); XCTAssertNil(chat.chatChoices[.claude]?.effort)
        chat.select(kind: .shell)
        XCTAssertNoThrow(try chat.request(), "a terminal is unaffected")
        chat.selectChatEffort("high")
        XCTAssertEqual(chat.chatChoices[.codex]?.effort, "low", "no chat is selected: nothing changed")
    }

    // MARK: One list for both providers

    func testBothProvidersAreOneListAndTheRowChosenIsTheProvider() throws {
        let gpt = ChatModelOption(id: "gpt-5.5", name: "GPT-5.5", efforts: ["low", "high"], defaultEffort: "low", supportsFast: true, isDefault: true)
        let mini = ChatModelOption(id: "gpt-5.4-mini", name: "GPT-5.4 mini", efforts: ["low"], defaultEffort: "low")
        var chat = form(.chat, .codex)
        chat.chatModels = [.codex: [gpt, mini], .claude: [opus, haiku]]
        chat.chatChoices[.claude] = NewChatChoice(model: opus)
        XCTAssertEqual(chat.chatRows, [.providerDefault(.codex), .model(.codex, gpt), .model(.codex, mini), .providerDefault(.claude), .model(.claude, opus), .model(.claude, haiku)],
                       "Codex first, each provider under its default, no 'last used' row while the list is known")
        XCTAssertEqual(Set(chat.chatRows.map(\.id)).count, chat.chatRows.count, "row ids are unique")
        chat.chooseChatRow(.model(.claude, haiku))
        XCTAssertEqual(chat.chatProvider, .claude)
        XCTAssertEqual(chat.chosenChatRow, .model(.claude, haiku))
        XCTAssertEqual(try request(chat).params, ["provider": .string("claude"), "project_id": .string(project), "model": .string("haiku")])
        XCTAssertNil(chat.chatChoices[.codex]?.chosen, "the other provider's choice is untouched")
        chat.chooseChatRow(.model(.codex, mini))
        XCTAssertEqual(try request(chat).params, ["provider": .string("codex"), "project_id": .string(project), "model": .string("gpt-5.4-mini"), "effort": .string("low")],
                       "a model is chosen with its own effort, as in a chat")
        XCTAssertEqual(chat.chatChoices[.claude]?.chosen, haiku, "and each keeps its own")
        chat.chooseChatRow(.providerDefault(.codex))
        XCTAssertEqual(try request(chat).params, ["provider": .string("codex"), "project_id": .string(project)])
        // A model a provider's list no longer names is chosen still, but no row shows it.
        chat.chatChoices[.claude] = NewChatChoice(model: ChatModelOption(id: "gone", name: "Gone"), usesModel: true)
        chat.selectChatProvider(.claude)
        XCTAssertNil(chat.chosenChatRow)
        XCTAssertEqual(try request(chat).model, "gone")
        // A terminal has no rows chosen and ignores a row.
        var shell = form(.shell, .codex)
        shell.chooseChatRow(.providerDefault(.claude))
        XCTAssertNil(shell.chosenChatRow); XCTAssertEqual(shell.chatProvider, .codex, "unchanged")
    }
}
