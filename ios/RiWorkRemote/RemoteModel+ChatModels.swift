import Foundation
import Observation
import RiWorkCore

// Choosing a chat's model, effort and Fast from the phone, and remembering what was chosen for the next chat. The decisions are
// `ChatModelChoices` and `NewChatChoice` in RiWorkCore (pure, tested); this file is the timing and the store.

extension ChatConversation {
    /// What the picker shows for this chat: the transcript's models and the chat's own model, effort and Fast, with a choice that was
    /// sent and has not come back yet laid over them. `fallback` is the tab list's entry, until the chat has told its own. With `switchable`
    /// (a desktop that lets a chat go on with the other provider) the other provider's rows come too.
    func modelChoices(fallback: ChatInfo?, switchable: Bool = false) -> ChatModelChoices {
        let info = transcript.info ?? fallback
        var choices = ChatModelChoices(models: transcript.models.isEmpty ? modelCatalogue : transcript.models,
                                       model: pendingModel?.model ?? info?.model, effort: pendingModel?.effort ?? info?.effort,
                                       fast: pendingModel?.fast ?? info?.fast ?? false)
        if switchable, let provider = info?.provider {
            choices.switching = switching
                ? ChatProviderSwitch(provider: provider.other, models: switchCatalogue, blocked: "Going on with \(provider.other.title)…")
                : ChatProviderSwitch(provider: provider.other, models: switchCatalogue, transcript: transcript)
        }
        return choices
    }
    /// The chat's own word (an `info` event) has what was asked for: the choice no longer needs showing from here.
    func settleModelChoice() {
        guard let pending = pendingModel, let info = transcript.info, pending.isSettled(by: info, models: transcript.models.isEmpty ? modelCatalogue : transcript.models) else { return }
        pendingModel = nil
        modelExpiry?.cancel()
    }
}

extension RemoteModel {
    /// Sends one choice as one `Configure`, and shows it at once. A choice that changes nothing (the model already runs) is not sent. If
    /// the desktop refuses, or the link is down, the picker takes it back and says why; when it goes through, the choice is what the next
    /// chat of this provider starts with.
    @discardableResult
    func chooseChatModel(_ chat: ChatInfo, _ change: ChatModelChoices.Change) async -> ChatControlError? {
        let conversation = conversation(chat.id)
        let choices = conversation.modelChoices(fallback: chat)
        guard let configuration = choices.configuration(for: change) else { return nil }
        let previous = conversation.pendingModel
        conversation.pendingModel = configuration.merged(over: previous)
        let failure = await sendChatCommand(chat.id, configuration.command)
        if let failure {
            conversation.pendingModel = previous
            conversation.notice = failure.message
            return failure
        }
        conversation.notice = nil
        let provider = conversation.transcript.info?.provider ?? chat.provider
        rememberChatChoice(chatChoice(for: provider).remembering(choices.applying(configuration)), for: provider)
        // The desktop's own word (an `info` event) normally replaces this within a moment; if it never comes, the picker does not keep a
        // choice that may since have been changed from the Mac.
        conversation.modelExpiry?.cancel()
        conversation.modelExpiry = Task { [weak conversation] in
            try? await Task.sleep(for: .seconds(10))
            if !Task.isCancelled { conversation?.pendingModel = nil }
        }
        return nil
    }

    // MARK: Going on with the other provider

    /// Moves a chat to the other provider, on one of its models (nil: its default), in place: the chat keeps its tab and its conversation
    /// (`switch`). Sent once. Refused here, as on the Mac, while a turn runs or waits; what went wrong is said above the composer.
    @discardableResult
    func switchChatProvider(_ chat: ChatInfo, model id: String?) async -> ChatControlError? {
        let conversation = conversation(chat.id)
        guard !conversation.switching else { return .busy }
        guard let switching = conversation.modelChoices(fallback: chat, switchable: true).switching else { return .unsupported }
        if let blocked = switching.blocked { conversation.notice = blocked; return .failed(blocked) }
        guard let command = switching.command(model: id) else { return nil }
        conversation.switching = true
        defer { conversation.switching = false }
        let failure = await sendChatCommand(chat.id, command)
        conversation.notice = failure?.message
        return failure
    }

    /// The other provider's rows of a chat's picker, from the phone's catalogue at once (`prepareSwitchCatalogue`) and then the desktop's
    /// (`loadSwitchCatalogue`).
    func prepareSwitchCatalogue(_ chat: ChatInfo) {
        let conversation = conversation(chat.id)
        guard conversation.switchCatalogue.isEmpty else { return }
        let fallback = cachedOrBundledModels(provider: (conversation.transcript.info ?? chat).provider.other)
        conversation.switchCatalogue = fallback.models
        conversation.switchCatalogueSource = fallback.source
    }
    func loadSwitchCatalogue(_ chat: ChatInfo) async {
        prepareSwitchCatalogue(chat)
        let conversation = conversation(chat.id)
        let other = (conversation.transcript.info ?? chat).provider.other
        let token = generation
        guard let models = try? await providerModels(other), generation == token,
              (conversation.transcript.info ?? chat).provider.other == other else { return }
        conversation.switchCatalogue = models
        conversation.switchCatalogueSource = .live
        rememberModelCatalogue(models, provider: other)
    }

    /// A provider's models for a chat that does not run it yet: `chat.models` on a desktop that has it (read from saved chats, starting
    /// nothing), else what the chats of it the phone can read said.
    func providerModels(_ provider: ChatProvider) async throws -> [ChatModelOption] {
        guard desktopFeatures.chatModels else { return try await availableChatModels(provider: provider) }
        guard state == .connected else { throw ChatControlError.notConnected }
        let reply = try await client.chatModels(ChatModelsRequest(provider: provider, projectID: projectID))
        // The Mac's own words, on one line and bounded.
        if let error = reply.error { throw ChatControlError.failed(String(error.split(whereSeparator: \.isNewline).joined(separator: " ").prefix(240))) }
        guard !reply.models.isEmpty else {
            throw ChatControlError.failed("No \(provider.title) chat has listed its models yet. Its default is on offer; the list follows once one has.")
        }
        return reply.models
    }

    // MARK: Remembered for the next chat

    static let newChatProviderKey = "riwork.newChat.provider"
    /// The provider of the last chat made from the phone; the chat kind an older version remembered per provider says it too.
    var lastChatProvider: ChatProvider {
        if let word = defaults.string(forKey: Self.newChatProviderKey), let provider = ChatProvider(rawValue: word) { return provider }
        return defaults.string(forKey: Self.newTerminalKindKey).flatMap(NewTerminalKind.legacyChatProvider) ?? .codex
    }
    func rememberChatProvider(_ provider: ChatProvider) {
        if defaults.string(forKey: Self.newChatProviderKey) != provider.rawValue { defaults.set(provider.rawValue, forKey: Self.newChatProviderKey) }
    }

    static func newChatChoiceKey(_ provider: ChatProvider) -> String { "riwork.newChat.\(provider.rawValue)" }

    /// The model, effort and Fast the last chat of this provider was set to, or the default when there was none (or what is stored cannot
    /// be read).
    func chatChoice(for provider: ChatProvider) -> NewChatChoice {
        defaults.data(forKey: Self.newChatChoiceKey(provider)).flatMap { try? JSONDecoder().decode(NewChatChoice.self, from: $0) } ?? NewChatChoice()
    }
    func rememberChatChoice(_ choice: NewChatChoice, for provider: ChatProvider) {
        guard choice != chatChoice(for: provider), let data = try? JSONEncoder().encode(choice) else { return }
        defaults.set(data, forKey: Self.newChatChoiceKey(provider))
    }
    /// What the New terminal sheet starts with, for both providers.
    func rememberedChatChoices() -> [ChatProvider: NewChatChoice] {
        Dictionary(uniqueKeysWithValues: ChatProvider.allCases.map { ($0, chatChoice(for: $0)) })
    }
}

extension NewTerminalSheetModel {
    /// A tap on a row of a new chat's model list (any provider's), on the default or the last model of the chosen provider, on an effort, or on
    /// Fast: the choice is made and the ring goes there, so touch and keyboard agree.
    func chooseChatRow(_ row: NewChatRow) {
        guard !busy else { return }
        touch(.chatModel) { $0.chooseChatRow(row) }
    }
    func chooseChatModel(last: Bool) { touch(.chatModel) { $0.selectChatModel(last: last) } }
    func chooseChatEffort(_ effort: String) { touch(.chatEffort) { $0.selectChatEffort(effort) } }
    func setChatFast(_ on: Bool) { touch(.chatFast) { $0.setChatFast(on) } }

    private func touch(_ field: NewTerminalForm.Field, _ edit: (inout NewTerminalForm) -> Void) {
        keyboardInUse = false; chatError = nil
        edit(&form)
        form.focus = form.fields.contains(field) ? field : .kind
    }
}

enum ChatCatalogueSource: String {
    case live, cached, bundled
    var label: String? {
        switch self {
        case .live: nil
        case .cached: "Cached fallback · Last successfully loaded list. Availability may differ for your account."
        case .bundled: "Bundled fallback · Recorded provider models. Availability may differ for your account."
        }
    }
}

extension RemoteModel {
    static let catalogueLoadingLimit: Duration = .seconds(4)

    func catalogueAccount(provider: ChatProvider, chat: ChatInfo? = nil) -> String? {
        guard provider == .codex else { return nil }
        if let chat {
            return chatConversations[chat.id]?.transcript.info?.codexAccountID ?? chat.codexAccountID
                ?? chats.first(where: { $0.id == chat.id })?.codexAccountID
        }
        let accounts = Set(chats.filter { $0.provider == provider && $0.projectID == projectID }.compactMap(\.codexAccountID))
        return accounts.count == 1 ? accounts.first : nil
    }

    /// Unknown accounts are additionally isolated by project; paired desktop identity survives app relaunch and renaming.
    func catalogueCacheKey(provider: ChatProvider, chat: ChatInfo? = nil) -> String? {
        guard let desktopID = desktop?.pairing.desktop_id else { return nil }
        let account = catalogueAccount(provider: provider, chat: chat) ?? "unknown-project-\(chat?.projectID ?? projectID ?? "global")"
        return "riwork.modelCatalogue.v1.\(desktopID).\(provider.rawValue).\(account)"
    }

    func cachedOrBundledModels(provider: ChatProvider, chat: ChatInfo? = nil) -> (models: [ChatModelOption], source: ChatCatalogueSource) {
        if let key = catalogueCacheKey(provider: provider, chat: chat), let data = defaults.data(forKey: key),
           let models = try? JSONDecoder().decode([ChatModelOption].self, from: data), !models.isEmpty {
            return (models, .cached)
        }
        return (ChatBundledModels.models(for: provider), .bundled)
    }

    func rememberModelCatalogue(_ models: [ChatModelOption], provider: ChatProvider, chat: ChatInfo? = nil) {
        guard !models.isEmpty, let key = catalogueCacheKey(provider: provider, chat: chat),
              let data = try? JSONEncoder().encode(models) else { return }
        if defaults.data(forKey: key) != data { defaults.set(data, forKey: key) }
    }

    /// Fallback rows are selectable immediately; successful provider events replace them.
    func prepareChatCatalogue(_ chat: ChatInfo) {
        let conversation = conversation(chat.id)
        if !conversation.transcript.models.isEmpty {
            conversation.modelCatalogueSource = .live
            rememberModelCatalogue(conversation.transcript.models, provider: chat.provider, chat: conversation.transcript.info ?? chat)
        } else if conversation.modelCatalogueSource != .live || conversation.modelCatalogue.isEmpty {
            let fallback = cachedOrBundledModels(provider: chat.provider, chat: conversation.transcript.info ?? chat)
            conversation.modelCatalogue = fallback.models
            conversation.modelCatalogueSource = fallback.source
        }
    }

    /// Read-only lookup. A legacy chat can use a newer compatible chat's catalogue. Only complete, bounded replays are trusted,
    /// and the last Models event wins, including an empty event that withdraws a former catalogue.
    func availableChatModels(provider: ChatProvider, chat: ChatInfo? = nil) async throws -> [ChatModelOption] {
        guard state == .connected else { throw ChatControlError.notConnected }
        let token = generation
        let project = projectID
        let account = catalogueAccount(provider: provider, chat: chat)
        let deadline = ContinuousClock.now + Self.catalogueLoadingLimit
        func check() throws {
            try Task.checkCancellation()
            guard generation == token, projectID == project else { throw CancellationError() }
            guard ContinuousClock.now < deadline else { throw ChatControlError.failed("Live model lookup took too long. Use the fallback list or Retry.") }
        }
        // An already-followed chat's latest event is authoritative and incurs no replay.
        if let chat, let conversation = chatConversations[chat.id], !conversation.transcript.models.isEmpty {
            return conversation.transcript.models
        }
        var sources = chat.map { [$0] } ?? []
        var lastError: (any Error)?
        do { sources += try await client.listChats(ChatListRequest(projectID: nil)) }
        catch { lastError = error }
        try check()
        sources += tabs.compactMap(\.chatInfo)
        var seen = Set<String>()
        let candidates = sources.filter { candidate in
            guard candidate.provider == provider, seen.insert(candidate.id).inserted else { return false }
            if candidate.id == chat?.id { return true }
            if provider == .codex, let account { return candidate.codexAccountID == account }
            return candidate.projectID == project && candidate.codexAccountID == nil
        }.prefix(8)
        for candidate in candidates {
            try check()
            do {
                var since: UInt64 = 0
                var latest: [ChatModelOption]?
                for _ in 0..<20 {
                    let reply = try await client.chatEvents(ChatEventsRequest(chatID: candidate.id, since: since, waitMilliseconds: 0, maxEvents: 500))
                    try check()
                    for envelope in reply.events {
                        if case .models(let models) = envelope.event { latest = models }
                    }
                    if !reply.more {
                        if let live = chatConversations[candidate.id], live.feed.next >= reply.next, !live.transcript.models.isEmpty {
                            return live.transcript.models
                        }
                        if let latest, !latest.isEmpty { return latest }
                        break
                    }
                    guard reply.next > since else { break }
                    since = reply.next
                }
            } catch is CancellationError { throw CancellationError() }
            catch { lastError = error }
        }
        if let lastError { throw lastError }
        throw ChatControlError.failed("The running chat host has not published a model list. You can use the fallback list and Retry later; model availability depends on your account.")
    }
}

extension NewTerminalSheetModel {
    /// Reads the models of a new chat, both providers' unless one is named. Each shows its fallback at once and its live list when that
    /// comes; one provider's failure is said under its own rows and never touches the other's.
    func loadChatModels(_ only: ChatProvider? = nil) async {
        let providers = only.map { [$0] } ?? ChatProvider.allCases
        // Side by side, so one slow provider does not hold the other's list back; leaving the sheet cancels both.
        let loads = providers.map { provider in Task { await self.loadChatModels(of: provider) } }
        await withTaskCancellationHandler {
            for load in loads { await load.value }
        } onCancel: {
            for load in loads { load.cancel() }
        }
    }
    private func loadChatModels(of provider: ChatProvider) async {
        let token = UUID()
        let generation = model.generation
        let project = model.projectID
        catalogueRequests[provider] = token
        let fallback = model.cachedOrBundledModels(provider: provider)
        if form.chatModels[provider]?.isEmpty != false || chatModelsSources[provider] != .live {
            form.chatModels[provider] = fallback.models
            chatModelsSources[provider] = fallback.source
        }
        loadingProviders.insert(provider); chatModelsErrors[provider] = nil
        let timeout = Task { [weak self] in
            try? await Task.sleep(for: RemoteModel.catalogueLoadingLimit)
            guard !Task.isCancelled, let self, catalogueRequests[provider] == token, model.generation == generation, model.projectID == project else { return }
            catalogueRequests[provider] = UUID(); loadingProviders.remove(provider)
            chatModelsErrors[provider] = "Live model lookup took too long. Use the fallback list or Retry."
        }
        defer { timeout.cancel(); if catalogueRequests[provider] == token { loadingProviders.remove(provider) } }
        do {
            let models = try await model.providerModels(provider)
            guard catalogueRequests[provider] == token, form.kind.isChat, model.generation == generation, model.projectID == project else { return }
            form.chatModels[provider] = models
            chatModelsSources[provider] = .live
            model.rememberModelCatalogue(models, provider: provider)
        } catch is CancellationError { }
        catch {
            guard catalogueRequests[provider] == token, form.kind.isChat, model.generation == generation, model.projectID == project else { return }
            chatModelsErrors[provider] = ChatControlError.from(error, operation: .list).message
        }
    }
    func chooseChatModel(_ option: ChatModelOption) {
        guard !busy else { return }
        keyboardInUse = false; chatError = nil
        form.selectChatModel(option)
        form.focus = .chatModel
    }
}
