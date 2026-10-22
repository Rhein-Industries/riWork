import Foundation

/// Rewrites the open hypothesis and a 32-word overlap; the rest of a long dictation is already rewritten.
public struct DictationTextCache: Sendable {
    private let vocabulary: SpeechVocabulary
    private var settled = ""
    private var overlap = ""
    private var beginsSegment = true
    public init(vocabulary: SpeechVocabulary) { self.vocabulary = vocabulary }
    public func text(open: String = "") -> String { settled + vocabulary.rewrite(overlap + continuation(open)) }
    public mutating func beginSegment() { beginsSegment = true }
    public mutating func settle(_ text: String) {
        let rewritten = vocabulary.rewrite(overlap + continuation(text))
        let words = rewritten.split(whereSeparator: \.isWhitespace)
        if words.count > 32 {
            let cut = words[words.count - 32].startIndex
            settled += rewritten[..<cut]
            overlap = String(rewritten[cut...])
        } else { overlap = rewritten }
        beginsSegment = false
    }
    private func continuation(_ text: String) -> String {
        guard beginsSegment, let previous = overlap.last ?? settled.last, !text.isEmpty else { return text }
        var next = text.trimmingCharacters(in: .whitespacesAndNewlines)
        // A recognizer capitalizes every new request. Preserve sentence starts, acronyms and vocabulary names.
        let precedingWordEnd = overlap.last(where: { !$0.isWhitespace }) ?? settled.last(where: { !$0.isWhitespace }) ?? previous
        if !".!?…\n".contains(precedingWordEnd), let word = next.split(whereSeparator: \.isWhitespace).first,
           let first = word.first, first.isUppercase, word != "I", word.dropFirst().allSatisfy({ !$0.isLetter || $0.isLowercase }),
           !vocabulary.terms.contains(where: { $0 == word.trimmingCharacters(in: .punctuationCharacters) && $0.first?.isUppercase == true }) {
            next.replaceSubrange(next.startIndex...next.startIndex, with: first.lowercased())
        }
        return (previous.isWhitespace ? "" : " ") + next
    }
}

/// A request replaces its own hypothesis. A substantial reset within a request first commits the preceding utterance.
public struct RollingDictationTranscript: Sendable {
    public private(set) var generation = 0
    private var tail = ""
    private var cache: DictationTextCache
    public init(vocabulary: SpeechVocabulary = SpeechVocabulary(terms: [])) { cache = DictationTextCache(vocabulary: vocabulary) }
    public var text: String { cache.text(open: tail) }
    @discardableResult public mutating func update(_ text: String, generation: Int) -> Bool {
        guard generation == self.generation else { return false }
        let next = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !next.isEmpty else { return true } // Empty terminal results must keep the preceding partial.
        if next.count * 2 < tail.count, !tail.lowercased().hasPrefix(next.lowercased()) {
            let oldWords = tail.split(whereSeparator: \.isWhitespace)
            let newWords = next.split(whereSeparator: \.isWhitespace)
            let maximum = min(8, oldWords.count, newWords.count)
            let shared = (1...max(1, maximum)).reversed().first { count in
                count <= maximum && oldWords.suffix(count).map { $0.lowercased() } == newWords.prefix(count).map { $0.lowercased() }
            } ?? 0
            // A reset can repeat the previous utterance's last words; leave that overlap in the new hypothesis only.
            let cut = shared == 0 ? tail.endIndex : oldWords[oldWords.count - shared].startIndex
            cache.settle(String(tail[..<cut]).trimmingCharacters(in: .whitespacesAndNewlines))
            cache.beginSegment()
        }
        tail = next
        return true
    }
    public mutating func roll() {
        cache.settle(tail)
        cache.beginSegment()
        tail = ""
        generation += 1
    }
}

/// Tracks actual buffers, including silence. One missing-buffer interval gets one recovery and a further three-second grace.
public final class AudioBufferWatchdog: @unchecked Sendable {
    public enum Action: Sendable, Equatable { case none, recover, lost }
    private let lock = NSLock()
    private var lastBuffer: TimeInterval?
    private var recoveryStarted: TimeInterval?
    private var firstSpeech: TimeInterval?
    public init() {}
    public func start(at time: TimeInterval = ProcessInfo.processInfo.systemUptime) {
        lock.lock(); defer { lock.unlock() }
        lastBuffer = time
    }
    public func received(at time: TimeInterval = ProcessInfo.processInfo.systemUptime, nonSilent: Bool = false) {
        lock.lock(); defer { lock.unlock() }
        lastBuffer = time
        recoveryStarted = nil
        if nonSilent, firstSpeech == nil { firstSpeech = time }
    }
    public var firstNonSilent: TimeInterval? { lock.lock(); defer { lock.unlock() }; return firstSpeech }
    public func beginRecovery(at time: TimeInterval = ProcessInfo.processInfo.systemUptime) {
        lock.lock(); defer { lock.unlock() }
        if recoveryStarted == nil { recoveryStarted = time }
    }
    public func poll(at time: TimeInterval = ProcessInfo.processInfo.systemUptime) -> Action {
        lock.lock(); defer { lock.unlock() }
        if let recoveryStarted { return time - recoveryStarted >= 3 ? .lost : .none }
        guard let lastBuffer, time - lastBuffer >= 3 else { return .none }
        recoveryStarted = time
        return .recover
    }
}
