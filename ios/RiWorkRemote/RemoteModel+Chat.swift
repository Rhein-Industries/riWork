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
    var modelCatalogue: [ChatModelOption] = []
    var modelCatalogueSource: ChatCatalogueSource = .live
    var modelCatalogueRevision: UInt64 = 0
    /// What the person has typed and not sent. Kept per chat, so changing tabs loses nothing, and saved as it changes
    /// (`ChatDraftStore`), so leaving the project, the background, a dropped link or a relaunch lose nothing either.
    var draft = "" { didSet { if draft != oldValue { onDraftChange?(draft) } } }
    @ObservationIgnored var onDraftChange: ((String) -> Void)?
    /// The files staged for the message, shown as cards above the composer (`ChatAttachmentStrip`): on the Mac already, their paths go
    /// into the message when it is sent. Saved with the draft.
    var attachments: [StagedAttachment] = [] { didSet { if attachments != oldValue { onAttachmentsChange?(attachments) } } }
    @ObservationIgnored var onAttachmentsChange: (([StagedAttachment]) -> Void)?
    /// Something to send: text, or a card whose file the Mac still has (an expired one is not sent).
    var hasMessage: Bool { attachments.contains { !$0.isExpired() } || !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    /// The other provider's models, for going on with it (`switch`); empty until read.
    var switchCatalogue: [ChatModelOption] = []
    var switchCatalogueSource: ChatCatalogueSource = .bundled
    /// A `switch` is on its way: the other provider's rows wait for it.
    var switching = false
    /// The cards the person opened (command output, diffs, reasoning), by item id. Kept here because rows come and go as the list scrolls.
    var expanded: Set<String> = []
    /// The mode just chosen, shown until the desktop says so itself.
    var pendingMode: ChatApprovalMode?
    /// The model, effort or Fast just chosen, shown until the desktop says so itself (`RemoteModel+ChatModels.swift`).
    var pendingModel: ChatModelChoices.Configuration?
    /// Counts requests to go to the end of the transcript: a message sent, the menu.
    private(set) var jumps = 0
    func jumpToEnd() { jumps &+= 1 }
    /// A message is on its way, and a second is not sent until it has been answered.
    var sending = false
    /// Requests whose answer is on its way, or has been given and not yet confirmed by the desktop: they leave the bar at once.
    var answered: Set<String> = []
    /// The phone's own messages for this chat, one per source, in the banner row above the composer (`ChatNoticeBanners`).
    var alerts = ChatAlerts()
    /// The last thing that went wrong with a command, in the banner row until the next one goes through or it is closed.
    var notice: String? {
        get { alerts.text(.action) }
        set { if let newValue { alerts.show(.action, newValue) } else { alerts.clear(.action) } }
    }
    /// The provider notices (transcript items) the person closed; one comes back only when the provider says it again.
    var dismissedNotices: Set<String> = []
    /// Counts requests to show the usage windows (a usage limit's banner asks; the chip in the chat's row answers).
    private(set) var usageDetailRequests = 0
    func showUsageDetail() { usageDetailRequests &+= 1 }
    /// The read error the person closed; it comes back when another one, or the same one after a successful read, is said.
    var dismissedReadError: String?
    /// Reading the events failed; shown (and retried) while the link is up.
    var readError: ChatControlError?
    var historyLoading = false
    var historyError: String?
    var legacyLoading = false
    @ObservationIgnored var snapshotUnavailableGeneration: UUID?
    @ObservationIgnored var resourceFallbackGeneration: UUID?
    @ObservationIgnored var resourceBlockedGeneration: UUID?
    /// The first `response_too_large` (or `snapshot_limit`) of a connection starts the bounded replay; false once it is running.
    func recoverResourceLimit(_ error: ChatControlError, connection: UUID) -> Bool {
        guard case .resourceLimit = error, resourceFallbackGeneration != connection, !feed.degradedReplay else { return false }
        resourceFallbackGeneration = connection; snapshotUnavailableGeneration = connection
        feed.beginDegradedReplay(); readError = nil; legacyLoading = true
        sayOnce("shortened", .desktop, "Long messages are shortened here; full text is on your Mac.", level: .info)
        return true
    }
    /// Events per page while an older relay refuses even a bounded page as too large: halved on each refusal down to one, and a
    /// single event it still refuses is passed over (`ChatFeed.skipOversized`), so the chat never stops at it. Nil: the usual page.
    @ObservationIgnored var oversizeCap: Int?
    func passOversized() {
        readError = nil
        let cap = oversizeCap ?? Self.boundedPage
        if cap > 1 { oversizeCap = cap / 2; return }
        oversizeCap = nil
        feed.skipOversized()
        sayOnce("oversized", .read, "An update was too large to show here; it is on your Mac. Later messages keep arriving.")
    }
    static let boundedPage = 100
    /// Says a note in the banner row once per chat in this run: closed or cleared, it does not come back with the next page.
    @ObservationIgnored var noteSaid: (String) -> Bool = { _ in false }
    func sayOnce(_ note: String, _ source: ChatAlert.Source, _ text: String, level: ChatNoticeLevel = .warning) {
        if !noteSaid(note) { alerts.show(source, text, level: level) }
    }
    func install(_ snapshot: ChatSnapshotReply) {
        feed.install(snapshot)
        modelCatalogue = transcript.models; modelCatalogueSource = .live; modelCatalogueRevision &+= 1
        legacyLoading = false
        // A real snapshot: whatever an older desktop or a degraded replay had to say about loading is over.
        alerts.clear(.desktop)
    }
    func hydrate(_ snapshot: ChatSnapshotReply) { feed.hydrate(snapshot) }
    func prepend(_ snapshot: ChatSnapshotReply, before: UInt64) { feed.prepend(snapshot, requestedBefore: before) }

    /// The chat is no longer on the desktop.
    var gone = false
    /// A reader is following it now (the chat is on screen). Nothing is asked of the desktop for a chat that is not.
    private(set) var following = false
    @ObservationIgnored var follower: UUID?
    @ObservationIgnored var lastUsed = ContinuousClock.now
    @ObservationIgnored var modeExpiry: Task<Void, Never>?
    @ObservationIgnored var modelExpiry: Task<Void, Never>?

    init(id: String) { self.id = id }

    /// Takes in an answer to a request made with `since`. A mode that was asked for and has now arrived is no longer pending.
    @discardableResult
    func accept(_ reply: ChatEventsReply, since: UInt64) -> ChatFeed.Outcome {
        let previousNext = feed.next
        let provider = transcript.info?.provider
        let outcome = feed.accept(reply, since: since)
        if outcome == .restarted { modelCatalogue = []; modelCatalogueRevision &+= 1 }
        else if let provider, let now = transcript.info?.provider, now != provider {
            // The chat went on with the other provider: what was held for the last one (its list, a choice on its way) is not this one's.
            modelCatalogue = transcript.models; modelCatalogueSource = .live; modelCatalogueRevision &+= 1
            pendingModel = nil; modelExpiry?.cancel()
            switchCatalogue = []
        } else {
            for envelope in reply.events where envelope.seq > previousNext {
                if case .models = envelope.event {
                    modelCatalogue = transcript.models; modelCatalogueSource = .live; modelCatalogueRevision &+= 1
                }
            }
        }
        if let pending = pendingMode, transcript.info?.approvalMode == pending { pendingMode = nil }
        settleModelChoice()
        // A request the desktop has resolved needs no hiding any more; one it has asked again does.
        let waiting = Set(transcript.approvals.map(\.requestID) + transcript.questions.map(\.requestID) + transcript.elidedRequests.map(\.requestID))
        answered.formIntersection(waiting)
        return outcome
    }
    func setFollowing(_ on: Bool) { if following != on { following = on } }
    func reset() {
        let degraded = feed.degradedReplay
        feed = ChatFeed()
        if degraded { feed.beginDegradedReplay() }
        modelCatalogue = []; modelCatalogueRevision &+= 1; answered = []; readError = nil
    }
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
        let legacy = ProjectTabs.tabs(sessions: sessions, chats: chats, chatsAvailable: chatSupport != .unsupported, missing: missingSessionIDs)
        guard let sharedTabs else { return legacy }
        let listed = Dictionary(legacy.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        let shared: [ProjectTab] = sharedTabs.visible.compactMap { entry in
            if entry.kind == .shell, var session = sessions.first(where: { $0.id == entry.sessionID && $0.alive && $0.mode != .chat && !missingSessionIDs.contains($0.id) }) {
                session.sharedTitle = entry.title; return .terminal(session)
            }
            guard let tab = listed[entry.sessionID] else { return nil }
            switch tab {
            case .chat(var info): info.title = entry.title; return .chat(info)
            case .terminal(var session): session.sharedTitle = entry.title; return .terminal(session)
            case .orchestratorChat(var session, var info): session.sharedTitle = entry.title; info.title = entry.title; return .orchestratorChat(session, info)
            case .unavailable: return tab
            }
        }
        // Global orchestrators are device-local views outside project membership.
        return shared + legacy.filter { $0.session?.kind == "orchestrator" && $0.session?.project_id == nil }
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
        made.noteSaid = { [weak self] note in !(self?.chatNotesSaid.insert("\(id)|\(note)").inserted ?? false) }
        restoreDraft(made)
        if chatConversations.count >= Self.keptConversations {
            // A conversation with a message on its way stays: its answer must land in it, not in one made again later.
            let spare = chatConversations.values.filter { !$0.following && !$0.sending && $0.id != selectedChatID }.sorted { $0.lastUsed < $1.lastUsed }
            for old in spare.prefix(chatConversations.count - Self.keptConversations + 1) { chatConversations[old.id] = nil }
        }
        chatConversations[id] = made
        return made
    }
    static let keptConversations = 8
    /// Puts back what was typed into a chat the last time it was open (this run or before), and from then on saves every change.
    /// A message whose sending the desktop never answered (the app ended meanwhile) comes back before it, with a warning: it is not sent
    /// again by itself.
    func restoreDraft(_ conversation: ChatConversation) {
        let id = conversation.id
        if chatSendsInFlight.contains(id) { conversation.sending = true }
        if let saved = chatDrafts.draft(id) {
            if chatSendsInFlight.contains(id) {
                // Its message is on its way in this run (the conversation was let go meanwhile, with the desktop or the project): only
                // the text is the composer's, and the guard goes with it, so a second message is not sent before the first is answered.
                conversation.draft = saved.text
                conversation.attachments = saved.attachments
            } else {
                let restored = saved.restored
                conversation.draft = restored.text
                conversation.attachments = restored.attachments
                // The Mac keeps an upload for a day; a draft lasts a month. Cards past that are marked on the card and never sent.
                let expired = restored.attachments.filter { $0.isExpired() }.count
                if expired > 0 {
                    conversation.alerts.show(.action, expired == 1 ? "An attached file has expired on the Mac. Remove it and attach it again."
                                                                   : "\(expired) attached files have expired on the Mac. Remove them and attach them again.")
                }
                if restored.uncertain {
                    chatDrafts.restoreUncertain(restored.text, attachments: restored.attachments, for: id)
                    conversation.alerts.show(.action, "Your last message may not have reached the Mac. It is back in the composer: check the conversation before sending it again.")
                }
            }
        }
        conversation.onDraftChange = { [weak self] text in self?.chatDrafts.setText(text, for: id) }
        conversation.onAttachmentsChange = { [weak self] cards in self?.chatDrafts.setAttachments(cards, for: id) }
    }

    // MARK: Selecting

    /// Puts a chat on screen. The terminal behind it is released by the screen, which stops showing it.
    func selectChat(_ id: String) {
        guard tabs.contains(where: { $0.chatInfo?.id == id }) else { return }
        _ = conversation(id)
        selectedChatID = id; selectedBlockedID = nil
        rememberTab("chat:\(id)")
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
        for id in chatConversations.keys where !known.contains(id) && chatConversations[id]?.following != true && !chatSendsInFlight.contains(id) { chatConversations[id] = nil }
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
        rememberTerminalKind(.chat)
        rememberChatProvider(request.provider)
        // A reconnect or another desktop in the meantime: the chat exists, but this screen is no longer its.
        guard generation == token, state == .connected else { return nil }
        if projectID != nil {
            if let index = chats.firstIndex(where: { $0.id == created.id }) { chats[index] = created } else { chats.append(created) }
        }
        if let project = projectID, let list = await sharedTabsOfProject(project), generation == token, projectID == project { acceptSharedTabs(list) }
        guard generation == token, state == .connected else { return nil }
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
        following: while !Task.isCancelled, conversation.follower == token {
            if conversation.resourceBlockedGeneration == generation { return }
            guard chatFollowWanted, !conversation.gone else {
                if conversation.gone { return }
                try? await Task.sleep(for: chatIdleInterval)
                continue
            }
            if !conversation.feed.loaded, conversation.snapshotUnavailableGeneration != generation {
                let connection = generation
                do {
                    let snapshot = try await client.chatSnapshot(chatID: id)
                    guard !Task.isCancelled, conversation.follower == token, generation == connection else { continue }
                    conversation.install(snapshot)
                    conversation.readError = nil
                    if let info = conversation.transcript.info { prepareChatCatalogue(info) }
                } catch {
                    guard !Task.isCancelled, conversation.follower == token, generation == connection else { continue }
                    if RemoteError.isUnsupportedMethod(error) {
                        conversation.snapshotUnavailableGeneration = connection
                        conversation.legacyLoading = true
                        conversation.alerts.show(.desktop, "Update RiWork on the Mac for recent-first loading. Loading older desktop history…", level: .info)
                    } else {
                        let failure = ChatControlError.from(error, operation: .events)
                        if case .notFound = failure {
                            conversation.gone = true; conversation.readError = failure
                            await refreshChatsQuietly(); return
                        }
                        if case .resourceLimit = failure {
                            // Already replaying bounded: read the events instead of the snapshot for this connection.
                            if !conversation.recoverResourceLimit(failure, connection: connection) { conversation.snapshotUnavailableGeneration = connection }
                            continue
                        }
                        conversation.readError = failure
                        await noteLinkLossIfNeeded()
                        try? await Task.sleep(for: backoff.failure())
                        continue
                    }
                }
            }
            let connection = generation
            let canWait = waitSlots.canWait(at: ProcessInfo.processInfo.systemUptime)
            let since = conversation.feed.next, pin = conversation.feed.historyCursor
            let request: ChatEventsRequest
            do { request = try ChatEventsRequest(chatID: id, since: since, waitMilliseconds: canWait ? chatWaitMilliseconds : 0, maxEvents: conversation.oversizeCap ?? (conversation.feed.historyCursor == nil && !conversation.feed.degradedReplay ? nil : ChatConversation.boundedPage), complete: conversation.feed.historyCursor != nil, bounded: conversation.feed.degradedReplay) } catch { return }
            do {
                let reply = try await chatEventsFlight(request)
                guard !Task.isCancelled, conversation.follower == token, generation == connection,
                      conversation.feed.next == since, conversation.feed.historyCursor == pin else { continue }
                if let cursor = conversation.feed.historyCursor {
                    var ids = Set<String>()
                    for envelope in reply.events where envelope.seq > conversation.feed.next {
                        switch envelope.event {
                        case .itemDelta(let id, _): if !conversation.feed.hasItem(id) { ids.insert(id) }
                        case .itemStarted(let item), .itemCompleted(let item): if !conversation.feed.hasItem(item.id) { ids.insert(item.id) }
                        // An elided item keeps an older row's place: that row is hydrated first, as for any other change to it.
                        case .elided(let elided): if elided.isItem, let id = elided.itemID, !conversation.feed.hasItem(id) { ids.insert(id) }
                        default: break
                        }
                    }
                    let missing = Array(ids).sorted()
                    for start in stride(from: 0, to: missing.count, by: 100) {
                        let base = try await client.chatSnapshot(chatID: id, cursor: cursor, itemIDs: Array(missing[start..<min(start + 100, missing.count)]))
                        guard !Task.isCancelled, conversation.follower == token, generation == connection,
                              conversation.feed.next == since, conversation.feed.historyCursor == pin else { continue following }
                        conversation.hydrate(base)
                    }
                }
                guard !Task.isCancelled, conversation.follower == token, generation == connection,
                      conversation.feed.next == since, conversation.feed.historyCursor == pin else { continue }
                conversation.readError = nil
                backoff.success()
                if !reply.events.isEmpty { conversation.oversizeCap = nil }
                conversation.accept(reply, since: since)
                if !reply.more {
                    // The older desktop's history is in: the note about loading it has done its work.
                    if conversation.legacyLoading, conversation.alerts.text(.desktop)?.hasPrefix("Update RiWork") == true { conversation.alerts.clear(.desktop) }
                    conversation.legacyLoading = false
                }
                // A chat that went on with the other provider: its tab shows the new one's mark and name at once.
                if let info = conversation.transcript.info, let at = chats.firstIndex(where: { $0.id == info.id }),
                   chats[at].provider != info.provider || chats[at].title != info.title { chats[at] = info }
                if let info = conversation.transcript.info {
                    prepareChatCatalogue(info)
                }
                if chatSupport == .unknown { chatSupport = .supported }
                // Cut short: the rest is already there. Not waiting for a slot: a short read that found nothing waits a moment.
                if reply.more { continue }
                if !canWait, reply.events.isEmpty { try? await Task.sleep(for: .milliseconds(300)) }
            } catch is CancellationError {
                // The screen went (or the chat was switched): not a failure.
                return
            } catch ChatControlError.notFound {
                guard generation == connection, conversation.follower == token else { continue }
                conversation.gone = true
                conversation.readError = .notFound(.events)
                await refreshChatsQuietly()
                return
            } catch ChatControlError.unsupported {
                guard generation == connection, conversation.follower == token else { continue }
                if conversation.feed.degradedReplay {
                    conversation.resourceBlockedGeneration = connection
                    conversation.readError = .failed("Update RiWork on the Mac to recover this large chat."); return
                }
                chatSupport = .unsupported
                return
            } catch {
                guard !Task.isCancelled, conversation.follower == token, generation == connection else { continue }
                guard conversation.feed.next == since, conversation.feed.historyCursor == pin else { continue }
                if case RemoteError.rpc(let code, _) = error, code == "snapshot_expired" { conversation.reset(); continue }
                if case ChatControlError.invalid(let reason) = error, reason.contains("cannot continue after") { conversation.reset(); continue }
                let failure = ChatControlError.from(error, operation: .events)
                if case .resourceLimit = failure {
                    // An older relay that cannot send a page even bounded: smaller pages, then past the one event it cannot send.
                    if !conversation.recoverResourceLimit(failure, connection: connection) { conversation.passOversized() }
                    continue
                }
                if conversation.feed.degradedReplay {
                    switch failure {
                    case .invalid, .unreadableReply:
                        conversation.resourceBlockedGeneration = connection; conversation.readError = failure; return
                    default: break
                    }
                }
                conversation.readError = failure
                await noteLinkLossIfNeeded()
                try? await Task.sleep(for: backoff.failure())
            }
        }
    }

    /// History has its own cancellation scope and never changes controls or the live cursor.
    func loadOlderChat(_ id: String, beforeInstall: @MainActor () async -> Void = {}) async {
        let conversation = conversation(id)
        guard chatFollowWanted, conversation.following, !conversation.historyLoading, conversation.feed.hasOlder,
              let cursor = conversation.feed.historyCursor else { return }
        let before = conversation.feed.before, connection = generation, follower = conversation.follower
        conversation.historyLoading = true; conversation.historyError = nil
        defer { conversation.historyLoading = false }
        do {
            let page = try await client.chatSnapshot(chatID: id, cursor: cursor, before: before)
            guard !Task.isCancelled, generation == connection, conversation.follower == follower, conversation.following else { return }
            await beforeInstall()
            guard !Task.isCancelled, generation == connection, conversation.follower == follower, conversation.following else { return }
            conversation.prepend(page, before: before)
        } catch {
            guard !Task.isCancelled, generation == connection, conversation.follower == follower,
                  conversation.feed.historyCursor == cursor else { return }
            let failure = ChatControlError.from(error, operation: .events)
            if case .resourceLimit = failure, conversation.recoverResourceLimit(failure, connection: connection) {
                // The bounded replay takes over from the snapshot pages.
            } else if case RemoteError.rpc(let code, _) = error, code == "snapshot_expired" {
                conversation.reset(); conversation.alerts.show(.desktop, "History changed on the Mac. Loading the current recent messages again.", level: .info)
            } else { conversation.historyError = "Can’t load older messages. Try again." }
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

    /// Sends the draft as a message, with the paths of its cards after the text (`ChatAttachmentMessage`). The text and the cards leave the
    /// composer at once; if the message did not go through they come back, so nothing the person wrote or attached is lost, and what
    /// happened is said above the composer.
    @discardableResult
    func sendChatDraft(_ chatID: String) async -> ChatControlError? {
        let conversation = conversation(chatID)
        guard conversation.hasMessage else { return nil }
        // Files on their way go with the next message once the Mac has them, not before.
        guard pendingAttachments(for: chatID).isEmpty else { return .busy }
        // The text exactly as typed: the message is byte for byte what it was when paths went into the draft.
        return await sendChatMessage(chatID, conversation.draft, attachments: conversation.attachments, restoring: true)
    }
    /// Sends `text` as a message (also Retry on a failed chat, which sends the last message again), with the paths of `attachments`.
    @discardableResult
    func sendChatMessage(_ chatID: String, _ text: String, attachments: [StagedAttachment] = [], restoring: Bool = false) async -> ChatControlError? {
        let conversation = conversation(chatID)
        // One message at a time per chat, also across a conversation let go and made again while the first is on its way.
        guard !conversation.sending, !chatSendsInFlight.contains(chatID) else { return .busy }
        conversation.sending = true
        chatSendsInFlight.insert(chatID)
        // The text is held until the desktop answers: if the app ends before that, it comes back into the composer. The token ties the
        // answer to this send in the saved draft.
        let token = restoring ? chatDrafts.beginSending(text, attachments: attachments, for: chatID) : nil
        if restoring { conversation.draft = ""; conversation.attachments = [] }
        // The conversation the person sees now, if any: the one this send started from may have been let go and made again (or let
        // go with nothing in its place). A conversation let go is never written to: its text may be older than the saved draft.
        var live: ChatConversation? { chatConversations[chatID] }
        defer {
            conversation.sending = false; live?.sending = false
            chatSendsInFlight.remove(chatID)
        }
        let failure = await sendChatCommand(chatID, .send(text: ChatAttachmentMessage.compose(text, attachments)))
        if let failure {
            if let token {
                // The saved draft has whatever was typed since, in whichever composer: the message comes back before it there, and the
                // composer shown (if any) shows the same. A stale answer (another send owns the draft now) changes nothing.
                let uncertain = failure.outcomeIsUncertain
                if let back = chatDrafts.returnUnsent(text, attachments: attachments, token: token, uncertain: uncertain, for: chatID), let live {
                    live.draft = back.text
                    live.attachments = back.attachments
                }
                if uncertain {
                    // The Mac may have it (the link dropped, it timed out, or its answer could not be read): flagged in the saved draft
                    // until the person sends it again or empties the composer, across relaunches; never sent again by itself.
                    live?.alerts.show(.action, failure.message)
                    return failure
                }
            }
            (live ?? conversation).notice = failure.message
            return failure
        }
        if let token { chatDrafts.endSending(for: chatID, token: token) }
        // Sent: the cards' pictures are no longer needed (the files stay on the Mac, where the message names them).
        for card in attachments where !chatDrafts.attachmentIDs.contains(card.id) { chatAttachmentImages.remove(card.id) }
        live?.notice = nil
        live?.jumpToEnd()
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
    /// Closes a provider notice. It goes from this screen at once; a sticky one (a reached usage limit, a sign-in) is also dismissed on
    /// the host, so it stays closed on every device (`dismiss_notice`, docs/chat-notices.md). A refusal is said; the line stays closed here.
    @discardableResult
    func dismissChatNotice(_ chatID: String, _ notice: ChatProviderNotice) async -> ChatControlError? {
        let conversation = conversation(chatID)
        conversation.dismissedNotices.insert(notice.id)
        guard notice.isSticky else { return nil }
        let failure = await sendChatCommand(chatID, .dismissNotice(itemID: notice.id))
        if let failure { conversation.notice = failure.message }
        return failure
    }
    @discardableResult
    func compactChat(_ chatID: String) async -> ChatControlError? {
        let failure = await sendChatCommand(chatID, .compact)
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
