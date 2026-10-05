import SwiftUI
import UIKit
import GameController
import RiWorkCore

/// "New terminal": where, what, Create. Fast with one thumb (big rows, Create at the bottom) and complete from a hardware
/// keyboard: up and down choose the kind, tab and left/right move between the controls, space flips the toggle, return creates and
/// escape cancels, from the moment the sheet is up.
struct NewTerminalSheet: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var sheet: NewTerminalSheetModel
    @State private var hardwareKeyboard = GCKeyboard.coalesced != nil

    private var showFocus: Bool { sheet.keyboardInUse }
    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: style.cased("New terminal")) {
                Button("Cancel", action: sheet.cancel).accessibilityHint("Closes without opening a terminal")
            }
            ScrollView { formContent }.scrollBounceBehavior(.basedOnSize).desktopSheetFooter(style) { footer }
        }
        .desktopSheetSurface(style)
        .foregroundStyle(style.text).font(style.face(13, relativeTo: .body)).tint(style.accent)
        .background { NewTerminalKeys(onKey: handle).frame(width: 1, height: 1).accessibilityHidden(true) }
        .presentationDetents([.medium, .large]).presentationDragIndicator(.visible)
        .onChange(of: sheet.model.worktrees) { _, _ in sheet.refreshTargets() }
        .onReceive(NotificationCenter.default.publisher(for: .GCKeyboardDidConnect)) { _ in hardwareKeyboard = true }
        .onReceive(NotificationCenter.default.publisher(for: .GCKeyboardDidDisconnect)) { _ in hardwareKeyboard = GCKeyboard.coalesced != nil }
    }

    private var formContent: some View {
        VStack(alignment: .leading, spacing: 0) {
            sectionLabel(style.cased("Where"))
            targetRow
            sectionLabel(style.cased("What"))
            VStack(spacing: 0) {
                ForEach(sheet.form.kinds) { kindRow($0) }
            }
            .accessibilityElement(children: .contain).accessibilityLabel("Terminal kind")
            if sheet.form.kind.isOrchestrator {
                Picker("Orchestrator runs as", selection: Binding(get: { sheet.form.orchestratorMode }, set: { sheet.setOrchestratorMode($0) })) {
                    ForEach(NewOrchestratorMode.allCases) { Text($0.title).tag($0) }
                }
                .padding(12).overlay { ring(.orchestratorMode) }.disabled(sheet.busy)
                Text("Applies when starting a new orchestrator. An existing one opens in its current mode. Chat uses the Mac’s chat provider.")
                    .font(style.system(.caption)).foregroundStyle(style.muted).padding(.horizontal, 12)
            }
            if sheet.form.kind.isChat { NewChatModelSection(sheet: sheet) }
            if sheet.form.kind.isAgent { unrestrictedRow }
            if let problem = sheet.problem { messageRow(problem) }
        }
    }

    private func handle(_ key: NewTerminalSheetKey) {
        switch key {
        case .form(let key): sheet.press(key)
        case .create: sheet.keyboardInUse = true; sheet.create()
        case .cancel: sheet.cancel()
        }
    }

    private func sectionLabel(_ text: String) -> some View {
        Text(text).font(style.system(.caption, weight: .bold)).foregroundStyle(style.muted)
            .padding(.horizontal, 12).padding(.top, 12).padding(.bottom, 4).accessibilityAddTraits(.isHeader)
    }
    private func ring(_ field: NewTerminalForm.Field) -> some View {
        DesktopRing(shown: showFocus && sheet.form.focus == field, capsule: field == .create)
    }

    // MARK: Where

    @ViewBuilder private var targetRow: some View {
        let form = sheet.form
        // An orchestrator belongs to the project (not to one of its worktrees) or to none, so there is nothing to choose for it.
        let scoped = form.kind.isOrchestrator
        let title = form.kind == .globalOrchestrator ? "All projects" : ((scoped ? form.target?.projectName : form.target?.title) ?? "No project")
        let choosable = form.targets.count > 1 && !scoped
        let content = HStack(spacing: 10) {
            Image(systemName: form.kind == .globalOrchestrator ? "globe" : "folder").frame(width: style.pt(22)).foregroundStyle(style.accent)
            VStack(alignment: .leading, spacing: 1) {
                Text(title).lineLimit(1)
                if let branch = form.target?.branchLabel, choosable {
                    Text(branch).font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1)
                }
            }
            Spacer(minLength: 4)
            if choosable { Image(systemName: "chevron.up.chevron.down").font(style.system(.caption)).foregroundStyle(style.muted) }
        }
        .padding(.horizontal, 12).frame(maxWidth: .infinity, minHeight: style.pt(48), alignment: .leading)
        .contentShape(Rectangle()).overlay { ring(.target) }
        if choosable {
            Menu {
                ForEach(Array(form.targets.enumerated()), id: \.element.id) { index, target in
                    Button { sheet.select(targetAt: index) } label: {
                        Label(target.branchLabel ?? target.title, systemImage: index == form.targetIndex ? "checkmark" : "arrow.triangle.branch")
                    }
                }
            } label: { content }
                .accessibilityLabel("Where: \(form.target?.title ?? "none")").accessibilityHint("Choose a worktree")
        } else {
            content.accessibilityElement(children: .combine).accessibilityLabel("Where: \(title)")
        }
    }

    // MARK: What

    private func icon(_ kind: NewTerminalKind) -> String {
        switch kind {
        case .shell: "terminal"
        case .codex: "chevron.left.forwardslash.chevron.right"
        case .claude: "sparkles"
        case .grok: "bolt"
        case .codexChat: ChatProvider.codex.glyph
        case .claudeChat: ChatProvider.claude.glyph
        case .projectOrchestrator: "point.3.connected.trianglepath.dotted"
        case .globalOrchestrator: "globe"
        }
    }
    private func detail(_ kind: NewTerminalKind) -> String {
        if kind.isOrchestrator { return "Orchestrator" }
        return kind == .shell ? "The Mac’s login shell" : (kind.isChat ? "Native chat" : "Agent")
    }
    private func kindRow(_ kind: NewTerminalKind) -> some View {
        let selected = sheet.form.kind == kind
        return Button { sheet.select(kind: kind) } label: {
            HStack(spacing: 10) {
                Image(systemName: selected ? "largecircle.fill.circle" : "circle").frame(width: style.pt(22)).foregroundStyle(selected ? style.accent : style.muted)
                Image(systemName: icon(kind)).frame(width: style.pt(20)).foregroundStyle(style.muted)
                Text(kind.title).font(style.face(14, bold: selected, relativeTo: .body)).lineLimit(1).minimumScaleFactor(0.8)
                Spacer(minLength: 4)
                Text(detail(kind)).font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted)
            }
            .padding(.horizontal, 12).frame(maxWidth: .infinity, minHeight: style.pt(48), alignment: .leading)
            .overlay { if selected { ring(.kind) } }
            .desktopRowFill(style, selected: selected)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(kind.title).accessibilityHint(detail(kind))
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var unrestrictedRow: some View {
        Toggle(isOn: Binding(get: { sheet.form.unrestricted }, set: { sheet.setUnrestricted($0) })) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Unrestricted: no approval prompts")
                Text(sheet.form.kind.isChat ? "Starts in Full mode: the agent can run commands and change files on your Mac without asking. You can change the mode in the chat."
                     : "The agent can run commands and change files on your Mac without asking.")
                    .font(style.system(.caption)).foregroundStyle(style.muted).fixedSize(horizontal: false, vertical: true)
            }
        }
        // Native's accent is black or white: the switch is the system green when on, so on and off are told apart.
        .tint(style.native ? Color(uiColor: .systemGreen) : style.accent)
        .padding(.horizontal, 12).padding(.vertical, 6).frame(minHeight: style.pt(48))
        .overlay { ring(.unrestricted) }
        .padding(.top, 8)
    }

    private func messageRow(_ message: NewTabProblem) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: message.outcomeIsUncertain ? "questionmark.circle" : "exclamationmark.triangle")
            Text(message.message).font(style.system(.footnote)).fixedSize(horizontal: false, vertical: true)
        }
        .foregroundStyle(style.warning).padding(12).frame(maxWidth: .infinity, alignment: .leading)
        .background(style.warning.opacity(0.1), in: style.block())
        .padding(.horizontal, style.native ? 8 : 0)
        .accessibilityElement(children: .combine).accessibilityAddTraits(.updatesFrequently)
        .padding(.top, 8)
    }

    // MARK: Footer

    private var createTitle: String {
        if sheet.problem?.outcomeIsUncertain == true { return "Try again" }
        // An orchestrator is one per project: Create opens the one that is there, or starts it.
        return sheet.form.kind.isOrchestrator ? "Open \(sheet.form.kind.title.lowercased())" : "Create \(sheet.form.kind.title)"
    }
    private var footer: some View {
        VStack(spacing: 6) {
            DesktopRule()
            if hardwareKeyboard || sheet.keyboardInUse {
                Text("↩ create   ⎋ cancel   ↑↓ choose   ⇥ next control").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted)
                    .lineLimit(1).minimumScaleFactor(0.7).padding(.horizontal, 12).accessibilityHidden(true)
            }
            Button { sheet.keyboardInUse = false; sheet.create() } label: {
                HStack(spacing: 8) {
                    if sheet.busy { ProgressView().controlSize(.small) }
                    Text(sheet.busy ? (sheet.form.kind.isOrchestrator ? "Opening…" : "Creating…") : createTitle).font(style.face(14, bold: true, relativeTo: .headline))
                }.frame(maxWidth: .infinity, minHeight: style.pt(48))
            }
            .buttonStyle(DesktopButtonStyle(prominent: true))
            .disabled(!sheet.canCreate)
            .overlay { ring(.create) }
            .padding(.horizontal, 12).padding(.bottom, 8)
            .accessibilityHint(sheet.unsupported ? sheet.unsupportedMessage : "Opens it on your Mac and switches to it")
        }.background(style.glass ? style.surface : style.panel)
    }
}

// MARK: - Keyboard

/// What a key in the sheet means.
enum NewTerminalSheetKey: Equatable {
    case form(NewTerminalForm.Key)
    case create, cancel
}

/// Takes the keyboard for the sheet. It becomes first responder as soon as it is on screen, so no tap is needed first, and its
/// commands win over the system's own use of the arrows, tab, return and escape. It shows no software keyboard (it is not a text
/// input).
@MainActor final class NewTerminalKeyView: UIView {
    var onKey: (NewTerminalSheetKey) -> Void = { _ in }

    static let bindings: [(input: String, flags: UIKeyModifierFlags, key: NewTerminalSheetKey)] = [
        (UIKeyCommand.inputUpArrow, [], .form(.up)), (UIKeyCommand.inputDownArrow, [], .form(.down)),
        (UIKeyCommand.inputLeftArrow, [], .form(.left)), (UIKeyCommand.inputRightArrow, [], .form(.right)),
        ("\t", [], .form(.tab)), ("\t", .shift, .form(.backTab)), (" ", [], .form(.space)),
        ("\r", [], .create), (UIKeyCommand.inputEscape, [], .cancel)
    ]
    private lazy var commands: [UIKeyCommand] = Self.bindings.map { binding in
        let command = UIKeyCommand(input: binding.input, modifierFlags: binding.flags, action: #selector(fired(_:)))
        command.wantsPriorityOverSystemBehavior = true
        return command
    }
    override var keyCommands: [UIKeyCommand]? { commands }
    override var canBecomeFirstResponder: Bool { true }

    @objc func fired(_ command: UIKeyCommand) {
        guard let input = command.input, let match = Self.bindings.first(where: { $0.input == input && $0.flags == command.modifierFlags }) else { return }
        onKey(match.key)
    }
    override func didMoveToWindow() {
        super.didMoveToWindow()
        takeFocus()
    }
    /// A sheet is still animating in when its content is first laid out; the next turn of the run loop is early enough.
    func takeFocus() {
        guard window != nil, !isFirstResponder else { return }
        DispatchQueue.main.async { [weak self] in
            guard let self, self.window != nil, !self.isFirstResponder else { return }
            _ = self.becomeFirstResponder()
        }
    }
}

private struct NewTerminalKeys: UIViewRepresentable {
    var onKey: (NewTerminalSheetKey) -> Void
    func makeUIView(context: Context) -> NewTerminalKeyView {
        let view = NewTerminalKeyView()
        view.isAccessibilityElement = false
        return view
    }
    func updateUIView(_ view: NewTerminalKeyView, context: Context) {
        view.onKey = onKey
        // A menu or a toggle may have taken the keyboard; the next update gives it back.
        view.takeFocus()
    }
}
