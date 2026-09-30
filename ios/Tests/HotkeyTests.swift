import XCTest
@testable import RiWorkCore

private func parse(_ text: String) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: Data(text.utf8)) }

final class HotkeyTests: XCTestCase {
    private func hotkey(_ label: String = "Clear", _ steps: [KeyItem] = [.text("/clear"), .key(.enter)], id: String = "11111111-1111-4111-8111-111111111111") -> Hotkey {
        Hotkey(id: id, label: label, steps: steps)
    }
    private func assertInvalid(_ hotkey: Hotkey, _ expected: HotkeyError, _ message: String = "", file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertThrowsError(try hotkey.validate(), message, file: file, line: line) { XCTAssertEqual($0 as? HotkeyError, expected, message, file: file, line: line) }
    }

    // MARK: Built in

    func testBuiltInHotkeysMapToTheRightItems() {
        let expected: [(label: String, items: [KeyItem])] = [
            ("^C", [.key(.control("c"))]), ("^D", [.key(.control("d"))]), ("^Z", [.key(.control("z"))]), ("^L", [.key(.control("l"))]),
            ("^R", [.key(.control("r"))]), ("^A", [.key(.control("a"))]), ("^E", [.key(.control("e"))]), ("^U", [.key(.control("u"))]),
            ("^W", [.key(.control("w"))]), ("Esc Esc", [.key(.escape), .key(.escape)])
        ]
        XCTAssertEqual(Hotkey.builtIn.map(\.label), expected.map(\.label))
        XCTAssertEqual(Hotkey.builtIn.map(\.items), expected.map(\.items))
        XCTAssertEqual(Set(Hotkey.builtIn.map(\.id)).count, Hotkey.builtIn.count, "ids are unique")
        for hotkey in Hotkey.builtIn {
            XCTAssertTrue(hotkey.isBuiltIn)
            XCTAssertNoThrow(try hotkey.validate(), hotkey.label)
            XCTAssertNoThrow(try KeyItem.validate(batch: hotkey.items), "\(hotkey.label) is a valid batch")
        }
    }
    func testBuiltInsAreOnlyEverNamedKeysFromTheWhitelist() {
        for hotkey in Hotkey.builtIn { for case .key(let key) in hotkey.steps { XCTAssertNotNil(TerminalKey(name: key.name), key.name) } }
    }

    // MARK: Validation

    func testAValidHotkeyPasses() throws {
        XCTAssertNoThrow(try hotkey().validate())
        XCTAssertNoThrow(try hotkey("Exit", [.key(.control("c")), .text("exit"), .key(.enter)]).validate())
        XCTAssertNoThrow(try hotkey("é🙂 ok", [.text("héllo 🙂")]).validate())
    }
    func testTheLabelIsRequiredShortAndPlain() {
        assertInvalid(hotkey(""), .emptyLabel)
        assertInvalid(hotkey("   "), .emptyLabel)
        XCTAssertNoThrow(try hotkey("123456789012").validate(), "twelve is allowed")
        assertInvalid(hotkey("1234567890123"), .labelTooLong(12))
        assertInvalid(hotkey("a\nb"), .invalidLabel)
        assertInvalid(hotkey("a\u{1b}b"), .invalidLabel)
        assertInvalid(hotkey("a\u{2028}b"), .invalidLabel)
    }
    func testTheStepsAreBoundedAndNotEmpty() {
        assertInvalid(hotkey("Clear", []), .noSteps)
        XCTAssertNoThrow(try hotkey("Clear", Array(repeating: .key(.tab), count: 64)).validate())
        assertInvalid(hotkey("Clear", Array(repeating: .key(.tab), count: 65)), .tooManySteps(64))
    }
    func testTextStepsCannotCarryControlCharactersOrBeEmpty() {
        for bad in ["a\nb", "a\rb", "a\tb", "\u{7}", "a\u{1b}[A", "a\u{0}b", "line\u{2028}sep", "para\u{2029}sep", "\u{7f}"] {
            assertInvalid(hotkey("Clear", [.key(.enter), .text(bad)]), .invalidStep(index: 1, reason: "text cannot contain line breaks or control characters; use the Enter key instead."), bad.debugDescription)
        }
        assertInvalid(hotkey("Clear", [.text("")]), .invalidStep(index: 0, reason: "the text is empty."))
        assertInvalid(hotkey("Clear", [.text(String(repeating: "x", count: 4097))]), .invalidStep(index: 0, reason: "the text is too long."))
    }
    func testOnlyWhitelistedKeysAreAccepted() {
        for key in TerminalKey.choices { XCTAssertNoThrow(try hotkey("Clear", [.key(key)]).validate(), key.name) }
        // A Ctrl key for anything but a lowercase ASCII letter is not on the whitelist.
        for letter in ["A", "1", "é", "-"] as [Character] {
            assertInvalid(hotkey("Clear", [.text("x"), .key(.control(letter))]), .invalidStep(index: 1, reason: "this key cannot be sent."), "C-\(letter)")
        }
    }
    func testAllTheTextTogetherStaysWithinOneSend() {
        // Two steps of 3000 bytes are each fine, but a hotkey must fit one batch: neighbouring text is joined.
        assertInvalid(hotkey("Clear", [.text(String(repeating: "a", count: 3000)), .text(String(repeating: "b", count: 3000))]),
                      .invalidStep(index: 1, reason: "there is more text than one send can carry."))
    }
    func testChoicesAreTheContractKeysAndNothingElse() {
        XCTAssertEqual(TerminalKey.choices.count, 14 + 26)
        XCTAssertEqual(Set(TerminalKey.choices.map(\.name)).count, 40)
        XCTAssertEqual(TerminalKey.choices.first, .enter)
        XCTAssertEqual(TerminalKey.control("c").title, "Ctrl+C")
        XCTAssertEqual(TerminalKey.backTab.title, "Shift-Tab")
        for key in TerminalKey.choices { XCTAssertEqual(TerminalKey(name: key.name), key, "\(key.name) round-trips") }
    }

    // MARK: What is sent

    func testNeighbouringTextStepsAreJoinedAndKeysStayApart() {
        let joined = hotkey("Clear", [.text("git "), .text("status"), .key(.enter), .key(.enter), .text("a"), .key(.tab), .text("b")])
        XCTAssertEqual(joined.items, [.text("git status"), .key(.enter), .key(.enter), .text("a"), .key(.tab), .text("b")])
        XCTAssertEqual(joined.summary, "git  status ⏎ ⏎ a ⇥ b")
        XCTAssertNoThrow(try KeyItem.validate(batch: joined.items))
    }
    func testControlCThenExitThenEnter() {
        let bail = hotkey("Bail", [.key(.control("c")), .text("exit"), .key(.enter)])
        XCTAssertEqual(bail.items, [.key(.control("c")), .text("exit"), .key(.enter)])
    }

    // MARK: Storage

    func testAHotkeyRoundTripsThroughItsStoredForm() throws {
        let original = hotkey("Bail", [.key(.control("c")), .text("exit 🙂"), .key(.enter)])
        XCTAssertEqual(original.json["steps"], .array([.object(["key": .string("C-c")]), .object(["text": .string("exit 🙂")]), .object(["key": .string("Enter")])]),
                       "steps are stored in the wire form")
        XCTAssertEqual(try Hotkey(json: original.json), original)
    }
    func testStoredHotkeysAreValidatedOnTheWayIn() throws {
        for bad in ["{\"id\":\"\",\"label\":\"x\",\"steps\":[{\"key\":\"Enter\"}]}",
                    "{\"id\":\"a\",\"label\":\"x\",\"steps\":[]}",
                    "{\"id\":\"a\",\"label\":\"x\",\"steps\":[{\"key\":\"F5\"}]}",
                    "{\"id\":\"a\",\"label\":\"x\",\"steps\":[{\"text\":\"a\\nb\"}]}",
                    "{\"id\":\"a\",\"label\":\"x\",\"steps\":[{\"text\":\"a\",\"key\":\"Enter\"}]}",
                    "{\"id\":\"a\",\"steps\":[{\"key\":\"Enter\"}]}",
                    "{\"id\":\"a\",\"label\":\"\",\"steps\":[{\"key\":\"Enter\"}]}",
                    "[]"] {
            XCTAssertThrowsError(try Hotkey(json: try parse(bad)), bad)
        }
    }

    // MARK: Library

    func testAddUpdateRemoveKeepOrder() throws {
        var library = HotkeyLibrary()
        try library.add(hotkey("One", id: "a")); try library.add(hotkey("Two", id: "b")); try library.add(hotkey("Three", id: "c"))
        XCTAssertEqual(library.hotkeys.map(\.label), ["One", "Two", "Three"])
        try library.update(hotkey("Deux", [.text("x")], id: "b"))
        XCTAssertEqual(library.hotkeys.map(\.label), ["One", "Deux", "Three"])
        XCTAssertEqual(library.hotkeys[1].steps, [.text("x")])
        library.remove(id: "a")
        XCTAssertEqual(library.hotkeys.map(\.label), ["Deux", "Three"])
        library.remove(atOffsets: [1])
        XCTAssertEqual(library.hotkeys.map(\.label), ["Deux"])
    }
    func testTheLibraryRefusesWhatIsInvalidDuplicatedBuiltInOrTooMany() throws {
        var library = HotkeyLibrary()
        XCTAssertThrowsError(try library.add(hotkey("", id: "a")))
        try library.add(hotkey(id: "a"))
        XCTAssertThrowsError(try library.add(hotkey("Again", id: "a"))) { XCTAssertEqual($0 as? HotkeyError, .duplicate) }
        XCTAssertThrowsError(try library.add(Hotkey.builtIn[0])) { XCTAssertEqual($0 as? HotkeyError, .duplicate) }
        XCTAssertThrowsError(try library.update(hotkey("Ghost", id: "zzz"))) { XCTAssertEqual($0 as? HotkeyError, .unknown) }
        XCTAssertThrowsError(try library.update(hotkey("", id: "a"))) { _ in XCTAssertEqual(library.hotkeys[0].label, "Clear", "a refused edit changes nothing") }
        for index in 1..<HotkeyLibrary.maxHotkeys { try library.add(hotkey("H\(index)", id: "id\(index)")) }
        XCTAssertThrowsError(try library.add(hotkey("Last", id: "over"))) { XCTAssertEqual($0 as? HotkeyError, .tooManyHotkeys(24)) }
        XCTAssertEqual(library.hotkeys.count, 24)
    }
    func testMovingMatchesSwiftUIsOnMove() throws {
        func order(_ offsets: [Int], to destination: Int) throws -> [String] {
            var library = HotkeyLibrary()
            for id in ["a", "b", "c", "d", "e"] { try library.add(hotkey(id.uppercased(), id: id)) }
            library.move(fromOffsets: offsets, toOffset: destination)
            return library.hotkeys.map(\.id)
        }
        XCTAssertEqual(try order([0], to: 3), ["b", "c", "a", "d", "e"], "forward: lands before the item that was at 3")
        XCTAssertEqual(try order([3], to: 1), ["a", "d", "b", "c", "e"], "backward")
        XCTAssertEqual(try order([4], to: 0), ["e", "a", "b", "c", "d"])
        XCTAssertEqual(try order([0], to: 5), ["b", "c", "d", "e", "a"], "to the very end")
        XCTAssertEqual(try order([1, 3], to: 0), ["b", "d", "a", "c", "e"], "several at once keep their order")
        XCTAssertEqual(try order([1, 2], to: 5), ["a", "d", "e", "b", "c"])
        XCTAssertEqual(try order([2], to: 2), ["a", "b", "c", "d", "e"], "onto itself changes nothing")
        XCTAssertEqual(try order([2], to: 3), ["a", "b", "c", "d", "e"], "just after itself changes nothing")
        XCTAssertEqual(try order([9], to: 0), ["a", "b", "c", "d", "e"], "out of range is ignored")
    }
    func testTheLibraryRoundTripsAndSurvivesBadStorage() throws {
        var library = HotkeyLibrary()
        try library.add(hotkey("Clear", id: "a")); try library.add(hotkey("Bail", [.key(.control("c")), .text("exit"), .key(.enter)], id: "b"))
        XCTAssertEqual(HotkeyLibrary(encoded: library.encoded), library)
        for garbage in [nil, "", "not json", "{}", "[]", "{\"v\":2,\"hotkeys\":[]}", "{\"v\":1}", "{\"v\":1,\"hotkeys\":\"x\"}"] as [String?] {
            XCTAssertEqual(HotkeyLibrary(encoded: garbage), HotkeyLibrary(), "\(String(describing: garbage))")
        }
        // Bad entries are dropped one by one; the good ones stay, in order, without repeats or built-in ids.
        let mixed = """
        {"v":1,"hotkeys":[
          {"id":"a","label":"Good","steps":[{"key":"Enter"}]},
          {"id":"b","label":"","steps":[{"key":"Enter"}]},
          {"id":"a","label":"Repeat","steps":[{"key":"Tab"}]},
          {"id":"builtin.c-c","label":"Fake","steps":[{"text":"x"}]},
          {"id":"c","label":"Bad key","steps":[{"key":"F1"}]},
          {"id":"d","label":"Also good","steps":[{"text":"ls"},{"key":"Enter"}]}
        ]}
        """
        XCTAssertEqual(HotkeyLibrary(encoded: mixed).hotkeys.map(\.label), ["Good", "Also good"])
    }
    func testAStoredLibraryLongerThanTheLimitIsCut() throws {
        let entries = (0..<40).map { "{\"id\":\"h\($0)\",\"label\":\"H\($0)\",\"steps\":[{\"key\":\"Tab\"}]}" }.joined(separator: ",")
        XCTAssertEqual(HotkeyLibrary(encoded: "{\"v\":1,\"hotkeys\":[\(entries)]}").hotkeys.count, HotkeyLibrary.maxHotkeys)
    }
}

/// Alt (Meta) on the key bar: readline style, Escape first and then the key or text.
final class AltMappingTests: XCTestCase {
    func testAltThenALetterSendsEscapeThenTheLetter() {
        var mapper = KeyMapper()
        mapper.toggleAlt()
        XCTAssertTrue(mapper.altArmed)
        XCTAssertEqual(mapper.insert("b"), [.key(.escape), .text("b")])
        XCTAssertFalse(mapper.altArmed, "one key only")
        XCTAssertEqual(mapper.insert("b"), [.text("b")])
    }
    func testAltThenAKeySendsEscapeThenTheKey() {
        var mapper = KeyMapper()
        mapper.toggleAlt()
        XCTAssertEqual(mapper.press(.left), [.key(.escape), .key(.left)])
        XCTAssertFalse(mapper.altArmed)
        mapper.toggleAlt()
        XCTAssertEqual(mapper.deleteBackward(), [.key(.escape), .key(.backspace)], "Alt+Backspace deletes a word in readline")
        XCTAssertFalse(mapper.altArmed)
    }
    func testAltAndControlTogetherSendEscapeThenTheControlKey() {
        var mapper = KeyMapper()
        mapper.toggleControl(); mapper.toggleAlt()
        XCTAssertEqual(mapper.insert("h"), [.key(.escape), .key(.control("h"))])
        XCTAssertFalse(mapper.controlArmed)
        XCTAssertFalse(mapper.altArmed)
    }
    func testAltOnlyPrefixesTextOnceAndKeepsTheRestLiteral() {
        var mapper = KeyMapper()
        mapper.toggleAlt()
        XCTAssertEqual(mapper.insert("ab\n"), [.key(.escape), .text("ab"), .key(.enter)])
        XCTAssertEqual(mapper.insert("cd"), [.text("cd")])
    }
    func testAltStaysArmedWhenNothingWouldBeSent() {
        var mapper = KeyMapper()
        mapper.toggleAlt()
        XCTAssertEqual(mapper.insert("\u{7}"), [], "a bare control character is dropped")
        XCTAssertTrue(mapper.altArmed)
        XCTAssertEqual(mapper.insert(""), [])
        XCTAssertTrue(mapper.altArmed)
        XCTAssertEqual(mapper.insert("f"), [.key(.escape), .text("f")])
    }
    func testTogglingAltTwiceDisarmsIt() {
        var mapper = KeyMapper()
        mapper.toggleAlt(); mapper.toggleAlt()
        XCTAssertFalse(mapper.altArmed)
        XCTAssertEqual(mapper.press(.tab), [.key(.tab)])
    }
    func testAHotkeyIsSentAsDefinedAndConsumesBothModifiers() {
        var mapper = KeyMapper()
        mapper.toggleAlt(); mapper.toggleControl()
        let clear = Hotkey(label: "Clear", steps: [.text("/clear"), .key(.enter)])
        XCTAssertEqual(mapper.run(clear), [.text("/clear"), .key(.enter)], "no Escape in front")
        XCTAssertFalse(mapper.altArmed)
        XCTAssertFalse(mapper.controlArmed)
        XCTAssertEqual(mapper.run(Hotkey.builtIn[0]), [.key(.control("c"))])
    }
    func testDisarmModifiersClearsBoth() {
        var mapper = KeyMapper()
        mapper.toggleAlt(); mapper.toggleControl()
        mapper.disarmModifiers()
        XCTAssertFalse(mapper.altArmed)
        XCTAssertFalse(mapper.controlArmed)
    }
    func testEscapeThenTextSurvivesTheBufferIntoValidBatches() throws {
        var mapper = KeyMapper()
        var buffer = KeyBuffer()
        mapper.toggleAlt()
        XCTAssertTrue(buffer.append(mapper.insert("b"), now: Date(timeIntervalSince1970: 1_000)))
        let batch = try XCTUnwrap(buffer.nextBatch())
        XCTAssertEqual(batch.items, [.key(.escape), .text("b")])
        XCTAssertNoThrow(try KeyItem.validate(batch: batch.items))
    }
}
