import SwiftUI
import RiWorkCore

struct HostedDashboardDecision: Identifiable {
    let hostID: String
    let hostName: String
    let decision: DashboardDecision
    var saved = false
    var id: String { "\(hostID):\(decision.id)" }
}

/// The phone's landing screen combines only paired hosts. Every card carries its host identity into navigation and responses.
struct HostDashboardSections: View {
    @Environment(\.desktopStyle) private var style
    let store: HostDashboardStore
    var openHost: (String) -> Void
    var openTarget: (String, DashboardTarget) -> Void
    var review: (HostedDashboardDecision) -> Void
    @State private var filter = Filter.all
    private enum Filter: String, CaseIterable { case all = "All", decisions = "Needs you", working = "Working", projects = "Projects" }
    var body: some View {
        let aggregate = store.aggregate
        let decisions = aggregate.decisionRows.map { HostedDashboardDecision(hostID: $0.hostID, hostName: $0.hostName, decision: $0.value, saved: $0.stale) }
        Section {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Label("All hosts", systemImage: "rectangle.3.group").font(style.face(17, bold: true, relativeTo: .headline))
                    Spacer()
                    Text("\(aggregate.freshHostCount)/\(aggregate.totalHostCount) current").font(style.system(.caption)).foregroundStyle(style.muted)
                }
                LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], alignment: .leading, spacing: 12) {
                    metric("Working", aggregate.working, symbol: "bolt", tint: style.accent)
                    metric("Needs you", aggregate.attention, symbol: "hand.raised", tint: style.gold)
                    metric("Projects", aggregate.projects, symbol: "folder", tint: style.text)
                    metric("Open tasks", aggregate.openTasks, symbol: "checklist", tint: style.text)
                }
                if aggregate.staleHostCount > 0 || aggregate.unavailableHostCount > 0 {
                    Text("\(aggregate.staleHostCount) saved · \(aggregate.unavailableHostCount) not read. Totals include the last successful reads.")
                        .font(style.system(.caption)).foregroundStyle(style.warning)
                }
                Picker("Dashboard view", selection: $filter) {
                    ForEach(Filter.allCases, id: \.self) { Text($0.rawValue).tag($0) }
                }.pickerStyle(.segmented)
            }.padding(.vertical, 8).accessibilityIdentifier("all-hosts-dashboard")
        }.listRowBackground(style.background)

        if filter == .all {
            ForEach(store.hosts.filter { $0.snapshot?.intelligence.state != "disabled" && $0.snapshot != nil }) { host in
                if let intelligence = host.snapshot?.intelligence {
                    Section("\(host.name) · Overview") {
                        DashboardIntelligenceCard(intelligence: intelligence).listRowBackground(style.background)
                    }
                }
            }
        }

        if filter == .all || filter == .decisions {
            Section("Decisions") {
                if decisions.isEmpty {
                    Text(aggregate.sampledHostCount == 0 ? "Read a host to see pending decisions." : "No pending decisions in the host snapshots.")
                        .font(style.system(.footnote)).foregroundStyle(style.muted)
                }
                ForEach(decisions.prefix(20)) { item in
                    Button { review(item) } label: {
                        VStack(alignment: .leading, spacing: 5) {
                            HStack {
                                Label("\(item.hostName) · \(item.decision.session.name)", systemImage: "hand.raised")
                                Spacer(minLength: 4)
                                if item.saved { Text("Saved") }
                            }.font(style.face(11, bold: true, relativeTo: .caption)).foregroundStyle(style.gold)
                            Text(item.decision.title).font(style.face(14, relativeTo: .body)).foregroundStyle(style.text).lineLimit(3)
                            HStack {
                                Text(item.decision.session.place).font(style.system(.caption)).foregroundStyle(style.muted)
                                Spacer()
                                Label("Review", systemImage: "chevron.right").font(style.system(.caption)).foregroundStyle(style.accent)
                            }
                        }.padding(.vertical, 6).frame(maxWidth: .infinity, minHeight: 44, alignment: .leading).contentShape(Rectangle())
                    }.buttonStyle(.plain).accessibilityIdentifier("host-decision-\(item.id)")
                }
                // Past the 20 shown, and those a host counted but did not send: one host's many must not hide that there are more.
                let unseen = max(0, decisions.count - 20) + store.hosts.reduce(0) { $0 + ($1.snapshot?.decisions.more ?? 0) }
                if unseen > 0 {
                    Text("\(unseen) more \(unseen == 1 ? "decision" : "decisions"). Open a host to see all of its own.")
                        .font(style.system(.caption)).foregroundStyle(style.muted)
                }
                if store.hosts.contains(where: { $0.snapshot?.decisionSources.complete == false }) {
                    Text("Some hosts have partial decision coverage. Open a host or chat for the full state.").font(style.system(.caption)).foregroundStyle(style.muted)
                }
            }.listRowBackground(style.background)
            if !aggregate.attentionRows.isEmpty {
                Section("Other attention") {
                    ForEach(aggregate.attentionRows.prefix(8)) { row in
                        targetRow(host: row.hostName, title: row.value.session?.name ?? row.value.projectName ?? "Needs attention",
                                  detail: row.value.session?.place ?? "Open host for details", stale: row.stale) { openTarget(row.hostID, row.value.target) }
                    }
                }.listRowBackground(style.background)
            }
        }
        if filter == .all || filter == .working {
            Section("Working now") {
                if aggregate.workingRows.isEmpty { Text("No working agents in the host snapshots.").font(style.system(.footnote)).foregroundStyle(style.muted) }
                ForEach(aggregate.workingRows.prefix(12)) { row in
                    targetRow(host: row.hostName, title: row.value.session.name, detail: row.value.session.place, stale: row.stale) {
                        openTarget(row.hostID, row.value.session.target)
                    }
                }
            }.listRowBackground(style.background)
        }
        if filter == .all || filter == .projects {
            Section("Projects across hosts") {
                ForEach(aggregate.projectRows.prefix(filter == .projects ? 150 : 8)) { row in
                    targetRow(host: row.hostName, title: row.value.name,
                              detail: "\(row.value.agents?.working.map(String.init) ?? "—") working · \(row.value.tasks?.open.map(String.init) ?? "—") open tasks", stale: row.stale) {
                        openTarget(row.hostID, .project(id: row.value.id))
                    }
                }
            }.listRowBackground(style.background)
        }
    }
    private func metric(_ title: String, _ count: HostDashboardCount, symbol: String, tint: Color) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Label(title, systemImage: symbol).font(style.system(.caption)).foregroundStyle(style.muted)
            Text(count.value.map(String.init) ?? "—").font(style.face(25, bold: true, relativeTo: .title2)).foregroundStyle(tint).monospacedDigit()
            if !count.isComplete { Text("\(count.knownHosts)/\(count.totalHosts) hosts read").font(style.system(.caption2)).foregroundStyle(style.muted) }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
    private func targetRow(host: String, title: String, detail: String, stale: Bool, open: @escaping () -> Void) -> some View {
        Button(action: open) {
            VStack(alignment: .leading, spacing: 3) {
                HStack {
                    Text(host).foregroundStyle(style.accent)
                    if stale { Text("Saved").foregroundStyle(style.warning) }
                }.font(style.face(10, bold: true, relativeTo: .caption2))
                Text(title).font(style.face(13, bold: true, relativeTo: .headline)).foregroundStyle(style.text).lineLimit(2)
                if !detail.isEmpty { Text(detail).font(style.system(.caption)).foregroundStyle(style.muted).lineLimit(2) }
            }.frame(maxWidth: .infinity, minHeight: 44, alignment: .leading).contentShape(Rectangle())
        }.buttonStyle(.plain)
    }
}

struct DashboardIntelligenceCard: View {
    @Environment(\.desktopStyle) private var style
    let intelligence: DashboardIntelligence
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label("Haiku overview", systemImage: "sparkles").font(style.face(12, bold: true, relativeTo: .headline))
                Spacer()
                if intelligence.stale { Text("Saved").foregroundStyle(style.warning).font(style.system(.caption)) }
            }
            if let summary = intelligence.summary {
                Text(summary).font(style.face(14, relativeTo: .body)).textSelection(.enabled)
                ForEach(intelligence.highlights) { row in
                    Label(row.text, systemImage: "circle.fill").font(style.system(.footnote)).foregroundStyle(style.text)
                }
                ForEach(intelligence.followups) { row in
                    Label(row.text, systemImage: "arrow.turn.down.right").font(style.system(.footnote)).foregroundStyle(style.muted)
                }
            } else {
                Text(intelligence.error ?? "Gathering observed work for the overview…").font(style.system(.footnote)).foregroundStyle(style.muted)
            }
            Text("AI summary of observed activity").font(style.system(.caption2)).foregroundStyle(style.muted)
        }.padding(.vertical, 6).accessibilityIdentifier("dashboard-intelligence")
    }
}

struct DashboardDecisionSheet: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.dismiss) private var dismiss
    let item: HostedDashboardDecision
    /// A response to this request already went out from this phone (`RemoteRootView` remembers it until a read no longer lists it).
    var answered = false
    let respond: @MainActor (ChatCommand) async throws -> Void
    @State private var busy = false
    @State private var sent = false
    @State private var failure: String?
    @State private var uncertain = false
    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: style.cased("Decision")) { Button("Done") { dismiss() }.disabled(busy) }
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text(item.hostName).font(style.face(17, bold: true, relativeTo: .headline))
                    Text([item.decision.session.name, item.decision.session.place].filter { !$0.isEmpty }.joined(separator: " · "))
                        .font(style.system(.caption)).foregroundStyle(style.muted)
                    if sent {
                        Label("Response sent", systemImage: "checkmark.circle.fill").foregroundStyle(style.accent)
                    } else if answered && failure == nil && !busy {
                        Text(item.decision.title).textSelection(.enabled)
                        Text(DashboardResponseError.alreadySent.localizedDescription).foregroundStyle(style.warning)
                    } else {
                        switch item.decision.content {
                        case .approval(let approval):
                            if approval.choices.isEmpty {
                                Text(approval.title).textSelection(.enabled)
                                Text("This host offers choices this app cannot read. Open the chat to answer.").foregroundStyle(style.warning)
                            } else {
                                ChatApprovalBar(approval: approval, count: 1, keyHints: false, detailHeight: 320, busy: busy || uncertain || answered) {
                                    send(.approve(requestID: item.decision.requestID, decision: $0))
                                }
                            }
                        case .question(let question):
                            ChatQuestionBar(question: question, scrollHeight: 560, busy: busy || uncertain || answered) {
                                send(.answer(requestID: item.decision.requestID, answers: $0.answers))
                            }
                        }
                    }
                    if let failure { Text(failure).font(style.system(.footnote)).foregroundStyle(style.warning).textSelection(.enabled) }
                    if busy { ProgressView("Checking current request…") }
                    Text("The host's current request is checked again before your response is sent.").font(style.system(.caption)).foregroundStyle(style.muted)
                }.padding(16)
            }
        }.desktopSheetSurface(style).foregroundStyle(style.text).tint(style.accent)
            .interactiveDismissDisabled(busy)
    }
    private func send(_ command: ChatCommand) {
        guard !busy, !sent, !uncertain, !answered else { return }
        busy = true; failure = nil
        Task {
            do { try await respond(command); sent = true }
            catch {
                failure = error.localizedDescription
                // Only a command handed to the connection can have gone through; every `DashboardResponseError` sent nothing.
                uncertain = (error as? ChatControlError)?.outcomeIsUncertain == true
            }
            busy = false
        }
    }
}
