import Foundation
import Observation
import RiWorkCore

/// Whether the desktop has chats, learned from `ready.features.chat` and from what it answers, and reset by every new connection.
enum ChatSupport: Equatable { case unknown, supported, unsupported }

/// One open chat as the phone holds it: what has been read of it, what the person has typed and not sent, and what is on its way.
/// It outlives the screen (leaving a chat and coming back finds the transcript where it was, and the reading goes on from there), but not
/// the app, and it is dropped with the desktop it belongs to.
@MainActor @Observable final class ChatConversation: Identifiable {
    let id: String
    /// The transcript and how far into the desktop's event log it has read.
    private(set) var feed = ChatFeed()
    var transcript: ChatTranscript { feed.transcript }
    /// What the person has typed and not sent. Kept per chat, so changing tabs loses nothing.
    var draft = ""
    /// The cards the person opened (command output, diffs, reasoning), by item id. Kept here because rows come and go as the list scrolls.
    var expanded: Set<String> = []
    /// The mode just chosen, shown until the desktop says so itself.
    var pendingMode: ChatApprovalMode?
    /// Counts requests to go to the end of the transcript: a message sent, the menu.
    private(set) var jumps = 0
    func jumpToEnd() { jumps &+= 1 }
    /// A message is on its way, and a second is not sent until it has been answered.
    var sending = false
    /// Requests whose answer is on its way, or has been given and not yet confirmed by the desktop: they leave the bar at once.
    var answered: Set<String> = []
    /// The last thing that went wrong with a command, said quietly above the composer until the next one goes through.
    var notice: String?
    /// Reading the events failed; shown (and retried) while the link is up.
    var readError: ChatControlError?
    /// The chat is no longer on the desktop.
    var gone = false
    /// A reader is following it now (the chat is on screen). Nothing is asked of the desktop for a chat that is not.
    private(set) var following = false
    @ObservationIgnored var follower: UUID?
    @ObservationIgnored var lastUsed = ContinuousClock.now
    @ObservationIgnored var modeExpiry: Task<Void, Never>?

    init(id: String) { self.id = id }

    /// Takes in an answer to a request made with `since`. A mode that was asked for and has now arrived is no longer pending.
    @discardableResult
    func accept(_ reply: ChatEventsReply, since: UInt64) -> ChatFeed.Outcome {
        let outcome = feed.accept(reply, since: since)
        if let pending = pendingMode, transcript.info?.approvalMode == pending { pendingMode = nil }
        // A request the desktop has resolved needs no hiding any more; one it has asked again does.
        let waiting = Set(transcript.approvals.map(\.requestID) + transcript.questions.map(\.requestID))
        answered.formIntersection(waiting)
        return outcome
    }
    func setFollowing(_ on: Bool) { if following != on { following = on } }
    func reset() { feed = ChatFeed(); answered = []; readError = nil }
    /// The approvals still to be answered, first in line first: those that were just answered are already out of the way.
    var openApprovals: [ChatApproval] { transcript.approvals.filter { !answered.contains($0.requestID) } }
    var openQuestions: [ChatQuestion] { transcript.questions.filter { !answered.contains($0.requestID) } }
}

// Chats on the phone: listing them, creating them (never twice), following one while it is on screen and sending it commands. The
// pure parts (the vocabulary, the transcript fold, the requests) are in RiWorkCore; this file is the timing.
extension RemoteModel {
    /// The sheet may offer Codex chat and Claude chat: the desktop said it has them, and has not refused them since.
    var chatsOffered: Bool { chatSupport == .supported }
    /// The strip: every terminal, every chat and every orchestrator, which runs as one or the other (`ProjectTabs`). A chat is not a
    /// terminal because it is an orchestrator: `mode` says, and the strip follows it.
    var tabs: [ProjectTab] {
        ProjectTabs.tabs(sessions: sessions, chats: chats, chatsAvailable: chatSupport != .unsupported, missing: missingSessionIDs)
    }
    /// The chosen project's own chats in tab order: those that are not an orchestrator's.
    var projectChats: [ChatInfo] { tabs.compactMap { if case .chat(let info) = $0 { info } else { nil } } }
    /// The chat on screen, if one is: a listed chat, or the chat (`chat_id`) of an orchestrator that runs as one.
    var selectedChat: ChatInfo? {
        guard let id = selectedChatID else { return nil }
        return tabs.lazy.compactMap(\.chatInfo).first { $0.id == id }
    }
    /// A chat is the screen on top of the tabs, so the terminal is not.
    var chatIsOnScreen: Bool { selectedChat != nil }
    /// The orchestrator whose notice is on screen ("Update the Mac to open this orchestrator"), if one is.
    var selectedBlocked: (session: RemoteSession, opening: SessionOpening)? {
        guard let id = selectedBlockedID else { return nil }
        for case .unavailable(let session, let opening) in tabs where session.id == id { return (session, opening) }
        return nil
    }
    /// Something other than the terminal is the screen on top of the tabs: it is released (its long poll stops, its pinned size is
    /// cleared) and its key view is not in the hierarchy.
    var terminalCovered: Bool { selectedChat != nil || selectedBlocked != nil }
    /// The orchestrator a chat is, when it is one: it is called by the orchestrator's name.
    func orchestrator(ofChat id: String) -> RemoteSession? {
        for case .orchestratorChat(let session, let info) in tabs where info.id == id { return session }
        return nil
    }

    /// What a chat's tab says it is doing: what the chat itself says while it is followed (a fresher word than the list's), otherwise
    /// what the list said.
    func chatActivity(_ chat: ChatInfo) -> AgentActivity {
        if let conversation = chatConversations[chat.id], conversation.following, conversation.feed.loaded { return conversation.transcript.state.activity }
        return chat.state.activity
    }
    /// The state the screen shows for a chat.
    func chatState(_ chat: ChatInfo) -> ChatState {
        if let conversation = chatConversations[chat.id], conversation.feed.loaded { return conversation.transcript.state }
        return chat.state
    }

    /// The conversation of a chat, made on first use. At most `keptConversations` are kept; the one used longest ago, and not followed,
    /// goes first.
    func conversation(_ id: String) -> ChatConversation {
        if let known = chatConversations[id] { known.lastUsed = .now; return known }
        let made = ChatConversation(id: id)
        if chatConversations.count >= Self.keptConversations {
            let spare = chatConversations.values.filter { !$0.following && $0.id != selectedChatID }.sorted { $0.lastUsed < $1.lastUsed }
            for old in spare.prefix(chatConversations.count - Self.keptConversations + 1) { chatConversations[old.id] = nil }
        }
        chatConversations[id] = made
        return made
    }
    static let keptConversations = 8

    // MARK: Selecting

    /// Puts a chat on screen. The terminal behind it is released by the screen, which stops showing it.
    func selectChat(_ id: String) {
        guard tabs.contains(where: { $0.chatInfo?.id == id }) else { return }
        _ = conversation(id)
        selectedChatID = id; selectedBlockedID = nil
    }
    func deselectChat() { selectedChatID = nil }

    /// Puts an orchestrator's tab on screen. One that runs as a chat opens that chat, by its `chat_id`; one the phone cannot open
    /// shows why; a terminal is chosen the way it always was (`chooseSession`).
    func chooseOrchestrator(_ tab: ProjectTab) {
        switch tab {
        case .orchestratorChat(_, let info): selectChat(info.id)
        case .unavailable(let session, _): selectedChatID = nil; selectedBlockedID = session.id
        case .terminal, .chat: break
        }
    }
    /// A chat, or an orchestrator's notice, that is no longer in the strip (the desktop no longer lists it, or no longer runs it that
    /// way) puts the terminals back.
    func reconcileChatSelection() {
        if selectedChatID != nil, selectedChat == nil { selectedChatID = nil }
        if selectedBlockedID != nil, selectedBlocked == nil { selectedBlockedID = nil }
    }

    // MARK: Listing

    /// The tab list of a project, as `chats.list` gave it. A chat that is selected and no longer listed puts the terminals back.
    func installChats(_ listed: [ChatInfo], project: String) {
        guard projectID == project else { return }
        let ordered = listed
        if ordered != chats { chats = ordered }
        reconcileChatSelection()
        // A chat the desktop no longer has is not held on to (an orchestrator's chat is the orchestrator list's to keep).
        let known = Set(ordered.map(\.id)).union(tabs.compactMap { $0.chatInfo?.id })
        for id in chatConversations.keys where !known.contains(id) && chatConversations[id]?.following != true { chatConversations[id] = nil }
        lastListRead[.sessions] = .now
    }

    /// The chats of a project, for a project load; nil when the desktop has none to offer or the list could not be read.
    func chatsOfProject(_ id: String) async -> [ChatInfo]? {
        guard chatSupport != .unsupported, desktopFeatures.chat, let request = try? ChatListRequest(projectID: id) else { return nil }
        return try? await client.listChats(request)
    }

    /// Reads the project's chats again without the side effects of a full refresh. A failed read is skipped: the next one tries again.
    func refreshChatsQuietly() async {
        guard state == .connected, chatSupport != .unsupported, desktopFeatures.chat, let project = projectID, let request = try? ChatListRequest(projectID: project) else { return }
        let token = generation
        do {
            let listed = try await client.listChats(request)
            guard generation == token, projectID == project, state == .connected else { return }
            installChats(listed, project: project)
        } catch ChatControlError.unsupported {
            if generation == token { chatSupport = .unsupported }
        } catch {}
    }

    // MARK: Creating

    /// Opens a chat on the desktop and puts it on screen. Returns nil on success, otherwise why not. The request is sent once: a lost
    /// answer (timeout, dropped link) is not asked again, because the chat may exist, and the person is told to look at the tab list.
    @discardableResult
    func createChat(_ request: ChatCreateRequest, onCreated: @MainActor (ChatInfo) -> Void = { _ in }) async -> ChatControlError? {
        guard !creatingChat else { return .busy }
        guard state == .connected else { return .notConnected }
        guard chatSupport != .unsupported else { return .unsupported }
        creatingChat = true
        defer { creatingChat = false }
        let token = generation
        let created: ChatInfo
        do {
            created = try await client.createChat(request)
        } catch {
            let failure = ChatControlError.from(error, operation: .create(request.provider))
            if generation == token, failure == .unsupported { chatSupport = .unsupported }
            return failure
        }
        if generation == token { chatSupport = .supported }
        rememberTerminalKind(request.provider == .codex ? .codexChat : .claudeChat)
        // A reconnect or another desktop in the meantime: the chat exists, but this screen is no longer its.
        guard generation == token, state == .connected else { return nil }
        if projectID != nil {
            if let index = chats.firstIndex(where: { $0.id == created.id }) { chats[index] = created } else { chats.append(created) }
        }
        onCreated(created)
        selectChat(created.id)
        await refreshChatsQuietly()
        return nil
    }

    // MARK: Following

    /// The long poll runs only for a chat that is on screen, in the foreground, on a live connection.
    var chatFollowWanted: Bool { state == .connected && appActive && chatSupport != .unsupported }

    /// Follows a chat for as long as the caller lives: the chat screen runs this in `.task`, so it ends when the screen goes (or the
    /// chat is switched), and nothing is asked of the desktop for a chat nobody is looking at.
    ///
    /// It asks `chat.events` with `since: next` and a wait of about 20 s, so the desktop answers the moment there is something new, and
    /// asks again at once. A page that was cut (`more`) is followed by the next without waiting. After a dropped connection it carries on
    /// from `next` once the link is back (it idles while it is not); after an error it waits 250 ms, doubling up to 2 s. The desktop lets
    /// a device have two requests waiting at once, shared with the terminal's own long poll, and one cancelled here keeps its place until
    /// its wait has run out, so the count is kept (`waitSlots`) and a poll that would be a third is a short read instead.
    func followChat(_ id: String) async {
        let conversation = conversation(id)
        let token = UUID()
        conversation.follower = token
        conversation.setFollowing(true)
        defer { if conversation.follower == token { conversation.follower = nil; conversation.setFollowing(false) } }
        var backoff = LongPollBackoff()
        while !Task.isCancelled, conversation.follower == token {
            guard chatFollowWanted, !conversation.gone else {
                if conversation.gone { return }
                try? await Task.sleep(for: chatIdleInterval)
                continue
            }
            let canWait = waitSlots.canWait(at: ProcessInfo.processInfo.systemUptime)
            let since = conversation.feed.next
            let request: ChatEventsRequest
            do { request = try ChatEventsRequest(chatID: id, since: since, waitMilliseconds: canWait ? chatWaitMilliseconds : 0) } catch { return }
            do {
                let reply = try await chatEventsFlight(request)
                guard !Task.isCancelled, conversation.follower == token else { return }
                conversation.readError = nil
                backoff.success()
                conversation.accept(reply, since: since)
                if chatSupport == .unknown { chatSupport = .supported }
                // Cut short: the rest is already there. Not waiting for a slot: a short read that found nothing waits a moment.
                if reply.more { continue }
                if !canWait, reply.events.isEmpty { try? await Task.sleep(for: .milliseconds(300)) }
            } catch is CancellationError {
                // The screen went (or the chat was switched): not a failure.
                return
            } catch ChatControlError.notFound {
                conversation.gone = true
                conversation.readError = .notFound(.events)
                await refreshChatsQuietly()
                return
            } catch ChatControlError.unsupported {
                chatSupport = .unsupported
                return
            } catch {
                guard !Task.isCancelled, conversation.follower == token else { return }
                conversation.readError = ChatControlError.from(error, operation: .events)
                await noteLinkLossIfNeeded()
                try? await Task.sleep(for: backoff.failure())
            }
        }
    }

    /// The one `chat.events` request. It runs as its own task so that cancelling the caller cancels only this request (the connection,
    /// its keys and every other request stay as they were), and the desktop's late answer is dropped. The desktop does not know: it
    /// keeps the wait, and its slot, until the wait runs out.
    private func chatEventsFlight(_ request: ChatEventsRequest) async throws -> ChatEventsReply {
        let flight = Task { [client] in try await client.chatEvents(request) }
        let started = ProcessInfo.processInfo.systemUptime
        let token = generation
        do {
            return try await withTaskCancellationHandler { try await flight.value } onCancel: { flight.cancel() }
        } catch is CancellationError {
            if request.isLongPoll, generation == token { waitSlots.abandon(startedAt: started, wait: Double(request.waitMilliseconds) / 1000) }
            throw CancellationError()
        }
    }

    /// A request failed: if the connection itself is gone, say so, as every other failed read does.
    func noteLinkLossIfNeeded() async {
        guard state == .connected, !(await client.isConnected()) else { return }
        state = .failed; snapshotStale = true; polling?.cancel()
        scheduleReconnectIfNeeded()
    }

    // MARK: Commands

    /// One command, sent once. The phone never sends a command again by itself: whether a message went through when the connection
    /// dropped is for the person to see in the transcript.
    @discardableResult
    func sendChatCommand(_ chatID: String, _ command: ChatCommand) async -> ChatControlError? {
        guard state == .connected else { return .notConnected }
        guard chatSupport != .unsupported else { return .unsupported }
        let request: ChatCommandRequest
        do { request = try ChatCommandRequest(chatID: chatID, command: command) } catch { return ChatControlError.from(error, operation: .command) }
        let token = generation
        do {
            try await client.sendChatCommand(request)
            return nil
        } catch {
            let failure = ChatControlError.from(error, operation: .command)
            if generation == token, failure == .unsupported { chatSupport = .unsupported }
            await noteLinkLossIfNeeded()
            return failure
        }
    }

    /// Sends the draft as a message. The text leaves the composer at once; if the message did not go through it comes back, so nothing the
    /// person wrote is lost, and what happened is said above the composer.
    @discardableResult
    func sendChatDraft(_ chatID: String) async -> ChatControlError? {
        let conversation = conversation(chatID)
        let text = conversation.draft
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
        return await sendChatMessage(chatID, text, restoring: true)
    }
    /// Sends `text` as a message (also Retry on a failed chat, which sends the last message again).
    @discardableResult
    func sendChatMessage(_ chatID: String, _ text: String, restoring: Bool = false) async -> ChatControlError? {
        let conversation = conversation(chatID)
        guard !conversation.sending else { return .busy }
        conversation.sending = true
        if restoring { conversation.draft = "" }
        defer { conversation.sending = false }
        let failure = await sendChatCommand(chatID, .send(text: text))
        if let failure {
            if restoring { conversation.draft = conversation.draft.isEmpty ? text : text + "\n" + conversation.draft }
            conversation.notice = failure.message
            return failure
        }
        conversation.notice = nil
        conversation.jumpToEnd()
        return nil
    }

    /// Allow, Allow for the session, Deny or Stop for a waiting request. It leaves the bar at once; if the desktop says it was answered
    /// already, that is said quietly, and the transcript is the truth.
    @discardableResult
    func decideChatApproval(_ chatID: String, _ approval: ChatApproval, _ decision: ChatDecision) async -> ChatControlError? {
        let conversation = conversation(chatID)
        guard !conversation.answered.contains(approval.requestID) else { return .busy }
        conversation.answered.insert(approval.requestID)
        let failure = await sendChatCommand(chatID, .approve(requestID: approval.requestID, decision: decision))
        if let failure {
            // Not sent: back in the bar. Sent and refused (answered already, or the turn is over): the events tell, and it stays out.
            if case .notConnected = failure { conversation.answered.remove(approval.requestID) }
            if failure.outcomeIsUncertain { conversation.answered.remove(approval.requestID) }
            conversation.notice = failure.message
        } else { conversation.notice = nil }
        return failure
    }
    /// Answers the questions of a waiting request.
    @discardableResult
    func answerChatQuestion(_ chatID: String, _ form: ChatAnswerForm) async -> ChatControlError? {
        let conversation = conversation(chatID)
        let requestID = form.question.requestID
        guard form.isComplete, !conversation.answered.contains(requestID) else { return .busy }
        conversation.answered.insert(requestID)
        let failure = await sendChatCommand(chatID, .answer(requestID: requestID, answers: form.answers))
        if let failure {
            if case .notConnected = failure { conversation.answered.remove(requestID) }
            if failure.outcomeIsUncertain { conversation.answered.remove(requestID) }
            conversation.notice = failure.message
        } else { conversation.notice = nil }
        return failure
    }
    @discardableResult
    func interruptChat(_ chatID: String) async -> ChatControlError? {
        let failure = await sendChatCommand(chatID, .interrupt)
        conversation(chatID).notice = failure?.message
        return failure
    }
    @discardableResult
    func compactChat(_ chatID: String) async -> ChatControlError? {
        let failure = await sendChatCommand(chatID, .compact)
        conversation(chatID).notice = failure?.message
        return failure
    }
    /// The desktop confirms the model through its info event; errors stay in the chat.
    @discardableResult
    func setChatModel(_ chatID: String, _ name: String) async -> ChatControlError? {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, name.utf8.count <= ChatLimits.modelBytes,
              !name.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else {
            let failure = ChatControlError.failed("Enter a model name on one line, up to \(ChatLimits.modelBytes) bytes.")
            conversation(chatID).notice = failure.message
            return failure
        }
        let failure = await sendChatCommand(chatID, .configure(model: name))
        conversation(chatID).notice = failure?.message
        return failure
    }
    /// Changes the approval mode. The picker shows the new mode at once and takes it back if the desktop refuses.
    @discardableResult
    func setChatMode(_ chatID: String, _ mode: ChatApprovalMode) async -> ChatControlError? {
        let conversation = conversation(chatID)
        let previous = conversation.pendingMode
        conversation.pendingMode = mode
        let failure = await sendChatCommand(chatID, .configure(approvalMode: mode))
        if let failure {
            conversation.pendingMode = previous
            conversation.notice = failure.message
            return failure
        }
        conversation.notice = nil
        // The desktop's own word (an `info` event) normally replaces this within a moment; if it never comes, the picker does not keep a
        // choice that may since have been changed from the Mac.
        conversation.modeExpiry?.cancel()
        conversation.modeExpiry = Task { [weak conversation] in
            try? await Task.sleep(for: .seconds(10))
            if !Task.isCancelled { conversation?.pendingMode = nil }
        }
        return nil
    }
    /// Stops the agent's process. The chat stays; the next message starts it again.
    @discardableResult
    func stopChat(_ chatID: String) async -> ChatControlError? {
        guard state == .connected else { return .notConnected }
        guard let request = try? ChatStopRequest(chatID: chatID) else { return .invalid("The chat id is not a UUID.") }
        let conversation = conversation(chatID)
        do {
            try await client.stopChat(request)
            conversation.notice = nil
            return nil
        } catch {
            let failure = ChatControlError.from(error, operation: .stop)
            conversation.notice = failure.message
            await noteLinkLossIfNeeded()
            return failure
        }
    }
}
