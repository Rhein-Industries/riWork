import SwiftUI
import UIKit
import RiWorkCore

// The dashboard: what choosing a desktop opens, before any project. It mirrors the desktop's global dashboard from what the desktop
// sends (`dashboard.get`): how current it is, the global orchestrator (Open, or Start when there is none), the totals, what needs you,
// what is working, what finished in the last hour, tasks in progress, the automations, and every project with its activity and task
// status. A row opens its project, its session or its chat; the global orchestrator opens on the Global screen, outside every project.
// Looking at it, drawing it and reading it again create nothing. A desktop without the dashboard still lists and opens its projects
// here, and the screen says why the rest is missing.

/// At most this many rows of a section on the phone; the rest is counted ("12 more on the Mac").
private let phoneListLimit = 12

struct DashboardView: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel
    /// A project's tabs (a dashboard row asked for its tab first, `RemoteModel.requestProjectTab`).
    var openProject: (RemoteProject) -> Void
    /// The Global screen.
    var openGlobal: () -> Void
    @State private var starting = false

    var body: some View {
        VStack(spacing: 0) {
            ConnectionPanel(model: model)
            DesktopRule()
            // A Mac without the dashboard is never read: the notice below says why, and no freshness is claimed for it.
            if model.dashboardSupport != .unsupported {
                TimelineView(.periodic(from: .now, by: 15)) { context in freshnessLine(model.dashboardFreshness(now: context.date)) }
                DesktopRule()
            }
            List {
                if let reason = model.dashboardUnavailableReason {
                    Label(reason, systemImage: "arrow.down.app").font(style.system(.footnote)).foregroundStyle(style.warning)
                        .listRowBackground(style.background).accessibilityIdentifier("dashboard-unavailable")
                }
                globalSection
                if let dashboard = model.dashboard, model.dashboardSupport != .unsupported {
                    totalsSection(dashboard)
                    attentionSection(dashboard)
                    workingSection(dashboard)
                    if !dashboard.finished.rows.isEmpty { finishedSection(dashboard) }
                    tasksSection(dashboard)
                    automationsSection(dashboard)
                }
                projectsSection
            }
            .listStyle(.plain).scrollContentBackground(.hidden).environment(\.defaultMinListRowHeight, style.pt(40))
            .refreshable { await model.refresh(); await model.refreshDashboardQuietly() }
        }
        .background(style.background)
        // Read again every 10 s while it is on screen, and never while it is not.
        .task { await model.keepFresh(.dashboard) }
    }

    // MARK: How current it is

    private func freshnessLine(_ freshness: DashboardFreshness) -> some View {
        HStack(spacing: 6) {
            Circle().fill(freshness.stale ? style.warning : style.accent).frame(width: 6, height: 6).accessibilityHidden(true)
            Text(freshness.text).font(style.face(11, relativeTo: .caption)).foregroundStyle(freshness.stale ? style.warning : style.muted).lineLimit(2)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12).padding(.vertical, 6).frame(minHeight: style.pt(28))
        .accessibilityElement(children: .combine).accessibilityIdentifier("dashboard-freshness")
    }

    // MARK: The global orchestrator

    private var globalStatus: GlobalOrchestratorStatus {
        GlobalOrchestratorStatus.decide(entry: model.globalTabs.first?.session, dashboard: model.dashboardSupport == .unsupported ? nil : model.dashboard?.global,
                                        canStart: model.orchestratorsOffered, exitedTerminal: model.globalTerminalExited)
    }
    private var globalSection: some View {
        let status = globalStatus
        return Section {
            HStack(spacing: 8) {
                Image(systemName: status.mode == .chat ? (status.provider?.glyph ?? "bubble.left") : "globe").font(.system(size: style.pt(14))).foregroundStyle(style.magenta)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Global orchestrator").font(style.face(13, bold: true, relativeTo: .headline)).foregroundStyle(style.text)
                    Text(status.summary).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted).lineLimit(2)
                }
                Spacer(minLength: 4)
                ActivityIndicator(activity: status.activity)
                globalButton(status)
            }
            .frame(minHeight: style.pt(44))
            .accessibilityElement(children: .contain)
            .listRowBackground(style.background).listRowSeparatorTint(style.divider)
        } header: { sectionHeader("Global") }
    }
    @ViewBuilder private func globalButton(_ status: GlobalOrchestratorStatus) -> some View {
        switch status.action {
        case .open:
            Button(status.actionTitle) { openGlobal() }.buttonStyle(DesktopButtonStyle(prominent: true, compact: true))
                .accessibilityIdentifier("dashboard-open-global")
        case .start:
            Button(starting || model.creatingOrchestrator ? "Starting…" : status.actionTitle) { start() }
                .buttonStyle(DesktopButtonStyle(prominent: true, compact: true))
                .disabled(model.state != .connected || starting || model.creatingOrchestrator)
                .accessibilityIdentifier("dashboard-start-global")
                .accessibilityHint("Starts it on your Mac, outside every project")
        case .unavailable:
            Text(status.actionTitle).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted).multilineTextAlignment(.trailing)
        }
    }
    /// An explicit Start: the Global screen comes up at once (it says it is starting) and the request is sent once; the answer is
    /// shown there, or what went wrong is said. A person who leaves meanwhile is not moved back by the answer.
    private func start() {
        guard !starting else { return }
        starting = true
        openGlobal()
        Task {
            if let failure = await model.startGlobalOrchestrator() { model.error = failure.message }
            starting = false
        }
    }

    // MARK: Totals

    private func totalsSection(_ dashboard: DashboardSnapshot) -> some View {
        let totals = dashboard.totals
        let working = totals.agents?.working, subagents = totals.agents?.subagents ?? 0
        let live: String = {
            if let sessions = totals.liveSessions { return "\(sessions) live" }
            if let shells = totals.liveShells { return "\(shells) terminals live · chats not read yet" }
            return "Live sessions —"
        }()
        let tasks = totals.tasks
        return Section {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 12) {
                    figure(working.map { $0 == 0 ? "None working" : "\($0) working" + (subagents > 0 ? " +\(subagents)" : "") } ?? "Working —", tint: (working ?? 0) > 0 ? style.accent : style.muted)
                    figure(totals.attention.map { $0 == 0 ? "Nothing needs you" : "\($0) need you" } ?? "Needs you —", tint: (totals.attention ?? 0) > 0 ? style.gold : style.muted)
                }
                HStack(spacing: 12) {
                    figure(live, tint: style.muted)
                    figure(totals.activeProjects.flatMap { active in totals.projects.map { "\(active) of \($0) projects active" } } ?? "Projects —", tint: style.muted)
                }
                HStack(spacing: 8) {
                    Text(taskTotals(tasks)).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted)
                    if let done = tasks?.done, let total = tasks?.total, total > 0 {
                        ProgressView(value: Double(done), total: Double(total)).tint(style.accent).frame(maxWidth: 120).accessibilityHidden(true)
                    }
                }
            }
            .padding(.vertical, 4)
            .accessibilityElement(children: .combine).accessibilityIdentifier("dashboard-totals")
            .listRowBackground(style.background).listRowSeparatorTint(style.divider)
        } header: { sectionHeader("Overview") }
    }
    private func figure(_ text: String, tint: Color) -> some View {
        Text(text).font(style.face(12, bold: true, relativeTo: .subheadline)).foregroundStyle(tint).monospacedDigit().lineLimit(1).minimumScaleFactor(0.8)
    }
    private func taskTotals(_ tasks: DashboardTaskCounts?) -> String {
        guard let tasks, let total = tasks.total, let done = tasks.done else { return "Tasks —" }
        if total == 0 { return "No tasks" }
        var parts = ["\(done)/\(total) tasks done"]
        if let progress = tasks.inProgress, progress > 0 { parts.append("\(progress) in progress") }
        if let todo = tasks.todo, todo > 0 { parts.append("\(todo) to do") }
        return parts.joined(separator: " · ")
    }

    // MARK: Needs you, working, finished

    private func attentionSection(_ dashboard: DashboardSnapshot) -> some View {
        Section {
            if dashboard.attention.rows.isEmpty { quiet("Nothing needs you.") }
            ForEach(Array(dashboard.attention.rows.prefix(phoneListLimit).enumerated()), id: \.offset) { _, item in attentionRow(item) }
            more(dashboard.attention, shown: min(phoneListLimit, dashboard.attention.rows.count))
        } header: { sectionHeader("Needs you") }
    }
    private func attentionRow(_ item: DashboardAttention) -> some View {
        let (icon, tint, title, detail): (String, Color, String, String) = {
            let when = item.at.map { "since " + ago($0) } ?? nil
            switch item.kind {
            case .waiting:
                return ("exclamationmark.circle.fill", style.gold, item.session?.name ?? "Agent", [item.session?.place, "Waiting for input", when].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · "))
            case .chatFailed(let message):
                return ("xmark.octagon.fill", style.error, item.session?.name ?? "Chat", [item.session?.place, message.isEmpty ? "Failed" : message].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · "))
            case .automation(let title, let outcome, let message):
                return ("exclamationmark.triangle.fill", style.gold, title, [item.projectName ?? (item.projectID == nil ? "App" : nil), outcome.capitalized, message.isEmpty ? nil : message].compactMap { $0 }.joined(separator: " · "))
            case .other(let word):
                return ("questionmark.circle", style.muted, item.session?.name ?? word, item.session?.place ?? "")
            }
        }()
        return rowButton(destination: model.dashboardDestination(item.target, session: item.session, projectID: item.projectID)) {
            Image(systemName: icon).foregroundStyle(tint).font(.system(size: style.pt(13))).accessibilityHidden(true)
            rowText(title, detail)
        }
    }
    private func workingSection(_ dashboard: DashboardSnapshot) -> some View {
        Section {
            if dashboard.working.rows.isEmpty { quiet("No agent is working.") }
            ForEach(Array(dashboard.working.rows.prefix(phoneListLimit).enumerated()), id: \.offset) { _, work in
                workRow(work, detail: work.sinceUnix.map { "for " + DashboardText.duration(Date().timeIntervalSince1970 - Double($0)) })
            }
            more(dashboard.working, shown: min(phoneListLimit, dashboard.working.rows.count))
        } header: { sectionHeader("Working now") }
    }
    private func finishedSection(_ dashboard: DashboardSnapshot) -> some View {
        Section {
            ForEach(Array(dashboard.finished.rows.prefix(phoneListLimit).enumerated()), id: \.offset) { _, work in
                workRow(work, detail: work.sinceUnix.map { "finished " + ago($0) })
            }
            more(dashboard.finished, shown: min(phoneListLimit, dashboard.finished.rows.count))
        } header: { sectionHeader("Finished in the last hour") }
    }
    private func workRow(_ work: DashboardWork, detail: String?) -> some View {
        rowButton(destination: model.dashboardDestination(work.session.target, session: work.session)) {
            ActivityIndicator(activity: work.activity, subagents: work.subagents ?? 0).frame(minWidth: style.pt(16)).fixedSize()
            rowText(work.session.name, [work.session.place, detail].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · "))
        }
    }

    // MARK: Tasks and automations

    private func tasksSection(_ dashboard: DashboardSnapshot) -> some View {
        Section {
            if dashboard.tasksInProgress.rows.isEmpty { quiet("No task is in progress.") }
            ForEach(dashboard.tasksInProgress.rows.prefix(phoneListLimit)) { task in
                rowButton(destination: .project(id: task.projectID, tab: nil)) {
                    Image(systemName: "circle.lefthalf.filled").foregroundStyle(style.accent).font(.system(size: style.pt(12))).accessibilityHidden(true)
                    rowText(task.title, [task.projectName, task.branch, task.updatedAt.map { "updated " + ago($0) }].compactMap { $0 }.joined(separator: " · "))
                }
            }
            more(dashboard.tasksInProgress, shown: min(phoneListLimit, dashboard.tasksInProgress.rows.count))
        } header: { sectionHeader("Tasks in progress") }
    }
    private func automationsSection(_ dashboard: DashboardSnapshot) -> some View {
        Section {
            switch dashboard.automations {
            case .notRead: quiet("Reading automations…")
            case .unreadable(let message): quiet("Automations could not be read: \(message)", tint: style.warning)
            case .read(let total, let paused, let next, let items):
                if total == 0 { quiet("No automations.") } else {
                    let summary = [total.map { "\($0) automation\($0 == 1 ? "" : "s")" }, paused.flatMap { $0 > 0 ? "\($0) paused" : nil },
                                   next.map { "next: \($0.title) \(until($0.at))" }].compactMap { $0 }.joined(separator: " · ")
                    quiet(summary)
                    ForEach(items.rows.prefix(5)) { item in automationRow(item) }
                    more(items, shown: min(5, items.rows.count))
                }
            }
        } header: { sectionHeader("Automations") }
    }
    private func automationRow(_ item: DashboardAutomation) -> some View {
        let state: String = item.paused ? "Paused" : (item.lastRunFailed ? "Last run \(item.lastOutcome ?? "failed")" : (item.nextRun.map { "Next \(until($0))" } ?? "No next run"))
        let destination: DashboardDestination = item.projectID.map { .project(id: $0, tab: nil) } ?? .none
        return rowButton(destination: destination) {
            Image(systemName: item.paused ? "pause.circle" : (item.lastRunFailed ? "exclamationmark.triangle" : "clock")).foregroundStyle(item.lastRunFailed ? style.gold : style.muted)
                .font(.system(size: style.pt(12))).accessibilityHidden(true)
            rowText(item.title, [item.projectName ?? (item.projectID == nil ? "App" : nil), state].compactMap { $0 }.joined(separator: " · "))
        }
    }

    // MARK: Projects

    /// Every project: the dashboard's, busiest first (the desktop's order), then any the project list has that the dashboard did not send;
    /// without a dashboard, the project list in the order chosen on the Projects screen.
    private var projectsSection: some View {
        let listed = model.dashboardSupport == .unsupported ? [] : (model.dashboard?.projects.rows ?? [])
        let shown = Set(listed.map(\.id))
        let rest = model.sortedProjects.filter { !shown.contains($0.id) }
        return Section {
            ForEach(listed) { project in dashboardProjectRow(project) }
            ForEach(rest) { project in plainProjectRow(project) }
            if listed.isEmpty, rest.isEmpty {
                quiet(model.loading ? "Loading projects…" : "No projects. Open a project on your desktop and refresh.")
            }
        } header: { sectionHeader("Projects") }
    }
    private func dashboardProjectRow(_ project: DashboardProject) -> some View {
        let activity = projectActivity(project)
        return rowButton(destination: .project(id: project.id, tab: nil), identifier: "dashboard-project-\(project.id)") {
            Image(systemName: "folder").font(.system(size: style.pt(14))).foregroundStyle(style.accent).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(project.name).font(style.face(13, bold: true, relativeTo: .headline)).foregroundStyle(style.text).lineLimit(1)
                    if let folder = project.folder { Text(folder).font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1) }
                }
                Text(activity).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted).lineLimit(1)
                Text(DashboardText.tasks(project.tasks)).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted).lineLimit(1)
            }
            Spacer(minLength: 4)
            VStack(alignment: .trailing, spacing: 2) {
                if let attention = project.attention, attention > 0 {
                    Text("\(attention) need\(attention == 1 ? "s" : "") you").font(style.face(11, bold: true, relativeTo: .caption)).foregroundStyle(style.gold)
                }
                if let agents = project.agents, let working = agents.working, let waiting = agents.waiting, working + waiting > 0 {
                    ProjectAgentBadges(agents: ProjectAgents(working: working, waiting: waiting))
                }
                Text(project.lastActivity.map(ago) ?? "—").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted)
            }
        }
    }
    private func projectActivity(_ project: DashboardProject) -> String {
        var parts: [String] = []
        if let working = project.agents?.working, working > 0 { parts.append("\(working) working") }
        if let waiting = project.agents?.waiting, waiting > 0 { parts.append("\(waiting) waiting") }
        if let sessions = project.liveSessions { parts.append(sessions == 0 ? "no live sessions" : "\(sessions) live") }
        else if let shells = project.liveShells { parts.append("\(shells) terminals live") }
        switch project.orchestrator {
        case .terminal(let activity)?: parts.append("orchestrator in a terminal" + (DashboardText.activity(activity).map { " · \($0.lowercased())" } ?? ""))
        case .chat(let provider, let state)?: parts.append("orchestrator \(provider?.chatTitle ?? "chat") · \(DashboardText.chatState(state).lowercased())")
        case nil: break
        }
        return parts.isEmpty ? "Quiet" : parts.joined(separator: " · ")
    }
    private func plainProjectRow(_ project: RemoteProject) -> some View {
        rowButton(destination: .project(id: project.id, tab: nil), identifier: "dashboard-project-\(project.id)") {
            Image(systemName: "folder").font(.system(size: style.pt(14))).foregroundStyle(style.accent).accessibilityHidden(true)
            rowText(project.name, project.root)
            Spacer(minLength: 4)
            if let agents = project.agents, !agents.isIdle { ProjectAgentBadges(agents: agents) }
        }
    }

    // MARK: Pieces

    private func sectionHeader(_ text: String) -> some View {
        Text(style.cased(text)).font(style.system(.caption, weight: .bold)).foregroundStyle(style.muted).accessibilityAddTraits(.isHeader)
    }
    private func quiet(_ text: String, tint: Color? = nil) -> some View {
        Text(text).font(style.face(11, relativeTo: .caption)).foregroundStyle(tint ?? style.muted).listRowBackground(style.background).listRowSeparator(.hidden)
    }
    @ViewBuilder private func more<Row>(_ list: DashboardList<Row>, shown: Int) -> some View {
        let hidden = (list.total ?? list.rows.count) - shown
        if hidden > 0 { quiet("\(hidden) more on the Mac") }
    }
    private func rowText(_ title: String, _ detail: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(style.face(12, bold: true, relativeTo: .subheadline)).foregroundStyle(style.text).lineLimit(1)
            if !detail.isEmpty { Text(detail).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted).lineLimit(2) }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
    /// A row that opens where it leads; one that leads nowhere on the phone is plain text.
    @ViewBuilder private func rowButton(destination: DashboardDestination, identifier: String? = nil, @ViewBuilder label: () -> some View) -> some View {
        let content = HStack(spacing: 8) { label() }.frame(minHeight: style.pt(40)).contentShape(Rectangle())
        Group {
            if destination == .none {
                content.accessibilityElement(children: .combine)
            } else {
                Button { open(destination) } label: { content }.buttonStyle(.plain)
            }
        }
        .accessibilityIdentifier(identifier ?? "")
        .listRowBackground(style.background).listRowInsets(EdgeInsets(top: 2, leading: 12, bottom: 2, trailing: 12)).listRowSeparatorTint(style.divider)
    }
    private func open(_ destination: DashboardDestination) {
        switch destination {
        case .global: openGlobal()
        case .project(let id, let tab):
            guard let project = model.project(withID: id) else {
                model.error = "That project is not in the Mac's project list. Refresh to see the current list."
                Task { await model.refreshProjectsQuietly() }
                return
            }
            model.requestProjectTab(id, key: tab)
            openProject(project)
        case .none: break
        }
    }
    private func ago(_ unix: UInt64) -> String {
        let age = Date().timeIntervalSince1970 - Double(unix)
        return age < 5 ? "just now" : "\(DashboardText.duration(age)) ago"
    }
    private func until(_ unix: UInt64) -> String {
        let wait = Double(unix) - Date().timeIntervalSince1970
        return wait < 5 ? "now" : "in \(DashboardText.duration(wait))"
    }
}

/// The Global screen: the global orchestrator alone, outside every project. A chat opens by its `chat_id`, a terminal by its session id,
/// one the phone cannot open says why in words of its own (never of a project). Back returns to the dashboard; no project is selected,
/// switched or adopted, and each project's selection and remembered tab are as they were.
struct GlobalOrchestratorScreen: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel
    var onBack: () -> Void
    @State private var followOutput = true
    @State private var onScreen = false
    @State private var showingDisplay = false
    @State private var sessionInfo: SessionInfo?
    @State private var chatNotices = 0
    @State private var starting = false

    private var content: TabScreenContent {
        if model.selectedChat != nil { return .chat }
        if model.selectedBlocked != nil { return .unavailable }
        return model.sessionID != nil && !model.terminalCovered ? .terminal : .none
    }
    private var chrome: TabScreenChrome { TabScreenChrome.decide(content: content, focusMode: model.focusMode) }

    var body: some View {
        let chrome = chrome
        VStack(spacing: 0) {
            if chrome.header == .navigationRow {
                header(chrome)
                if let note = model.orchestratorNotice { NoteLine(text: note) { model.clearOrchestratorNotice() } }
            }
            if model.scope == .global, let chat = model.selectedChat {
                ChatScreen(model: model, chat: chat, openNotices: chatNotices).id(chat.id)
            } else if model.scope == .global, let blocked = model.selectedBlocked {
                OrchestratorNotice(session: blocked.session, opening: blocked.opening, global: true)
            } else if model.scope == .global, model.sessionID != nil {
                SessionConsole(model: model, followOutput: $followOutput)
            } else {
                emptyState
            }
        }
        .background((chrome.header == .hidden ? style.terminalBackground : style.background).ignoresSafeArea())
        .statusBarHidden(chrome.statusBarHidden)
        .onAppear { model.enterGlobal(); onScreen = true; syncTerminalVisible() }
        .onDisappear { onScreen = false; syncTerminalVisible(); model.leaveGlobal() }
        .onChange(of: model.terminalCovered) { _, _ in syncTerminalVisible() }
        .onChange(of: model.sessionID) { _, _ in sessionInfo = nil; followOutput = true }
        .onChange(of: model.focusMode) { _, _ in model.updateKeepAwake() }
        // The global orchestrators read again every few seconds while this is on screen: its state, or one started on the Mac.
        .task { await model.keepFresh(.global) }
        .sheet(item: $sessionInfo) { SessionInfoSheet(info: $0).desktopThemed(model.theme.style) }
        .sheet(isPresented: $showingDisplay) { DisplaySettingsSheet(model: model).desktopThemed(model.theme.style) }
    }
    private func syncTerminalVisible() { model.setTerminalVisible(onScreen && model.scope == .global && !model.terminalCovered) }

    private func header(_ chrome: TabScreenChrome) -> some View {
        let tab = model.scope == .global ? model.globalTab : nil
        return HStack(spacing: 0) {
            Button { onBack() } label: { Image(systemName: "chevron.left") }
                .accessibilityLabel("Back to the dashboard")
                .buttonStyle(TargetButtonStyle()).frame(width: style.target)
            HStack(spacing: 6) {
                Image(systemName: tab?.chatInfo.map { $0.provider.glyph } ?? "globe").foregroundStyle(style.magenta).accessibilityHidden(true)
                Text("Global orchestrator").font(style.face(13, bold: true, relativeTo: .headline)).lineLimit(1).accessibilityAddTraits(.isHeader)
                if let chat = tab?.chatInfo { ActivityIndicator(activity: model.chatActivity(chat)) }
                else if let session = tab?.session { ActivityIndicator(activity: session.shownActivity, subagents: session.subagents_working) }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            menu(chrome)
        }
        .frame(minHeight: CGFloat(TabScreenChrome.rowHeight(scale: style.scale)))
        .background(style.panel)
        .overlay(alignment: .bottom) { DesktopRule() }
    }
    private func menu(_ chrome: TabScreenChrome) -> some View {
        Menu {
            if let chat = model.selectedChat { ChatMenuSection(model: model, chat: chat, showNotices: { chatNotices += 1 }) }
            if chrome.terminalActions {
                Section("Terminal") {
                    Button("Focus mode", systemImage: "arrow.up.left.and.arrow.down.right") { model.setFocusMode(true) }
                    Button("Jump to latest output", systemImage: "arrow.down.to.line") { model.jumpToLatest() }
                    Button("Display…", systemImage: "textformat.size") { showingDisplay = true }
                    Button("Copy screen text", systemImage: "doc.on.doc") { UIPasteboard.general.string = model.screenTextForCopy }.disabled(!model.hasOutput)
                    Button("Refresh output", systemImage: "arrow.clockwise") { Task { await model.readOutput() } }.disabled(model.state != .connected)
                }
            }
            Section("Global orchestrator") {
                Button("Session info", systemImage: "info.circle") { showSessionInfo() }.disabled(content == .none)
                Button("Refresh", systemImage: "arrow.clockwise") { Task { await model.refreshGlobalQuietly() } }.disabled(model.state != .connected)
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
            sessionInfo = SessionInfo(id: chat.id, title: "Global orchestrator", cwd: info.cwd, kind: "\(info.provider.chatTitle) · \(info.approvalMode.title)", activity: model.chatState(chat).spokenActivity, since: nil)
        } else if let blocked = model.selectedBlocked {
            sessionInfo = SessionInfo(id: blocked.session.id, title: blocked.session.title, cwd: blocked.session.cwd, kind: blocked.session.kind, activity: nil, since: nil)
        } else if let session = model.session {
            sessionInfo = SessionInfo(id: session.id, title: session.title, cwd: session.cwd, kind: session.kind, activity: session.activitySummary, since: session.activity_since_unix)
        }
    }
    /// No global orchestrator to show (the list has none to open): Start, on a tap, while the desktop can; a Start on its way says so.
    /// Never a wait without end: when the dashboard knows one the list does not show yet, Start opens the one the Mac has
    /// (`orchestrator.create` returns it unchanged) and the next read of the list shows it anyway.
    private var emptyState: some View {
        let busy = starting || model.creatingOrchestrator
        let listedElsewhere: Bool = { switch model.dashboard?.global { case .chat?, .terminal?: true; default: false } }()
        let detail: String = {
            if busy { return "Starting it on your Mac…" }
            if listedElsewhere { return "Your Mac's dashboard has one that its orchestrator list does not show yet. Start opens the one your Mac has." }
            if model.globalTerminalExited { return "Its last terminal has exited. Starting it again opens it on this screen, not in a project." }
            return "It works across every project and belongs to none. Starting it here opens it on this screen, not in a project."
        }()
        return VStack {
            VStack(alignment: .leading, spacing: 12) {
                Label(style.cased("No global orchestrator"), systemImage: "globe").font(style.face(14, bold: true, relativeTo: .headline))
                Text(detail).foregroundStyle(style.muted)
                if model.orchestratorsOffered {
                    Button(busy ? "Starting…" : "Start global orchestrator", systemImage: "play") { start() }
                        .buttonStyle(DesktopButtonStyle(prominent: true))
                        .disabled(model.state != .connected || busy)
                        .accessibilityIdentifier("global-start")
                } else {
                    Text(OrchestratorControlError.unsupportedMessage).font(style.system(.footnote)).foregroundStyle(style.warning)
                }
            }.padding(20).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
            ConnectionPanel(model: model).padding()
        }
    }
    private func start() {
        guard !starting else { return }
        starting = true
        Task {
            if let failure = await model.startGlobalOrchestrator() { model.error = failure.message }
            starting = false
        }
    }
}
