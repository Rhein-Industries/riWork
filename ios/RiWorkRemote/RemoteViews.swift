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
}

struct RemoteRootView: View {
    @Bindable var model: RemoteModel
    @State private var path: [RemoteRoute] = []
    @State private var pairingRequest: PairingRequest?
    @State private var renaming: SavedDesktop?
    @State private var removing: SavedDesktop?
    var body: some View {
        NavigationStack(path: $path) {
            Group {
                if model.desktops.isEmpty {
                    EmptyDesktopOverlay { pairingRequest = PairingRequest(text: "") }
                } else {
                    List {
                        Section {
                            Label("Continue your desktop terminals", systemImage: "hammer.fill")
                                .font(.headline).padding(.vertical, 8)
                        }
                        Section("Desktops") {
                            ForEach(model.desktops) { desktop in
                                Button { path.append(.projects(desktop.id)) } label: {
                                    HStack(spacing: 12) {
                                        Image(systemName: "desktopcomputer").font(.title3).foregroundStyle(.tint)
                                        VStack(alignment: .leading, spacing: 4) {
                                            Text(desktop.name).font(.headline).foregroundStyle(.primary)
                                            Text(desktop.pairing.relayHost).font(.caption).foregroundStyle(.secondary)
                                        }
                                        Spacer()
                                        Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary)
                                    }.padding(.vertical, 6)
                                }
                                .accessibilityHint("Choose a project on this desktop")
                                .contextMenu {
                                    Button("Rename", systemImage: "pencil") { renaming = desktop }
                                    Button("Remove from this device", systemImage: "trash", role: .destructive) { removing = desktop }
                                }
                                .swipeActions {
                                    Button("Remove", role: .destructive) { removing = desktop }
                                    Button("Rename") { renaming = desktop }.tint(.blue)
                                }
                            }
                            Button("Pair a desktop", systemImage: "plus.circle") { pairingRequest = PairingRequest(text: "") }
                        }
                        Section { Label("Pairing keys stay in this device’s Keychain.", systemImage: "lock.shield").font(.footnote).foregroundStyle(.secondary) }
                    }
                }
            }
            .navigationTitle("RiWork")
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Add desktop", systemImage: "plus") { pairingRequest = PairingRequest(text: "") } } }
            .navigationDestination(for: RemoteRoute.self) { route in
                switch route {
                case .projects(let desktopID):
                    ProjectSelectionView(model: model, onSelect: { path.append(.terminals($0)) })
                        .task(id: desktopID) { await model.activate(desktopID) }
                case .terminals(let project):
                    TerminalTabsView(model: model, project: project)
                }
            }
        }
        .sheet(item: $pairingRequest) { request in
            PairDesktopSheet(model: model, initialText: request.text, onAdded: { id in path = [.projects(id)] })
        }
        .sheet(item: $renaming) { RenameDesktopSheet(model: model, desktop: $0) }
        .confirmationDialog("Remove \(removing?.name ?? "desktop")?", isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }), titleVisibility: .visible) {
            Button("Remove pairing", role: .destructive) {
                guard let id = removing?.id else { return }
                Task { do { try await model.remove(id: id) } catch { model.error = error.localizedDescription } }
                removing = nil
            }
        } message: { Text("Removes the local pairing keys. Desktop sessions keep running. Revoke this device on the desktop to deny future access.") }
        .onOpenURL { url in pairingRequest = PairingRequest(text: url.absoluteString) }
    }
}

private struct EmptyDesktopOverlay: View {
    var add: () -> Void
    var body: some View {
        VStack(spacing: 18) {
            Image(systemName: "terminal").font(.system(size: 46, weight: .light)).foregroundStyle(.tint)
            Text("Your terminals, wherever you are").font(.title2.bold()).multilineTextAlignment(.center)
            Text("Pair a RiWork desktop, choose a project, and switch between its open terminals.").foregroundStyle(.secondary).multilineTextAlignment(.center)
            Button("Pair a desktop", action: add).buttonStyle(.borderedProminent).controlSize(.large)
        }.padding(28).frame(maxWidth: .infinity, maxHeight: .infinity).background(.background)
    }
}

struct ConnectionPanel: View {
    @Bindable var model: RemoteModel
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label(model.state.label, systemImage: model.state.symbol).font(.subheadline.weight(.semibold))
                    .foregroundStyle(model.state == .connected ? Color.green : Color.secondary)
                Spacer()
                if model.state == .connecting { ProgressView().accessibilityLabel("Connecting to desktop") }
            }
            if let error = model.error {
                Text(error).font(.footnote).foregroundStyle(.secondary).textSelection(.enabled)
                Button("Dismiss message") { model.error = nil }.font(.caption)
            }
            HStack {
                if model.state == .connected {
                    Button("Disconnect") { Task { await model.disconnect() } }.buttonStyle(.bordered)
                    Button("Refresh", systemImage: "arrow.clockwise") { Task { await model.refresh() } }.buttonStyle(.bordered).disabled(model.loading)
                } else {
                    Button(model.state == .connecting ? "Cancel connection" : "Reconnect", systemImage: model.state == .connecting ? "xmark" : "arrow.clockwise") {
                        Task { if model.state == .connecting { await model.disconnect() } else { await model.connect() } }
                    }.buttonStyle(.borderedProminent)
                }
            }
        }.padding(.vertical, 4)
    }
}

struct ProjectSelectionView: View {
    @Bindable var model: RemoteModel
    var onSelect: (RemoteProject) -> Void
    @State private var search = ""
    var body: some View {
        List {
            Section { ConnectionPanel(model: model) }
            if model.snapshotStale && !model.projects.isEmpty {
                Section { Label("Saved projects · reconnect to refresh", systemImage: "clock.badge.exclamationmark").font(.subheadline).foregroundStyle(.orange) }
            }
            Section("Choose a project") {
                ForEach(model.projects.filter { search.isEmpty || $0.name.localizedCaseInsensitiveContains(search) }) { project in
                    Button { onSelect(project) } label: {
                        HStack(spacing: 12) {
                            Image(systemName: "folder").font(.title3).foregroundStyle(.tint)
                            VStack(alignment: .leading, spacing: 4) {
                                Text(project.name).font(.headline).foregroundStyle(.primary)
                                Text(project.root).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(2)
                            }
                            Spacer()
                            Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary)
                        }.padding(.vertical, 5)
                    }.accessibilityHint("Open tabs for this project’s existing terminals")
                }
                if model.projects.isEmpty { Text(model.loading ? "Loading projects…" : "No projects available. Add a project on your desktop and refresh.").foregroundStyle(.secondary) }
            }
        }
        .navigationTitle("Projects")
        .searchable(text: $search, prompt: "Find a project")
        .refreshable { await model.refresh() }
    }
}

struct TerminalTabsView: View {
    @Bindable var model: RemoteModel
    let project: RemoteProject
    private var openSessions: [RemoteSession] { model.sessions.filter(\.alive) }
    var body: some View {
        VStack(spacing: 0) {
            if !openSessions.isEmpty {
                ScrollViewReader { proxy in
                    ScrollView(.horizontal) {
                        HStack(spacing: 6) {
                            ForEach(openSessions) { session in
                                Button { Task { await model.chooseSession(session) } } label: {
                                    VStack(alignment: .leading, spacing: 4) {
                                        Label(session.title, systemImage: session.kind == "orchestrator" ? "point.3.connected.trianglepath.dotted" : "terminal")
                                            .font(.subheadline.weight(.semibold)).lineLimit(1)
                                        Text(tabDetail(session)).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(1)
                                    }
                                    .padding(.horizontal, 14).padding(.vertical, 10)
                                    .background(model.sessionID == session.id ? Color.accentColor.opacity(0.13) : Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
                                    .overlay(RoundedRectangle(cornerRadius: 10).stroke(model.sessionID == session.id ? Color.accentColor.opacity(0.5) : .clear, lineWidth: 1))
                                }
                                .buttonStyle(.plain).id(session.id)
                                .accessibilityLabel("\(session.title), \(session.shortID)")
                                .accessibilityAddTraits(model.sessionID == session.id ? .isSelected : [])
                            }
                        }.padding(.horizontal, 12).padding(.vertical, 8)
                    }.scrollIndicators(.hidden)
                        .onChange(of: model.sessionID) { _, id in if let id { withAnimation { proxy.scrollTo(id, anchor: .center) } } }
                }
                Divider()
            }
            if model.sessionID == nil && openSessions.isEmpty {
                VStack {
                    ContentUnavailableView("No open terminals", systemImage: "terminal", description: Text("Open a terminal or worker in this project on your desktop, then refresh."))
                    ConnectionPanel(model: model).padding()
                }
            } else { SessionConsole(model: model) }
        }
        .navigationTitle(project.name).navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    ForEach(openSessions) { session in Button("\(session.title) · \(session.shortID)") { Task { await model.chooseSession(session) } } }
                    Divider()
                    Button("Refresh terminal tabs", systemImage: "arrow.clockwise") { Task { await model.refresh() } }
                    if model.state == .connected { Button("Disconnect", systemImage: "wifi.slash") { Task { await model.disconnect() } } }
                    else { Button("Reconnect", systemImage: "arrow.clockwise") { Task { await model.connect() } } }
                } label: { Label("Terminal tabs and connection", systemImage: "ellipsis.circle") }
            }
        }
        .task(id: project.id) { await model.chooseProject(project.id); await selectInitialTab() }
        .onChange(of: openSessions) { _, _ in Task { await selectInitialTab() } }
        .onAppear { model.setTerminalVisible(true) }
        .onDisappear { model.setTerminalVisible(false) }
    }
    private func selectInitialTab() async {
        if model.sessionID == nil, let first = openSessions.first { await model.chooseSession(first) }
    }
    private func tabDetail(_ session: RemoteSession) -> String {
        if let tree = model.worktrees.first(where: { $0.id == session.worktree_id }) { return "\(tree.branch) · \(session.shortID)" }
        return session.shortID
    }
}

private struct SubmissionReview: Identifiable {
    let id = UUID()
    let sessionID: String
    let line: String
    let title: String
    let cwd: String
}

struct SessionConsole: View {
    @Bindable var model: RemoteModel
    @State private var review: SubmissionReview?
    @State private var acknowledge = false
    @State private var followOutput = true
    @ScaledMetric(relativeTo: .body) private var terminalFontSize = 14.0
    var body: some View {
        Group {
            if let id = model.sessionID {
                VStack(spacing: 0) {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack(alignment: .top) {
                            Image(systemName: "terminal.fill").foregroundStyle(.tint)
                            VStack(alignment: .leading, spacing: 3) {
                                Text(model.session?.title ?? "Selected session unavailable").font(.headline)
                                Text(id).font(.caption.monospaced()).foregroundStyle(.secondary).textSelection(.enabled)
                            }
                            Spacer(minLength: 0)
                        }
                        if let session = model.session { Text(session.cwd).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(2) }
                        HStack(spacing: 6) {
                            Image(systemName: outputStale ? "clock.badge.exclamationmark" : "checkmark.circle")
                            Text(outputStale ? "Stale output · \(model.state.label.lowercased())" : "Latest snapshot")
                            if let date = model.lastOutputAt { Text(date, style: .time) }
                        }.font(.caption).foregroundStyle(outputStale ? Color.orange : Color.secondary).accessibilityElement(children: .combine)
                    }.padding(16).frame(maxWidth: .infinity, alignment: .leading).background(.bar)
                    if let error = model.error {
                        HStack(alignment: .top) {
                            Image(systemName: "exclamationmark.circle")
                            Text(error).font(.footnote)
                            Spacer(minLength: 0)
                            Button("Dismiss", systemImage: "xmark") { model.error = nil }.labelStyle(.iconOnly)
                        }.padding(12).background(Color.orange.opacity(0.1))
                    }
                    if let pending = model.pendingInput {
                        VStack(alignment: .leading, spacing: 6) {
                            Label(model.sending ? "Submitting once…" : "Unconfirmed submission", systemImage: "exclamationmark.bubble").font(.subheadline.bold())
                            Text("Session \(pending.shellID.prefix(8)) · request \(pending.id.prefix(8))").font(.caption.monospaced())
                            Text(pending.line).font(.caption.monospaced()).lineLimit(4).textSelection(.enabled)
                            if !model.sending {
                                Text("Check that session’s output. This input will not be resent.").font(.footnote)
                                Button("I reviewed the output…") { acknowledge = true }.font(.subheadline)
                            }
                        }.padding(14).frame(maxWidth: .infinity, alignment: .leading).background(Color.orange.opacity(0.12))
                    }
                    ScrollViewReader { proxy in
                        ScrollView([.horizontal, .vertical]) {
                            VStack(alignment: .leading, spacing: 0) {
                                if model.state == .connected && !model.viewportReady && model.viewportError == nil {
                                    ProgressView("Fitting desktop terminal…").padding(20)
                                } else if model.output.isEmpty { Text(model.lastOutputAt == nil ? "Output will appear when this session is connected." : "The session has no output yet.").foregroundStyle(.secondary).padding(20) }
                                else { Text(model.output).font(.system(size: terminalFontSize, design: .monospaced)).fixedSize(horizontal: true, vertical: true).textSelection(.enabled).padding(16).accessibilityLabel("Terminal output") }
                                Color.clear.frame(height: 1).id("output-end")
                            }.frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .defaultScrollAnchor(.bottomLeading, for: .initialOffset)
                        .defaultScrollAnchor(.topLeading, for: .alignment)
                        .background(Color(uiColor: .secondarySystemBackground))
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
                    VStack(alignment: .leading, spacing: 10) {
                        if let notice = model.deliveryNotice { Text(notice).font(.footnote).foregroundStyle(.secondary) }
                        HStack {
                            TextField("Continue the selected session…", text: $model.draft, axis: .vertical)
                                .lineLimit(1...4).textFieldStyle(.roundedBorder).autocorrectionDisabled().textInputAutocapitalization(.never)
                                .disabled(!model.canEditDraft)
                                .accessibilityLabel("Continuation prompt or terminal command")
                            Button("Review", systemImage: "arrow.up") {
                                if let session = model.session {
                                    review = SubmissionReview(sessionID: session.id, line: model.draft, title: session.title, cwd: session.cwd)
                                }
                            }
                                .labelStyle(.iconOnly).buttonStyle(.borderedProminent).controlSize(.large)
                                .disabled(!model.canSend || model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                                .accessibilityLabel("Review submission to selected session")
                        }
                        Text("Submits one line followed by Return to this existing session.").font(.caption).foregroundStyle(.secondary)
                        if model.state != .connected { Button("Reconnect") { Task { await model.connect() } }.buttonStyle(.bordered).disabled(model.state == .connecting) }
                    }.padding(14).background(.bar)
                }
            } else {
                ContentUnavailableView("Choose an existing session", systemImage: "terminal", description: Text("Open a worker or orchestrator from the workspace to read its output and continue work."))
            }
        }
        .toolbar {
            if model.sessionID != nil {
                ToolbarItemGroup(placement: .topBarTrailing) {
                    Toggle("Follow output", systemImage: "arrow.down.to.line", isOn: $followOutput).toggleStyle(.button)
                    Button("Refresh output", systemImage: "arrow.clockwise") { Task { await model.readOutput() } }.disabled(model.state != .connected)
                }
            }
        }
        .sheet(item: $review) { submission in
            NavigationStack {
                Form {
                    Section("Send to existing session") {
                        Text(submission.title).font(.headline)
                        Text(submission.sessionID).font(.caption.monospaced()).textSelection(.enabled)
                        Text(submission.cwd).font(.caption.monospaced())
                    }
                    Section("One line + Return") { Text(submission.line).font(.body.monospaced()).textSelection(.enabled) }
                    Section { Text("Terminal commands execute on your desktop. The app submits this line once and leaves the session running.").font(.footnote).foregroundStyle(.secondary) }
                    Section {
                        Button("Submit to this session", systemImage: "arrow.up.circle.fill") {
                            guard submission.sessionID == model.sessionID else { review = nil; return }
                            review = nil
                            Task { await model.submit(expectedSessionID: submission.sessionID, line: submission.line) }
                        }.disabled(!model.canSend || submission.sessionID != model.sessionID)
                    }
                }.navigationTitle("Review submission").navigationBarTitleDisplayMode(.inline)
                    .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { review = nil } } }
            }.presentationDetents([.medium, .large])
        }
        .confirmationDialog("Acknowledge unconfirmed input?", isPresented: $acknowledge, titleVisibility: .visible) {
            Button("Acknowledge after review") { do { try model.acknowledgeUncertainInput() } catch { model.error = error.localizedDescription } }
        } message: { Text("This clears the local warning. It does not submit or retry anything. Review the indicated session before creating a new input.") }
    }
    private var outputStale: Bool { model.state != .connected || model.snapshotStale || model.outputSessionID != model.sessionID || model.session?.alive != true || !model.viewportReady }
    private func reportViewport(_ size: CGSize) {
        let font = UIFont.monospacedSystemFont(ofSize: terminalFontSize, weight: .regular)
        let width = ("M" as NSString).size(withAttributes: [.font: font]).width
        model.reportViewport(TerminalViewport.fit(width: size.width - 32, height: size.height - 32, cellWidth: width, lineHeight: font.lineHeight))
    }
}
