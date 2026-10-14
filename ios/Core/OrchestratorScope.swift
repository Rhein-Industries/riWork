import Foundation

// Where an orchestrator belongs. `orchestrators.list` gives every orchestrator of the desktop: a project's (`project_id` its project) and
// the global one (`project_id` null), which belongs to no project. The global orchestrator, whether it runs in a terminal or as a chat, is
// shown on the dashboard's Global screen only: it is never a tab of a project (the earlier strip or the shared one), never one of a
// project's chats or open terminals, never counted for a project, and never chosen, restored or remembered as a project's tab. A project's
// orchestrator is a tab of its own project only. Everything here is pure and `Sendable`; the model asks it what each scope holds.

public enum OrchestratorScope {
    /// The global orchestrator: an orchestrator with no project.
    public static func isGlobal(_ session: RemoteSession) -> Bool { session.kind == "orchestrator" && session.project_id == nil }

    /// The global orchestrators the desktop listed, newest first (there is normally one).
    public static func globalEntries(_ orchestrators: [RemoteSession]) -> [RemoteSession] {
        orchestrators.filter(isGlobal).sorted { $0.created_at_unix > $1.created_at_unix }
    }

    /// Every id that names a global orchestrator: its entry id and, when it runs as a chat, its chat id. A project's chat list or shared
    /// tab list that names one (a desktop should not) has it taken out.
    public static func globalIDs(_ orchestrators: [RemoteSession]) -> Set<String> {
        var ids = Set<String>()
        for entry in orchestrators where isGlobal(entry) {
            ids.insert(entry.id)
            if let chat = entry.chat_id { ids.insert(chat) }
        }
        return ids
    }

    /// A project's terminal-list entries, in the strip's order: its own orchestrator first, then its shells, newest first within each.
    /// Another project's orchestrator and the global one are not among them; neither is anything without a project when none is chosen.
    public static func projectEntries(orchestrators: [RemoteSession], shells: [RemoteSession], project: String?) -> [RemoteSession] {
        let own = orchestrators.filter { $0.kind == "orchestrator" && $0.project_id != nil && $0.project_id == project }
        let global = globalIDs(orchestrators)
        return (own + shells.filter { !isGlobal($0) && !global.contains($0.id) }).sorted {
            if $0.kind != $1.kind { return $0.kind == "orchestrator" }
            return $0.created_at_unix > $1.created_at_unix
        }
    }

    /// A project's chats (`chats.list`) without any that is a global orchestrator's chat.
    public static func projectChats(_ chats: [ChatInfo], orchestrators: [RemoteSession]) -> [ChatInfo] {
        let global = globalIDs(orchestrators)
        guard !global.isEmpty else { return chats }
        return chats.filter { !global.contains($0.id) }
    }

    /// A project's strip: the earlier one (every terminal and chat of the project) or, with the desktop's shared list, its visible entries
    /// in its order and with its titles. The global orchestrator is never in it, whatever either list says.
    /// - `missing`: terminals the desktop no longer has.
    /// - `knownInfo`: what the phone knows of a chat besides `chats` (its live reader's info), for a shared chat tab whose `chats.list`
    ///   read failed: membership and names come from the shared list, so the chat stays in the strip.
    public static func projectTabs(orchestrators: [RemoteSession], shells: [RemoteSession], chats: [ChatInfo], shared: SharedTabsReply?,
                                   project: String?, chatsAvailable: Bool, missing: Set<String> = [],
                                   knownInfo: (String) -> ChatInfo? = { _ in nil }) -> [ProjectTab] {
        let entries = projectEntries(orchestrators: orchestrators, shells: shells, project: project)
        let legacy = ProjectTabs.tabs(sessions: entries, chats: projectChats(chats, orchestrators: orchestrators), chatsAvailable: chatsAvailable, missing: missing)
        guard let shared else { return legacy }
        let global = globalIDs(orchestrators)
        // Chat and shell identities are kept apart (`ProjectTab.key`), even when their ids coincide.
        var listed = Dictionary(legacy.map { ($0.key, $0) }, uniquingKeysWith: { first, _ in first })
        // An older desktop can share an orchestrator's chat tab while refusing chat reads. Keep its update notice in the strip.
        for case .unavailable(let session, let opening) in legacy {
            if let id = session.chat_id { listed["chat:\(id)"] = .unavailable(session, opening) }
        }
        return shared.visible.compactMap { entry in
            guard !global.contains(entry.sessionID) else { return nil }
            if entry.kind == .shell, var session = entries.first(where: { $0.id == entry.sessionID && $0.alive && $0.mode != .chat && !missing.contains($0.id) }) {
                session.sharedTitle = entry.title; return .terminal(session)
            }
            guard entry.kind != .unknown else { return nil }
            guard let tab = listed[entry.key] else {
                // Membership and names come from tabs.list. A separate chats.list failure must not erase its visible chats.
                guard entry.kind == .chat, chatsAvailable else { return nil }
                var info = knownInfo(entry.sessionID) ?? ChatInfo(id: entry.sessionID, provider: .codex, projectID: project, state: chatState(entry.status))
                info.title = entry.title; info.sharedTitle = entry.title
                return .chat(info)
            }
            switch tab {
            case .chat(var info): info.title = entry.title; info.sharedTitle = entry.title; return .chat(info)
            case .terminal(var session): session.sharedTitle = entry.title; return .terminal(session)
            case .orchestratorChat(var session, var info):
                session.sharedTitle = entry.title; info.title = entry.title; info.sharedTitle = entry.title; return .orchestratorChat(session, info)
            case .unavailable(var session, let opening): session.sharedTitle = entry.title; return .unavailable(session, opening)
            }
        }
    }
    /// A chat's state from what its shared tab says, for a chat the phone has no metadata for.
    static func chatState(_ status: SharedTab.Status) -> ChatState {
        switch status {
        case .working: .running
        case .waiting: .waiting
        case .error: .failed("")
        case .stopped: .stopped
        case .done, .idle, .unknown: .idle
        }
    }

    /// A project's open terminals (what runs in a terminal and is alive): with the shared list, its visible shells in its order; without
    /// it, every live terminal entry of the project. Never the global orchestrator.
    public static func projectOpenSessions(orchestrators: [RemoteSession], shells: [RemoteSession], shared: SharedTabsReply?, project: String?,
                                           missing: Set<String> = []) -> [RemoteSession] {
        let entries = projectEntries(orchestrators: orchestrators, shells: shells, project: project)
        let open: (RemoteSession) -> Bool = { $0.mode != .chat && $0.alive && !missing.contains($0.id) }
        guard let shared else { return entries.filter(open) }
        return shared.allEntries.filter { $0.kind == .shell && !$0.hidden }.compactMap { entry in entries.first { $0.id == entry.sessionID && open($0) } }
    }

    /// A project's hidden tabs that can be opened ("Open shell/worker…" and its count), without any that names a global orchestrator.
    public static func projectOpenable(_ shared: SharedTabsReply?, orchestrators: [RemoteSession]) -> [SharedTabStrip.Openable] {
        let global = globalIDs(orchestrators)
        return SharedTabStrip.openable(shared).filter { !global.contains($0.tab.sessionID) }
    }

    /// The Edit tabs sheet's groups of a project, without any tab that names a global orchestrator (a group left empty goes).
    public static func projectGroups(_ shared: SharedTabsReply, orchestrators: [RemoteSession]) -> [SharedTabStrip.Group] {
        let global = globalIDs(orchestrators)
        return SharedTabStrip.groups(shared).compactMap { group in
            let tabs = group.tabs.filter { !global.contains($0.sessionID) }
            return tabs.isEmpty ? nil : SharedTabStrip.Group(parent: group.parent, title: group.title, tabs: tabs)
        }
    }

    /// The Global screen's tabs: each global orchestrator as a terminal, a chat (by its `chat_id`) or a notice, newest first. Nothing of a
    /// project is ever among them.
    public static func globalTabs(orchestrators: [RemoteSession], chatsAvailable: Bool, missing: Set<String> = []) -> [ProjectTab] {
        ProjectTabs.tabs(sessions: globalEntries(orchestrators), chats: [], chatsAvailable: chatsAvailable, missing: missing)
    }

    /// The global orchestrator's terminals that can be opened (alive, in a terminal, still on the desktop).
    public static func globalOpenSessions(orchestrators: [RemoteSession], missing: Set<String> = []) -> [RemoteSession] {
        globalEntries(orchestrators).filter { $0.mode != .chat && $0.alive && !missing.contains($0.id) }
    }

    /// The key a global orchestrator's tab is remembered by (`SavedDesktop.globalTabKey`): `shell:<id>` for one in a terminal,
    /// `chat:<chat id>` for one that runs as a chat, and `orchestrator:<id>` for one the phone cannot open (its notice), so that only a
    /// terminal is ever read as a terminal.
    public static func key(_ tab: ProjectTab) -> String {
        switch tab {
        case .terminal(let session): "shell:\(session.id)"
        case .chat(let info), .orchestratorChat(_, let info): "chat:\(info.id)"
        case .unavailable(let session, _): "orchestrator:\(session.id)"
        }
    }
    /// The id a remembered key names when it is of that kind (`"shell"`, `"chat"` or `"orchestrator"`).
    public static func id(in key: String?, kind: String) -> String? {
        guard let key, key.hasPrefix(kind + ":") else { return nil }
        let id = String(key.dropFirst(kind.count + 1))
        return id.isEmpty ? nil : id
    }

    /// Which global tab to show: the remembered one while it is still there, else the first. Nil when there is none.
    public static func restoredGlobalTab(remembered: String?, in tabs: [ProjectTab]) -> ProjectTab? {
        if let remembered, let tab = tabs.first(where: { key($0) == remembered }) { return tab }
        return tabs.first
    }
}
