import Foundation

// The tab strip of a project, decided apart from drawing. An entry of `shells.list` / `orchestrators.list` is a terminal unless the
// desktop says it runs as a chat (`"mode": "chat"`, with the `chat_id` of that chat and its `provider`): the Mac can run an orchestrator
// as a chat instead of a terminal. A chat orchestrator opens the chat screen for its `chat_id` (never for its own `id`, which may differ
// from it), a terminal one opens a terminal exactly as before, and a chat orchestrator on a desktop that has no chats opens neither.
//
// Everything here is pure and `Sendable`; the model asks it what each entry means and what the strip holds.

/// How the desktop runs a terminal-list entry.
public enum SessionMode: String, Codable, Sendable, Hashable {
    case terminal, chat

    /// The word the desktop sent. An absent mode, another type or a word the phone does not know is a terminal: what every entry was
    /// before the field existed.
    public init(wire value: JSONValue?) {
        let word = value?.string?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        self = word == "chat" ? .chat : .terminal
    }
}

/// What opening one entry means.
public enum SessionOpening: Sendable, Hashable {
    /// The terminal screen (`shell.output` of the entry's `id`).
    case terminal
    /// The chat screen (`chat.events` and `chat.command` of this chat id).
    case chat(String)
    /// It runs as a chat and the desktop has none to offer this phone (an older RiWork): there is nothing to open.
    case needsUpdate
    /// It runs as a chat but did not say which one, so there is nothing to open yet.
    case notReady

    /// What the screen says in place of a terminal or a chat; nil for the two that open.
    public var notice: (headline: String, detail: String)? {
        switch self {
        case .terminal, .chat: nil
        case .needsUpdate: ("Update the Mac to open this orchestrator", "It runs as a chat, which the RiWork on your Mac does not offer yet.")
        case .notReady: ("This orchestrator’s chat isn’t ready", "Refresh the tabs in a moment.")
        }
    }
}

extension RemoteSession {
    /// What opening this entry means. `chatsAvailable` is whether the desktop has chats (`features.chat`, and has not refused them).
    /// A terminal is a terminal whatever else it carries; a chat is never tried as a terminal, so an entry that is a chat and cannot be
    /// opened as one opens nothing.
    public func opening(chatsAvailable: Bool) -> SessionOpening {
        guard mode == .chat else { return .terminal }
        guard chatsAvailable else { return .needsUpdate }
        guard let chat_id else { return .notReady }
        return .chat(chat_id)
    }

    /// The chat this entry is, as far as the entry itself says: its id, agent, place and, from what it is doing, its state. The chat
    /// screen reads the rest (`info` event) once it follows the chat; until then the tab draws from this (an entry that names no agent
    /// draws as Claude until the chat says which it is). The title is the entry's own ("Project orchestrator"), because that is what
    /// the tab is called.
    func chatInfo(chatID: String, listed: ChatInfo?) -> ChatInfo {
        var info = listed ?? ChatInfo(id: chatID, provider: provider ?? .claude, projectID: project_id, worktreeID: worktree_id, cwd: cwd,
                                      createdAtUnix: created_at_unix, state: Self.chatState(activity))
        info.title = title
        return info
    }
    /// A chat's state from what an entry says its agent is doing: busy, waiting for a person, or at rest.
    static func chatState(_ activity: AgentActivity) -> ChatState {
        switch activity {
        case .working: .running
        case .waiting: .waiting
        case .done, .unknown, .exited: .idle
        }
    }
}

/// One cell of the strip.
public enum ProjectTab: Sendable, Equatable, Identifiable {
    /// A shell, an agent or an orchestrator that runs in a terminal.
    case terminal(RemoteSession)
    /// A chat of the project (`chats.list`).
    case chat(ChatInfo)
    /// An orchestrator that runs as a chat. The chat is the listed one when `chats.list` has it, otherwise made from the entry; its
    /// title is the entry's.
    case orchestratorChat(RemoteSession, ChatInfo)
    /// An orchestrator that runs as a chat and cannot be opened (`SessionOpening.needsUpdate` or `.notReady`).
    case unavailable(RemoteSession, SessionOpening)

    /// Unique in a strip: a terminal's or a blocked entry's own id, a chat's chat id.
    public var id: String {
        switch self {
        case .terminal(let session), .unavailable(let session, _): session.id
        case .chat(let info), .orchestratorChat(_, let info): info.id
        }
    }
    /// The chat the tab opens, if it opens one.
    public var chatInfo: ChatInfo? {
        switch self {
        case .chat(let info), .orchestratorChat(_, let info): info
        case .terminal, .unavailable: nil
        }
    }
    /// The terminal-list entry behind the tab, if there is one.
    public var session: RemoteSession? {
        switch self {
        case .terminal(let session), .orchestratorChat(let session, _), .unavailable(let session, _): session
        case .chat: nil
        }
    }
}

public enum ProjectTabs {
    /// The strip, left to right: the entries in the order they were given (the model puts orchestrators first), each a terminal, a chat
    /// or a notice by what `opening` says, and then the project's other chats, newest first.
    /// - A terminal that is not alive, or that the desktop no longer has (`missing`), is not a tab. A chat is not held to that: a chat
    ///   at rest is still a chat, and the next message starts its agent again.
    /// - A chat orchestrator whose chat `chats.list` also lists is one tab, not two.
    public static func tabs(sessions: [RemoteSession], chats: [ChatInfo], chatsAvailable: Bool, missing: Set<String> = []) -> [ProjectTab] {
        let listed = Dictionary(chats.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        var owned = Set<String>()
        var tabs: [ProjectTab] = []
        for session in sessions {
            let opening = session.opening(chatsAvailable: chatsAvailable)
            switch opening {
            case .terminal:
                if session.alive, !missing.contains(session.id) { tabs.append(.terminal(session)) }
            case .chat(let chatID):
                // Two entries for one chat would be two tabs with the same identity.
                guard owned.insert(chatID).inserted else { continue }
                tabs.append(.orchestratorChat(session, session.chatInfo(chatID: chatID, listed: listed[chatID])))
            case .needsUpdate, .notReady:
                tabs.append(.unavailable(session, opening))
            }
        }
        return tabs + ChatTabs.ordered(chats).filter { !owned.contains($0.id) }.map(ProjectTab.chat)
    }
}
