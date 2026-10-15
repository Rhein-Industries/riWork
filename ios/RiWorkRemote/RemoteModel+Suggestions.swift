import Foundation
import RiWorkCore

// The catalog behind a chat composer's `/` and `@` list: the chat's commands and its folder's files, read from the Mac with
// `chat.suggestions` the first time a list opens in the chat and again once it is 30 s old (`ComposerCatalogCache`). Filtering is the
// composer's own, over what is here (`ComposerSuggestionList`), so typing never waits for the Mac.

extension RemoteModel {
    /// What the composer of `chatID` can suggest now. Nothing on a desktop that does not answer `chat.suggestions`, and not the commands
    /// of a provider the chat has left.
    func composerCatalog(for chatID: String) -> ComposerCatalogState {
        guard desktopFeatures.chatSuggestions, composerCatalogs.refusedConnection != generation else { return .unsupported }
        return composerCatalogs.state(for: chatID, provider: suggestionProvider(chatID), connected: state == .connected)
    }

    /// A list opened (or is still open) in the composer of `chatID`: the catalog is read unless a fresh one is here or one is on its way.
    /// Returns at once; the answer lands in `composerCatalogs` and the composer filters it when it does. An older desktop that announced
    /// the feature and still does not know the method gets no further request on this connection. Called on every key while a list is
    /// open: `composerCatalogs` (which the chat screen observes) is written only when a read starts.
    func wantComposerSuggestions(_ chatID: String) {
        let now = ContinuousClock.now, provider = suggestionProvider(chatID)
        guard desktopFeatures.chatSuggestions, composerCatalogs.refusedConnection != generation, state == .connected,
              composerCatalogs.wantsFetch(chatID, provider: provider, now: now), composerCatalogs.beginFetch(chatID, provider: provider, now: now)
        else { return }
        let token = generation, client = client
        Task { [weak self] in
            do {
                let reply = try await client.chatSuggestions(ChatSuggestionsRequest(chatID: chatID))
                // Every path is lowercased once here, off the main thread, not on each key.
                let catalog = await Task.detached(priority: .userInitiated) { ComposerCatalog(reply: reply) }.value
                guard let self else { return }
                guard self.generation == token else { self.composerCatalogs.abandon(chatID); return }
                self.composerCatalogs.store(catalog, for: chatID, now: .now)
            } catch ChatControlError.unsupported {
                guard let self else { return }
                self.composerCatalogs.abandon(chatID)
                if self.generation == token { self.composerCatalogs.refusedConnection = token }
            } catch {
                self?.composerCatalogs.fail(chatID, now: .now)
            }
        }
    }

    /// The provider the chat works with now, as its screen has it (a `switch` changes it): a catalog of another one is read again.
    private func suggestionProvider(_ chatID: String) -> ChatProvider? {
        (chatConversations[chatID]?.transcript.info ?? chats.first { $0.id == chatID })?.provider
    }
}
