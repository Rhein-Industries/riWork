import Foundation
import RiWorkCore

extension RemoteModel {
    /// One standalone Configure, never retried and never shown as a hopeful local value.
    /// A busy refusal is the host's to explain; its restart and rollback semantics stay there.
    @discardableResult
    func setChatContributor(_ chatID: String, _ mode: ChatContributorMode) async -> ChatControlError? {
        let conversation = conversation(chatID)
        guard !conversation.changingContributor else { return .busy }
        conversation.changingContributor = true
        defer { conversation.changingContributor = false }
        let connection = generation
        let failure = await sendChatCommand(chatID, .configure(contributorMode: mode))
        guard generation == connection, chatConversations[chatID] === conversation else { return failure }
        conversation.notice = failure?.message
        // Even a refused or unanswered restart is followed by a read: only the host knows
        // whether the new process resumed, rolled back, or the command arrived at all.
        if state == .connected { await refreshChatContributor(conversation, connection: connection, commandFailure: failure) }
        return failure
    }

    private func refreshChatContributor(_ conversation: ChatConversation, connection: UUID, commandFailure: ChatControlError?) async {
        let id = conversation.id
        do {
            do {
                let snapshot = try await client.chatSnapshot(chatID: id)
                guard generation == connection, chatConversations[id] === conversation,
                      snapshot.next >= conversation.feed.next else { return }
                guard let info = snapshot.controls.compactMap({ event -> ChatInfo? in
                    if case .info(let info) = event, info.id == id { return info }; return nil
                }).last else { throw ChatControlError.unreadableReply }
                conversation.contributorState.observe(info, through: snapshot.next)
            } catch where RemoteError.isUnsupportedMethod(error) {
                // Hosts before snapshots still list ChatInfo. Do not overwrite an event
                // received while this unversioned list read was on its way, nor let a page
                // asked for before it undo it (`ChatContributorState.observeListed`).
                let since = conversation.feed.next
                let listed = try await client.listChats(ChatListRequest())
                guard generation == connection, chatConversations[id] === conversation,
                      conversation.feed.next == since else { return }
                guard let info = listed.first(where: { $0.id == id }) else { throw ChatControlError.notFound(.events) }
                conversation.contributorState.observeListed(info)
            }
        } catch {
            guard generation == connection, chatConversations[id] === conversation else { return }
            let reason = ChatControlError.from(error, operation: .events).message
            conversation.notice = [commandFailure?.message, "Could not refresh contributor mode. \(reason)"].compactMap { $0 }.joined(separator: "\n")
            await noteLinkLossIfNeeded()
        }
    }
}
