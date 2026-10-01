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
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel
    @State private var path: [RemoteRoute] = []
    @State private var pairingRequest: PairingRequest?
    @State private var showingDisplay = false
    @State private var renaming: SavedDesktop?
    @State private var removing: SavedDesktop?
    // A double Back tap during the pop animation would otherwise pop an empty stack and trap.
    private func pop() { if !path.isEmpty { path.removeLast() } }
    var body: some View {
        NavigationStack(path: $path) {
            VStack(spacing: 0) {
                WorkspaceBar(title: "RIWORK") {
                    Button("Display settings", systemImage: "textformat.size") { showingDisplay = true }.labelStyle(.iconOnly)
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
                                            Image(systemName: "desktopcomputer").font(.system(size: style.pt(14))).foregroundStyle(style.accent)
                                            VStack(alignment: .leading, spacing: 2) {
                                                Text(desktop.name).font(style.mono(13, bold: true, relativeTo: .headline)).foregroundStyle(style.text)
                                                Text(desktop.pairing.relayHost).font(style.mono(11, relativeTo: .caption)).foregroundStyle(style.muted)
                                            }
                                            Spacer(minLength: 4)
                                            Image(systemName: "chevron.right").font(style.system(.caption)).foregroundStyle(style.muted)
                                        }.frame(minHeight: style.pt(44)).contentShape(Rectangle())
                                    }.buttonStyle(.plain).accessibilityHint("Choose a project on this desktop")
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
                }
            }
            .background(style.background)
            .toolbar(.hidden, for: .navigationBar)
            .navigationDestination(for: RemoteRoute.self) { route in
                switch route {
                case .projects(let desktopID):
                    VStack(spacing: 0) {
                        WorkspaceBar(title: "PROJECTS", back: pop) { EmptyView() }
                        ProjectSelectionView(model: model, onSelect: { path.append(.terminals($0)) },
                                             onNewTerminal: { project in model.newTerminalRequestedProject = project.id; path.append(.terminals(project)) })
                    }.background(style.background).id(desktopID).toolbar(.hidden, for: .navigationBar)
                case .terminals(let project):
                    TerminalTabsView(model: model, project: project, onBack: pop)
                        .toolbar(.hidden, for: .navigationBar)
                }
            }
        }
        .background(style.background.ignoresSafeArea())
        .sheet(item: $pairingRequest) { request in
            PairDesktopSheet(model: model, initialText: request.text, fromLink: request.fromLink, onAdded: { id in
                path = [.projects(id)]
                Task { await model.activate(id) }
            }).desktopThemed(model.theme.style)
        }
        .sheet(isPresented: $showingDisplay) { DisplaySettingsSheet(model: model).desktopThemed(model.theme.style) }
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
}

private struct LibraryFailureView: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel
    @State private var confirmingReset = false
    @State private var resetError: String?
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("SAVED PAIRINGS UNAVAILABLE", systemImage: "exclamationmark.lock").font(style.mono(14, bold: true, relativeTo: .headline))
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
            Label("NO DESKTOP CONNECTED", systemImage: "terminal").font(style.mono(14, bold: true, relativeTo: .headline))
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
                Text(model.state.label).font(style.mono(11, relativeTo: .caption))
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
                    Text(error).font(style.system(.caption)).foregroundStyle(style.warning).textSelection(.enabled)
                    Button("Dismiss message", systemImage: "xmark") { model.error = nil }.labelStyle(.iconOnly)
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
    @State private var search = ""
    var body: some View {
        VStack(spacing: 0) {
            ConnectionPanel(model: model)
            DesktopRule()
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(style.muted)
                TextField("Find a project", text: $search).textFieldStyle(.plain).autocorrectionDisabled()
                if !search.isEmpty { Button("Clear filter", systemImage: "xmark") { search = "" }.labelStyle(.iconOnly) }
            }.padding(.horizontal, 12).frame(minHeight: style.pt(44))
            DesktopRule()
            if model.snapshotStale && !model.projects.isEmpty {
                Label("Saved projects · reconnect to refresh", systemImage: "clock.badge.exclamationmark")
                    .font(style.system(.caption)).foregroundStyle(style.warning).padding(8)
            }
            List {
                ForEach(model.projects.filter { search.isEmpty || $0.name.localizedCaseInsensitiveContains(search) }) { project in
                    Button { onSelect(project) } label: {
                        HStack(spacing: 8) {
                            Image(systemName: "folder").font(.system(size: style.pt(14))).foregroundStyle(style.accent)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(project.name).font(style.mono(13, bold: true, relativeTo: .headline)).foregroundStyle(style.text)
                                Text(project.root).font(style.mono(11, relativeTo: .caption)).foregroundStyle(style.muted).lineLimit(1)
                            }
                            Spacer(minLength: 4)
                            Image(systemName: "chevron.right").font(style.system(.caption)).foregroundStyle(style.muted)
                        }.frame(minHeight: style.pt(40)).contentShape(Rectangle())
                    }.buttonStyle(.plain).accessibilityHint("Open tabs for this project’s existing terminals")
                        .listRowBackground(style.background).listRowInsets(EdgeInsets(top: 2, leading: 12, bottom: 2, trailing: 12))
                        .listRowSeparatorTint(style.divider)
                        .swipeActions(edge: .leading) {
                            if let onNewTerminal { Button("New terminal", systemImage: "plus") { onNewTerminal(project) }.tint(style.accent) }
                        }
                        .contextMenu {
                            if let onNewTerminal { Button("New terminal", systemImage: "plus") { onNewTerminal(project) } }
                        }
                }
                if model.projects.isEmpty {
                    Text(model.loading ? "Loading projects…" : "No projects. Open a project on your desktop and refresh.")
                        .foregroundStyle(style.muted).listRowBackground(style.background)
                }
            }.listStyle(.plain).scrollContentBackground(.hidden).environment(\.defaultMinListRowHeight, style.pt(44))
                .refreshable { await model.refresh() }
        }.background(style.background)
    }
}

struct TerminalTabsView: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel
    let project: RemoteProject
    var onBack: () -> Void
    @State private var sessionInfo: SessionInfo?
    @State private var showingDisplay = false
    @State private var followOutput = true
    @State private var newTerminal: NewTerminalSheetModel?
    @State private var closing: RemoteSession?
    private var openSessions: [RemoteSession] { model.openSessions }
    private var focused: Bool { model.focusMode && model.sessionID != nil }
    var body: some View {
        let _ = Perf.count("body.TerminalTabsView")
        VStack(spacing: 0) {
            // Focus mode drops all of this: only the shell (and its keyboard) stays.
            if !focused {
                WorkspaceBar(title: project.name, back: onBack, compact: true, onDoubleTap: { if model.sessionID != nil { model.setFocusMode(true) } }) {
                    Button("New terminal", systemImage: "plus") { openNewTerminal() }
                        .labelStyle(.iconOnly).disabled(model.state != .connected || model.projectID != project.id)
                        // A desktop that is too old still answers a tap, with the reason.
                        .opacity(model.terminalControl == .unsupported ? 0.45 : 1)
                        .accessibilityHint(model.terminalControl == .unsupported ? TerminalControlError.unsupportedMessage : "Opens a shell or an agent on your Mac")
                    Button("Focus mode", systemImage: "arrow.up.left.and.arrow.down.right") { model.setFocusMode(true) }
                        .labelStyle(.iconOnly).disabled(model.sessionID == nil)
                    Button("Session info", systemImage: "info.circle") {
                        if let session = model.session { sessionInfo = SessionInfo(id: session.id, title: session.title, cwd: session.cwd, kind: session.kind) }
                    }.labelStyle(.iconOnly).disabled(model.sessionID == nil)
                    Menu {
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
                        Button("Refresh terminal tabs", systemImage: "arrow.clockwise") { Task { await model.refresh() } }
                        if let session = model.session, model.canClose(session) {
                            Button("Close this terminal…", systemImage: "xmark.circle", role: .destructive) { closing = session }
                        }
                        Divider()
                        if model.state == .connected { Button("Disconnect", systemImage: "wifi.slash") { Task { await model.disconnect() } } }
                        else { Button("Reconnect", systemImage: "arrow.clockwise") { Task { await model.connect() } } }
                    } label: { Label("Terminal tabs and connection", systemImage: "ellipsis") }.labelStyle(.iconOnly)
                }
                if !openSessions.isEmpty { tabStrip }
            }
            if model.sessionID == nil && openSessions.isEmpty {
                VStack {
                    VStack(alignment: .leading, spacing: 12) {
                        Label("NO OPEN TERMINALS", systemImage: "terminal").font(style.mono(14, bold: true, relativeTo: .headline))
                        Text("Open one here, or on your desktop and then refresh.").foregroundStyle(style.muted)
                        Button("New terminal", systemImage: "plus") { openNewTerminal() }.buttonStyle(DesktopButtonStyle(prominent: true))
                            .disabled(model.state != .connected || model.projectID != project.id)
                    }.padding(20).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
                    PendingInputNotice(model: model)
                    ConnectionPanel(model: model).padding()
                }
            } else { SessionConsole(model: model, followOutput: $followOutput) }
        }
        .background((focused ? style.terminalBackground : style.background).ignoresSafeArea())
        .statusBarHidden(focused)
        .onChange(of: model.sessionID) { _, _ in sessionInfo = nil; followOutput = true }
        .onChange(of: model.focusMode) { _, _ in model.updateKeepAwake() }
        .sheet(item: $sessionInfo) { SessionInfoSheet(info: $0).desktopThemed(model.theme.style) }
        .sheet(isPresented: $showingDisplay) { DisplaySettingsSheet(model: model).desktopThemed(model.theme.style) }
        .sheet(item: $newTerminal) { NewTerminalSheet(sheet: $0).desktopThemed(model.theme.style) }
        .alert("Close terminal?", isPresented: Binding(get: { closing != nil }, set: { if !$0 { closing = nil } }), presenting: closing) { session in
            Button("Close terminal", role: .destructive) { close(session) }
            Button("Cancel", role: .cancel) {}
        } message: { session in Text("Close \(session.title) · \(session.shortID)? This ends its running process on the Mac.") }
        // ⌘N, wherever the keyboard is (also in focus mode, where the header is gone). SwiftUI turns it into a UIKeyCommand on the
        // hosting controller, which the terminal's key view passes on: it claims no ⌘ combination.
        .background {
            Button("New terminal") { openNewTerminal() }
                .keyboardShortcut("n", modifiers: .command)
                .disabled(!model.canOpenNewTerminal || model.projectID != project.id || newTerminal != nil)
                .frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
        }
        .task(id: project.id) { await model.chooseProject(project.id) }
        .onAppear { model.setTerminalVisible(true); openRequestedNewTerminal() }
        .onDisappear { model.setTerminalVisible(false); if model.newTerminalRequestedProject == project.id { model.newTerminalRequestedProject = nil } }
        .onChange(of: model.newTerminalRequestedProject) { _, _ in openRequestedNewTerminal() }
        .onChange(of: model.projectID) { _, _ in openRequestedNewTerminal() }
        .onChange(of: model.state) { _, _ in openRequestedNewTerminal() }
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
                    ForEach(openSessions) { session in
                        Button { Task { await model.chooseSession(session) } } label: {
                            VStack(alignment: .leading, spacing: 1) {
                                // Orchestrators carry the secondary accent, as they do on the desktop.
                                Label { Text(session.title) } icon: {
                                    Image(systemName: session.kind == "orchestrator" ? "point.3.connected.trianglepath.dotted" : "terminal")
                                        .foregroundStyle(session.kind == "orchestrator" ? style.magenta : style.text)
                                }.font(style.mono(12, relativeTo: .subheadline)).lineLimit(1)
                                Text(tabDetail(session)).font(style.mono(10, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1)
                            }
                            .padding(.horizontal, 10).frame(minHeight: style.pt(36))
                            .background(model.sessionID == session.id ? style.active : style.panel)
                            .overlay(alignment: .trailing) { Rectangle().fill(style.divider).frame(width: 1) }
                            .overlay(alignment: .bottom) { Rectangle().fill(model.sessionID == session.id ? style.accent : style.divider).frame(height: 1) }
                        }
                        .buttonStyle(.plain).id(session.id)
                        .accessibilityLabel("\(session.title), \(session.shortID)")
                        .accessibilityAddTraits(model.sessionID == session.id ? .isSelected : [])
                        .contextMenu {
                            Button("New terminal", systemImage: "plus") { openNewTerminal() }
                            if model.canClose(session) { Button("Close terminal…", systemImage: "xmark.circle", role: .destructive) { closing = session } }
                        }
                        .accessibilityAction(named: "Close terminal") { if model.canClose(session) { closing = session } }
                    }
                }
            }.scrollIndicators(.hidden)
                .onChange(of: model.sessionID) { _, id in if let id { withAnimation { proxy.scrollTo(id, anchor: .center) } } }
        }
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
    @Environment(\.desktopStyle) private var style
    let info: SessionInfo
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: "SESSION INFO") { Button("Done") { dismiss() } }
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    Text(info.title).font(style.mono(14, bold: true, relativeTo: .headline))
                    Text(info.kind).font(style.system(.caption)).foregroundStyle(style.muted)
                    DesktopRule()
                    Text("SESSION UUID").font(style.system(.caption)).foregroundStyle(style.muted)
                    Text(info.id).textSelection(.enabled).accessibilityLabel("Session UUID: \(info.id)")
                    Text("WORKING DIRECTORY").font(style.system(.caption)).foregroundStyle(style.muted)
                    Text(info.cwd).textSelection(.enabled)
                }.padding(16).frame(maxWidth: .infinity, alignment: .leading)
            }
        }.background(style.background).foregroundStyle(style.text)
            .font(style.mono(13, relativeTo: .body)).tint(style.accent).buttonStyle(DesktopButtonStyle())
            .presentationDetents([.medium, .large]).presentationCornerRadius(8)
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
                Text("Session \(pending.shellID.prefix(8)) · request \(pending.id.prefix(8))").font(style.mono(12, relativeTo: .caption))
                Text(pending.line).font(style.mono(12, relativeTo: .caption)).lineLimit(4).textSelection(.enabled)
                if !model.sending {
                    Text("Check that session’s output. This input will not be resent.").font(style.system(.footnote))
                    Button("I reviewed the output…") { acknowledge = true }.font(style.system(.subheadline))
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
    @State private var editor: HotkeyEditorStart?
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
    var body: some View {
        let _ = Perf.count("body.SessionConsole")
        Group {
            if model.sessionID != nil {
                VStack(spacing: 0) {
                    if !focused {
                        StatusStrip(model: model)
                        if let error = model.error {
                            HStack(alignment: .top) {
                                Image(systemName: "exclamationmark.circle")
                                Text(error).font(style.system(.footnote))
                                Spacer(minLength: 0)
                                Button("Dismiss", systemImage: "xmark") { model.error = nil }.labelStyle(.iconOnly)
                            }.padding(.horizontal, 12).padding(.vertical, 8).background(style.warning.opacity(0.1))
                        }
                        PendingInputNotice(model: model)
                    }
                    terminal
                    bottomPanel
                }
                .background {
                    if model.directTyping {
                        KeyCapture(focus: keyFocus, isEnabled: model.session?.alive == true,
                                   label: "Terminal input for \(model.session?.title ?? "session") \(model.session?.shortID ?? "")",
                                   presentation: focused ? .pill : .strip, hotkeys: model.hotkeys.custom,
                                   shortcuts: model.hotkeys.shortcuts, palette: palette,
                                   onEditHotkeys: { openEditor(.list) }, onNewHotkey: { openEditor(.new) }, onEditHotkey: { openEditor(.edit($0)) },
                                   onKeyEvent: { model.keyboard.events.record($0) },
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
        keyFocus.suspendForModal()
        editor = start
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
            Button(keyFocus.isActive ? "Hide keyboard" : "Show keyboard", systemImage: keyFocus.isActive ? "keyboard.chevron.compact.down" : "keyboard") {
                if keyFocus.isActive { keyFocus.userDismiss() } else { keyFocus.focus() }
            }.labelStyle(.iconOnly).buttonStyle(DesktopButtonStyle(compact: true))
        }.font(style.mono(10, relativeTo: .caption2)).foregroundStyle(style.muted)
            .padding(.leading, 8).frame(minHeight: style.pt(36)).background(style.panel).overlay(alignment: .top) { DesktopRule() }
    }
    /// The line composer: today's behaviour, for desktops that cannot take keys and for anyone who prefers it.
    private var composer: some View {
        VStack(alignment: .leading, spacing: 4) {
            if !focused, let notice = model.deliveryNotice { Text(notice).font(style.mono(10, relativeTo: .caption2)).foregroundStyle(style.muted) }
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
            if !focused {
                Text("One line + Return · selected \(model.session?.shortID ?? "—")").font(style.mono(10, relativeTo: .caption2)).foregroundStyle(style.muted)
                if model.state != .connected { Button("Reconnect") { Task { await model.connect() } }.disabled(model.state == .connecting) }
            }
        }.padding(8).background(style.panel).overlay(alignment: .top) { DesktopRule() }
    }
    private var canSubmit: Bool { model.canSend && !model.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
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
            Image(systemName: outputStale ? "clock.badge.exclamationmark" : "checkmark.circle")
            Text(outputStale ? "Stale · \(model.state.label.lowercased())" : (model.syncMode == .live ? "Live" : "Latest snapshot"))
            if model.syncMode != .live || outputStale, let date = model.lastOutputAt { Text(date, style: .time) }
            if model.outputInMode { CopyModeBadge() }
            Spacer(minLength: 4)
            if let viewport = model.appliedViewport, model.viewportSessionID == model.sessionID {
                Text("\(viewport.columns)×\(viewport.rows)").monospacedDigit()
                    .accessibilityLabel("Terminal size \(viewport.columns) columns, \(viewport.rows) rows")
            }
        }.font(style.mono(10, relativeTo: .caption2)).foregroundStyle(outputStale ? style.warning : style.muted)
            .padding(.horizontal, 8).padding(.vertical, 2).background(style.panel)
    }
    private var outputStale: Bool { model.state != .connected || model.snapshotStale || model.outputSessionID != model.sessionID || model.session?.alive != true || !model.viewportReady }
}
