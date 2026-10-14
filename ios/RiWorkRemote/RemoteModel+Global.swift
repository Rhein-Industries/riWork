import Foundation
import Observation
import RiWorkCore

/// Whose tabs the selection belongs to: a project's, or the global orchestrator's (the dashboard's Global screen).
enum SelectionScope: Equatable { case project, global }

// The global orchestrator belongs to no project. It is opened on the dashboard's Global screen, never in a project's tab strip: putting
// it on screen, starting it, or reading it again never selects, switches or adopts a project, and never changes a project's selected
// tab or its restore. Its own selection (which global orchestrator, normally the only one) is remembered apart, per desktop
// (`SavedDesktop.globalTabKey`), and put back on relaunch and reconnect. The lists come from `orchestrators.list` as before; a terminal
// one is read and typed into by the usual terminal pipeline (`sessionID` follows the scope), a chat one is opened by its `chat_id`.
extension RemoteModel {
    /// The global orchestrators the desktop lists (normally one).
    var globalOrchestrators: [RemoteSession] { OrchestratorScope.globalEntries(orchestrators) }
    /// The global orchestrator's tab on screen while the Global screen is up, else the one it would show: the remembered one while the
    /// desktop lists it, else the first (normally the only one). Derived, never written back: a remembered choice that is missing for a
    /// moment (a list read while a chat host restarts) comes back with it.
    var globalTab: ProjectTab? { OrchestratorScope.restoredGlobalTab(remembered: desktop?.globalTabKey, in: globalTabs) }
    /// `globalTab`'s key (`shell:`, `chat:` or `orchestrator:`): what `sessionID`, `selectedChatID` and `selectedBlockedID` read on the
    /// Global screen.
    var shownGlobalKey: String? { globalTab.map(OrchestratorScope.key) }
    /// Global orchestrators whose terminal has ended, or that the desktop no longer has: what Start is offered for (there is nothing to
    /// open), as the desktop's "its last terminal has exited".
    var globalTerminalExited: Bool {
        globalOrchestrators.contains { $0.mode != .chat && (!$0.alive || missingSessionIDs.contains($0.id)) }
    }

    /// Remembers a choice on the Global screen (`SavedDesktop.globalTabKey`) and shows it.
    func rememberGlobalSelection(_ key: String) {
        if desktop?.globalTabKey != key { try? updateDesktop { $0.globalTabKey = key } }
        globalShownChanged()
    }

    /// The Global screen came up: its tabs are the scope until it goes. The remembered global orchestrator (or the only one) is put on
    /// screen; the project's selection, its remembered tab and a restore it is waiting for are left exactly as they are.
    func enterGlobal() {
        guard scope != .global else { reconcileGlobalSelection(); return }
        let previous = sessionID
        scope = .global
        switchedScope(from: previous)
        reconcileGlobalSelection()
    }
    /// The Global screen went: the project's tabs are the scope again, with the selection they had. What changed in the project's lists
    /// meanwhile is reconciled now (only for a project whose lists are in), and a restore that waited runs.
    func leaveGlobal() {
        guard scope == .global else { return }
        let previous = sessionID
        scope = .project
        clearOrchestratorNotice()
        if let project = projectID, loadedProjectID == project {
            restorePendingTab(project: project)
            reconcileChatSelection()
            try? reconcileSelectedSession()
        }
        switchedScope(from: previous)
    }

    /// After the global orchestrators were read again: the Global screen shows the one there is now (`globalTab`), and what it showed
    /// before, if it went, is let go. Nothing remembered yet: what is shown is remembered. The selection is the global one's own; a
    /// project's is never read or written here.
    func reconcileGlobalSelection() {
        guard scope == .global else { return }
        if desktop?.globalTabKey == nil, let shown = shownGlobalKey { try? updateDesktop { $0.globalTabKey = shown } }
        globalShownChanged()
        if let chat = selectedChatID { _ = conversation(chat) }
    }

    /// Puts a global orchestrator's tab on the Global screen the way its tab opens: a terminal by its session id, a chat by its `chat_id`,
    /// one the phone cannot open by its notice. Off the Global screen it is only remembered, for when the screen comes up.
    func openGlobalTab(_ tab: ProjectTab) async {
        guard scope == .global else { try? updateDesktop { $0.globalTabKey = OrchestratorScope.key(tab) }; return }
        switch tab {
        case .terminal(let session): await chooseGlobalTerminal(session)
        case .orchestratorChat, .unavailable: chooseOrchestrator(tab)
        case .chat: break
        }
    }

    /// Asks the root view to put the Global screen up (from a project's menu or sheet): it goes to the dashboard's Global screen, out of
    /// the project, whose selection stays as it was.
    func requestGlobalScreen() { globalScreenRequests &+= 1 }

    /// Starts the global orchestrator (or opens the one the Mac already has: `orchestrator.create` returns it unchanged), asked only by an
    /// explicit Start on the Dashboard or the Global screen, which put the Global screen up themselves first. The request carries no
    /// project.
    @discardableResult
    func startGlobalOrchestrator() async -> OrchestratorControlError? {
        guard let request = try? NewOrchestratorRequest(projectID: nil) else { return .invalid("The global orchestrator could not be asked for.") }
        return await createOrchestrator(request, showGlobal: false)
    }

    /// The global orchestrators read again (the Global screen's own refresh): nothing of a project is read or changed.
    func refreshGlobalQuietly() async {
        guard state == .connected else { return }
        let token = generation
        guard let managers = try? await rpc("orchestrators.list")["orchestrators"].decode([RemoteSession].self) else { return }
        guard generation == token, state == .connected else { return }
        if managers != orchestrators { orchestrators = managers }
        lastListRead[.global] = .now
        if scope == .global {
            reconcileChatSelection()
            reconcileGlobalSelection()
        }
    }
}
