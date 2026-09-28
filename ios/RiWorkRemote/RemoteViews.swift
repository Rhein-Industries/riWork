import SwiftUI
import UIKit
import RiWorkCore

private enum RemoteRoute: Hashable {
    case projects(String)
    case terminals(RemoteProject)
}

private struct PairingRequest: Identifiable {
    let id = UUID()
    let text: String
    /// Links can be opened by any app or web page, so they get a details-and-confirm step.
    var fromLink = false
}

struct RemoteRootView: View {
    @Bindable var model: RemoteModel
    @State private var path: [RemoteRoute] = []
    @State private var pairingRequest: PairingRequest?
    @State private var renaming: SavedDesktop?
    @State private var removing: SavedDesktop?
    // A double Back tap during the pop animation would otherwise pop an empty stack and trap.
    private func pop() { if !path.isEmpty { path.removeLast() } }
    var body: some View {
        NavigationStack(path: $path) {
            VStack(spacing: 0) {
                WorkspaceBar(title: "RIWORK") {
                    Button("Add desktop", systemImage: "plus") { pairingRequest = PairingRequest(text: "") }.labelStyle(.iconOnly).disabled(model.loadFailed)
                }
                if model.loadFailed {
                    LibraryFailureView(model: model)
                } else if model.desktops.isEmpty {
                    EmptyDesktopOverlay { pairingRequest = PairingRequest(text: "") }
                } else {
                    List {
                        Section("Desktops") {
                            ForEach(model.desktops) { desktop in
                                HStack(spacing: 0) {
                                    Button {
                                        path.append(.projects(desktop.id))
                                        Task { await model.activate(desktop.id) }
                                    } label: {
                                        HStack(spacing: 8) {
                                            Image(systemName: "desktopcomputer").font(.system(size: 14)).foregroundStyle(DesktopStyle.accent)
                                            VStack(alignment: .leading, spacing: 2) {
                                                Text(desktop.name).font(.custom("Menlo-Bold", size: 13, relativeTo: .headline)).foregroundStyle(DesktopStyle.text)
                                                Text(desktop.pairing.relayHost).font(.custom("Menlo", size: 11, relativeTo: .caption)).foregroundStyle(DesktopStyle.muted)
                                            }
                                            Spacer(minLength: 4)
                                            Image(systemName: "chevron.right").font(.caption).foregroundStyle(DesktopStyle.muted)
                                        }.frame(minHeight: 44).contentShape(Rectangle())
                                    }.buttonStyle(.plain).accessibilityHint("Choose a project on this desktop")
                                    Menu {
                                        Button("Rename", systemImage: "pencil") { renaming = desktop }
                                        Button("Remove from this device", systemImage: "trash", role: .destructive) { removing = desktop }
                                    } label: { Label("Desktop options for \(desktop.name)", systemImage: "ellipsis") }.labelStyle(.iconOnly)
                                }
                                .listRowBackground(DesktopStyle.background)
                                .listRowInsets(EdgeInsets(top: 0, leading: 12, bottom: 0, trailing: 8))
                                .listRowSeparatorTint(DesktopStyle.divider)
                                .contextMenu {
                                    Button("Rename", systemImage: "pencil") { renaming = desktop }
                                    Button("Remove from this device", systemImage: "trash", role: .destructive) { removing = desktop }
                                }
                                .swipeActions {
                                    Button("Remove", role: .destructive) { removing = desktop }
                                    Button("Rename") { renaming = desktop }.tint(DesktopStyle.accent)
                                }
                            }
                            Button("Pair a desktop", systemImage: "plus.circle") { pairingRequest = PairingRequest(text: "") }.listRowBackground(DesktopStyle.background)
                        }
                        Section { Label("Pairing keys stay in Keychain.", systemImage: "lock.shield").font(.caption).foregroundStyle(DesktopStyle.muted).listRowBackground(DesktopStyle.background) }
                    }
                    .listStyle(.plain).scrollContentBackground(.hidden)
                    .environment(\.defaultMinListRowHeight, 44)
                }
            }
            .background(DesktopStyle.background)
            .toolbar(.hidden, for: .navigationBar)
            .navigationDestination(for: RemoteRoute.self) { route in
                switch route {
                case .projects(let desktopID):
                    VStack(spacing: 0) {
                        WorkspaceBar(title: "PROJECTS", back: pop) { EmptyView() }
                        ProjectSelectionView(model: model, onSelect: { path.append(.terminals($0)) })
                    }.background(DesktopStyle.background).id(desktopID).toolbar(.hidden, for: .navigationBar)
                case .terminals(let project):
                    TerminalTabsView(model: model, project: project, onBack: pop)
                        .toolbar(.hidden, for: .navigationBar)
                }
            }
        }
        .background(DesktopStyle.background.ignoresSafeArea())
        .sheet(item: $pairingRequest) { request in
            PairDesktopSheet(model: model, initialText: request.text, fromLink: request.fromLink, onAdded: { id in
                path = [.projects(id)]
                Task { await model.activate(id) }
            })
        }
        .sheet(item: $renaming) { RenameDesktopSheet(model: model, desktop: $0) }
        .alert("Remove pairing?", isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }), presenting: removing) { desktop in
            Button("Remove pairing", role: .destructive) {
                Task { do { try await model.remove(id: desktop.id) } catch { model.error = error.localizedDescription } }
                removing = nil
            }
            Button("Cancel", role: .cancel) { removing = nil }
        } message: { desktop in Text("Removes \(desktop.name)’s local pairing keys. Desktop sessions keep running. Revoke this device on the desktop to deny future access.") }
        .onOpenURL { url in pairingRequest = PairingRequest(text: url.absoluteString, fromLink: true) }
    }
}

private struct LibraryFailureView: View {
    @Bindable var model: RemoteModel
    @State private var confirmingReset = false
    @State private var resetError: String?
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("SAVED PAIRINGS UNAVAILABLE", systemImage: "exclamationmark.lock").font(.custom("Menlo-Bold", size: 14, relativeTo: .headline))
            Text(model.loadFailure ?? "").foregroundStyle(DesktopStyle.warning).textSelection(.enabled)
            Text("Nothing was changed or deleted. Pairing and removing desktops is paused until this loads.").foregroundStyle(DesktopStyle.muted)
            if let resetError { Text(resetError).foregroundStyle(DesktopStyle.error) }
            Button("Try again", systemImage: "arrow.clockwise") { model.loadLibrary() }.buttonStyle(DesktopButtonStyle(prominent: true))
            Button("Erase saved pairings…", systemImage: "trash", role: .destructive) { confirmingReset = true }
        }.padding(20).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center).background(DesktopStyle.background)
            .alert("Erase saved pairings?", isPresented: $confirmingReset) {
                Button("Erase", role: .destructive) { do { try model.resetLibrary() } catch { resetError = error.localizedDescription } }
                Button("Cancel", role: .cancel) {}
            } message: { Text("Use this only if trying again never works. It deletes every pairing stored on this device, and each desktop has to be paired again.") }
    }
}

private struct EmptyDesktopOverlay: View {
    var add: () -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("NO DESKTOP CONNECTED", systemImage: "terminal").font(.custom("Menlo-Bold", size: 14, relativeTo: .headline))
            Text("Pair a desktop, choose a project, then continue in its open terminal tabs.").foregroundStyle(DesktopStyle.muted)
            Button("Pair a desktop", systemImage: "plus", action: add).buttonStyle(DesktopButtonStyle(prominent: true))
        }.padding(20).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center).background(DesktopStyle.background)
    }
}

struct ConnectionPanel: View {
    @Bindable var model: RemoteModel
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Circle().fill(model.state == .connected ? DesktopStyle.accent : DesktopStyle.warning).frame(width: 6, height: 6)
                Text(model.state.label).font(.custom("Menlo", size: 11, relativeTo: .caption))
                Spacer(minLength: 4)
                if model.state == .connecting { ProgressView().controlSize(.small) }
                if model.state == .connected {
                    Button("Refresh", systemImage: "arrow.clockwise") { Task { await model.refresh() } }.labelStyle(.iconOnly).disabled(model.loading)
                    Button("Disconnect", systemImage: "wifi.slash") { Task { await model.disconnect() } }.labelStyle(.iconOnly)
                } else {
                    Button(model.state == .connecting ? "Cancel" : "Reconnect", systemImage: model.state == .connecting ? "xmark" : "arrow.clockwise") {
                        Task { if model.state == .connecting { await model.disconnect() } else { await model.connect() } }
                    }
                }
            }
            if let error = model.error {
                HStack(alignment: .top) {
                    Text(error).font(.caption).foregroundStyle(DesktopStyle.warning).textSelection(.enabled)
                    Button("Dismiss message", systemImage: "xmark") { model.error = nil }.labelStyle(.iconOnly)
                }
            }
        }.padding(.horizontal, 12).background(DesktopStyle.panel)
    }
}

struct ProjectSelectionView: View {
    @Bindable var model: RemoteModel
    var onSelect: (RemoteProject) -> Void
    @State private var search = ""
    var body: some View {
        VStack(spacing: 0) {
            ConnectionPanel(model: model)
            DesktopRule()
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(DesktopStyle.muted)
                TextField("Find a project", text: $search).textFieldStyle(.plain).autocorrectionDisabled()
                if !search.isEmpty { Button("Clear filter", systemImage: "xmark") { search = "" }.labelStyle(.iconOnly) }
            }.padding(.horizontal, 12).frame(minHeight: 44)
            DesktopRule()
            if model.snapshotStale && !model.projects.isEmpty {
                Label("Saved projects · reconnect to refresh", systemImage: "clock.badge.exclamationmark")
                    .font(.caption).foregroundStyle(DesktopStyle.warning).padding(8)
            }
            List {
                ForEach(model.projects.filter { search.isEmpty || $0.name.localizedCaseInsensitiveContains(search) }) { project in
                    Button { onSelect(project) } label: {
                        HStack(spacing: 8) {
                            Image(systemName: "folder").font(.system(size: 14)).foregroundStyle(DesktopStyle.accent)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(project.name).font(.custom("Menlo-Bold", size: 13, relativeTo: .headline)).foregroundStyle(DesktopStyle.text)
                                Text(project.root).font(.custom("Menlo", size: 11, relativeTo: .caption)).foregroundStyle(DesktopStyle.muted).lineLimit(1)
                            }
                            Spacer(minLength: 4)
                            Image(systemName: "chevron.right").font(.caption).foregroundStyle(DesktopStyle.muted)
                        }.frame(minHeight: 40).contentShape(Rectangle())
                    }.buttonStyle(.plain).accessibilityHint("Open tabs for this project’s existing terminals")
                        .listRowBackground(DesktopStyle.background).listRowInsets(EdgeInsets(top: 2, leading: 12, bottom: 2, trailing: 12))
                        .listRowSeparatorTint(DesktopStyle.divider)
                }
                if model.projects.isEmpty {
                    Text(model.loading ? "Loading projects…" : "No projects. Open a project on your desktop and refresh.")
                        .foregroundStyle(DesktopStyle.muted).listRowBackground(DesktopStyle.background)
                }
            }.listStyle(.plain).scrollContentBackground(.hidden).environment(\.defaultMinListRowHeight, 44)
                .refreshable { await model.refresh() }
        }.background(DesktopStyle.background)
    }
}

struct TerminalTabsView: View {
    @Bindable var model: RemoteModel
    let project: RemoteProject
    var onBack: () -> Void
    @State private var sessionInfo: SessionInfo?
    @State private var followOutput = true
    private var openSessions: [RemoteSession] { model.openSessions }
    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: project.name, back: onBack) {
                Button("Session info", systemImage: "info.circle") {
                    if let session = model.session { sessionInfo = SessionInfo(id: session.id, title: session.title, cwd: session.cwd, kind: session.kind) }
                }.labelStyle(.iconOnly).disabled(model.sessionID == nil)
                Menu {
                    Toggle("Follow output", isOn: $followOutput)
                    Button("Refresh output", systemImage: "arrow.clockwise") { Task { await model.readOutput() } }.disabled(model.state != .connected)
                    Button("Refresh terminal tabs", systemImage: "arrow.clockwise") { Task { await model.refresh() } }
                    Divider()
                    if model.state == .connected { Button("Disconnect", systemImage: "wifi.slash") { Task { await model.disconnect() } } }
                    else { Button("Reconnect", systemImage: "arrow.clockwise") { Task { await model.connect() } } }
                } label: { Label("Terminal tabs and connection", systemImage: "ellipsis") }.labelStyle(.iconOnly)
            }
            if !openSessions.isEmpty {
                ScrollViewReader { proxy in
                    ScrollView(.horizontal) {
                        HStack(spacing: 0) {
                            ForEach(openSessions) { session in
                                Button { Task { await model.chooseSession(session) } } label: {
                                    VStack(alignment: .leading, spacing: 2) {
                                        Label(session.title, systemImage: session.kind == "orchestrator" ? "point.3.connected.trianglepath.dotted" : "terminal")
                                            .font(.custom("Menlo", size: 12, relativeTo: .subheadline)).lineLimit(1)
                                        Text(tabDetail(session)).font(.custom("Menlo", size: 10, relativeTo: .caption2)).foregroundStyle(DesktopStyle.muted).lineLimit(1)
                                    }
                                    .padding(.horizontal, 10).frame(minHeight: 44)
                                    .background(model.sessionID == session.id ? DesktopStyle.active : DesktopStyle.panel)
                                    .overlay(alignment: .trailing) { Rectangle().fill(DesktopStyle.divider).frame(width: 1) }
                                    .overlay(alignment: .bottom) { Rectangle().fill(model.sessionID == session.id ? DesktopStyle.accent : DesktopStyle.divider).frame(height: 1) }
                                }
                                .buttonStyle(.plain).id(session.id)
                                .accessibilityLabel("\(session.title), \(session.shortID)")
                                .accessibilityAddTraits(model.sessionID == session.id ? .isSelected : [])
                            }
                        }
                    }.scrollIndicators(.hidden)
                        .onChange(of: model.sessionID) { _, id in if let id { withAnimation { proxy.scrollTo(id, anchor: .center) } } }
                }
            }
            if model.sessionID == nil && openSessions.isEmpty {
                VStack {
                    VStack(alignment: .leading, spacing: 12) {
                        Label("NO OPEN TERMINALS", systemImage: "terminal").font(.custom("Menlo-Bold", size: 14, relativeTo: .headline))
                        Text("Open a terminal in this project on your desktop, then refresh.").foregroundStyle(DesktopStyle.muted)
                    }.padding(20).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
                    PendingInputNotice(model: model)
                    ConnectionPanel(model: model).padding()
                }
            } else { SessionConsole(model: model, followOutput: $followOutput) }
        }.background(DesktopStyle.background)
        .onChange(of: model.sessionID) { _, _ in sessionInfo = nil; followOutput = true }
        .sheet(item: $sessionInfo) { SessionInfoSheet(info: $0) }
        .task(id: project.id) { await model.chooseProject(project.id) }
        .onAppear { model.setTerminalVisible(true) }
        .onDisappear { model.setTerminalVisible(false) }
    }
    private func tabDetail(_ session: RemoteSession) -> String {
        if let tree = model.worktrees.first(where: { $0.id == session.worktree_id }) { return "\(tree.branch) · \(session.shortID)" }
        return session.shortID
    }
}

struct SessionInfo: Identifiable {
    let id: String
    let title: String
    let cwd: String
    let kind: String
}

private struct SessionInfoSheet: View {
    let info: SessionInfo
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: "SESSION INFO") { Button("Done") { dismiss() } }
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text(info.title).font(.custom("Menlo-Bold", size: 14, relativeTo: .headline))
                    Text(info.kind).font(.caption).foregroundStyle(DesktopStyle.muted)
                    DesktopRule()
                    Text("SESSION UUID").font(.caption).foregroundStyle(DesktopStyle.muted)
                    Text(info.id).textSelection(.enabled).accessibilityLabel("Session UUID: \(info.id)")
                    Text("WORKING DIRECTORY").font(.caption).foregroundStyle(DesktopStyle.muted)
                    Text(info.cwd).textSelection(.enabled)
                }.padding(16).frame(maxWidth: .infinity, alignment: .leading)
            }
        }.background(DesktopStyle.background).foregroundStyle(DesktopStyle.text)
            .font(.custom("Menlo", size: 13, relativeTo: .body)).tint(DesktopStyle.accent).buttonStyle(DesktopButtonStyle())
            .presentationDetents([.medium, .large]).presentationCornerRadius(8)
    }
}

private struct PendingInputNotice: View {
    @Bindable var model: RemoteModel
    @State private var acknowledge = false
    var body: some View {
        if let pending = model.pendingInput {
            VStack(alignment: .leading, spacing: 6) {
                Label(model.sending ? "Submitting once…" : "Unconfirmed submission", systemImage: "exclamationmark.bubble").font(.subheadline.bold())
                Text("Session \(pending.shellID.prefix(8)) · request \(pending.id.prefix(8))").font(.caption.monospaced())
                Text(pending.line).font(.caption.monospaced()).lineLimit(4).textSelection(.enabled)
                if !model.sending {
                    Text("Check that session’s output. This input will not be resent.").font(.footnote)
                    Button("I reviewed the output…") { acknowledge = true }.font(.subheadline)
                }
            }.padding(14).frame(maxWidth: .infinity, alignment: .leading).background(DesktopStyle.warning.opacity(0.12))
                .alert("Acknowledge unconfirmed input?", isPresented: $acknowledge) {
                    Button("Acknowledge after review") { do { try model.acknowledgeUncertainInput() } catch { model.error = error.localizedDescription } }
                    Button("Cancel", role: .cancel) {}
                } message: { Text("This clears the local warning. It does not submit or retry anything. Review the indicated session before creating a new input.") }
        }
    }
}

struct SessionConsole: View {
    @Bindable var model: RemoteModel
    @Binding var followOutput: Bool
    @ScaledMetric(relativeTo: .body) private var terminalFontSize = 12.0
    var body: some View {
        Group {
            if model.sessionID != nil {
                VStack(spacing: 0) {
                    HStack(spacing: 6) {
                        Image(systemName: outputStale ? "clock.badge.exclamationmark" : "checkmark.circle")
                        Text(outputStale ? "Stale · \(model.state.label.lowercased())" : "Latest snapshot")
                        if let date = model.lastOutputAt { Text(date, style: .time) }
                        Spacer(minLength: 4)
                        if let viewport = model.appliedViewport, model.viewportSessionID == model.sessionID {
                            Text("\(viewport.columns)×\(viewport.rows)").monospacedDigit()
                                .accessibilityLabel("Terminal size \(viewport.columns) columns, \(viewport.rows) rows")
                        }
                    }.font(.custom("Menlo", size: 10, relativeTo: .caption2)).foregroundStyle(outputStale ? DesktopStyle.warning : DesktopStyle.muted)
                        .padding(.horizontal, 8).padding(.vertical, 4).background(DesktopStyle.panel)
                    if let error = model.error {
                        HStack(alignment: .top) {
                            Image(systemName: "exclamationmark.circle")
                            Text(error).font(.footnote)
                            Spacer(minLength: 0)
                            Button("Dismiss", systemImage: "xmark") { model.error = nil }.labelStyle(.iconOnly)
                        }.padding(12).background(DesktopStyle.warning.opacity(0.1))
                    }
                    PendingInputNotice(model: model)
                    ScrollViewReader { proxy in
                        ScrollView([.horizontal, .vertical]) {
                            VStack(alignment: .leading, spacing: 0) {
                                if model.state == .connected && !model.viewportReady && model.viewportError == nil {
                                    ProgressView("Fitting desktop terminal…").padding(20)
                                } else if model.output.isEmpty { Text(model.lastOutputAt == nil ? "Output will appear when this session is connected." : "The session has no output yet.").foregroundStyle(.secondary).padding(20) }
                                else { Text(model.output).font(.custom("Menlo", fixedSize: terminalFontSize)).fixedSize(horizontal: true, vertical: true).textSelection(.enabled).padding(8).accessibilityLabel("Terminal output") }
                                Color.clear.frame(height: 1).id("output-end")
                            }.frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .defaultScrollAnchor(.bottomLeading, for: .initialOffset)
                        .defaultScrollAnchor(.topLeading, for: .alignment)
                        .background(DesktopStyle.background)
                        .background {
                            GeometryReader { geometry in
                                Color.clear
                                    .onAppear { reportViewport(geometry.size) }
                                    .onChange(of: geometry.size) { _, size in reportViewport(size) }
                                    .onChange(of: terminalFontSize) { _, _ in reportViewport(geometry.size) }
                            }
                        }
                        .onChange(of: model.output) { _, _ in if followOutput { proxy.scrollTo("output-end", anchor: .bottomLeading) } }
                    }
                    VStack(alignment: .leading, spacing: 4) {
                        if let notice = model.deliveryNotice { Text(notice).font(.custom("Menlo", size: 10, relativeTo: .caption2)).foregroundStyle(DesktopStyle.muted) }
                        HStack {
                            CommandField(text: $model.draft, placeholder: "Continue the selected session…", isEnabled: model.canEditDraft,
                                         label: "Continuation prompt or terminal command", onSubmit: { if canSubmit { send() } },
                                         onRejectedInput: { model.error = "Paste one line at a time. Multi-line input is not sent." })
                                .modifier(DesktopField())
                            Button("Send", systemImage: "arrow.up", action: send)
                                .labelStyle(.titleAndIcon).buttonStyle(DesktopButtonStyle(prominent: true))
                                .disabled(!canSubmit)
                                .accessibilityLabel("Send to selected terminal")
                                .accessibilityHint("Submits this line once followed by Return")
                        }
                        Text("One line + Return · selected \(model.session?.shortID ?? "—")").font(.custom("Menlo", size: 10, relativeTo: .caption2)).foregroundStyle(DesktopStyle.muted)
                        if model.state != .connected { Button("Reconnect") { Task { await model.connect() } }.disabled(model.state == .connecting) }
                    }.padding(8).background(DesktopStyle.panel).overlay(alignment: .top) { DesktopRule() }
                }
            } else {
                Text("Choose an open terminal tab.").foregroundStyle(DesktopStyle.muted).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
    }
    private var canSubmit: Bool { model.canSend && !model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    private func send() {
        guard let selectedID = model.sessionID else { return }
        let line = model.draft
        Task { await model.submit(expectedSessionID: selectedID, line: line) }
    }
    private var outputStale: Bool { model.state != .connected || model.snapshotStale || model.outputSessionID != model.sessionID || model.session?.alive != true || !model.viewportReady }
    private func reportViewport(_ size: CGSize) {
        let font = UIFont(name: "Menlo-Regular", size: terminalFontSize) ?? UIFont.monospacedSystemFont(ofSize: terminalFontSize, weight: .regular)
        let width = ("M" as NSString).size(withAttributes: [.font: font]).width
        model.reportViewport(TerminalViewport.fit(width: size.width - 16, height: size.height - 16, cellWidth: width, lineHeight: font.lineHeight))
    }
}
