import Foundation
import NativeShell
import Observation
import RiWorkCore
import UIKit

/// Native screens retain their existing input, keyboard, approval and pairing
/// behavior while NSH owns the library, dashboards and project navigation.
enum NSHMobilePresentation {
    case project(RemoteProject)
    case global
    case addHost
    case hostDetails(String)
    case settings
    case newProject
    case libraryRecovery
    case decision(hostID: String, decision: DashboardDecision)
    case dismiss
}

/// A projection of RemoteModel, never a second transport or credential store.
/// Every JS payload is assembled field by field: SavedDesktop, Pairing,
/// pending terminal input and chat/approval bodies never cross this bridge.
@MainActor final class NSHMobileBridge {
    let model: RemoteModel
    var onPresentation: ((NSHMobilePresentation) -> Void)?
    var onRootShortcutPolicyChange: ((Bool) -> Void)?
    private weak var app: NshAppView?
    private let hostDashboards = HostDashboardStore()
    /// The host calls the shared TSX screens use (`relay.*`, `platform.*`, `nav.*`).
    private(set) lazy var hostAPI = NSHHostAPI(bridge: self)
    private var route = "hosts"
    private(set) var nativePresented = false
    private var revision = 0
    private var observation = UUID()
    private var navigation = UUID()
    private var openingHostID: String?
    private var respondingHostID: String?
    private var activityTask: Task<Void, Never>?
    private var activityScope: ActivityScope?
    /// The activity loop leaves `dashboard.get` to the shared TSX dashboard, which reads it over `relay.call`.
    private var activityLeavesDashboard = false
    private var freshnessTask: Task<Void, Never>?
    private var rows: [String: RowAction] = [:]
    private var sentDecisions: [DecisionKey: Date] = [:]

    private struct Failure: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private struct DecisionKey: Hashable {
        let hostID, chatID, requestID: String
        init(hostID: String, decision: DashboardDecision) {
            self.hostID = hostID; chatID = decision.chatID; requestID = decision.requestID
        }
    }
    private enum RowAction {
        case destination(hostID: String, target: DashboardTarget, session: DashboardSession?, projectID: String?)
        case decision(hostID: String, value: DashboardDecision)
    }

    init(model: RemoteModel) {
        self.model = model
        hostDashboards.compressTraffic = { [weak model] in model?.compressTraffic ?? true }
    }

    func attach(to app: NshAppView) {
        if self.app !== app { detach() }
        self.app = app
        app.handle("mobile.snapshot") { [weak self] (_: Any?) async throws -> Any? in
            guard let self else { throw Failure(message: "The mobile view has closed.") }
            self.publishAndObserve()
            return self.snapshot()
        }
        app.handle("mobile.action") { [weak self] (data: Any?) async throws -> Any? in
            guard let self else { throw Failure(message: "The mobile view has closed.") }
            try await self.perform(data)
            self.publishAndObserve()
            return self.snapshot()
        }
        hostAPI.attach(to: app)
        publishAndObserve()
    }

    func detach() {
        observation = UUID(); navigation = UUID()
        app?.removeHandler("mobile.snapshot"); app?.removeHandler("mobile.action")
        hostAPI.detach()
        app = nil
        onRootShortcutPolicyChange?(false)
        activityTask?.cancel(); activityTask = nil; activityScope = nil
        freshnessTask?.cancel(); freshnessTask = nil
        hostDashboards.setVisible(false, desktops: model.desktops, sceneActive: model.appActive, activeHostID: activeOverviewHostID)
    }

    /// Called by the UIView host when light/dark or contrast traits change.
    func traitsChanged() { publishAndObserve() }

    func setNativePresented(_ presented: Bool) {
        guard nativePresented != presented else { return }
        nativePresented = presented
        publishAndObserve()
    }
    func nativeDismissed() { setNativePresented(false) }
    var rootShortcutsActive: Bool {
        app != nil && model.appActive && !nativePresented && openingHostID == nil && respondingHostID == nil && !model.loadFailed
            && route != "projects" && model.keyboard.usesClicks && model.keyboard.usesHardwareLayout
    }
    func rootShortcut(_ action: TabShortcutAction) {
        // All hosts belongs to no host. Its guide may open, but a command must
        // never reach the desktop that happened to be used previously.
        guard rootShortcutsActive, route == "dashboard", model.selectedDesktopID != nil else { return }
        switch action {
        case .projects: showProjects()
        case .newTerminal:
            guard let id = model.projectID, model.project(withID: id) != nil, model.state == .connected else { return }
            model.newTerminalRequestedProject = id
            try? openProject(id)
        case .modelPicker:
            if model.selectedChat != nil { model.keyboard.requestModelPicker() }
        default: break
        }
    }
    func showProjects() {
        navigation = UUID(); nativePresented = false
        route = model.selectedDesktopID == nil ? "hosts" : "projects"
        publishAndObserve()
    }
    func showGlobal() {
        guard model.selectedDesktopID != nil else { return }
        navigation = UUID(); route = "dashboard"
        present(.global)
    }
    /// The host the person opened or is opening: the one `relay.call` and the decision calls reach.
    var interactiveHostID: String? { openingHostID ?? model.selectedDesktopID }
    func projectCreated(_ project: RemoteProject) {
        model.newTerminalRequestedProject = project.id
        present(.project(project))
    }

    func connect(hostID: String) async throws {
        guard !model.loadFailed, model.desktops.contains(where: { $0.id == hostID }) else {
            throw Failure(message: "This saved desktop is no longer available.")
        }
        guard openingHostID == nil, respondingHostID == nil else {
            throw Failure(message: "Another desktop operation is still in progress.")
        }
        let token = UUID(); navigation = token; openingHostID = hostID
        route = "dashboard"; nativePresented = false
        publishAndObserve()
        // The relay permits one mobile socket per pairing. Close overview
        // clients before handing the selected pairing to RemoteModel.
        await hostDashboards.stop()
        guard navigation == token else { openingHostID = nil; publishAndObserve(); return }
        await model.activate(hostID)
        if model.selectedDesktopID == hostID, model.state == .connected {
            await model.refreshDashboardQuietly()
        }
        openingHostID = nil
        // Back during connection never pushes another screen on completion.
        publishAndObserve()
    }

    private func perform(_ value: Any?) async throws {
        guard let data = value as? [String: Any], let type = data["type"] as? String else {
            throw Failure(message: "Invalid mobile action.")
        }
        guard !nativePresented || type == "dismissNative" else {
            throw Failure(message: "Close the current native screen first.")
        }
        switch type {
        case "connect": try await connect(hostID: required(data, "hostID"))
        case "disconnect": await disconnectInteractive()
        case "refresh":
            if model.loadFailed { model.loadLibrary() }
            else if route == "hosts" {
                if model.state == .connected { await model.refreshDashboardQuietly(); mirrorActiveDashboard() }
                await hostDashboards.refresh()
            } else {
                await model.refresh()
                await model.refreshDashboardQuietly()
            }
        case "showProjects": showProjects()
        case "cycleProjectSort": model.cycleProjectSort()
        case "setProjectSort":
            guard let sort = ProjectSort(rawValue: try required(data, "value")) else { throw Failure(message: "Unknown project order.") }
            model.setProjectSort(sort)
        case "showDashboard": try navigate("dashboard")
        case "navigate": try navigate(required(data, "page"))
        case "back": try navigate(route == "projects" ? "dashboard" : "hosts")
        case "openProject", "newTerminal":
            if let hostID = data["hostID"] as? String, hostID != model.selectedDesktopID {
                throw Failure(message: "The selected desktop changed. Open the project from its current desktop.")
            }
            let id = try required(data, "projectID")
            if type == "newTerminal" {
                guard model.state == .connected, model.project(withID: id) != nil else { throw Failure(message: "Connect to this project's desktop first.") }
                model.newTerminalRequestedProject = id
            }
            try openProject(id)
        case "openGlobal": try await openGlobal(start: true)
        case "openDashboardRow": try await openRow(required(data, "rowID"))
        case "present": try present(named: required(data, "name"), hostID: data["hostID"] as? String)
        case "dismissNative": present(.dismiss)
        default: throw Failure(message: "Unknown mobile action.")
        }
    }

    func disconnectInteractive() async {
        navigation = UUID()
        await model.disconnect()
    }
    /// The global orchestrator's screen; with `start`, it is started first when there is none (an explicit Start).
    func openGlobal(start: Bool) async throws {
        guard model.state == .connected else { throw Failure(message: "Connect to this desktop first.") }
        let status = globalStatus
        guard status.action != .unavailable else { throw Failure(message: status.actionTitle) }
        showGlobal()
        if start, status.action == .start, let failure = await model.startGlobalOrchestrator() { model.error = failure.message }
    }
    /// A native screen by name (`mobile.action` `present`, `nav.present`).
    func present(named name: String, hostID: String?) throws {
        switch name {
        case "addHost":
            guard !model.loadFailed else { throw Failure(message: "Restore the saved pairing library first.") }
            present(.addHost)
        case "settings": present(.settings)
        case "hostDetails":
            guard let id = hostID, model.desktops.contains(where: { $0.id == id }) else { throw Failure(message: "This desktop is no longer saved.") }
            present(.hostDetails(id))
        case "newProject":
            guard model.canOpenNewProject else { throw Failure(message: "Connect to a desktop that supports creating projects.") }
            present(.newProject)
        case "libraryRecovery": present(.libraryRecovery)
        default: throw Failure(message: "Unknown native screen.")
        }
    }

    private func required(_ data: [String: Any], _ key: String) throws -> String {
        guard let text = data[key] as? String, !text.isEmpty, text.utf8.count <= 2048 else {
            throw Failure(message: "The action is missing its \(key).")
        }
        return text
    }
    private func navigate(_ page: String) throws {
        guard ["hosts", "dashboard", "projects"].contains(page) else { throw Failure(message: "Unknown mobile page.") }
        guard page == "hosts" || model.selectedDesktopID != nil || openingHostID != nil else {
            throw Failure(message: "Choose a desktop first.")
        }
        navigation = UUID(); route = page
        publishAndObserve()
    }
    func openProject(_ id: String, tab: String? = nil) throws {
        guard model.state == .connected, let project = model.project(withID: id) else {
            throw Failure(message: "This project is unavailable. Refresh the desktop and try again.")
        }
        model.requestProjectTab(id, key: tab)
        // TerminalTabsView owns chooseProject, viewport release and the input
        // lifecycle. Opening it here does not start a second inventory task.
        present(.project(project))
    }
    private func present(_ request: NSHMobilePresentation) {
        nativePresented = true
        publishAndObserve()
        onPresentation?(request)
    }

    private func openRow(_ id: String) async throws {
        // Rebuild the projection before resolving the ID: removed requests or
        // rows from a previous host cannot be replayed as a different target.
        _ = snapshot()
        guard let action = rows[id] else { throw Failure(message: "This dashboard row changed. Refresh and try again.") }
        switch action {
        case .decision(let hostID, let decision):
            guard model.desktops.contains(where: { $0.id == hostID }) else { throw Failure(message: "This desktop is no longer saved.") }
            present(.decision(hostID: hostID, decision: decision))
        case .destination(let hostID, let target, let session, let projectID):
            if model.selectedDesktopID != hostID || model.state != .connected {
                try await connect(hostID: hostID)
                guard route == "dashboard", model.selectedDesktopID == hostID, model.state == .connected else { return }
            }
            switch model.dashboardDestination(target, session: session, projectID: projectID) {
            case .global: showGlobal()
            case .project(let id, let tab): try openProject(id, tab: tab)
            case .none: throw Failure(message: "Manage this item on the desktop.")
            }
        }
    }

    func isDecisionAnswered(_ item: HostedDashboardDecision) -> Bool {
        sentDecisions[DecisionKey(hostID: item.hostID, decision: item.decision)] != nil
    }
    func respondToDecision(_ item: HostedDashboardDecision, command: ChatCommand) async throws {
        let key = DecisionKey(hostID: item.hostID, decision: item.decision)
        guard sentDecisions[key] == nil else { throw DashboardResponseError.alreadySent }
        guard openingHostID == nil, respondingHostID == nil,
              let desktop = model.desktops.first(where: { $0.id == item.hostID }) else {
            throw DashboardResponseError.hostUnavailable
        }
        respondingHostID = item.hostID
        defer { respondingHostID = nil; publishAndObserve() }
        await hostDashboards.stop()
        let interactive = activeOverviewHostID == item.hostID
        do {
            if interactive {
                if model.state != .connected { await model.activate(item.hostID) }
                try await model.respondToDashboardDecision(item.decision, hostID: item.hostID, command: command)
            } else {
                try await hostDashboards.respond(to: item.decision, on: desktop, command: command)
            }
        } catch {
            if (error as? ChatControlError)?.outcomeIsUncertain == true { sentDecisions[key] = .now }
            throw error
        }
        sentDecisions[key] = .now
        if interactive { await model.refreshDashboardQuietly(); mirrorActiveDashboard() }
    }

    private var activeOverviewHostID: String? {
        guard let id = model.selectedDesktopID else { return nil }
        switch model.state {
        case .connected, .connecting, .suspended: return id
        case .failed: return model.reconnectTask != nil || model.hasPendingKeys(forDesktop: id) ? id : nil
        case .disconnected: return model.wantsConnection ? id : nil
        }
    }
    private func mirrorActiveDashboard() {
        guard let id = activeOverviewHostID, let desktop = model.desktops.first(where: { $0.id == id }) else { return }
        let state: HostDashboardEntry.State = model.dashboardSupport == .unsupported ? .unsupported
            : (model.state == .connected ? .ready : (model.state == .connecting ? .refreshing : .offline))
        hostDashboards.updateActiveHost(HostDashboardEntry(id: id, name: desktop.name, snapshot: model.dashboard,
            readAt: model.dashboardReadAt, state: state, failure: model.dashboardFailure))
    }
    private func syncLifecycle() {
        if model.selectedDesktopID == nil && openingHostID == nil { route = "hosts" }
        let visible = app != nil && !nativePresented && model.appActive
        if visible && route != "projects" {
            if freshnessTask == nil {
                // Matches the original TimelineView: update the age of a saved
                // read while visible, including when an offline host is silent.
                // This does not perform any network read.
                freshnessTask = Task { [weak self] in
                    while !Task.isCancelled {
                        do { try await Task.sleep(for: .seconds(15)) } catch { return }
                        guard !Task.isCancelled, let self else { return }
                        self.publishAndObserve()
                    }
                }
            }
        } else { freshnessTask?.cancel(); freshnessTask = nil }
        hostDashboards.setVisible(visible && route == "hosts" && openingHostID == nil && respondingHostID == nil && !model.loadFailed,
            desktops: model.desktops, sceneActive: model.appActive, activeHostID: activeOverviewHostID)
        mirrorActiveDashboard()
        let scope: ActivityScope? = visible && openingHostID == nil ? (route == "projects" ? .projects : .dashboard) : nil
        // A host's own page is the shared TSX dashboard, which reads `dashboard.get` itself (`relay.call`, installed into the
        // model as Swift's own read would be): Swift then keeps only the lists its rows open with.
        let leavesDashboard = scope == .dashboard && route == "dashboard"
        if scope != activityScope || leavesDashboard != activityLeavesDashboard {
            activityTask?.cancel(); activityTask = nil; activityScope = scope; activityLeavesDashboard = leavesDashboard
            if let scope {
                // Reuse the existing visibility-scoped network refresh. This
                // never polls a JS snapshot or creates another transport.
                activityTask = Task { [weak model] in
                    guard let model else { return }
                    if leavesDashboard { await model.keepDashboardListsFresh() } else { await model.keepFresh(scope) }
                }
            }
        }
        sentDecisions = sentDecisions.filter { key, sentAt in
            guard model.desktops.contains(where: { $0.id == key.hostID }) else { return false }
            guard let host = hostDashboards.hosts.first(where: { $0.id == key.hostID }),
                  let readAt = host.readAt, readAt > sentAt, let snapshot = host.snapshot else { return true }
            return snapshot.decisions.rows.contains { $0.chatID == key.chatID && $0.requestID == key.requestID }
        }
    }
    private func publishAndObserve() {
        guard let app else { return }
        let token = UUID(); observation = token
        syncLifecycle()
        onRootShortcutPolicyChange?(rootShortcutsActive)
        revision &+= 1
        let data = withObservationTracking { snapshot() } onChange: { [weak self] in
            // Observation fires before the mutation completes. Hop once onto
            // MainActor, then re-arm, so JS receives the completed new state.
            Task { @MainActor [weak self] in
                guard let self, self.observation == token, self.app != nil else { return }
                self.publishAndObserve()
            }
        }
        app.send("mobile.snapshot", data)
        hostAPI.publish()
    }

    private func snapshot() -> [String: Any] {
        rows.removeAll(keepingCapacity: true)
        // These observed values also drive lifecycle changes with no timer.
        _ = model.appActive; _ = model.loadFailed
        // The active host's read (now the shared dashboard's, over `relay.call`) is mirrored into the overview as it lands.
        _ = model.dashboard; _ = model.dashboardReadAt; _ = model.dashboardFailure; _ = model.dashboardSupport
        _ = model.keyboard.usesClicks; _ = model.keyboard.usesHardwareLayout
        let currentHost = openingHostID ?? model.selectedDesktopID
        let sameHost = currentHost == model.selectedDesktopID
        let hosts: [[String: Any]] = model.desktops.map { desktop in
            let overview = hostDashboards.hosts.first { $0.id == desktop.id }
            var value: [String: Any] = ["id": desktop.id, "name": desktop.name,
                "address": URLComponents(string: desktop.pairing.relay_url)?.host ?? "Paired desktop",
                "selected": desktop.id == currentHost,
                "connection": desktop.id == model.selectedDesktopID ? connectionState : "disconnected"]
            value["detail"] = overview?.freshness().text
            return value
        }
        let projects: [[String: Any]] = sameHost ? model.sortedProjects.map { project in
            var value: [String: Any] = ["id": project.id, "name": project.name, "path": project.root,
                "selected": project.id == model.projectID,
                "tone": (project.agents?.waiting ?? 0) > 0 ? "gold" : ((project.agents?.working ?? 0) > 0 ? "working" : "muted")]
            value["activity"] = project.agents?.spoken
            value["folder"] = model.dashboard?.projects.rows.first(where: { $0.id == project.id })?.folder
            return value
        } : []
        var connection: [String: Any] = ["state": connectionState, "label": model.state.label]
        connection["error"] = model.error
        var result: [String: Any] = ["schema": 1, "revision": revision, "route": route,
            "nativePresented": nativePresented, "desktops": hosts, "connection": connection,
            "loading": model.loading || openingHostID != nil, "projects": projects,
            "projectSort": ["value": model.projectSort.rawValue, "title": model.projectSort.title, "detail": model.projectSort.detail],
            "keyboard": ["hardware": model.keyboard.usesHardwareLayout, "clicks": model.keyboard.usesClicks],
            "homeDashboard": homeDashboardDTO(), "theme": themeDTO(),
            "canCreateProject": sameHost && model.canOpenNewProject,
            "projectsStale": model.snapshotStale,
            "libraryUnavailable": model.loadFailed]
        result["selectedHostID"] = currentHost
        result["error"] = model.loadFailure ?? model.error
        return result
    }

    private var connectionState: String {
        switch model.state {
        case .disconnected: "disconnected"
        case .connecting: "connecting"
        case .connected: "connected"
        case .suspended: "suspended"
        case .failed: "failed"
        }
    }
    private var globalStatus: GlobalOrchestratorStatus {
        GlobalOrchestratorStatus.decide(entry: model.globalTabs.first?.session,
            dashboard: model.dashboardSupport == .unsupported ? nil : model.dashboard?.global,
            canStart: model.orchestratorsOffered, exitedTerminal: model.globalTerminalExited)
    }
    func themeDTO() -> [String: Any] {
        let style = model.theme.style
        if style.theme.followsSystemAppearance {
            let traits = app?.traitCollection ?? UITraitCollection.current
            let background = style.backgroundUI.resolvedColor(with: traits)
            var br: CGFloat = 0, bg: CGFloat = 0, bb: CGFloat = 0, ba: CGFloat = 1
            background.getRed(&br, green: &bg, blue: &bb, alpha: &ba)
            func color(_ source: UIColor) -> String {
                var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 1
                source.resolvedColor(with: traits).getRed(&r, green: &g, blue: &b, alpha: &a)
                func byte(_ value: CGFloat) -> Int { Int((max(0, min(1, value)) * 255).rounded()) }
                return String(format: "#%02x%02x%02x", byte(r * a + br * (1 - a)), byte(g * a + bg * (1 - a)), byte(b * a + bb * (1 - a)))
            }
            return ["scale": style.scale, "fontFamily": "system", "dark": traits.userInterfaceStyle == .dark,
                "palette": ["bg": color(style.backgroundUI), "panel": color(style.panelUI), "panelActive": color(style.activeUI),
                    "divider": color(style.dividerUI), "text": color(style.textUI), "muted": color(style.mutedUI),
                    "cyan": color(style.accentUI), "working": color(style.workingUI), "gold": color(style.goldUI),
                    "magenta": color(style.magentaUI), "error": color(style.errorUI)]]
        }
        let dark = style.theme.dark ?? (app?.traitCollection.userInterfaceStyle == .dark)
        func color(_ value: ThemeColor) -> String {
            let rgb = dark ? value.dark : value.light
            return String(format: "#%02x%02x%02x", Int(rgb.red), Int(rgb.green), Int(rgb.blue))
        }
        let palette: [String: String] = ["bg": color(style.theme.background), "panel": color(style.theme.panel),
            "panelActive": color(style.theme.active), "divider": color(style.theme.divider),
            "text": color(style.theme.text), "muted": color(style.theme.muted),
            "cyan": color(style.theme.accent), "working": color(style.theme.working),
            "gold": color(style.theme.gold), "magenta": color(style.theme.magenta), "error": color(style.theme.error)]
        return ["scale": style.scale, "fontFamily": "system", "dark": dark, "palette": palette]
    }

    private func metric(_ id: String, _ label: String, _ value: Int?) -> [String: Any] {
        ["id": id, "label": label, "value": value.map { $0 as Any } ?? NSNull()]
    }
    private func homeDashboardDTO() -> [String: Any] {
        let aggregate = hostDashboards.aggregate
        func count(_ id: String, _ label: String, _ value: HostDashboardCount) -> [String: Any] {
            var result = metric(id, label, value.value)
            result["coverage"] = "\(value.knownHosts) of \(value.totalHosts) hosts"
            return result
        }
        let allRows = hostDashboards.hosts.flatMap { host -> [[String: Any]] in
            guard let snapshot = host.snapshot else { return [] }
            return dashboardRows(snapshot, hostID: host.id, hostName: host.name, stale: host.freshness().stale, limit: 20)
        }
        // Reuse the original aggregate's cross-host priority/age ordering.
        // Sorting each section separately keeps the comparison transitive.
        var ranks: [String: Int] = [:]
        func rank<Row: Sendable & Equatable>(_ values: [HostDashboardRow<Row>], section: String) {
            for (index, row) in values.enumerated() { ranks["\(row.hostID):\(section):\(row.id.localID)"] = index }
        }
        rank(aggregate.decisionRows, section: "decisions"); rank(aggregate.attentionRows, section: "attention")
        rank(aggregate.workingRows, section: "working"); rank(aggregate.finishedRows, section: "finished")
        rank(aggregate.projectRows, section: "projects"); rank(aggregate.taskRows, section: "tasks")
        let orderedRows = ["decisions", "attention", "working", "finished", "tasks", "automations", "projects", "history"].flatMap { section in
            allRows.enumerated().filter { $0.element["section"] as? String == section }.sorted { left, right in
                let a = ranks[left.element["id"] as? String ?? ""] ?? Int.max
                let b = ranks[right.element["id"] as? String ?? ""] ?? Int.max
                return a == b ? left.offset < right.offset : a < b
            }.map(\.element)
        }
        let intelligence = hostDashboards.hosts.compactMap { host in
            host.snapshot.flatMap { intelligenceDTO($0.intelligence, hostID: host.id, hostName: host.name) }
        }
        var more: [String: Int] = [:]
        for host in hostDashboards.hosts {
            guard let snapshot = host.snapshot else { continue }
            for (section, count) in omittedRows(snapshot, limit: 20) { more[section, default: 0] += count }
        }
        var notes: [String] = []
        if aggregate.staleHostCount > 0 || aggregate.unavailableHostCount > 0 {
            notes.append("\(aggregate.staleHostCount) saved · \(aggregate.unavailableHostCount) not read. Totals include the last successful reads.")
        }
        if hostDashboards.hosts.contains(where: { $0.snapshot?.decisionSources.complete == false }) {
            notes.append("Decision coverage is incomplete on some desktops. Open a desktop or chat for the full state.")
        }
        return ["available": aggregate.sampledHostCount > 0,
            "freshness": "\(aggregate.freshHostCount) of \(aggregate.totalHostCount) hosts current",
            "stale": aggregate.freshHostCount < aggregate.totalHostCount,
            "summary": [count("working", "Working", aggregate.working), count("attention", "Need you", aggregate.attention),
                count("projects", "Projects", aggregate.projects), count("tasks", "Open tasks", aggregate.openTasks)],
            "rows": orderedRows, "intelligence": intelligence, "notes": notes, "more": more]
    }

    private func intelligenceDTO(_ value: DashboardIntelligence, hostID: String, hostName: String?) -> [String: Any]? {
        guard value.state != "disabled" else { return nil }
        var result: [String: Any] = ["id": hostID, "state": value.state, "stale": value.stale,
            "highlights": value.highlights.map(\.text), "followups": value.followups.map(\.text)]
        result["hostName"] = hostName; result["summary"] = value.summary; result["error"] = value.error
        result["model"] = value.model; result["generatedAt"] = value.generatedAt
        return result
    }
    private func omittedRows(_ snapshot: DashboardSnapshot, limit: Int = 12) -> [String: Int] {
        func count<Row: Sendable & Equatable>(_ list: DashboardList<Row>, limit: Int) -> Int { max(0, (list.total ?? list.rows.count) - min(limit, list.rows.count)) }
        var counts = ["decisions": count(snapshot.decisions, limit: limit), "attention": count(snapshot.attention, limit: limit),
            "working": count(snapshot.working, limit: limit), "finished": count(snapshot.finished, limit: limit),
            "tasks": count(snapshot.tasksInProgress, limit: limit), "history": count(snapshot.recentActivity, limit: limit),
            "projects": count(snapshot.projects, limit: 150)]
        if case .read(_, _, _, let items) = snapshot.automations { counts["automations"] = count(items, limit: limit) }
        return counts
    }
    private func timeLabel(_ unix: UInt64, future: Bool = false) -> String {
        let interval = Double(unix) - Date().timeIntervalSince1970
        if future { return interval < 5 ? "now" : "in \(DashboardText.duration(interval))" }
        return interval > -5 ? "just now" : "\(DashboardText.duration(-interval)) ago"
    }

    private func dashboardRows(_ snapshot: DashboardSnapshot, hostID: String, hostName: String? = nil, stale: Bool = false, limit: Int = 12) -> [[String: Any]] {
        var result: [[String: Any]] = []
        func add(_ section: String, _ localID: String, _ title: String, subtitle: String? = nil,
                 status: String? = nil, tone: String = "muted", folder: String? = nil, action: RowAction? = nil) {
            let id = "\(hostID):\(section):\(localID)"
            if let action { rows[id] = action }
            var row: [String: Any] = ["id": id, "hostID": hostID, "section": section,
                "title": title, "tone": tone, "enabled": action != nil, "stale": stale]
            row["hostName"] = hostName; row["subtitle"] = subtitle; row["status"] = status
            row["folder"] = folder
            result.append(row)
        }
        func destination(_ target: DashboardTarget, _ session: DashboardSession? = nil, _ project: String? = nil) -> RowAction? {
            if case .automations(nil) = target { return nil }
            return .destination(hostID: hostID, target: target, session: session, projectID: project)
        }
        for decision in snapshot.decisions.rows.prefix(limit) {
            let sent = sentDecisions[DecisionKey(hostID: hostID, decision: decision)] != nil
            add("decisions", decision.id, decision.title, subtitle: decision.session.name,
                status: sent ? "Response sent" : "Review and answer", tone: "gold",
                action: .decision(hostID: hostID, value: decision))
        }
        for (index, attention) in snapshot.attention.rows.prefix(limit).enumerated() {
            let title: String; let detail: String
            switch attention.kind {
            case .waiting: title = attention.session?.name ?? "Agent"; detail = "Waiting for input"
            case .chatFailed(let message): title = attention.session?.name ?? "Chat"; detail = message
            case .automation(let name, let outcome, let message): title = name; detail = message.isEmpty ? outcome : message
            case .other(let name): title = attention.session?.name ?? name; detail = name
            }
            add("attention", "\(targetID(attention.target)):\(index)", title,
                subtitle: attention.session?.place ?? attention.projectName, status: detail, tone: attention.kind.isFailure ? "error" : "gold",
                action: destination(attention.target, attention.session, attention.projectID))
        }
        for (section, work) in [("working", snapshot.working.rows), ("finished", snapshot.finished.rows)] {
            for row in work.prefix(limit) {
                add(section, targetID(row.session.target), row.session.name, subtitle: row.session.place,
                    status: [row.activity.spoken(subagents: row.subagents ?? 0), row.sinceUnix.map { section == "working" ? "for \(DashboardText.duration(Date().timeIntervalSince1970 - Double($0)))" : timeLabel($0) }].compactMap { $0 }.joined(separator: " · "), tone: section == "working" ? "working" : "muted",
                    action: destination(row.session.target, row.session, row.session.projectID))
            }
        }
        for task in snapshot.tasksInProgress.rows.prefix(limit) {
            add("tasks", task.id, task.title, subtitle: task.projectName, status: "In progress",
                action: destination(.task(id: task.id), nil, task.projectID))
        }
        if case .read(_, _, _, let items) = snapshot.automations {
            for automation in items.rows.prefix(limit) {
                add("automations", automation.id, automation.title, subtitle: automation.projectName,
                    status: automation.paused ? "Paused" : [automation.nextRun.map { "Next \(timeLabel($0, future: true))" }, automation.lastOutcome].compactMap { $0 }.joined(separator: " · "),
                    tone: automation.lastRunIsFailure ? "error" : (automation.lastRunFailed ? "gold" : "muted"),
                    action: destination(.automations(projectID: automation.projectID), nil, automation.projectID))
            }
        }
        for project in snapshot.projects.rows.prefix(150) {
            var status: [String] = []
            if let working = project.agents?.working { status.append("\(working) working") }
            if let waiting = project.agents?.waiting, waiting > 0 { status.append("\(waiting) waiting") }
            if let open = project.tasks?.open { status.append("\(open) open tasks") }
            if let live = project.liveSessions { status.append("\(live) live sessions") }
            else if let terminals = project.liveShells { status.append("\(terminals) terminals") }
            switch project.orchestrator {
            case .terminal(let activity)?: status.append("Terminal orchestrator" + (DashboardText.activity(activity).map { " · \($0)" } ?? ""))
            case .chat(let provider, let state)?: status.append("\(provider?.chatTitle ?? "Chat") orchestrator · \(DashboardText.chatState(state))")
            case nil: break
            }
            if let at = project.lastActivity { status.append("Active \(timeLabel(at))") }
            add("projects", project.id, project.name, subtitle: project.folder, status: status.isEmpty ? nil : status.joined(separator: " · "),
                tone: (project.attention ?? 0) > 0 ? "gold" : (project.agents?.working ?? 0) > 0 ? "working" : "muted", folder: project.folder,
                action: destination(.project(id: project.id), nil, project.id))
        }
        for event in snapshot.recentActivity.rows.prefix(limit) {
            add("history", event.id, event.text, subtitle: event.session.name,
                status: [event.status, event.observedAt.map { timeLabel($0) }].compactMap { $0 }.joined(separator: " · "),
                action: destination(event.session.target, event.session, event.session.projectID))
        }
        return result
    }
    private func targetID(_ target: DashboardTarget) -> String {
        switch target {
        case .shell(let id): "shell:\(id)"
        case .chat(let id, _, _): "chat:\(id)"
        case .project(let id): "project:\(id)"
        case .task(let id): "task:\(id)"
        case .automations(let id): "automations:\(id ?? "global")"
        }
    }
}
