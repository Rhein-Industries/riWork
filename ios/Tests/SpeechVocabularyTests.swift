import XCTest
@testable import RiWorkCore

final class SpeechVocabularyTests: XCTestCase {
    // MARK: Building

    func testTheAgentsComeFirstThenTheSessionsNamesThenItsScreenThenTheCommandLine() {
        let vocabulary = SpeechVocabulary.build(SpeechVocabularySources(names: ["riWork", "ios-speech-input"], paths: ["/Users/me/ocean/riWork/ios"],
                                                                        text: "error in KeyBarView.swift: capsuleInset"))
        XCTAssertEqual(Array(vocabulary.terms.prefix(4)), SpeechVocabulary.agents)
        let index = { (term: String) in vocabulary.terms.firstIndex(of: term) ?? .max }
        XCTAssertLessThan(index("ios-speech-input"), index("KeyBarView.swift"))
        XCTAssertLessThan(index("capsuleInset"), index("kubectl"))
        XCTAssertTrue(vocabulary.terms.contains("ios"), "the last components of the working directory")
        XCTAssertFalse(vocabulary.terms.contains("Users"))
    }
    func testItIsBoundedAndHasNoRepeatsWhateverTheScreenHolds() {
        let screen = (0..<2_000).map { "someIdentifier\($0 % 300)Name other_word_\($0)" }.joined(separator: " ")
        let vocabulary = SpeechVocabulary.build(SpeechVocabularySources(names: Array(repeating: "riWork", count: 50) + ["RIWORK", "Claude"], text: screen))
        XCTAssertLessThanOrEqual(vocabulary.terms.count, SpeechVocabulary.limit)
        XCTAssertEqual(Set(vocabulary.terms.map { $0.lowercased() }).count, vocabulary.terms.count, "no repeats, ignoring case")
        XCTAssertEqual(vocabulary.terms.filter { $0.lowercased() == "riwork" }.count, 1)
        let fromScreen = vocabulary.terms.filter { $0.hasPrefix("someIdentifier") || $0.hasPrefix("other_word") }
        XCTAssertLessThanOrEqual(fromScreen.count, SpeechVocabulary.textLimit, "the screen leaves room for the command line's words")
        XCTAssertTrue(vocabulary.terms.contains("xcodebuild"))
    }
    func testOnlyTheEndOfALongScreenIsRead() {
        let old = "oldIdentifierAtTheTop " + String(repeating: "plain words here ", count: 1_000)
        let vocabulary = SpeechVocabulary.build(SpeechVocabularySources(text: old + " newIdentifier"))
        XCTAssertTrue(vocabulary.terms.contains("newIdentifier"))
        XCTAssertFalse(vocabulary.terms.contains("oldIdentifierAtTheTop"))
    }
    func testASmallerLimitIsKept() {
        XCTAssertEqual(SpeechVocabulary.build(SpeechVocabularySources(), limit: 10).terms.count, 10)
    }

    // MARK: Identifiers on screen

    func testIdentifiersAreTheThingsSaidAsOneWord() {
        let text = """
        commit 3f9c2a1b7d Merge branch 'ios-native' into main https://github.com/x/y
        modified: ios/RiWorkRemote/KeyBar.swift  snake_case_name  --force  1234567  the plain words  JSON  a  ok
        """
        let found = Set(SpeechVocabulary.identifiers(in: text))
        XCTAssertEqual(found, ["ios-native", "RiWorkRemote", "KeyBar.swift", "KeyBar", "snake_case_name", "JSON"], "a file also by its name alone")
    }
    func testIdentifiersSeenMoreOftenAndLaterRankFirst() {
        let found = SpeechVocabulary.identifiers(in: "alphaOne betaTwo alphaOne gammaThree betaTwo alphaOne deltaFour", limit: 3)
        XCTAssertEqual(found, ["alphaOne", "betaTwo", "deltaFour"])
    }

    // MARK: How terms are said

    func testSpokenWords() {
        XCTAssertEqual(SpeechVocabulary.spokenWords(of: "KeyBarView"), ["key", "bar", "view"])
        XCTAssertEqual(SpeechVocabulary.spokenWords(of: "ios-speech-input"), ["ios", "speech", "input"])
        XCTAssertEqual(SpeechVocabulary.spokenWords(of: "RemoteModel+Keys.swift"), ["remote", "model", "plus", "keys", "dot", "swift"])
        XCTAssertEqual(SpeechVocabulary.spokenWords(of: "snake_case"), ["snake", "case"])
        XCTAssertEqual(SpeechVocabulary.spokenWords(of: "iPhone17"), ["i", "phone", "17"])
    }

    // MARK: Rewriting what was heard

    private let session = SpeechVocabulary(terms: ["KeyBarView", "ios-speech-input", "RemoteModel+Keys.swift", "SessionVocabulary", "RiWork",
                                                    "ChatComposer", "Codex", "git", "iOS"])
    func testIdentifiersHeardAsWordsComeBackAsWritten() {
        XCTAssertEqual(session.rewrite("Ask Claude to refactor the key bar view capsule inset."), "Ask Claude to refactor the KeyBarView capsule inset.")
        XCTAssertEqual(session.rewrite("Check out IOS speech input, then run swift test."), "Check out ios-speech-input, then run swift test.")
        XCTAssertEqual(session.rewrite("open remote model plus keys.swift and fix it"), "open RemoteModel+Keys.swift and fix it")
        XCTAssertEqual(session.rewrite("rename it to session vocabulary"), "rename it to SessionVocabulary")
        XCTAssertEqual(session.rewrite("grep for chat composer in the iOS folder"), "grep for ChatComposer in the iOS folder")
    }
    func testASingleWordChangesOnlyIntoATermSpelledUnlikeAnOrdinaryWord() {
        XCTAssertEqual(session.rewrite("open riwork"), "open RiWork")
        XCTAssertEqual(session.rewrite("ask codex"), "ask codex", "Codex is spelled like a word: left as heard")
        XCTAssertEqual(session.rewrite("Git status"), "Git status")
    }
    func testOrdinaryTextIsLeftAlone() {
        let text = "Please look at the bar and the view, then tell me what you think."
        XCTAssertEqual(session.rewrite(text), text)
        XCTAssertEqual(SpeechVocabulary(terms: []).rewrite("key bar view"), "key bar view")
        XCTAssertEqual(session.rewrite(""), "")
    }

    // MARK: Text for its field

    func testATerminalLineHasNoFullStopAndACommandInLowerCase() {
        XCTAssertEqual(DictatedText.forTerminal("Git status."), "git status")
        XCTAssertEqual(DictatedText.forTerminal("Kubectl get pods.\n"), "kubectl get pods")
        XCTAssertEqual(DictatedText.forTerminal("Fix the tests."), "Fix the tests", "a sentence for an agent keeps its capital")
        XCTAssertEqual(DictatedText.forTerminal("ls ..."), "ls ...")
        XCTAssertEqual(DictatedText.forTerminal("one\ntwo"), "one two")
    }
    func testInsertingAtTheCaretKeepsWordsApart() {
        var result = DictatedText.insert("fix the build", into: "Please", at: NSRange(location: 6, length: 0))
        XCTAssertEqual(result.text, "Please fix the build"); XCTAssertEqual(result.caret, 20)
        result = DictatedText.insert("quickly", into: "Do it now.", at: NSRange(location: 3, length: 0))
        XCTAssertEqual(result.text, "Do quickly it now.")
        result = DictatedText.insert("Hello", into: "", at: NSRange(location: 0, length: 0))
        XCTAssertEqual(result.text, "Hello"); XCTAssertEqual(result.caret, 5)
        result = DictatedText.insert("there", into: "Hi .", at: NSRange(location: 3, length: 0))
        XCTAssertEqual(result.text, "Hi there.")
        result = DictatedText.insert("new", into: "replace old words", at: NSRange(location: 8, length: 3))
        XCTAssertEqual(result.text, "replace new words")
        result = DictatedText.insert("x", into: "abc", at: NSRange(location: 99, length: 5))
        XCTAssertEqual(result.text, "abc x", "a range past the end is clipped to it")
        XCTAssertEqual(DictatedText.insert("  ", into: "abc", at: NSRange(location: 1, length: 0)).text, "abc")
    }
    func testJoining() {
        XCTAssertEqual(DictatedText.joined("", "git status"), "git status")
        XCTAssertEqual(DictatedText.joined("git", "status "), "git status")
        XCTAssertEqual(DictatedText.joined("git", ""), "git")
    }
}
