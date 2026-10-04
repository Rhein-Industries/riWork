import Foundation

/// What the session on screen offers dictation to listen for: the names around it and the words on its screen. Read fresh each time
/// dictation starts, and never stored or sent anywhere.
public struct SpeechVocabularySources: Sendable, Equatable {
    /// Names that matter most: the desktop, project, worktree branches, tab and chat titles, models.
    public var names: [String]
    /// Working directories and roots: their last components are names too ("riWork", "ios-speech-input").
    public var paths: [String]
    /// What is on screen: the terminal's output or the chat's recent messages, oldest first. Only the end of it is read.
    public var text: String
    public init(names: [String] = [], paths: [String] = [], text: String = "") {
        self.names = names; self.paths = paths; self.text = text
    }
}

/// The words a dictation is biased towards, bounded and without repeats, most important first.
///
/// It does two things with them:
/// - hands them to the recognizer as contextual strings, where the recognizer takes them;
/// - rewrites what was heard onto them (`rewrite`): "key bar view" becomes `KeyBarView` and "ios speech input" `ios-speech-input`
///   when those are in the session. This works the same whatever the recognizer, and is what makes identifiers come out as written.
public struct SpeechVocabulary: Sendable, Equatable {
    /// Apple's recognizers advise no more than about a hundred contextual strings.
    public static let limit = 100
    /// At most this many come from the text on screen, so names and the fixed words always have room.
    public static let textLimit = 50
    /// Only the end of the screen's text is read.
    public static let textWindow = 8_000
    /// The agents and the app, which are said all the time and recognized badly without help.
    public static let agents = ["Claude", "Codex", "RiWork", "Claude Code"]
    /// Words of the command line that a general recognizer gets wrong or spells otherwise.
    public static let commandLine = [
        "git", "npm", "pnpm", "npx", "yarn", "cargo", "rustc", "swift", "xcodebuild", "xcrun", "simctl", "kubectl", "docker", "tmux", "ssh",
        "grep", "ripgrep", "rg", "sed", "awk", "ls", "cd", "mkdir", "rm", "cat", "curl", "uv", "pip", "python", "make", "brew", "jq",
        "worktree", "repo", "rebase", "stash", "diff", "commit", "merge", "pull request", "PR", "README", "JSON", "YAML",
        "TypeScript", "JavaScript", "SwiftUI", "UIKit", "iOS", "macOS", "localhost", "stdout", "stderr", "env", "sudo", "regex", "lint"
    ]

    public private(set) var terms: [String]
    /// Spoken key ("keybarview") to the term as written, for the rewrite.
    private var written: [String: String] = [:]
    /// The longest term in words, so the rewrite does not look further.
    private var longest = 1

    public init(terms: [String]) {
        var seen = Set<String>(), kept: [String] = []
        for term in terms {
            let trimmed = term.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty, trimmed.count <= 60, seen.insert(trimmed.lowercased()).inserted else { continue }
            kept.append(trimmed)
            if kept.count == Self.limit { break }
        }
        self.terms = kept
        for term in kept {
            let key = Self.key(term)
            guard key.count >= 3, written[key] == nil else { continue }
            written[key] = term
            longest = max(longest, Self.spokenWords(of: term).count + 2)
        }
    }

    /// The vocabulary for a session: the agents, the session's names, identifiers from its screen (those seen most and last first),
    /// then the command line's words, up to `limit`.
    public static func build(_ sources: SpeechVocabularySources, limit: Int = limit) -> SpeechVocabulary {
        var terms = agents
        terms += sources.names.flatMap { name in [name] + identifiers(in: name) }
        terms += sources.paths.flatMap(pathNames)
        terms += identifiers(in: String(sources.text.suffix(textWindow)), limit: textLimit)
        terms += commandLine
        var vocabulary = SpeechVocabulary(terms: terms)
        if vocabulary.terms.count > limit { vocabulary = SpeechVocabulary(terms: Array(vocabulary.terms.prefix(limit))) }
        return vocabulary
    }

    /// The last two components of a path, which are what a person calls a project or a worktree.
    static func pathNames(_ path: String) -> [String] {
        let parts = path.split(separator: "/").map(String.init).filter { !$0.isEmpty && $0 != "~" && !$0.hasPrefix(".") }
        return parts.suffix(2).reversed().filter { $0.count >= 2 && $0.count <= 40 && !["Users", "home", "src", "tmp"].contains($0) }
    }

    /// Identifiers in text: CamelCase, snake_case, kebab-case, file names and acronyms, the way a person would say them aloud as one
    /// thing. Plain words are left out (the recognizer knows them), and so are hashes, numbers and links. Ranked by how often they
    /// appear, then by how late.
    public static func identifiers(in text: String, limit: Int = .max) -> [String] {
        var counts: [String: Int] = [:], last: [String: Int] = [:]
        var index = 0
        for raw in text.split(whereSeparator: { $0.isWhitespace || "()[]{}<>\"'`,;=|".contains($0) }) {
            guard !raw.contains("://") else { continue }
            for piece in raw.split(separator: "/") {
                let token = piece.trimmingCharacters(in: CharacterSet(charactersIn: ".:!?*#@$%^&~-_"))
                guard isIdentifier(token) else { continue }
                // A file is also talked about by its name alone: `SpeechVocabulary.swift` is "speech vocabulary" too.
                let stem = token.lastIndex(of: ".").map { String(token[..<$0]) }
                for found in [token, stem].compactMap({ $0 }) where found == token || isIdentifier(found) {
                    counts[found, default: 0] += 1
                    last[found] = index
                    index += 1
                }
            }
        }
        let ranked = counts.keys.sorted { a, b in
            counts[a]! != counts[b]! ? counts[a]! > counts[b]! : last[a]! > last[b]!
        }
        return Array(ranked.prefix(limit))
    }
    static func isIdentifier(_ token: String) -> Bool {
        guard (3...40).contains(token.count), let first = token.first, first.isLetter else { return false }
        let scalars = Array(token.unicodeScalars)
        guard scalars.allSatisfy({ $0.isASCII && (CharacterSet.alphanumerics.contains($0) || "-_.+".unicodeScalars.contains($0)) }) else { return false }
        let letters = token.filter(\.isLetter)
        // A commit hash or an id that happens to start with a letter.
        if token.allSatisfy(\.isHexDigit), token.contains(where: \.isNumber) { return false }
        if token.filter(\.isNumber).count > letters.count { return false }
        let camel = zip(token, token.dropFirst()).contains { $0.isLowercase && $1.isUppercase }
        let joined = zip(token, token.dropFirst()).contains { ("-_".contains($0) && $1.isLetter) }
        let file: Bool = {
            guard let dot = token.lastIndex(of: "."), dot != token.startIndex else { return false }
            let ext = token[token.index(after: dot)...]
            return (1...5).contains(ext.count) && ext.allSatisfy(\.isLetter)
        }()
        let acronym = letters.count >= 2 && letters.count <= 8 && letters.allSatisfy(\.isUppercase) && letters.count == token.count
        return camel || joined || file || acronym
    }

    /// How a term is said, word by word: "KeyBarView" is "key bar view", "RemoteModel+Keys.swift" "remote model plus keys dot swift".
    public static func spokenWords(of term: some StringProtocol) -> [String] {
        var words: [String] = [], current = ""
        func flush() { if !current.isEmpty { words.append(current.lowercased()); current = "" } }
        var previous: Character?
        for character in term {
            switch character {
            case "+": flush(); words.append("plus")
            case ".": flush(); words.append("dot")
            case "/": flush(); words.append("slash")
            case "-", "_", " ": flush()
            default:
                if let previous, previous.isLowercase, character.isUppercase { flush() }
                else if let previous, previous.isLetter != character.isLetter, previous.isNumber || character.isNumber { flush() }
                current.append(character)
            }
            previous = character
        }
        flush()
        return words
    }
    /// The form a term and what was heard are compared in: its spoken words run together.
    static func key(_ text: some StringProtocol) -> String { spokenWords(of: text).joined() }

    /// What was heard, with the session's terms put back as they are written. Runs of up to a few words are matched against the spoken
    /// form of each term, longest first; punctuation the recognizer put around them is kept. A single word is rewritten only into a
    /// term that is spelled unlike plain English (inner capitals, digits, symbols), so an ordinary word never changes.
    public func rewrite(_ text: String) -> String {
        guard !written.isEmpty else { return text }
        let words = Self.words(in: text)
        guard !words.isEmpty else { return text }
        var out = "", cursor = text.startIndex, i = 0
        while i < words.count {
            var matched: (end: Int, term: String)?
            var j = min(words.count, i + longest) - 1
            while j >= i {
                let key = words[i...j].map { Self.key($0.core) }.joined()
                if let term = written[key], j > i || Self.isDistinctive(term, heard: words[i].core) { matched = (j, term); break }
                j -= 1
            }
            if let matched {
                out += text[cursor..<words[i].core.startIndex]
                out += matched.term
                cursor = words[matched.end].core.endIndex
                i = matched.end + 1
            } else {
                i += 1
            }
        }
        out += text[cursor...]
        return out
    }
    /// A one-word rewrite only into something that is not spelled like an ordinary word, and only when it changes the spelling.
    private static func isDistinctive(_ term: String, heard: Substring) -> Bool {
        guard term != heard else { return false }
        let inner = term.dropFirst().contains(where: \.isUppercase)
        return inner || term.contains(where: { $0.isNumber || "+._-".contains($0) })
    }
    /// The words of text, each without the punctuation around it ("inset." is "inset").
    private static func words(in text: String) -> [(whole: Substring, core: Substring)] {
        text.split(whereSeparator: \.isWhitespace).compactMap { whole in
            let edge = CharacterSet(charactersIn: ",.;:!?\"'()[]")
            var start = whole.startIndex, end = whole.endIndex
            while start < end, whole[start].unicodeScalars.allSatisfy(edge.contains) { start = whole.index(after: start) }
            while end > start, whole[whole.index(before: end)].unicodeScalars.allSatisfy(edge.contains) { end = whole.index(before: end) }
            return start < end ? (whole, whole[start..<end]) : nil
        }
    }
}

/// Dictated text made ready for where it goes.
public enum DictatedText {
    /// For a command line: no full stop at the end, and a command word the recognizer capitalized as the start of a sentence goes back
    /// to lower case ("Git status." is `git status`).
    public static func forTerminal(_ text: String) -> String {
        var line = text.components(separatedBy: .newlines).joined(separator: " ").trimmingCharacters(in: .whitespaces)
        if line.hasSuffix("."), !line.hasSuffix("..") { line.removeLast() }
        if let firstWord = line.split(separator: " ").first {
            let lower = firstWord.lowercased()
            if firstWord != lower, SpeechVocabulary.commandLine.contains(lower) {
                line = lower + line.dropFirst(firstWord.count)
            }
        }
        return line
    }
    /// Two pieces of dictation one after the other, with a space between them when both have words.
    public static func joined(_ first: String, _ second: String) -> String {
        let a = first.trimmingCharacters(in: .whitespaces), b = second.trimmingCharacters(in: .whitespaces)
        return a.isEmpty ? b : (b.isEmpty ? a : a + " " + b)
    }
    /// Text to insert into existing text at `range` (UTF-16 offsets): a space is added between it and a word right before or after, so
    /// dictating at the end of a sentence does not glue words together. Returns the new text and where the caret goes.
    public static func insert(_ dictated: String, into text: String, at range: NSRange) -> (text: String, caret: Int) {
        let ns = text as NSString
        let safe = NSRange(location: min(max(0, range.location), ns.length), length: 0)
        let clipped = NSRange(location: safe.location, length: min(max(0, range.length), ns.length - safe.location))
        var piece = dictated.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !piece.isEmpty else { return (text, clipped.location) }
        let before = clipped.location > 0 ? ns.substring(with: NSRange(location: clipped.location - 1, length: 1)) : ""
        let afterIndex = clipped.location + clipped.length
        let after = afterIndex < ns.length ? ns.substring(with: NSRange(location: afterIndex, length: 1)) : ""
        if let b = before.first, !b.isWhitespace, !"([{\"'`".contains(b) { piece = " " + piece }
        if let a = after.first, !a.isWhitespace, !".,;:!?)]}".contains(a) { piece += " " }
        let result = ns.replacingCharacters(in: clipped, with: piece)
        return (result, clipped.location + (piece as NSString).length)
    }
}
