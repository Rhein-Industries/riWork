import Foundation

// The messages a chat shows for a moment, in one place: a banner row directly above the composer. Two sources:
//
// - The provider's notices: `notice` items of the transcript (Claude's rate and usage limits, API retries, compaction, a model or
//   effort refused; Codex's errors, warnings, config warnings and "Reconnecting… n/5"). They are not transcript rows: each kind shows
//   once, its latest, and only while it belongs to the current turn (after the last thing the person sent), so a notice the provider
//   repeats every turn replaces itself instead of piling up. A reached usage limit and a sign-in (`rate_limit:*` at error level,
//   `auth_required`) are sticky: they stay, whatever turn they came in, until resolved, reset or dismissed; a usage warning is never a
//   banner (the chat's usage chip says it). Every notice of the chat stays reachable in a history (docs/chat-notices.md).
// - The phone's own: what went wrong sending, reading or connecting (`ChatAlert.Source`), one per source, replaced in place when it
//   recurs and taken away when its cause is resolved.
//
// Both can be dismissed with ×; a dismissed provider notice comes back only when the provider says it again (a new item). Closing a
// sticky one also tells the host (`dismiss_notice`), which keeps it closed on every device and marks the item `dismissed`.

/// A notice of the provider, as the banner and the history show it.
public struct ChatProviderNotice: Sendable, Equatable, Identifiable {
    /// The transcript item's id.
    public let id: String
    public let level: ChatNoticeLevel
    public let text: String
    /// What makes two notices "the same kind": the host's `kind` when it sends one ("rate_limit:seven_day", "reconnecting", …, the
    /// same key the Mac groups by), else the notice's own text, so a notice without a kind is a line of its own.
    public let kind: String
    /// How many times this kind was said in the chat, this one included.
    public var count: Int
    /// The host's own `kind`, when it sent one.
    public var hostKind: String? = nil
    public var resolved = false
    /// When the limit it is about resets (Unix seconds).
    public var resetsAt: UInt64? = nil
    /// The host dismissed it (on any device).
    public var dismissed = false

    /// A reached usage limit or a sign-in: it stays until resolved or dismissed, and its × is the host's dismissal.
    public var isSticky: Bool { ChatNotices.isSticky(hostKind) }
    /// A usage limit: its banner says when it resets and offers the usage detail.
    public var isUsageLimit: Bool { hostKind?.hasPrefix("rate_limit:") == true }
    /// The text with the reset time of a usage limit ("… · resets Thu 14:00").
    public func bannerText(calendar: Calendar = .current, locale: Locale = .current) -> String {
        guard isUsageLimit, let resetsAt else { return text }
        return text + " · resets " + ChatUsageLimits.resetTime(resetsAt, calendar: calendar, locale: locale)
    }

    /// The grouping key: `kind:<host kind>`, or `id:<item id>` for a notice without one: each is its own banner (an unknown kind is
    /// still the host's word for it, and is used as it is).
    public static func key(kind: String?, id: String) -> String {
        if let kind { return "kind:" + kind }
        return "id:" + id
    }
}

public enum ChatNotices {
    /// Every provider notice of the transcript, oldest first, each with how many of its kind came before it (itself included).
    public static func all(_ items: [ChatItem]) -> [ChatProviderNotice] {
        var counts: [String: Int] = [:]
        return items.compactMap { item in
            guard case .notice(let level, let text, let hostKind, let resolved, let resetsAt, let dismissed) = item.body else { return nil }
            let kind = ChatProviderNotice.key(kind: hostKind, id: item.id)
            counts[kind, default: 0] += 1
            return ChatProviderNotice(id: item.id, level: level, text: text, kind: kind, count: counts[kind]!, hostKind: hostKind, resolved: resolved,
                                      resetsAt: resetsAt, dismissed: dismissed)
        }
    }
    /// `rate_limit:*` (any window, known or not) and `auth_required`.
    public static func isSticky(_ kind: String?) -> Bool { kind.map { $0.hasPrefix("rate_limit:") || $0 == "auth_required" } ?? false }
    /// Whether one of the host's dismissal keys (`<provider>:<account>|<kind>@<resets>` or `…|<kind>#<item id>`) covers the notice.
    public static func hostDismissed(_ notice: ChatProviderNotice, keys: Set<String>) -> Bool {
        guard let kind = notice.hostKind, !keys.isEmpty else { return false }
        let suffix = notice.resetsAt.map { "|\(kind)@\($0)" } ?? "|\(kind)#\(notice.id)"
        return keys.contains { $0.hasSuffix(suffix) }
    }
    /// What the banner shows: for each kind (or each notice without one) its newest notice, when it is not resolved, its reset (if any)
    /// is still ahead, nobody dismissed it (here: `dismissed`, by item id; on any device: the item's flag or the snapshot's `hostKeys`),
    /// and it is live: said since the person last sent something (everything when nothing was sent yet), or sticky. A usage notice
    /// below error level (a warning from an older log) is never a banner. Most severe first, then newest first.
    public static func current(_ items: [ChatItem], dismissed: Set<String>, hostKeys: Set<String> = [], now: Date = .now) -> [ChatProviderNotice] {
        let start = items.lastIndex { if case .userMessage = $0.body { true } else { false } }.map { $0 + 1 } ?? 0
        let position = Dictionary(items.enumerated().map { ($0.element.id, $0.offset) }, uniquingKeysWith: { first, _ in first })
        var latest: [String: (order: Int, notice: ChatProviderNotice)] = [:]
        for notice in all(items) { latest[notice.kind] = (position[notice.id] ?? 0, notice) }
        return latest.values.filter { order, notice in
            guard !notice.resolved, !notice.dismissed, !dismissed.contains(notice.id), !hostDismissed(notice, keys: hostKeys) else { return false }
            if let resetsAt = notice.resetsAt, TimeInterval(resetsAt) <= now.timeIntervalSince1970 { return false }
            if notice.isUsageLimit, notice.level != .error { return false }
            return order >= start || notice.isSticky
        }
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
