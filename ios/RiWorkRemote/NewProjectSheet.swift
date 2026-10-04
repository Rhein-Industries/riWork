import SwiftUI
import UIKit
import GameController
import RiWorkCore

/// "New project": a name, whether to run `git init`, Create. Complete from a hardware keyboard from the moment the sheet is up
/// (the name field has the keyboard, no tap needed): type the name, return creates, escape cancels, tab moves between the name, the
/// Git switch and Create, space flips the switch (or presses Create) when it has the ring, and ⌘G flips the switch from anywhere,
/// also while typing, for a keyboard that has no tab (⌘. and ⌘↩ are cancel and create for the same reason).
struct NewProjectSheet: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var sheet: NewProjectSheetModel
    @State private var hardwareKeyboard = GCKeyboard.coalesced != nil

    private var showFocus: Bool { sheet.keyboardInUse }
    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: style.cased("New project")) {
                Button("Cancel", action: sheet.cancel).accessibilityHint("Closes without creating a project")
            }
            ScrollView { formContent }.scrollBounceBehavior(.basedOnSize)
            footer
        }
        .desktopSheetSurface(style)
        .foregroundStyle(style.text).font(style.face(13, relativeTo: .body)).tint(style.accent)
        // The keyboard belongs to the name field while it has the ring, and to this view while the switch or Create has it.
        .background { NewProjectKeys(wantsKeyboard: sheet.form.focus != .name, onKey: handle).frame(width: 1, height: 1).accessibilityHidden(true) }
        .presentationDetents([.medium, .large]).presentationDragIndicator(.visible)
        .onReceive(NotificationCenter.default.publisher(for: .GCKeyboardDidConnect)) { _ in hardwareKeyboard = true }
        .onReceive(NotificationCenter.default.publisher(for: .GCKeyboardDidDisconnect)) { _ in hardwareKeyboard = GCKeyboard.coalesced != nil }
    }

    private var formContent: some View {
        VStack(alignment: .leading, spacing: 0) {
            sectionLabel(style.cased("Name"))
            nameField
            nameNote
            sectionLabel(style.cased("Git"))
            gitRow
            if let message = sheet.message { messageRow(message) }
        }
    }

    private func handle(_ key: NewProjectSheetKey) {
        switch key {
        case .form(let key): sheet.press(key)
        case .create: sheet.keyboardInUse = true; sheet.create()
        case .cancel: sheet.cancel()
        case .toggleGit: sheet.toggleGitFromKeyboard()
        }
    }

    private func sectionLabel(_ text: String) -> some View {
        Text(text).font(style.system(.caption, weight: .bold)).foregroundStyle(style.muted)
            .padding(.horizontal, 12).padding(.top, 12).padding(.bottom, 4).accessibilityAddTraits(.isHeader)
    }
    private func ring(_ field: NewProjectForm.Field) -> some View {
        Rectangle().stroke(style.accent, lineWidth: 2).opacity(showFocus && sheet.form.focus == field ? 1 : 0).allowsHitTesting(false)
    }

    // MARK: Name

    private var nameField: some View {
        NewProjectNameInput(text: Binding(get: { sheet.form.name }, set: { sheet.setName($0) }), wantsFocus: sheet.form.focus == .name,
                            placeholder: "my-project", onFocus: { sheet.nameFieldFocused() }, onKey: handle)
            .frame(minHeight: style.pt(44)).padding(.horizontal, 10)
            .background(style.panel)
            .overlay { Rectangle().stroke(sheet.problem != nil ? style.warning : (sheet.form.focus == .name ? style.accent : style.divider), lineWidth: sheet.form.focus == .name ? 2 : 1).allowsHitTesting(false) }
            .padding(.horizontal, 12)
            .accessibilityLabel("Project name")
    }
    /// What is wrong with the name, as it is typed; otherwise where the project goes.
    @ViewBuilder private var nameNote: some View {
        if let problem = sheet.problem {
            Label(problem.message, systemImage: "exclamationmark.triangle").font(style.system(.footnote)).foregroundStyle(style.warning)
                .fixedSize(horizontal: false, vertical: true).padding(.horizontal, 12).padding(.top, 6)
                .accessibilityAddTraits(.updatesFrequently)
        } else {
            let place = "Created in your Mac’s default projects folder. The phone can’t pick another place."
            Text(sheet.form.check.name.map { "Creates “\($0)” in your Mac’s default projects folder. The phone can’t pick another place." } ?? place)
                .font(style.system(.footnote)).foregroundStyle(style.muted).fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, 12).padding(.top, 6)
        }
    }

    // MARK: Git

    private var gitRow: some View {
        Toggle(isOn: Binding(get: { sheet.form.git }, set: { sheet.setGit($0) })) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Create Git repository")
                Text("Runs git init in the new folder. No commit is made.")
                    .font(style.system(.caption)).foregroundStyle(style.muted).fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 6).frame(minHeight: style.pt(48))
        .overlay { ring(.git) }
    }

    private func messageRow(_ message: ProjectCreateError) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: message.outcomeIsUncertain ? "questionmark.circle" : "exclamationmark.triangle")
            Text(message.message).font(style.system(.footnote)).fixedSize(horizontal: false, vertical: true)
        }
        .foregroundStyle(style.warning).padding(12).frame(maxWidth: .infinity, alignment: .leading)
        .background(style.warning.opacity(0.1))
        .accessibilityElement(children: .combine).accessibilityAddTraits(.updatesFrequently)
        .padding(.top, 8)
    }

    // MARK: Footer

    private var createTitle: String { sheet.error?.outcomeIsUncertain == true ? "Try again" : "Create project" }
    private var footer: some View {
        VStack(spacing: 6) {
            DesktopRule()
            if hardwareKeyboard || sheet.keyboardInUse {
                Text("↩ create   ⎋ cancel   ⇥ next control   ⌘G git").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted)
                    .lineLimit(1).minimumScaleFactor(0.7).padding(.horizontal, 12).accessibilityHidden(true)
            }
            Button { sheet.keyboardInUse = false; sheet.create() } label: {
                HStack(spacing: 8) {
                    if sheet.busy { ProgressView().controlSize(.small) }
                    Text(sheet.busy ? "Creating…" : createTitle).font(style.face(14, bold: true, relativeTo: .headline))
                }.frame(maxWidth: .infinity, minHeight: style.pt(48))
            }
            .buttonStyle(DesktopButtonStyle(prominent: true))
            .disabled(!sheet.canCreate)
            .overlay { ring(.create) }
            .padding(.horizontal, 12).padding(.bottom, 8)
            .accessibilityHint(sheet.unsupported ? ProjectCreateError.unsupportedMessage : "Creates it on your Mac and opens it")
        }.background(style.panel)
    }
}

// MARK: - Keyboard

/// What a key in the sheet means.
enum NewProjectSheetKey: Equatable {
    case form(NewProjectForm.Key)
    case create, cancel, toggleGit
}

typealias NewProjectKeyBinding = (input: String, flags: UIKeyModifierFlags, key: NewProjectSheetKey)

/// The keys of the sheet, as `UIKeyCommand`s with priority over the system's own use of tab, the arrows and escape.
@MainActor enum NewProjectKeyTable {
    /// Wherever the keyboard is, the name field included, where everything else is typing or moving the caret: tab and shift-tab move
    /// the ring (so do up and down), escape cancels. ⌘. is cancel, ⌘↩ is create and ⌘G flips the Git switch, for a keyboard without
    /// tab or escape (the Clicks keyboard has neither).
    static let everywhere: [NewProjectKeyBinding] = [
        ("\t", [], .form(.tab)), ("\t", .shift, .form(.backTab)),
        (UIKeyCommand.inputUpArrow, [], .form(.up)), (UIKeyCommand.inputDownArrow, [], .form(.down)),
        (UIKeyCommand.inputEscape, [], .cancel), (".", .command, .cancel),
        ("\r", .command, .create), ("g", .command, .toggleGit)
    ]
    /// While the switch or Create has the ring nothing is being typed, so the rest of the keys are free: left and right move on, space
    /// flips the switch (or presses Create), return creates.
    static let onControls: [NewProjectKeyBinding] = everywhere + [
        (UIKeyCommand.inputLeftArrow, [], .form(.left)), (UIKeyCommand.inputRightArrow, [], .form(.right)),
        (" ", [], .form(.space)), ("\r", [], .create)
    ]

    static func commands(_ bindings: [NewProjectKeyBinding], action: Selector) -> [UIKeyCommand] {
        bindings.map { binding in
            let command = UIKeyCommand(input: binding.input, modifierFlags: binding.flags, action: action)
            command.wantsPriorityOverSystemBehavior = true
            return command
        }
    }
    static func key(for command: UIKeyCommand, in bindings: [NewProjectKeyBinding]) -> NewProjectSheetKey? {
        guard let input = command.input else { return nil }
        return bindings.first { $0.input == input && $0.flags == command.modifierFlags }?.key
    }
}

/// The name field. It takes the keyboard the moment it is on screen while it has the ring, and carries the commands that must win over
/// plain typing. Return is the delegate's (it creates, from the hardware and the software keyboard alike).
@MainActor final class NewProjectNameField: UITextField {
    var onKey: (NewProjectSheetKey) -> Void = { _ in }
    /// Whether the name has the ring, and so the keyboard.
    var wantsFocus = false
    private lazy var commands = NewProjectKeyTable.commands(NewProjectKeyTable.everywhere, action: #selector(fired(_:)))
    override var keyCommands: [UIKeyCommand]? { commands }

    @objc func fired(_ command: UIKeyCommand) {
        if let key = NewProjectKeyTable.key(for: command, in: NewProjectKeyTable.everywhere) { onKey(key) }
    }
    override func didMoveToWindow() {
        super.didMoveToWindow()
        takeFocus()
    }
    /// A sheet is still animating in when its content is first laid out; the next turn of the run loop is early enough.
    func takeFocus() {
        guard wantsFocus, window != nil, !isFirstResponder else { return }
        DispatchQueue.main.async { [weak self] in
            guard let self, self.wantsFocus, self.window != nil, !self.isFirstResponder else { return }
            _ = self.becomeFirstResponder()
        }
    }
}

private struct NewProjectNameInput: UIViewRepresentable {
    @Binding var text: String
    var wantsFocus: Bool
    var placeholder: String
    var onFocus: () -> Void
    var onKey: (NewProjectSheetKey) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }
    func makeUIView(context: Context) -> NewProjectNameField {
        let field = NewProjectNameField()
        field.delegate = context.coordinator
        field.addTarget(context.coordinator, action: #selector(Coordinator.changed(_:)), for: .editingChanged)
        field.borderStyle = .none
        // A folder name: typed as it is, never corrected, capitalized or dressed in smart punctuation.
        field.autocorrectionType = .no
        field.autocapitalizationType = .none
        field.spellCheckingType = .no
        field.smartQuotesType = .no
        field.smartDashesType = .no
        field.smartInsertDeleteType = .no
        field.inlinePredictionType = .no
        field.mathExpressionCompletionType = .no
        field.writingToolsBehavior = .none
        field.returnKeyType = .done
        field.enablesReturnKeyAutomatically = false
        field.adjustsFontForContentSizeCategory = true
        field.clearButtonMode = .never
        field.setContentHuggingPriority(.defaultLow, for: .horizontal)
        field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        field.accessibilityIdentifier = "new-project-name"
        return field
    }
    func updateUIView(_ field: NewProjectNameField, context: Context) {
        context.coordinator.parent = self
        let style = context.environment.desktopStyle
        if context.coordinator.appliedStyle != style || context.coordinator.appliedPlaceholder != placeholder {
            context.coordinator.appliedStyle = style; context.coordinator.appliedPlaceholder = placeholder
            field.font = UIFontMetrics(forTextStyle: .body).scaledFont(for: style.uiFace(size: 14))
            field.textColor = style.textUI
            field.tintColor = style.accentUI
            field.attributedPlaceholder = NSAttributedString(string: placeholder, attributes: [.foregroundColor: style.mutedUI])
        }
        if field.text != text { field.text = text }
        field.accessibilityLabel = "Project name"
        field.onKey = onKey
        field.wantsFocus = wantsFocus
        field.takeFocus()
    }
    func sizeThatFits(_ proposal: ProposedViewSize, uiView: NewProjectNameField, context: Context) -> CGSize? {
        CGSize(width: proposal.width ?? 200, height: max(uiView.intrinsicContentSize.height, 22))
    }

    @MainActor final class Coordinator: NSObject, UITextFieldDelegate {
        var parent: NewProjectNameInput
        var appliedStyle: DesktopStyle?
        var appliedPlaceholder: String?
        init(_ parent: NewProjectNameInput) { self.parent = parent }
        @objc func changed(_ field: UITextField) { parent.text = field.text ?? "" }
        func textFieldDidBeginEditing(_ textField: UITextField) { parent.onFocus() }
        func textFieldShouldReturn(_ textField: UITextField) -> Bool { parent.onKey(.create); return false }
        func textField(_ textField: UITextField, shouldChangeCharactersIn range: NSRange, replacementString string: String) -> Bool {
            // A name is one line: a pasted line break (or any control character) is dropped.
            guard string.unicodeScalars.contains(where: { KeyItem.isForbidden($0) }) else { return true }
            let cleaned = String(String.UnicodeScalarView(string.unicodeScalars.filter { !KeyItem.isForbidden($0) }))
            if let start = textField.position(from: textField.beginningOfDocument, offset: range.location),
               let end = textField.position(from: start, offset: range.length),
               let target = textField.textRange(from: start, to: end), !cleaned.isEmpty {
                textField.replace(target, withText: cleaned)
                parent.text = textField.text ?? ""
            }
            return false
        }
    }
}

/// Takes the keyboard while the switch or Create has the ring, so that space, return and the arrows still reach the sheet. Not a text
/// input, so no software keyboard comes up for it.
@MainActor final class NewProjectKeyView: UIView {
    var onKey: (NewProjectSheetKey) -> Void = { _ in }
    var wantsFocus = false
    private lazy var commands = NewProjectKeyTable.commands(NewProjectKeyTable.onControls, action: #selector(fired(_:)))
    override var keyCommands: [UIKeyCommand]? { commands }
    override var canBecomeFirstResponder: Bool { true }

    @objc func fired(_ command: UIKeyCommand) {
        if let key = NewProjectKeyTable.key(for: command, in: NewProjectKeyTable.onControls) { onKey(key) }
    }
    override func didMoveToWindow() {
        super.didMoveToWindow()
        takeFocus()
    }
    func takeFocus() {
        guard wantsFocus, window != nil, !isFirstResponder else { return }
        DispatchQueue.main.async { [weak self] in
            guard let self, self.wantsFocus, self.window != nil, !self.isFirstResponder else { return }
            _ = self.becomeFirstResponder()
        }
    }
}

private struct NewProjectKeys: UIViewRepresentable {
    var wantsKeyboard: Bool
    var onKey: (NewProjectSheetKey) -> Void
    func makeUIView(context: Context) -> NewProjectKeyView {
        let view = NewProjectKeyView()
        view.isAccessibilityElement = false
        return view
    }
    func updateUIView(_ view: NewProjectKeyView, context: Context) {
        view.onKey = onKey
        view.wantsFocus = wantsKeyboard
        // A menu or a switch may have taken the keyboard; the next update gives it back.
        view.takeFocus()
    }
}
