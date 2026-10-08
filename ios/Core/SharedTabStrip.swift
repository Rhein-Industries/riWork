import Foundation

// What the phone's tab row does with the desktop's shared tab list (`SharedTabsReply`, docs/shared-tabs.md), decided apart from
// drawing: which entry a tab is, where a dragged tab may go, what closing does, what the "Open shell/worker…" picker lists, and the
// groups of the Edit tabs sheet. The desktop's list is the truth: nothing here changes it, every change is one `TabUpdate` whose
// reply is installed as it comes.

public enum SharedTabStrip {
    /// The entry of a tab of the row: a chat's (by its chat id) or a shell's (by its session id).
    public static func entry(chatID: String?, sessionID: String?, in reply: SharedTabsReply?) -> SharedTab? {
        guard let reply else { return nil }
        let all = reply.allEntries
        if let chatID, let found = all.first(where: { $0.kind == .chat && $0.sessionID == chatID }) { return found }
        if let sessionID, let found = all.first(where: { $0.sessionID == sessionID }) { return found }
        return nil
    }

    /// Tabs that may be moved past each other: the same parent and the same pin group (docs/shared-tabs.md, Move).
    public static func sameGroup(_ a: SharedTab, _ b: SharedTab) -> Bool { a.parent == b.parent && a.pinned == b.pinned }

    /// The move for dropping `dragged` on `target` (it goes before it), or at the end of its group when `target` is nil. Nil when the
    /// drop says nothing (on itself, or where it already is) or is not allowed (another group: the desktop would refuse it).
    public static func move(_ dragged: SharedTab, onto target: SharedTab?, in reply: SharedTabsReply) -> TabUpdate? {
        // The whole group in shared order, hidden members included: a move is in that order, and showing one later must not reveal a
        // different order than the one the person made.
        let group = reply.allEntries.filter { sameGroup($0, dragged) }.sorted { $0.order < $1.order }
        guard let from = group.firstIndex(where: { $0.key == dragged.key }) else { return nil }
        guard let target else { return from == group.count - 1 ? nil : .move(dragged.key, before: nil) }
        guard target.key != dragged.key, sameGroup(target, dragged), let to = group.firstIndex(where: { $0.key == target.key }) else { return nil }
        if to == from + 1 { return nil }
        return .move(dragged.key, before: target.key)
    }

    /// Move left / Move right, as the Mac's tab menu has them: one place among the visible tabs of its sibling and pin group; nil at the
    /// group's boundary. Right goes before the tab after the next one in shared order (hidden members included), or to the group's end.
    public static func moveLeft(_ tab: SharedTab, in reply: SharedTabsReply) -> TabUpdate? {
        let visible = reply.visible.filter { sameGroup($0, tab) }
        guard let index = visible.firstIndex(where: { $0.key == tab.key }), index > 0 else { return nil }
        return .move(tab.key, before: visible[index - 1].key)
    }
    public static func moveRight(_ tab: SharedTab, in reply: SharedTabsReply) -> TabUpdate? {
        let visible = reply.visible.filter { sameGroup($0, tab) }
        guard let index = visible.firstIndex(where: { $0.key == tab.key }), index + 1 < visible.count else { return nil }
        let next = visible[index + 1]
        let group = reply.allEntries.filter { sameGroup($0, tab) }.sorted { $0.order < $1.order }
        guard let at = group.firstIndex(where: { $0.key == next.key }) else { return nil }
        let after = group[(at + 1)...].first { $0.key != tab.key }
        return .move(tab.key, before: after?.key)
    }

    /// The move for a list reorder of one group (the Edit tabs sheet's `onMove`): `group` in shown order, the row at `source` dropped at
    /// `destination` (SwiftUI's offset, past the moved row's old place).
    public static func move(in group: [SharedTab], from source: Int, to destination: Int) -> TabUpdate? {
        guard group.indices.contains(source), destination >= 0, destination <= group.count, destination != source, destination != source + 1 else { return nil }
        let before = destination < group.count ? group[destination].key : nil
        return .move(group[source].key, before: before)
    }

    /// The groups of the Edit tabs sheet, in the row's order: the pinned tabs, then the other top-level tabs, then the opened children
    /// of each parent; each can only be reordered within itself.
    public struct Group: Sendable, Equatable, Identifiable {
        public let parent: String?
        public let pinned: Bool
        public let title: String
        public let tabs: [SharedTab]
        public var id: String { "\(parent ?? "root"):\(pinned)" }
    }
    public static func groups(_ reply: SharedTabsReply) -> [Group] {
        // Kinds this phone does not know are ignored (the contract), so never reordered from here.
        let visible = reply.visible.filter { $0.kind != .unknown }
        let titles = Dictionary(reply.allEntries.map { ($0.key, $0.title) }, uniquingKeysWith: { first, _ in first })
        var groups: [Group] = []
        var seen: [String: Int] = [:]
        for tab in visible {
            let id = "\(tab.parent ?? "root"):\(tab.pinned)"
            if let index = seen[id] {
                groups[index] = Group(parent: groups[index].parent, pinned: groups[index].pinned, title: groups[index].title, tabs: groups[index].tabs + [tab])
            } else {
                let title = tab.parent.map { "In \(titles[$0] ?? "another tab")" } ?? (tab.pinned ? "Pinned" : "Tabs")
                seen[id] = groups.count
                groups.append(Group(parent: tab.parent, pinned: tab.pinned, title: title, tabs: [tab]))
            }
        }
        // Pinned first, then the other top-level tabs, then the children's groups in the order they first appear.
        return groups.sorted { rank($0) < rank($1) }
    }
    private static func rank(_ group: Group) -> Int { group.parent != nil ? 2 : (group.pinned ? 0 : 1) }

    /// One row of the "Open shell/worker…" picker: a hidden chat or shell that can be opened, and the tab it belongs to.
    public struct Openable: Sendable, Equatable, Identifiable {
        public let tab: SharedTab
        /// The parent's title, when it has one the list knows.
        public let parentTitle: String?
        public var id: String { tab.key }
    }
    /// What the picker lists: hidden chats and shells (a stopped shell cannot be opened; a stopped chat keeps its history), workers
    /// first, then by the shared order.
    public static func openable(_ reply: SharedTabsReply?) -> [Openable] {
        guard let reply else { return [] }
        let all = reply.allEntries
        let titles = Dictionary(all.map { ($0.key, $0.title) }, uniquingKeysWith: { first, _ in first })
        return all.filter { $0.hidden && $0.kind != .unknown && !($0.kind == .shell && $0.status == .stopped) }
            .sorted { ($0.isWorker ? 0 : 1, $0.order) < ($1.isWorker ? 0 : 1, $1.order) }
            .map { Openable(tab: $0, parentTitle: $0.parent.flatMap { titles[$0] }) }
    }

    /// What closing a tab does now, by the device's setting (`tab_close_behavior`) and what the tab is.
    public enum ClosePlan: Sendable, Equatable {
        /// A pinned tab is not closed: it must be unpinned first (the desktop refuses to hide a pinned one).
        case unpinFirst
        /// Ask: the sheet with Detach, Exit and Cancel; nothing changes until a choice.
        case ask
        /// Hide it; its process carries on (always, for a worker).
        case detach
        /// Hide it, then stop the chat or close the shell; a chat's history stays.
        case exit
    }
    public static func closePlan(_ tab: SharedTab, setting: TabCloseBehavior) -> ClosePlan {
        if tab.pinned { return .unpinFirst }
        switch setting.effectiveChoice(for: tab) {
        case .ask: return .ask
        case .detach: return .detach
        case .exit: return .exit
        }
    }

    /// What the long-press menu offers for a tab.
    public struct Actions: Sendable, Equatable {
        public let pin: Bool, unpin: Bool, rename: Bool, close: Bool
    }
    public static func actions(_ tab: SharedTab) -> Actions {
        // Only top-level tabs may be pinned; a pinned tab is unpinned before it can be closed.
        Actions(pin: !tab.pinned && tab.parent == nil, unpin: tab.pinned, rename: tab.kind != .unknown, close: !tab.pinned)
    }
}
