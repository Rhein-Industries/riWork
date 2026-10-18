import SwiftUI
import UIKit
import RiWorkCore

// Keyboard-driven pieces of the hotkey menu and editor: key commands for screens that have no text field, a text field that
// navigates, a view that learns a key, the menu itself and the key readout.

// MARK: - Key commands

/// A key command: what to press and what it does. Shown in the discoverability overlay (hold ⌘) when it has a title.
struct KeyAction {
    var input: String
    var modifiers: UIKeyModifierFlags = []
    var title: String?
    var run: @MainActor () -> Void
}

private struct TabShortcutsEnvironmentKey: EnvironmentKey {
    static let defaultValue: TabShortcutController? = nil
}
extension EnvironmentValues {
    var tabShortcuts: TabShortcutController? {
        get { self[TabShortcutsEnvironmentKey.self] }
        set { self[TabShortcutsEnvironmentKey.self] = newValue }
    }
}
extension View {
    func tabShortcutGuide(_ controller: TabShortcutController? = nil) -> some View {
        modifier(TabShortcutGuideModifier(controller: controller))
    }
}
/// On a sheet: while it is up, the Tab actions it has no use for (1–9, N, P, M) are not passed to the screen behind it.
private struct TabShortcutGuideModifier: ViewModifier {
    @Environment(\.tabShortcuts) private var inherited
    @State private var owner = UUID()
    var controller: TabShortcutController?
    func body(content: Content) -> some View {
        content.overlay { if let controller = controller ?? inherited { TabShortcutGuide(controller: controller) } }
            .onAppear { (controller ?? inherited)?.suspendApp(owner: owner) }
            .onDisappear { (controller ?? inherited)?.resumeApp(owner: owner) }
    }
}

@MainActor enum TabShortcutInput {
    static func record(_ press: UIPress, phase: KeyEventRecord.Phase) -> KeyEventRecord? {
        guard let key = press.key else { return nil }
        return KeyEventRecord(phase: phase, keyCode: Int(key.keyCode.rawValue), modifiers: KeyCaptureView.modifiers(key.modifierFlags),
                              rawModifiers: Int(key.modifierFlags.rawValue), characters: key.characters, charactersIgnoringModifiers: key.charactersIgnoringModifiers)
    }
    static func input(_ action: TabShortcutAction) -> String? {
        switch action {
        case .literalTab: "\t"
        case .escape: UIKeyCommand.inputEscape
        case .up: UIKeyCommand.inputUpArrow
        case .left: UIKeyCommand.inputLeftArrow
        case .down: UIKeyCommand.inputDownArrow
        case .right: UIKeyCommand.inputRightArrow
        default: nil
        }
    }
    /// The commands without plain Tab while Tab is the prefix; the prefix is asked once per query, not once per command.
    static func withoutPrefix(_ commands: [UIKeyCommand], _ controller: TabShortcutController?) -> [UIKeyCommand] {
        guard controller?.isEnabled() == true else { return commands }
        return commands.filter { !($0.input == "\t" && $0.modifierFlags.isEmpty) }
    }
    static func record(_ command: UIKeyCommand) -> KeyEventRecord? {
        guard let input = command.input, let code = KeyCaptureView.keyCode(forCommandInput: input) else { return nil }
        return KeyEventRecord(phase: .command, keyCode: code, modifiers: KeyCaptureView.modifiers(command.modifierFlags), characters: input, charactersIgnoringModifiers: input)
    }
    static func perform(_ action: TabShortcutAction, table: KeyActionTable) -> Bool {
        guard let input = input(action) else { return false }
        guard let entry = table.actions.first(where: { $0.input == input && $0.modifiers.isEmpty }) else { return true }
        entry.run(); return true
    }
}

/// Nonmodal so the same hardware keyboard chooses a shortcut while its complete map is visible.
struct TabShortcutGuide: View {
    @Environment(\.desktopStyle) private var style
    let controller: TabShortcutController
    private let shortcuts = [("Q", "Send Tab"), ("E", "Escape"), ("W A S D", "↑ ← ↓ →"), ("K", "Key menu"),
                             ("1–9", "Switch tab"), ("N", "New terminal"), ("P", "Back / projects"), ("M", "Choose model"), ("H / ?", "This guide")]
    var body: some View {
        if controller.guideVisible {
            VStack(spacing: 0) {
                Spacer(minLength: 16)
                ScrollView {
                  VStack(alignment: .leading, spacing: 10) {
                    HStack {
                        Label("Tab shortcuts", systemImage: "keyboard").font(style.face(17, bold: true, relativeTo: .headline))
                        Spacer()
                        Button("Close", systemImage: "xmark") { controller.cancelPrefix() }.labelStyle(.iconOnly).frame(minWidth: 44, minHeight: 44)
                    }
                    Text(controller.state.unknownShortcut ? "Unknown shortcut. Choose a key below, or press Esc or ⌘E to cancel." : "Press a key after Tab. Hold Tab for one second to show this guide.")
                        .font(style.system(.footnote)).foregroundStyle(style.muted)
                    ForEach(shortcuts, id: \.0) { key, title in
                        HStack {
                            Text("Tab  \(key)").font(.system(.callout, design: .monospaced)).foregroundStyle(style.accent)
                            Spacer()
                            Text(title).font(style.system(.callout)).foregroundStyle(style.text)
                        }
                    }
                    Text("Esc or ⌘E cancels the prefix. Touch Tab still sends a normal Tab. Actions depend on the screen.")
                        .font(style.system(.caption)).foregroundStyle(style.muted)
                  }.padding(16)
                }
                .scrollBounceBehavior(.basedOnSize)
                .frame(maxWidth: 460, maxHeight: 480).background(style.background, in: RoundedRectangle(cornerRadius: 16))
                .overlay(RoundedRectangle(cornerRadius: 16).stroke(style.divider))
                .shadow(color: .black.opacity(0.2), radius: 12, y: 4)
                .accessibilityIdentifier("tab-shortcuts-guide")
                Spacer(minLength: 16)
            }.padding(12)
        }
    }
}

/// The same missing keys everywhere on a Clicks keyboard. These aliases only join commands a screen already handles; ordinary
/// typing and the system's text editing shortcuts stay with their text field when no menu is being navigated.
@MainActor enum ClicksNavigation {
    struct Binding {
        let input: String
        let modifiers: UIKeyModifierFlags
        let alias: String
        let aliasModifiers: UIKeyModifierFlags
    }
    static let bindings: [Binding] = [
        Binding(input: UIKeyCommand.inputEscape, modifiers: [], alias: "e", aliasModifiers: .command),
        Binding(input: "\t", modifiers: [], alias: "t", aliasModifiers: .command),
        Binding(input: "\t", modifiers: .shift, alias: "t", aliasModifiers: [.command, .shift]),
        Binding(input: UIKeyCommand.inputUpArrow, modifiers: [], alias: "w", aliasModifiers: .command),
        Binding(input: UIKeyCommand.inputLeftArrow, modifiers: [], alias: "a", aliasModifiers: .command),
        Binding(input: UIKeyCommand.inputDownArrow, modifiers: [], alias: "s", aliasModifiers: .command),
        Binding(input: UIKeyCommand.inputRightArrow, modifiers: [], alias: "d", aliasModifiers: .command),
        Binding(input: UIKeyCommand.inputPageUp, modifiers: [], alias: "b", aliasModifiers: .command),
        Binding(input: UIKeyCommand.inputPageDown, modifiers: [], alias: "f", aliasModifiers: .command)
    ]

    static func alias(input: String?, modifiers: UIKeyModifierFlags) -> Binding? {
        bindings.first { $0.input == input && $0.modifiers == modifiers }
    }
    static func original(input: String?, modifiers: UIKeyModifierFlags) -> Binding? {
        bindings.first { $0.alias == input && $0.aliasModifiers == modifiers }
    }

    /// An alias takes precedence over a conflicting conventional shortcut while the Clicks profile is in use: ⌘W moves up in a
    /// list instead of closing it. The native key remains available, too.
    @MainActor static func actions(_ actions: [KeyAction], enabled: Bool) -> [KeyAction] {
        guard enabled else { return actions }
        let aliases = actions.compactMap { action -> KeyAction? in
            guard let binding = alias(input: action.input, modifiers: action.modifiers) else { return nil }
            return KeyAction(input: binding.alias, modifiers: binding.aliasModifiers, title: action.title, run: action.run)
        }
        return actions.filter { action in !aliases.contains { $0.input == action.input && $0.modifiers == action.modifiers } } + aliases
    }
}

/// Builds `UIKeyCommand`s from actions and runs the one that fired. Priority over the system, so Tab, arrows and Escape are ours.
@MainActor final class KeyActionTable {
    var actions: [KeyAction] = []
    func commands(action: Selector) -> [UIKeyCommand] {
        actions.map { entry in
            let command = UIKeyCommand(input: entry.input, modifierFlags: entry.modifiers, action: action)
            command.wantsPriorityOverSystemBehavior = true
            if let title = entry.title { command.discoverabilityTitle = title }
            return command
        }
    }
    func run(_ command: UIKeyCommand) {
        actions.first { $0.input == command.input && $0.modifiers == command.modifierFlags }?.run()
    }
}

/// An invisible view that holds the keyboard while it is `active`, so a screen without text fields (the hotkey list) can be driven
/// by key commands. It gives the keyboard up when it is not active or goes away.
struct KeyCommandHost: UIViewRepresentable {
    @Environment(\.tabShortcuts) private var tabShortcuts
    var active: Bool
    var actions: [KeyAction]
    var avoidTextInput = false

    final class HostView: UIView {
        let table = KeyActionTable()
        var tabShortcuts: TabShortcutController?
        var active = false
        var avoidTextInput = false
        override var canBecomeFirstResponder: Bool { true }
        override var keyCommands: [UIKeyCommand]? { TabShortcutInput.withoutPrefix(table.commands(action: #selector(fire(_:))), tabShortcuts) }
        @objc func fire(_ command: UIKeyCommand) {
            if let event = TabShortcutInput.record(command), handleTabShortcut(event) { return }
            table.run(command)
        }
        func handleTabShortcut(_ event: KeyEventRecord) -> Bool {
            tabShortcuts?.handle(event, perform: { TabShortcutInput.perform($0, table: self.table) }) ?? false
        }
        override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            let rest = presses.filter { press in TabShortcutInput.record(press, phase: .down).map { !handleTabShortcut($0) } ?? true }
            if !rest.isEmpty { super.pressesBegan(Set(rest), with: event) }
        }
        override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            let rest = presses.filter { press in TabShortcutInput.record(press, phase: .up).map { !handleTabShortcut($0) } ?? true }
            if !rest.isEmpty { super.pressesEnded(Set(rest), with: event) }
        }
        override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) { tabShortcuts?.cancel(); super.pressesCancelled(presses, with: event) }
        override func resignFirstResponder() -> Bool { let resigned = super.resignFirstResponder(); if resigned { tabShortcuts?.cancelPrefix() }; return resigned }
        override func didMoveToWindow() {
            super.didMoveToWindow()
            if window == nil { _ = resignFirstResponder() } else { takeFocus() }
        }
        private static func firstResponder(in view: UIView) -> UIView? {
            if view.isFirstResponder { return view }
            for child in view.subviews { if let responder = firstResponder(in: child) { return responder } }
            return nil
        }
        private var canTakeFocus: Bool {
            guard active, let window, !isFirstResponder else { return false }
            if avoidTextInput, let responder = Self.firstResponder(in: window), responder !== self, responder is any UITextInput { return false }
            return true
        }
        func takeFocus() {
            guard canTakeFocus else { return }
            DispatchQueue.main.async { [weak self] in
                guard let self, self.canTakeFocus else { return }
                _ = self.becomeFirstResponder()
            }
        }
    }
    func makeUIView(context: Context) -> HostView {
        let view = HostView()
        view.isAccessibilityElement = false
        return view
    }
    func updateUIView(_ view: HostView, context: Context) {
        view.table.actions = actions
        view.tabShortcuts = tabShortcuts
        view.active = active
        view.avoidTextInput = avoidTextInput
        if active { view.takeFocus() } else if view.isFirstResponder { _ = view.resignFirstResponder() }
    }
    static func dismantleUIView(_ view: HostView, coordinator: ()) { _ = view.resignFirstResponder() }
}

// MARK: - A text field that navigates

/// A one-line text field for the hotkey form: key commands go with the field (Tab, Shift-Tab, Escape, Command-S and the rest),
/// Return saves. SwiftUI's `TextField` has no hook for any of those.
struct NavTextField: UIViewRepresentable {
    @Environment(\.tabShortcuts) private var tabShortcuts
    @Binding var text: String
    var placeholder: String
    var label: String
    /// Whether this field is the one that has the keyboard, and the shared actions and Return handler.
    var isFocused: Bool
    var onFocus: () -> Void
    var onBlur: () -> Void
    var actions: [KeyAction]
    var onReturn: () -> Void

    final class Field: UITextField {
        let table = KeyActionTable()
        var tabShortcuts: TabShortcutController?
        override var keyCommands: [UIKeyCommand]? {
            let commands = table.commands(action: #selector(fire(_:)))
            return markedTextRange == nil ? TabShortcutInput.withoutPrefix(commands, tabShortcuts) : commands
        }
        @objc func fire(_ command: UIKeyCommand) {
            if let event = TabShortcutInput.record(command), handleTabShortcut(event) { return }
            table.run(command)
        }
        func handleTabShortcut(_ event: KeyEventRecord) -> Bool {
            tabShortcuts?.handle(event, markedText: markedTextRange != nil, perform: { TabShortcutInput.perform($0, table: self.table) }) ?? false
        }
        override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            let rest = presses.filter { press in TabShortcutInput.record(press, phase: .down).map { !handleTabShortcut($0) } ?? true }
            if !rest.isEmpty { super.pressesBegan(Set(rest), with: event) }
        }
        override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            let rest = presses.filter { press in TabShortcutInput.record(press, phase: .up).map { !handleTabShortcut($0) } ?? true }
            if !rest.isEmpty { super.pressesEnded(Set(rest), with: event) }
        }
        override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) { tabShortcuts?.cancel(); super.pressesCancelled(presses, with: event) }
        override func resignFirstResponder() -> Bool { let resigned = super.resignFirstResponder(); if resigned { tabShortcuts?.cancelPrefix() }; return resigned }
    }
    func makeCoordinator() -> Coordinator { Coordinator(self) }
    func makeUIView(context: Context) -> Field {
        let field = Field()
        field.delegate = context.coordinator
        field.addTarget(context.coordinator, action: #selector(Coordinator.changed(_:)), for: .editingChanged)
        field.borderStyle = .none
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
        field.adjustsFontForContentSizeCategory = true
        field.setContentHuggingPriority(.defaultLow, for: .horizontal)
        field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return field
    }
    func updateUIView(_ field: Field, context: Context) {
        context.coordinator.parent = self
        let style = context.environment.desktopStyle
        if context.coordinator.appliedStyle != style || context.coordinator.appliedPlaceholder != placeholder {
            context.coordinator.appliedStyle = style; context.coordinator.appliedPlaceholder = placeholder
            field.font = UIFontMetrics(forTextStyle: .body).scaledFont(for: style.uiFace(size: 13))
            field.textColor = style.textUI
            field.tintColor = style.accentUI
            field.attributedPlaceholder = NSAttributedString(string: placeholder, attributes: [.foregroundColor: style.mutedUI])
        }
        if field.text != text { field.text = text }
        field.accessibilityLabel = label
        field.table.actions = actions
        field.tabShortcuts = tabShortcuts
        if isFocused, !field.isFirstResponder, field.window != nil {
            DispatchQueue.main.async { if !field.isFirstResponder { _ = field.becomeFirstResponder() } }
        }
    }
    func sizeThatFits(_ proposal: ProposedViewSize, uiView: Field, context: Context) -> CGSize? {
        CGSize(width: proposal.width ?? 200, height: max(uiView.intrinsicContentSize.height, 22))
    }

    @MainActor final class Coordinator: NSObject, UITextFieldDelegate {
        var parent: NavTextField
        var appliedStyle: DesktopStyle?
        var appliedPlaceholder: String?
        init(_ parent: NavTextField) { self.parent = parent }
        @objc func changed(_ field: UITextField) { parent.text = field.text ?? "" }
        func textFieldDidBeginEditing(_ textField: UITextField) { parent.onFocus() }
        func textFieldDidEndEditing(_ textField: UITextField) { parent.onBlur() }
        func textFieldShouldReturn(_ textField: UITextField) -> Bool { parent.onReturn(); return false }
        func textField(_ textField: UITextField, shouldChangeCharactersIn range: NSRange, replacementString string: String) -> Bool {
            // Names and steps are one line: a pasted line break is dropped.
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

// MARK: - Learning a key

/// Watches the keyboard for one key or chord. While it is on screen and `active` it holds the keyboard and sees every key
/// press (a hardware keyboard is needed): `onEvent` gets each event, and `onChord` the first complete chord, a key with the modifiers
/// held, or a modifier pressed and let go on its own (a tap). Escape alone is not a chord, it cancels.
struct KeyLearnView: UIViewRepresentable {
    var active: Bool
    var onEvent: (KeyEventRecord) -> Void
    /// Nil to only watch (the key tester).
    var onChord: ((KeyChord) -> Void)?
    /// Whether Escape on its own cancels (a shortcut cannot be a bare Esc) or is a key like any other (recording a key step).
    var escapeCancels = true
    var onCancel: () -> Void

    final class LearnView: UIView {
        var onEvent: ((KeyEventRecord) -> Void)?
        var onChord: ((KeyChord) -> Void)?
        var onCancel: (() -> Void)?
        var escapeCancels = true
        private var taps = ModifierTapDetector()
        override var canBecomeFirstResponder: Bool { true }

        /// iOS keeps many ⌘ combinations for its own menu (⌘E, ⌘F, ⌘B …) and never hands them to `pressesBegan`. A key command with
        /// priority is the only way to see them, so every combination that includes ⌘ is registered; the others arrive as presses.
        private static let commandCombinations: [UIKeyCommand] = {
            let inputs = KeyCaptureView.learnableInputs
            let flagSets: [UIKeyModifierFlags] = [[.command], [.command, .shift], [.command, .alternate], [.command, .control],
                                                  [.command, .shift, .alternate], [.command, .shift, .control], [.command, .alternate, .control], [.command, .shift, .alternate, .control]]
            return inputs.flatMap { input in
                flagSets.map { flags -> UIKeyCommand in
                    let command = UIKeyCommand(input: input, modifierFlags: flags, action: #selector(LearnView.captured(_:)))
                    command.wantsPriorityOverSystemBehavior = true
                    return command
                }
            }
        }()
        override var keyCommands: [UIKeyCommand]? { Self.commandCombinations }
        @objc func captured(_ command: UIKeyCommand) {
            guard let input = command.input, let code = KeyCaptureView.keyCode(forCommandInput: input) else { return }
            let modifiers = KeyCaptureView.modifiers(command.modifierFlags)
            onEvent?(KeyEventRecord(phase: .command, keyCode: code, modifiers: modifiers, rawModifiers: Int(command.modifierFlags.rawValue), characters: input, charactersIgnoringModifiers: input))
            taps.keyDown(code)   // a key was used with the modifier, so the modifier is not a tap
            onChord?(KeyChord(keyCode: code, modifiers: modifiers))
        }
        override func didMoveToWindow() { super.didMoveToWindow(); if window == nil { _ = resignFirstResponder() } }
        override func resignFirstResponder() -> Bool { taps.reset(); return super.resignFirstResponder() }

        private func event(_ press: UIPress, _ phase: KeyEventRecord.Phase) -> KeyEventRecord? {
            guard let key = press.key else { return nil }
            let record = KeyEventRecord(phase: phase, keyCode: Int(key.keyCode.rawValue), modifiers: KeyCaptureView.modifiers(key.modifierFlags),
                                        rawModifiers: Int(key.modifierFlags.rawValue), characters: key.characters, charactersIgnoringModifiers: key.charactersIgnoringModifiers)
            onEvent?(record)
            return record
        }
        override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            for press in presses {
                guard let record = self.event(press, .down), let code = record.keyCode else { continue }
                taps.keyDown(code)
                guard onChord != nil, !HIDKey.isModifier(code) else { continue }
                if escapeCancels, code == HIDKey.escape, record.modifiers.isEmpty { onCancel?(); continue }
                onChord?(KeyChord(keyCode: code, modifiers: record.modifiers))
            }
        }
        override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            for press in presses {
                guard let record = self.event(press, .up), let code = record.keyCode else { continue }
                if let tap = taps.keyUp(code) { onChord?(tap) }
            }
        }
        override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            taps.reset()
            for press in presses { _ = self.event(press, .cancelled) }
        }
    }
    func makeUIView(context: Context) -> LearnView {
        let view = LearnView()
        view.isAccessibilityElement = false
        return view
    }
    func updateUIView(_ view: LearnView, context: Context) {
        view.onEvent = onEvent; view.onChord = onChord; view.onCancel = onCancel; view.escapeCancels = escapeCancels
        if active, !view.isFirstResponder, view.window != nil {
            DispatchQueue.main.async { if !view.isFirstResponder { _ = view.becomeFirstResponder() } }
        } else if !active, view.isFirstResponder {
            _ = view.resignFirstResponder()
        }
    }
    static func dismantleUIView(_ view: LearnView, coordinator: ()) { _ = view.resignFirstResponder() }
}

// MARK: - The hotkey menu

/// The hotkey menu over the terminal: a filter line, the matching rows, and what the keys do. Driven from the keyboard by
/// `KeyCaptureView`; rows also take taps.
struct HotkeyPaletteView: View {
    @Environment(\.desktopStyle) private var style
    let controller: PaletteController

    var body: some View {
        if let state = controller.state {
            VStack(spacing: 0) {
                HStack(spacing: 8) {
                    Image(systemName: "command").foregroundStyle(style.magenta)
                    (Text(state.query.isEmpty ? "Type to filter hotkeys" : state.query).foregroundStyle(state.query.isEmpty ? style.muted : style.text)
                        + Text("▍").foregroundStyle(style.accent))
                        .lineLimit(1).frame(maxWidth: .infinity, alignment: .leading)
                        .accessibilityLabel("Filter").accessibilityValue(state.query)
                    Button("Close hotkey menu", systemImage: "xmark") { controller.close() }.labelStyle(.iconOnly).buttonStyle(TargetButtonStyle(tinted: true))
                }
                .font(style.face(13, relativeTo: .body)).padding(.leading, 10).frame(minHeight: style.target)
                DesktopRule()
                if state.results.isEmpty {
                    Text("No hotkey matches “\(state.query)”").font(style.system(.footnote)).foregroundStyle(style.muted)
                        .frame(maxWidth: .infinity, minHeight: style.pt(44), alignment: .center)
                } else {
                    ScrollViewReader { proxy in
                        ScrollView {
                            LazyVStack(spacing: 0) {
                                ForEach(Array(state.results.enumerated()), id: \.element.id) { index, entry in
                                    row(entry, selected: index == state.selection).id(entry.id)
                                        .onTapGesture { controller.choose(index: index) }
                                }
                            }
                        }
                        .frame(maxHeight: style.target * 7.5)
                        .onChange(of: state.selection) { _, selection in
                            if state.results.indices.contains(selection) { proxy.scrollTo(state.results[selection].id) }
                        }
                    }
                }
                DesktopRule()
                Text("⇥ ↑↓ move · ⏎ send · ⇧⏎ edit · ⌘N new · ⎋ ⌘K close")
                    .font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1).minimumScaleFactor(0.7)
                    .frame(maxWidth: .infinity, minHeight: style.pt(26))
            }
            .background(style.panel, in: RoundedRectangle(cornerRadius: 8))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(style.divider, lineWidth: 1))
            .shadow(color: .black.opacity(0.25), radius: 8, y: 2)
            .padding(.horizontal, 10).padding(.top, 8).frame(maxWidth: 520)
            .accessibilityElement(children: .contain).accessibilityLabel("Hotkey menu")
            .accessibilityAddTraits(.isModal)
            .transition(.opacity)
        }
    }
    private func row(_ entry: PaletteEntry, selected: Bool) -> some View {
        let tint: Color = switch entry.kind {
        case .hotkey: style.magenta
        case .key: style.text
        case .newHotkey, .configure: style.accent
        }
        return HStack(spacing: 8) {
            Text(entry.title).font(style.face(13, bold: true, relativeTo: .body)).foregroundStyle(tint).lineLimit(1)
            Text(entry.detail).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted).lineLimit(1)
            Spacer(minLength: 4)
            if let shortcut = entry.shortcut {
                Text(shortcut).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.accent)
                    .padding(.horizontal, 5).padding(.vertical, 1).overlay(RoundedRectangle(cornerRadius: 3).stroke(style.divider, lineWidth: 1))
            }
        }
        .padding(.horizontal, 10).frame(minHeight: style.target)
        .background(selected ? style.active : .clear).contentShape(Rectangle())
        .accessibilityElement(children: .combine).accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
    }
}

// MARK: - The hotkey help

/// The hotkey help over the terminal (⌘/, or the key bar's ? button): every hotkey with its shortcut and what it sends, in two groups
/// (with a shortcut, without one), then the app's own shortcuts. It is a reference to look at while typing: it takes no keyboard, so
/// every key and chord goes on working under it. Tapping a row sends that hotkey. A list that does not fit scrolls by touch.
struct HotkeyHelpView: View {
    @Environment(\.desktopStyle) private var style
    let controller: HelpController

    var body: some View {
        if let help = controller.state {
            VStack(spacing: 0) {
                HStack(spacing: 8) {
                    Image(systemName: "questionmark.circle").foregroundStyle(style.magenta).accessibilityHidden(true)
                    Text("Hotkeys").font(style.face(13, bold: true, relativeTo: .body))
                    Spacer(minLength: 4)
                    Text("press a shortcut · ⌘/ ⎋ close").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1).minimumScaleFactor(0.7)
                    Button("Close hotkey help", systemImage: "xmark") { controller.close() }.labelStyle(.iconOnly).buttonStyle(TargetButtonStyle(tinted: true))
                }
                .padding(.leading, 10).frame(minHeight: style.target)
                DesktopRule()
                // The whole list when it fits, a scrolling one when it does not.
                ViewThatFits(in: .vertical) {
                    content(help)
                    ScrollView { content(help) }
                }
                .frame(maxHeight: style.target * 9.5)
            }
            .background(style.panel, in: RoundedRectangle(cornerRadius: 8))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(style.divider, lineWidth: 1))
            .shadow(color: .black.opacity(0.25), radius: 8, y: 2)
            .padding(.horizontal, 10).padding(.top, 8).frame(maxWidth: 520)
            .accessibilityElement(children: .contain).accessibilityLabel("Hotkey help")
            .transition(.opacity)
        }
    }

    private func content(_ help: HotkeyHelp) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            if !help.withShortcut.isEmpty { section(style.cased("With a shortcut"), help.withShortcut) }
            if !help.withoutShortcut.isEmpty { section(style.cased("No shortcut"), help.withoutShortcut) }
            heading(style.cased("App"))
            ForEach(help.appShortcuts) { shortcut in
                HStack(spacing: 8) {
                    Text(shortcut.keys).font(style.face(11, bold: true, relativeTo: .caption)).foregroundStyle(style.accent).lineLimit(1)
                    Text(shortcut.title).font(style.face(12, relativeTo: .body)).lineLimit(1)
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 10).frame(minHeight: style.pt(26))
                .accessibilityElement(children: .combine)
            }
        }
        .padding(.bottom, 4)
    }
    private func heading(_ title: String) -> some View {
        Text(title).font(style.face(10, bold: true, relativeTo: .caption2)).foregroundStyle(style.muted)
            .padding(.horizontal, 10).padding(.top, 8).padding(.bottom, 2).accessibilityAddTraits(.isHeader)
    }
    private func section(_ title: String, _ rows: [HotkeyHelp.Row]) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            heading(title)
            // As many columns as fit: two on an iPhone.
            LazyVGrid(columns: [GridItem(.adaptive(minimum: style.pt(172)), spacing: 0, alignment: .leading)], spacing: 0) {
                ForEach(rows) { cell($0) }
            }
        }
    }
    private func cell(_ row: HotkeyHelp.Row) -> some View {
        HStack(spacing: 6) {
            Text(row.shortcut ?? "—").font(style.face(11, bold: true, relativeTo: .caption)).foregroundStyle(row.shortcut == nil ? style.muted : style.accent)
                .lineLimit(1).frame(minWidth: style.pt(36), alignment: .leading)
            Text(row.label).font(style.face(12, bold: true, relativeTo: .body)).foregroundStyle(style.magenta).lineLimit(1).layoutPriority(1)
            Text(row.sends).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted).lineLimit(1)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 10).frame(maxWidth: .infinity, minHeight: style.target, alignment: .leading)
        .contentShape(Rectangle()).onTapGesture { controller.fire(row.hotkey) }
        .accessibilityElement(children: .ignore).accessibilityAddTraits(.isButton)
        .accessibilityLabel("\(row.label), \(row.shortcut.map { "shortcut \($0)" } ?? "no shortcut"), sends \(row.sends)")
        .accessibilityHint("Sends it")
    }
}

// MARK: - The key readout

/// The last key event the app received: its HID usage, modifiers and characters. It settles what a key, such as the Clicks
/// button, really sends. Touches go through it.
struct KeyEventOverlay: View {
    @Environment(\.desktopStyle) private var style
    let log: KeyEventLog
    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            if let event = log.last {
                ForEach(Array(event.lines.enumerated()), id: \.offset) { _, line in Text(line) }
                Text("events \(log.count)").foregroundStyle(style.muted)
            } else {
                Text("Press a key on the keyboard").foregroundStyle(style.muted)
            }
        }
        .font(style.face(9, relativeTo: .caption2)).monospacedDigit().foregroundStyle(style.text)
        .padding(.horizontal, 6).padding(.vertical, 4)
        .background(style.panel.opacity(0.85), in: RoundedRectangle(cornerRadius: 5))
        .overlay(RoundedRectangle(cornerRadius: 5).stroke(style.divider, lineWidth: 1))
        .padding(6).allowsHitTesting(false).accessibilityHidden(true)
    }
}
