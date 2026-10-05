import Foundation
import Observation
import RiWorkCore

// Choosing a chat's model, effort and Fast from the phone, and remembering what was chosen for the next chat. The decisions are
// `ChatModelChoices` and `NewChatChoice` in RiWorkCore (pure, tested); this file is the timing and the store.

extension ChatConversation {
    /// What the picker shows for this chat: the transcript's models and the chat's own model, effort and Fast, with a choice that was
    /// sent and has not come back yet laid over them. `fallback` is the tab list's entry, until the chat has told its own.
    func modelChoices(fallback: ChatInfo?) -> ChatModelChoices {
        let info = transcript.info ?? fallback
        return ChatModelChoices(models: transcript.models.isEmpty ? modelCatalogue : transcript.models,
                                model: pendingModel?.model ?? info?.model, effort: pendingModel?.effort ?? info?.effort,
                                fast: pendingModel?.fast ?? info?.fast ?? false)
    }
    /// The chat's own word (an `info` event) has what was asked for: the choice no longer needs showing from here.
    func settleModelChoice() {
        guard let pending = pendingModel, let info = transcript.info, pending.isSettled(by: info, models: transcript.models) else { return }
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

    // MARK: Remembered for the next chat

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
    /// A tap on the default or the last model of a new chat, on an effort, or on Fast: the choice is made and the ring goes there, so touch
    /// and keyboard agree.
    func chooseChatModel(last: Bool) { touch(.chatModel) { $0.selectChatModel(last: last) } }
    func chooseChatEffort(_ effort: String) { touch(.chatEffort) { $0.selectChatEffort(effort) } }
    func setChatFast(_ on: Bool) { touch(.chatFast) { $0.setChatFast(on) } }

    private func touch(_ field: NewTerminalForm.Field, _ edit: (inout NewTerminalForm) -> Void) {
        keyboardInUse = false; chatError = nil
        edit(&form)
        form.focus = form.fields.contains(field) ? field : .kind
    }
}

extension RemoteModel {
    /// Reuses the provider's Models events. Reading a catalogue never starts a chat or sends a command.
    /// New chats use an existing chat of the same provider and project, including its orchestrator.
    func availableChatModels(provider: ChatProvider, chat: ChatInfo? = nil) async throws -> [ChatModelOption] {
        guard state == .connected else { throw ChatControlError.notConnected }
        let token = generation
        let project = projectID
        let candidates: [ChatInfo]
        if let chat { candidates = [chat] }
        else {
            let listed = try await client.listChats(ChatListRequest(projectID: project))
            var seen = Set<String>()
            candidates = (listed + tabs.compactMap(\.chatInfo))
                .filter { $0.provider == provider && $0.projectID == project && seen.insert($0.id).inserted }
                .sorted { $0.createdAtUnix > $1.createdAtUnix }
        }
        var lastError: (any Error)?
        for candidate in candidates {
            try Task.checkCancellation()
            guard generation == token, projectID == project else { throw CancellationError() }
            if let conversation = chatConversations[candidate.id], !conversation.transcript.models.isEmpty {
                return conversation.transcript.models
            }
            do {
                var since: UInt64 = 0
                for _ in 0..<20 {
                    let reply = try await client.chatEvents(ChatEventsRequest(chatID: candidate.id, since: since, waitMilliseconds: 0, maxEvents: 500))
                    try Task.checkCancellation()
                    guard generation == token, projectID == project else { throw CancellationError() }
                    let catalogue = reply.events.compactMap { envelope -> [ChatModelOption]? in
                        if case .models(let models) = envelope.event { return models }
                        return nil
                    }.last
                    if let catalogue, !catalogue.isEmpty { return catalogue }
                    guard reply.more, reply.next > since else { break }
                    since = reply.next
                }
            } catch is CancellationError { throw CancellationError() }
            catch { lastError = error }
        }
        if let lastError { throw lastError }
        throw ChatControlError.failed("No model list is available yet. Start a chat with the provider default, then reopen Model after it connects. If a connected chat still has no list, update RiWork on your Mac and retry.")
    }
}

extension NewTerminalSheetModel {
    func loadChatModels() async {
        guard let provider = form.kind.chatProvider else { return }
        let token = UUID()
        catalogueRequest = token
        loadingChatModels = true; chatModelsError = nil
        defer { if catalogueRequest == token { loadingChatModels = false } }
        do {
            let models = try await model.availableChatModels(provider: provider)
            guard catalogueRequest == token, form.kind.chatProvider == provider else { return }
            form.chatModels[provider] = models
        } catch is CancellationError { }
        catch {
            guard catalogueRequest == token, form.kind.chatProvider == provider else { return }
            chatModelsError = ChatControlError.from(error, operation: .list).message
        }
    }
    func chooseChatModel(_ option: ChatModelOption) {
        guard !busy else { return }
        keyboardInUse = false; chatError = nil
        form.selectChatModel(option)
        form.focus = .chatModel
    }
}
