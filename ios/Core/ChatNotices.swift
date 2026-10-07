import Foundation

// The messages a chat shows for a moment, in one place: a banner row directly above the composer. Two sources:
//
// - The provider's notices: `notice` items of the transcript (Claude's rate and usage limits, API retries, compaction, a model or
//   effort refused; Codex's errors, warnings, config warnings and "Reconnecting… n/5"). They are not transcript rows: each kind shows
//   once, its latest, and only while it belongs to the current turn (after the last thing the person sent), so a notice the provider
//   repeats every turn replaces itself instead of piling up. Every notice of the chat stays reachable in a history.
// - The phone's own: what went wrong sending, reading or connecting (`ChatAlert.Source`), one per source, replaced in place when it
//   recurs and taken away when its cause is resolved.
//
// Both can be dismissed with ×; a dismissed provider notice comes back only when the provider says it again (a new item).

/// A notice of the provider, as the banner and the history show it.
public struct ChatProviderNotice: Sendable, Equatable, Identifiable {
    /// The transcript item's id.
    public let id: String
    public let level: ChatNoticeLevel
    public let text: String
    /// What makes two notices "the same kind": the host's own kind when it sends one, else the text with its numbers taken out
    /// ("Reconnecting… 2/5" and "Reconnecting… 3/5" are one kind).
    public let kind: String
    /// How many times this kind was said in the chat, this one included.
    public var count: Int

    public static func kind(of text: String) -> String {
        let folded = text.lowercased().unicodeScalars.map { CharacterSet.decimalDigits.contains($0) ? "#" : String($0) }.joined()
        let squeezed = folded.replacingOccurrences(of: "#+", with: "#", options: .regularExpression)
        return String(squeezed.trimmingCharacters(in: .whitespacesAndNewlines).prefix(80))
    }
}

public enum ChatNotices {
    /// Every provider notice of the transcript, oldest first, each with how many of its kind came before it (itself included).
    public static func all(_ items: [ChatItem]) -> [ChatProviderNotice] {
        var counts: [String: Int] = [:]
        return items.compactMap { item in
            guard case .notice(let level, let text) = item.body else { return nil }
            let kind = ChatProviderNotice.kind(of: text)
            counts[kind, default: 0] += 1
            return ChatProviderNotice(id: item.id, level: level, text: text, kind: kind, count: counts[kind]!)
        }
    }
    /// What the banner shows: the latest notice of each kind said since the person last sent something (all of them when nothing was
    /// sent yet), not dismissed, most severe first and then newest first.
    public static func current(_ items: [ChatItem], dismissed: Set<String>) -> [ChatProviderNotice] {
        let start = items.lastIndex { if case .userMessage = $0.body { true } else { false } }.map { $0 + 1 } ?? 0
        let all = all(items)
        let inTurn = Set(items[start...].map(\.id))
        var latest: [String: (order: Int, notice: ChatProviderNotice)] = [:]
        for (order, notice) in all.enumerated() where inTurn.contains(notice.id) { latest[notice.kind] = (order, notice) }
        return latest.values.filter { !dismissed.contains($0.notice.id) }
            .sorted { ($0.notice.level.rank, $0.order) > ($1.notice.level.rank, $1.order) }.map(\.notice)
    }
    /// Whether a transcript row is drawn for the item: notices are the banner's and the history's, not the transcript's.
    public static func isTranscriptRow(_ item: ChatItem) -> Bool {
        if case .notice = item.body { return false }
        return true
    }
}

extension ChatNoticeLevel {
    /// Error above warning above info.
    public var rank: Int {
        switch self {
        case .info: 0
        case .warning: 1
        case .error: 2
        }
    }
}

/// One of the phone's own messages for a chat (or a terminal tab): where it came from, so a second one from the same place replaces
/// the first and resolving the cause takes it away.
public struct ChatAlert: Sendable, Equatable, Identifiable {
    public enum Source: String, Sendable, Hashable, CaseIterable {
        /// Sending, answering or a control (mode, model, compact) went wrong; said once per attempt, replaced by the next.
        case action
        /// The transcript could not be read; gone when a read succeeds.
        case read
        /// The link is down; gone when it is back.
        case link
        /// The desktop is too old for something (recent-first loading); gone with the conversation.
        case desktop
    }
    public var id: Source { source }
    public let source: Source
    public var level: ChatNoticeLevel
    public var text: String
    /// How many times it was said in a row (shown as "×3" when more than once).
    public var repeats: Int
    public init(source: Source, level: ChatNoticeLevel, text: String, repeats: Int = 1) {
        self.source = source; self.level = level; self.text = text; self.repeats = repeats
    }
}

/// The phone's own messages for one chat: at most one per source.
public struct ChatAlerts: Sendable, Equatable {
    public private(set) var items: [ChatAlert] = []
    public init() {}
    /// Says `text`; the same source says it in place of what it said before (a repeat of the same text counts up).
    public mutating func show(_ source: ChatAlert.Source, _ text: String, level: ChatNoticeLevel = .warning) {
        if let index = items.firstIndex(where: { $0.source == source }) {
            let repeats = items[index].text == text ? items[index].repeats + 1 : 1
            items[index] = ChatAlert(source: source, level: level, text: text, repeats: repeats)
        } else {
            items.append(ChatAlert(source: source, level: level, text: text))
        }
    }
    /// The cause is resolved, or the person closed it.
    public mutating func clear(_ source: ChatAlert.Source) { items.removeAll { $0.source == source } }
    public func text(_ source: ChatAlert.Source) -> String? { items.first { $0.source == source }?.text }
    /// Most severe first.
    public var ordered: [ChatAlert] { items.sorted { $0.level.rank > $1.level.rank } }
}
