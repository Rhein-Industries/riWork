import XCTest
@testable import RiWorkCore

/// The composer's `/`, `@` and `&` list: the shared rules against `Fixtures/composer-suggestions.json` (which the engine's
/// `crates/riwork-engine/src/chat/suggest_tests.rs` checks too), the `chat.suggestions` request and reply, the list as the composer drives
/// it, the catalog cache and the keys while the list is open.
final class ComposerSuggestionsTests: XCTestCase {
    private let chat = "c4a70000-0000-4000-8000-000000000001"
    private let requestID = "55555555-5555-4555-8555-555555555555"

    private func fixture() throws -> JSONValue {
        #if SWIFT_PACKAGE
        let bundle = Bundle.module
        #else
        let bundle = Bundle(for: Self.self)
        #endif
        let url = try XCTUnwrap(bundle.url(forResource: "composer-suggestions", withExtension: "json", subdirectory: "Fixtures")
                                ?? bundle.url(forResource: "composer-suggestions", withExtension: "json"))
        return try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: url))
    }
    private func value(_ json: String) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: Data(json.utf8)) }
    /// `text` without the caret mark, and the UTF-16 offset where it was.
    private func caret(_ text: String, _ mark: String) throws -> (String, Int) {
        let range = try XCTUnwrap(text.range(of: mark), "every vector marks its caret")
        return (text.replacingCharacters(in: range, with: ""), text.utf16.distance(from: text.utf16.startIndex, to: range.lowerBound))
    }
    /// `text` with the mark put in at UTF-16 offset `at`.
    private func marked(_ text: String, at: Int, _ mark: String) -> String {
        let index = String.Index(utf16Offset: at, in: text)
        return String(text[..<index]) + mark + String(text[index...])
    }
    private func token(_ text: String, _ trigger: ComposerTrigger) -> String { (text as NSString).substring(with: trigger.range) }
    private func strings(_ value: JSONValue) -> [String] { value.array.compactMap(\.string) }
    private func number(_ value: JSONValue) -> Double? { if case .number(let number) = value { number } else { nil } }
    private func commands(_ names: [String]) -> [ComposerCommand] {
        names.map { ComposerCommand(name: $0, summary: "", sends: "", handler: .provider) }
    }

    // MARK: The shared rules

    func testTheTriggerRuleMatchesTheSharedVectors() throws {
        let fixture = try fixture()
        let mark = try XCTUnwrap(fixture["caret"].string)
        let vectors = fixture["triggers"].array
        XCTAssertGreaterThan(vectors.count, 20, "the fixture covers both sigils and the texts that open nothing")
        for vector in vectors {
            let (text, at) = try caret(try XCTUnwrap(vector["text"].string), mark)
            let found = ComposerSuggestions.trigger(in: text, caret: at)
            switch vector["expect"] {
            case .null:
                XCTAssertNil(found, "\(text.debugDescription) opens nothing")
            case let expect:
                let trigger = try XCTUnwrap(found, "\(text.debugDescription) opens a list")
                XCTAssertEqual(trigger.sigil.rawValue, expect["sigil"].string, text.debugDescription)
                XCTAssertEqual(trigger.query, expect["query"].string, text.debugDescription)
                XCTAssertEqual(token(text, trigger), expect["token"].string, text.debugDescription)
                XCTAssertEqual(trigger.caret, at)
                XCTAssertTrue(trigger.start < at && at <= trigger.end, "the caret is after the sigil and in the token: \(text.debugDescription)")
            }
        }
    }
    func testAcceptingMatchesTheSharedVectors() throws {
        let fixture = try fixture()
        let mark = try XCTUnwrap(fixture["caret"].string)
        XCTAssertFalse(fixture["accept"].array.isEmpty)
        for vector in fixture["accept"].array {
            let (text, at) = try caret(try XCTUnwrap(vector["text"].string), mark)
            let trigger = try XCTUnwrap(ComposerSuggestions.trigger(in: text, caret: at), "an accept vector is in a token: \(text.debugDescription)")
            let accepted = ComposerSuggestions.accept(text, trigger: trigger, insert: try XCTUnwrap(vector["insert"].string))
            XCTAssertEqual(marked(accepted.text, at: accepted.caret, mark), vector["expect"].string, text.debugDescription)
        }
    }
    func testTheCommandsTheFixtureListsAreReadAsTheDesktopWritesThem() throws {
        let fixture = try fixture()
        let claude = try fixture["commands"]["claude"].decode([ComposerCommand].self)
        XCTAssertEqual(claude.map(\.name), ["compact", "memo"])
        XCTAssertEqual(claude.first?.handler, .provider)
        XCTAssertNil(claude.first?.argument)
        XCTAssertEqual(claude.first?.insertion, "/compact")
        XCTAssertEqual(try fixture["commands"]["codex"].decode([ComposerCommand].self).map(\.name), ["memo"], "Codex reads a typed /compact as words, but /memo is RiWork's")
        XCTAssertEqual(try fixture["commands"]["muse"].decode([ComposerCommand].self).map(\.name), ["memo"])
    }
    func testCommandsRankAsTheSharedVectorsSay() throws {
        let table = try fixture()["command_ranking"]
        let list = commands(strings(table["commands"]))
        XCTAssertFalse(table["cases"].array.isEmpty)
        for vector in table["cases"].array {
            let query = try XCTUnwrap(vector["query"].string)
            XCTAssertEqual(ComposerSuggestions.rankCommands(query, list).map(\.name), strings(vector["expect"]), "query \(query.debugDescription)")
        }
    }
    func testFilesRankAsTheSharedVectorsSay() throws {
        let table = try fixture()["file_ranking"]
        let entries = try table["entries"].decode([ComposerFileEntry].self)
        let catalog = ComposerCatalog(chatID: chat, provider: .claude, commands: [], files: entries)
        XCTAssertFalse(table["cases"].array.isEmpty)
        for vector in table["cases"].array {
            let query = try XCTUnwrap(vector["query"].string)
            let limit = try XCTUnwrap(number(vector["limit"]).map { Int($0) })
            XCTAssertEqual(ComposerSuggestions.rankFiles(query, entries, limit: limit).map(\.path), strings(vector["expect"]), "query \(query.debugDescription)")
            XCTAssertEqual(catalog.rankFiles(query, limit: limit).map(\.path), strings(vector["expect"]), "a prepared catalog ranks the same: \(query.debugDescription)")
        }
    }
    func testInsertionsKindsAndSizesMatchTheSharedVectors() throws {
        let fixture = try fixture()
        XCTAssertFalse(fixture["insertions"].array.isEmpty || fixture["kinds"].array.isEmpty)
        for vector in fixture["insertions"].array {
            XCTAssertEqual(try vector["entry"].decode(ComposerFileEntry.self).insertion, vector["insert"].string)
        }
        for vector in fixture["kinds"].array {
            let entry = try vector["entry"].decode(ComposerFileEntry.self)
            XCTAssertEqual(entry.kind, vector["kind"].string)
            XCTAssertEqual(ComposerPreview(file: entry).kind, vector["kind"].string, "the card says the kind the row does")
        }
        XCTAssertFalse(fixture["sizes"].array.isEmpty)
        for vector in fixture["sizes"].array {
            let bytes = try XCTUnwrap(number(vector["bytes"]).map { UInt64($0) })
            XCTAssertEqual(ComposerSuggestions.sizeLabel(bytes), vector["label"].string, "\(bytes) bytes")
        }
    }
    func testThePhonesReplyReadsAsTheSharedFixture() throws {
        let fixture = try fixture()
        let reply = try ChatSuggestionsRequest(chatID: chat).parse(fixture["reply"])
        XCTAssertEqual(reply.chatID, chat)
        XCTAssertEqual(reply.provider, .claude)
        XCTAssertEqual(reply.root, "/Users/me/proj")
        XCTAssertEqual(reply.commands, try fixture["commands"]["claude"].decode([ComposerCommand].self), "a Claude chat's reply carries the Claude commands")
        XCTAssertEqual(reply.files, ComposerFileListing(entries: [ComposerFileEntry(path: "README.md", bytes: 2048), ComposerFileEntry(path: "src", dir: true),
                                                                  ComposerFileEntry(path: "src/main.rs", bytes: 900)]))
        XCTAssertEqual(reply.fileBound, 5000)
        XCTAssertEqual(reply.models, try fixture["reply"]["models"].decode([ComposerModelEntry].self))
        XCTAssertEqual(reply.models.map(\.id), ["default", "opus", "haiku"], "the provider's order, as the desktop sends it")
        XCTAssertEqual(reply.models.first?.isDefault, true)
        let catalog = ComposerCatalog(reply: reply)
        XCTAssertEqual(catalog.provider, .claude)
        XCTAssertEqual(catalog.rankFiles("ma").map(\.path), ["src/main.rs"])
        XCTAssertEqual(catalog.rankModels("").map(\.id), ["opus", "default", "haiku"], "the most used first")
    }

    // MARK: `&` models

    private func models(_ fixture: JSONValue) throws -> [ComposerModelEntry] {
        try fixture["model_ranking"]["models"].decode([ComposerModelEntry].self)
    }
    func testAModelIsPutInAsItsMentionAsTheSharedVectorsSay() throws {
        let vectors = try fixture()["model_insertions"].array
        XCTAssertFalse(vectors.isEmpty)
        for vector in vectors {
            let model = ComposerModelEntry(id: try XCTUnwrap(vector["id"].string), name: "Name")
            XCTAssertEqual(model.insertion, vector["insert"].string)
            XCTAssertEqual(ComposerSuggestion.model(model, current: true).insertion, vector["insert"].string, "the chat's own model goes in too")
            XCTAssertEqual(ComposerSuggestion.model(model, current: false).title, vector["insert"].string, "its row reads as what goes in")
        }
    }
    func testModelsRankByUseAsTheSharedVectorsSay() throws {
        let fixture = try fixture()
        let table = fixture["model_ranking"], list = try models(fixture)
        XCTAssertFalse(table["cases"].array.isEmpty)
        for vector in table["cases"].array {
            let query = try XCTUnwrap(vector["query"].string)
            XCTAssertEqual(ComposerSuggestions.rankModels(query, list).map(\.id), strings(vector["expect"]), "query \(query.debugDescription)")
        }
        XCTAssertEqual(ComposerSuggestions.orderModels(list).map(\.id), strings(table["cases"].array[0]["expect"]),
                       "before anything is typed: the most used first, ties and unused ones in the provider's order")
        for vector in table["current"].array {
            XCTAssertEqual(ComposerSuggestions.currentModel(list, chosen: vector["chosen"].string), vector["expect"].string, "chosen \(vector["chosen"])")
        }
    }
    func testModelRowsAndPreviewsReadAsTheSharedVectorsSay() throws {
        let fixture = try fixture()
        XCTAssertFalse(fixture["model_details"].array.isEmpty || fixture["model_previews"].array.isEmpty)
        for vector in fixture["model_details"].array {
            let chats = try XCTUnwrap(number(vector["chats"]).map { UInt64($0) })
            let current = vector["current"] == .bool(true)
            XCTAssertEqual(ComposerModelEntry(id: "m", name: "M", chats: chats).detail(current: current), vector["detail"].string)
        }
        for vector in fixture["model_usage_labels"].array {
            let chats = try XCTUnwrap(number(vector["chats"]).map { UInt64($0) })
            let provider = try XCTUnwrap(vector["provider"].string.flatMap(ChatProvider.init(rawValue:)))
            XCTAssertEqual(ComposerSuggestions.usageLabel(chats: chats, provider: provider), vector["label"].string)
        }
        let list = try models(fixture)
        for vector in fixture["model_previews"].array {
            let model = try XCTUnwrap(list.first { $0.id == vector["model"].string })
            let provider = try XCTUnwrap(vector["provider"].string.flatMap(ChatProvider.init(rawValue:)))
            let current = vector["current"] == .bool(true)
            let preview = ComposerPreview(model: model, current: current, provider: provider)
            let expect = vector["expect"]
            XCTAssertEqual(preview.title, expect["title"].string)
            XCTAssertEqual(preview.kind, expect["kind"].string)
            XCTAssertEqual(preview.body, strings(expect["body"]))
            XCTAssertEqual(preview.inserts, expect["inserts"].string)
            XCTAssertEqual(preview.effect, expect["effect"].string)
        }
    }
    func testTheHighlightedModelStaysWhenTheUseIsReadAgainOnlyOnceMoved() throws {
        let first = [ComposerModelEntry(id: "a", name: "A", isDefault: true, chats: 5), ComposerModelEntry(id: "b", name: "B", chats: 1),
                     ComposerModelEntry(id: "c", name: "C", chats: 0)]
        let later = [ComposerModelEntry(id: "a", name: "A", isDefault: true, chats: 5), ComposerModelEntry(id: "b", name: "B", chats: 9),
                     ComposerModelEntry(id: "c", name: "C", chats: 0)]
        let catalog = { (models: [ComposerModelEntry]) in ComposerCatalogState.ready(ComposerCatalog(chatID: self.chat, provider: .codex, commands: [], files: [], models: models)) }
        var list = ComposerSuggestionList()
        list.update(text: "&", caret: 1, catalog: catalog(first))
        XCTAssertEqual(list.items.map(\.title), ["&a", "&b", "&c"])
        // Never moved: the catalog read again (its counts changed) keeps the highlight on the first row, the most used.
        list.update(text: "&", caret: 1, catalog: catalog(later))
        XCTAssertEqual(list.items.map(\.title), ["&b", "&a", "&c"])
        XCTAssertEqual(list.highlightedItem?.title, "&b")
        // Moved to C: it stays C's, wherever C goes.
        list.move(.down); list.move(.down)
        XCTAssertEqual(list.highlightedItem?.title, "&c")
        list.update(text: "&", caret: 1, catalog: catalog(first))
        XCTAssertEqual(list.highlightedItem?.title, "&c")
        // A new query starts at the best match again.
        list.update(text: "&b", caret: 2, catalog: catalog(first))
        XCTAssertEqual(list.highlighted, 0)
        // The chat's model, given with blank space around it, is still the one marked.
        list.update(text: "&", caret: 1, catalog: catalog(first), chosenModel: " b ")
        XCTAssertEqual(list.items.map(\.detail), ["Used in 5 chats", "Current · used in 1 chat", "Not used yet"])
    }
    func testAModelsDecodingKeepsTheGoodOnesAndDefaultsTheRest() throws {
        let decoded = try value(#"[{"id":"a","name":"A"},{"id":"  ","name":"Blank"},{"id":"two words","name":"Spaced"},{"name":"No id"},{"id":"b"},{"id":"c","name":"C","chats":"many","is_default":"yes"}]"#)
            .decode([Lenient<ComposerModelEntry>].self).compactMap(\.value)
        XCTAssertEqual(decoded, [ComposerModelEntry(id: "a", name: "A"), ComposerModelEntry(id: "c", name: "C")])
        // A desktop from before `&` sends no models: the reply still reads, and offers none.
        var older = try fixture()["reply"]
        if case .object(var fields) = older { fields["models"] = nil; older = .object(fields) }
        let reply = try ChatSuggestionsRequest(chatID: chat).parse(older)
        XCTAssertTrue(reply.models.isEmpty)
        var list = ComposerSuggestionList()
        list.update(text: "&", caret: 1, catalog: .ready(ComposerCatalog(reply: reply)))
        XCTAssertFalse(list.isShown, "no models, no `&` list")
        XCTAssertNil(list.accepted())
    }
    func testAcceptingAModelPutsItsMentionInTheDraftAndNothingElse() throws {
        let fixture = try fixture()
        let catalog = ComposerCatalog(chatID: chat, provider: .codex, commands: [], files: [], models: try models(fixture))
        var list = ComposerSuggestionList()
        list.update(text: "review it &", caret: 11, catalog: .ready(catalog), chosenModel: "o3")
        XCTAssertTrue(list.isShown)
        XCTAssertEqual(list.items.map(\.title), ["&gpt-5.1", "&gpt-5.1-codex", "&o3", "&gpt-5.1-codex-mini", "&gpt-4.1"], "the most used first")
        XCTAssertEqual(list.items.map(\.detail), ["Used in 7 chats", "Used in 3 chats", "Current · used in 3 chats", "Not used yet", "Not used yet"],
                       "the chat's own model is marked, not left out")
        XCTAssertEqual(list.preview, ComposerPreview(title: "&gpt-5.1", kind: "Codex model", body: ["GPT-5.1", "7 of your Codex chats use it now"],
                                                     inserts: "&gpt-5.1",
                                                     effect: "Inserts &gpt-5.1 in the message. It does not switch this chat; the orchestrator still decides."))
        // Accepting gives the composer text to put in and nothing else: the list has no way to ask the chat for anything, so no
        // `chat.command` (a `configure` or any other) can come of it.
        var accepted = try XCTUnwrap(list.accepted())
        XCTAssertEqual(accepted.text, "review it &gpt-5.1 ")
        XCTAssertEqual(accepted.caret, 19)
        // Before more words, and the chat's own model: its mention goes in like any other, and what follows stays.
        let draft = "&o design the API, then build it"
        list.update(text: draft, caret: 2, catalog: .ready(catalog), chosenModel: "o3")
        XCTAssertEqual(list.items.map(\.title), ["&o3", "&gpt-5.1-codex", "&gpt-5.1-codex-mini"])
        XCTAssertEqual(list.preview?.kind, "Codex model · current")
        accepted = try XCTUnwrap(list.accepted())
        XCTAssertEqual(accepted.text, "&o3 design the API, then build it", "the token is replaced, no space doubled")
        XCTAssertEqual(accepted.caret, 4)
        // A second mention later in the same draft, put in with a tap: the first one and the text around it stay as they were.
        let second = "&o3 design the API, then &gp build it", caret = "&o3 design the API, then &gp".utf16.count
        list.update(text: second, caret: caret, catalog: .ready(catalog), chosenModel: "o3")
        XCTAssertEqual(list.items.map(\.title), ["&gpt-5.1", "&gpt-5.1-codex", "&gpt-5.1-codex-mini", "&gpt-4.1"])
        XCTAssertNil(list.tap(1), "the first tap only highlights")
        accepted = try XCTUnwrap(list.tap(1))
        XCTAssertEqual(accepted.text, "&o3 design the API, then &gpt-5.1-codex build it")
        XCTAssertEqual(accepted.caret, "&o3 design the API, then &gpt-5.1-codex ".utf16.count)
        // The caret back in a mention offers it again, as a path's would be; a `&` inside a word is only text.
        list.update(text: accepted.text, caret: 2, catalog: .ready(catalog), chosenModel: "o3")
        XCTAssertEqual(list.trigger?.query, "o")
        list.update(text: "tom&jerry", caret: 4, catalog: .ready(catalog), chosenModel: "o3")
        XCTAssertFalse(list.isShown)
    }

    // MARK: Rules the fixture does not spell out

    func testACaretInsideACharacterOrOutsideTheTextOpensNothing() {
        // 😀 is two UTF-16 units: a caret between them is inside it.
        XCTAssertNil(ComposerSuggestions.trigger(in: "@😀", caret: 2))
        XCTAssertEqual(ComposerSuggestions.trigger(in: "@😀", caret: 3)?.query, "😀")
        XCTAssertEqual(ComposerSuggestions.trigger(in: "@😀 x", caret: 3)?.range, NSRange(location: 0, length: 3), "the token's range is the text view's")
        XCTAssertNil(ComposerSuggestions.trigger(in: "@a", caret: 3))
        XCTAssertNil(ComposerSuggestions.trigger(in: "@a", caret: -1))
        XCTAssertNil(ComposerSuggestions.trigger(in: "", caret: 0))
        // A combining accent is a character of its own, as Rust counts them.
        XCTAssertEqual(ComposerSuggestions.trigger(in: "@e\u{301}", caret: 2)?.query, "e")
    }
    func testEveryKindOfBlankSpaceEndsAToken() {
        for space in ["\u{a0}", "\u{2003}", "\u{3000}", "\r", "\u{2028}"] {
            XCTAssertEqual(ComposerSuggestions.trigger(in: "a\(space)@b", caret: 4 - 1 + space.utf16.count)?.query, "b", "after \(space.debugDescription)")
            XCTAssertEqual(ComposerSuggestions.trigger(in: "\(space)/co", caret: space.utf16.count + 3)?.sigil, .slash, "\(space.debugDescription) before a / is blank")
        }
        XCTAssertNil(ComposerSuggestions.trigger(in: "\u{200b}/co", caret: 4), "a zero-width space is not blank space (as in Rust)")
    }
    func testMatchingIsByCharacterNotBySwiftsStringEquality() {
        // Swift calls "café" with a combining accent equal to "café" with a precomposed one; Rust (and so the desktop) does not.
        XCTAssertTrue(ComposerSuggestions.rankCommands("cafe\u{301}", commands(["caf\u{e9}"])).isEmpty)
        XCTAssertEqual(ComposerSuggestions.rankCommands("caf", commands(["caf\u{e9}"])).map(\.name), ["caf\u{e9}"])
        // Ties go by Unicode scalar value, as Rust compares strings: "e" (U+0065) before "é" (U+00E9), whatever follows.
        let entries = [ComposerFileEntry(path: "\u{e9}ab"), ComposerFileEntry(path: "e\u{301}z")]
        XCTAssertEqual(ComposerSuggestions.rankFiles("", entries, limit: 10).map(\.path), ["e\u{301}z", "\u{e9}ab"])
        // Lengths are in characters too: "é" spelled with two is the longer name.
        let lengths = [ComposerFileEntry(path: "e\u{301}.md"), ComposerFileEntry(path: "z.md")]
        XCTAssertEqual(ComposerSuggestions.rankFiles("", lengths, limit: 10).map(\.path), ["z.md", "e\u{301}.md"])
    }
    func testASizeNeverOverflows() {
        XCTAssertEqual(ComposerSuggestions.sizeLabel(.max), "17592186044416.0 MB")
        XCTAssertEqual(ComposerSuggestions.sizeLabel(1023 * 1024 + 511), "1023 KB")
    }
    func testAPreviewSaysWhatItIsAndWhatSendingDoes() throws {
        let compact = try XCTUnwrap(try fixture()["commands"]["claude"].decode([ComposerCommand].self).first)
        let preview = ComposerPreview(command: compact, provider: .claude)
        XCTAssertEqual(preview.title, "/compact")
        XCTAssertEqual(preview.kind, "Claude command")
        XCTAssertEqual(preview.body, ["Summarize the conversation so far to free up context", "Takes no argument"])
        XCTAssertEqual(preview.inserts, "/compact")
        XCTAssertTrue(preview.effect.hasPrefix("Sending: Goes to Claude as typed."))
        let memo = ComposerCommand(name: "memo", summary: "s", argument: "TEXT", sends: "x", handler: .riwork)
        XCTAssertEqual(ComposerPreview(command: memo, provider: .codex).kind, "RiWork command")
        XCTAssertEqual(ComposerPreview(command: memo, provider: .codex).body[1], "Takes TEXT")
        XCTAssertEqual(ComposerPreview(command: compact, provider: .muse).kind, "Muse command")
        let file = ComposerPreview(file: ComposerFileEntry(path: "docs/my notes.md", bytes: 2048))
        XCTAssertEqual(file.title, "`docs/my notes.md`")
        XCTAssertEqual(file.kind, "File · 2 KB")
        XCTAssertTrue(file.body.isEmpty, "the phone does not read files over the relay")
        XCTAssertEqual(file.effect, "Puts in its path, relative to the chat's folder; nothing is attached or sent.")
    }

    // MARK: chat.suggestions

    func testTheRequestIsTheChatAloneAndIsCheckedAsTheDesktopChecksIt() throws {
        let request = try ChatSuggestionsRequest(chatID: chat)
        XCTAssertEqual(request.params, ["chat_id": .string(chat)])
        XCTAssertEqual(try ChatSuggestionsRequest(params: request.params), request)
        XCTAssertNoThrow(try RequestValidation.validate(method: "chat.suggestions", params: request.params, id: requestID))
        XCTAssertThrowsError(try ChatSuggestionsRequest(chatID: "nope")) { XCTAssertEqual($0 as? ChatValidationError, .invalidID) }
        XCTAssertThrowsError(try ChatSuggestionsRequest(chatID: chat.uppercased()))
        for bad: [String: JSONValue] in [[:], ["chat_id": .string("nope")], ["chat_id": .number(1)], ["chat_id": .string(chat), "extra": .null], ["provider": .string("claude")]] {
            XCTAssertThrowsError(try RequestValidation.validate(method: "chat.suggestions", params: bad, id: requestID), "\(bad)")
        }
    }
    func testACommandOrAnEntryThePhoneCannotReadIsLeftOutNotTheReply() throws {
        let reply = try ChatSuggestionsRequest(chatID: chat).parse(try value("""
        {"chat_id":"\(chat)","provider":"claude","root":"/w","file_bound":5000,
         "commands":[{"name":"compact","summary":"s","argument":null,"sends":"x","handler":"provider"},
                     {"name":"new","summary":"s","sends":"x","handler":"elsewhere"},{"name":"","summary":"s","sends":"x","handler":"riwork"},
                     {"name":"two words","summary":"s","sends":"x","handler":"riwork"},{"summary":"no name"},
                     {"name":"memo","summary":"s","argument":"TEXT","sends":"x","handler":"riwork","later":true}],
         "files":{"entries":[{"path":"a.txt","dir":false,"bytes":5},{"path":""},{"dir":true},{"path":7},
                             {"path":"odd","dir":"yes","bytes":-1},{"path":"b","dir":true,"bytes":null}],"truncated":true},
         "later":{"x":1}}
        """))
        XCTAssertEqual(reply.commands.map(\.name), ["compact", "memo"])
        XCTAssertEqual(reply.commands.last?.argument, "TEXT")
        XCTAssertEqual(reply.files.entries, [ComposerFileEntry(path: "a.txt", bytes: 5), ComposerFileEntry(path: "odd"), ComposerFileEntry(path: "b", dir: true)],
                       "an odd dir or size reads as missing; an entry without a path is left out")
        XCTAssertTrue(reply.files.truncated)
        let bare = try ChatSuggestionsRequest(chatID: chat).parse(try value(#"{"chat_id":"\#(chat)","provider":"codex","commands":[],"files":{"entries":[]}}"#))
        XCTAssertEqual(bare, ChatSuggestionsReply(chatID: chat, provider: .codex, root: "", commands: [], files: ComposerFileListing()), "what a desktop leaves out has its default")
    }
    func testAnAnswerForAnotherChatOrWithoutItsListsIsNotBelieved() throws {
        let request = try ChatSuggestionsRequest(chatID: chat)
        let other = "44444444-4444-4444-8444-444444444444"
        for bad in [#"{"chat_id":"\#(other)","provider":"claude","commands":[],"files":{"entries":[]}}"#,
                    #"{"chat_id":"\#(chat)","provider":"grok","commands":[],"files":{"entries":[]}}"#,
                    #"{"chat_id":"\#(chat)","provider":"claude","files":{"entries":[]}}"#,
                    #"{"chat_id":"\#(chat)","provider":"claude","commands":[],"files":[]}"#,
                    #"{"chat_id":"\#(chat)","provider":"claude","commands":{},"files":{"entries":[]}}"#, "[]", "null"] {
            XCTAssertThrowsError(try request.parse(try value(bad)), bad) { XCTAssertEqual($0 as? ChatControlError, .unreadableReply, bad) }
        }
    }
    func testTheFeatureIsReadOnlyWhenTheDesktopSaysTrue() throws {
        XCTAssertFalse(DesktopFeatures(ready: try value(#"{"features":{"chat":true,"chat_models":true}}"#)).chatSuggestions)
        XCTAssertTrue(DesktopFeatures(ready: try value(#"{"features":{"chat":true,"chat_suggestions":true}}"#)).chatSuggestions)
        XCTAssertFalse(DesktopFeatures(ready: try value(#"{"features":{"chat_suggestions":"yes"}}"#)).chatSuggestions)
        XCTAssertFalse(DesktopFeatures().chatSuggestions)
    }
    func testAnOlderDesktopsRefusalIsUnsupported() {
        let refusal = RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")
        XCTAssertEqual(ChatControlError.from(refusal, operation: .list), .unsupported)
    }

    // MARK: The list

    private let compact = ComposerCommand(name: "compact", summary: "Summarize the conversation so far to free up context", sends: "Goes to Claude as typed.", handler: .provider)
    private func catalog(commands names: [String] = ["compact", "context", "pact"], files: [ComposerFileEntry]? = nil, truncated: Bool = false) -> ComposerCatalog {
        let list = names.map { $0 == "compact" ? compact : ComposerCommand(name: $0, summary: "\($0) summary", sends: "x", handler: .provider) }
        return ComposerCatalog(chatID: chat, provider: .claude, commands: list, files: files ?? [
            ComposerFileEntry(path: "README.md", bytes: 2048), ComposerFileEntry(path: "src", dir: true), ComposerFileEntry(path: "src/main.rs", bytes: 900),
            ComposerFileEntry(path: "src/chat", dir: true), ComposerFileEntry(path: "src/chat/main_view.rs", bytes: 300)
        ], filesTruncated: truncated)
    }
    /// The list after `text` with the caret at the end (or at `caret`).
    private func list(_ text: String, caret: Int? = nil, _ state: ComposerCatalogState) -> ComposerSuggestionList {
        var list = ComposerSuggestionList()
        list.update(text: text, caret: caret ?? text.utf16.count, catalog: state)
        return list
    }

    func testASlashAtTheStartOpensTheCommandsAndTypingFiltersThem() {
        let ready = ComposerCatalogState.ready(catalog())
        var list = ComposerSuggestionList()
        list.update(text: "/", caret: 1, catalog: ready)
        XCTAssertTrue(list.isShown)
        XCTAssertEqual(list.items.map(\.title), ["/compact", "/context", "/pact"], "every command, in the registry's order")
        XCTAssertEqual(list.highlighted, 0)
        XCTAssertEqual(list.preview?.kind, "Claude command")
        XCTAssertEqual(list.items.first?.detail, "Summarize the conversation so far to free up context")
        list.update(text: "/pa", caret: 3, catalog: ready)
        XCTAssertEqual(list.items.map(\.title), ["/pact", "/compact"], "a name that starts with it first")
        list.update(text: "/zz", caret: 3, catalog: ready)
        XCTAssertFalse(list.isShown, "nothing matches: no list")
        XCTAssertNil(list.accepted())
    }
    func testAnAtOpensTheFilesShallowestFirstAndTypingFiltersThem() {
        let ready = ComposerCatalogState.ready(catalog())
        var list = ComposerSuggestionList()
        list.update(text: "see @", caret: 5, catalog: ready)
        XCTAssertEqual(list.items.map(\.title), ["src/", "README.md", "src/chat/", "src/main.rs", "src/chat/main_view.rs"])
        XCTAssertEqual(list.items.map(\.detail), ["Folder", "File · 2 KB", "Folder", "File · 900 bytes", "File · 300 bytes"])
        list.update(text: "see @main", caret: 9, catalog: ready)
        XCTAssertEqual(list.items.map(\.title), ["src/main.rs", "src/chat/main_view.rs"])
        XCTAssertEqual(list.preview, ComposerPreview(file: ComposerFileEntry(path: "src/main.rs", bytes: 900)))
    }
    func testAFileListHoldsAtMostFiftyAndSaysWhenTheFolderWasCut() {
        let many = (0..<120).map { ComposerFileEntry(path: String(format: "f%03d.txt", $0), bytes: 1) }
        let cut = list("@f", .ready(catalog(files: many, truncated: true)))
        XCTAssertEqual(cut.items.count, ComposerSuggestions.shownFiles)
        XCTAssertEqual(cut.items.first?.title, "f000.txt")
        XCTAssertTrue(cut.filesTruncated)
        XCTAssertFalse(list("/", .ready(catalog(files: many, truncated: true))).filesTruncated, "a command list says nothing about files")
    }
    func testASigilInsideAWordOrAfterTheStartOpensNothing() {
        let ready = ComposerCatalogState.ready(catalog())
        for text in ["a/b", "me@host", "please /com", "x /", "/usr/bin", "mail me@ho"] {
            XCTAssertFalse(list(text, ready).isShown, text)
            XCTAssertNil(list(text, ready).trigger, text)
        }
        XCTAssertFalse(list("/compact now", caret: 0, ready).isShown, "the caret before the sigil")
        XCTAssertFalse(list("/compact ", ready).isShown, "the token is done")
        var selecting = ComposerSuggestionList()
        selecting.update(text: "/com", caret: nil, catalog: ready)
        XCTAssertFalse(selecting.isShown, "text selected rather than a caret placed")
    }
    func testUpAndDownMoveTheHighlightAndWrap() {
        var list = list("/", .ready(catalog()))
        list.move(.down)
        XCTAssertEqual(list.highlightedItem?.title, "/context")
        list.move(.down); list.move(.down)
        XCTAssertEqual(list.highlighted, 0, "down from the last row goes round to the first")
        list.move(.up)
        XCTAssertEqual(list.highlightedItem?.title, "/pact", "up from the first goes round to the last")
        list.highlight(1)
        XCTAssertEqual(list.highlighted, 1)
        list.highlight(9)
        XCTAssertEqual(list.highlighted, 1, "a row that is not there is not highlighted")
        var empty = ComposerSuggestionList()
        empty.move(.down)
        XCTAssertEqual(empty.highlighted, 0)
    }
    func testANewQueryHighlightsTheBestMatchAndARereadKeepsTheRow() {
        let first = catalog()
        var list = ComposerSuggestionList()
        list.update(text: "/c", caret: 2, catalog: .ready(first))
        list.move(.down)
        XCTAssertEqual(list.highlightedItem?.title, "/context")
        list.update(text: "/c", caret: 2, catalog: .ready(catalog()))
        XCTAssertEqual(list.highlightedItem?.title, "/context", "the catalog read again: the same row stays highlighted")
        list.update(text: "/co", caret: 3, catalog: .ready(first))
        XCTAssertEqual(list.highlighted, 0, "a new query: the best match")
    }
    func testAcceptingPutsTheRowInWithASpaceAndNeverSends() {
        var list = list("/co", .ready(catalog()))
        let accepted = list.accepted()
        XCTAssertEqual(accepted?.text, "/compact ")
        XCTAssertEqual(accepted?.caret, 9)
        list.update(text: "/compact ", caret: 9, catalog: .ready(catalog()))
        XCTAssertFalse(list.isShown, "the token is done: the list goes")
        var mention = self.list("see @ma please", caret: 7, .ready(catalog()))
        XCTAssertEqual(mention.highlightedItem?.title, "src/main.rs")
        XCTAssertEqual(mention.accepted()?.text, "see src/main.rs please")
        XCTAssertEqual(mention.accepted()?.caret, 16, "after the space that was there")
        mention.update(text: "see src/main.rs please", caret: 16, catalog: .ready(catalog()))
        XCTAssertFalse(mention.isShown)
        // While the list is open, ⏎ and Tab put the row in; nothing on the keyboard sends.
        let open = ChatKeyContext(composerIsEmpty: false, suggesting: true)
        XCTAssertEqual(ChatKeyRouter.action(for: .return, in: open), .acceptSuggestion)
        XCTAssertEqual(ChatKeyRouter.action(for: .tab, in: open), .acceptSuggestion)
        XCTAssertFalse(ChatKey.allCases.contains { ChatKeyRouter.action(for: $0, in: open) == .send }, "no key sends while the list is open")
    }
    func testATapHighlightsARowAndASecondTapPutsItIn() {
        var list = list("/", .ready(catalog()))
        XCTAssertNil(list.tap(2), "the first tap only highlights")
        XCTAssertEqual(list.highlighted, 2)
        XCTAssertEqual(list.preview?.title, "/pact", "and the card follows")
        XCTAssertEqual(list.tap(2)?.text, "/pact ", "a tap on the highlighted row puts it in")
        XCTAssertNil(list.tap(7))
    }
    func testEscapeClosesTheListUntilTheTokenGoes() {
        let ready = ComposerCatalogState.ready(catalog())
        var list = list("see @s", ready)
        XCTAssertTrue(list.isShown)
        list.dismiss()
        XCTAssertFalse(list.isShown)
        XCTAssertNil(list.accepted())
        list.update(text: "see @sr", caret: 7, catalog: ready)
        XCTAssertFalse(list.isShown, "typing on in the same token keeps it closed")
        list.update(text: "see @sr", caret: 5, catalog: ready)
        XCTAssertFalse(list.isShown, "so does moving inside it")
        list.update(text: "see @sr ", caret: 8, catalog: ready)
        XCTAssertFalse(list.isShown)
        list.update(text: "see @sr @", caret: 9, catalog: ready)
        XCTAssertTrue(list.isShown, "another token opens it")
        list.update(text: "see @sr", caret: 7, catalog: ready)
        XCTAssertTrue(list.isShown, "the closed token went away (the caret left it), so it opens again")
        var slash = self.list("/c", ready)
        slash.dismiss()
        slash.update(text: "", caret: 0, catalog: ready)
        slash.update(text: "/c", caret: 2, catalog: ready)
        XCTAssertTrue(slash.isShown, "deleted and typed again")
    }
    func testAnAtListSaysTheFilesAreBeingListedUntilTheyAre() {
        var list = list("@sr", .loading)
        XCTAssertTrue(list.isShown)
        XCTAssertTrue(list.listingFiles)
        XCTAssertTrue(list.items.isEmpty)
        XCTAssertNil(list.accepted(), "nothing to put in yet")
        list.move(.down)
        XCTAssertEqual(list.highlighted, 0)
        list.update(text: "@sr", caret: 3, catalog: .ready(catalog()))
        XCTAssertFalse(list.listingFiles)
        XCTAssertEqual(list.items.first?.title, "src/")
        XCTAssertFalse(self.list("/c", .loading).isShown, "a command list waits without a row")
    }
    func testWithoutTheCatalogNoListOpens() {
        for state: ComposerCatalogState in [.unsupported, .unavailable] {
            XCTAssertFalse(list("/c", state).isShown, "\(state)")
            XCTAssertFalse(list("@s", state).isShown, "\(state)")
            XCTAssertFalse(list("@s", state).listingFiles)
        }
        XCTAssertNotNil(list("@s", .unsupported).trigger, "the token is still known, so the composer can ask for the catalog")
    }
    func testAQueryOfOtherScalarsIsRankedAgainThoughItLooksTheSame() {
        // "été.txt" with é as one scalar, "école.txt" with e and a combining acute: the matching compares scalars.
        let catalog = catalog(files: [ComposerFileEntry(path: "\u{e9}t\u{e9}.txt"), ComposerFileEntry(path: "e\u{301}cole.txt")])
        var list = ComposerSuggestionList()
        list.update(text: "@\u{e9}", caret: 2, catalog: .ready(catalog))
        XCTAssertEqual(list.items.map(\.title), ["\u{e9}t\u{e9}.txt"])
        list.update(text: "@e\u{301}", caret: 3, catalog: .ready(catalog))
        XCTAssertEqual(list.items.map(\.title), ["e\u{301}cole.txt"], "canonically equal to the query before, but other scalars")
        XCTAssertEqual(list.items.map(\.title), ComposerSuggestions.rankFiles("e\u{301}", catalog.files, limit: 50).map(\.shownPath))
    }
    func testAnEmptyDraftAfterSendingClosesTheList() {
        var list = list("/co", .ready(catalog()))
        list.update(text: "", caret: nil, catalog: .ready(catalog()))
        XCTAssertFalse(list.isShown)
        XCTAssertEqual(list.text, "")
    }

    // MARK: The catalog cache

    func testACatalogIsReadOnceAndKeptForThirtySeconds() {
        var cache = ComposerCatalogCache()
        let start = ContinuousClock.now
        XCTAssertEqual(cache.state(for: chat, connected: true), .loading)
        XCTAssertEqual(cache.state(for: chat, connected: false), .unavailable, "nothing to show while the link is down")
        XCTAssertTrue(cache.wantsFetch(chat, now: start))
        XCTAssertEqual(cache.count, 0, "asking changes nothing")
        XCTAssertTrue(cache.beginFetch(chat, now: start))
        XCTAssertFalse(cache.wantsFetch(chat, now: start + .milliseconds(5)))
        XCTAssertFalse(cache.beginFetch(chat, now: start + .milliseconds(5)), "one read on the wire at a time")
        XCTAssertEqual(cache.state(for: chat, connected: true), .loading)
        let first = catalog()
        cache.store(first, for: chat, now: start + .seconds(1))
        XCTAssertEqual(cache.state(for: chat, connected: true), .ready(first))
        XCTAssertFalse(cache.wantsFetch(chat, now: start + .seconds(30)))
        XCTAssertFalse(cache.beginFetch(chat, now: start + .seconds(30)), "fresh for 30 s")
        XCTAssertTrue(cache.beginFetch(chat, now: start + .seconds(31)), "read again after")
        XCTAssertEqual(cache.state(for: chat, connected: true), .ready(first), "the old one is shown meanwhile")
        XCTAssertEqual(cache.state(for: chat, connected: false), .ready(first))
        cache.fail(chat, now: start + .seconds(32))
        XCTAssertEqual(cache.state(for: chat, connected: true), .ready(first), "a failed read keeps what was read")
    }
    func testAFailedReadIsTriedAgainOnlyAfterAPause() {
        var cache = ComposerCatalogCache()
        let start = ContinuousClock.now
        XCTAssertTrue(cache.beginFetch(chat, now: start))
        cache.fail(chat, now: start)
        XCTAssertEqual(cache.state(for: chat, connected: true), .unavailable)
        XCTAssertFalse(cache.beginFetch(chat, now: start + .seconds(9)))
        XCTAssertTrue(cache.beginFetch(chat, now: start + .seconds(10)))
        XCTAssertEqual(cache.state(for: chat, connected: true), .loading)
        cache.abandon(chat)
        XCTAssertTrue(cache.beginFetch(chat, now: start + .seconds(10)), "an answer from a connection that is gone counts for nothing")
    }
    func testACatalogOfTheProviderTheChatLeftIsNotOfferedAndIsReadAgainAtOnce() {
        var cache = ComposerCatalogCache()
        let start = ContinuousClock.now
        XCTAssertTrue(cache.beginFetch(chat, provider: .claude, now: start))
        let claude = catalog()
        cache.store(claude, for: chat, now: start)
        XCTAssertEqual(cache.state(for: chat, provider: .claude, connected: true), .ready(claude))
        XCTAssertFalse(cache.wantsFetch(chat, provider: .claude, now: start + .seconds(1)))
        // The chat went on with Codex (`switch`): Claude's /compact would be sent to Codex as words.
        XCTAssertEqual(cache.state(for: chat, provider: .codex, connected: true), .loading, "Claude's commands are not offered")
        XCTAssertEqual(cache.state(for: chat, provider: .codex, connected: false), .unavailable)
        XCTAssertTrue(cache.wantsFetch(chat, provider: .codex, now: start + .seconds(1)), "read again, though not 30 s old")
        XCTAssertTrue(cache.beginFetch(chat, provider: .codex, now: start + .seconds(1)))
        XCTAssertFalse(cache.beginFetch(chat, provider: .codex, now: start + .seconds(1)), "one read on the wire at a time")
        XCTAssertEqual(cache.state(for: chat, provider: .codex, connected: true), .loading)
        let codex = ComposerCatalog(chatID: chat, provider: .codex, commands: [ComposerCommand(name: "memo", summary: "s", sends: "x", handler: .riwork)], files: [])
        cache.store(codex, for: chat, now: start + .seconds(2))
        XCTAssertEqual(cache.state(for: chat, provider: .codex, connected: true), .ready(codex))
        XCTAssertFalse(cache.wantsFetch(chat, provider: .codex, now: start + .seconds(3)))
        XCTAssertEqual(cache.state(for: chat, connected: true), .ready(codex), "a caller that does not know the provider gets what was read")
    }
    func testOnlyTheMostRecentlyUsedChatsAreKept() {
        var cache = ComposerCatalogCache()
        let start = ContinuousClock.now
        let ids = (0..<8).map { String(format: "c4a70000-0000-4000-8000-%012d", $0) }
        for (offset, id) in ids.enumerated() {
            let at = start + .seconds(offset)
            XCTAssertTrue(cache.beginFetch(id, now: at))
            cache.store(catalog(), for: id, now: at)
        }
        XCTAssertEqual(cache.count, ComposerCatalogCache.keptChats)
        XCTAssertEqual(cache.state(for: ids[0], connected: true), .loading, "the oldest went")
        XCTAssertNotEqual(cache.state(for: ids[7], connected: true), .loading)
    }

    // MARK: Keys while the list is open

    private func action(_ key: ChatKey, suggesting: Bool, empty: Bool = false, approval: ChatApproval? = nil, busy: Bool = false) -> ChatKeyAction {
        ChatKeyRouter.action(for: key, in: ChatKeyContext(composerIsEmpty: empty, approval: approval, busy: busy, suggesting: suggesting))
    }
    func testWhileTheListIsOpenTheArrowsMoveReturnAndTabAcceptAndEscapeCloses() {
        XCTAssertEqual(action(.up, suggesting: true), .moveSuggestion(.up))
        XCTAssertEqual(action(.down, suggesting: true), .moveSuggestion(.down))
        XCTAssertEqual(action(.return, suggesting: true), .acceptSuggestion)
        XCTAssertEqual(action(.tab, suggesting: true), .acceptSuggestion)
        XCTAssertEqual(action(.escape, suggesting: true), .closeSuggestions)
        XCTAssertEqual(action(.shiftReturn, suggesting: true), .insertNewline, "⇧⏎ is still a new line")
        XCTAssertEqual(action(.commandPeriod, suggesting: true, busy: true), .interrupt, "and ⌘. still interrupts")
        XCTAssertEqual(action(.commandDelete, suggesting: true), .none)
    }
    func testWithTheListClosedTheKeysAreAsBefore() {
        XCTAssertEqual(action(.return, suggesting: false), .send)
        XCTAssertEqual(action(.escape, suggesting: false), .none)
        for key: ChatKey in [.up, .down, .tab] { XCTAssertEqual(action(key, suggesting: false), .none, "\(key) is the text view's: the composer does not claim it") }
        let pending = ChatApproval(requestID: "r1", kind: .command, title: "rm -rf build", choices: [.accept, .decline])
        XCTAssertEqual(action(.return, suggesting: true, empty: true, approval: pending), .decide(.accept), "a request answered with an empty draft keeps its keys")
        XCTAssertEqual(action(.escape, suggesting: true, empty: true, approval: pending), .decide(.decline))
    }

    // MARK: A held Return

    func testAReturnHeldAfterAcceptingIsSwallowedUntilItComesUpOrPauses() {
        let start = ContinuousClock.now
        var held = HeldReturn()
        XCTAssertFalse(held.isRepeat(at: start), "a Return with nothing accepted is the composer's own")
        held.accepted(at: start)
        XCTAssertTrue(held.isRepeat(at: start + .milliseconds(450)), "the first repeat")
        XCTAssertTrue(held.isRepeat(at: start + .milliseconds(1_000)), "repeats keep the hold going")
        XCTAssertFalse(held.isRepeat(at: start + .milliseconds(2_000)), "a Return after a pause sends again")
        held.accepted(at: start)
        held.released()
        XCTAssertFalse(held.isRepeat(at: start + .milliseconds(100)), "a Return pressed again after the key came up sends")
    }
}
