import Foundation

// The chat vocabulary of the desktop (`src/chat/model.rs`) as the phone reads and writes it. The JSON is the serde form of those Rust
// types, so the names and tags here are part of the wire: `ChatEvent` is tagged `event`, `ChatItemBody` `type`, `ChatState` `state`,
// `ChatDelta` `kind` (with its text under `text`), `ChatTurnOutcome` `outcome` and `ChatCommand` `command`; every enum word is snake_case.
//
// Reading is lenient, as it is for every optional field the desktop adds (`AgentActivity.swift`). A desktop newer than the phone may
// send an event, an item or a delta the phone has never heard of: that one is skipped (`ChatEventsReply`), never the batch it came in.
// A word the phone does not know inside a thing it does know (a new approval mode, a new step status) is read as its nearest harmless
// meaning instead, because dropping the event would drop what it carries (an approval that was answered, a state that changed).

/// Skips what it cannot read: an array of these keeps its good elements.
struct Lenient<Value: Decodable>: Decodable {
    let value: Value?
    init(from decoder: any Decoder) throws { value = try? Value(from: decoder) }
}

extension KeyedDecodingContainer {
    /// The value under `key`, or nil when it is missing, null or of another type than the phone expects.
    func tolerant<Value: Decodable>(_ type: Value.Type, forKey key: Key) -> Value? { try? decodeIfPresent(type, forKey: key) }
    /// A snake_case word of an enum, or `fallback` when it is missing, not a string or not one the phone knows.
    func lenient<Word: RawRepresentable>(_ type: Word.Type, forKey key: Key, default fallback: Word) -> Word where Word.RawValue == String {
        tolerant(String.self, forKey: key).flatMap(Word.init(rawValue:)) ?? fallback
    }
    /// The readable elements of an array that may hold some the phone cannot read; missing or not an array is empty.
    func leniently<Element: Decodable>(_ type: [Element].Type, forKey key: Key) -> [Element] {
        (tolerant([Lenient<Element>].self, forKey: key) ?? []).compactMap(\.value)
    }
}

// MARK: - Chats

public enum ChatProvider: String, Codable, Sendable, CaseIterable, Hashable {
    case codex, claude
    public var title: String { self == .codex ? "Codex" : "Claude" }
    /// "Codex chat", "Claude chat": what a tab or a row calls it. Never "Claude Code".
    public var chatTitle: String { title + " chat" }
}

/// How much the agent may do without asking (`ApprovalMode`).
public enum ChatApprovalMode: String, Codable, Sendable, CaseIterable, Hashable, Identifiable {
    case supervised, autoEdit = "auto_edit", full, plan
    public var id: String { rawValue }
    public var title: String {
        switch self {
        case .supervised: "Supervised"
        case .autoEdit: "Auto-edit"
        case .full: "Full"
        case .plan: "Plan"
        }
    }
    public var detail: String {
        switch self {
        case .supervised: "Asks before commands and edits"
        case .autoEdit: "Edits files freely, asks for the rest"
        case .full: "Never asks"
        case .plan: "Plans first, changes nothing"
        }
    }
}

/// What a chat is doing as a whole (`ChatState`, tagged `state`).
public enum ChatState: Sendable, Equatable, Hashable, Codable {
    case starting, idle, running, waiting, stopped
    case failed(String)
    /// A state a newer desktop invented. Drawn as nothing in particular.
    case unknown(String)

    private enum Keys: String, CodingKey { case state, message }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let word = try c.decode(String.self, forKey: .state)
        switch word {
        case "starting": self = .starting
        case "idle": self = .idle
        case "running": self = .running
        case "waiting": self = .waiting
        case "stopped": self = .stopped
        case "failed": self = .failed(c.tolerant(String.self, forKey: .message) ?? "")
        default: self = .unknown(word)
        }
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case .starting: try c.encode("starting", forKey: .state)
        case .idle: try c.encode("idle", forKey: .state)
        case .running: try c.encode("running", forKey: .state)
        case .waiting: try c.encode("waiting", forKey: .state)
        case .stopped: try c.encode("stopped", forKey: .state)
        case .failed(let message): try c.encode("failed", forKey: .state); try c.encode(message, forKey: .message)
        case .unknown(let word): try c.encode(word, forKey: .state)
        }
    }
    /// A turn is in progress or waits for the person: the composer offers Interrupt.
    public var isBusy: Bool { self == .running || self == .waiting }
}

/// One chat, as the host knows it (`ChatInfo`).
public struct ChatInfo: Codable, Sendable, Equatable, Hashable, Identifiable {
    public var id: String
    public var provider: ChatProvider
    public var projectID: String?
    public var worktreeID: String?
    public var cwd: String
    public var title: String
    public var createdAtUnix: UInt64
    public var providerThreadID: String?
    public var model: String?
    public var effort: String?
    /// Fast mode was asked for (Codex's fast service tier, Claude's `fastMode`): the person's choice, not what the provider granted. A
    /// desktop that predates it never says, which reads as off.
    public var fast: Bool
    public var approvalMode: ChatApprovalMode
    public var codexAccountID: String?
    public var state: ChatState

    public init(id: String, provider: ChatProvider, projectID: String? = nil, worktreeID: String? = nil, cwd: String = "", title: String = "",
                createdAtUnix: UInt64 = 0, providerThreadID: String? = nil, model: String? = nil, effort: String? = nil, fast: Bool = false,
                approvalMode: ChatApprovalMode = .supervised, codexAccountID: String? = nil, state: ChatState = .starting) {
        self.id = id; self.provider = provider; self.projectID = projectID; self.worktreeID = worktreeID; self.cwd = cwd; self.title = title
        self.createdAtUnix = createdAtUnix; self.providerThreadID = providerThreadID; self.model = model; self.effort = effort; self.fast = fast
        self.approvalMode = approvalMode; self.codexAccountID = codexAccountID; self.state = state
    }

    private enum Keys: String, CodingKey {
        case id, provider, cwd, title, model, effort, fast, state
        case projectID = "project_id", worktreeID = "worktree_id", createdAtUnix = "created_at_unix", providerThreadID = "provider_thread_id"
        case approvalMode = "approval_mode", codexAccountID = "codex_account_id"
    }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        id = try c.decode(String.self, forKey: .id)
        provider = try c.decode(ChatProvider.self, forKey: .provider)
        projectID = try c.decodeIfPresent(String.self, forKey: .projectID)
        worktreeID = try c.decodeIfPresent(String.self, forKey: .worktreeID)
        cwd = try c.decodeIfPresent(String.self, forKey: .cwd) ?? ""
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        createdAtUnix = try c.decodeIfPresent(UInt64.self, forKey: .createdAtUnix) ?? 0
        providerThreadID = try c.decodeIfPresent(String.self, forKey: .providerThreadID)
        model = try c.decodeIfPresent(String.self, forKey: .model)
        effort = try c.decodeIfPresent(String.self, forKey: .effort)
        fast = c.tolerant(Bool.self, forKey: .fast) ?? false
        approvalMode = c.lenient(ChatApprovalMode.self, forKey: .approvalMode, default: .supervised)
        codexAccountID = try c.decodeIfPresent(String.self, forKey: .codexAccountID)
        state = c.tolerant(ChatState.self, forKey: .state) ?? .starting
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(id, forKey: .id)
        try c.encode(provider, forKey: .provider)
        try c.encodeIfPresent(projectID, forKey: .projectID)
        try c.encodeIfPresent(worktreeID, forKey: .worktreeID)
        try c.encode(cwd, forKey: .cwd)
        try c.encode(title, forKey: .title)
        try c.encode(createdAtUnix, forKey: .createdAtUnix)
        try c.encodeIfPresent(providerThreadID, forKey: .providerThreadID)
        try c.encodeIfPresent(model, forKey: .model)
        try c.encodeIfPresent(effort, forKey: .effort)
        try c.encode(fast, forKey: .fast)
        try c.encode(approvalMode, forKey: .approvalMode)
        try c.encodeIfPresent(codexAccountID, forKey: .codexAccountID)
        try c.encode(state, forKey: .state)
    }
}

/// A model the provider offers (`ModelOption`), as its driver found it out (Codex `model/list`, Claude's `initialize` reply). The chat's
/// `model` is the `id`. Only the id and the name are required; the rest is read leniently (a missing or odd field is its default).
public struct ChatModelOption: Codable, Sendable, Equatable, Hashable, Identifiable {
    /// What `model` takes: Codex's model id, Claude's alias or model name (the default model's is `default`).
    public var id: String
    /// The name to show.
    public var name: String
    public var description: String
    /// The reasoning efforts the model takes, in the order to offer them. Empty when there is none to choose.
    public var efforts: [String]
    /// The effort the provider uses when none is chosen.
    public var defaultEffort: String?
    /// The model has a fast mode.
    public var supportsFast: Bool
    /// The provider uses this model when none is chosen.
    public var isDefault: Bool

    public init(id: String, name: String? = nil, description: String = "", efforts: [String] = [], defaultEffort: String? = nil, supportsFast: Bool = false, isDefault: Bool = false) {
        self.id = id; self.name = name ?? id; self.description = description; self.efforts = efforts; self.defaultEffort = defaultEffort
        self.supportsFast = supportsFast; self.isDefault = isDefault
    }

    private enum Keys: String, CodingKey {
        case id, name, description, efforts
        case defaultEffort = "default_effort", supportsFast = "supports_fast", isDefault = "is_default"
    }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let id = try c.decode(String.self, forKey: .id)
        // A model with no id cannot be chosen: it is not one.
        guard !id.isEmpty else { throw DecodingError.dataCorruptedError(forKey: .id, in: c, debugDescription: "A model has an id") }
        self.id = id
        let name = c.tolerant(String.self, forKey: .name) ?? ""
        self.name = name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? id : name
        description = c.tolerant(String.self, forKey: .description) ?? ""
        efforts = c.leniently([String].self, forKey: .efforts)
        defaultEffort = c.tolerant(String.self, forKey: .defaultEffort)
        supportsFast = c.tolerant(Bool.self, forKey: .supportsFast) ?? false
        isDefault = c.tolerant(Bool.self, forKey: .isDefault) ?? false
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(id, forKey: .id)
        try c.encode(name, forKey: .name)
        try c.encode(description, forKey: .description)
        try c.encode(efforts, forKey: .efforts)
        try c.encode(defaultEffort, forKey: .defaultEffort)
        try c.encode(supportsFast, forKey: .supportsFast)
        try c.encode(isDefault, forKey: .isDefault)
    }
}

// MARK: - Items

public enum ChatItemStatus: String, Codable, Sendable, Hashable {
    case inProgress = "in_progress", completed, failed
    /// Refused at an approval prompt.
    case declined, interrupted
    /// Done or not, an item the phone cannot name is not one that spins forever.
    public var isFinished: Bool { self != .inProgress }
}

public enum ChatChangeKind: String, Codable, Sendable, Hashable { case add, modify, delete, rename }

public struct ChatFileChange: Codable, Sendable, Equatable, Hashable {
    public var path: String
    public var kind: ChatChangeKind
    /// A unified diff when the provider gives one.
    public var diff: String?
    public init(path: String, kind: ChatChangeKind, diff: String? = nil) { self.path = path; self.kind = kind; self.diff = diff }
    private enum Keys: String, CodingKey { case path, kind, diff }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        path = try c.decode(String.self, forKey: .path)
        kind = c.lenient(ChatChangeKind.self, forKey: .kind, default: .modify)
        diff = try c.decodeIfPresent(String.self, forKey: .diff)
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(path, forKey: .path)
        try c.encode(kind, forKey: .kind)
        try c.encodeIfPresent(diff, forKey: .diff)
    }
}

public enum ChatStepStatus: String, Codable, Sendable, Hashable { case pending, inProgress = "in_progress", completed }

/// One line of a plan or a to-do list.
public struct ChatStep: Codable, Sendable, Equatable, Hashable {
    public var text: String
    public var status: ChatStepStatus
    public init(text: String, status: ChatStepStatus = .pending) { self.text = text; self.status = status }
    private enum Keys: String, CodingKey { case text, status }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        text = try c.decode(String.self, forKey: .text)
        status = c.lenient(ChatStepStatus.self, forKey: .status, default: .pending)
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(text, forKey: .text)
        try c.encode(status, forKey: .status)
    }
}

public enum ChatNoticeLevel: String, Codable, Sendable, Hashable { case info, warning, error }

/// The content of one transcript item (`ItemBody`, tagged `type`).
public enum ChatItemBody: Sendable, Equatable, Codable {
    case userMessage(String)
    /// Markdown.
    case agentMessage(String)
    case reasoning(String)
    case plan(explanation: String?, steps: [ChatStep])
    case command(command: String, cwd: String?, output: String, exitCode: Int?)
    case fileChange([ChatFileChange])
    case toolCall(server: String?, tool: String, input: JSONValue, output: String?)
    case webSearch(String)
    case todo([ChatStep])
    case compaction
    /// `kind` is the host's dedupe key ("rate_limit:seven_day", "api_retry", "reconnecting", …): notices of one kind are one line of
    /// the banner row. Older hosts send none.
    case notice(level: ChatNoticeLevel, text: String, kind: String? = nil)

    private enum Keys: String, CodingKey {
        case type, text, explanation, steps, command, cwd, output, changes, server, tool, input, query, items, level, kind
        case exitCode = "exit_code"
    }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let type = try c.decode(String.self, forKey: .type)
        switch type {
        case "user_message": self = .userMessage(try c.decode(String.self, forKey: .text))
        case "agent_message": self = .agentMessage(try c.decode(String.self, forKey: .text))
        case "reasoning": self = .reasoning(try c.decode(String.self, forKey: .text))
        case "plan": self = .plan(explanation: try c.decodeIfPresent(String.self, forKey: .explanation), steps: c.leniently([ChatStep].self, forKey: .steps))
        case "command":
            self = .command(command: try c.decode(String.self, forKey: .command), cwd: try c.decodeIfPresent(String.self, forKey: .cwd),
                            output: try c.decodeIfPresent(String.self, forKey: .output) ?? "", exitCode: c.tolerant(Int.self, forKey: .exitCode))
        case "file_change": self = .fileChange(c.leniently([ChatFileChange].self, forKey: .changes))
        case "tool_call":
            self = .toolCall(server: try c.decodeIfPresent(String.self, forKey: .server), tool: try c.decode(String.self, forKey: .tool),
                             input: c.tolerant(JSONValue.self, forKey: .input) ?? .null, output: try c.decodeIfPresent(String.self, forKey: .output))
        case "web_search": self = .webSearch(try c.decode(String.self, forKey: .query))
        case "todo": self = .todo(c.leniently([ChatStep].self, forKey: .items))
        case "compaction": self = .compaction
        case "notice":
            // A kind that is not a non-empty string is no kind (the notice is then its own line, by its text).
            let kind = (try? c.decodeIfPresent(String.self, forKey: .kind))?.flatMap { $0.trimmingCharacters(in: .whitespaces).isEmpty ? nil : $0 }
            self = .notice(level: c.lenient(ChatNoticeLevel.self, forKey: .level, default: .info), text: try c.decode(String.self, forKey: .text), kind: kind)
        default: throw DecodingError.dataCorruptedError(forKey: .type, in: c, debugDescription: "Unknown item type \(type)")
        }
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case .userMessage(let text): try c.encode("user_message", forKey: .type); try c.encode(text, forKey: .text)
        case .agentMessage(let text): try c.encode("agent_message", forKey: .type); try c.encode(text, forKey: .text)
        case .reasoning(let text): try c.encode("reasoning", forKey: .type); try c.encode(text, forKey: .text)
        case .plan(let explanation, let steps):
            try c.encode("plan", forKey: .type); try c.encode(explanation, forKey: .explanation); try c.encode(steps, forKey: .steps)
        case .command(let command, let cwd, let output, let exitCode):
            try c.encode("command", forKey: .type); try c.encode(command, forKey: .command); try c.encodeIfPresent(cwd, forKey: .cwd)
            try c.encode(output, forKey: .output); try c.encodeIfPresent(exitCode, forKey: .exitCode)
        case .fileChange(let changes): try c.encode("file_change", forKey: .type); try c.encode(changes, forKey: .changes)
        case .toolCall(let server, let tool, let input, let output):
            try c.encode("tool_call", forKey: .type); try c.encodeIfPresent(server, forKey: .server); try c.encode(tool, forKey: .tool)
            try c.encode(input, forKey: .input); try c.encodeIfPresent(output, forKey: .output)
        case .webSearch(let query): try c.encode("web_search", forKey: .type); try c.encode(query, forKey: .query)
        case .todo(let items): try c.encode("todo", forKey: .type); try c.encode(items, forKey: .items)
        case .compaction: try c.encode("compaction", forKey: .type)
        case .notice(let level, let text, let kind):
            try c.encode("notice", forKey: .type); try c.encode(level, forKey: .level); try c.encode(text, forKey: .text); try c.encodeIfPresent(kind, forKey: .kind)
        }
    }
}

/// One entry of the transcript (`Item`).
public struct ChatItem: Codable, Sendable, Equatable, Identifiable {
    /// Unique within the chat.
    public var id: String
    public var turnID: String?
    public var status: ChatItemStatus
    public var body: ChatItemBody
    public init(id: String, turnID: String? = nil, status: ChatItemStatus = .inProgress, body: ChatItemBody) {
        self.id = id; self.turnID = turnID; self.status = status; self.body = body
    }
    private enum Keys: String, CodingKey { case id, status, body; case turnID = "turn_id" }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        id = try c.decode(String.self, forKey: .id)
        turnID = try c.decodeIfPresent(String.self, forKey: .turnID)
        status = c.lenient(ChatItemStatus.self, forKey: .status, default: .completed)
        body = try c.decode(ChatItemBody.self, forKey: .body)
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(id, forKey: .id)
        try c.encodeIfPresent(turnID, forKey: .turnID)
        try c.encode(status, forKey: .status)
        try c.encode(body, forKey: .body)
    }
}

/// Appended to an item while it streams (`Delta`, tagged `kind` with its text under `text`).
public enum ChatDelta: Sendable, Equatable, Hashable, Codable {
    /// More of an agent message's or reasoning item's text.
    case text(String)
    /// More of a command's output.
    case output(String)
    private enum Keys: String, CodingKey { case kind, text }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let kind = try c.decode(String.self, forKey: .kind)
        switch kind {
        case "text": self = .text(try c.decode(String.self, forKey: .text))
        case "output": self = .output(try c.decode(String.self, forKey: .text))
        default: throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "Unknown delta kind \(kind)")
        }
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case .text(let text): try c.encode("text", forKey: .kind); try c.encode(text, forKey: .text)
        case .output(let text): try c.encode("output", forKey: .kind); try c.encode(text, forKey: .text)
        }
    }
}

// MARK: - Approvals, questions, usage

public enum ChatDecision: String, Codable, Sendable, Hashable, CaseIterable {
    case accept
    /// Accept this and the same kind of request for the rest of the session.
    case acceptForSession = "accept_for_session"
    case decline
    /// Decline and stop the turn.
    case cancel
}

public enum ChatApprovalKind: String, Codable, Sendable, Hashable { case command, fileChange = "file_change", permissions, tool }

/// A turn waits for the user to allow something (`Approval`).
public struct ChatApproval: Codable, Sendable, Equatable, Hashable, Identifiable {
    public var requestID: String
    /// The transcript item the request is about, when there is one.
    public var itemID: String?
    public var kind: ChatApprovalKind
    /// One line: the command, the file, the tool.
    public var title: String
    /// More detail: the reason, the input, a diff.
    public var detail: String
    /// The decisions the provider offers, in its order.
    public var choices: [ChatDecision]
    public var id: String { requestID }
    public init(requestID: String, itemID: String? = nil, kind: ChatApprovalKind, title: String, detail: String = "", choices: [ChatDecision]) {
        self.requestID = requestID; self.itemID = itemID; self.kind = kind; self.title = title; self.detail = detail; self.choices = choices
    }
    private enum Keys: String, CodingKey {
        case kind, title, detail, choices
        case requestID = "request_id", itemID = "item_id"
    }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        requestID = try c.decode(String.self, forKey: .requestID)
        itemID = try c.decodeIfPresent(String.self, forKey: .itemID)
        kind = c.lenient(ChatApprovalKind.self, forKey: .kind, default: .tool)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        detail = try c.decodeIfPresent(String.self, forKey: .detail) ?? ""
        choices = c.leniently([ChatDecision].self, forKey: .choices)
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(requestID, forKey: .requestID)
        try c.encodeIfPresent(itemID, forKey: .itemID)
        try c.encode(kind, forKey: .kind)
        try c.encode(title, forKey: .title)
        try c.encode(detail, forKey: .detail)
        try c.encode(choices, forKey: .choices)
    }
    /// What can be pressed: the provider's own list, or Allow and Deny when it sent none the phone can read.
    public var offered: [ChatDecision] { choices.isEmpty ? [.accept, .decline] : choices }
    public func offers(_ decision: ChatDecision) -> Bool { offered.contains(decision) }
}

public struct ChatQuestionOption: Codable, Sendable, Equatable, Hashable {
    public var label: String
    public var description: String
    public init(label: String, description: String = "") { self.label = label; self.description = description }
    private enum Keys: String, CodingKey { case label, description }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        label = try c.decode(String.self, forKey: .label)
        description = try c.decodeIfPresent(String.self, forKey: .description) ?? ""
    }
}

public struct ChatQuestionPrompt: Codable, Sendable, Equatable, Hashable {
    public var header: String?
    public var question: String
    public var options: [ChatQuestionOption]
    public var multiSelect: Bool
    public init(header: String? = nil, question: String, options: [ChatQuestionOption] = [], multiSelect: Bool = false) {
        self.header = header; self.question = question; self.options = options; self.multiSelect = multiSelect
    }
    private enum Keys: String, CodingKey { case header, question, options; case multiSelect = "multi_select" }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        header = try c.decodeIfPresent(String.self, forKey: .header)
        question = try c.decode(String.self, forKey: .question)
        options = c.leniently([ChatQuestionOption].self, forKey: .options)
        multiSelect = try c.decodeIfPresent(Bool.self, forKey: .multiSelect) ?? false
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encodeIfPresent(header, forKey: .header)
        try c.encode(question, forKey: .question)
        try c.encode(options, forKey: .options)
        try c.encode(multiSelect, forKey: .multiSelect)
    }
}

/// A turn waits for the user to answer questions (`Question`).
public struct ChatQuestion: Codable, Sendable, Equatable, Hashable, Identifiable {
    public var requestID: String
    public var questions: [ChatQuestionPrompt]
    public var id: String { requestID }
    public init(requestID: String, questions: [ChatQuestionPrompt]) { self.requestID = requestID; self.questions = questions }
    private enum Keys: String, CodingKey { case questions; case requestID = "request_id" }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        requestID = try c.decode(String.self, forKey: .requestID)
        questions = c.leniently([ChatQuestionPrompt].self, forKey: .questions)
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(requestID, forKey: .requestID)
        try c.encode(questions, forKey: .questions)
    }
}

public struct ChatUsage: Codable, Sendable, Equatable, Hashable {
    public var inputTokens: UInt64
    public var outputTokens: UInt64
    public var cachedInputTokens: UInt64
    /// The model's context window, when known.
    public var contextWindow: UInt64?
    /// Tokens of the window in use now, when known.
    public var contextUsed: UInt64?
    /// The provider's own estimate (Claude's `total_cost_usd`), never a bill.
    public var costUSD: Double?
    public init(inputTokens: UInt64 = 0, outputTokens: UInt64 = 0, cachedInputTokens: UInt64 = 0, contextWindow: UInt64? = nil, contextUsed: UInt64? = nil, costUSD: Double? = nil) {
        self.inputTokens = inputTokens; self.outputTokens = outputTokens; self.cachedInputTokens = cachedInputTokens
        self.contextWindow = contextWindow; self.contextUsed = contextUsed; self.costUSD = costUSD
    }
    private enum Keys: String, CodingKey {
        case inputTokens = "input_tokens", outputTokens = "output_tokens", cachedInputTokens = "cached_input_tokens"
        case contextWindow = "context_window", contextUsed = "context_used", costUSD = "cost_usd"
    }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        inputTokens = c.tolerant(UInt64.self, forKey: .inputTokens) ?? 0
        outputTokens = c.tolerant(UInt64.self, forKey: .outputTokens) ?? 0
        cachedInputTokens = c.tolerant(UInt64.self, forKey: .cachedInputTokens) ?? 0
        contextWindow = c.tolerant(UInt64.self, forKey: .contextWindow)
        contextUsed = c.tolerant(UInt64.self, forKey: .contextUsed)
        costUSD = c.tolerant(Double.self, forKey: .costUSD)
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(inputTokens, forKey: .inputTokens)
        try c.encode(outputTokens, forKey: .outputTokens)
        try c.encode(cachedInputTokens, forKey: .cachedInputTokens)
        try c.encodeIfPresent(contextWindow, forKey: .contextWindow)
        try c.encodeIfPresent(contextUsed, forKey: .contextUsed)
        try c.encodeIfPresent(costUSD, forKey: .costUSD)
    }
}

/// How a turn ended (`TurnOutcome`, tagged `outcome`).
public enum ChatTurnOutcome: Sendable, Equatable, Hashable, Codable {
    case completed, interrupted
    case failed(String)
    private enum Keys: String, CodingKey { case outcome, message }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .outcome) {
        case "interrupted": self = .interrupted
        case "failed": self = .failed(c.tolerant(String.self, forKey: .message) ?? "")
        // A turn that ended in a way the phone has no word for still ended: its open items close as completed.
        default: self = .completed
        }
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case .completed: try c.encode("completed", forKey: .outcome)
        case .interrupted: try c.encode("interrupted", forKey: .outcome)
        case .failed(let message): try c.encode("failed", forKey: .outcome); try c.encode(message, forKey: .message)
        }
    }
}

// MARK: - Events

/// Everything that happens in a chat, in order (`ChatEvent`, tagged `event`).
public enum ChatEvent: Sendable, Equatable, Codable {
    /// The chat's metadata changed (thread id learned, model, effort, Fast or mode changed).
    case info(ChatInfo)
    case state(ChatState)
    case turnStarted(turnID: String)
    case turnCompleted(turnID: String, outcome: ChatTurnOutcome)
    case itemStarted(ChatItem)
    case itemDelta(itemID: String, delta: ChatDelta)
    /// The item's final form; replaces what deltas built.
    case itemCompleted(ChatItem)
    case approvalRequested(ChatApproval)
    case approvalResolved(requestID: String, decision: ChatDecision)
    case questionRequested(ChatQuestion)
    case questionResolved(requestID: String)
    case usage(ChatUsage)
    /// The models the provider offers: sent once after the handshake and again if the list changes; each replaces the last.
    case models([ChatModelOption])

    private enum Keys: String, CodingKey {
        case event, info, state, outcome, item, delta, approval, decision, question, usage, models
        case turnID = "turn_id", itemID = "item_id", requestID = "request_id"
    }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let word = try c.decode(String.self, forKey: .event)
        switch word {
        case "info": self = .info(try c.decode(ChatInfo.self, forKey: .info))
        case "state": self = .state(try c.decode(ChatState.self, forKey: .state))
        case "turn_started": self = .turnStarted(turnID: try c.decode(String.self, forKey: .turnID))
        case "turn_completed":
            self = .turnCompleted(turnID: try c.decode(String.self, forKey: .turnID), outcome: try c.decode(ChatTurnOutcome.self, forKey: .outcome))
        case "item_started": self = .itemStarted(try c.decode(ChatItem.self, forKey: .item))
        case "item_delta": self = .itemDelta(itemID: try c.decode(String.self, forKey: .itemID), delta: try c.decode(ChatDelta.self, forKey: .delta))
        case "item_completed": self = .itemCompleted(try c.decode(ChatItem.self, forKey: .item))
        case "approval_requested": self = .approvalRequested(try c.decode(ChatApproval.self, forKey: .approval))
        // Resolved is what takes a request off the screen, so a decision the phone has no word for must not lose the event.
        case "approval_resolved": self = .approvalResolved(requestID: try c.decode(String.self, forKey: .requestID), decision: c.lenient(ChatDecision.self, forKey: .decision, default: .decline))
        case "question_requested": self = .questionRequested(try c.decode(ChatQuestion.self, forKey: .question))
        case "question_resolved": self = .questionResolved(requestID: try c.decode(String.self, forKey: .requestID))
        case "usage": self = .usage(try c.decode(ChatUsage.self, forKey: .usage))
        // A list that is no array is no list: the event is skipped and the models held stay. A model the phone cannot read is left out.
        case "models": self = .models(try c.decode([Lenient<ChatModelOption>].self, forKey: .models).compactMap(\.value))
        default: throw DecodingError.dataCorruptedError(forKey: .event, in: c, debugDescription: "Unknown event \(word)")
        }
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case .info(let info): try c.encode("info", forKey: .event); try c.encode(info, forKey: .info)
        case .state(let state): try c.encode("state", forKey: .event); try c.encode(state, forKey: .state)
        case .turnStarted(let turnID): try c.encode("turn_started", forKey: .event); try c.encode(turnID, forKey: .turnID)
        case .turnCompleted(let turnID, let outcome):
            try c.encode("turn_completed", forKey: .event); try c.encode(turnID, forKey: .turnID); try c.encode(outcome, forKey: .outcome)
        case .itemStarted(let item): try c.encode("item_started", forKey: .event); try c.encode(item, forKey: .item)
        case .itemDelta(let itemID, let delta): try c.encode("item_delta", forKey: .event); try c.encode(itemID, forKey: .itemID); try c.encode(delta, forKey: .delta)
        case .itemCompleted(let item): try c.encode("item_completed", forKey: .event); try c.encode(item, forKey: .item)
        case .approvalRequested(let approval): try c.encode("approval_requested", forKey: .event); try c.encode(approval, forKey: .approval)
        case .approvalResolved(let requestID, let decision):
            try c.encode("approval_resolved", forKey: .event); try c.encode(requestID, forKey: .requestID); try c.encode(decision, forKey: .decision)
        case .questionRequested(let question): try c.encode("question_requested", forKey: .event); try c.encode(question, forKey: .question)
        case .questionResolved(let requestID): try c.encode("question_resolved", forKey: .event); try c.encode(requestID, forKey: .requestID)
        case .usage(let usage): try c.encode("usage", forKey: .event); try c.encode(usage, forKey: .usage)
        case .models(let models): try c.encode("models", forKey: .event); try c.encode(models, forKey: .models)
        }
    }
}

// MARK: - Commands

/// What the person asks of a chat (`ChatCommand`, tagged `command`). The only thing here the phone sends.
public enum ChatCommand: Sendable, Equatable, Hashable, Codable {
    /// Start a turn, or steer the running one where the provider allows it. On a stopped chat it resumes it.
    case send(text: String)
    case interrupt
    case approve(requestID: String, decision: ChatDecision)
    /// One entry per question, in order: the chosen labels, or free text.
    case answer(requestID: String, answers: [[String]])
    /// Each field is a change; one left out stays as it is. `fast` is `false` to turn it off, `nil` to leave it.
    case configure(model: String? = nil, effort: String? = nil, approvalMode: ChatApprovalMode? = nil, fast: Bool? = nil)
    case compact
    /// Stops the provider process; the chat resumes with the next message.
    case stop

    private enum Keys: String, CodingKey {
        case command, text, decision, answers, model, effort, fast
        case requestID = "request_id", approvalMode = "approval_mode"
    }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let word = try c.decode(String.self, forKey: .command)
        switch word {
        case "send": self = .send(text: try c.decode(String.self, forKey: .text))
        case "interrupt": self = .interrupt
        case "approve": self = .approve(requestID: try c.decode(String.self, forKey: .requestID), decision: try c.decode(ChatDecision.self, forKey: .decision))
        case "answer": self = .answer(requestID: try c.decode(String.self, forKey: .requestID), answers: try c.decode([[String]].self, forKey: .answers))
        case "configure":
            self = .configure(model: try c.decodeIfPresent(String.self, forKey: .model), effort: try c.decodeIfPresent(String.self, forKey: .effort),
                              approvalMode: try c.decodeIfPresent(ChatApprovalMode.self, forKey: .approvalMode), fast: try c.decodeIfPresent(Bool.self, forKey: .fast))
        case "compact": self = .compact
        case "stop": self = .stop
        default: throw DecodingError.dataCorruptedError(forKey: .command, in: c, debugDescription: "Unknown command \(word)")
        }
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case .send(let text): try c.encode("send", forKey: .command); try c.encode(text, forKey: .text)
        case .interrupt: try c.encode("interrupt", forKey: .command)
        case .approve(let requestID, let decision): try c.encode("approve", forKey: .command); try c.encode(requestID, forKey: .requestID); try c.encode(decision, forKey: .decision)
        case .answer(let requestID, let answers): try c.encode("answer", forKey: .command); try c.encode(requestID, forKey: .requestID); try c.encode(answers, forKey: .answers)
        case .configure(let model, let effort, let approvalMode, let fast):
            try c.encode("configure", forKey: .command)
            try c.encodeIfPresent(model, forKey: .model); try c.encodeIfPresent(effort, forKey: .effort); try c.encodeIfPresent(approvalMode, forKey: .approvalMode)
            try c.encodeIfPresent(fast, forKey: .fast)
        case .compact: try c.encode("compact", forKey: .command)
        case .stop: try c.encode("stop", forKey: .command)
        }
    }
}
