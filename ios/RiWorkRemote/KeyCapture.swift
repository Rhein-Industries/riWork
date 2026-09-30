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

/// A view that becomes first responder to bring up the keyboard, and turns everything typed into `KeyItem`s.
/// It draws nothing: the terminal's own screen shows what the shell echoes.
@MainActor final class KeyCaptureView: UIView, UIKeyInput {
    /// Returns false when the input was refused (buffer full).
    var onItems: (([KeyItem]) -> Bool)?
    var onActiveChange: ((Bool) -> Void)?
    private(set) var mapper = KeyMapper() { didSet { bar.setArmed(control: mapper.controlArmed, alt: mapper.altArmed) } }
    let bar = KeyBarView()
    /// The hotkeys the person added; the built-in ones are always there.
    var hotkeys: [Hotkey] = [] { didSet { bar.hotkeys = hotkeys } }
    var onEditHotkeys: (() -> Void)?
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
    /// Replaces the software keyboard. Tests set an empty view: that is how iOS lays out a hardware keyboard (the bar alone
    /// at the bottom edge), which a simulator cannot attach.
    var inputViewStandIn: UIView?
    override var inputView: UIView? { inputViewStandIn }
    override func becomeFirstResponder() -> Bool {
        let became = super.becomeFirstResponder()
        if became { onActiveChange?(true) }
        return became
    }
    override func resignFirstResponder() -> Bool {
        let resigned = super.resignFirstResponder()
        if resigned { mapper.disarmModifiers(); onActiveChange?(false) }
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
        case .alt: mapper.toggleAlt()
        case .text(let symbol): emit(mapper.insert(symbol))
        case .paste: paste(nil)
        case .hide: _ = resignFirstResponder()
        case .hotkey(let id): if let hotkey = (Hotkey.builtIn + hotkeys).first(where: { $0.id == id }) { emit(mapper.run(hotkey)) }
        case .editHotkeys: onEditHotkeys?()
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
    /// Focus mode draws the bar as a pill when it is alone at the bottom edge (hardware keyboard).
    var presentation = KeyBarView.Presentation.strip
    var hotkeys: [Hotkey] = []
    var onEditHotkeys: () -> Void = {}
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
        // New colors reach the bar in place: the view, its focus and the keyboard are not rebuilt, so typing goes on.
        view.bar.style = context.environment.desktopStyle
        view.bar.presentation = presentation
        view.hotkeys = hotkeys
        view.onEditHotkeys = onEditHotkeys
        focus.view = view
    }
    static func dismantleUIView(_ view: KeyCaptureView, coordinator: ()) {
        // Resign first (reports "inactive"), then detach: the keyboard must not outlive the capture view.
        _ = view.resignFirstResponder()
        view.onItems = nil
        view.onActiveChange = nil
    }
}
