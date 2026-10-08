import Foundation
import Observation
import RiWorkCore

/// Whether the desktop opens orchestrators, learned from `ready.features.orchestrator_create` and from what it answers, and reset by
/// every new connection.
enum OrchestratorCreateSupport: Equatable { case unknown, supported, unsupported }

// Opening the project's orchestrator, or the global one, on the desktop (`orchestrator.create`). The request is never retried by the
// app: a lost answer leaves the outcome unknown, and the person is told to look at the tab list. When the orchestrator is there already
// the desktop returns it and the phone just opens it, with a note.
extension RemoteModel {
    /// The sheet may offer the orchestrators and the menu its action: the desktop said it can, and has not refused since.
    var orchestratorsOffered: Bool { orchestratorCreateSupport == .supported }

    /// Opens the orchestrator of `request`'s scope (starting it if the desktop does not have one) and puts it on screen: a chat when the
    /// desktop runs it as one (by its `chat_id`), otherwise a terminal. Returns nil on success, otherwise why not. `onOpened` runs as soon
    /// as the desktop has answered, so a sheet can go away at once.
    @discardableResult
    func createOrchestrator(_ request: NewOrchestratorRequest, onOpened: @MainActor (RemoteSession) -> Void = { _ in }) async -> OrchestratorControlError? {
        guard !creatingOrchestrator else { return .busy }
        guard state == .connected else { return .notConnected }
        guard orchestratorCreateSupport != .unsupported else { return .unsupported }
        creatingOrchestrator = true
        defer { creatingOrchestrator = false }
        let token = generation
        let reply: NewOrchestratorReply
        do {
            reply = try await client.createOrchestrator(request)
        } catch {
            let failure = OrchestratorControlError.from(error)
            if generation == token, failure == .unsupported { orchestratorCreateSupport = .unsupported }
            // Something was made, or may have been: the tab list shows it if so.
            if generation == token, failure == .unreadableReply || failure == .exitedRightAway { await refreshSessionsQuietly() }
            return failure
        }
        if generation == token { orchestratorCreateSupport = .supported }
        // A reconnect or another desktop in the meantime: the orchestrator exists, but this screen is no longer its.
        guard generation == token, state == .connected else { return nil }
        let entry = reply.session
        if desktopFeatures.tabs, let project = projectID, entry.project_id == project {
            let key = entry.mode == .chat ? "chat:\(entry.chat_id ?? entry.id)" : "shell:\(entry.id)"
            do { try await unhideTab(key) } catch { return .failed(error.localizedDescription) }
            guard generation == token, projectID == project, state == .connected else { return nil }
        }
        if let index = orchestrators.firstIndex(where: { $0.id == entry.id }) { orchestrators[index] = entry } else { orchestrators.append(entry) }
        onOpened(entry)
        await openOrchestratorTab(entry)
        if !reply.created { noteOrchestrator("\(entry.title) is already running.") }
        await refreshSessionsQuietly()
        return nil
    }

    /// Puts an orchestrator's tab on screen the way its tab would: a terminal is selected, a chat is opened by its `chat_id`, and one
    /// the phone cannot open says why. One that is not among this project's tabs (another project's) is left alone.
    func openOrchestratorTab(_ session: RemoteSession) async {
        guard let tab = tabs.first(where: { $0.session?.id == session.id }) else { return }
        switch tab {
        case .terminal(let terminal): await chooseSession(terminal)
        case .orchestratorChat, .unavailable: chooseOrchestrator(tab)
        case .chat: break
        }
    }

    // MARK: The note

    static let orchestratorNoticeDuration: Duration = .seconds(5)

    /// Says something short under the tab strip for a few seconds.
    func noteOrchestrator(_ text: String) {
        orchestratorNotice = text
        orchestratorNoticeExpiry?.cancel()
        orchestratorNoticeExpiry = Task { [weak self] in
            try? await Task.sleep(for: Self.orchestratorNoticeDuration)
            if !Task.isCancelled { self?.orchestratorNotice = nil }
        }
    }
    func clearOrchestratorNotice() {
        orchestratorNoticeExpiry?.cancel(); orchestratorNoticeExpiry = nil
        orchestratorNotice = nil
    }
}
