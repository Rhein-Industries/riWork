import Foundation

// What a person typed into a chat's composer and has not sent, kept per chat across everything short of sending it: another tab,
// the projects list, the app in the background, a dropped link, a relaunch. The Mac's composer keeps the same promise (its editors
// live as long as the chat view, and a send keeps a snapshot of the draft until the desktop answers).
//
// A message on its way is held too (`sending`) until the desktop says it arrived: if the app ends before that, the text comes back
// into the composer with a warning, never sent again by itself (the Mac's "Outcome uncertain: inspect the transcript before
// explicitly resending").

/// One chat's unsent text.
public struct ChatDraft: Codable, Sendable, Equatable {
    /// What is in the composer.
    public var text: String
    /// The files staged for the message (`StagedAttachment`): cards above the composer.
    public var attachments: [StagedAttachment]
    /// A message sent and not yet answered by the desktop: its text, and the files that went with it.
    public var sending: String?
    public var sendingAttachments: [StagedAttachment]
    /// Which send `sending` belongs to: only that send's answer may end it or bring it back.
    public var sendToken: String?
    /// The text holds a message that may have reached the desktop already: said again on every restore until the person sends or
    /// empties the composer.
    public var uncertain: Bool
    /// When it last changed, so drafts of chats long gone can be let go.
    public var updatedAt: Date
    public init(text: String = "", attachments: [StagedAttachment] = [], sending: String? = nil, sendingAttachments: [StagedAttachment] = [],
                sendToken: String? = nil, uncertain: Bool = false, updatedAt: Date = .now) {
        self.text = text; self.attachments = attachments; self.sending = sending; self.sendingAttachments = sendingAttachments
        self.sendToken = sendToken; self.uncertain = uncertain; self.updatedAt = updatedAt
    }
    private enum Keys: String, CodingKey { case text, attachments, sending, sendingAttachments, sendToken, uncertain, updatedAt }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
        attachments = (try? c.decodeIfPresent([StagedAttachment].self, forKey: .attachments)) ?? []
        sending = try c.decodeIfPresent(String.self, forKey: .sending)
        sendingAttachments = (try? c.decodeIfPresent([StagedAttachment].self, forKey: .sendingAttachments)) ?? []
        sendToken = try c.decodeIfPresent(String.self, forKey: .sendToken)
        uncertain = try c.decodeIfPresent(Bool.self, forKey: .uncertain) ?? false
        updatedAt = try c.decodeIfPresent(Date.self, forKey: .updatedAt) ?? .now
    }
    public var isEmpty: Bool { text.isEmpty && attachments.isEmpty && (sending ?? "").isEmpty && sendingAttachments.isEmpty }
    /// Nothing in the composer: no text and no cards.
    var composerIsEmpty: Bool { text.isEmpty && attachments.isEmpty }

    /// What the composer shows when the chat is opened again after the app ended: the text and cards, and before them a message whose
    /// sending was never answered (`uncertain`), which the person decides about.
    public var restored: (text: String, attachments: [StagedAttachment], uncertain: Bool) {
        let sent = sending ?? ""
        guard !sent.isEmpty || !sendingAttachments.isEmpty else { return (text, attachments, uncertain) }
        return (sent.isEmpty ? text : text.isEmpty ? sent : sent + "\n" + text, Self.merged(sendingAttachments, attachments), true)
    }
    /// `first` then those of `then` not in it already.
    public static func merged(_ first: [StagedAttachment], _ then: [StagedAttachment]) -> [StagedAttachment] {
        let ids = Set(first.map(\.id))
        return first + then.filter { !ids.contains($0.id) }
    }
}

/// The drafts of all chats, by chat id (chat ids are UUIDs, unique across desktops), in `UserDefaults`. Writes are cheap: the defaults
/// keep them in memory and save in the background.
@MainActor public final class ChatDraftStore {
    public static let key = "chat.drafts.v1"
    /// Drafts kept at most; the oldest go first.
    public static let maximumDrafts = 64
    /// A draft not touched for this long belongs to a chat that is most likely gone.
    public static let maximumAge: TimeInterval = 30 * 24 * 3600
    /// The longest text kept, in UTF-8 bytes (a pasted log can be huge; the composer keeps all of it, the store a bounded part).
    public static let maximumBytes = 256 * 1024

    private let defaults: UserDefaults
    private var drafts: [String: ChatDraft]
    private let now: () -> Date

    public init(defaults: UserDefaults, now: @escaping () -> Date = { .now }) {
        self.defaults = defaults; self.now = now
        let saved = defaults.data(forKey: Self.key).flatMap { try? JSONDecoder().decode([String: ChatDraft].self, from: $0) } ?? [:]
        let cutoff = now().addingTimeInterval(-Self.maximumAge)
        drafts = saved.filter { !$0.value.isEmpty && $0.value.updatedAt >= cutoff }
        if drafts.count != saved.count { save() }
    }

    public func draft(_ chatID: String) -> ChatDraft? { drafts[chatID] }
    public var chatIDs: Set<String> { Set(drafts.keys) }
    /// The upload ids of every card any draft holds, staged or on its way.
    public var attachmentIDs: Set<String> { Set(drafts.values.flatMap { ($0.attachments + $0.sendingAttachments).map(\.id) }) }

    /// The composer's text changed.
    public func setText(_ text: String, for chatID: String) {
        update(chatID) { $0.text = Self.bounded(text) }
    }
    /// The cards above the composer changed.
    public func setAttachments(_ attachments: [StagedAttachment], for chatID: String) {
        update(chatID) { $0.attachments = attachments }
    }
    /// A message leaves the composer: held until its answer (`endSending` or `returnUnsent` with the token returned here). Sending is
    /// the person's decision about an uncertain one, too.
    @discardableResult
    public func beginSending(_ text: String, attachments: [StagedAttachment] = [], for chatID: String) -> String {
        let token = UUID().uuidString
        update(chatID) { $0.sending = Self.bounded(text); $0.sendingAttachments = attachments; $0.sendToken = token; $0.uncertain = false }
        return token
    }
    /// The desktop has the message: it is no longer on its way. Only the send `token` belongs to may say so.
    public func endSending(for chatID: String, token: String) {
        guard drafts[chatID]?.sendToken == token else { return }
        update(chatID) { $0.sending = nil; $0.sendingAttachments = []; $0.sendToken = nil }
    }
    /// The message did not go through (or may not have): it comes back before whatever is in the draft now (typed since, in whichever
    /// composer), its cards before the ones staged since, flagged when its outcome is unknown. Returns the draft's text and cards now,
    /// or nil when `token` is not the send in the draft (a stale answer), which changes nothing.
    @discardableResult
    public func returnUnsent(_ text: String, attachments: [StagedAttachment] = [], token: String, uncertain: Bool, for chatID: String) -> (text: String, attachments: [StagedAttachment])? {
        guard let draft = drafts[chatID], draft.sendToken == token else { return nil }
        let back = text.isEmpty ? draft.text : draft.text.isEmpty ? text : text + "\n" + draft.text
        let cards = ChatDraft.merged(attachments, draft.attachments)
        update(chatID) {
            $0.text = Self.bounded(back); $0.attachments = cards; $0.sending = nil; $0.sendingAttachments = []; $0.sendToken = nil
            $0.uncertain = uncertain || $0.uncertain
        }
        return (back, cards)
    }
    /// A message never answered is back in the composer as `text` and `attachments`: an ordinary draft now, still flagged until sent or
    /// emptied.
    public func restoreUncertain(_ text: String, attachments: [StagedAttachment] = [], for chatID: String) {
        update(chatID) { $0.text = Self.bounded(text); $0.attachments = attachments; $0.sending = nil; $0.sendingAttachments = []; $0.sendToken = nil; $0.uncertain = true }
    }
    /// No longer on its way, whichever send it was (a draft restored after a relaunch).
    public func endSending(for chatID: String) {
        update(chatID) { $0.sending = nil; $0.sendingAttachments = []; $0.sendToken = nil }
    }
    /// The person cleared it, or the chat is gone for good.
    public func remove(_ chatID: String) {
        guard drafts.removeValue(forKey: chatID) != nil else { return }
        save()
    }

    private func update(_ chatID: String, _ change: (inout ChatDraft) -> Void) {
        var draft = drafts[chatID] ?? ChatDraft()
        let before = draft
        change(&draft)
        if draft.composerIsEmpty { draft.uncertain = false }
        guard draft != before else { return }
        draft.updatedAt = now()
        if draft.isEmpty { drafts[chatID] = nil } else { drafts[chatID] = draft }
        if drafts.count > Self.maximumDrafts {
            for old in drafts.sorted(by: { $0.value.updatedAt < $1.value.updatedAt }).prefix(drafts.count - Self.maximumDrafts) where old.key != chatID {
                drafts[old.key] = nil
            }
        }
        save()
    }
    private func save() {
        if drafts.isEmpty { defaults.removeObject(forKey: Self.key) }
        else if let data = try? JSONEncoder().encode(drafts) { defaults.set(data, forKey: Self.key) }
    }
    /// The first `maximumBytes` of `text`, cut between characters.
    static func bounded(_ text: String) -> String {
        guard text.utf8.count > maximumBytes else { return text }
        var bytes = 0, end = text.startIndex
        for index in text.indices {
            let size = text[index].utf8.count
            guard bytes + size <= maximumBytes else { break }
            bytes += size; end = text.index(after: index)
        }
        return String(text[..<end])
    }
}
