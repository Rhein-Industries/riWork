import Foundation

// The model picker of a chat, as decisions without a screen: which model a chat runs, what the toolbar's chip says, which efforts and whether
// Fast are on offer, what one choice sends, and what a keyboard does in the picker. Pure, so all of it is tested without one.
//
// The list of models is the provider's own (`ChatEvent.models`, kept in `ChatTranscript.models`); the phone has none before a chat exists
// and none from a desktop that predates it, and then there is no picker at all (`ChatModelChoices.isAvailable`).

// MARK: - Words

/// How an effort is called. The wire words are the provider's (`low`, `medium`, `high`, `xhigh` ...); these are for people.
public enum ChatEffort {
    /// "High", "X-High": short enough for a segment of five on a phone.
    public static func title(_ effort: String) -> String {
        switch effort.lowercased() {
        case "minimal": return "Minimal"
        case "low": return "Low"
        case "medium": return "Medium"
        case "high": return "High"
        case "xhigh", "x-high", "x_high", "extra_high", "extra-high": return "X-High"
        case "max": return "Max"
        default:
            let words = effort.replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: "-", with: " ").trimmingCharacters(in: .whitespaces)
            return words.isEmpty ? effort : words.prefix(1).uppercased() + words.dropFirst()
        }
    }
    /// For VoiceOver: "Extra high".
    public static func spoken(_ effort: String) -> String {
        let title = title(effort)
        return title == "X-High" ? "Extra high" : title
    }
}

extension ChatModelOption {
    /// "Opus 4.1" for "Claude Opus 4.1" and "Default" for "Default (recommended)": the chip has little room, and the tab says whose it is.
    public var shortName: String {
        var text = name.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.isEmpty { text = id }
        // A parenthesis that only recommends says nothing the "Default" mark does not; one that says what the model is ("1M context") stays.
        if let open = text.lastIndex(of: "("), text.hasSuffix(")"), text[open...].lowercased().contains("recommended") {
            text = String(text[..<open]).trimmingCharacters(in: .whitespaces)
        }
        if text.lowercased().hasPrefix("claude "), text.count > "claude ".count {
            let rest = text.dropFirst("claude ".count).trimmingCharacters(in: .whitespaces)
            if !rest.isEmpty { text = rest }
        }
        return text.isEmpty ? id : text
    }
    /// The entry of `efforts` that is `effort` (ignoring case), if the model takes it.
    public func effort(matching effort: String) -> String? { efforts.first { $0.caseInsensitiveCompare(effort) == .orderedSame } }
    /// The effort used when none is chosen, if it is one of the model's.
    public var defaultEffortChoice: String? { defaultEffort.flatMap(effort(matching:)) }
}

// MARK: - What the chat runs

/// The picker's view of a chat: the models on offer and the model, effort and Fast the chat has now (with a choice just made folded in).
/// It says what the chip shows, which efforts and whether Fast are on offer for the model, and what each choice sends.
public struct ChatModelChoices: Sendable, Equatable {
    /// The models on offer, in the provider's order, one per id.
    public let models: [ChatModelOption]
    /// What the chat runs (`ChatInfo.model`); nil when it has not said, which is the provider's default model.
    public let modelID: String?
    /// What the chat was asked to reason with (`ChatInfo.effort`); nil is the model's own default.
    public let effort: String?
    /// Fast was asked for (`ChatInfo.fast`).
    public let fast: Bool

    public init(models: [ChatModelOption], model: String?, effort: String?, fast: Bool) {
        var seen = Set<String>()
        self.models = models.filter { seen.insert($0.id).inserted }
        self.modelID = model; self.effort = effort; self.fast = fast
    }
    /// From a transcript, with a choice that has been sent and not yet confirmed by the desktop laid over what the chat says.
    public init(transcript: ChatTranscript, fallback: ChatInfo? = nil, pending: Configuration? = nil) {
        let info = transcript.info ?? fallback
        self.init(models: transcript.models, model: pending?.model ?? info?.model, effort: pending?.effort ?? info?.effort, fast: pending?.fast ?? info?.fast ?? false)
    }

    /// The desktop gave a list: without one (an older desktop, a driver that has not said yet) there is nothing to choose from.
    public var isAvailable: Bool { !models.isEmpty }

    /// The model the chat runs. An exact id; else the same ignoring case; else the one whose id is inside a longer name (Claude may report
    /// `claude-opus-4-1-20250805` for the alias `opus`), the longest first; with no model named, the provider's default.
    public var current: ChatModelOption? { Self.resolve(modelID, in: models) }
    public var currentIndex: Int? { current.flatMap { option in models.firstIndex { $0.id == option.id } } }

    static func resolve(_ id: String?, in models: [ChatModelOption]) -> ChatModelOption? {
        guard let id = id?.trimmingCharacters(in: .whitespacesAndNewlines), !id.isEmpty else { return models.first { $0.isDefault } }
        if let exact = models.first(where: { $0.id == id }) { return exact }
        let lower = id.lowercased()
        if let folded = models.first(where: { $0.id.lowercased() == lower }) { return folded }
        return models.filter { $0.id.lowercased() != "default" && lower.contains($0.id.lowercased()) }.max { $0.id.count < $1.id.count }
    }

    /// The efforts of the model that is chosen, in the provider's order. Empty when it has none to choose (or no model is known).
    public var efforts: [String] { current?.efforts ?? [] }
    /// The effort the control shows as chosen: the chat's own when the model takes it, else the model's default. Nil when there is none.
    public var selectedEffort: String? {
        guard let current, !current.efforts.isEmpty else { return nil }
        if let effort, let match = current.effort(matching: effort) { return match }
        return current.defaultEffortChoice
    }
    public var selectedEffortIndex: Int? { selectedEffort.flatMap { efforts.firstIndex(of: $0) } }
    /// Fast is on offer: the model has one.
    public var showsFast: Bool { current?.supportsFast == true }
    public var fastIsOn: Bool { showsFast && fast }

    // MARK: The chip

    /// The model's short name; what the chat says when it is not in the list; "Model" when it says nothing.
    public var chipTitle: String {
        if let current { return current.shortName }
        if let id = modelID?.trimmingCharacters(in: .whitespacesAndNewlines), !id.isEmpty { return id }
        return "Model"
    }
    /// A bolt when Fast is on. With a model the list does not know, the chat's own word is believed.
    public var chipShowsFast: Bool { current.map { $0.supportsFast && fast } ?? fast }
    /// "Opus 4.1 ⚡".
    public var chipText: String { chipShowsFast ? chipTitle + " ⚡" : chipTitle }
    /// For VoiceOver.
    public var spoken: String {
        var parts = [chipTitle]
        if let effort = selectedEffort { parts.append("\(ChatEffort.spoken(effort)) effort") }
        if chipShowsFast { parts.append("Fast on") }
        return parts.joined(separator: ", ")
    }

    // MARK: Choosing

    /// One thing the person chose.
    public enum Change: Sendable, Equatable {
        case model(String)
        case effort(String)
        case fast(Bool)
    }

    /// What one choice sends: one `Configure`. Each field is a change; a field left out stays as it is.
    public struct Configuration: Sendable, Equatable {
        public var model: String?
        public var effort: String?
        public var fast: Bool?
        public init(model: String? = nil, effort: String? = nil, fast: Bool? = nil) { self.model = model; self.effort = effort; self.fast = fast }
        public var command: ChatCommand { .configure(model: model, effort: effort, fast: fast) }
        /// This laid over an earlier one that has not been confirmed yet: what this sets wins, the rest stays.
        public func merged(over earlier: Configuration?) -> Configuration {
            Configuration(model: model ?? earlier?.model, effort: effort ?? earlier?.effort, fast: fast ?? earlier?.fast)
        }
        /// The desktop's own word (`info`) has what was asked for, so it no longer needs to be remembered here.
        public func isSettled(by info: ChatInfo, models: [ChatModelOption]) -> Bool {
            if let model {
                guard info.model == model || ChatModelChoices.resolve(info.model, in: models)?.id == model else { return false }
            }
            if let effort { guard info.effort?.caseInsensitiveCompare(effort) == .orderedSame else { return false } }
            if let fast { guard info.fast == fast else { return false } }
            return true
        }
    }

    /// The `Configure` for a choice; nil when it changes nothing (the model already runs, the effort is already chosen, Fast is already so) or
    /// is not on offer. Choosing a model also carries what the model cannot keep, in the same command: an effort it does not take becomes its
    /// own default, and Fast is turned off when it has none.
    public func configuration(for change: Change) -> Configuration? {
        switch change {
        case .model(let id):
            guard let option = models.first(where: { $0.id == id }), option.id != current?.id else { return nil }
            var configuration = Configuration(model: option.id)
            if let effort, !option.efforts.isEmpty, option.effort(matching: effort) == nil {
                configuration.effort = option.defaultEffortChoice ?? option.efforts.first
            }
            if fast, !option.supportsFast { configuration.fast = false }
            return configuration
        case .effort(let word):
            guard let match = current?.effort(matching: word) else { return nil }
            if let effort, effort.caseInsensitiveCompare(match) == .orderedSame { return nil }
            return Configuration(effort: match)
        case .fast(let on):
            guard showsFast, on != fastIsOn else { return nil }
            return Configuration(fast: on)
        }
    }

    /// The same chat with a choice made: what the picker shows at once, before the desktop has said so.
    public func applying(_ configuration: Configuration) -> ChatModelChoices {
        ChatModelChoices(models: models, model: configuration.model ?? modelID, effort: configuration.effort ?? effort, fast: configuration.fast ?? fast)
    }
}

// MARK: - The keyboard

/// Where the ring is in the picker, and what the arrows do. The picker is one column of stops: a row per model, then the effort row (when
/// the model takes efforts), then Fast (when it has one). Up, down and tab move between them (and wrap), left and right move along the
/// efforts, Return and space choose what the ring is on. Choosing sends one `Configure`; moving sends nothing.
public struct ChatModelCursor: Equatable, Sendable {
    public enum Stop: Equatable, Sendable { case model(Int), effort, fast }
    public enum Key: Equatable, Sendable { case up, down, left, right, tab, backTab, space, `return` }

    public private(set) var stop: Stop
    /// The effort the ring is on while it is on the efforts.
    public private(set) var effortIndex: Int

    /// On the model the chat runs, or the first.
    public init(for choices: ChatModelChoices) {
        stop = .model(choices.currentIndex ?? 0)
        effortIndex = choices.selectedEffortIndex ?? 0
    }

    public func stops(in choices: ChatModelChoices) -> [Stop] {
        choices.models.indices.map(Stop.model) + (choices.efforts.isEmpty ? [] : [.effort]) + (choices.showsFast ? [.fast] : [])
    }

    /// Puts the ring back on something that is still there after the rows changed (a model was chosen and its efforts are not the last's).
    public mutating func reconcile(with choices: ChatModelChoices) {
        let all = stops(in: choices)
        if !all.contains(stop) { stop = all.first { if case .model = $0 { true } else { false } } ?? all.first ?? .model(0) }
        effortIndex = choices.efforts.isEmpty ? 0 : min(max(0, effortIndex), choices.efforts.count - 1)
    }

    /// A tap puts the ring where the finger is, so touch and keyboard agree.
    public mutating func place(_ stop: Stop, effort index: Int? = nil, in choices: ChatModelChoices) {
        self.stop = stop
        if let index { effortIndex = index }
        reconcile(with: choices)
    }

    /// What the key does. Returns the choice a Return or a space made, nil for a key that only moved the ring.
    public mutating func handle(_ key: Key, in choices: ChatModelChoices) -> ChatModelChoices.Change? {
        reconcile(with: choices)
        let all = stops(in: choices)
        guard !all.isEmpty else { return nil }
        switch key {
        case .down, .tab: move(by: 1, among: all, in: choices)
        case .up, .backTab: move(by: -1, among: all, in: choices)
        case .left: if stop == .effort { effortIndex = max(0, effortIndex - 1) }
        case .right: if stop == .effort { effortIndex = min(choices.efforts.count - 1, effortIndex + 1) }
        case .space, .return:
            switch stop {
            case .model(let index): return choices.models.indices.contains(index) ? .model(choices.models[index].id) : nil
            case .effort: return choices.efforts.indices.contains(effortIndex) ? .effort(choices.efforts[effortIndex]) : nil
            case .fast: return .fast(!choices.fastIsOn)
            }
        }
        return nil
    }

    private mutating func move(by steps: Int, among all: [Stop], in choices: ChatModelChoices) {
        let at = all.firstIndex(of: stop) ?? 0
        stop = all[((at + steps) % all.count + all.count) % all.count]
        // Arriving on the efforts, the ring is on the one that is chosen.
        if stop == .effort, let chosen = choices.selectedEffortIndex { effortIndex = chosen }
    }
}
