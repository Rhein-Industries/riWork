import SwiftUI
import UIKit
import RiWorkCore

// The composer of a chat: a multi-line text view that reads a hardware keyboard the way a chat needs it (⏎ sends, ⇧⏎ is a new line, and
// with a request waiting and nothing typed, ⏎ allows it, ⇧⏎ allows it for the session and ⎋ denies it), and the bars above it for
// requests and questions.

/// The text view of the composer.
///
/// - **Hardware keyboard.** Return and Shift-Return, and with a request waiting also Escape and ⌘⌫, are `UIKeyCommand`s with priority
///   over the system, because a text view would otherwise insert a line break for Return and ignore the rest. What each means is
///   decided by `ChatKeyRouter` from what is on the screen now; this view reports the key, and puts in the line break when the router
///   says it is the text's. The commands are re-read for every key press: Escape and ⌘⌫ belong to the request only while one waits and
///   nothing is typed, and every key is the text view's own while a word is being composed (marked text), so a Japanese or Chinese
///   keyboard still confirms its candidate with Return.
/// - **Software keyboard.** Return makes a new line, as in any chat; the Send button sends.
/// - **⌘. (interrupt) is not here:** it is a SwiftUI shortcut on the screen, so it works wherever the keyboard is.
@MainActor final class ChatComposerTextView: UITextView {
    /// A key the router should judge, and what it came to. The view does not know what is on the screen.
    var onKey: (ChatKey) -> ChatKeyAction = { _ in .none }
    /// A request waits for the keys that answer it (and nothing is typed).
    var answersApproval = false
    private lazy var typingCommands: [UIKeyCommand] = [
        Self.command("\r", [], action: #selector(fired(_:))),
        Self.command("\r", .shift, action: #selector(fired(_:)))
    ]
    private lazy var requestCommands: [UIKeyCommand] = [
        Self.command(UIKeyCommand.inputEscape, [], action: #selector(fired(_:))),
        Self.command(UIKeyCommand.inputDelete, .command, action: #selector(fired(_:)))
    ]
    private static func command(_ input: String, _ flags: UIKeyModifierFlags, action: Selector) -> UIKeyCommand {
        let command = UIKeyCommand(input: input, modifierFlags: flags, action: action)
        command.wantsPriorityOverSystemBehavior = true
        return command
    }
    override var keyCommands: [UIKeyCommand]? {
        // A word being composed is the input method's: its Return is not ours.
        guard markedTextRange == nil else { return nil }
        return answersApproval && text.isEmpty ? typingCommands + requestCommands : typingCommands
    }
    @objc func fired(_ command: UIKeyCommand) {
        guard let key = Self.key(input: command.input, flags: command.modifierFlags) else { return }
        if onKey(key) == .insertNewline { insertText("\n") }
    }
    /// The key a command stands for.
    static func key(input: String?, flags: UIKeyModifierFlags) -> ChatKey? {
        switch (input, flags.intersection([.shift, .command, .alternate, .control])) {
        case ("\r", []): .return
        case ("\r", .shift): .shiftReturn
        case (UIKeyCommand.inputEscape, []): .escape
        case (UIKeyCommand.inputDelete, .command): .commandDelete
        default: nil
        }
    }
}

/// The composer's text view in SwiftUI. It grows with its text up to `maxLines`, then scrolls.
struct ChatComposerField: UIViewRepresentable {
    @Binding var text: String
    var placeholderLabel: String
    var isEnabled: Bool
    var answersApproval: Bool
    /// Counts requests to take the keyboard.
    var focusToken: Int
    var maxLines = 6
    var onKey: (ChatKey) -> ChatKeyAction
    var onFocusChange: (Bool) -> Void = { _ in }
    /// Where dictation puts its words: this view, at its caret.
    var insertion: TextInsertion?

    func makeCoordinator() -> Coordinator { Coordinator(self) }
    func makeUIView(context: Context) -> ChatComposerTextView {
        let view = ChatComposerTextView()
        view.delegate = context.coordinator
        view.backgroundColor = .clear
        view.textContainerInset = UIEdgeInsets(top: 8, left: 4, bottom: 8, right: 4)
        view.textContainer.lineFragmentPadding = 4
        view.isScrollEnabled = false
        view.adjustsFontForContentSizeCategory = true
        // Prompts carry code and flags: no smart punctuation, no corrections.
        view.autocorrectionType = .no
        view.spellCheckingType = .no
        view.smartQuotesType = .no
        view.smartDashesType = .no
        view.smartInsertDeleteType = .no
        view.autocapitalizationType = .sentences
        view.inlinePredictionType = .no
        view.writingToolsBehavior = .none
        view.returnKeyType = .default
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.accessibilityIdentifier = "chat-composer"
        return view
    }
    func updateUIView(_ view: ChatComposerTextView, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        let style = context.environment.desktopStyle
        if coordinator.appliedStyle != style {
            coordinator.appliedStyle = style
            view.font = UIFontMetrics(forTextStyle: .callout).scaledFont(for: .systemFont(ofSize: 16 * CGFloat(style.scale)))
            view.textColor = style.textUI
            view.tintColor = style.accentUI
            view.keyboardAppearance = style.colorScheme == .dark ? .dark : .default
        }
        if view.text != text { view.text = text }
        view.accessibilityLabel = placeholderLabel
        view.isEditable = isEnabled
        view.onKey = onKey
        view.answersApproval = answersApproval
        insertion?.view = view
        if coordinator.lastFocusToken != focusToken {
            coordinator.lastFocusToken = focusToken
            // The view may not be in a window yet (the screen is still arriving): the next turn of the run loop is early enough.
            DispatchQueue.main.async { if view.window != nil, isEnabled, !view.isFirstResponder { _ = view.becomeFirstResponder() } }
        }
    }
    func sizeThatFits(_ proposal: ProposedViewSize, uiView: ChatComposerTextView, context: Context) -> CGSize? {
        let width = proposal.width ?? 280
        let fitted = uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        let line = uiView.font?.lineHeight ?? 20
        let maximum = line * CGFloat(maxLines) + uiView.textContainerInset.top + uiView.textContainerInset.bottom
        let height = min(max(fitted.height, line + uiView.textContainerInset.top + uiView.textContainerInset.bottom), maximum)
        uiView.isScrollEnabled = fitted.height > maximum
        return CGSize(width: width, height: height)
    }

    @MainActor final class Coordinator: NSObject, UITextViewDelegate {
        var parent: ChatComposerField
        var appliedStyle: DesktopStyle?
        var lastFocusToken = 0
        init(_ parent: ChatComposerField) { self.parent = parent }
        func textViewDidChange(_ textView: UITextView) { parent.text = textView.text }
        func textViewDidBeginEditing(_ textView: UITextView) { parent.onFocusChange(true) }
        func textViewDidEndEditing(_ textView: UITextView) { parent.onFocusChange(false) }
    }
}

// MARK: - The composer

/// The text field, Send and Interrupt, and the notice under them.
struct ChatComposer: View {
    @Environment(\.desktopStyle) private var style
    let conversation: ChatConversation
    let provider: ChatProvider
    let state: ChatState
    /// A request waits for the keys that answer it.
    let approval: ChatApproval?
    let connected: Bool
    let focusToken: Int
    let send: () -> Void
    let interrupt: () -> Void
    let decide: (ChatDecision) -> Void
    @State private var focused = false
    @State private var dictation = TextInsertion()

    private var placeholder: String {
        switch state {
        case .stopped, .failed: "Message to start \(provider.title) again"
        default: "Message \(provider.title)"
        }
    }
    private var canSend: Bool { connected && !conversation.sending && !conversation.draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    private var keyContext: ChatKeyContext {
        ChatKeyContext(composerIsEmpty: conversation.draft.isEmpty, approval: approval, busy: state.isBusy, canSend: connected && !conversation.sending)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let notice = conversation.notice {
                HStack(alignment: .top, spacing: 6) {
                    Text(notice).font(style.system(.caption)).foregroundStyle(style.muted).fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 0)
                    Button("Dismiss message", systemImage: "xmark") { conversation.notice = nil }.labelStyle(.iconOnly).font(style.system(.caption)).foregroundStyle(style.muted)
                }
                .padding(.horizontal, 12)
                .accessibilityElement(children: .combine)
            }
            HStack(alignment: .bottom, spacing: 4) {
                ChatComposerField(text: Binding(get: { conversation.draft }, set: { conversation.draft = $0 }), placeholderLabel: "Message to \(provider.title)",
                                  isEnabled: true, answersApproval: approval != nil, focusToken: focusToken, onKey: handle, onFocusChange: { focused = $0 },
                                  insertion: dictation)
                    .overlay(alignment: .topLeading) {
                        if conversation.draft.isEmpty {
                            Text(placeholder).font(style.prose).foregroundStyle(style.muted).padding(.top, 8).padding(.leading, 8)
                                .lineLimit(1).allowsHitTesting(false).accessibilityHidden(true)
                        }
                    }
                    .background(style.background)
                    .overlay(RoundedRectangle(cornerRadius: 3).stroke(focused ? style.accent : style.divider, lineWidth: 1))
                if state.isBusy {
                    Button(action: interrupt) { Image(systemName: "stop.circle.fill").font(.system(size: style.pt(24))).foregroundStyle(style.gold) }
                        .buttonStyle(.plain).frame(width: style.pt(44), height: style.pt(44)).contentShape(Rectangle())
                        .disabled(!connected)
                        .accessibilityLabel("Interrupt").accessibilityHint("Stops what \(provider.title) is doing now")
                }
                DictationButton(owner: .chat(conversation.id), insertion: dictation)
                Button(action: send) {
                    Image(systemName: "arrow.up.circle.fill").font(.system(size: style.pt(28)))
                        .foregroundStyle(canSend ? style.accent : style.muted.opacity(0.6))
                }
                .buttonStyle(.plain).frame(width: style.pt(44), height: style.pt(44)).contentShape(Rectangle())
                .disabled(!canSend)
                .accessibilityLabel("Send").accessibilityHint(conversation.sending ? "Sending" : "Sends the message")
            }
            .padding(.horizontal, 8)
        }
        .padding(.vertical, 6).background(style.panel).overlay(alignment: .top) { DesktopRule() }
    }

    /// A key from the hardware keyboard: the router says what it means now.
    private func handle(_ key: ChatKey) -> ChatKeyAction {
        let action = ChatKeyRouter.action(for: key, in: keyContext)
        switch action {
        case .send: send()
        case .decide(let decision): decide(decision)
        case .interrupt: interrupt()
        case .insertNewline, .none: break
        }
        return action
    }
}

// MARK: - A request waiting

/// The request on top, pinned above the composer: what it is, and the buttons the provider offers.
struct ChatApprovalBar: View {
    @Environment(\.desktopStyle) private var style
    let approval: ChatApproval
    /// How many are waiting, this one included.
    let count: Int
    let keyHints: Bool
    /// The most the details may scroll before they scroll.
    let detailHeight: CGFloat
    let busy: Bool
    let decide: (ChatDecision) -> Void
    @State private var showDetail = false

    private var icon: String {
        switch approval.kind {
        case .command: "terminal"
        case .fileChange: "doc.badge.gearshape"
        case .permissions: "lock.open"
        case .tool: "wrench.and.screwdriver"
        }
    }
    private var kindWord: String {
        switch approval.kind {
        case .command: "Run this command?"
        case .fileChange: "Change these files?"
        case .permissions: "Grant this permission?"
        case .tool: "Use this tool?"
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: icon).foregroundStyle(style.gold).accessibilityHidden(true)
                Text(kindWord).font(style.mono(11, bold: true, relativeTo: .caption)).foregroundStyle(style.gold)
                Spacer(minLength: 0)
                if count > 1 { Text("1 of \(count)").font(style.mono(10, relativeTo: .caption2)).foregroundStyle(style.muted).monospacedDigit() }
            }
            Text(verbatim: approval.title.isEmpty ? "(no description)" : approval.title).font(style.code).foregroundStyle(style.text)
                .lineLimit(showDetail ? 12 : 3).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            if !approval.detail.isEmpty {
                Button { showDetail.toggle() } label: {
                    Label(showDetail ? "Hide details" : "Details", systemImage: "chevron.right").labelStyle(.titleAndIcon)
                        .font(style.mono(10, relativeTo: .caption2)).foregroundStyle(style.muted)
                        .frame(minHeight: style.pt(28), alignment: .leading).contentShape(Rectangle())
                }
                .buttonStyle(.plain).accessibilityValue(showDetail ? "Shown" : "Hidden")
                if showDetail {
                    BoundedScroll(maxHeight: detailHeight) {
                        Group {
                            if approval.kind == .fileChange { DiffView(text: approval.detail) }
                            else { Text(verbatim: approval.detail).font(style.codeSmall).foregroundStyle(style.text).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading) }
                        }
                    }
                }
            }
            LazyVGrid(columns: [GridItem(.adaptive(minimum: style.pt(120)), spacing: 6)], alignment: .leading, spacing: 6) {
                ForEach(approval.offered, id: \.self) { decision in
                    Button { decide(decision) } label: { Text(Self.title(decision)).lineLimit(1).minimumScaleFactor(0.8).frame(maxWidth: .infinity) }
                        .buttonStyle(DesktopButtonStyle(prominent: decision == .accept))
                        .foregroundStyle(decision == .cancel ? style.error : (decision == .accept ? style.accent : style.text))
                        .overlay(Rectangle().stroke(decision == .cancel ? style.error.opacity(0.6) : style.divider, lineWidth: 1))
                        .disabled(busy)
                        .accessibilityLabel(Self.spoken(decision))
                }
            }
            if keyHints {
                Text(Self.hint(for: approval)).font(style.mono(10, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1).minimumScaleFactor(0.7).accessibilityHidden(true)
            }
        }
        .padding(10).frame(maxWidth: .infinity, alignment: .leading)
        .background(style.gold.opacity(0.12))
        .overlay(alignment: .top) { Rectangle().fill(style.gold).frame(height: 2) }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Approval needed. \(kindWord) \(approval.title)")
    }

    static func title(_ decision: ChatDecision) -> String {
        switch decision {
        case .accept: "Allow"
        case .acceptForSession: "Allow for session"
        case .decline: "Deny"
        case .cancel: "Stop"
        }
    }
    static func spoken(_ decision: ChatDecision) -> String {
        switch decision {
        case .accept: "Allow"
        case .acceptForSession: "Allow for the rest of the session"
        case .decline: "Deny"
        case .cancel: "Deny and stop the turn"
        }
    }
    /// The keys that answer it, only those that are offered.
    static func hint(for approval: ChatApproval) -> String {
        [approval.offers(.accept) ? "⏎ allow" : nil, approval.offers(.acceptForSession) ? "⇧⏎ for session" : nil,
         approval.offers(.decline) || approval.offers(.cancel) ? "⎋ deny" : nil].compactMap { $0 }.joined(separator: "  ")
    }
}

// MARK: - Questions

/// The questions of a request: the options as buttons, a field for an answer of your own, and Send.
struct ChatQuestionBar: View {
    @Environment(\.desktopStyle) private var style
    let question: ChatQuestion
    /// The most the questions may scroll before they scroll.
    let scrollHeight: CGFloat
    let busy: Bool
    let submit: (ChatAnswerForm) -> Void
    @State private var form: ChatAnswerForm

    init(question: ChatQuestion, scrollHeight: CGFloat, busy: Bool, submit: @escaping (ChatAnswerForm) -> Void) {
        self.question = question; self.scrollHeight = scrollHeight; self.busy = busy; self.submit = submit
        _form = State(initialValue: ChatAnswerForm(question: question))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: "questionmark.bubble").foregroundStyle(style.accent).accessibilityHidden(true)
                Text(question.questions.count > 1 ? "Questions" : "Question").font(style.mono(11, bold: true, relativeTo: .caption)).foregroundStyle(style.accent)
            }
            BoundedScroll(maxHeight: scrollHeight) {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(question.questions.indices, id: \.self) { prompt in promptView(prompt) }
                }
            }
            Button { submit(form) } label: { Text("Send answer").frame(maxWidth: .infinity) }
                .buttonStyle(DesktopButtonStyle(prominent: true)).disabled(busy || !form.isComplete)
                .accessibilityHint(form.isComplete ? "" : "Answer every question first")
        }
        .padding(10).frame(maxWidth: .infinity, alignment: .leading)
        .background(style.accent.opacity(0.10))
        .overlay(alignment: .top) { Rectangle().fill(style.accent).frame(height: 2) }
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder private func promptView(_ index: Int) -> some View {
        let prompt = question.questions[index]
        VStack(alignment: .leading, spacing: 6) {
            if let header = prompt.header, !header.isEmpty { ChatCaption(text: header.uppercased()) }
            Text(prompt.question).font(style.prose).foregroundStyle(style.text).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            if prompt.multiSelect { Text("Choose any").font(style.system(.caption)).foregroundStyle(style.muted) }
            ForEach(prompt.options.indices, id: \.self) { option in
                let chosen = form.isChosen(prompt: index, option: option)
                Button { form.toggle(prompt: index, option: option) } label: {
                    HStack(alignment: .top, spacing: 8) {
                        Image(systemName: chosen ? (prompt.multiSelect ? "checkmark.square.fill" : "largecircle.fill.circle") : (prompt.multiSelect ? "square" : "circle"))
                            .foregroundStyle(chosen ? style.accent : style.muted)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(prompt.options[option].label).font(style.mono(12, bold: chosen, relativeTo: .footnote)).foregroundStyle(style.text)
                            if !prompt.options[option].description.isEmpty {
                                Text(prompt.options[option].description).font(style.system(.caption)).foregroundStyle(style.muted).fixedSize(horizontal: false, vertical: true)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 8).frame(maxWidth: .infinity, minHeight: style.pt(40), alignment: .leading)
                    .background(chosen ? style.active : .clear).overlay(Rectangle().stroke(chosen ? style.accent : style.divider, lineWidth: 1)).contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(prompt.options[option].label).accessibilityHint(prompt.options[option].description)
                .accessibilityAddTraits(chosen ? .isSelected : [])
            }
            TextField(prompt.options.isEmpty ? "Your answer" : "Or answer in your own words", text: Binding(get: { form.text[index] }, set: { form.setText(prompt: index, $0) }), axis: .vertical)
                .lineLimit(1...4).textFieldStyle(.plain).padding(8).background(style.background)
                .overlay(RoundedRectangle(cornerRadius: 3).stroke(style.divider, lineWidth: 1))
                .font(style.prose)
                .autocorrectionDisabled()
                .accessibilityLabel("Your own answer to: \(prompt.question)")
        }
    }
}
