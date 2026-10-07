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
    /// A message sent and not yet answered by the desktop.
    public var sending: String?
    /// The text holds a message that may have reached the desktop already: said again on every restore until the person sends or
    /// empties the composer.
    public var uncertain: Bool
    /// When it last changed, so drafts of chats long gone can be let go.
    public var updatedAt: Date
    public init(text: String = "", sending: String? = nil, uncertain: Bool = false, updatedAt: Date = .now) {
        self.text = text; self.sending = sending; self.uncertain = uncertain; self.updatedAt = updatedAt
    }
    private enum Keys: String, CodingKey { case text, sending, uncertain, updatedAt }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
        sending = try c.decodeIfPresent(String.self, forKey: .sending)
        uncertain = try c.decodeIfPresent(Bool.self, forKey: .uncertain) ?? false
        updatedAt = try c.decodeIfPresent(Date.self, forKey: .updatedAt) ?? .now
    }
    public var isEmpty: Bool { text.isEmpty && (sending ?? "").isEmpty }

    /// What the composer shows when the chat is opened again after the app ended: the text, and before it a message whose sending was
    /// never answered (`uncertain`), which the person decides about.
    public var restored: (text: String, uncertain: Bool) {
        guard let sending, !sending.isEmpty else { return (text, uncertain) }
        return (text.isEmpty ? sending : sending + "\n" + text, true)
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

    /// The composer's text changed.
    public func setText(_ text: String, for chatID: String) {
        update(chatID) { $0.text = Self.bounded(text) }
    }
    /// A message leaves the composer: held until `endSending`. Sending is the person's decision about an uncertain one, too.
    public func beginSending(_ text: String, for chatID: String) {
        update(chatID) { $0.sending = Self.bounded(text); $0.uncertain = false }
    }
    /// A message never answered is back in the composer as `text`: an ordinary draft now, still flagged until sent or emptied.
    public func restoreUncertain(_ text: String, for chatID: String) {
        update(chatID) { $0.text = Self.bounded(text); $0.sending = nil; $0.uncertain = true }
    }
    /// The desktop has it, or it came back into the composer (which saved it as text): either way it is no longer on its way.
    public func endSending(for chatID: String) {
        update(chatID) { $0.sending = nil }
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
        if draft.text.isEmpty { draft.uncertain = false }
        guard draft.text != before.text || draft.sending != before.sending || draft.uncertain != before.uncertain else { return }
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
