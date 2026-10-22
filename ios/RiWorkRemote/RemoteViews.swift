import SwiftUI
import UIKit
import RiWorkCore

private enum RemoteRoute: Hashable {
    /// A desktop's dashboard: what choosing a desktop opens, outside every project.
    case dashboard(String)
    case projects(String)
    case terminals(RemoteProject)
    /// The global orchestrator's own screen, reached from the dashboard (Back returns there, never into a project).
    case global(String)
}

private struct PairingRequest: Identifiable {
    let id = UUID()
    let text: String
    /// Links can be opened by any app or web page, so they get a details-and-confirm step.
    var fromLink = false
}

struct RemoteRootView: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.scenePhase) private var scenePhase
    @Bindable var model: RemoteModel
    @State private var path: [RemoteRoute] = []
    @State private var pairingRequest: PairingRequest?
    @State private var showingDisplay = false
    @State private var renaming: SavedDesktop?
    @State private var removing: SavedDesktop?
    @State private var newProject: NewProjectSheetModel?
    @State private var hostDashboards = HostDashboardStore()
    @State private var openingHostID: String?
    @State private var dashboardDecision: HostedDashboardDecision?
    /// Responses this phone sent or may have sent, and when: the card cannot go out again until a read after that no longer lists it.
    @State private var sentDecisions: [SentDecision: Date] = [:]
    @State private var tabShortcutOwner = UUID()
    private struct SentDecision: Hashable {
        let hostID, chatID, requestID: String
        init(_ item: HostedDashboardDecision) { hostID = item.hostID; chatID = item.decision.chatID; requestID = item.decision.requestID }
    }
    // A double Back tap during the pop animation would otherwise pop an empty stack and trap.
    private func pop() { if !path.isEmpty { path.removeLast() } }
    /// The "New project" sheet. A project it creates is selected: its terminal screen opens, and with it the "New terminal" sheet, as
    /// the list's "New terminal" action does. The person still has to press Create there; Escape leaves the new project empty.
    private func openNewProject() {
        guard newProject == nil, model.canOpenNewProject else { return }
        let sheet = NewProjectSheetModel(model: model)
        let sheetBinding = $newProject, pathBinding = $path
        let model = model
        sheet.dismiss = { sheetBinding.wrappedValue = nil }
        sheet.onCreated = { project in
            model.newTerminalRequestedProject = project.id
            pathBinding.wrappedValue.append(.terminals(project))
        }
        newProject = sheet
    }
    private var navigationContent: some View {
        NavigationStack(path: $path) {
            landingContent
            .background(style.background)
            .toolbar(.hidden, for: .navigationBar)
            .navigationDestination(for: RemoteRoute.self) { route in
                switch route {
                case .dashboard(let desktopID):
                    VStack(spacing: 0) {
                        WorkspaceBar(title: style.cased("Dashboard"), back: pop) {
                            Button("Projects", systemImage: "folder") { path.append(.projects(desktopID)) }.labelStyle(.iconOnly)
                                .accessibilityHint("Find, sort and create projects")
                        }
                        DashboardView(model: model, openProject: { path.append(.terminals($0)) }, openGlobal: { path.append(.global(desktopID)) },
                                      reviewDecision: { dashboardDecision = HostedDashboardDecision(hostID: desktopID, hostName: model.desktop?.name ?? "Host", decision: $0) })
                    }.background(style.background).id(desktopID).toolbar(.hidden, for: .navigationBar).edgeSwipeBack()
                case .global:
                    GlobalOrchestratorScreen(model: model, onBack: pop)
                        .toolbar(.hidden, for: .navigationBar).edgeSwipeBack()
                case .projects(let desktopID):
                    VStack(spacing: 0) {
                        WorkspaceBar(title: style.cased("Projects"), back: pop) {
                            // A desktop found too old hides it for the connection; none connected dims it.
                            if model.offersNewProject {
                                Button("New project", systemImage: "plus") { openNewProject() }.labelStyle(.iconOnly)
                                    .disabled(!model.canOpenNewProject || newProject != nil)
                                    .accessibilityHint("Creates a project on your Mac")
                            }
                        }
                        ProjectSelectionView(model: model, onSelect: { path.append(.terminals($0)) },
                                             onNewTerminal: { project in model.newTerminalRequestedProject = project.id; path.append(.terminals(project)) },
                                             onNewProject: model.offersNewProject ? { openNewProject() } : nil,
                                             shortcutsActive: path.last == .projects(desktopID) && newProject == nil)
                    }.background(style.background).id(desktopID).toolbar(.hidden, for: .navigationBar).edgeSwipeBack()
                case .terminals(let project):
                    TerminalTabsView(model: model, project: project, onBack: pop, onProjects: {
                        if let id = model.selectedDesktopID { path = [.dashboard(id), .projects(id)] }
                    })
                        .toolbar(.hidden, for: .navigationBar).edgeSwipeBack()
                }
            }
        }
    }
    private var presentedNavigation: some View {
        navigationContent
        .background(style.background.ignoresSafeArea())
        .environment(\.tabShortcuts, model.keyboard.tabShortcuts)
        .background {
            KeyCommandHost(active: rootTabHostActive, actions: [], avoidTextInput: true)
                .environment(\.tabShortcuts, model.keyboard.tabShortcuts)
                .frame(width: 1, height: 1).accessibilityHidden(true)
        }
        .overlay { if !projectTabsOnTop { TabShortcutGuide(controller: model.keyboard.tabShortcuts) } }
        .sheet(item: $pairingRequest) { request in
            PairDesktopSheet(model: model, initialText: request.text, fromLink: request.fromLink, onAdded: { id in
                openHost(id)
            }).desktopThemed(model.theme.style)
        }
        .sheet(isPresented: $showingDisplay) { DisplaySettingsSheet(model: model).desktopThemed(model.theme.style) }
        .sheet(item: $newProject) { NewProjectSheet(sheet: $0).desktopThemed(model.theme.style) }
        .sheet(item: $dashboardDecision) { item in
            DashboardDecisionSheet(item: item, answered: sentDecisions[SentDecision(item)] != nil) { command in try await respondToDecision(item, command: command) }
                .desktopThemed(model.theme.style)
        }
        // Renaming belongs to that one desktop, so it wears that desktop's colors even while another one is shown.
        .sheet(item: $renaming) { RenameDesktopSheet(model: model, desktop: $0).desktopThemed(model.theme.style(for: $0.id)) }
        .alert("Remove pairing?", isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }), presenting: removing) { desktop in
            Button("Remove pairing", role: .destructive) {
                Task { do { try await model.remove(id: desktop.id) } catch { model.error = error.localizedDescription } }
                removing = nil
            }
            Button("Cancel", role: .cancel) { removing = nil }
        } message: { desktop in Text("Removes \(desktop.name)’s local pairing keys. Desktop sessions keep running. Revoke this device on the desktop to deny future access.") }
        .onOpenURL { url in pairingRequest = PairingRequest(text: url.absoluteString, fromLink: true) }
    }
    var body: some View {
        // Two parts, not one expression: as one, the compiler gives up type-checking it.
        observedNavigation
        .onChange(of: model.dashboard) { _, _ in mirrorActiveDashboard() }
        .onChange(of: hostDashboards.hosts) { _, _ in pruneSentDecisions() }
        .task(id: path.isEmpty && scenePhase == .active) {
            installRootTabHandler()
            syncHostDashboards()
            guard path.isEmpty, scenePhase == .active else { return }
            while !Task.isCancelled {
                if model.state == .connected { await model.refreshDashboardQuietly(); mirrorActiveDashboard() }
                do { try await Task.sleep(for: .seconds(15)) } catch { break }
            }
        }
    }
    private var observedNavigation: some View {
        presentedNavigation
        // The global orchestrator opened or started from a project (its menu, the New terminal sheet): the dashboard's Global screen,
        // out of the project, so Back leads to the dashboard and never into a project.
        .onChange(of: model.globalScreenRequests) { _, _ in
            guard let id = model.selectedDesktopID else { return }
            path = [.dashboard(id), .global(id)]
        }
        .onChange(of: path) { _, _ in model.keyboard.tabShortcuts.cancelPrefix(); installRootTabHandler(); syncHostDashboards() }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { model.keyboard.tabShortcuts.cancel() }
            syncHostDashboards()
        }
        .onChange(of: model.desktops.map { "\($0.id):\($0.name)" }) { _, _ in syncHostDashboards() }
        .onChange(of: model.state) { _, _ in syncHostDashboards() }
        .onChange(of: model.keyboard.hardware.isAttached) { _, _ in model.keyboard.tabShortcuts.cancel() }
        .onChange(of: model.keyboard.software.isShown) { _, _ in model.keyboard.tabShortcuts.cancel() }
        .onChange(of: model.keyboard.layout) { _, _ in model.keyboard.tabShortcuts.cancel() }
    }
    private var landingContent: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: style.cased("RiWork")) {
                Button("Display settings", systemImage: "textformat.size") { showingDisplay = true }.labelStyle(.iconOnly)
                Button("Add desktop", systemImage: "plus") { pairingRequest = PairingRequest(text: "") }.labelStyle(.iconOnly).disabled(model.loadFailed)
            }
            if model.loadFailed {
                LibraryFailureView(model: model)
            } else if model.desktops.isEmpty {
                EmptyDesktopOverlay { pairingRequest = PairingRequest(text: "") }
            } else {
                List {
                    HostDashboardSections(store: hostDashboards, openHost: { openHost($0) },
                                          openTarget: { openHost($0, target: $1) }, review: { dashboardDecision = $0 })
                    Section("Desktops") {
                        ForEach(model.desktops) { desktop in
                            HStack(spacing: 0) {
                                Button {
                                    openHost(desktop.id)
                                } label: {
                                    HStack(spacing: 8) {
                                        Image(systemName: "desktopcomputer").font(.system(size: style.pt(14))).foregroundStyle(style.accent)
                                        VStack(alignment: .leading, spacing: 2) {
                                            Text(desktop.name).font(style.face(13, bold: true, relativeTo: .headline)).foregroundStyle(style.text)
                                            Text(desktop.pairing.relayHost).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted)
                                            if let host = hostDashboards.hosts.first(where: { $0.id == desktop.id }) {
                                                Text(host.freshness().text).font(style.face(10, relativeTo: .caption2))
                                                    .foregroundStyle(host.freshness().stale ? style.warning : style.muted).lineLimit(2)
                                            }
                                        }
                                        Spacer(minLength: 4)
                                        Image(systemName: "chevron.right").font(style.system(.caption)).foregroundStyle(style.muted)
                                    }.frame(minHeight: style.pt(44)).contentShape(Rectangle())
                                }.buttonStyle(.plain).accessibilityHint("Open this desktop's dashboard and projects")
                                Menu {
                                    Button("Rename", systemImage: "pencil") { renaming = desktop }
                                    Button("Remove from this device", systemImage: "trash", role: .destructive) { removing = desktop }
                                } label: { Label("Desktop options for \(desktop.name)", systemImage: "ellipsis") }.labelStyle(.iconOnly)
                            }
                            .listRowBackground(style.background)
                            .listRowInsets(EdgeInsets(top: 0, leading: 12, bottom: 0, trailing: 8))
                            .listRowSeparatorTint(style.divider)
                            .contextMenu {
                                Button("Rename", systemImage: "pencil") { renaming = desktop }
                                Button("Remove from this device", systemImage: "trash", role: .destructive) { removing = desktop }
                            }
                            .swipeActions {
                                Button("Remove", role: .destructive) { removing = desktop }
                                Button("Rename") { renaming = desktop }.tint(style.accent)
                            }
                        }
                        Button("Pair a desktop", systemImage: "plus.circle") { pairingRequest = PairingRequest(text: "") }.listRowBackground(style.background)
                    }
                    Section { Label("Pairing keys stay in Keychain.", systemImage: "lock.shield").font(style.system(.caption)).foregroundStyle(style.muted).listRowBackground(style.background) }
                }
                .listStyle(.plain).scrollContentBackground(.hidden)
                .environment(\.defaultMinListRowHeight, style.pt(44))
                .refreshable { await refreshHostDashboards() }
            }
        }
    }
    private func installRootTabHandler() {
        if let route = path.last {
            switch route { case .terminals, .projects: return; default: break }
        }
        model.keyboard.tabShortcuts.setAppHandler(owner: tabShortcutOwner) { action in
            // All hosts belongs to no host: its Tab actions would reach whichever host was chosen last, past `openHost`. Only a host's
            // own screens act, and on that host.
            guard let route = path.last, let id = model.selectedDesktopID else { return }
            switch route {
            case .dashboard(let routeID), .global(let routeID): guard routeID == id else { return }
            default: return
            }
            switch action {
            case .projects: path = [.dashboard(id), .projects(id)]
            case .newTerminal:
                if let projectID = model.projectID, let project = model.project(withID: projectID) {
                    model.newTerminalRequestedProject = projectID
                    path = [.dashboard(id), .terminals(project)]
                }
            case .modelPicker: if model.selectedChat != nil { model.keyboard.requestModelPicker() }
            default: break
            }
        }
    }
    private var projectTabsOnTop: Bool {
        if let route = path.last, case .terminals = route { return true }
        return false
    }
    private var rootTabHostActive: Bool {
        guard scenePhase == .active, model.keyboard.usesClicks, model.keyboard.usesHardwareLayout,
              pairingRequest == nil, !showingDisplay, renaming == nil, removing == nil, newProject == nil,
              dashboardDecision == nil, openingHostID == nil else { return false }
        guard let route = path.last else { return true }
        if case .dashboard = route { return true }
        return false
    }
    /// The host whose one mobile socket belongs to the interactive model. A failed connection that will not retry by itself (no queued
    /// keys, no reconnect on its way) holds none, so the overview reads that host like any other.
    private var activeOverviewHostID: String? {
        guard let id = model.selectedDesktopID else { return nil }
        switch model.state {
        case .connected, .connecting, .suspended: return id
        case .failed: return model.reconnectTask != nil || model.hasPendingKeys(forDesktop: id) ? id : nil
        case .disconnected: return model.wantsConnection ? id : nil
        }
    }
    private func syncHostDashboards() {
        hostDashboards.compressTraffic = { [model] in model.compressTraffic }
        hostDashboards.setVisible(path.isEmpty && openingHostID == nil && !model.loadFailed,
                                  desktops: model.desktops, sceneActive: scenePhase == .active, activeHostID: activeOverviewHostID)
        mirrorActiveDashboard()
    }
    private func mirrorActiveDashboard() {
        guard let id = activeOverviewHostID, let desktop = model.desktops.first(where: { $0.id == id }) else { return }
        let state: HostDashboardEntry.State = model.dashboardSupport == .unsupported ? .unsupported
            : (model.state == .connected ? .ready : (model.state == .connecting ? .refreshing : .offline))
        hostDashboards.updateActiveHost(HostDashboardEntry(id: id, name: desktop.name, snapshot: model.dashboard,
                                                         readAt: model.dashboardReadAt, state: state, failure: model.dashboardFailure))
    }
    private func refreshHostDashboards() async {
        if model.state == .connected { await model.refreshDashboardQuietly(); mirrorActiveDashboard() }
        await hostDashboards.refresh()
    }
    /// Close the overview's temporary mobile socket before handing this exact pairing to the interactive model. The overview stays
    /// paused (and other hosts wait) until the activation is done: it must not register the pairing while the model does.
    private func openHost(_ id: String, target: DashboardTarget? = nil) {
        guard openingHostID == nil, model.desktops.contains(where: { $0.id == id }) else { return }
        openingHostID = id
        Task {
            defer { openingHostID = nil; syncHostDashboards() }
            await hostDashboards.stop()
            // The host's own screen comes up at once and shows the connection's progress there.
            path = [.dashboard(id)]
            await model.activate(id)
            guard model.selectedDesktopID == id else { if path == [.dashboard(id)] { path = [] }; return }
            if let target, model.state == .connected {
                await model.refreshDashboardQuietly()
                // The person may have gone Back or on while this connected: never push onto a stack they have left.
                guard path == [.dashboard(id)], model.selectedDesktopID == id else { return }
                let attention = model.dashboard?.attention.rows.first { $0.target == target }
                let session = attention?.session ?? model.dashboard?.working.rows.first { $0.session.target == target }?.session
                    ?? model.dashboard?.finished.rows.first { $0.session.target == target }?.session
                var projectID = attention?.projectID ?? session?.projectID
                if case .task(let taskID) = target { projectID = model.dashboard?.tasksInProgress.rows.first { $0.id == taskID }?.projectID }
                switch model.dashboardDestination(target, session: session, projectID: projectID) {
                case .global: path.append(.global(id))
                case .project(let projectID, let tabKey):
                    if let project = model.project(withID: projectID) { model.requestProjectTab(projectID, key: tabKey); path.append(.terminals(project)) }
                default: break
                }
            }
        }
    }
    /// The interactive host answers over its own connection; any other host over a temporary one, so answering a card never switches,
    /// disconnects or clears the host in use. Either way the same checks run (`DashboardResponder`).
    private func respondToDecision(_ item: HostedDashboardDecision, command: ChatCommand) async throws {
        let key = SentDecision(item)
        guard sentDecisions[key] == nil else { throw DashboardResponseError.alreadySent }
        guard openingHostID == nil, let desktop = model.desktops.first(where: { $0.id == item.hostID }) else { throw DashboardResponseError.hostUnavailable }
        openingHostID = item.hostID
        defer { openingHostID = nil; syncHostDashboards() }
        // Every overview socket closes first: the response may need this host's one mobile socket.
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
    /// A response is forgotten once a read made after it no longer lists its request (or its host is gone).
    private func pruneSentDecisions() {
        guard !sentDecisions.isEmpty else { return }
        let kept = sentDecisions.filter { key, sentAt in
            guard let host = hostDashboards.hosts.first(where: { $0.id == key.hostID }) else { return false }
            guard let readAt = host.readAt, readAt > sentAt, let snapshot = host.snapshot else { return true }
            return snapshot.decisions.rows.contains { $0.chatID == key.chatID && $0.requestID == key.requestID }
        }
        if kept.count != sentDecisions.count { sentDecisions = kept }
    }
}

private struct LibraryFailureView: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel
    @State private var confirmingReset = false
    @State private var resetError: String?
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label(style.cased("Saved pairings unavailable"), systemImage: "exclamationmark.lock").font(style.face(14, bold: true, relativeTo: .headline))
            Text(model.loadFailure ?? "").foregroundStyle(style.warning).textSelection(.enabled)
            Text("Nothing was changed or deleted. Pairing and removing desktops is paused until this loads.").foregroundStyle(style.muted)
            if let resetError { Text(resetError).foregroundStyle(style.error) }
            Button("Try again", systemImage: "arrow.clockwise") { model.loadLibrary() }.buttonStyle(DesktopButtonStyle(prominent: true))
            Button("Erase saved pairings…", systemImage: "trash", role: .destructive) { confirmingReset = true }
        }.padding(20).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center).background(style.background)
            .alert("Erase saved pairings?", isPresented: $confirmingReset) {
                Button("Erase", role: .destructive) { do { try model.resetLibrary() } catch { resetError = error.localizedDescription } }
                Button("Cancel", role: .cancel) {}
            } message: { Text("Use this only if trying again never works. It deletes every pairing stored on this device, and each desktop has to be paired again.") }
    }
}

private struct EmptyDesktopOverlay: View {
    @Environment(\.desktopStyle) private var style
    var add: () -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label(style.cased("No desktop connected"), systemImage: "terminal").font(style.face(14, bold: true, relativeTo: .headline))
            Text("Pair a desktop, choose a project, then continue in its open terminal tabs.").foregroundStyle(style.muted)
            Button("Pair a desktop", systemImage: "plus", action: add).buttonStyle(DesktopButtonStyle(prominent: true))
        }.padding(20).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center).background(style.background)
    }
}

struct ConnectionPanel: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Circle().fill(model.state == .connected ? style.accent : style.warning).frame(width: 6, height: 6)
                Text(model.state.label).font(style.face(11, relativeTo: .caption))
                Spacer(minLength: 4)
                if model.state == .connecting { ProgressView().controlSize(.small) }
                if model.state == .connected {
                    Button("Refresh", systemImage: "arrow.clockwise") { Task { await model.refresh() } }.labelStyle(.iconOnly).buttonStyle(TargetButtonStyle(tinted: true)).disabled(model.loading)
                    Button("Disconnect", systemImage: "wifi.slash") { Task { await model.disconnect() } }.labelStyle(.iconOnly).buttonStyle(TargetButtonStyle(tinted: true))
                } else {
                    Button(model.state == .connecting ? "Cancel" : "Reconnect", systemImage: model.state == .connecting ? "xmark" : "arrow.clockwise") {
                        Task { if model.state == .connecting { await model.disconnect() } else { await model.connect() } }
                    }.buttonStyle(TargetButtonStyle(tinted: true))
                }
            }
            if let error = model.error {
                HStack(alignment: .top) {
                    Text(error).font(style.system(.caption)).foregroundStyle(style.warning).textSelection(.enabled)
                    Button("Dismiss message", systemImage: "xmark") { model.error = nil }.labelStyle(.iconOnly).buttonStyle(TargetButtonStyle(tinted: true))
                }
            }
        }.padding(.horizontal, 12).background(style.panel)
    }
}

struct ProjectSelectionView: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel
    var onSelect: (RemoteProject) -> Void
    /// Opens a terminal in that project: its screen comes up with the "New terminal" sheet.
    var onNewTerminal: ((RemoteProject) -> Void)?
    /// Opens the "New project" sheet; nil when the desktop cannot create projects.
    var onNewProject: (() -> Void)?
    /// This list is the screen on top and no sheet is over it: only then is ⌘⇧N live.
    var shortcutsActive = true
    @State private var search = ""
    @State private var keyboardProjectID: String?
    @State private var tabShortcutOwner = UUID()
    @FocusState private var searchFocused: Bool
    private var visibleProjects: [RemoteProject] { model.visibleProjects(matching: search) }
    private var highlightedProject: RemoteProject? {
        let projects = visibleProjects
        return projects.first(where: { $0.id == keyboardProjectID }) ?? projects.first
    }
    private var navigationActive: Bool { shortcutsActive && model.keyboard.usesHardwareLayout && !searchFocused }
    var body: some View {
        let projects = visibleProjects
        let highlightedID = model.keyboard.usesHardwareLayout ? highlightedProject?.id : nil
        VStack(spacing: 0) {
            ConnectionPanel(model: model)
            DesktopRule()
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(style.muted)
                TextField("Find a project", text: $search).textFieldStyle(.plain).autocorrectionDisabled()
                    .focused($searchFocused).accessibilityIdentifier("projects-search")
                    .onSubmit { searchFocused = false }
                if !search.isEmpty { Button("Clear filter", systemImage: "xmark") { search = "" }.labelStyle(.iconOnly) }
                if model.projects.count > 1 { ProjectSortMenu(model: model) }
            }.padding(.horizontal, 12).frame(minHeight: style.pt(44))
            DesktopRule()
            if model.snapshotStale && !model.projects.isEmpty {
                Label("Saved projects · reconnect to refresh", systemImage: "clock.badge.exclamationmark")
                    .font(style.system(.caption)).foregroundStyle(style.warning).padding(8)
            }
            ScrollViewReader { proxy in
            List {
                ForEach(projects) { project in
                    Button { keyboardProjectID = project.id; onSelect(project) } label: {
                        HStack(spacing: 8) {
                            Image(systemName: "folder").font(.system(size: style.pt(14))).foregroundStyle(style.accent)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(project.name).font(style.face(13, bold: true, relativeTo: .headline)).foregroundStyle(style.text)
                                Text(project.root).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted).lineLimit(1)
                            }
                            Spacer(minLength: 4)
                            if let agents = project.agents, !agents.isIdle { ProjectAgentBadges(agents: agents) }
                            Image(systemName: "chevron.right").font(style.system(.caption)).foregroundStyle(style.muted)
                        }.frame(minHeight: style.pt(40)).contentShape(Rectangle())
                    }.buttonStyle(.plain).accessibilityHint("Open tabs for this project’s existing terminals")
                        .accessibilityValue(project.agents?.spoken ?? "")
                        .accessibilityAddTraits(highlightedID == project.id ? .isSelected : [])
                        .accessibilityIdentifier("project-\(project.id)").id(project.id)
                        .listRowBackground(highlightedID == project.id ? style.accent.opacity(0.14) : style.background)
                        .listRowInsets(EdgeInsets(top: 2, leading: 12, bottom: 2, trailing: 12))
                        .listRowSeparatorTint(style.divider)
                        .swipeActions(edge: .leading) {
                            if let onNewTerminal { Button("New terminal", systemImage: "plus") { onNewTerminal(project) }.tint(style.accent) }
                        }
                        .contextMenu {
                            if let onNewTerminal { Button("New terminal", systemImage: "plus") { onNewTerminal(project) } }
                        }
                }
                if model.projects.isEmpty {
                    Text(model.loading ? "Loading projects…" : "No projects. Open a project on your desktop and refresh\(onNewProject == nil ? "." : ", or create one.")")
                        .foregroundStyle(style.muted).listRowBackground(style.background)
                    if let onNewProject, !model.loading {
                        Button("New project", systemImage: "plus", action: onNewProject).buttonStyle(DesktopButtonStyle(prominent: true))
                            .disabled(!model.canOpenNewProject).listRowBackground(style.background)
                    }
                }
            }.listStyle(.plain).scrollContentBackground(.hidden).environment(\.defaultMinListRowHeight, style.pt(44))
                .refreshable { await model.refresh() }
                .onChange(of: highlightedProject?.id) { _, id in
                    if let id { withAnimation { proxy.scrollTo(id, anchor: .center) } }
                }
            }
        }.background(style.background)
            // Agent counts change while this list is looked at: read it again every few seconds, only while it is on screen.
            .task { await model.keepFresh(.projects) }
            .onAppear { reconcileProjectSelection(); installProjectTabHandler() }
            .onChange(of: projects.map(\.id)) { _, _ in reconcileProjectSelection() }
            .onDisappear { model.keyboard.tabShortcuts.clearAppHandler(owner: tabShortcutOwner) }
            .onChange(of: shortcutsActive) { _, active in
                if active { installProjectTabHandler() }
                else { model.keyboard.tabShortcuts.clearAppHandler(owner: tabShortcutOwner) }
            }
            .onChange(of: model.keyboard.usesHardwareLayout) { _, _ in
                if shortcutsActive { installProjectTabHandler() }
            }
            // ⌘⇧N. ⌘N is "New terminal" on the terminal screen, so this is the same thing for a project. It is registered only while
            // this list is the screen on top, and nothing on screen here claims it: the terminal's key view (KeyCapture) holds ⌘K and
            // ⌘, and, while the hotkey menu is open, ⌘N and ⌘. ; a person's hotkey shortcuts and the Clicks template (⌘ and ⌘⇧ with
            // E T W A S D B F C Z R L, never N) are matched there too, and that view is on the terminal screen, not here.
            .background {
                KeyCommandHost(active: navigationActive, actions: projectNavigationActions, avoidTextInput: true)
                    .frame(width: 0, height: 0)
                Button("Find a project") { searchFocused = true }
                    .keyboardShortcut("f", modifiers: .command).disabled(!shortcutsActive)
                    .frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
                if let onNewProject {
                    Button("New project", action: onNewProject)
                        .keyboardShortcut("n", modifiers: [.command, .shift])
                        .disabled(!shortcutsActive || !model.canOpenNewProject)
                        .frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
                }
                // ⌘O steps through the sort orders (Recent, Name, Date added). No other shortcut of the app uses it: ⌘K ⌘, ⌘N ⌘⇧N ⌘/ and
                // the Clicks template's letters are taken, and like ⌘⇧N it is live only while this list is the screen on top.
                Button("Next sort order") { model.cycleProjectSort() }
                    .keyboardShortcut("o", modifiers: .command)
                    .disabled(!shortcutsActive || model.projects.count < 2)
                    .frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
            }
    }
    private func reconcileProjectSelection() {
        let projects = visibleProjects
        if !projects.contains(where: { $0.id == keyboardProjectID }) { keyboardProjectID = projects.first?.id }
    }
    private func moveProject(_ direction: Int) {
        guard navigationActive, !visibleProjects.isEmpty else { return }
        let projects = visibleProjects
        let current = projects.firstIndex(where: { $0.id == highlightedProject?.id }) ?? 0
        keyboardProjectID = projects[(current + direction + projects.count) % projects.count].id
    }
    private var projectNavigationActions: [KeyAction] {
        guard navigationActive else { return [] }
        return ClicksNavigation.actions([
            KeyAction(input: "f", modifiers: .command, title: "Find a project") { searchFocused = true },
            KeyAction(input: UIKeyCommand.inputUpArrow, title: "Previous project") { moveProject(-1) },
            KeyAction(input: UIKeyCommand.inputDownArrow, title: "Next project") { moveProject(1) },
            KeyAction(input: "\t", title: "Next project") { moveProject(1) },
            KeyAction(input: "\t", modifiers: .shift, title: "Previous project") { moveProject(-1) },
            KeyAction(input: "\r", title: "Open selected project") {
                if navigationActive, let project = highlightedProject { onSelect(project) }
            },
            KeyAction(input: UIKeyCommand.inputEscape, title: "Clear project filter") { search = "" }
        ], enabled: model.keyboard.usesClicks)
    }
    private func installProjectTabHandler() {
        guard shortcutsActive else { return }
        model.keyboard.tabShortcuts.setAppHandler(owner: tabShortcutOwner) { action in
            guard navigationActive else { return }
            switch action {
            case .newTerminal:
                if model.state == .connected, let project = highlightedProject { onNewTerminal?(project) }
            // These belong to a terminal/chat screen. The project list already is Projects;
            // never forward them to an old selected project's handler.
            case .projects, .selectTab, .modelPicker, .keyMenu: break
            default: break
            }
        }
    }
}

struct TerminalTabsView: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel
    let project: RemoteProject
    var onBack: () -> Void
    var onProjects: (() -> Void)? = nil
    @State private var tabShortcutOwner = UUID()
    @State private var sessionInfo: SessionInfo?
    @State private var showingDisplay = false
    @State private var followOutput = true
    @State private var newTerminal: NewTerminalSheetModel?
    @State private var closing: RemoteSession?
    // The shared tab list (docs/shared-tabs.md), when the desktop has one: the tab asked to be closed (the Detach / Exit sheet), the
    // one being renamed, the "Open shell/worker…" picker, the Edit tabs sheet and the tab a drag is over.
    @State private var closingTab: SharedTab?
    @State private var renamingTab: SharedTab?
    @State private var renameText = ""
    @State private var showingWorkers = false
    @State private var showingEditTabs = false
    @State private var dropTarget: String?
    /// This screen is up (not covered by another one). The terminal is on it only while no chat is.
    @State private var onScreen = false
    /// Counts times the New terminal sheet went away: a chat on screen takes the keyboard back for its composer.
    @State private var chatRefocus = 0
    /// Counts requests from the ⋯ menu to show the chat's notices.
    @State private var chatNotices = 0
    private var openSessions: [RemoteSession] { model.openSessions }
    /// What is under the navigation row, for `TabScreenChrome`.
    private var content: TabScreenContent {
        if model.selectedChat != nil { return .chat }
        if model.selectedBlocked != nil { return .unavailable }
        return model.sessionID != nil && !model.terminalCovered ? .terminal : .none
    }
    /// One row for every kind of tab, so switching between a shell and a chat moves nothing.
    private var chrome: TabScreenChrome { TabScreenChrome.decide(content: content, focusMode: model.focusMode) }
    private var focused: Bool { chrome.header == .hidden }
    /// A physical keyboard leaves the whole screen available: keep the tab strip within reach of its keys. Showing the software
    /// keyboard returns to the touch layout, which keeps the strip clear of the composer's smaller working area.
    private var navigationAtBottom: Bool { model.keyboard.usesHardwareLayout }
    var body: some View {
        let _ = Perf.count("body.TerminalTabsView")
        // Three parts, not one expression: as one, the Release build gives up type-checking it.
        lifecycle(dialogs(screen))
    }
    /// The navigation row over the tab's content, and what follows the session shown.
    @ViewBuilder private var screen: some View {
        let chrome = chrome
        VStack(spacing: 0) {
            // The Global screen has taken over (the global orchestrator was opened from this project and the path replaced): for the
            // moment this screen is still in the hierarchy it draws nothing, so nothing global is shown, followed or sized from here.
            if model.scope == .global {
                Spacer(minLength: 0)
            } else {
            // Focus mode drops all of this: only the shell (and its keyboard) stays.
            if chrome.header == .navigationRow && !navigationAtBottom {
                navigationRow(chrome)
                if let note = model.orchestratorNotice { NoteLine(text: note) { model.clearOrchestratorNotice() } }
            }
            if let chat = model.selectedChat {
                ChatScreen(model: model, chat: chat, refocus: chatRefocus, openNotices: chatNotices).id(chat.id)
            } else if let blocked = model.selectedBlocked {
                OrchestratorNotice(session: blocked.session, opening: blocked.opening)
            } else if model.sessionID == nil && openSessions.isEmpty {
                VStack {
                    VStack(alignment: .leading, spacing: 12) {
                        Label(style.cased("No open terminals"), systemImage: "terminal").font(style.face(14, bold: true, relativeTo: .headline))
                        Text("Open one here, or on your desktop and then refresh.").foregroundStyle(style.muted)
                        Button("New terminal", systemImage: "plus") { openNewTerminal() }.buttonStyle(DesktopButtonStyle(prominent: true))
                            .disabled(model.state != .connected || model.projectID != project.id)
                    }.padding(20).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
                    PendingInputNotice(model: model)
                    ConnectionPanel(model: model).padding()
                }
            } else { SessionConsole(model: model, followOutput: $followOutput) }
            if chrome.header == .navigationRow && navigationAtBottom {
                if let note = model.orchestratorNotice { NoteLine(text: note) { model.clearOrchestratorNotice() } }
                navigationRow(chrome)
                    .overlay(alignment: .top) { if !style.glass { DesktopRule() } }
            }
            }
        }
        .background((focused ? style.terminalBackground : style.background).ignoresSafeArea())
        .overlay(alignment: .topTrailing) {
            if focused && UIDevice.current.userInterfaceIdiom == .phone {
                Text(project.name).font(style.face(9, relativeTo: .caption2)).lineLimit(1)
                    .foregroundStyle(style.terminalForeground).padding(.horizontal, 8).padding(.vertical, 4)
                    .background(.ultraThinMaterial, in: Capsule()).frame(maxWidth: 180, alignment: .trailing).padding(6)
                    .allowsHitTesting(false).accessibilityLabel("Current project: \(project.name)")
                    .accessibilityIdentifier("current-project-focus").chatLayoutProbe("current-project-focus")
            }
        }
        .statusBarHidden(chrome.statusBarHidden)
        .onChange(of: model.sessionID) { _, _ in sessionInfo = nil; followOutput = true }
        .onChange(of: model.focusMode) { _, _ in model.updateKeepAwake() }
        // A confirmation asked on the last connection does not carry over to this one.
        .onChange(of: model.connectionEpoch) { _, _ in closing = nil; closingTab = nil }
        // A desktop that no longer offers shared tabs (a reconnect to an older one): what was open for them goes.
        .onChange(of: model.desktopFeatures.tabs && model.sharedTabs != nil) { _, shared in
            if !shared { showingWorkers = false; showingEditTabs = false; closingTab = nil; renamingTab = nil; dropTarget = nil }
        }
    }
    /// The sheets, the close and rename questions and the alerts over this screen.
    private func dialogs(_ content: some View) -> some View {
        content
        .sheet(isPresented: $showingWorkers) {
            OpenTabSheet(rows: OrchestratorScope.projectOpenable(model.sharedTabs, orchestrators: model.orchestrators), open: { key in
                showingWorkers = false
                tabAction { try await model.openTab(key) }
            }, done: { showingWorkers = false }).desktopThemed(model.theme.style)
        }
        .sheet(isPresented: $showingEditTabs) {
            EditTabsSheet(model: model, move: { update in tabAction { try await model.updateTabs(update) } }, done: { showingEditTabs = false })
                .desktopThemed(model.theme.style)
        }
        // Ask: Detach keeps the session running; Exit stops it (a chat keeps its history). The same words as the Mac's dialog.
        .confirmationDialog(closingTab.map { "Close \($0.title)?" } ?? "", isPresented: Binding(get: { closingTab != nil }, set: { if !$0 { closingTab = nil } }),
                            titleVisibility: .visible, presenting: closingTab) { entry in
            Button("Detach") { close(entry, .detach) }
            if model.canExitTab(entry) { Button("Exit", role: .destructive) { close(entry, .exit) } }
            Button("Cancel", role: .cancel) { closingTab = nil }
        } message: { entry in
            if !model.canExitTab(entry) { Text("Detach keeps the orchestrator running on your Mac. Stop it on the Mac.") }
            else {
                Text(entry.kind == .chat ? "Detach keeps the chat running on your Mac. Exit stops it; its history stays."
                                         : "Detach keeps the shell running on your Mac. Exit closes it and ends its process.")
            }
        }
        .alert("Rename tab", isPresented: Binding(get: { renamingTab != nil }, set: { if !$0 { renamingTab = nil } }), presenting: renamingTab) { entry in
            TextField("Title", text: $renameText)
            Button("Rename") { let title = renameText.trimmingCharacters(in: .whitespacesAndNewlines); tabAction { try await model.renameTab(entry.key, title: title) } }
            Button("Cancel", role: .cancel) {}
        } message: { _ in Text("On your Mac too. Leave it empty to use the session's own name.") }
        .sheet(item: $sessionInfo) { SessionInfoSheet(info: $0).desktopThemed(model.theme.style) }
        .sheet(isPresented: $showingDisplay) { DisplaySettingsSheet(model: model).desktopThemed(model.theme.style) }
        .sheet(item: $newTerminal, onDismiss: { chatRefocus += 1 }) { NewTerminalSheet(sheet: $0).desktopThemed(model.theme.style) }
        .sheet(isPresented: Binding(get: { model.memoEditor != nil }, set: { if !$0 { model.closeMemoEditor() } }), onDismiss: { chatRefocus += 1 }) {
            ProjectMemoSheet(model: model).desktopThemed(model.theme.style)
        }
        .alert("Close terminal?", isPresented: Binding(get: { closing != nil }, set: { if !$0 { closing = nil } }), presenting: closing) { session in
            Button("Close terminal", role: .destructive) { close(session) }
            Button("Cancel", role: .cancel) {}
        } message: { session in Text("Close \(session.title) · \(session.shortID)? This ends its running process on the Mac.") }
    }
    /// ⌘N, and what runs while this screen is up.
    private func lifecycle(_ content: some View) -> some View {
        content
        .environment(\.tabShortcuts, model.keyboard.tabShortcuts)
        .overlay { TabShortcutGuide(controller: model.keyboard.tabShortcuts) }
        // ⌘N, wherever the keyboard is (also in focus mode, where the header is gone). SwiftUI turns it into a UIKeyCommand on the
        // hosting controller, which the terminal's key view passes on: it claims no ⌘ combination.
        .background {
            Button("New terminal") { openNewTerminal() }
                .keyboardShortcut("n", modifiers: .command)
                .disabled(!model.canOpenNewTerminal || model.projectID != project.id || newTerminal != nil)
                .frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
        }
        .background { projectKeyboardShortcuts }
        // A project with no open tab, or one the phone cannot open, has no terminal or composer to take the Tab prefix: this does.
        .background {
            KeyCommandHost(active: tabKeyHostActive, actions: [], avoidTextInput: true)
                .environment(\.tabShortcuts, model.keyboard.tabShortcuts)
                .frame(width: 1, height: 1).accessibilityHidden(true)
        }
        .task(id: project.id) { await model.chooseProject(project.id) }
        // Each tab's state changes while the strip is looked at: read the tabs again every few seconds. Focus mode hides the strip, and
        // the reading goes on: it runs for as long as this screen is up, whatever is on it. A chat on it takes its live worker cards'
        // states from these same lists (`WorkerTimeline`), so they stay fresh while the chat is shown.
        .task(id: project.id) { await model.keepFresh(.sessions) }
        // The terminal is released (its long poll, its pinned size) the moment a chat takes the screen, and picked up again when a terminal tab does.
        .onAppear { onScreen = true; syncTerminalVisible(); openRequestedNewTerminal(); installTabHandler() }
        .onDisappear {
            model.keyboard.tabShortcuts.clearAppHandler(owner: tabShortcutOwner)
            onScreen = false; syncTerminalVisible()
            if model.newTerminalRequestedProject == project.id { model.newTerminalRequestedProject = nil }
            // A tab a dashboard row asked for belongs to this visit (Back during the load): it is not put up on a later one.
            if model.requestedTab?.project == project.id { model.requestedTab = nil }
        }
        .onChange(of: model.terminalCovered) { _, _ in syncTerminalVisible() }
        .onChange(of: model.newTerminalRequestedProject) { _, _ in openRequestedNewTerminal() }
        .onChange(of: model.projectID) { _, _ in openRequestedNewTerminal() }
        .onChange(of: model.state) { _, _ in openRequestedNewTerminal() }
    }
    private func installTabHandler() {
        model.keyboard.tabShortcuts.setAppHandler(owner: tabShortcutOwner) { action in
            guard projectShortcutsActive else { return }
            switch action {
            case .selectTab(let number): selectTab(at: number - 1)
            case .newTerminal: openNewTerminal()
            case .projects: (onProjects ?? onBack)()
            case .modelPicker: if model.selectedChat != nil { model.keyboard.requestModelPicker() }
            default: break
            }
        }
    }
    /// These stay available in both keyboard layouts, including focus mode. Custom terminal hotkeys keep their existing ownership:
    /// while a shell is taking direct input, a binding the person assigned is left to its key view.
    private var projectShortcutsActive: Bool {
        onScreen && model.scope == .project && model.projectID == project.id && newTerminal == nil && sessionInfo == nil
            && !showingDisplay && !showingWorkers && !showingEditTabs && closing == nil && closingTab == nil && renamingTab == nil
            && model.memoEditor == nil
    }
    private var tabKeyHostActive: Bool {
        (content == .none || content == .unavailable) && projectShortcutsActive && model.keyboard.usesClicks && model.keyboard.usesHardwareLayout
    }
    private func terminalOwnsShortcut(_ input: String, shift: Bool = false) -> Bool {
        guard content == .terminal, model.directTyping, let code = HIDKey.code(forCharacter: input) else { return false }
        let chord = KeyChord(keyCode: code, modifiers: shift ? [.command, .shift] : .command)
        return model.hotkeys.shortcutMap.action(for: chord) != nil
    }
    private var projectKeyboardShortcuts: some View {
        Group {
            ForEach(1...9, id: \.self) { number in
                Button("Select tab \(number)") { selectTab(at: number - 1) }
                    .keyboardShortcut(KeyEquivalent(Character(String(number))), modifiers: .command)
                    .disabled(!projectShortcutsActive || model.tabs.count < number || terminalOwnsShortcut(String(number)))
            }
            Button("Previous tab") { cycleTab(-1) }
                .keyboardShortcut("[", modifiers: [.command, .shift])
                .disabled(!projectShortcutsActive || model.tabs.count < 2 || terminalOwnsShortcut("[", shift: true))
            Button("Next tab") { cycleTab(1) }
                .keyboardShortcut("]", modifiers: [.command, .shift])
                .disabled(!projectShortcutsActive || model.tabs.count < 2 || terminalOwnsShortcut("]", shift: true))
            Button("Back", action: onBack)
                .keyboardShortcut("p", modifiers: [.command, .shift])
                .disabled(!projectShortcutsActive || terminalOwnsShortcut("p", shift: true))
            Button("Display settings") { showingDisplay = true }
                .keyboardShortcut(",", modifiers: [.command, .shift])
                .disabled(!projectShortcutsActive || terminalOwnsShortcut(",", shift: true))
            Button("Open a worker or shell") { showingWorkers = true }
                .keyboardShortcut("o", modifiers: [.command, .shift])
                .disabled(!projectShortcutsActive || !model.desktopFeatures.tabs || model.sharedTabs == nil || model.state != .connected
                          || terminalOwnsShortcut("o", shift: true))
        }
        .frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
    }
    private var selectedTabKey: String? {
        model.selectedChatID.map { "chat:\($0)" } ?? model.selectedBlockedID.map { "shell:\($0)" }
            ?? (model.terminalCovered ? nil : model.sessionID.map { "shell:\($0)" })
    }
    private func selectTab(at index: Int) {
        guard projectShortcutsActive, model.tabs.indices.contains(index) else { return }
        switch model.tabs[index] {
        case .terminal(let session): Task { await model.chooseSession(session) }
        case .chat(let chat): model.selectChat(chat.id)
        case .orchestratorChat, .unavailable: model.chooseOrchestrator(model.tabs[index])
        }
    }
    private func cycleTab(_ step: Int) {
        let tabs = model.tabs
        guard !tabs.isEmpty else { return }
        let index = tabs.firstIndex(where: { $0.key == selectedTabKey }) ?? (step > 0 ? tabs.count - 1 : 0)
        selectTab(at: (index + step + tabs.count) % tabs.count)
    }
    /// Back, the tab strip and the menu: the same row, at the same height, over a terminal and over a chat. With no tab open the
    /// project’s name stands where the strip would be. New terminal and Open a worker or shell… are in the menu's Tab section.
    ///
    /// Back and ⋯ match: each is a 44-point target from its screen edge with its glyph centred in it (12 pt from the edge to a 20-point
    /// glyph, 12 pt from it to the strip), and the strip has everything in between. No target reaches over the strip, so a tab scrolled
    /// to either end is still the tab's.
    private func navigationRow(_ chrome: TabScreenChrome) -> some View {
        HStack(spacing: 0) {
            Button { onBack() } label: { Image(systemName: "chevron.left") }
                .accessibilityLabel("Back")
                .buttonStyle(TargetButtonStyle()).frame(width: style.target)
                .chatLayoutProbe("back")
            Group {
                if model.tabs.isEmpty {
                    Text(project.name).font(style.face(13, bold: true, relativeTo: .headline)).lineLimit(1).accessibilityAddTraits(.isHeader)
                        .frame(maxWidth: .infinity, alignment: .leading)
                } else {
                    tabStrip
                }
            }
            .chatLayoutProbe("row-strip")
            screenMenu(chrome).chatLayoutProbe("more-options")
        }
        .clearsWindowControls()
        .accessibilityElement(children: .contain)
        .frame(minHeight: CGFloat(TabScreenChrome.rowHeight(scale: style.scale)))
        .chatLayoutProbe("navigation")
        .chatLayoutProbe("navigation-bottom", visible: navigationAtBottom)
        // Hosted tests and the screenshot harness open what a person reaches through menus (inert outside DEBUG).
        .background {
            Color.clear
                .chatLayoutProbe("open-workers", action: { showingWorkers = true })
                .chatLayoutProbe("edit-tabs", action: { showingEditTabs = true })
                .chatLayoutProbe("display-settings", action: { showingDisplay = true })
                .chatLayoutProbe("close-current", action: { if let entry = currentEntry { requestClose(entry) } })
                .chatLayoutProbe("drop-preview", action: { dropTarget = model.sharedTabs?.visible.dropFirst().first?.key })
                // The terminal's own Close confirmation, as its menu presents it and as its Close button confirms it.
                .chatLayoutProbe("legacy-close-confirmation", visible: closing != nil, action: { if let session = model.session, model.legacyCloseAvailable(session) { closing = session } })
                .chatLayoutProbe("legacy-close-confirm", action: { if let session = closing { close(session) } })
        }
    }
    /// The shared entry of the tab on screen, if the desktop shares its tabs.
    private var currentEntry: SharedTab? {
        guard model.desktopFeatures.tabs else { return nil }
        if let chat = model.selectedChat { return SharedTabStrip.entry(chatID: chat.id, sessionID: nil, in: model.sharedTabs) }
        guard !model.terminalCovered, let session = model.session else { return nil }
        return SharedTabStrip.entry(chatID: nil, sessionID: session.id, in: model.sharedTabs)
    }
    /// A tab's own actions on the shared list: rename, move, close (by the setting); the same in its long-press menu and in ⋯.
    @ViewBuilder private func sharedTabItems(_ entry: SharedTab) -> some View {
        let actions = SharedTabStrip.actions(entry)
        if actions.rename { Button("Rename…", systemImage: "pencil") { renameText = entry.title; renamingTab = entry } }
        // One place at a time within its group, as the Mac's tab menu has it (and a reorder without dragging).
        if let shared = model.sharedTabs {
            let left = SharedTabStrip.moveLeft(entry, in: shared), right = SharedTabStrip.moveRight(entry, in: shared)
            if left != nil || right != nil {
                Button("Move left", systemImage: "arrow.left") { if let left { tabAction { try await model.updateTabs(left) } } }.disabled(left == nil)
                Button("Move right", systemImage: "arrow.right") { if let right { tabAction { try await model.updateTabs(right) } } }.disabled(right == nil)
            }
        }
        Button("Edit tabs…", systemImage: "arrow.up.arrow.down") { showingEditTabs = true }
        if actions.close {
            Button(entry.isWorker ? "Close (detach)" : "Close tab…", systemImage: "xmark") { requestClose(entry) }
        }
    }
    /// Closing by the setting: Ask shows the sheet; Detach and Exit go at once; a worker always detaches.
    private func requestClose(_ entry: SharedTab) {
        switch SharedTabStrip.closePlan(entry, setting: model.tabCloseBehavior, exitable: model.canExitTab(entry)) {
        case .ask, .detachOnly: closingTab = entry
        case .detach: close(entry, .detach)
        case .exit: close(entry, .exit)
        }
    }
    private func close(_ entry: SharedTab, _ choice: TabCloseBehavior) {
        closingTab = nil
        tabAction { try await model.closeTab(entry.key, choice: choice) }
    }
    /// One change to the shared list; its reply is installed by the model, and a failure is said where the screen says things.
    private func tabAction(_ run: @escaping @MainActor () async throws -> Void) {
        Task { @MainActor in do { try await run() } catch { report(error.localizedDescription) } }
    }
    private func report(_ message: String) {
        if let chat = model.selectedChat { model.conversation(chat.id).alerts.show(.action, message) } else { model.error = message }
    }
    /// The screen's one menu, in sections: what is on screen (a chat's actions, or a terminal's), then the tab, then the project and the
    /// connection. A chat has no ⋯ of its own; its row under this one holds only the model, the usage ring and the mode.
    private func screenMenu(_ chrome: TabScreenChrome) -> some View {
        Menu {
            if let chat = model.selectedChat {
                ChatMenuSection(model: model, chat: chat, showNotices: { chatNotices += 1 })
            }
            if chrome.terminalActions {
                Section("Terminal") {
                    Button("Focus mode", systemImage: "arrow.up.left.and.arrow.down.right") { model.setFocusMode(true) }
                    if UIDevice.current.userInterfaceIdiom == .pad { Toggle("Follow output", isOn: $followOutput) }
                    else { Button("Jump to latest output", systemImage: "arrow.down.to.line") { model.jumpToLatest() } }
                    Button("Display…", systemImage: "textformat.size") { showingDisplay = true }
                    Menu("Text size · \(Int(model.terminalFontSize)) pt", systemImage: "textformat.size") {
                        Button("Larger", systemImage: "plus") { model.stepTerminalFontSize(1) }.disabled(model.terminalFontSize >= TerminalFontSize.range.upperBound)
                        Button("Smaller", systemImage: "minus") { model.stepTerminalFontSize(-1) }.disabled(model.terminalFontSize <= TerminalFontSize.range.lowerBound)
                        Button("Reset to \(Int(TerminalFontSize.standard)) pt", systemImage: "arrow.counterclockwise") { model.setTerminalFontSize(TerminalFontSize.standard) }
                    }
                    if model.keysSupport != .unsupported {
                        Toggle("Line composer instead of direct typing", isOn: Binding(get: { model.preferLineComposer }, set: { model.setPreferLineComposer($0) }))
                    }
                    // The screen and the last 500 lines of scrollback above it, not everything loaded.
                    Button("Copy screen text", systemImage: "doc.on.doc") { UIPasteboard.general.string = model.screenTextForCopy }.disabled(!model.hasOutput)
                    Button("Refresh output", systemImage: "arrow.clockwise") { Task { await model.readOutput() } }.disabled(model.state != .connected)
                }
            }
            // The tab: what it is and, for a terminal, closing it. (Closing any tab, and detaching a worker's, belong here too.)
            Section("Tab") {
                // A new tab: the New terminal sheet (a shell, Claude or Codex); with the desktop's shared tabs also the hidden chats and
                // shells (workers first).
                Button("New terminal", systemImage: "plus") { openNewTerminal() }
                    .disabled(model.state != .connected || model.projectID != project.id)
                if model.desktopFeatures.tabs, model.sharedTabs != nil {
                    let hidden = OrchestratorScope.projectOpenable(model.sharedTabs, orchestrators: model.orchestrators).count
                    Button(hidden > 0 ? "Open a worker or shell… (\(hidden))" : "Open a worker or shell…", systemImage: "rectangle.stack.badge.plus") { showingWorkers = true }
                        .disabled(model.state != .connected)
                }
                Button("Session info", systemImage: "info.circle") { showSessionInfo() }.disabled(content == .none)
                if let entry = currentEntry { sharedTabItems(entry) }
                // Without shared tabs only: with them, closing goes by the tab (a worker detaches; Ask / Detach / Exit; Hide before Exit).
                if chrome.terminalActions, let session = model.session, model.legacyCloseAvailable(session) {
                    Button("Close this terminal…", systemImage: "xmark.circle", role: .destructive) { closing = session }
                }
            }
            Section(project.name) {
                // The project's own Markdown notes, shared with the Mac (RemoteModel+Memo.swift).
                Button("Memo…", systemImage: "note.text") { model.openMemoEditor(projectID: project.id, name: project.name) }
                Button("Refresh terminal tabs", systemImage: "arrow.clockwise") { Task { await model.refresh() } }
                // The project's own orchestrator is a row of the New terminal sheet. The global one belongs to no project: this opens
                // the dashboard's Global screen (Open, or Start there), leaving this project's tabs as they are.
                Button("Global orchestrator", systemImage: "globe") { model.requestGlobalScreen() }
                if model.state == .connected { Button("Disconnect", systemImage: "wifi.slash") { Task { await model.disconnect() } } }
                else { Button("Reconnect", systemImage: "arrow.clockwise") { Task { await model.connect() } } }
            }
        } label: {
            Label("More options", systemImage: "ellipsis").labelStyle(.iconOnly)
                .frame(width: style.target, height: style.target).contentShape(Rectangle())
        }
            .buttonStyle(.plain)
    }
    private func showSessionInfo() {
        if let chat = model.selectedChat {
            let info = model.chatConversations[chat.id]?.transcript.info ?? chat
            // An orchestrator that runs as a chat keeps its name, and says which chat it is by the chat's id.
            let orchestrator = model.orchestrator(ofChat: chat.id)
            sessionInfo = SessionInfo(id: chat.id, title: orchestrator?.title ?? ChatTabs.title(info), cwd: info.cwd,
                                      kind: "\(info.provider.chatTitle) · \(info.approvalMode.title)", activity: model.chatState(chat).spokenActivity, since: nil)
        } else if let blocked = model.selectedBlocked {
            sessionInfo = SessionInfo(id: blocked.session.id, title: blocked.session.title, cwd: blocked.session.cwd, kind: blocked.session.kind, activity: nil, since: nil)
        } else if let session = model.session { sessionInfo = SessionInfo(id: session.id, title: session.title, cwd: session.cwd, kind: session.kind, activity: session.activitySummary, since: session.activity_since_unix) }
    }
    /// The terminal is this screen's only while the project's tabs are the scope: the Global screen, coming up over a path replacement
    /// before this one has gone, owns it then.
    private func syncTerminalVisible() {
        guard model.scope == .project else { return }
        model.setTerminalVisible(onScreen && !model.terminalCovered)
    }
    private func openNewTerminal() {
        guard newTerminal == nil, model.state == .connected, model.projectID == project.id, let sheet = NewTerminalSheetModel(model: model) else { return }
        let binding = $newTerminal
        sheet.dismiss = { binding.wrappedValue = nil }
        newTerminal = sheet
    }
    /// The project list's "New terminal": once this project is the loaded one and the link is up, the sheet opens.
    private func openRequestedNewTerminal() {
        guard model.newTerminalRequestedProject == project.id, model.projectID == project.id, model.state == .connected else { return }
        model.newTerminalRequestedProject = nil
        openNewTerminal()
    }
    private func close(_ session: RemoteSession) {
        Task { if let failure = await model.closeTerminal(session) { model.error = failure.message } }
    }
    private var tabStrip: some View {
        ScrollViewReader { proxy in
            ScrollView(.horizontal) {
                HStack(spacing: 0) {
                    ForEach(model.tabs, id: \.key) { tab in
                        let entry = sharedEntry(tab)
                        Group {
                            switch tab {
                            case .terminal(let session): terminalTab(session, entry: entry)
                            case .chat(let chat): chatTab(chat, entry: entry)
                            case .orchestratorChat(let session, let chat): chatTab(chat, orchestrator: session, entry: entry)
                            case .unavailable(let session, let opening): unavailableTab(session, opening)
                            }
                        }
                        .modifier(SharedTabDrag(entry: entry, target: $dropTarget, drop: drop))
                    }
                    // Past the last tab: a drop here puts a tab at the end of its group.
                    if model.sharedTabs != nil, model.desktopFeatures.tabs {
                        Color.clear.frame(width: 32, height: CGFloat(TabScreenChrome.rowHeight(scale: style.scale)))
                            .overlay(alignment: .leading) { if dropTarget == "end" { Capsule().fill(style.accent).frame(width: 3).padding(.vertical, 8) } }
                            .dropDestination(for: String.self) { keys, _ in drop(keys.first, onto: nil) } isTargeted: { dropTarget = $0 ? "end" : (dropTarget == "end" ? nil : dropTarget) }
                    }
                }
            }.scrollIndicators(.hidden)
                .onChange(of: model.sessionID) { _, _ in scrollToSelected(proxy, animated: true) }
                .onChange(of: model.selectedChatID) { _, _ in scrollToSelected(proxy, animated: true) }
                .onChange(of: model.selectedBlockedID) { _, _ in scrollToSelected(proxy, animated: true) }
                // The tab on screen is in view when the row comes up, and when the list changes under it (a move, a tab opened or
                // closed, here or on the Mac).
                .onAppear { scrollToSelected(proxy, animated: false) }
                // A name change adjusts widths too, including live provider names on a desktop without shared tabs.
                .onChange(of: model.tabs.map { "\($0.key)|\($0.title)" }) { _, _ in scrollToSelected(proxy, animated: true) }
        }
    }
    private func scrollToSelected(_ proxy: ScrollViewProxy, animated: Bool) {
        let id = model.selectedChatID.map { "chat:\($0)" } ?? model.selectedBlockedID.map { "shell:\($0)" }
            ?? (model.terminalCovered ? nil : model.sessionID.map { "shell:\($0)" })
        guard let id else { return }
        // After this layout pass, when the tabs have their widths.
        DispatchQueue.main.async { if animated { withAnimation { proxy.scrollTo(id, anchor: .center) } } else { proxy.scrollTo(id, anchor: .center) } }
    }
    /// The shared entry of a tab of the row (nil without the desktop's shared tabs: the old strip).
    private func sharedEntry(_ tab: ProjectTab) -> SharedTab? {
        guard model.desktopFeatures.tabs, let shared = model.sharedTabs else { return nil }
        switch tab {
        case .terminal(let session): return SharedTabStrip.entry(chatID: nil, sessionID: session.id, in: shared)
        case .chat(let info): return SharedTabStrip.entry(chatID: info.id, sessionID: nil, in: shared)
        case .orchestratorChat(let session, let info): return SharedTabStrip.entry(chatID: info.id, sessionID: session.id, in: shared)
        case .unavailable: return nil
        }
    }
    /// A tab dropped on another (it goes before it) or past the last (`target` nil): one Move, only within its sibling and pin group.
    private func drop(_ key: String?, onto target: SharedTab?) -> Bool {
        dropTarget = nil
        guard let key, let shared = model.sharedTabs, let dragged = shared.allEntries.first(where: { $0.key == key }) else { return false }
        guard let update = SharedTabStrip.move(dragged, onto: target, in: shared) else { return false }
        tabAction { try await model.updateTabs(update) }
        return true
    }
    /// A terminal's tab: a shell, an agent, or an orchestrator that runs in a terminal.
    @ViewBuilder private func projectCaption(selected: Bool) -> some View {
        if selected && UIDevice.current.userInterfaceIdiom == .phone {
            Text(project.name + (model.desktops.count > 1 ? " · \(model.desktop?.name ?? "")" : ""))
                .font(style.face(9, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1)
                .accessibilityLabel("Current project: \(project.name)")
                .accessibilityIdentifier("current-project").chatLayoutProbe("current-project")
        }
    }
    private func terminalTab(_ session: RemoteSession, entry: SharedTab? = nil) -> some View {
        let selected = !model.terminalCovered && model.sessionID == session.id
        return Button { Task { await model.chooseSession(session) } } label: {
            VStack(alignment: .leading, spacing: 1) {
                // Orchestrators carry the secondary accent, as they do on the desktop.
                HStack(spacing: 6) {
                    Label { Text(session.title) } icon: {
                        Image(systemName: session.kind == "orchestrator" ? "point.3.connected.trianglepath.dotted" : "terminal")
                            .foregroundStyle(session.kind == "orchestrator" ? style.magenta : style.text)
                    }.font(style.face(12, relativeTo: .subheadline)).lineLimit(1)
                    // With the shared list, the Mac's own status for the tab (idle, working, waiting, error, done, stopped), as its
                    // strip draws it; without it, the shell's activity as before.
                    if let entry, entry.status != .unknown {
                        SharedTabStatusDot(status: entry.status).chatLayoutProbe("status-\(entry.key)=\(entry.status.rawValue)").id(entry.status)
                    } else {
                        ActivityIndicator(activity: session.shownActivity, subagents: session.subagents_working)
                    }
                }
                projectCaption(selected: selected)
            }
            .frame(minHeight: CGFloat(TabScreenChrome.rowHeight(scale: style.scale)))
            .tabChrome(selected: selected, waiting: entry.map { $0.status == .waiting } ?? (session.shownActivity == .waiting))
        }
        .buttonStyle(.plain).id("shell:\(session.id)")
        .accessibilityLabel(["\(session.title), \(tabDetail(session))", entry.flatMap { $0.status == .unknown ? nil : OpenTabSheet.status($0.status) } ?? session.activitySummary]
            .compactMap { $0 }.joined(separator: ", "))
        .accessibilityAddTraits(selected ? .isSelected : [])
        .contextMenu {
            Text(tabDetail(session))
            if let entry { Section { sharedTabItems(entry) } }
            Button("New terminal", systemImage: "plus") { openNewTerminal() }
            if model.legacyCloseAvailable(session) { Button("Close terminal…", systemImage: "xmark.circle", role: .destructive) { closing = session } }
        }
        // The same close as the menu's: by the shared tab when there is one, else the terminal's own.
        .accessibilityActions {
            if let entry {
                Button(entry.isWorker ? "Close (detach)" : "Close tab") { requestClose(entry) }
            } else if model.legacyCloseAvailable(session) {
                Button("Close terminal") { if model.legacyCloseAvailable(session) { closing = session } }
            }
        }
    }
    /// A chat's tab, beside the terminals': its provider's glyph, its name and the same activity indicator. An orchestrator that runs
    /// as a chat has this look too, under the orchestrator's name ("Project orchestrator", already the chat's title here); it is
    /// opened by its chat's id.
    private func chatTab(_ chat: ChatInfo, orchestrator: RemoteSession? = nil, entry: SharedTab? = nil) -> some View {
        let selected = model.selectedChatID == chat.id
        let activity = model.chatActivity(chat)
        let state = model.chatState(chat)
        let branch = model.worktrees.first(where: { $0.id == (orchestrator?.worktree_id ?? chat.worktreeID) })?.branch
        let metadataKnown = model.hasChatMetadata(chat.id)
        let detail = metadataKnown ? ChatTabs.detail(chat, branch: branch) : "Chat"
        return Button { model.selectChat(chat.id) } label: {
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 6) {
                    Label { Text(ChatTabs.title(chat)) } icon: {
                        Image(systemName: metadataKnown ? chat.provider.glyph : "bubble.left.and.text.bubble.right").foregroundStyle(style.accent)
                    }
                        .font(style.face(12, relativeTo: .subheadline)).lineLimit(1)
                    ActivityIndicator(activity: activity)
                    if case .failed = state { Image(systemName: "exclamationmark.triangle.fill").font(style.system(.caption2)).foregroundStyle(style.error).accessibilityHidden(true) }
                }
                projectCaption(selected: selected)
            }
            .frame(minHeight: CGFloat(TabScreenChrome.rowHeight(scale: style.scale)))
            .tabChrome(selected: selected, waiting: activity == .waiting)
            .opacity(state == .stopped && !selected ? 0.6 : 1)
        }
        .buttonStyle(.plain).id("chat:\(chat.id)")
        .accessibilityLabel([ChatTabs.title(chat) + ", " + detail, state.spokenCondition, activity.spoken()].compactMap { $0 }.joined(separator: ", "))
        .accessibilityAddTraits(selected ? .isSelected : [])
        .contextMenu {
            Text(detail)
            if let entry { Section { sharedTabItems(entry) } }
            Button("New terminal", systemImage: "plus") { openNewTerminal() }
            Button("Stop agent", systemImage: "stop.circle", role: .destructive) { Task { await model.stopChat(chat.id) } }.disabled(state == .stopped || model.state != .connected)
        }
    }
    /// An orchestrator that runs as a chat the phone cannot open: its tab says what it is, and choosing it says why it is shut.
    private func unavailableTab(_ session: RemoteSession, _ opening: SessionOpening) -> some View {
        let selected = model.selectedBlockedID == session.id
        let headline = opening.notice?.headline ?? session.title
        return Button { model.chooseOrchestrator(.unavailable(session, opening)) } label: {
            VStack(alignment: .leading, spacing: 1) {
                Label { Text(session.title) } icon: {
                    Image(systemName: session.provider?.glyph ?? "point.3.connected.trianglepath.dotted").foregroundStyle(style.magenta)
                }.font(style.face(12, relativeTo: .subheadline)).lineLimit(1)
                projectCaption(selected: selected)
            }
            .frame(minHeight: CGFloat(TabScreenChrome.rowHeight(scale: style.scale)))
            .tabChrome(selected: selected, waiting: false)
            .opacity(selected ? 1 : 0.7)
        }
        .buttonStyle(.plain).id("shell:\(session.id)")
        .accessibilityLabel("\(session.title), \(headline)")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
    private func tabDetail(_ session: RemoteSession) -> String {
        if let tree = model.worktrees.first(where: { $0.id == session.worktree_id }) { return "\(tree.branch) · \(session.shortID)" }
        return session.shortID
    }
}

/// One cell of the tab strip, a terminal's or a chat's.
/// - Terminal look: the selected tab is underlined in the accent color; one that waits for a person, in gold.
/// - Native: the selected tab is filled with the content's own background, as on the desktop; only waiting keeps a mark, in the
///   signal color, and on glass the cells sit on the screen's surface with no rules between them.
private struct TabChrome: ViewModifier {
    @Environment(\.desktopStyle) private var style
    let selected: Bool
    let waiting: Bool
    func body(content: Content) -> some View {
        let cell = content.padding(.horizontal, 10).frame(minHeight: style.pt(36))
        if style.native {
            cell
                .background(selected ? (style.glass ? style.active : style.background) : (style.glass ? style.surface : style.panel), ignoresSafeAreaEdges: [])
                .overlay(alignment: .bottom) { if waiting { Capsule().fill(style.gold).frame(height: 2).padding(.horizontal, 8) } }
                .padding(.horizontal, style.glass ? 2 : 0)
                .overlay(alignment: .trailing) { if !style.glass { Rectangle().fill(style.divider).frame(width: 1) } }
        } else {
            cell
                .background(selected ? style.active : style.panel, ignoresSafeAreaEdges: [])
                .overlay(alignment: .trailing) { Rectangle().fill(style.divider).frame(width: 1) }
                .overlay(alignment: .bottom) { Rectangle().fill(selected ? style.accent : (waiting ? style.gold : style.divider)).frame(height: waiting ? 2 : 1) }
        }
    }
}
private extension View {
    func tabChrome(selected: Bool, waiting: Bool) -> some View { modifier(TabChrome(selected: selected, waiting: waiting)) }
}

/// A short note under the tab strip that goes by itself ("Project orchestrator is already running."); a tap takes it away.
struct NoteLine: View {
    @Environment(\.desktopStyle) private var style
    let text: String
    let dismiss: () -> Void
    var body: some View {
        Button(action: dismiss) {
            HStack(spacing: 6) {
                Image(systemName: "info.circle").accessibilityHidden(true)
                Text(text).lineLimit(4)
            }
            .font(style.system(.footnote)).foregroundStyle(style.muted)
            .padding(.horizontal, 12).padding(.vertical, 6).frame(maxWidth: .infinity, minHeight: style.target, alignment: .leading)
            .modifier(NoteSurface())
        }
        .buttonStyle(.plain)
        .accessibilityHint("Dismisses the note")
        .onAppear { UIAccessibility.post(notification: .announcement, argument: text) }
    }
    /// A band of the panel color in the terminal look; in Native a rounded panel set in from the edges, on glass on iOS 26, as the
    /// upload line is.
    private struct NoteSurface: ViewModifier {
        @Environment(\.desktopStyle) private var style
        func body(content: Content) -> some View {
            if style.native {
                content.background(style.glass ? Color.clear : style.panel, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                    .nativeGlass(style, in: RoundedRectangle(cornerRadius: 12, style: .continuous)).padding(.horizontal, 8).padding(.vertical, 4)
            } else {
                content.background(style.panel)
            }
        }
    }
}

/// In place of an orchestrator that runs as a chat when there is no chat to open: the RiWork on the Mac has none to offer
/// ("Update the Mac to open this orchestrator"), or the entry did not say which chat it is. The terminal is not tried.
struct OrchestratorNotice: View {
    @Environment(\.desktopStyle) private var style
    let session: RemoteSession
    let opening: SessionOpening
    /// The global orchestrator's, on the Global screen: its words name it and say nothing of a project or its tabs.
    var global = false
    var body: some View {
        let notice = global ? opening.globalNotice : opening.notice
        VStack(alignment: .leading, spacing: 12) {
            Label(style.cased(notice?.headline ?? session.title), systemImage: "arrow.down.app").font(style.face(14, bold: true, relativeTo: .headline))
            Text(notice?.detail ?? "").foregroundStyle(style.muted)
        }
        .accessibilityElement(children: .combine)
        .padding(20).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
    }
}

struct SessionInfo: Identifiable {
    let id: String
    let title: String
    let cwd: String
    let kind: String
    /// What the agent was doing when the sheet opened ("Working, 2 subagents"), and since when (the desktop's Unix seconds).
    var activity: String?
    var since: UInt64?
}

struct SessionInfoSheet: View {
    @Environment(\.desktopStyle) private var style
    let info: SessionInfo
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: style.cased("Session info")) { Button("Done") { dismiss() } }
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text(info.title).font(style.face(14, bold: true, relativeTo: .headline))
                    Text(info.kind).font(style.system(.caption)).foregroundStyle(style.muted)
                    if let activity = info.activity {
                        HStack(spacing: 4) {
                            Text(activity)
                            if let since = info.since { Text("since").foregroundStyle(style.muted); Text(Date(timeIntervalSince1970: TimeInterval(since)), style: .time) }
                        }.font(style.system(.footnote))
                    }
                    DesktopRule()
                    Text(style.cased("Session UUID")).font(style.system(.caption)).foregroundStyle(style.muted)
                    Text(info.id).textSelection(.enabled).accessibilityLabel("Session UUID: \(info.id)")
                    Text(style.cased("Working directory")).font(style.system(.caption)).foregroundStyle(style.muted)
                    Text(info.cwd).textSelection(.enabled)
                }.padding(16).frame(maxWidth: .infinity, alignment: .leading)
            }
        }.desktopSheetSurface(style).foregroundStyle(style.text)
            .font(style.face(13, relativeTo: .body)).tint(style.accent).buttonStyle(DesktopButtonStyle())
            .presentationDetents([.medium, .large])
    }
}

private struct PendingInputNotice: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel
    @State private var acknowledge = false
    var body: some View {
        if let pending = model.pendingInput {
            VStack(alignment: .leading, spacing: 6) {
                Label(model.sending ? "Submitting once…" : "Unconfirmed submission", systemImage: "exclamationmark.bubble").font(style.system(.subheadline, weight: .bold))
                Text("Session \(pending.shellID.prefix(8)) · request \(pending.id.prefix(8))").font(style.face(12, relativeTo: .caption))
                Text(pending.line).font(style.mono(12, relativeTo: .caption)).lineLimit(4).textSelection(.enabled)
                if !model.sending {
                    Text("Check that session’s output. This input will not be resent.").font(style.system(.footnote))
                    Button("I reviewed the output…") { acknowledge = true }.font(style.system(.subheadline)).buttonStyle(TargetButtonStyle(tinted: true))
                }
            }.padding(14).frame(maxWidth: .infinity, alignment: .leading).background(style.warning.opacity(0.12))
                .alert("Acknowledge unconfirmed input?", isPresented: $acknowledge) {
                    Button("Acknowledge after review") { do { try model.acknowledgeUncertainInput() } catch { model.error = error.localizedDescription } }
                    Button("Cancel", role: .cancel) {}
                } message: { Text("This clears the local warning. It does not submit or retry anything. Review the indicated session before creating a new input.") }
        }
    }
}

struct SessionConsole: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel
    @Binding var followOutput: Bool
    @State private var keyFocus = KeyFocus()
    /// The hotkey menu (⌘K) and the editor it leads to.
    @State private var palette = PaletteController()
    /// The hotkey help (⌘/): a reference over the terminal that leaves the keyboard where it is.
    @State private var help = HelpController()
    @State private var editor: HotkeyEditorStart?
    /// A dictated line waiting to be checked before it is typed into the shell (`TerminalDictationPanel`).
    @State private var dictatedLine: String?
    /// The photo or file picker (the key bar's paperclip, or the one beside the keyboard button or Send).
    @State private var picking: AttachmentChoice?
    @Environment(\.scenePhase) private var scenePhase
    /// Live pinch scale. A GestureState resets by itself if the gesture is cancelled; the size is committed (and the
    /// grid recomputed) only when the pinch ends.
    @GestureState private var pinchScale: Double?
    @State private var controlsVisible = true
    @State private var controlsReveal = 0
    /// The scroll pane's size. The screen is at least this big and pinned top-left, like a real terminal whose empty rows are below.
    @State private var paneSize = CGSize.zero
    private var fontSize: Double { pinchScale.map { TerminalFontSize.pinched(from: model.terminalFontSize, scale: $0) } ?? model.terminalFontSize }
    private var focused: Bool { model.focusMode }
    private var shellUploadFailed: Bool { model.sessionID.flatMap { model.uploadActivity(for: .shell($0)) }?.failed == true }
    var body: some View {
        let _ = Perf.count("body.SessionConsole")
        Group {
            if model.sessionID != nil {
                VStack(spacing: 0) {
                    if !focused {
                        StatusStrip(model: model)
                        PendingInputNotice(model: model)
                    }
                    terminal
                    // What went wrong, in the same place and look as a chat's banner row: above the input, with ×, in focus mode too.
                    // It goes by itself when the cause is resolved (a read that succeeds, a reconnect).
                    VStack(spacing: 4) {
                        if let error = model.error {
                            ChatNoticeLine(line: ChatNoticeBanners.Line(id: "shell-error", level: .warning, icon: ChatNoticeBanners.icon(.warning), text: error,
                                                                        close: { model.error = nil }))
                        }
                        if let id = model.sessionID, let activity = model.uploadActivity(for: .shell(id)), case .failed(let message) = activity.phase {
                            ChatNoticeLine(line: ChatNoticeBanners.Line(id: "upload", level: .warning, icon: ChatNoticeBanners.icon(.warning), text: message,
                                                                        close: { model.dismissUploadFailure() }))
                        }
                        if let line = ChatNoticeBanners.dictationLine(.terminal) { ChatNoticeLine(line: line) }
                    }
                    .padding(.vertical, model.error != nil || ChatNoticeBanners.dictationLine(.terminal) != nil || shellUploadFailed ? 4 : 0)
                    bottomPanel
                }
                .background {
                    if model.directTyping {
                        KeyCapture(focus: keyFocus, isEnabled: model.session?.alive == true,
                                   label: "Terminal input for \(model.session?.title ?? "session") \(model.session?.shortID ?? "")",
                                   hotkeys: model.hotkeys.resolvedHotkeys(usingClicks: model.keyboard.usesClicks),
                                   hardwareKeyboard: model.keyboard.usesHardwareLayout, usesClicks: model.keyboard.usesClicks,
                                   shortcuts: model.hotkeys.resolvedShortcuts(usingClicks: model.keyboard.usesClicks), palette: palette, help: help,
                                   onEditHotkeys: { openEditor(.list) }, onNewHotkey: { openEditor(.new) }, onEditHotkey: { openEditor(.edit($0)) },
                                   onKeyEvent: { model.keyboard.events.record($0) },
                                   dictation: DictationController.shared.barState(for: .terminal), onDictate: dictate,
                                   onAttach: openPicker, onPasteFiles: pasteFiles,
                                   onItems: { model.type($0) == .accepted })
                            .frame(width: 1, height: 1).accessibilityHidden(true)
                    }
                }
            } else {
                Text("Choose an open terminal tab.").foregroundStyle(style.muted).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        // The keyboard goes first (`openEditor`), so the editor is not competing with it for the screen, and comes back with the shell.
        .sheet(item: $editor, onDismiss: { keyFocus.restoreAfterModalDismissal() }) {
            HotkeyEditorSheet(store: model.hotkeys, keyboard: model.keyboard, start: $0).desktopThemed(model.theme.style)
        }
        .attachmentPicker($picking, onDone: { keyFocus.restoreAfterModalDismissal() }) { sources in
            if let id = model.sessionID { model.attach(sources, to: .shell(id)) }
        }
        .onAppear { configureFocus(); keyFocus.shellReady(readyShell) }
        .onChange(of: model.sessionID) { _, _ in configureFocus() }
        .onChange(of: readyShell) { _, shell in configureFocus(); keyFocus.shellReady(shell) }
        // A hardware keyboard that arrives after the shell was ready, and the app coming back to the front.
        .onChange(of: model.keyboard.hardware.isAttached) { _, _ in configureFocus(); keyFocus.autoFocus() }
        .onChange(of: scenePhase) { _, phase in if phase == .active { configureFocus(); keyFocus.autoFocus() } }
        .onChange(of: model.keyboard.focusSetting) { _, _ in keyFocus.autoFocus() }
    }
    /// A shell is ready when it is connected, alive and showing its first live screen; that is when it may take the keyboard.
    private var readyShell: String? {
        guard model.directTyping, model.state == .connected, let id = model.sessionID, model.session?.alive == true,
              model.outputSessionID == id, !model.snapshotStale else { return nil }
        return id
    }
    private func configureFocus() {
        keyFocus.shellID = model.sessionID
        keyFocus.policy = { [model] in (model.keyboard.focusSetting, model.keyboard.hardware.isAttached, model.directTyping && model.session?.alive == true) }
    }
    /// Puts the keyboard away, shows the editor, and brings the keyboard back when it closes.
    private func openEditor(_ start: HotkeyEditorStart) {
        palette.close()
        help.close()
        keyFocus.suspendForModal()
        editor = start
    }
    /// Like the editor: the keyboard goes first and comes back when the picker is done. The paperclip's menu leaves the keyboard up
    /// until a choice is made, so a menu closed without one changes nothing.
    private func openPicker(_ choice: AttachmentChoice) {
        palette.close()
        help.close()
        keyFocus.suspendForModal()
        picking = choice
    }
    /// A paste that found files or a lone picture: they go to the Mac and their paths into the shell. False leaves it to the text paste.
    private func pasteFiles() -> Bool {
        let sources = PasteboardAttachments.sources()
        guard let id = model.sessionID, !sources.isEmpty else { return false }
        model.attach(sources, to: .shell(id))
        return true
    }
    /// The iPad keeps the terminal it has always had (two axes, a follow toggle); the iPhone has `PhoneTerminal`.
    private var usesPhoneTerminal: Bool { UIDevice.current.userInterfaceIdiom != .pad }
    private var terminal: some View {
        Group {
            if usesPhoneTerminal { phoneTerminal } else { legacyTerminal }
        }
        .background(style.terminalBackground)
        .background {
            GeometryReader { geometry in
                Color.clear
                    .onAppear { paneSize = geometry.size; model.reportTerminalArea(geometry.size) }
                    .onChange(of: geometry.size) { _, size in paneSize = size; model.reportTerminalArea(size) }
            }
        }
        // The chip and notices float over the pane instead of taking room from it, so they appearing or vanishing never
        // changes the terminal's size (and so never resizes the desktop). Content gets a matching bottom margin.
        .overlay(alignment: .bottom) { FloatingStatus(model: model) }
        .overlay(alignment: .bottom) {
            // Dictation, and its review, exist only while the desktop's mic setting is on.
            if style.mic {
                TerminalDictationPanel(controller: .shared, review: $dictatedLine, canType: model.session?.alive == true, type: typeDictated,
                                       done: { keyFocus.focus() })
            }
        }
        // The setting turned off: a line waiting to be checked goes with the mic, and does not come back with it.
        .onChange(of: style.mic) { _, on in if !on { dictatedLine = nil } }
        // A file on its way to the Mac: at the top, clear of the chip and notices at the bottom.
        .overlay(alignment: .top) {
            if let id = model.sessionID, let activity = model.uploadActivity(for: .shell(id)), !activity.failed {
                UploadStatusBar(activity: activity, cancel: model.cancelUpload, dismiss: model.dismissUploadFailure)
            }
        }
        .onChange(of: model.sessionAutoSwitches) { _, _ in configureFocus(); keyFocus.shellReplacedWithoutTap() }
        .simultaneousGesture(TapGesture().onEnded { terminalTapped() })
        .simultaneousGesture(magnify)
        // Top right, where lines end; top left in focus mode, where the text controls are.
        .overlay(alignment: focused ? .topLeading : .topTrailing) {
            if model.showLatency { LatencyOverlay(model: model) }
        }
        // The key readout (Display settings): what the last key sent. Bottom left, clear of the hotkey menu at the top and of the
        // pending-input chip.
        .overlay(alignment: .bottomLeading) {
            if model.keyboard.showKeyEvents { KeyEventOverlay(log: model.keyboard.events).padding(.bottom, floatingInset) }
        }
        // The hotkey menu (⌘K, or the key bar's ⌘ button): typed into from the keyboard, tapped for touch.
        .overlay(alignment: .top) { HotkeyPaletteView(controller: palette) }
        .animation(.easeInOut(duration: 0.12), value: palette.isOpen)
        // The hotkey help (⌘/, or the key bar's ?): shown in the same place, never together with the menu.
        .overlay(alignment: .top) { HotkeyHelpView(controller: help) }
        .animation(.easeInOut(duration: 0.12), value: help.isOpen)
        .overlay(alignment: .topTrailing) {
            if focused {
                FocusControls(visible: controlsVisible, fontSize: model.terminalFontSize,
                              smaller: { model.stepTerminalFontSize(-1); revealControls() }, larger: { model.stepTerminalFontSize(1); revealControls() },
                              exit: { model.setFocusMode(false) })
            }
        }
        .task(id: controlsReveal) {
            controlsVisible = true
            try? await Task.sleep(for: .seconds(3.5))
            if !Task.isCancelled { controlsVisible = false }
        }
        .onChange(of: focused) { _, _ in revealControls() }
        .accessibilityHint(model.directTyping ? "Double tap to type into the terminal" : "")
        .accessibilityAction(named: "Show keyboard") { keyFocus.focus() }
    }
    /// iPhone: vertical only, sticky bottom, history paging, paged swipes for full-screen programs. One scroll view per shell, so
    /// opening or switching to a shell starts at its bottom.
    private var phoneTerminal: some View {
        PhoneTerminal(model: model, fontSize: fontSize, committedSize: model.terminalFontSize, showCursor: cursorVisible,
                      padding: model.terminalLayout.padding)
            .id(model.sessionID)
    }
    private var legacyTerminal: some View {
        ScrollViewReader { proxy in
            ScrollView([.horizontal, .vertical]) {
                VStack(alignment: .leading, spacing: 0) {
                    if model.state == .connected && !model.viewportReady && model.viewportError == nil && model.output.isEmpty {
                        ProgressView("Fitting desktop terminal…").padding(20)
                    } else if model.output.isEmpty {
                        Text(model.lastOutputAt == nil ? "Output will appear when this session is connected." : "The session has no output yet.").foregroundStyle(style.terminalForeground.opacity(0.6)).padding(20)
                    } else {
                        TerminalScreenView(screen: model.styledOutput, showCursor: cursorVisible, fontSize: fontSize,
                                           committedSize: model.terminalFontSize, boldIsBright: model.boldIsBright)
                            .textSelection(.enabled).padding(model.terminalLayout.padding).accessibilityLabel("Terminal output")
                    }
                    Color.clear.frame(height: 1).id("output-end")
                }.frame(minWidth: paneSize.width, minHeight: max(0, paneSize.height - 1), alignment: .topLeading)
            }
            .defaultScrollAnchor(.bottomLeading, for: .initialOffset)
            .defaultScrollAnchor(.topLeading, for: .alignment)
            .contentMargins(.bottom, floatingInset, for: .scrollContent)
            .onChange(of: floatingInset) { _, _ in if followOutput { proxy.scrollTo("output-end", anchor: .bottomLeading) } }
            .onChange(of: model.output) { _, _ in if followOutput { proxy.scrollTo("output-end", anchor: .bottomLeading) } }
            // Typing pins the view to the bottom, where the prompt and cursor are.
            .onChange(of: model.typedCount) { _, _ in followOutput = true; proxy.scrollTo("output-end", anchor: .bottomLeading) }
        }
    }
    private var magnify: some Gesture {
        MagnifyGesture()
            .updating($pinchScale) { value, state, _ in state = value.magnification }
            .onEnded { value in model.setTerminalFontSize(TerminalFontSize.pinched(from: model.terminalFontSize, scale: value.magnification)) }
    }
    private func revealControls() { controlsReveal += 1 }
    private func terminalTapped() {
        // A link opens (the surface does that); the keyboard stays as it was.
        if model.touchBeganOnLink { model.linkTouchedAt = nil; return }
        if focused { revealControls() }
        if model.directTyping { keyFocus.focus() }
    }
    @ViewBuilder private var bottomPanel: some View {
        if model.directTyping {
            // Present exactly while the keyboard is down, so it changes the pane only when the keyboard does.
            if !focused && !keyFocus.isActive { directBar }
        } else if model.sessionID != nil {
            composer
        }
    }
    /// Room kept under the last line for the chip and notices that float over the pane (the iPad's scroll view; on the iPhone the terminal
    /// reads this itself, so that a key typed does not rebuild this whole screen).
    private var floatingInset: Double { model.floatingInset(style: style) }
    /// Shown while the keyboard is down or something needs saying. Names the shell that keystrokes go to.
    private var directBar: some View {
        HStack(spacing: 8) {
            Image(systemName: "keyboard")
            Text("Tap the terminal to type · \(model.session?.shortID ?? "—")").lineLimit(1)
            Spacer(minLength: 4)
            if model.state != .connected {
                Button("Reconnect") { Task { await model.connect() } }.disabled(model.state == .connecting).buttonStyle(DesktopButtonStyle(compact: true))
            }
            AttachButton(choose: openPicker).equatable().disabled(model.state != .connected || model.session?.alive != true)
            Button(keyFocus.isActive ? "Hide keyboard" : "Show keyboard", systemImage: keyFocus.isActive ? "keyboard.chevron.compact.down" : "keyboard") {
                if keyFocus.isActive { keyFocus.userDismiss() } else { keyFocus.focus() }
            }.labelStyle(.iconOnly).buttonStyle(DesktopButtonStyle(compact: true))
        }.font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted)
            .padding(.leading, 8).frame(minHeight: style.pt(36)).background(style.panel).overlay(alignment: .top) { DesktopRule() }
    }
    /// The line composer: today's behaviour, for desktops that cannot take keys and for anyone who prefers it.
    private var composer: some View {
        VStack(alignment: .leading, spacing: 4) {
            if !focused, let notice = model.deliveryNotice { Text(notice).font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted) }
            HStack {
                CommandField(text: $model.draft, placeholder: "Continue the selected session…", isEnabled: model.canEditDraft,
                             label: "Continuation prompt or terminal command", onSubmit: { if canSubmit { send() } },
                             onRejectedInput: { model.error = "Paste one line at a time. Multi-line input is not sent." })
                    .modifier(DesktopField())
                AttachButton(compact: false, choose: openPicker).equatable().disabled(model.state != .connected || model.session?.alive != true)
                // Only while the desktop's mic setting is on.
                if style.mic { TerminalMicButton(isEnabled: model.canEditDraft, action: dictate) }
                Button("Send", systemImage: "arrow.up", action: send)
                    .labelStyle(.titleAndIcon).buttonStyle(DesktopButtonStyle(prominent: true))
                    .disabled(!canSubmit)
                    .accessibilityLabel("Send to selected terminal")
                    .accessibilityHint("Submits this line once followed by Return")
            }
            if !focused {
                Text("One line + Return · selected \(model.session?.shortID ?? "—")").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted)
                if model.state != .connected { Button("Reconnect") { Task { await model.connect() } }.disabled(model.state == .connecting) }
            }
        }.padding(8).background(style.panel).overlay(alignment: .top) { DesktopRule() }
    }
    private var canSubmit: Bool { model.canSend && !model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    /// The mic (key bar or line composer): the line goes to a field to be checked first, never straight to the shell.
    private func dictate() {
        DictationController.shared.toggle(for: .terminal) { [model] text in
            let line = DictatedText.forTerminal(text)
            if model.directTyping { dictatedLine = DictatedText.joined(dictatedLine ?? "", line) }
            else { model.draft = DictatedText.joined(model.draft, line) }
        }
    }
    /// A checked dictated line, typed into the shell.
    private func typeDictated(_ line: String, submit: Bool) {
        let items: [KeyItem] = submit ? [.text(line), .key(.enter)] : [.text(line)]
        if model.type(items) != .accepted { model.error = "The dictated line could not be typed into this terminal." }
    }
    private func send() {
        guard let selectedID = model.sessionID else { return }
        let line = model.draft
        Task { await model.submit(expectedSessionID: selectedID, line: line) }
    }
    /// The cursor is drawn only for a live, current screen of this session (not while a resize is settling).
    private var cursorVisible: Bool { model.state == .connected && !model.snapshotStale && model.outputSessionID == model.sessionID && model.session?.alive == true }
}

/// The line above the terminal: how fresh the screen is, the link, the terminal size. A view of its own because what it reads
/// (the time of the last answer on an older desktop, the connection) changes on its own schedule and should rebuild only this line.
struct StatusStrip: View {
    @Environment(\.desktopStyle) private var style
    let model: RemoteModel
    var body: some View {
        HStack(spacing: 6) {
            // The two symbols differ in height by a fraction of a point, so this strip's height follows the state; the terminal under
            // it ignores sub-point changes of its area (`reportTerminalArea`), or the two could feed each other without end.
            Image(systemName: outputStale ? "clock.badge.exclamationmark" : "checkmark.circle")
            Text(outputStale ? "Stale · \(model.state.label.lowercased())" : (model.syncMode == .live ? "Live" : "Latest snapshot"))
            if model.syncMode != .live || outputStale, let date = model.lastOutputAt { Text(date, style: .time) }
            if model.outputInMode { CopyModeBadge() }
            Spacer(minLength: 4)
            if let viewport = model.appliedViewport, model.viewportSessionID == model.sessionID {
                Text("\(viewport.columns)×\(viewport.rows)").monospacedDigit()
                    .accessibilityLabel("Terminal size \(viewport.columns) columns, \(viewport.rows) rows")
            }
        }.font(style.face(10, relativeTo: .caption2)).foregroundStyle(outputStale ? style.warning : style.muted)
            .padding(.horizontal, 8).padding(.vertical, 2).background(style.panel)
    }
    private var outputStale: Bool { model.state != .connected || model.snapshotStale || model.outputSessionID != model.sessionID || model.session?.alive != true || !model.viewportReady }
}

// MARK: - Swiping back

extension View {
    /// The standard swipe from the left edge goes back, as on any pushed screen, although this screen hides the navigation bar for a
    /// header of its own (UIKit turns the gesture off with the bar). The navigation stack's own gesture is used, so the swipe is the
    /// system's (interactive, cancellable) and the stack's path follows it as it does the Back button.
    func edgeSwipeBack() -> some View { background(EdgeSwipeBack().frame(width: 0, height: 0).accessibilityHidden(true)) }
}

/// Turns the navigation controller's edge swipe back on for the screen it is on. Only that gesture: it begins only at the screen's
/// left edge (the tab row's Back is there, the terminal, its key bar and the tab strip start further in), and scrolling views wait
/// for it to fail there, so a drag from the edge goes back instead of scrolling.
struct EdgeSwipeBack: UIViewControllerRepresentable {
    func makeUIViewController(context: Context) -> Controller { Controller() }
    func updateUIViewController(_ controller: Controller, context: Context) { controller.enable() }

    final class Controller: UIViewController {
        override func viewWillAppear(_ animated: Bool) { super.viewWillAppear(animated); enable() }
        override func viewDidAppear(_ animated: Bool) { super.viewDidAppear(animated); enable() }
        func enable() {
            guard let navigation = navigationController, let gesture = navigation.interactivePopGestureRecognizer else { return }
            EdgeSwipeBackDelegate.shared.navigation = navigation
            gesture.delegate = EdgeSwipeBackDelegate.shared
            gesture.isEnabled = true
        }
    }
}

@MainActor final class EdgeSwipeBackDelegate: NSObject, UIGestureRecognizerDelegate {
    static let shared = EdgeSwipeBackDelegate()
    weak var navigation: UINavigationController?
    func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
        // Not on the first screen (nothing to go back to), and not while a push or pop is already moving.
        guard let navigation else { return false }
        return navigation.viewControllers.count > 1 && navigation.transitionCoordinator == nil && navigation.presentedViewController == nil
    }
    func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer, shouldBeRequiredToFailBy other: UIGestureRecognizer) -> Bool {
        // A scroll view under the edge (the tab strip, the transcript, the composer) waits for the edge swipe to fail.
        other.view is UIScrollView
    }
}

// MARK: - Shared tabs

/// A tab of the row as a drag source and a drop target for reordering: a long press lifts it (the row's horizontal scroll and the
/// screen's edge swipe are plain drags and are untouched), dropping it on another tab puts it before that one, within its group. A
/// bar shows where it would go.
private struct SharedTabDrag: ViewModifier {
    @Environment(\.desktopStyle) private var style
    let entry: SharedTab?
    @Binding var target: String?
    let drop: (String?, SharedTab?) -> Bool
    func body(content: Content) -> some View {
        if let entry {
            // A tab keeps the width of its title: dragging wraps it in a view that would otherwise propose less.
            content.fixedSize(horizontal: true, vertical: false)
                .draggable(entry.key) {
                    Text(entry.title).font(style.face(12, relativeTo: .subheadline)).lineLimit(1).padding(.horizontal, 12).padding(.vertical, 8)
                        .background(style.active, in: Capsule())
                }
                .dropDestination(for: String.self) { keys, _ in drop(keys.first, entry) } isTargeted: { over in
                    if over { target = entry.key } else if target == entry.key { target = nil }
                }
                .overlay(alignment: .leading) { if target == entry.key { Capsule().fill(style.accent).frame(width: 3).padding(.vertical, 8).accessibilityHidden(true) } }
                .chatLayoutProbe("tab-\(entry.key)")
        } else {
            content
        }
    }
}

/// "Open shell/worker…": the hidden chats and shells of the project, workers first, each with its kind, state and the tab it belongs
/// to. Opening one shows it as a tab here and on the Mac (it starts nothing).
struct OpenTabSheet: View {
    @Environment(\.desktopStyle) private var style
    let rows: [SharedTabStrip.Openable]
    let open: (String) -> Void
    let done: () -> Void
    var body: some View {
        NavigationStack {
            List(rows) { row in
                Button { open(row.tab.key) } label: {
                    HStack(spacing: 10) {
                        Image(systemName: row.tab.kind == .chat ? "bubble.left.and.text.bubble.right" : "terminal").foregroundStyle(style.accent)
                            .frame(width: 24).accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(row.tab.title).font(style.system(.body)).foregroundStyle(style.text).lineLimit(1)
                            Text(detail(row)).font(style.system(.caption)).foregroundStyle(style.muted).lineLimit(1)
                        }
                        Spacer(minLength: 4)
                        HStack(spacing: 5) {
                            SharedTabStatusDot(status: row.tab.status)
                            Text(Self.status(row.tab.status)).font(style.system(.caption, weight: .medium)).foregroundStyle(row.tab.status == .waiting ? style.gold : (row.tab.status == .error ? style.error : style.muted))
                        }
                    }
                    .frame(minHeight: style.target).contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .listRowBackground(style.background)
                .accessibilityLabel("\(row.tab.title), \(detail(row)), \(Self.status(row.tab.status))").accessibilityHint("Opens it as a tab")
                .chatLayoutProbe("open-\(row.tab.key)", action: { open(row.tab.key) })
            }
            .listStyle(.plain).scrollContentBackground(.hidden).background(style.background)
            .overlay { if rows.isEmpty { Text("Nothing hidden to open.").font(style.system(.footnote)).foregroundStyle(style.muted) } }
            .navigationTitle("Open shell/worker").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done", action: done) } }
        }
        .presentationDetents([.medium, .large])
    }
    private func detail(_ row: SharedTabStrip.Openable) -> String {
        let kind = row.tab.kind == .chat ? "Chat" : "Shell"
        let role = row.tab.isWorker ? "Worker" : kind
        return [row.tab.isWorker ? "\(role) · \(kind.lowercased())" : role, row.parentTitle.map { "in \($0)" }].compactMap { $0 }.joined(separator: " · ")
    }
    static func status(_ status: SharedTab.Status) -> String {
        switch status {
        case .working: "Working"
        case .waiting: "Waiting"
        case .error: "Error"
        case .done: "Done"
        case .idle: "Idle"
        case .stopped: "Stopped"
        case .unknown: ""
        }
    }
}

/// A shared tab's state as the Mac's strip draws it: working in the accent, waiting gold, error red, done muted, idle a small muted
/// point (a live shell with nothing running), stopped an empty ring.
struct SharedTabStatusDot: View {
    @Environment(\.desktopStyle) private var style
    let status: SharedTab.Status
    var body: some View {
        let size = style.pt(7)
        Group {
            switch status {
            case .working: Circle().fill(style.accent).frame(width: size, height: size)
            case .waiting: Circle().fill(style.gold).frame(width: size, height: size)
            case .error: Circle().fill(style.error).frame(width: size, height: size)
            case .done: Circle().fill(style.muted).frame(width: size, height: size)
            case .idle: Circle().fill(style.muted.opacity(0.7)).frame(width: size * 0.55, height: size * 0.55)
            case .stopped: Circle().stroke(style.muted, lineWidth: 1).frame(width: size, height: size)
            case .unknown: EmptyView()
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// Edit tabs: the row's tabs in groups (the top-level tabs, each parent's opened children) with drag handles; a move is one Move of
/// the shared list within its group. The accessible way to reorder, beside dragging in the row.
struct EditTabsSheet: View {
    @Environment(\.desktopStyle) private var style
    let model: RemoteModel
    let move: (TabUpdate) -> Void
    let done: () -> Void
    var body: some View {
        NavigationStack {
            List {
                if let shared = model.sharedTabs {
                    ForEach(OrchestratorScope.projectGroups(shared, orchestrators: model.orchestrators)) { group in
                        Section(group.title) {
                            ForEach(group.tabs) { tab in
                                HStack(spacing: 10) {
                                    Image(systemName: tab.kind == .chat ? "bubble.left.and.text.bubble.right" : "terminal").foregroundStyle(style.accent).frame(width: 24)
                                        .accessibilityHidden(true)
                                    Text(tab.title).foregroundStyle(style.text).lineLimit(1)
                                    Spacer(minLength: 4)
                                }
                                .frame(minHeight: style.target)
                                .listRowBackground(style.background)
                            }
                            .onMove { from, to in
                                guard let source = from.first, let update = SharedTabStrip.move(in: group.tabs, from: source, to: to) else { return }
                                move(update)
                            }
                        }
                    }
                }
            }
            .listStyle(.insetGrouped).scrollContentBackground(.hidden).background(style.background)
            .environment(\.editMode, .constant(.active))
            .navigationTitle("Edit tabs").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done", action: done) } }
        }
        .presentationDetents([.medium, .large])
    }
}
