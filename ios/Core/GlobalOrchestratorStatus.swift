import Foundation

// What the dashboard says about the global orchestrator, and what its button does. The entry of `orchestrators.list` (every desktop
// that lists orchestrators has it) says whether there is one to open and how it runs; the desktop's dashboard, when it sends one, adds
// what its chat is doing. Nothing is invented: a figure neither says is not shown. Pure and `Sendable`.

public struct GlobalOrchestratorStatus: Sendable, Equatable {
    public enum Action: Sendable, Equatable {
        /// There is one: open the Global screen (nothing is asked of the desktop).
        case open
        /// There is none: start it, on an explicit tap only (`orchestrator.create` with no project).
        case start
        /// There is none and this desktop cannot start one from the phone.
        case unavailable
    }
    /// How it runs, when known.
    public let mode: SessionMode?
    public let provider: ChatProvider?
    /// "Working", "Waiting for input", "Ready", "Failed", …; nil when nothing is known.
    public let state: String?
    /// What its agent is doing, for the activity mark.
    public let activity: AgentActivity
    /// "Claude chat · Working", "Runs in a terminal · Working", "No global orchestrator yet", …
    public let summary: String
    public let action: Action
    /// The button's words.
    public var actionTitle: String {
        switch action {
        case .open: mode == .chat ? "Open global chat" : "Open global orchestrator"
        case .start: "Start global orchestrator"
        case .unavailable: "Update the Mac to start it here"
        }
    }

    /// - `entry`: the global orchestrator in `orchestrators.list` (the newest, when there are several), nil when it lists none.
    /// - `dashboard`: the dashboard's `global`, nil without a dashboard. - `canStart`: the desktop offers `orchestrator.create`.
    public static func decide(entry: RemoteSession?, dashboard: DashboardGlobal?, canStart: Bool) -> GlobalOrchestratorStatus {
        if let entry {
            let mode = entry.mode
            var provider = entry.provider
            var state: String?
            var activity = entry.shownActivity
            switch dashboard {
            case .chat(let id, let named, let chatState)? where mode == .chat && (entry.chat_id == nil || entry.chat_id == id):
                provider = named ?? provider
                state = DashboardText.chatState(chatState)
                activity = SessionActivity.of(chatState) ?? activity
            case .terminal(let id, let known)? where mode == .terminal && id == entry.id:
                if let known { activity = entry.alive ? known : .exited }
                state = DashboardText.activity(activity)
            default:
                state = DashboardText.activity(activity)
            }
            let summary: String
            if mode == .chat { summary = [(provider?.chatTitle ?? "Chat"), state].compactMap { $0 }.joined(separator: " · ") }
            else { summary = ["Runs in a terminal", state].compactMap { $0 }.joined(separator: " · ") }
            return GlobalOrchestratorStatus(mode: mode, provider: provider, state: state, activity: activity, summary: summary, action: .open)
        }
        let none: (String) -> GlobalOrchestratorStatus = { text in
            GlobalOrchestratorStatus(mode: nil, provider: nil, state: nil, activity: .unknown, summary: text, action: canStart ? .start : .unavailable)
        }
        switch dashboard {
        case .none(let exited)?: return none(exited ? "Its last terminal has exited" : "No global orchestrator yet")
        case .chat(_, let provider, let chatState)?:
            // The dashboard has one the orchestrator list does not show yet: it is opened, never started again.
            return GlobalOrchestratorStatus(mode: .chat, provider: provider, state: DashboardText.chatState(chatState), activity: SessionActivity.of(chatState) ?? .unknown,
                                            summary: [(provider?.chatTitle ?? "Chat"), DashboardText.chatState(chatState)].joined(separator: " · "), action: .open)
        case .terminal(_, let known)?:
            return GlobalOrchestratorStatus(mode: .terminal, provider: nil, state: DashboardText.activity(known), activity: known ?? .unknown,
                                            summary: ["Runs in a terminal", DashboardText.activity(known)].compactMap { $0 }.joined(separator: " · "), action: .open)
        case .unknown?, nil: return none("No global orchestrator listed")
        }
    }
}

/// What a chat's state means for the activity mark.
enum SessionActivity {
    static func of(_ state: ChatState) -> AgentActivity? {
        switch state {
        case .running: .working
        case .waiting: .waiting
        case .idle: .done
        case .failed: .exited
        case .starting, .stopped, .unknown: nil
        }
    }
}
