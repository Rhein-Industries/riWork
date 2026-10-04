import SwiftUI
import UIKit
import RiWorkCore

/// One-line command entry. SwiftUI's TextField cannot turn off smart quotes and dashes (`--oneline` would
/// become an em dash, `"` a curly quote), inline prediction, or writing tools, and its vertical axis lets Return insert a newline.
struct CommandField: UIViewRepresentable {
    @Binding var text: String
    var placeholder: String
    var isEnabled: Bool
    var label: String
    var onSubmit: () -> Void
    var onRejectedInput: () -> Void
    /// Takes the keyboard as soon as it is on screen (the review of a dictated line).
    var takesFocus = false

    func makeCoordinator() -> Coordinator { Coordinator(self) }
    func makeUIView(context: Context) -> UITextField {
        let field = UITextField()
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
        field.returnKeyType = .send
        field.enablesReturnKeyAutomatically = true
        field.font = UIFontMetrics(forTextStyle: .body).scaledFont(for: UIFont(name: "Menlo", size: 13) ?? .monospacedSystemFont(ofSize: 13, weight: .regular))
        field.adjustsFontForContentSizeCategory = true
        colorize(field, context.environment.desktopStyle, coordinator: context.coordinator)
        field.setContentHuggingPriority(.defaultLow, for: .horizontal)
        field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        if takesFocus { DispatchQueue.main.async { if field.window != nil { _ = field.becomeFirstResponder() } } }
        return field
    }
    /// Colors are applied on creation and again only when the synced palette changes, so typing is never disturbed.
    private func colorize(_ field: UITextField, _ style: DesktopStyle, coordinator: Coordinator) {
        guard coordinator.appliedStyle != style || coordinator.appliedPlaceholder != placeholder else { return }
        coordinator.appliedStyle = style; coordinator.appliedPlaceholder = placeholder
        field.font = UIFontMetrics(forTextStyle: .body).scaledFont(for: style.uiFont("Menlo", size: 13))
        field.textColor = style.textUI
        field.tintColor = style.accentUI
        field.attributedPlaceholder = NSAttributedString(string: placeholder, attributes: [.foregroundColor: style.mutedUI])
    }
    func updateUIView(_ field: UITextField, context: Context) {
        context.coordinator.parent = self
        colorize(field, context.environment.desktopStyle, coordinator: context.coordinator)
        if field.text != text { field.text = text }
        field.isEnabled = isEnabled
        field.alpha = isEnabled ? 1 : 0.45
        field.accessibilityLabel = label
    }
    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UITextField, context: Context) -> CGSize? {
        CGSize(width: proposal.width ?? 200, height: max(uiView.intrinsicContentSize.height, 22))
    }

    @MainActor final class Coordinator: NSObject, UITextFieldDelegate {
        var parent: CommandField
        var appliedStyle: DesktopStyle?
        var appliedPlaceholder: String?
        init(_ parent: CommandField) { self.parent = parent }
        @objc func changed(_ field: UITextField) { parent.text = field.text ?? "" }
        func textFieldShouldReturn(_ textField: UITextField) -> Bool { parent.onSubmit(); return false }
        func textField(_ textField: UITextField, shouldChangeCharactersIn range: NSRange, replacementString string: String) -> Bool {
            guard string.unicodeScalars.contains(where: InputValidation.isForbidden) else { return true }
            // A copied command often ends in a newline: keep the command and drop the break. Anything multi-line is refused.
            let cleaned = string.trimmingCharacters(in: .newlines)
            guard !cleaned.unicodeScalars.contains(where: InputValidation.isForbidden) else { parent.onRejectedInput(); return false }
            if let start = textField.position(from: textField.beginningOfDocument, offset: range.location),
               let end = textField.position(from: start, offset: range.length),
               let target = textField.textRange(from: start, to: end) {
                textField.replace(target, withText: cleaned)
                parent.text = textField.text ?? ""
            }
            return false
        }
    }
}
