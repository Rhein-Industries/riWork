import Foundation
import Observation
import RiWorkCore

/// Whether the desktop answers `dashboard.get`: from `ready.features.dashboard`, then from what it answers. Reset by every new connection.
enum DashboardSupport: Equatable { case unknown, supported, unsupported }

/// Why a dashboard response was not sent. Every case is definite: nothing went out. A failure after the command was handed to the
/// connection is a `ChatControlError` instead, and may be uncertain (`outcomeIsUncertain`).
enum DashboardResponseError: Error, Equatable, LocalizedError {
    case hostUnavailable, changedHost, staleRequest, unverifiable, alreadySent
    /// The host could not be reached or its current request read; the reason is the connection's.
    case notSent(String)
    var errorDescription: String? {
        switch self {
        case .hostUnavailable: "The selected host is unavailable. Reconnect and review its current request."
        case .changedHost: "The host changed while checking this request. Nothing was sent."
        case .staleRequest: "This request changed or was already answered. Refresh the dashboard and review the current request."
        case .unverifiable: "The host’s current request could not be read in full, so it could not be checked. Nothing was sent. Open the chat to answer."
        case .alreadySent: "A response to this request already went out from this phone. Open the chat to see what happened before answering again."
        case .notSent(let reason): "Nothing was sent. \(reason)"
        }
    }
}

/// The one path a dashboard card's response takes, over whichever connection is bound to its host: a fresh snapshot of the same chat
/// must still hold this exact request and its choices, and then exactly one `chat.command` goes out, never retried. Everything up to
/// handing the command to the connection fails definitely (`DashboardResponseError`); only what follows can be uncertain.
@MainActor enum DashboardResponder {
    static func respond(_ decision: DashboardDecision, command: ChatCommand, client: any RemoteTransport, identity: UploadConnectionIdentity,
                        stillCurrent: () -> Bool) async throws {
        let snapshot: ChatSnapshotReply
        do { snapshot = try await client.chatSnapshot(chatID: decision.chatID) }
        catch { throw DashboardResponseError.notSent(ChatControlError.from(error, operation: .events).message) }
        guard stillCurrent() else { throw DashboardResponseError.changedHost }
        switch decision.check(command, snapshot: snapshot) {
        case .current: break
        case .changed: throw DashboardResponseError.staleRequest
        case .unreadable: throw DashboardResponseError.unverifiable
        }
        let request: ChatCommandRequest
        do { request = try ChatCommandRequest(chatID: decision.chatID, command: command) }
        catch { throw DashboardResponseError.notSent(error.localizedDescription) }
        guard !Task.isCancelled, await client.isConnected() else { throw DashboardResponseError.notSent("The connection closed before the response was sent.") }
        guard stillCurrent() else { throw DashboardResponseError.changedHost }
        do {
            let reply = try await client.request(method: "chat.command", params: request.params, id: UUID().uuidString.lowercased(), boundTo: identity)
            try request.parse(reply)
        } catch is CancellationError where !Task.isCancelled {
            // Nobody cancelled this response: the transport found its connection bound to another host and sealed nothing.
            throw DashboardResponseError.changedHost
        } catch { throw ChatControlError.from(error, operation: .command) }
    }
}

// The dashboard: the desktop's global picture (`dashboard.get`, arranged by the desktop's own `global_overview`), the global orchestrator
// it opens, and every project. It is read only while it is on screen (`keepFresh(.dashboard)`), never more often than every 10 s, and
// reading it, drawing it or reading it again creates nothing: no chat, no shell, no orchestrator (`dashboard.get`, `projects.list` and
// `orchestrators.list` are reads). A desktop without the method is never asked; the screen says why and still lists and opens projects.
extension RemoteModel {
    /// A dashboard card has no authority until this exact host's complete current control set confirms it.
    func respondToDashboardDecision(_ decision: DashboardDecision, hostID: String, command: ChatCommand) async throws {
        guard state == .connected, selectedDesktopID == hostID, let desktop else { throw DashboardResponseError.hostUnavailable }
        let token = generation
        try await DashboardResponder.respond(decision, command: command, client: client, identity: UploadConnectionIdentity(pairing: desktop.pairing)) {
            generation == token && selectedDesktopID == hostID && state == .connected
        }
    }
    /// Why the dashboard's figures are not there, when the desktop has none to send; nil while it has, or may have.
    var dashboardUnavailableReason: String? {
        guard dashboardSupport == .unsupported else { return nil }
        return "The RiWork on your Mac does not send its dashboard yet. Update RiWork on the Mac to see what needs you, what is working, tasks and automations here. Projects still open below."
    }
    /// How current the dashboard on screen is: live, the age of the last read, or why it is not being read.
    func dashboardFreshness(now: Date = .now) -> DashboardFreshness {
        DashboardFreshness.decide(connected: state == .connected, connectionLabel: state.label, readAt: dashboardReadAt, failure: dashboardFailure, now: now)
    }

    /// The dashboard and what it opens, read again: the projects, the orchestrators and the desktop's dashboard, together. A read that
    /// fails is said on the screen (the last one stays, marked by its age); the next refresh tries again.
    func refreshDashboardQuietly() async {
        guard state == .connected else { return }
        let token = generation
        async let projectsRead: Void = refreshProjectsQuietly()
        async let globalRead: Void = refreshGlobalQuietly()
        async let dashboardRead: Void = readDashboard()
        _ = await (projectsRead, globalRead, dashboardRead)
        if generation == token { lastListRead[.dashboard] = .now }
    }

    /// One `dashboard.get`, when the desktop offers it and none is on its way.
    func readDashboard() async {
        guard state == .connected, desktopFeatures.dashboard, dashboardSupport != .unsupported, !dashboardInFlight else { return }
        dashboardInFlight = true
        let token = generation
        defer { if generation == token { dashboardInFlight = false } }
        do {
            let reply = try await rpc("dashboard.get")
            guard generation == token else { return }
            let snapshot = try DashboardSnapshot(wire: reply)
            if snapshot != dashboard { dashboard = snapshot }
            dashboardReadAt = .now; dashboardFailure = nil; dashboardSupport = .supported
        } catch is CancellationError {
        } catch {
            guard generation == token else { return }
            // A desktop whose connector is newer than what answers it: no dashboard for this connection, and the screen says why.
            if RemoteError.isUnsupportedMethod(error) { dashboardSupport = .unsupported; dashboardFailure = nil; return }
            dashboardFailure = Self.dashboardFailureText(error)
            await noteLinkLossIfNeeded()
        }
    }
    static func dashboardFailureText(_ error: any Error) -> String {
        switch error {
        case RemoteError.rpc("response_too_large", _): "The dashboard is too large to send over this link."
        case RemoteError.timeout: "The Mac did not answer in time."
        case let failure as DashboardSnapshot.ReadError: failure.localizedDescription
        default: error.localizedDescription
        }
    }
    /// Another desktop: nothing of the last one's dashboard is shown.
    func clearDashboard() {
        dashboard = nil; dashboardReadAt = nil; dashboardFailure = nil; dashboardSupport = .unknown; requestedTab = nil
    }

    // MARK: Where a row leads

    /// Where a dashboard row leads, knowing which ids are the global orchestrator's (from the orchestrators and the dashboard itself).
    func dashboardDestination(_ target: DashboardTarget, session: DashboardSession? = nil, projectID: String? = nil) -> DashboardDestination {
        var ids = OrchestratorScope.globalIDs(orchestrators)
        switch dashboard?.global {
        case .terminal(let id, _)?: ids.insert(id)
        case .chat(let id, _, _)?: ids.insert(id)
        default: break
        }
        return DashboardNavigation.destination(target, session: session, projectID: projectID, globalIDs: ids)
    }
    /// The project a dashboard row opens, as the project list has it (nil when the list no longer has it).
    func project(withID id: String) -> RemoteProject? { projects.first { $0.id == id } }
    /// Asks for a project's tab to be put on screen once that project's lists are in (`applyRequestedTab`); nil asks for its remembered tab.
    func requestProjectTab(_ project: String, key: String?) {
        requestedTab = key.map { (project, $0) }
    }
    /// Puts up the tab a dashboard row asked for, when its project's lists are in: a listed chat or open terminal is selected; one the
    /// shared list has hidden (a worker) is opened as the "Open shell/worker…" picker opens it. Nothing is created.
    func applyRequestedTab(project: String) {
        guard scope == .project, let request = requestedTab, request.project == project, projectID == project, loadedProjectID == project else { return }
        requestedTab = nil
        restoreTabPending = false
        let key = request.key
        if let chat = OrchestratorScope.id(in: key, kind: "chat"), projectTabs.contains(where: { $0.chatInfo?.id == chat }) {
            if selectedChatID != chat { selectChat(chat) }
            return
        }
        if let shell = OrchestratorScope.id(in: key, kind: "shell"), let session = openSessions.first(where: { $0.id == shell }) {
            Task { await chooseSession(session) }
            return
        }
        if desktopFeatures.tabs, sharedTabs?.allEntries.contains(where: { $0.key == key }) == true {
            Task { do { try await openTab(key) } catch { self.error = error.localizedDescription } }
        }
    }
}
