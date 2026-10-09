import Foundation

// The model choice of a new chat. The phone has no list of models before a chat exists (the list comes from the chat's own provider, in its
// first events), so a new chat offers what it can know: the provider's default, and the model last used with that provider, with the effort
// and Fast it was used with. The choice is remembered per provider, so the next chat starts as the last one did.

/// What a new Codex or Claude chat is started with: the provider's default model, or the last model used with it.
public struct NewChatChoice: Codable, Sendable, Equatable {
    /// The last model chosen, as the phone knew it then (its efforts and whether it has Fast): the second row. Nil until one was.
    public var model: ChatModelOption?
    /// The second row is chosen, not the default.
    public var usesModel: Bool
    /// The effort chosen for `model`; nil is the model's own default.
    public var effort: String?
    /// Fast was chosen for `model`.
    public var fast: Bool

    public init(model: ChatModelOption? = nil, usesModel: Bool = false, effort: String? = nil, fast: Bool = false) {
        self.model = model; self.usesModel = usesModel && model != nil; self.effort = effort; self.fast = fast
    }
    private enum Keys: String, CodingKey { case model, usesModel = "uses_model", effort, fast }
    /// Read leniently: what was stored by another version, or damaged, is the default rather than an error.
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let model = c.tolerant(ChatModelOption.self, forKey: .model)
        self.init(model: model, usesModel: c.tolerant(Bool.self, forKey: .usesModel) ?? false, effort: c.tolerant(String.self, forKey: .effort), fast: c.tolerant(Bool.self, forKey: .fast) ?? false)
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encodeIfPresent(model, forKey: .model)
        try c.encode(usesModel, forKey: .usesModel)
        try c.encodeIfPresent(effort, forKey: .effort)
        try c.encode(fast, forKey: .fast)
    }

    /// The model the chat will start with; nil is the provider's default.
    public var chosen: ChatModelOption? { usesModel ? model : nil }
    public var efforts: [String] { chosen?.efforts ?? [] }
    /// The effort shown as chosen: the person's own when the model takes it, else the model's default.
    public var selectedEffort: String? {
        guard let chosen, !chosen.efforts.isEmpty else { return nil }
        if let effort, let match = chosen.effort(matching: effort) { return match }
        return chosen.defaultEffortChoice
    }
    public var showsFast: Bool { chosen?.supportsFast == true }
    public var fastIsOn: Bool { showsFast && fast }

    public mutating func useDefault() { usesModel = false }
    public mutating func useLastModel() { usesModel = model != nil }
    public mutating func choose(effort word: String) { if let match = chosen?.effort(matching: word) { effort = match } }
    public mutating func setFast(_ on: Bool) { fast = on }

    // MARK: What chat.create gets

    /// Nothing is sent for the default: the Mac's own choice stays its own.
    public var requestedModel: String? { chosen?.id }
    /// Only an effort the person chose (and the model takes) is sent; otherwise the model's default is the provider's.
    public var requestedEffort: String? { effort.flatMap { chosen?.effort(matching: $0) } }
    /// Only when on: off is the default, and a desktop that predates Fast denies the field.
    public var requestedFast: Bool { fastIsOn }

    /// "Opus 4.1 · High · Fast" for a chosen model, nil for the default.
    public var summary: String? {
        guard let chosen else { return nil }
        return [chosen.shortName, selectedEffort.map(ChatEffort.title), fastIsOn ? "Fast" : nil].compactMap { $0 }.joined(separator: " · ")
    }

    // MARK: Remembering

    /// What to remember after a model, effort or Fast was chosen inside a chat: that model, with what it is set to, as the next chat's choice.
    /// A chat whose model is not known (the list does not name it) changes nothing.
    public func remembering(_ choices: ChatModelChoices) -> NewChatChoice {
        guard let current = choices.current else { return self }
        return NewChatChoice(model: current, usesModel: true, effort: choices.selectedEffort, fast: choices.fastIsOn)
    }
}

extension ChatCreateRequest {
    /// A new chat with the choice made in the sheet: model, effort and Fast only where the person chose them.
    public init(provider: ChatProvider, target: Target, approvalMode: ChatApprovalMode? = nil, choice: NewChatChoice) throws {
        try self.init(provider: provider, target: target, approvalMode: approvalMode, model: choice.requestedModel, effort: choice.requestedEffort, fast: choice.requestedFast)
    }
}

// MARK: - In the sheet

/// One row of a new chat's model list, which offers both providers: a provider's default, the model last used with it (while its list is not
/// known), or one of its models. Choosing one chooses the provider too.
public enum NewChatRow: Sendable, Equatable, Identifiable {
    case providerDefault(ChatProvider)
    case last(ChatProvider, ChatModelOption)
    case model(ChatProvider, ChatModelOption)

    public var provider: ChatProvider {
        switch self {
        case .providerDefault(let provider), .last(let provider, _), .model(let provider, _): provider
        }
    }
    public var id: String {
        switch self {
        case .providerDefault(let provider): "\(provider.rawValue):default"
        case .last(let provider, let option): "\(provider.rawValue):last:\(option.id)"
        case .model(let provider, let option): "\(provider.rawValue):model:\(option.id)"
        }
    }
}

extension NewTerminalForm {
    /// The choice for the provider of a new chat; nil for a terminal.
    public var chatChoice: NewChatChoice? { kind.isChat ? chatChoices[chatProvider] ?? NewChatChoice() : nil }

    /// Whether a control exists now: the model rows for a chat, its efforts when the chosen model has them, Fast when it has one.
    func chatFieldShown(_ field: Field) -> Bool {
        switch field {
        case .chatModel: chatChoice != nil
        case .chatEffort: chatChoice?.efforts.isEmpty == false
        case .chatFast: chatChoice?.showsFast == true
        default: true
        }
    }

    /// Every provider's rows, Codex first, each under its default: the models it is known to offer, or the one last used with it while
    /// that is not known.
    public var chatRows: [NewChatRow] {
        ChatProvider.allCases.flatMap { provider -> [NewChatRow] in
            let models = chatModels[provider] ?? []
            var rows: [NewChatRow] = [.providerDefault(provider)]
            if models.isEmpty, let last = chatChoices[provider]?.model { rows.append(.last(provider, last)) }
            return rows + models.map { .model(provider, $0) }
        }
    }
    /// The row that is chosen; nil for a terminal, and for a model the provider's list no longer names.
    public var chosenChatRow: NewChatRow? {
        guard let choice = chatChoice else { return nil }
        guard let chosen = choice.chosen else { return .providerDefault(chatProvider) }
        let models = chatModels[chatProvider] ?? []
        if models.isEmpty { return .last(chatProvider, chosen) }
        return models.first { $0.id == chosen.id }.map { .model(chatProvider, $0) }
    }
    /// A row was chosen: its provider, and its model (with what the model cannot keep dropped) or the provider's default.
    public mutating func chooseChatRow(_ row: NewChatRow) {
        guard kind.isChat else { return }
        selectChatProvider(row.provider)
        switch row {
        case .providerDefault: selectChatModel(last: false)
        case .last: selectChatModel(last: true)
        case .model(_, let option): selectChatModel(option)
        }
    }

    /// Chooses the last model used (or the default) for the provider of the chat.
    public mutating func selectChatModel(last: Bool) { change { if last { $0.useLastModel() } else { $0.useDefault() } } }
    /// Chooses one of the models of the chat's provider.
    public mutating func selectChatModel(_ option: ChatModelOption) {
        guard kind.isChat, chatModels[chatProvider]?.contains(where: { $0.id == option.id }) == true else { return }
        let models = chatModels[chatProvider] ?? []
        change { choice in
            let choices = ChatModelChoices(models: models, model: choice.requestedModel, effort: choice.effort, fast: choice.fast)
            if let configuration = choices.configuration(for: .model(option.id)) {
                choice = choice.remembering(choices.applying(configuration))
            } else {
                choice.model = option; choice.usesModel = true
            }
        }
    }
    public mutating func selectChatEffort(_ effort: String) { change { $0.choose(effort: effort) } }
    public mutating func setChatFast(_ on: Bool) { change { $0.setFast(on) } }

    private mutating func change(_ edit: (inout NewChatChoice) -> Void) {
        guard kind.isChat else { return }
        var choice = chatChoices[chatProvider] ?? NewChatChoice()
        edit(&choice)
        chatChoices[chatProvider] = choice
        if !fields.contains(focus) { focus = .kind }
    }

    /// Up and down choose within the model rows (across both providers) and the efforts; space flips Fast. Left, right and tab still move the
    /// ring, and up and down on Fast choose the kind as they do on the other toggle. True when the key was the chat controls'.
    mutating func handleChat(_ key: Key) -> Bool {
        guard let choice = chatChoice else { return false }
        switch (focus, key) {
        case (.chatModel, .up), (.chatModel, .down):
            let rows = chatRows
            guard !rows.isEmpty else { return true }
            let at = chosenChatRow.flatMap { chosen in rows.firstIndex { $0.id == chosen.id } }
                ?? rows.firstIndex { $0 == .providerDefault(chatProvider) } ?? 0
            chooseChatRow(rows[(at + (key == .down ? 1 : -1) + rows.count) % rows.count])
            return true
        case (.chatEffort, .up), (.chatEffort, .down):
            let efforts = choice.efforts
            guard let at = choice.selectedEffort.flatMap({ efforts.firstIndex(of: $0) }) else { return true }
            let next = efforts[min(max(0, at + (key == .down ? 1 : -1)), efforts.count - 1)]
            change { $0.choose(effort: next) }
            return true
        case (.chatFast, .space): change { $0.setFast(!choice.fastIsOn) }; return true
        default: return false
        }
    }
}
