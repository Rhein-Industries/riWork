import XCTest
import Speech
import RiWorkCore
@testable import RiWorkRemote

/// Recorded speech through the real on-device recognizers, with and without the session's vocabulary.
///
/// The clips are made on the Mac with `scripts/make-speech-clips.sh` (the `say` voice reading developer sentences) and are read from
/// `RIWORK_SPEECH_CLIPS` (`TEST_RUNNER_RIWORK_SPEECH_CLIPS=… xcodebuild test`), or from `/tmp/riwork-speech-clips`. Without them, or
/// without an installed speech model, the tests are skipped: they show what the recognizer does, they are not about the app's logic.
@MainActor final class SpeechClipTests: XCTestCase {
    /// What each clip says, and the identifiers in it that the vocabulary should bring out as written.
    static let clips: [(sentence: String, identifiers: [String])] = [
        ("git checkout ios-speech-input and run swift test", ["git", "ios-speech-input"]),
        ("Ask Claude to refactor the KeyBarView capsule inset", ["Claude", "KeyBarView"]),
        ("Codex, open RemoteModel plus Keys dot swift and fix the pending key buffer", ["Codex", "RemoteModel"]),
        ("run xcodebuild with the iPhone 17 Pro simulator", ["xcodebuild", "iPhone 17 Pro"]),
        ("cd into the riWork worktree and run npm install", ["RiWork", "worktree", "npm"]),
        ("kubectl get pods in the staging namespace", ["kubectl"]),
        ("Tell Codex to rename SpeechVocabulary to SessionVocabulary", ["Codex", "SpeechVocabulary", "SessionVocabulary"]),
        ("grep for ChatComposer in the ios folder", ["grep", "ChatComposer"])
    ]
    /// A session like the one these were said in: the branch, the project, and a screen that shows the identifiers.
    static let session = SpeechVocabulary.build(SpeechVocabularySources(
        names: ["MacBook Pro", "riWork", "ios-speech-input", "Codex worker", "Terminal"],
        paths: ["/Users/me/ocean/riWork", "/Users/me/ocean/riWork/.claude/worktrees/ios-speech-input"],
        text: """
        $ git status
        On branch ios-speech-input
        modified:   ios/RiWorkRemote/KeyBar.swift   (KeyBarView, capsuleInset)
        modified:   ios/RiWorkRemote/RemoteModel+Keys.swift
        new file:   ios/Core/SpeechVocabulary.swift (SessionVocabulary)
        ios/RiWorkRemote/ChatComposer.swift: ChatComposer
        $ xcodebuild -scheme RiWorkRemote test
        $ kubectl get pods -n staging
        """))

    private var directory: URL? {
        let path = ProcessInfo.processInfo.environment["RIWORK_SPEECH_CLIPS"] ?? "/tmp/riwork-speech-clips"
        let url = URL(fileURLWithPath: path)
        return FileManager.default.fileExists(atPath: url.appendingPathComponent("clip1.wav").path) ? url : nil
    }

    /// A simulator without the speech model can wait for it forever: that is a skip, not a failure.
    private static func withDeadline(_ seconds: Double = 60, _ work: @escaping @MainActor () async throws -> String) async throws -> String {
        try await withThrowingTaskGroup(of: String?.self) { group in
            group.addTask { try await work() }
            group.addTask { try await Task.sleep(for: .seconds(seconds)); return nil }
            defer { group.cancelAll() }
            guard let first = try await group.next(), let text = first else { throw XCTSkip("The recognizer did not answer within \(Int(seconds)) s here") }
            return text
        }
    }

    struct Score { var plain = 0, rewriteOnly = 0, contextOnly = 0, both = 0, total = 0 }

    /// Runs every clip through a recognizer twice, without and with the session's contextual strings, and counts the identifiers that
    /// came out exactly as written: plain, plain + rewrite, contextual strings alone, and both (what the app does).
    private func compare(_ name: String, _ recognize: @escaping @MainActor (URL, SpeechVocabulary) async throws -> String) async throws -> Score {
        let directory = try XCTUnwrap(directory)
        var score = Score()
        var seconds: [Double] = []
        for (index, clip) in Self.clips.enumerated() {
            let url = directory.appendingPathComponent("clip\(index + 1).wav")
            let start = Date()
            let plain = try await Self.withDeadline { try await recognize(url, SpeechVocabulary(terms: [])) }
            seconds.append(Date().timeIntervalSince(start))
            let context = try await Self.withDeadline { try await recognize(url, Self.session) }
            let variants = [plain, Self.session.rewrite(plain), context, Self.session.rewrite(context)]
            let hits = variants.map { text in clip.identifiers.filter { text.contains($0) }.count }
            score.plain += hits[0]; score.rewriteOnly += hits[1]; score.contextOnly += hits[2]; score.both += hits[3]
            score.total += clip.identifiers.count
            print("SPEECH \(name) clip\(index + 1) said: \(clip.sentence)\n  plain:   \(plain) [\(hits[0])]\n  context: \(context) [\(hits[2])]\n  app:     \(variants[3]) [\(hits[3])]")
        }
        let median = seconds.sorted()[seconds.count / 2]
        print("SPEECH \(name) of \(score.total) identifiers as written: plain \(score.plain), rewrite only \(score.rewriteOnly), contextual strings only \(score.contextOnly), both \(score.both); median \(String(format: "%.2f", median)) s per clip")
        return score
    }

    func testTheShippedEngineGetsMoreIdentifiersRightWithTheSession() async throws {
        try XCTSkipIf(directory == nil, "No speech clips (scripts/make-speech-clips.sh)")
        let score: Score
        if #available(iOS 26, *), SpeechTranscriber.isAvailable {
            do {
                score = try await compare("SpeechTranscriber") { try await AnalyzerSpeechEngine.transcribe(file: $0, vocabulary: $1, rewrite: false) }
            } catch DictationProblem.unsupported, DictationProblem.modelUnavailable {
                throw XCTSkip("No speech model for this simulator")
            }
        } else {
            try XCTSkipIf(SFSpeechRecognizer.authorizationStatus() != .authorized, "Speech recognition is not authorized for the test host")
            score = try await compare("SFSpeechRecognizer") { try await RecognizerSpeechEngine.transcribe(file: $0, vocabulary: $1, rewrite: false) }
        }
        XCTAssertGreaterThan(score.both, score.plain, "the session's vocabulary should bring out identifiers")
    }

    /// The older recognizer (iOS 18 to 25), when the test host may use it: it takes contextual strings, which `SpeechTranscriber` ignores.
    func testTheOlderRecognizerForComparison() async throws {
        try XCTSkipIf(directory == nil, "No speech clips (scripts/make-speech-clips.sh)")
        try XCTSkipIf(SFSpeechRecognizer.authorizationStatus() != .authorized || RecognizerSpeechEngine.recognizer() == nil,
                      "Speech recognition is not authorized for the test host, or has no on-device model here")
        let score = try await compare("SFSpeechRecognizer") { try await RecognizerSpeechEngine.transcribe(file: $0, vocabulary: $1, rewrite: false) }
        XCTAssertGreaterThanOrEqual(score.both, score.plain)
    }
}
