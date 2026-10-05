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

extension NewTerminalForm {
    /// The choice for the chat kind that is selected; nil for a terminal.
    public var chatChoice: NewChatChoice? { kind.chatProvider.map { chatChoices[$0] ?? NewChatChoice() } }

    /// Whether a control exists now: the model rows for a chat, its efforts when the chosen model has them, Fast when it has one.
    func chatFieldShown(_ field: Field) -> Bool {
        switch field {
        case .chatModel: chatChoice != nil
        case .chatEffort: chatChoice?.efforts.isEmpty == false
        case .chatFast: chatChoice?.showsFast == true
        default: true
        }
    }

    /// Chooses the last model used (or the default) for the chat kind that is selected.
    public mutating func selectChatModel(last: Bool) { change { if last { $0.useLastModel() } else { $0.useDefault() } } }
    public mutating func selectChatModel(_ option: ChatModelOption) {
        guard let provider = kind.chatProvider, chatModels[provider]?.contains(where: { $0.id == option.id }) == true else { return }
        let models = chatModels[provider] ?? []
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
        guard let provider = kind.chatProvider else { return }
        var choice = chatChoices[provider] ?? NewChatChoice()
        edit(&choice)
        chatChoices[provider] = choice
        if !fields.contains(focus) { focus = .kind }
    }

    /// Up and down choose within the model rows and the efforts; space flips Fast. Left, right and tab still move the ring, and up and down on
    /// Fast choose the kind as they do on the other toggle. True when the key was the chat controls'.
    mutating func handleChat(_ key: Key) -> Bool {
        guard let choice = chatChoice else { return false }
        switch (focus, key) {
        case (.chatModel, .up), (.chatModel, .down):
            let models = kind.chatProvider.flatMap { chatModels[$0] } ?? []
            guard !models.isEmpty else {
                change { if key == .up { $0.useDefault() } else { $0.useLastModel() } }
                return true
            }
            let index = choice.chosen.flatMap { chosen in models.firstIndex { $0.id == chosen.id } }.map { $0 + 1 } ?? 0
            let next = (index + (key == .down ? 1 : -1) + models.count + 1) % (models.count + 1)
            if next == 0 { change { $0.useDefault() } } else { selectChatModel(models[next - 1]) }
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
