import SwiftUI
import UIKit
import RiWorkCore

/// A chat, full screen under the tab strip: a compact toolbar (mode, Compact, usage), what the chat is doing when it is not simply
/// ready, the transcript, the request or question that waits, and the composer.
///
/// The screen follows the chat while it is on screen (`RemoteModel.followChat` runs in `.task`, so it ends with the screen), keeps the
/// transcript at the bottom while it streams unless the reader has scrolled up, and gives the keyboard to the composer when the chat
/// opens, by the same rule as the terminal (Settings, "Focus keyboard when a shell opens").
///
/// There is no `KeyCapture` here: that is the terminal's key view (⌘K ⌘, ⌘/, hotkeys, the Clicks template), and it is not in this
/// hierarchy. The keys of a chat are the composer's (see `ChatComposerTextView`) and ⌘. below, and none is one the terminal uses.
struct ChatScreen: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel
    let chat: ChatInfo
    /// Counts times a sheet over the screen went away: the composer takes the keyboard back.
    var refocus = 0
    @State private var focusToken = 0
    /// The model picker is up.
    @State private var showModels = false
    /// The height the screen has now. With the software keyboard up it is under half of the phone, and the bars above the composer must
    /// leave the transcript room, so what they may scroll is a share of it.
    @State private var height: CGFloat = 800
    /// The photo or file picker of the composer's paperclip.
    @State private var picking: AttachmentChoice?
    /// Scrolling room the bar over the composer gives up so the transcript keeps its last message in view (`transcriptChanged`).
    @State private var squeeze: CGFloat = 0

    private var conversation: ChatConversation { model.chatConversations[chat.id] ?? ChatConversation(id: chat.id) }
    private var state: ChatState { model.chatState(chat) }
    private var info: ChatInfo { conversation.transcript.info ?? chat }
    private var connected: Bool { model.state == .connected }

    var body: some View {
        let conversation = conversation
        let approvals = conversation.openApprovals
        let questions = conversation.openQuestions
        VStack(spacing: 0) {
            ChatToolbar(model: model, chat: info, conversation: conversation, state: state, showModels: $showModels)
            ChatStatusLines(model: model, chat: info, conversation: conversation, state: state)
            ChatTranscriptList(conversation: conversation, provider: info.provider, state: state, hardwareKeyboard: model.keyboard.hardware.isAttached)
                .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { transcriptChanged($0, barShown: !approvals.isEmpty || !questions.isEmpty) }
            if let approval = approvals.first {
                ChatApprovalBar(approval: approval, count: approvals.count, keyHints: model.keyboard.hardware.isAttached, detailHeight: max(70, height * 0.2 - squeeze),
                                busy: !connected || conversation.answered.contains(approval.requestID)) { decision in decide(approval, decision) }
                    .id(approval.requestID)
            } else if let question = questions.first {
                ChatQuestionBar(question: question, scrollHeight: max(leastQuestionScroll, height * 0.3 - squeeze), busy: !connected || conversation.answered.contains(question.requestID)) { form in
                    Task { await model.answerChatQuestion(chat.id, form) }
                }
                .id(question.requestID)
            }
            if let activity = model.uploadActivity(for: .chat(chat.id)) {
                UploadStatusBar(activity: activity, cancel: model.cancelUpload, dismiss: model.dismissUploadFailure)
            }
            ChatComposer(conversation: conversation, provider: info.provider, state: state, approval: approvals.first, connected: connected, focusToken: focusToken,
                         send: { Task { await model.sendChatDraft(chat.id) } }, interrupt: interrupt, decide: { decision in if let approval = approvals.first { decide(approval, decision) } },
                         attach: { picking = $0 }, pasteFiles: pasteFiles)
        }
        .background(style.background)
        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height = $0 }
        .attachmentPicker($picking, onDone: requestFocus) { model.attach($0, to: .chat(chat.id)) }
        .task(id: chat.id) { await model.followChat(chat.id) }
        .onAppear { requestFocus() }
        .onChange(of: chat.id) { _, _ in requestFocus() }
        .onChange(of: refocus) { _, _ in requestFocus() }
        .onChange(of: model.keyboard.hardware.isAttached) { _, _ in requestFocus() }
        // A request that needs the person is announced, since a person using VoiceOver is not looking at the bar.
        .onChange(of: approvals.first?.requestID) { _, id in
            if id != nil, let approval = approvals.first { UIAccessibility.post(notification: .announcement, argument: "Approval needed. \(approval.title)") }
        }
        // ⌘. interrupts, wherever the keyboard is. The terminal's hotkey menu uses ⌘. only while it is open on a terminal.
        .background {
            Button("Interrupt") { interrupt() }
                .keyboardShortcut(".", modifiers: .command)
                .disabled(!state.isBusy || !connected)
                .frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
        }
        // ⌘M opens the model picker (⌘M again closes it, from the sheet). The terminal has no ⌘M and its key view is not on this screen.
        .background {
            Button("Choose model") { showModels = true }
                .keyboardShortcut("m", modifiers: .command)
                .disabled(!connected || !conversation.modelChoices(fallback: info).isAvailable)
                .frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
        }
        .sheet(isPresented: $showModels, onDismiss: requestFocus) {
            ChatModelSheet(model: model, chat: info) { showModels = false }.desktopThemed(model.theme.style)
        }
    }

    /// The least the questions may scroll in: a question and an answer, at least.
    private var leastQuestionScroll: CGFloat { style.native ? 60 : 100 }
    /// The least of the transcript that stays on screen above a request or question bar: the last message, at one line.
    private var leastTranscript: CGFloat { style.pt(48) }
    /// In Native, where the bars are taller, a bar over a short screen (the keyboard up) gives up scrolling room until the transcript
    /// keeps `leastTranscript`, and takes it back as the screen grows again. Each new height moves the bar by the difference, so it
    /// settles at once; the terminal look keeps its bars as they were.
    private func transcriptChanged(_ transcript: CGFloat, barShown: Bool) {
        guard style.native, barShown else { if squeeze != 0 { squeeze = 0 }; return }
        let next = min(max(0, squeeze + leastTranscript - transcript), height * 0.3)
        if abs(next - squeeze) >= 1 { squeeze = next }
    }
    private func decide(_ approval: ChatApproval, _ decision: ChatDecision) {
        Task { await model.decideChatApproval(chat.id, approval, decision) }
    }
    private func interrupt() { Task { await model.interruptChat(chat.id) } }
    private func requestFocus() {
        let context = KeyboardFocusContext(hardwareKeyboard: model.keyboard.hardware.isAttached)
        if KeyboardFocusPolicy.decide(setting: model.keyboard.focusSetting, context: context).shouldFocus { focusToken &+= 1 }
    }
    /// A paste of files or a lone picture in the composer: they go to the Mac and their paths into the message.
    private func pasteFiles() -> Bool {
        let sources = PasteboardAttachments.sources()
        guard !sources.isEmpty else { return false }
        model.attach(sources, to: .chat(chat.id))
        return true
    }
}

// MARK: - Toolbar

private struct ChatToolbar: View {
    @Environment(\.desktopStyle) private var style
    let model: RemoteModel
    let chat: ChatInfo
    let conversation: ChatConversation
    let state: ChatState
    @Binding var showModels: Bool

    @State private var choosingModel = false
    @State private var modelName = ""

    private var shownMode: ChatApprovalMode { conversation.pendingMode ?? chat.approvalMode }
    private var choices: ChatModelChoices { conversation.modelChoices(fallback: chat) }
    private func icon(_ mode: ChatApprovalMode) -> String {
        switch mode {
        case .supervised: "hand.raised"
        case .autoEdit: "pencil"
        case .full: "bolt.shield"
        case .plan: "list.bullet.rectangle"
        }
    }
    private var connected: Bool { model.state == .connected }
    private var meter: ChatUsageMeter? { conversation.transcript.usage.map(ChatUsageMeter.init) }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 2) {
                if style.native {
                    // Native: the controls on glass (iOS 26), as the workspace bar has them; the model chip is a capsule of its own
                    // beside the mode menu's, the two in one container as the controls on the right are.
                    NativeGlassGroup(style: style) {
                        modeMenu.nativeGlass(style, in: Capsule())
                        modelChip
                    }
                    // The chip gives way first when the bar is full, as it does in the terminal look.
                    .layoutPriority(-1)
                    Spacer(minLength: 4)
                    NativeGlassGroup(style: style) {
                        compactButton.nativeGlass(style, in: Capsule())
                        optionsMenu.nativeGlass(style, in: Capsule())
                    }
                } else {
                    modeMenu
                    modelChip
                    Spacer(minLength: 4)
                    compactButton
                    optionsMenu
                }
            }
            .buttonStyle(DesktopButtonStyle(compact: true))
            .padding(.horizontal, style.native ? 6 : 4).padding(.vertical, style.glass ? 4 : 0)
            HStack {
                Button {
                    modelName = chat.model ?? ""
                    choosingModel = true
                } label: {
                    Label(chat.model ?? "Provider default model", systemImage: "cpu")
                        .font(style.face(10, relativeTo: .caption2)).lineLimit(1)
                }
                .buttonStyle(.plain).disabled(!connected || state.isBusy || state == .starting)
                .accessibilityLabel("Change model").accessibilityValue(chat.model ?? "Provider default")
                Spacer(minLength: 0)
            }.foregroundStyle(style.muted).padding(.horizontal, 12).padding(.bottom, 4)
            if let meter, let text = meter.text {
                HStack(spacing: 8) {
                    if let fraction = meter.contextFraction { ContextBar(fraction: fraction) }
                    Text(text).font(style.face(10, relativeTo: .caption2)).monospacedDigit().foregroundStyle(style.muted).lineLimit(1).minimumScaleFactor(0.75)
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 12).padding(.bottom, 4)
                .accessibilityElement(children: .ignore).accessibilityLabel("Usage").accessibilityValue(meter.spoken ?? text)
            }
            if !style.glass { DesktopRule() }
        }
        .background(style.glass ? style.surface : style.panel)
        .alert("Chat model", isPresented: $choosingModel) {
            TextField("Model name", text: $modelName).textInputAutocapitalization(.never).autocorrectionDisabled()
            Button("Cancel", role: .cancel) {}
            Button("Apply") {
                Task { await model.setChatModel(chat.id, modelName) }
            }.disabled(modelName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        } message: {
            Text("Enter a model name supported by \(chat.provider.title). It applies to the next message.")
        }
    }

    /// The model, with a bolt when Fast is on. A desktop that sends no list of models has no chip: the toolbar is as it was.
    @ViewBuilder private var modelChip: some View {
        if choices.isAvailable { ChatModelChip(choices: choices, enabled: connected) { showModels = true } }
    }
    private var modeMenu: some View {
        Menu {
            Picker("Approval mode", selection: Binding(get: { shownMode }, set: { mode in Task { await model.setChatMode(chat.id, mode) } })) {
                ForEach(ChatApprovalMode.allCases) { mode in Label("\(mode.title) · \(mode.detail)", systemImage: icon(mode)).tag(mode) }
            }
        } label: {
            HStack(spacing: 5) {
                Image(systemName: icon(shownMode)).accessibilityHidden(true)
                Text(shownMode.title).font(style.face(11, bold: true, relativeTo: .caption))
                Image(systemName: "chevron.up.chevron.down").font(style.system(.caption2)).foregroundStyle(style.muted).accessibilityHidden(true)
            }
            .foregroundStyle(shownMode == .full ? style.gold : style.text).padding(.horizontal, 8)
            .frame(minHeight: style.pt(40)).contentShape(Rectangle())
        }
        .disabled(!connected)
        .accessibilityLabel("Approval mode").accessibilityValue("\(shownMode.title), \(shownMode.detail)")
        .accessibilityHint("Choose Supervised, Auto-edit, Full or Plan")
    }
    private var compactButton: some View {
        Button { Task { await model.compactChat(chat.id) } } label: {
            Label("Compact", systemImage: "arrow.down.right.and.arrow.up.left").labelStyle(.titleAndIcon).font(style.face(11, relativeTo: .caption))
        }
        .disabled(!connected || state.isBusy || state == .starting)
        .accessibilityHint("Summarizes the conversation to free up context")
    }
    private var optionsMenu: some View {
        Menu {
            Button("Change model", systemImage: "cpu") {
                modelName = chat.model ?? ""
                choosingModel = true
            }
            .disabled(!connected || state.isBusy || state == .starting)
            Button("Jump to latest", systemImage: "arrow.down.to.line") { conversation.jumpToEnd() }
            Button("Stop agent", systemImage: "stop.circle", role: .destructive) { Task { await model.stopChat(chat.id) } }
                .disabled(!connected || state == .stopped)
            if let id = chat.providerThreadID { Button("Copy session id", systemImage: "doc.on.doc") { UIPasteboard.general.string = id } }
        } label: { Label("Chat options", systemImage: "ellipsis") }
            .labelStyle(.iconOnly).frame(minWidth: style.pt(40), minHeight: style.pt(40))
    }
}

/// How full the context is: a thin bar that turns gold above 80 % and red above 95 %.
private struct ContextBar: View {
    @Environment(\.desktopStyle) private var style
    let fraction: Double
    var body: some View {
        Capsule().fill(style.divider).frame(width: style.pt(56), height: 4)
            .overlay(alignment: .leading) {
                Capsule().fill(fraction > 0.95 ? style.error : (fraction > 0.8 ? style.gold : style.accent)).frame(width: max(2, style.pt(56) * fraction), height: 4)
            }
            .accessibilityHidden(true)

    }
}

// MARK: - What the chat is doing

/// The lines between the toolbar and the transcript that say something is not simply fine: no link, Starting, Stopped, Failed (with
/// Retry), a chat that is gone, a transcript that cannot be read.
private struct ChatStatusLines: View {
    @Environment(\.desktopStyle) private var style
    let model: RemoteModel
    let chat: ChatInfo
    let conversation: ChatConversation
    let state: ChatState

    var body: some View {
        let banner = ChatBanner(state: state, provider: chat.provider, lastMessage: conversation.transcript.lastUserMessage)
        VStack(spacing: 0) {
            if conversation.gone {
                line(icon: "questionmark.folder", text: "This chat is gone from the Mac.", tint: style.warning) {
                    Button("Back") { model.deselectChat() }.buttonStyle(DesktopButtonStyle(compact: true)).nativeGlass(style, in: Capsule())
                }
            } else if model.state != .connected {
                line(icon: "wifi.slash", text: model.state == .connecting ? "Connecting…" : "Not connected. Your chat is kept; it carries on when the link is back.", tint: style.warning) {
                    if model.state != .connecting { Button("Reconnect") { Task { await model.connect() } }.buttonStyle(DesktopButtonStyle(compact: true)).nativeGlass(style, in: Capsule()) }
                }
            } else if conversation.readError != nil {
                line(icon: "arrow.triangle.2.circlepath", text: "Can’t read this chat right now. Trying again…", tint: style.warning) { EmptyView() }
            }
            if let banner {
                line(icon: icon(banner), text: banner.text, tint: tint(banner), working: { if case .starting = banner { true } else { false } }()) {
                    if case .failed(_, let retry?) = banner {
                        Button("Retry") { Task { await model.sendChatMessage(chat.id, retry) } }.buttonStyle(DesktopButtonStyle(compact: true)).nativeGlass(style, in: Capsule())
                            .disabled(model.state != .connected || conversation.sending)
                            .accessibilityHint("Sends your last message again")
                    }
                }
            }
        }
    }

    private func icon(_ banner: ChatBanner) -> String {
        switch banner {
        case .starting: "hourglass"
        case .stopped: "pause.circle"
        case .failed: "exclamationmark.triangle.fill"
        }
    }
    private func tint(_ banner: ChatBanner) -> Color {
        if case .failed = banner { return style.error }
        return style.muted
    }

    private func line<Trailing: View>(icon: String, text: String, tint: Color, working: Bool = false, @ViewBuilder trailing: () -> Trailing) -> some View {
        HStack(alignment: .center, spacing: 8) {
            if working { ActivityIndicator(activity: .working) } else { Image(systemName: icon).foregroundStyle(tint).accessibilityHidden(true) }
            Text(text).font(style.system(.footnote)).foregroundStyle(tint == style.muted ? style.muted : style.text).fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 4)
            trailing()
        }
        .padding(.horizontal, 12).padding(.vertical, 6).frame(maxWidth: .infinity, alignment: .leading)
        .modifier(StatusLineSurface(tint: tint))
        .accessibilityElement(children: .combine)
    }
}

/// What a status line sits on. The terminal look: a band in its signal color with a rule under it. Native: a rounded tinted panel set
/// in from the edges, as the request bars and the upload line are, and a muted line (Starting, Stopped) on nothing at all.
private struct StatusLineSurface: ViewModifier {
    @Environment(\.desktopStyle) private var style
    let tint: Color
    func body(content: Content) -> some View {
        let quiet = tint == style.muted
        if style.native {
            content.background(tint.opacity(quiet ? 0 : 0.12), in: style.block(12)).padding(.horizontal, 8).padding(.top, quiet ? 0 : 6)
        } else {
            content.background(tint.opacity(quiet ? 0 : 0.10)).overlay(alignment: .bottom) { DesktopRule() }
        }
    }
}

// MARK: - Transcript

/// The transcript, newest at the bottom. It follows the bottom while the chat streams and the reader is there; scrolled up, it stays
/// where it is and a pill takes the reader back (`StickyBottom`, the rule the terminal follows too).
private struct ChatTranscriptList: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let conversation: ChatConversation
    let provider: ChatProvider
    let state: ChatState
    let hardwareKeyboard: Bool
    @State private var position = ScrollPosition(edge: .bottom)
    @State private var sticky = StickyBottom()
    @State private var userDriven = false

    var body: some View {
        let transcript = conversation.transcript
        ScrollViewReader { proxy in
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                if !conversation.feed.loaded {
                    placeholder("Loading the conversation…", spinner: true)
                } else if transcript.items.isEmpty {
                    placeholder("Ask \(provider.title) to work in this project.", spinner: false)
                }
                ForEach(transcript.items) { item in
                    ChatItemRow(item: item, provider: provider, open: open(for: item), toggle: toggle).equatable()
                }
                if state == .running || state == .waiting || state == .starting { workingRow }
                Color.clear.frame(height: 6).id(Self.end)
            }
            .padding(.top, 4)
        }
        .scrollPosition($position)
        .defaultScrollAnchor(.bottom)
        // Dragging the list puts the software keyboard away, as in a chat; a hardware keyboard has none to put away, and must not lose
        // the composer to a scroll.
        .scrollDismissesKeyboard(hardwareKeyboard ? .never : .interactively)
        .onScrollPhaseChange { _, phase in userDriven = phase == .interacting || phase == .decelerating }
        .onScrollGeometryChange(for: ScrollMetrics.self) { geometry in
            ScrollMetrics(offset: geometry.contentOffset.y, contentHeight: geometry.contentSize.height, viewportHeight: geometry.containerSize.height,
                          topInset: geometry.contentInsets.top, bottomInset: geometry.contentInsets.bottom)
        } action: { old, new in
            // The bottom moved away from a view that follows it (a bar came up over the transcript, the transcript grew). The position
            // already says "bottom", so setting it again does nothing: the reader scrolls there itself.
            if sticky.metricsChanged(from: old, to: new, lineHeight: 24, userDriven: userDriven) == .scrollToBottom { proxy.scrollTo(Self.end, anchor: .bottom) }
        }
        .onChange(of: transcript.items.count) { _, count in sticky.contentChanged(end: count, epoch: 0) }
        .onChange(of: conversation.jumps) { _, _ in jump() }
        .onChange(of: conversation.feed.loaded) { _, _ in jump() }
        .overlay(alignment: .bottomTrailing) { pill }
        .accessibilityLabel("\(provider.chatTitle) conversation")
        }
    }
    private static let end = "chat-transcript-end"

    private func open(for item: ChatItem) -> Set<String> {
        guard !conversation.expanded.isEmpty else { return [] }
        return conversation.expanded.filter { $0 == item.id || $0.hasPrefix(item.id + "#") }
    }
    private var toggle: @MainActor (String) -> Void {
        { [conversation, reduceMotion] key in
            withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.15)) {
                if conversation.expanded.contains(key) { conversation.expanded.remove(key) } else { conversation.expanded.insert(key) }
            }
        }
    }
    private func jump() {
        sticky.jumpToBottom()
        withAnimation(reduceMotion ? nil : .easeOut(duration: 0.2)) { position.scrollTo(edge: .bottom) }
    }

    private func placeholder(_ text: String, spinner: Bool) -> some View {
        HStack(spacing: 8) {
            if spinner { ProgressView().controlSize(.small) }
            Text(text).font(style.system(.footnote)).foregroundStyle(style.muted)
        }
        .padding(16).frame(maxWidth: .infinity, alignment: .center)
    }
    private var workingRow: some View {
        HStack(spacing: 8) {
            ActivityIndicator(activity: state == .waiting ? .waiting : .working)
            Text(state == .waiting ? "Waiting for you" : (state == .starting ? "Starting…" : "Working…")).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .accessibilityElement(children: .combine).accessibilityLabel(state == .waiting ? "Waiting for you" : "Working")
    }
    @ViewBuilder private var pill: some View {
        if let pill = sticky.pill {
            Button(action: jump) {
                if style.native {
                    // Native: the arrow is a symbol, and the pill is glass on iOS 26, material before.
                    Label(pill.newLines > 0 ? "Latest · \(pill.newLines) new" : "Latest", systemImage: "arrow.down").labelStyle(.titleAndIcon)
                        .font(style.face(11, bold: true, relativeTo: .caption)).foregroundStyle(style.accent)
                        .padding(.horizontal, 12).frame(minHeight: style.pt(34))
                        .background { if !style.glass { Capsule().fill(.ultraThinMaterial) } }
                        .nativeGlass(style, in: Capsule()).contentShape(Capsule())
                } else {
                    Text(pill.newLines > 0 ? "↓ Latest · \(pill.newLines) new" : "↓ Latest").font(style.mono(11, bold: true, relativeTo: .caption)).foregroundStyle(style.accent)
                        .padding(.horizontal, 12).frame(minHeight: style.pt(30)).background(.ultraThinMaterial, in: Capsule())
                        .overlay(Capsule().stroke(style.divider, lineWidth: 1)).contentShape(Capsule())
                }
            }
            .buttonStyle(.plain).padding(.trailing, 10).padding(.bottom, 8).transition(.opacity)
            .accessibilityLabel(pill.newLines > 0 ? "Jump to latest, \(pill.newLines) new" : "Jump to latest")
        }
    }
}
