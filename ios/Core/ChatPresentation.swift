import Foundation

// The decisions behind the chat screen that are not drawing: what a state is called and how it is shown, what a key does, how an
// answer to a question is put together, and the usage meter's words. Pure, so they are tested without a screen.

// MARK: - State

extension ChatState {
    /// What the tab strip's indicator shows, in the words of terminal agents: a chat that is starting or running is working, one that
    /// waits for an approval or an answer is waiting, and the rest draw nothing.
    public var activity: AgentActivity {
        switch self {
        case .starting, .running: .working
        case .waiting: .waiting
        case .idle, .stopped, .failed, .unknown: .unknown
        }
    }
    /// "Working", "Waiting for input" or nil, for VoiceOver (shared with terminal tabs).
    public var spokenActivity: String? { activity.spoken() }
    /// "Stopped" or "Failed" for VoiceOver, the two states a tab does not draw an indicator for.
    public var spokenCondition: String? {
        switch self {
        case .stopped: "Stopped"
        case .failed: "Failed"
        default: nil
        }
    }
    /// A word for the state, for a row and for VoiceOver.
    public var title: String {
        switch self {
        case .starting: "Starting"
        case .idle: "Ready"
        case .running: "Working"
        case .waiting: "Waiting for input"
        case .stopped: "Stopped"
        case .failed: "Failed"
        case .unknown(let word): word.isEmpty ? "Unknown" : word.prefix(1).uppercased() + word.dropFirst()
        }
    }
}

/// The line under the toolbar that says what the chat is doing when that is not simply "ready".
public enum ChatBanner: Sendable, Equatable {
    case starting(ChatProvider)
    /// The agent is not running; the next message starts it again.
    case stopped
    /// The agent failed. `retry` is the message that was last sent, which Retry sends again; nil when there was none.
    case failed(message: String, retry: String?)

    public init?(state: ChatState, provider: ChatProvider, lastMessage: String?) {
        switch state {
        case .starting: self = .starting(provider)
        case .stopped: self = .stopped
        case .failed(let message): self = .failed(message: TerminalControlError.readable(message), retry: lastMessage)
        case .idle, .running, .waiting, .unknown: return nil
        }
    }
    public var text: String {
        switch self {
        case .starting(let provider): "Starting \(provider.title)…"
        case .stopped: "Stopped. Your next message starts it again."
        case .failed(let message, let retry): retry == nil ? "Failed: \(message) Send a message to try again." : "Failed: \(message)"
        }
    }
}

// MARK: - Keys

/// The keys of a hardware keyboard that the composer reads.
public enum ChatKey: Sendable, Equatable, CaseIterable {
    case `return`, shiftReturn, escape
    /// ⌘⌫ : Escape for a keyboard without one (the Clicks keyboard has none), where Escape means Deny.
    case commandDelete
    /// ⌘. : interrupt.
    case commandPeriod
}

/// The chords of those keys, as the rest of the app writes them (`KeyChord`), so that tests can see none of them is taken by the
/// terminal's shortcuts. The composer claims them as key commands; the terminal's `KeyCapture` is not on a chat screen at all.
public enum ChatKeyBindings {
    public static let period = 0x37
    public static func chord(_ key: ChatKey) -> KeyChord {
        switch key {
        case .return: KeyChord(keyCode: HIDKey.returnKey)
        case .shiftReturn: KeyChord(keyCode: HIDKey.returnKey, modifiers: .shift)
        case .escape: KeyChord(keyCode: HIDKey.escape)
        case .commandDelete: KeyChord(keyCode: HIDKey.backspace, modifiers: .command)
        case .commandPeriod: KeyChord(keyCode: period, modifiers: .command)
        }
    }
}

/// What the screen looks like when a key comes.
public struct ChatKeyContext: Sendable, Equatable {
    public var composerIsEmpty: Bool
    /// The request on top of the approval bar, if there is one.
    public var approval: ChatApproval?
    /// A turn is in progress or waits for the person: there is something to interrupt.
    public var busy: Bool
    /// Connected and nothing being sent just now.
    public var canSend: Bool
    public init(composerIsEmpty: Bool, approval: ChatApproval? = nil, busy: Bool = false, canSend: Bool = true) {
        self.composerIsEmpty = composerIsEmpty; self.approval = approval; self.busy = busy; self.canSend = canSend
    }
    /// With a request waiting and nothing typed, Return, Shift-Return and Escape answer it. Typing anything gives them back to the text.
    public var answersApproval: Bool { approval != nil && composerIsEmpty }
}

public enum ChatKeyAction: Sendable, Equatable {
    case send
    /// Not ours: the text view puts the line break in.
    case insertNewline
    case decide(ChatDecision)
    case interrupt
    /// The key means nothing now and is swallowed.
    case none
}

/// What a key does. ⏎ sends and ⇧⏎ makes a new line; with a request waiting and nothing typed ⏎ allows it, ⇧⏎ allows it for the session
/// and ⎋ (or ⌘⌫) denies it; ⌘. interrupts. A decision the provider does not offer is never made on a key: ⏎ does not choose Deny for you.
public enum ChatKeyRouter {
    public static func action(for key: ChatKey, in context: ChatKeyContext) -> ChatKeyAction {
        switch key {
        case .return:
            if context.answersApproval { return context.approval?.offers(.accept) == true ? .decide(.accept) : .none }
            return !context.composerIsEmpty && context.canSend ? .send : .none
        case .shiftReturn:
            if context.answersApproval { return context.approval?.offers(.acceptForSession) == true ? .decide(.acceptForSession) : .none }
            return .insertNewline
        case .escape, .commandDelete:
            guard context.answersApproval, let approval = context.approval else { return .none }
            if approval.offers(.decline) { return .decide(.decline) }
            return approval.offers(.cancel) ? .decide(.cancel) : .none
        case .commandPeriod:
            return context.busy ? .interrupt : .none
        }
    }
}

// MARK: - Answering questions

/// The state of the form for a `ChatQuestion`: for each question the options chosen and the free text typed. A single-choice question has
/// one answer, either an option or text; a multiple-choice question has any options and text besides.
public struct ChatAnswerForm: Sendable, Equatable {
    public let question: ChatQuestion
    public private(set) var chosen: [Set<Int>]
    public private(set) var text: [String]

    public init(question: ChatQuestion) {
        self.question = question
        chosen = Array(repeating: [], count: question.questions.count)
        text = Array(repeating: "", count: question.questions.count)
    }

    public func isChosen(prompt: Int, option: Int) -> Bool { chosen.indices.contains(prompt) && chosen[prompt].contains(option) }

    /// A tap on an option: a single-choice question takes it instead of what was chosen (and clears the text), a multiple-choice one flips it.
    public mutating func toggle(prompt: Int, option: Int) {
        guard question.questions.indices.contains(prompt), question.questions[prompt].options.indices.contains(option) else { return }
        if question.questions[prompt].multiSelect {
            if chosen[prompt].contains(option) { chosen[prompt].remove(option) } else { chosen[prompt].insert(option) }
        } else {
            chosen[prompt] = chosen[prompt] == [option] ? [] : [option]
            text[prompt] = ""
        }
    }
    /// Typing an answer: a single-choice question lets go of the option it had.
    public mutating func setText(prompt: Int, _ value: String) {
        guard text.indices.contains(prompt) else { return }
        text[prompt] = value
        if !question.questions[prompt].multiSelect, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { chosen[prompt] = [] }
    }

    private func typed(_ prompt: Int) -> String? {
        let trimmed = text[prompt].trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
    /// Every question has an option or some text.
    public var isComplete: Bool {
        !question.questions.isEmpty && question.questions.indices.allSatisfy { !chosen[$0].isEmpty || typed($0) != nil }
    }
    /// `Answer.answers`: one entry per question, in order: the labels chosen (in the order the options are listed), then the free text.
    public var answers: [[String]] {
        question.questions.indices.map { prompt in
            let labels = chosen[prompt].sorted().map { question.questions[prompt].options[$0].label }
            return labels + (typed(prompt).map { [$0] } ?? [])
        }
    }
}

// MARK: - Usage

/// The usage meter's words: how full the context is, what was spent, and Claude's cost as the estimate it is.
public struct ChatUsageMeter: Sendable, Equatable {
    public let usage: ChatUsage
    public init(_ usage: ChatUsage) { self.usage = usage }

    /// 0 to 1, when the window and what is in it are both known.
    public var contextFraction: Double? {
        guard let window = usage.contextWindow, window > 0, let used = usage.contextUsed else { return nil }
        return min(1, Double(used) / Double(window))
    }
    /// "42% of 200k" when the window is known, otherwise the tokens spent; nil when there is nothing to say.
    public var contextText: String? {
        if let fraction = contextFraction, let window = usage.contextWindow { return "\(Int((fraction * 100).rounded()))% of \(Self.tokens(window))" }
        let total = usage.inputTokens + usage.outputTokens
        return total > 0 ? "\(Self.tokens(total)) tokens" : nil
    }
    /// "42%", the number in the header's ring; nil when the window or what is in it is not known.
    public var percentText: String? { contextFraction.map { "\(Int(($0 * 100).rounded()))%" } }
    /// "84k / 200k tokens" when the window is known, otherwise the tokens spent ("13.3k tokens"); nil when there is nothing to say.
    public var tokensText: String? {
        if let window = usage.contextWindow, window > 0, let used = usage.contextUsed { return "\(Self.tokens(min(used, window))) / \(Self.tokens(window)) tokens" }
        let total = usage.inputTokens + usage.outputTokens
        return total > 0 ? "\(Self.tokens(total)) tokens" : nil
    }
    /// "≈ $0.42 (estimate)": the provider's own figure, never a bill.
    public var costText: String? {
        guard let cost = usage.costUSD, cost.isFinite, cost >= 0 else { return nil }
        return "≈ $\(String(format: cost >= 0.01 || cost == 0 ? "%.2f" : "%.3f", cost)) (estimate)"
    }
    public var text: String? {
        let parts = [contextText, costText].compactMap { $0 }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
    /// For VoiceOver, in words.
    public var spoken: String? {
        var parts: [String] = []
        if let fraction = contextFraction, let window = usage.contextWindow { parts.append("Context \(Int((fraction * 100).rounded())) percent full, of \(Self.tokens(window)) tokens") }
        else if let context = contextText { parts.append(context) }
        if let cost = costText { parts.append(cost.replacingOccurrences(of: "≈", with: "about")) }
        return parts.isEmpty ? nil : parts.joined(separator: ". ")
    }
    /// 999, 1.2k, 84k, 1.2M.
    public static func tokens(_ count: UInt64) -> String {
        func short(_ value: Double, _ unit: String) -> String {
            let rounded = (value * 10).rounded() / 10
            return (rounded >= 100 || rounded == rounded.rounded() ? String(Int(rounded.rounded())) : String(format: "%.1f", rounded)) + unit
        }
        switch count {
        case ..<1000: return String(count)
        // 999,999 is “1M”, not “1000k”.
        case ..<999_950: return short(Double(count) / 1000, "k")
        default: return short(Double(count) / 1_000_000, "M")
        }
    }
}

// MARK: - The tab strip

/// The chats of a project in the order of the tab strip: the newest first, as the terminals are.
public enum ChatTabs {
    public static func ordered(_ chats: [ChatInfo]) -> [ChatInfo] {
        chats.enumerated().sorted { ($0.element.createdAtUnix, $1.offset) > ($1.element.createdAtUnix, $0.offset) }.map(\.element)
    }
    /// The first line of a tab: the chat's own name, or what it is when it has none.
    public static func title(_ chat: ChatInfo) -> String {
        let named = chat.title.trimmingCharacters(in: .whitespacesAndNewlines)
        return named.isEmpty ? chat.provider.chatTitle : named
    }
    /// The second line of a tab: what it is (unless the first line already says), and where it runs, or its short id.
    public static func detail(_ chat: ChatInfo, branch: String?) -> String {
        let kind = title(chat) == chat.provider.chatTitle ? nil : chat.provider.chatTitle
        return [kind, branch ?? (kind == nil ? String(chat.id.prefix(8)) : nil)].compactMap { $0 }.joined(separator: " · ")
    }
}

// MARK: - Cards

/// Text for a card: command output and tool results can be enormous, and only the end of them is read.
public enum ChatOutput {
    /// The last `lines` lines of `text` (and no more than `maxCharacters` characters of them), and how many lines before them were left out.
    /// A trailing line break does not make an empty last line.
    public static func tail(_ text: String, lines: Int = 80, maxCharacters: Int = 20_000) -> (text: String, hidden: Int) {
        guard lines > 0, !text.isEmpty else { return ("", text.isEmpty ? 0 : Self.lineCount(text)) }
        let utf8 = text.utf8
        var end = utf8.endIndex
        if utf8.last == 0x0A { end = utf8.index(before: end) }
        var start = end, seen = 0
        while start > utf8.startIndex {
            let before = utf8.index(before: start)
            if utf8[before] == 0x0A { seen += 1; if seen == lines { break } }
            start = before
        }
        var body = String(text[start..<end])
        var hidden = start == utf8.startIndex ? 0 : Self.lineCount(String(text[utf8.startIndex..<start]))
        if body.count > maxCharacters {
            // Cut to the last whole line that fits, or in the middle of one if there is none.
            let cut = body.suffix(maxCharacters)
            let kept = cut.firstIndex(of: "\n").map { cut[cut.index(after: $0)...] } ?? cut
            hidden += body.split(separator: "\n", omittingEmptySubsequences: false).count - kept.split(separator: "\n", omittingEmptySubsequences: false).count
            body = String(kept)
        }
        return (body, hidden)
    }
    /// Lines in `text`: the number of line breaks, plus one for a last line that has no break after it.
    public static func lineCount(_ text: String) -> Int {
        guard !text.isEmpty else { return 0 }
        var count = 0
        for byte in text.utf8 where byte == 0x0A { count += 1 }
        return text.utf8.last == 0x0A ? count : count + 1
    }
}

/// What a tool card says about a call, from its JSON input.
public enum ChatToolSummary {
    /// The inputs that say what a call is about, in the order they are looked for.
    static let preferredKeys = ["command", "query", "path", "file_path", "url", "pattern", "description", "prompt", "text"]
    /// One line: the string that best says what the tool was asked to do, else the input as compact JSON, cut at `limit` characters.
    public static func line(for input: JSONValue, limit: Int = 100) -> String {
        var text = ""
        switch input {
        case .null: return ""
        case .string(let value): text = value
        case .object(let fields):
            if let key = preferredKeys.first(where: { fields[$0]?.string?.isEmpty == false }), let value = fields[key]?.string { text = value }
            else if let first = fields.keys.sorted().compactMap({ fields[$0]?.string }).first(where: { !$0.isEmpty }) { text = first }
            else { text = compact(input) }
        default: text = compact(input)
        }
        let flat = text.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }.joined(separator: " ")
        return flat.count > limit ? String(flat.prefix(limit - 1)) + "…" : flat
    }
    /// The input laid out for reading, bounded.
    public static func pretty(_ input: JSONValue, limit: Int = 4000) -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(input), let text = String(data: data, encoding: .utf8) else { return "" }
        return text.count > limit ? String(text.prefix(limit)) + "\n…" : text
    }
    private static func compact(_ input: JSONValue) -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return (try? encoder.encode(input)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
    }
}

extension ChatItemBody {
    /// A line for VoiceOver and for a row that is collapsed: what the item is.
    public var summary: String {
        switch self {
        case .userMessage(let text): "You: \(text)"
        case .agentMessage(let text): text
        case .reasoning: "Thinking"
        case .plan(_, let steps): "Plan, \(steps.filter { $0.status == .completed }.count) of \(steps.count) steps done"
        case .command(let command, _, _, let exitCode): "Command: \(command)" + (exitCode.map { $0 == 0 ? "" : ", exit \($0)" } ?? "")
        case .fileChange(let changes): changes.count == 1 ? "Edit: \(changes[0].path)" : "Edit: \(changes.count) files"
        case .toolCall(let server, let tool, _, _): "Tool: " + [server, tool].compactMap { $0 }.joined(separator: " ")
        case .webSearch(let query): "Web search: \(query)"
        case .todo(let items): "To-do, \(items.filter { $0.status == .completed }.count) of \(items.count) done"
        case .compaction: "Context compacted"
        case .notice(_, let text): text
        }
    }
}

extension ChatItemStatus {
    /// Spoken, for the cards that have one.
    public var spoken: String {
        switch self {
        case .inProgress: "running"
        case .completed: "done"
        case .failed: "failed"
        case .declined: "declined"
        case .interrupted: "interrupted"
        }
    }
}
