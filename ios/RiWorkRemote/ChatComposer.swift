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
    private var focusRequested = false
    /// SwiftUI may request focus before this view is attached. Keep that one request until UIKit gives it a window.
    func requestFocusWhenAttached() {
        focusRequested = true
        DispatchQueue.main.async { [weak self] in self?.fulfilFocusRequest() }
    }
    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window != nil { DispatchQueue.main.async { [weak self] in self?.fulfilFocusRequest() } }
    }
    private func fulfilFocusRequest() {
        guard focusRequested, window != nil, isEditable else { return }
        if isFirstResponder || becomeFirstResponder() { focusRequested = false }
    }

    /// A key the router should judge, and what it came to. The view does not know what is on the screen.
    var onKey: (ChatKey) -> ChatKeyAction = { _ in .none }
    /// A request waits for the keys that answer it (and nothing is typed).
    var answersApproval = false
    /// A paste that finds files or a lone picture sends them to the Mac (`PasteboardAttachments`); true when it took the paste.
    var onPasteFiles: (() -> Bool)?
    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        if action == #selector(paste(_:)), onPasteFiles != nil, PasteboardAttachments.available { return true }
        return super.canPerformAction(action, withSender: sender)
    }
    override func paste(_ sender: Any?) {
        if let onPasteFiles, PasteboardAttachments.available, onPasteFiles() { return }
        super.paste(sender)
    }
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
    var onPasteFiles: (() -> Bool)?

    /// Space above and below the text inside the field.
    static let verticalInset: CGFloat = 12
    /// Space between the field's text and its ends (the paperclip, the buttons); there is no container inset beside it.
    static let lineFragmentPadding: CGFloat = 4
    /// The text's font, at the text size `traits` ask for (the current one when nil).
    static func font(_ style: DesktopStyle, _ traits: UITraitCollection? = nil) -> UIFont {
        UIFontMetrics(forTextStyle: .body).scaledFont(for: .systemFont(ofSize: 17 * CGFloat(style.scale)), compatibleWith: traits)
    }
    /// The height of the field holding one line; its last line is centred in the band this tall at the field's bottom.
    static func lineBand(_ style: DesktopStyle, _ size: DynamicTypeSize) -> CGFloat {
        font(style, UITraitCollection(preferredContentSizeCategory: UIContentSizeCategory(size))).lineHeight + 2 * verticalInset
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }
    func makeUIView(context: Context) -> ChatComposerTextView {
        let view = ChatComposerTextView()
        view.delegate = context.coordinator
        view.backgroundColor = .clear
        view.textContainerInset = UIEdgeInsets(top: 12, left: 8, bottom: 12, right: 8)
        view.textContainer.lineFragmentPadding = Self.lineFragmentPadding
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
        let category = context.environment.dynamicTypeSize
        if coordinator.appliedStyle != style || coordinator.appliedTypeSize != category {
            coordinator.appliedStyle = style
            coordinator.appliedTypeSize = category
            // The field's rounded ends hold the paperclip and the buttons; the text needs no room of its own beside them beyond the
            // line fragment's padding.
            view.textContainerInset = UIEdgeInsets(top: Self.verticalInset, left: 0, bottom: Self.verticalInset, right: 0)
            view.font = Self.font(style, view.traitCollection)
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
        view.onPasteFiles = onPasteFiles
        if coordinator.lastFocusToken != focusToken {
            coordinator.lastFocusToken = focusToken
            if isEnabled { view.requestFocusWhenAttached() }
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
        var appliedTypeSize: DynamicTypeSize?
        var lastFocusToken = 0
        init(_ parent: ChatComposerField) { self.parent = parent }
        func textViewDidChange(_ textView: UITextView) { parent.text = textView.text }
        func textViewDidBeginEditing(_ textView: UITextView) { parent.onFocusChange(true) }
        func textViewDidEndEditing(_ textView: UITextView) { parent.onFocusChange(false) }
    }
}

// MARK: - The composer

/// The text field, Send and Interrupt. (What went wrong is said in the banner row above it, `ChatNoticeBanners`.)
///
/// One rounded field holds everything: the paperclip at its leading end, the text, and one action at its trailing end that changes with
/// what is useful now (`ComposerAction`): Send once something is typed, Stop while the agent works, the mic otherwise. The field has the
/// transcript's margins (`fieldMargin`); the glyphs stay small inside it, so the text gets the width, and the buttons at its ends keep a
/// 44-point target that reaches out over the margin to the screen's edge.
struct ChatComposer: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.dynamicTypeSize) private var typeSize
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
    /// The paperclip (a photo or a file goes to the Mac and its path into the message), and a paste of files.
    var attach: ((AttachmentChoice) -> Void)?
    var pasteFiles: (() -> Bool)?
    /// The cards of the files staged for the message (`ChatAttachmentStrip`), and × on one of them.
    var attachmentImages: ChatAttachmentImages?
    var removeAttachment: ((String) -> Void)?
    /// Files on their way to the Mac, as cards with their progress; × cancels them. Send waits for them.
    var pending: [PendingAttachment] = []
    var cancelPending: (() -> Void)?
    var dictation = DictationController.shared
    @State private var focused = false
    @State private var insertion = TextInsertion()

    private var canSend: Bool { connected && !conversation.sending && pending.isEmpty && conversation.hasMessage }
    private var keyContext: ChatKeyContext {
        ChatKeyContext(composerIsEmpty: conversation.draft.isEmpty && conversation.attachments.isEmpty, approval: approval, busy: state.isBusy, canSend: connected && !conversation.sending && pending.isEmpty)
    }
    private var actions: ComposerActions {
        ComposerActions(typed: conversation.hasMessage, busy: state.isBusy, mic: style.mic,
                        // A dictation that failed keeps the mic (and the alert it owns) until the alert is dismissed.
                        dictating: dictation.phase(for: .chat(conversation.id)) != .idle)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            // Every button is centred on the field's last line: on the field's middle while it holds one line, and beside the line
            // being typed (at the bottom, as Messages does) once it grows.
            let actions = actions
            if let attachmentImages, !conversation.attachments.isEmpty || !pending.isEmpty {
                ChatAttachmentStrip(attachments: conversation.attachments, pending: pending, images: attachmentImages, remove: { removeAttachment?($0) },
                                    cancelPending: { cancelPending?() }, leadingInset: ChatItemRow.horizontalInset)
                    .chatLayoutProbe("attachments")
            }
            HStack(alignment: .composerLine, spacing: 0) {
                if let attach {
                    // Its target reaches out over the margin to the screen's edge: drawn above the field.
                    ComposerPaperclip(connected: connected, reach: Self.fieldMargin, choose: attach).equatable().zIndex(1)
                }
                ChatComposerField(text: Binding(get: { conversation.draft }, set: { conversation.draft = $0 }), placeholderLabel: "Message to \(provider.title)",
                                  isEnabled: true, answersApproval: approval != nil, focusToken: focusToken, maxLines: typeSize.isAccessibilitySize ? 3 : 6, onKey: handle, onFocusChange: { focused = $0 },
                                  insertion: insertion, onPasteFiles: pasteFiles)
                    .alignmentGuide(.composerLine) { [band = ChatComposerField.lineBand(style, typeSize)] d in d.height - band / 2 }
                    .chatLayoutProbe("composer-field")
                // The last button's target reaches out over the margin to the screen's edge.
                if actions.stop { stopButton(reach: actions.mic || actions.send ? 0 : Self.fieldMargin) }
                if actions.mic {
                    DictationButton(owner: .chat(conversation.id), insertion: insertion, compact: true, reach: Self.fieldMargin, alerts: false).chatLayoutProbe("mic")
                }
                if actions.send { sendButton }
            }
            .padding(.leading, attach == nil ? style.pt(4) : 0).padding(.trailing, 0)
            .modifier(ComposerFieldSurface(focused: focused))
            .animation(.easeInOut(duration: 0.12), value: actions)
            .chatLayoutProbe("composer")
            .padding(.horizontal, Self.fieldMargin)
        }
        .animation(.easeInOut(duration: 0.15), value: conversation.attachments)
        // The card row spans the horizontal safe-area edges, its first card at the transcript's margin, and the field stands as close
        // above the keyboard as the terminal's key bar's capsule (`BottomBarGeometry.composerInsets`). Nothing is drawn behind them: the
        // field's own shape is on the screen's background.
        .padding(.horizontal, BottomBarGeometry.composerInsets(glass: style.glass).horizontal).padding(.top, 6)
        .padding(.bottom, BottomBarGeometry.composerInsets(glass: style.glass).bottom)
    }

    /// The field's margin on either side: the transcript's (`ChatItemRow.horizontalInset`), so the field lines up with the messages.
    static let fieldMargin = ChatItemRow.horizontalInset
    /// A button's column at the field's trailing end: its 30-point circle and 8 points after it. Its target is 44 points, from the
    /// column's start out to the screen's edge for the last button (`reach`), else back over the text's end. (It cannot reach over the
    /// text further: the text view, a UIKit view, takes the touches over it.)
    static func buttonColumn(_ style: DesktopStyle) -> CGFloat { style.pt(30) + style.pt(8) }
    /// A filled circle at the start of its column at the field's trailing end, `reach` more of target past the column.
    private func circle(_ symbol: String, fill: Color, glyph: Color, size: CGFloat = 15, reach: CGFloat) -> some View {
        Image(systemName: symbol).font(.system(size: style.pt(size), weight: .bold)).foregroundStyle(glyph)
            .frame(width: style.pt(30), height: style.pt(30)).background(fill, in: Circle())
            .frame(width: Self.buttonColumn(style) + reach, alignment: .leading)
    }
    private var sendButton: some View {
        Button(action: send) { circle("arrow.up", fill: canSend ? style.accent : style.active, glyph: canSend ? style.background : style.muted, reach: Self.fieldMargin) }
            .buttonStyle(TargetButtonStyle(dims: false, alignment: .trailing))
            .disabled(!canSend)
            .transition(.scale(scale: 0.6).combined(with: .opacity))
            .chatLayoutProbe("send")
            .padding(.trailing, -Self.fieldMargin)
            .accessibilityLabel("Send").accessibilityHint(conversation.sending ? "Sending" : "Sends the message")
    }
    private func stopButton(reach: CGFloat) -> some View {
        Button(action: interrupt) { circle("stop.fill", fill: style.gold.opacity(0.16), glyph: style.gold, size: 12, reach: reach) }
            .buttonStyle(TargetButtonStyle(dims: false, alignment: .trailing))
            .disabled(!connected)
            .transition(.opacity)
            .chatLayoutProbe("stop")
            .padding(.trailing, -reach)
            .accessibilityLabel("Interrupt").accessibilityHint("Stops what \(provider.title) is doing now")
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

extension VerticalAlignment {
    private enum ComposerLine: AlignmentID {
        static func defaultValue(in d: ViewDimensions) -> CGFloat { d[VerticalAlignment.center] }
    }
    /// The middle of the composer field's last line. A button's is its own middle.
    static let composerLine = VerticalAlignment(ComposerLine.self)
}

/// The composer's paperclip and its menu. Drawn again only when `connected` changes (or the look): the composer is redrawn with each
/// change to the chat while it streams, and a menu whose view is redrawn while it is open stops taking taps on its items.
private struct ComposerPaperclip: View, Equatable {
    @Environment(\.desktopStyle) private var style
    let connected: Bool
    /// How far the target reaches out past the field's edge: the field's margin, to the screen's edge.
    let reach: CGFloat
    let choose: (AttachmentChoice) -> Void
    nonisolated static func == (lhs: Self, rhs: Self) -> Bool { lhs.connected == rhs.connected && lhs.reach == rhs.reach }
    /// The paperclip's slot: the glyph's ink 8 points in from the field's edge (6 and its side bearing), and the text 8 points after it.
    /// Its target is the slot and the margin outside the field, from the screen's edge (44 points at least, past the slot over the text's
    /// leading padding when the two are less), the field's height tall.
    static func slot(_ style: DesktopStyle) -> CGFloat { style.pt(29) }
    var body: some View {
        let width = max(style.target, reach + Self.slot(style))
        AttachMenu(choose: choose) {
            Image(systemName: "paperclip").font(.system(size: style.pt(18))).foregroundStyle(connected ? style.muted : style.muted.opacity(0.5))
                .padding(.leading, reach + style.pt(6)).frame(width: width, height: style.target, alignment: .leading).contentShape(Rectangle())
        }
        // No padding of the menu's own around the target: the paperclip sits in the field's rounded leading end.
        .menuStyle(.button).buttonStyle(.plain)
        .disabled(!connected)
        .chatLayoutProbe("paperclip")
        .padding(.leading, -reach).padding(.trailing, Self.slot(style) + reach - width)
        .accessibilityHint("Sends it to the Mac and puts its path in the message")
    }
}

/// What the composer's field is drawn on: the whole row, paperclip and buttons included. The terminal look: the background in a hairline
/// frame that turns the accent color with the keyboard. Native: a rounded field, as Messages draws one, filled with the background and
/// framed the same way. Not of glass on iOS 26: glass casts a soft shadow well past the field's shape, a band behind the composer.
private struct ComposerFieldSurface: ViewModifier {
    @Environment(\.desktopStyle) private var style
    let focused: Bool
    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: style.pt(23), style: .continuous)
        if style.native {
            content.background(style.background, in: shape).overlay(shape.stroke(focused ? style.accent : style.divider, lineWidth: 1))
        } else {
            content.background(style.background).overlay(RoundedRectangle(cornerRadius: 3).stroke(focused ? style.accent : style.divider, lineWidth: 1))
        }
    }
}

/// Which buttons the composer's field holds at its trailing end. One slot, shared: Send once something is typed, Stop while the agent
/// works and nothing is typed, the mic (when the desktop's mic setting is on) while neither; Send, greyed, when there is no mic. The
/// mic stays while it listens, whatever the field holds, and Stop stays beside Send while the agent works, so Interrupt is one tap away
/// in every state (⌘. too, with a keyboard).
struct ComposerActions: Equatable {
    let send: Bool, stop: Bool, mic: Bool
    init(typed: Bool, busy: Bool, mic micOn: Bool, dictating: Bool) {
        mic = dictating || (micOn && !typed && !busy)
        stop = busy
        send = !dictating && (typed || (!busy && !micOn))
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
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Image(systemName: icon).foregroundStyle(style.gold).accessibilityHidden(true)
                Text(kindWord).font(style.system(.subheadline, weight: .semibold)).foregroundStyle(style.gold)
                Spacer(minLength: 0)
                if count > 1 { Text("1 of \(count)").font(style.system(.caption)).foregroundStyle(style.muted).monospacedDigit() }
                if !approval.detail.isEmpty {
                    Button { showDetail.toggle() } label: {
                        Image(systemName: showDetail ? "info.circle.fill" : "info.circle")
                            .font(style.system(.body)).foregroundStyle(style.muted)
                            .frame(width: 44, height: 44).contentShape(Rectangle())
                    }
                    .buttonStyle(.plain).accessibilityLabel(showDetail ? "Hide approval details" : "Show approval details")
                    .accessibilityValue(showDetail ? "Shown" : "Hidden")
                }
            }
            Text(verbatim: approval.title.isEmpty ? "(no description)" : approval.title).font(style.code).foregroundStyle(style.text)
                .lineLimit(showDetail ? 12 : 3).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            if !approval.detail.isEmpty {
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
                    decisionButton(decision)
                        .disabled(busy)
                        .accessibilityLabel(Self.spoken(decision))
                        .chatLayoutProbe("approval-\(decision.rawValue)")
                }
            }
            if keyHints {
                Text(Self.hint(for: approval)).font(style.system(.caption)).foregroundStyle(style.muted).lineLimit(1).minimumScaleFactor(0.7).accessibilityHidden(true)
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 8).frame(maxWidth: .infinity, alignment: .leading)
        .chatRequestSurface(style, tint: style.gold, opacity: 0.12)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Approval needed. \(kindWord) \(approval.title)")
    }

    /// One answer. The terminal look frames each in a rectangle; Native makes them capsules, Allow the filled one, the others on glass
    /// (iOS 26) or outlined.
    @ViewBuilder private func decisionButton(_ decision: ChatDecision) -> some View {
        let button = Button { decide(decision) } label: { Text(Self.title(decision)).font(style.system(.subheadline, weight: .semibold)).fixedSize(horizontal: false, vertical: true).frame(maxWidth: .infinity, minHeight: 44) }
            .buttonStyle(DesktopButtonStyle(prominent: decision == .accept))
            .foregroundStyle(decision == .cancel ? style.error : (decision == .accept ? style.accent : style.text))
        if !style.native {
            button.overlay(Rectangle().stroke(decision == .cancel ? style.error.opacity(0.6) : style.divider, lineWidth: 1))
        } else if decision == .accept {
            button
        } else if style.glass {
            // The button style sets the label's color, so Stop's red is the outline, on glass as off it.
            button.nativeGlass(style, in: Capsule()).overlay { if decision == .cancel { Capsule().stroke(style.error.opacity(0.6), lineWidth: 1) } }
        } else {
            button.overlay(Capsule().stroke(decision == .cancel ? style.error.opacity(0.6) : style.divider, lineWidth: 1))
        }
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

/// A request the relay could not send in full: it is waiting, its details are on the Mac, and nothing here answers it (no decisions,
/// no keys), so a partial request is never taken for the whole one.
struct ChatElidedRequestBar: View {
    @Environment(\.desktopStyle) private var style
    let request: ChatElidedRequest
    /// How many are waiting, this one included.
    let count: Int
    private var title: String { request.question ? "A question is waiting" : "An approval is waiting" }
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Image(systemName: "macbook").foregroundStyle(style.gold).accessibilityHidden(true)
                Text(title).font(style.system(.subheadline, weight: .semibold)).foregroundStyle(style.gold)
                Spacer(minLength: 0)
                if count > 1 { Text("1 of \(count)").font(style.system(.caption)).foregroundStyle(style.muted).monospacedDigit() }
            }
            Text("Details shown on your Mac. It was too large to send to the phone; answer it there.")
                .font(style.system(.footnote)).foregroundStyle(style.muted).fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, 12).padding(.vertical, 10).frame(maxWidth: .infinity, alignment: .leading)
        .chatRequestSurface(style, tint: style.gold, opacity: 0.12)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(title). Details shown on your Mac; answer it there.")
        .chatLayoutProbe("elided-request")
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
                Text(question.questions.count > 1 ? "Questions" : "Question").font(style.system(.subheadline, weight: .semibold)).foregroundStyle(style.accent)
                // Native: Send sits in the header, as a sheet's Done does, and leaves the answers the room under it.
                if style.native {
                    Spacer(minLength: 4)
                    sendButton { Text("Send") }.buttonStyle(DesktopButtonStyle(prominent: true, compact: true))
                }
            }
            BoundedScroll(maxHeight: scrollHeight) {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(question.questions.indices, id: \.self) { prompt in promptView(prompt) }
                }
            }
            if !style.native {
                sendButton { Text("Send answer").frame(maxWidth: .infinity) }.buttonStyle(DesktopButtonStyle(prominent: true))
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 8).frame(maxWidth: .infinity, alignment: .leading)
        .chatRequestSurface(style, tint: style.accent, opacity: 0.10)
        .accessibilityElement(children: .contain)
    }

    private func sendButton(@ViewBuilder _ label: () -> some View) -> some View {
        Button { submit(form) } label: { label() }
            .disabled(busy || !form.isComplete)
            .accessibilityLabel("Send answer")
            .chatLayoutProbe("question-send")
            .accessibilityHint(form.isComplete ? "" : "Answer every question first")
    }

    @ViewBuilder private func promptView(_ index: Int) -> some View {
        let prompt = question.questions[index]
        VStack(alignment: .leading, spacing: 6) {
            if let header = prompt.header, !header.isEmpty { ChatCaption(text: header) }
            Text(prompt.question).chatProse().foregroundStyle(style.text).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                .boundedScrollBreak()
            if prompt.multiSelect { Text("Choose any").font(style.system(.caption)).foregroundStyle(style.muted).boundedScrollBreak() }
            ForEach(prompt.options.indices, id: \.self) { option in
                let chosen = form.isChosen(prompt: index, option: option)
                Button { form.toggle(prompt: index, option: option) } label: {
                    HStack(alignment: .top, spacing: 8) {
                        Image(systemName: chosen ? (prompt.multiSelect ? "checkmark.square.fill" : "largecircle.fill.circle") : (prompt.multiSelect ? "square" : "circle"))
                            .foregroundStyle(chosen ? style.accent : style.muted)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(prompt.options[option].label).font(style.system(.subheadline, weight: chosen ? .semibold : .regular)).foregroundStyle(style.text)
                            if !prompt.options[option].description.isEmpty {
                                Text(prompt.options[option].description).font(style.system(.caption)).foregroundStyle(style.muted).fixedSize(horizontal: false, vertical: true)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 8).frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                    .background(chosen ? style.active : .clear, in: RoundedRectangle(cornerRadius: 10)).overlay(RoundedRectangle(cornerRadius: 10).stroke(chosen ? style.accent : style.divider, lineWidth: 1)).contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .boundedScrollBreak()
                .accessibilityLabel(prompt.options[option].label).accessibilityHint(prompt.options[option].description)
                .accessibilityAddTraits(chosen ? .isSelected : [])
            }
            TextField(prompt.options.isEmpty ? "Your answer" : "Or answer in your own words", text: Binding(get: { form.text[index] }, set: { form.setText(prompt: index, $0) }), axis: .vertical)
                .lineLimit(1...4).modifier(DesktopField())
                .chatProse()
                .boundedScrollBreak()
                .autocorrectionDisabled()
                .accessibilityLabel("Your own answer to: \(prompt.question)")
        }
    }
}
