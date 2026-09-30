import SwiftUI
import UIKit
import Observation
import RiWorkCore

/// Menlo metrics for the terminal grid, measured the same way the text is drawn.
enum TerminalFont {
    static func uiFont(size: Double) -> UIFont { UIFont(name: "Menlo-Regular", size: size) ?? UIFont.monospacedSystemFont(ofSize: size, weight: .regular) }
    static func cell(size: Double) -> (width: Double, height: Double) {
        let font = uiFont(size: size)
        return (("M" as NSString).size(withAttributes: [.font: font]).width, font.lineHeight)
    }
}

/// The compact key row above the keyboard: Esc, Tab, sticky Ctrl, arrows, paste, and hide-keyboard.
@MainActor final class KeyBarView: UIInputView {
    enum Action: Hashable {
        case key(TerminalKey), control, paste, hide
    }
    static let height: CGFloat = 44
    var onAction: ((Action) -> Void)?
    private(set) var buttons: [Action: UIButton] = [:]
    private var repeatTask: Task<Void, Never>?
    private var didRepeat = false

    init() {
        super.init(frame: CGRect(x: 0, y: 0, width: 320, height: Self.height), inputViewStyle: .keyboard)
        allowsSelfSizing = true
        backgroundColor = DesktopStyle.panelUI
        autoresizingMask = .flexibleWidth
        let rule = UIView()
        rule.backgroundColor = DesktopStyle.dividerUI
        rule.translatesAutoresizingMaskIntoConstraints = false
        addSubview(rule)
        let stack = UIStackView()
        stack.axis = .horizontal
        stack.distribution = .fillEqually
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            rule.topAnchor.constraint(equalTo: topAnchor), rule.leadingAnchor.constraint(equalTo: leadingAnchor),
            rule.trailingAnchor.constraint(equalTo: trailingAnchor), rule.heightAnchor.constraint(equalToConstant: 1),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 1), stack.bottomAnchor.constraint(equalTo: bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: safeAreaLayoutGuide.leadingAnchor), stack.trailingAnchor.constraint(equalTo: safeAreaLayoutGuide.trailingAnchor)
        ])
        let layout: [(Action, String?, String?, String)] = [
            (.key(.escape), "Esc", nil, "Escape"), (.key(.tab), "Tab", nil, "Tab"), (.control, "Ctrl", nil, "Control"),
            (.key(.left), nil, "arrow.left", "Left arrow"), (.key(.up), nil, "arrow.up", "Up arrow"),
            (.key(.down), nil, "arrow.down", "Down arrow"), (.key(.right), nil, "arrow.right", "Right arrow"),
            (.paste, nil, "doc.on.clipboard", "Paste"), (.hide, nil, "keyboard.chevron.compact.down", "Hide keyboard")
        ]
        for (action, title, symbol, label) in layout {
            let button = UIButton(type: .system)
            var configuration = UIButton.Configuration.plain()
            configuration.contentInsets = NSDirectionalEdgeInsets(top: 0, leading: 2, bottom: 0, trailing: 2)
            configuration.baseForegroundColor = DesktopStyle.textUI
            if let title {
                configuration.attributedTitle = AttributedString(title, attributes: AttributeContainer([.font: UIFont(name: "Menlo", size: 13) ?? .monospacedSystemFont(ofSize: 13, weight: .regular)]))
            }
            if let symbol { configuration.image = UIImage(systemName: symbol, withConfiguration: UIImage.SymbolConfiguration(pointSize: 14, weight: .regular)) }
            button.configuration = configuration
            button.accessibilityLabel = label
            button.accessibilityIdentifier = "keybar.\(label)"
            button.addAction(UIAction { [weak self] _ in self?.tapped(action) }, for: .touchUpInside)
            if case .key(let key) = action, [.left, .up, .down, .right].contains(key) {
                button.addAction(UIAction { [weak self] _ in self?.beginRepeat(action) }, for: .touchDown)
                button.addAction(UIAction { [weak self] _ in self?.endRepeat() }, for: [.touchUpInside, .touchUpOutside, .touchCancel, .touchDragExit])
            }
            stack.addArrangedSubview(button)
            buttons[action] = button
        }
        setControlArmed(false)
    }
    required init?(coder: NSCoder) { fatalError("KeyBarView is created in code") }
    override var intrinsicContentSize: CGSize { CGSize(width: UIView.noIntrinsicMetric, height: Self.height) }

    /// Ctrl is sticky: it stays highlighted until the next key uses it.
    func setControlArmed(_ armed: Bool) {
        guard let button = buttons[.control] else { return }
        button.configuration?.baseForegroundColor = armed ? DesktopStyle.accentUI : DesktopStyle.textUI
        button.configuration?.background.backgroundColor = armed ? DesktopStyle.activeUI : .clear
        button.isSelected = armed
        button.accessibilityValue = armed ? "armed" : "not armed"
        button.accessibilityTraits = armed ? [.button, .selected] : .button
    }
    /// Programmatic press, used by the buttons and by tests.
    func tapped(_ action: Action) {
        // A key that already repeated while held is not sent once more on release.
        if didRepeat { didRepeat = false; return }
        onAction?(action)
    }
    private func beginRepeat(_ action: Action) {
        didRepeat = false
        repeatTask?.cancel()
        repeatTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(400))
            while !Task.isCancelled, let self {
                self.didRepeat = true
                self.onAction?(action)
                try? await Task.sleep(for: .milliseconds(70))
            }
        }
    }
    private func endRepeat() { repeatTask?.cancel(); repeatTask = nil }
    /// The bar going away (keyboard hidden) while a finger is down must not leave an arrow repeating.
    override func didMoveToWindow() { super.didMoveToWindow(); if window == nil { endRepeat() } }
}

/// A view that becomes first responder to bring up the keyboard, and turns everything typed into `KeyItem`s.
/// It draws nothing: the terminal's own screen shows what the shell echoes.
@MainActor final class KeyCaptureView: UIView, UIKeyInput {
    /// Returns false when the input was refused (buffer full).
    var onItems: (([KeyItem]) -> Bool)?
    var onActiveChange: ((Bool) -> Void)?
    private(set) var mapper = KeyMapper() { didSet { bar.setControlArmed(mapper.controlArmed) } }
    let bar = KeyBarView()
    var isEnabled = true { didSet { if !isEnabled, isFirstResponder { _ = resignFirstResponder() } } }

    // Text input traits: every one of these would change what the shell receives.
    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var spellCheckingType: UITextSpellCheckingType = .no
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no
    var smartInsertDeleteType: UITextSmartInsertDeleteType = .no
    var inlinePredictionType: UITextInlinePredictionType = .no
    var mathExpressionCompletionType: UITextMathExpressionCompletionType = .no
    var writingToolsBehavior: UIWritingToolsBehavior = .none
    var keyboardType: UIKeyboardType = .asciiCapable
    var keyboardAppearance: UIKeyboardAppearance = .default
    var returnKeyType: UIReturnKeyType = .default
    var enablesReturnKeyAutomatically = false
    var isSecureTextEntry = false

    override init(frame: CGRect) {
        super.init(frame: frame)
        isAccessibilityElement = false
        bar.onAction = { [weak self] in self?.barAction($0) }
    }
    required init?(coder: NSCoder) { fatalError("KeyCaptureView is created in code") }

    override var canBecomeFirstResponder: Bool { isEnabled }
    override var inputAccessoryView: UIView? { bar }
    override func becomeFirstResponder() -> Bool {
        let became = super.becomeFirstResponder()
        if became { onActiveChange?(true) }
        return became
    }
    override func resignFirstResponder() -> Bool {
        let resigned = super.resignFirstResponder()
        if resigned { mapper.disarmControl(); bar.setControlArmed(false); onActiveChange?(false) }
        return resigned
    }

    // MARK: UIKeyInput
    /// Always true, or the delete key stops reporting once the (nonexistent) text is empty.
    var hasText: Bool { true }
    func insertText(_ text: String) { emit(mapper.insert(text)) }
    func deleteBackward() { emit(mapper.deleteBackward()) }
    private func emit(_ items: [KeyItem]) {
        guard !items.isEmpty else { return }
        _ = onItems?(items)
    }

    // MARK: Paste
    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        action == #selector(paste(_:)) ? UIPasteboard.general.hasStrings : super.canPerformAction(action, withSender: sender)
    }
    override func paste(_ sender: Any?) { if let text = UIPasteboard.general.string { pasteText(text) } }
    /// Multi-line paste becomes text plus Enter items; other control characters are dropped.
    func pasteText(_ text: String) { emit(KeyMapper.items(for: text)) }

    // MARK: Key bar
    private func barAction(_ action: KeyBarView.Action) {
        switch action {
        case .key(let key): emit(mapper.press(key))
        case .control: mapper.toggleControl()
        case .paste: paste(nil)
        case .hide: _ = resignFirstResponder()
        }
    }

    // MARK: Hardware keyboard
    private static let namedCommands: [(input: String, flags: UIKeyModifierFlags, key: TerminalKey)] = [
        (UIKeyCommand.inputUpArrow, [], .up), (UIKeyCommand.inputDownArrow, [], .down),
        (UIKeyCommand.inputLeftArrow, [], .left), (UIKeyCommand.inputRightArrow, [], .right),
        (UIKeyCommand.inputEscape, [], .escape), ("\t", [], .tab), ("\t", .shift, .backTab),
        (UIKeyCommand.inputHome, [], .home), (UIKeyCommand.inputEnd, [], .end),
        (UIKeyCommand.inputPageUp, [], .pageUp), (UIKeyCommand.inputPageDown, [], .pageDown)
    ]
    private lazy var commands: [UIKeyCommand] = {
        var list = Self.namedCommands.map { UIKeyCommand(input: $0.input, modifierFlags: $0.flags, action: #selector(keyCommandFired(_:))) }
        for value in 97...122 { list.append(UIKeyCommand(input: String(UnicodeScalar(UInt8(value))), modifierFlags: .control, action: #selector(keyCommandFired(_:)))) }
        // Tab, arrows and Escape otherwise move focus or do nothing in the terminal.
        for command in list { command.wantsPriorityOverSystemBehavior = true }
        return list
    }()
    override var keyCommands: [UIKeyCommand]? { commands }
    @objc func keyCommandFired(_ command: UIKeyCommand) {
        guard let input = command.input else { return }
        if command.modifierFlags == .control, let scalar = input.unicodeScalars.first, input.unicodeScalars.count == 1, let key = TerminalKey.control(forLetter: Character(scalar)) {
            emit(mapper.press(key)); return
        }
        if let match = Self.namedCommands.first(where: { $0.input == input && $0.flags == command.modifierFlags }) { emit(mapper.press(match.key)) }
    }
}

/// Lets SwiftUI focus the capture view (tap on the terminal) and see whether the keyboard is up.
@MainActor @Observable final class KeyFocus {
    var isActive = false
    @ObservationIgnored weak var view: KeyCaptureView?
    func focus() { _ = view?.becomeFirstResponder() }
    func dismiss() { _ = view?.resignFirstResponder() }
}

struct KeyCapture: UIViewRepresentable {
    let focus: KeyFocus
    var isEnabled: Bool
    var label: String
    var onItems: ([KeyItem]) -> Bool

    func makeUIView(context: Context) -> KeyCaptureView {
        let view = KeyCaptureView()
        view.onActiveChange = { [focus] active in focus.isActive = active }
        focus.view = view
        return view
    }
    func updateUIView(_ view: KeyCaptureView, context: Context) {
        view.onItems = onItems
        view.isEnabled = isEnabled
        view.accessibilityLabel = label
        focus.view = view
    }
    static func dismantleUIView(_ view: KeyCaptureView, coordinator: ()) {
        // Resign first (reports "inactive"), then detach: the keyboard must not outlive the capture view.
        _ = view.resignFirstResponder()
        view.onItems = nil
        view.onActiveChange = nil
    }
}
