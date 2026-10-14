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
    /// The global orchestrator's tab on screen while the Global screen is up, else the one it would show.
    var globalTab: ProjectTab? { OrchestratorScope.restoredGlobalTab(remembered: desktop?.globalTabKey, in: globalTabs) }

    /// Writes the global selection, or clears it when `key` is nil and the selection is of `kind` (a chat cleared leaves a terminal
    /// selection alone). A global terminal that stops being the selection stops being read.
    func setGlobalSelection(_ key: String?, replacing kind: String) {
        let current = desktop?.globalTabKey
        if key == nil, OrchestratorScope.id(in: current, kind: kind) == nil { return }
        guard key != current else { return }
        let previousTerminal = OrchestratorScope.id(in: current, kind: "shell")
        try? updateDesktop { $0.globalTabKey = key }
        if scope == .global, previousTerminal != nil, previousTerminal != sessionID { resetOutput(); snapshotStale = true; draft = ""; deliveryNotice = nil }
    }

    /// The Global screen came up: its tabs are the scope until it goes. The remembered global orchestrator (or the only one) is put on
    /// screen; the project's selection, its remembered tab and a restore it is waiting for are left exactly as they are.
    func enterGlobal() {
        guard scope != .global else { reconcileGlobalSelection(); return }
        let previous = sessionID
        scope = .global
        reconcileGlobalSelection()
        if let chat = selectedChatID { _ = conversation(chat) }
        switchedScope(from: previous)
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

    /// Keeps the global selection on a global orchestrator the desktop still lists: the remembered one, else the first, else none. The
    /// selection is the global one's own; a project's is never read or written here.
    func reconcileGlobalSelection() {
        let remembered = desktop?.globalTabKey
        let chosen = OrchestratorScope.restoredGlobalTab(remembered: remembered, in: globalTabs).map(OrchestratorScope.key)
        guard chosen != remembered else { return }
        let previousTerminal = OrchestratorScope.id(in: remembered, kind: "shell")
        try? updateDesktop { $0.globalTabKey = chosen }
        guard scope == .global else { return }
        if previousTerminal != sessionID {
            resetOutput(); snapshotStale = true; draft = ""; deliveryNotice = nil
            if previousTerminal != nil { sessionAutoSwitches &+= 1 }
        }
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
    /// explicit Start. The request carries no project.
    @discardableResult
    func startGlobalOrchestrator() async -> OrchestratorControlError? {
        guard let request = try? NewOrchestratorRequest(projectID: nil) else { return .invalid("The global orchestrator could not be asked for.") }
        return await createOrchestrator(request)
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
