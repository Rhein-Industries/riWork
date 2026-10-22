import SwiftUI
import UIKit
import RiWorkCore

/// A chat, full screen under the tab strip: a compact toolbar (model, context ring, mode), what the chat is doing when it is not simply
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
    /// `/memo`'s **Edit** was pressed: the memo editor of this project comes up once the preview has gone.
    @State private var editMemoAfterPreview: String?
    /// The history of the provider's notices is up.
    @State private var showNotices = false
    /// Counts requests from the tab row's ⋯ menu to show the notices.
    var openNotices = 0
    /// The height the screen has now. With the software keyboard up it is under half of the phone, and the bars above the composer must
    /// leave the transcript room, so what they may scroll is a share of it.
    @State private var height: CGFloat = 800
    /// The screen's height to the window's bottom, whatever the keyboard does, measured as the window is resized or turned: what inline
    /// images are sized by (`chatScreenHeight`).
    @State private var screenHeight: CGFloat = 800
    /// The photo or file picker of the composer's paperclip.
    @State private var picking: AttachmentChoice?
    /// Scrolling room the bar over the composer gives up so the transcript keeps its last message in view (`transcriptChanged`).
    @State private var squeeze: CGFloat = 0
    /// The tops of the transcript and of what floats over its bottom (the bars and the composer), in the window: what is seen of the
    /// transcript is between them.
    @State private var transcriptTop: CGFloat = 0
    @State private var bottomTop: CGFloat = 0
    @State private var squeezeCheck: Task<Void, Never>?
    @State private var viewingFile: ChatFileEntry?

    private var conversation: ChatConversation { model.chatConversations[chat.id] ?? model.conversation(chat.id) }
    private var state: ChatState { model.chatState(chat) }
    private var info: ChatInfo { conversation.transcript.info ?? chat }
    private var connected: Bool { model.state == .connected }
    /// What can say how this chat's workers are doing (`RemoteModel.workerInventory`). The lists are read again every few seconds while
    /// the project's screen is up (`keepFresh(.sessions)`, which this chat is part of). A chat with no worker card does not read them,
    /// so a change to them does not redraw it.
    private var workerInventory: WorkerInventory {
        guard conversation.transcript.items.contains(where: { $0.worker != nil }) else { return .unavailable }
        return model.workerInventory(of: info)
    }

    var body: some View {
        let conversation = conversation
        let approvals = conversation.openApprovals
        let questions = conversation.openQuestions
        let elidedRequests = conversation.transcript.elidedRequests
        let barShown = !approvals.isEmpty || !questions.isEmpty || !elidedRequests.isEmpty
        let fileIndex = conversation.fileCatalogCache.index(items: conversation.transcript.items,
                                                           revision: conversation.fileCatalogRevision, images: model.chatAttachmentImages)
        let files = fileIndex.entries
        VStack(spacing: 0) {
            ChatToolbar(model: model, chat: info, conversation: conversation, state: state, showModels: $showModels)
            ChatTranscriptList(conversation: conversation, provider: info.provider, state: state, workers: workerInventory, dismissesKeyboard: !model.keyboard.hardware.isAttached || model.keyboard.software.isShown, loadOlder: { beforeInstall in await model.loadOlderChat(chat.id, beforeInstall: beforeInstall) })
                .id(chat.id)
                .onGeometryChange(for: CGFloat.self) { $0.frame(in: .global).minY } action: { transcriptTop = $0; transcriptChanged(barShown: barShown) }
                // The composer floats over the transcript: the transcript scrolls on underneath it, seen through its glass field and
                // around it, and is inset by the height of all that stands over it, so its last message still ends above the field. The
                // bars and banner lines above the composer stand on the screen's background, as before.
                .safeAreaInset(edge: .bottom, spacing: 0) {
                    VStack(spacing: 0) {
                        VStack(spacing: 0) {
                            if let approval = approvals.first {
                                BoundedScroll(maxHeight: max(110, height * 0.52 - squeeze)) {
                                    ChatApprovalBar(approval: approval, count: approvals.count, keyHints: model.keyboard.hardware.isAttached, detailHeight: max(70, height * 0.2 - squeeze),
                                                    busy: !connected || conversation.catchingUp || conversation.answered.contains(approval.requestID)) { decision in decide(approval, decision) }
                                        .id(approval.requestID)
                                }
                            } else if let question = questions.first {
                                ChatQuestionBar(question: question, scrollHeight: max(leastQuestionScroll, height * 0.3 - squeeze), busy: !connected || conversation.catchingUp || conversation.answered.contains(question.requestID)) { form in
                                    Task { await model.answerChatQuestion(chat.id, form) }
                                }
                                .id(question.requestID)
                            } else if let request = elidedRequests.first {
                                ChatElidedRequestBar(request: request, count: elidedRequests.count).id(request.requestID)
                            }
                            // Every message of the moment, in one place: the link, the chat's state, what went wrong, the provider's notices.
                            ChatNoticeBanners(model: model, chat: info, conversation: conversation, state: state, showHistory: { showNotices = true })
                        }
                        .background(style.background)
                        if let choice = conversation.contributorState.choice(fallback: info) {
                            ChatContributorButton(choice: choice, connected: connected, changing: conversation.changingContributor) {
                                Task { await model.setChatContributor(chat.id, choice.nextMode) }
                            }
                        }
                        ChatComposer(conversation: conversation, provider: info.provider, state: state, approval: approvals.first, connected: connected && !conversation.catchingUp, focusToken: focusToken,
                                     send: send, interrupt: interrupt, decide: { decision in if let approval = conversation.openApprovals.first { decide(approval, decision) } },
                                     attach: { picking = $0 }, pasteFiles: pasteFiles,
                                     attachmentImages: model.chatAttachmentImages, removeAttachment: { model.removeStagedAttachment($0, from: chat.id) },
                                     pending: model.pendingAttachments(for: chat.id), cancelPending: model.cancelUpload,
                                     barAbove: barShown, suggestionCatalog: model.composerCatalog(for: chat.id), wantSuggestions: { model.wantComposerSuggestions(chat.id) },
                                     // The `&` list marks the model the picker shows as chosen.
                                     chosenModel: conversation.modelChoices(fallback: info).current?.id ?? conversation.modelChoices(fallback: info).modelID,
                                     usesClicks: model.keyboard.usesClicks)
                    }
                    .onGeometryChange(for: CGFloat.self) { $0.frame(in: .global).minY } action: { bottomTop = $0; transcriptChanged(barShown: barShown) }
                }
        }
        .frame(maxWidth: 760)
        .frame(maxWidth: .infinity)
        .background(style.background)
        // SwiftUI makes room for any keyboard it sees, a floating one or a hardware keyboard's shortcut bar too: the screen keeps to the
        // window's bottom unless a software keyboard is docked (`SoftwareKeyboardMonitor.isDocked`). Docking takes SwiftUI's own keyboard
        // motion; going back to the bottom (floating, split, undocked, hidden) eases over the keyboard's time.
        // Measured beside the content and out of the keyboard's way, so a docked keyboard does not change it and a resize always does.
        .background {
            Color.clear.ignoresSafeArea(.keyboard, edges: .bottom)
                .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { screenHeight = $0 }
        }
        .ignoresSafeArea(.keyboard, edges: keyboardDocked ? [] : .bottom)
        .animation(keyboardDocked ? nil : .easeOut(duration: 0.25), value: keyboardDocked)
        .environment(\.chatFileEntries, files)
        .environment(\.chatScreenHeight, screenHeight)
        .environment(\.chatFileIndex, fileIndex)
        .environment(\.chatOpenFile, { file in viewingFile = file })
        .navigationDestination(item: $viewingFile) { file in
            ChatFileViewer(files: files.filter { $0.turnID == file.turnID }, initialID: file.id, initialLine: file.line)
                .desktopThemed(model.theme.style)
                .toolbar(.hidden, for: .navigationBar)
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height = $0; squeeze = 0 }
        .attachmentPicker($picking, onDone: requestFocus) { model.attach($0, to: .chat(chat.id)) }
        .chatDropTarget(model: model, chat: chat)
        .task(id: chat.id) { await model.followChat(chat.id) }
        .onAppear { requestFocus() }
        .environment(\.chatAssetLoader, { [model, chatID = chat.id] item, reference in
            try await model.loadChatAsset(chatID: chatID, itemID: item, reference: reference)
        })
        .onChange(of: model.appActive) { _, _ in conversation.markCatchingUp() }
        .onChange(of: model.state) { _, _ in conversation.markCatchingUp() }
        .onChange(of: chat.id) { _, _ in requestFocus() }
        .onChange(of: refocus) { _, _ in requestFocus() }
        .onChange(of: openNotices) { _, _ in showNotices = true }
        .onChange(of: model.keyboard.modelPickerRequests) { _, _ in
            if connected, model.selectedChatID == chat.id { showModels = true }
        }
        .sheet(isPresented: $showNotices, onDismiss: requestFocus) {
            ChatNoticeHistory(notices: ChatNotices.all(conversation.transcript.items)) { showNotices = false }.desktopThemed(model.theme.style)
        }
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
                .disabled(!connected)
                .frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
        }
        .sheet(isPresented: $showModels, onDismiss: requestFocus) {
            ChatModelSheet(model: model, chat: info) { showModels = false }.desktopThemed(model.theme.style)
        }
        .sheet(item: Binding(get: { model.memoPreview?.chatID == chat.id ? model.memoPreview : nil }, set: { if $0 == nil { model.memoPreview = nil } }),
               onDismiss: memoPreviewGone) { shown in
            MemoPreviewSheet(preview: shown.preview, insert: { model.insertMemoPreview() }, edit: {
                editMemoAfterPreview = shown.preview.projectID
                model.memoPreview = nil
            }, close: { model.memoPreview = nil })
            .desktopThemed(model.theme.style)
        }
    }

    /// Return or Send: `/memo` puts the project's memo up instead of sending (`RemoteModel.openMemoPreview`); anything else is sent.
    private func send() {
        if model.openMemoPreview(chat: info) { return }
        Task { await model.sendChatDraft(chat.id) }
    }
    private func memoPreviewGone() {
        if let project = editMemoAfterPreview {
            editMemoAfterPreview = nil
            model.openMemoEditor(projectID: project, name: model.projectName(project))
        } else {
            requestFocus()
        }
    }

    /// The least the questions may scroll in: a question and an answer, at least.
    private var leastQuestionScroll: CGFloat { style.native ? 60 : 100 }
    /// A software keyboard is docked: only then does the screen make room for it, and the composer stands on it.
    private var keyboardDocked: Bool { model.keyboard.software.isDocked }
    /// The least of the transcript that stays on screen above a request or question bar: the last message, at one line.
    private var leastTranscript: CGFloat { style.pt(48) }
    /// In Native, where the bars are taller, a bar over a short screen (the keyboard up) gives up scrolling room until the transcript
    /// keeps `leastTranscript`; the terminal look keeps its bars as they were. It only ever gives more up, and starts over when the
    /// screen's height changes: a bar cuts its answers between rows, so its height moves in steps, and taking room back as the
    /// transcript grows would swing between two steps without end. It goes by the height once settled: the bars and the composer float
    /// over the transcript, and while they come up their inset changes in steps, so what is seen dips for a moment.
    private func transcriptChanged(barShown: Bool) {
        guard style.native, barShown else { if squeeze != 0 { squeeze = 0 }; squeezeCheck?.cancel(); squeezeCheck = nil; return }
        guard squeezeCheck == nil else { return }
        squeezeCheck = Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(120))
            squeezeCheck = nil
            let seen = bottomTop - transcriptTop
            guard !Task.isCancelled, seen < leastTranscript - 0.5 else { return }
            let next = min(squeeze + leastTranscript - seen, height * 0.3)
            if next - squeeze >= 1 { squeeze = next }
        }
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

/// The chat's own row under the tab row: the model, how full the context is, and the approval mode, each a tap away. The chat's
/// actions (Compact, Stop agent, Copy session id, …) are in the tab row's one ⋯ menu (`ChatMenuSection`), so the screen has a single ⋯.
private struct ChatToolbar: View {
    @Environment(\.desktopStyle) private var style
    let model: RemoteModel
    let chat: ChatInfo
    let conversation: ChatConversation
    let state: ChatState
    @Binding var showModels: Bool

    private var shownMode: ChatApprovalMode { conversation.pendingMode ?? chat.approvalMode }
    private var choices: ChatModelChoices { conversation.modelChoices(fallback: chat) }
    private var connected: Bool { model.state == .connected }
    private var meter: ChatUsageMeter? { conversation.transcript.usage.map(ChatUsageMeter.init).flatMap { $0.tokensText == nil ? nil : $0 } }

    var body: some View {
        // One line, always: the mode gives up its word first, then the model's name is cut (`ChatModelChip` gives way first of all);
        // the ring and the mode menu are never cut or moved to a second line.
        ViewThatFits(in: .horizontal) {
            row(modeTitle: true)
            row(modeTitle: false)
        }
        .chatToolbarSurface()
        .chatLayoutProbe("chat-toolbar")
    }
    private func row(modeTitle: Bool) -> some View {
        HStack(spacing: 2) {
            modelButton
            ring
            Spacer(minLength: 4)
            ChatActivityPicker(selection: Binding(get: { conversation.displayMode }, set: { conversation.setDisplayMode($0) }))
            .frame(width: 132, height: 28)
            .accessibilityIdentifier("chat-display-mode")
            .chatLayoutProbe("display-mode")
            .chatLayoutProbe("display-normal", action: { conversation.setDisplayMode(.normal) })
            .chatLayoutProbe("display-verbose", action: { conversation.setDisplayMode(.verbose) })
            modeButton(title: modeTitle)
        }
    }

    /// The context ring; with no context known yet but usage windows in, only the anchor for the detail a usage limit's banner opens.
    @ViewBuilder private var ring: some View {
        if meter != nil || !conversation.transcript.rateLimits.isEmpty {
            ChatUsageRing(meter: meter, windows: conversation.transcript.rateLimits, requests: conversation.usageDetailRequests)
        }
    }
    private var modelButton: some View {
        ChatModelChip(choices: choices, enabled: connected, compact: true) { showModels = true }
            .chatLayoutProbe("model")
    }
    private func modeButton(title: Bool) -> some View {
        Menu {
            Picker("Approval mode", selection: Binding(get: { shownMode }, set: { mode in Task { await model.setChatMode(chat.id, mode) } })) {
                ForEach(ChatApprovalMode.allCases) { mode in Label("\(mode.title) · \(mode.detail)", systemImage: mode.icon).tag(mode) }
            }
        } label: {
            HStack(spacing: 5) {
                Image(systemName: shownMode.icon).accessibilityHidden(true)
                if title { Text(shownMode.title).font(style.system(.footnote, weight: .medium)).lineLimit(1).fixedSize() }
                Image(systemName: "chevron.down").font(style.system(.caption2)).accessibilityHidden(true)
            }
            .foregroundStyle(shownMode == .full ? style.gold : style.muted)
            .padding(.horizontal, 6).frame(minWidth: style.target, minHeight: style.target).contentShape(Rectangle())
        }
        .buttonStyle(.plain).disabled(!connected)
        .accessibilityIdentifier("chat-permission-mode")
        .chatLayoutProbe("permissions")
        .accessibilityLabel("Approval mode").accessibilityValue("\(shownMode.title), \(shownMode.detail)")
        .accessibilityHint("Choose Supervised, Auto-edit, Full or Plan")
    }
}

/// iOS's SwiftUI segmented Picker keeps its 32pt backing control at mini/small
/// sizes. Size the native control itself so its labels and capsule fit 28pt.
private struct ChatActivityPicker: UIViewRepresentable {
    @Binding var selection: ChatDisplayMode

    func makeUIView(context: Context) -> UISegmentedControl {
        let control = UISegmentedControl(items: ["Normal", "Verbose"])
        control.accessibilityLabel = "Activity display"
        control.accessibilityIdentifier = "chat-display-mode"
        control.addTarget(context.coordinator, action: #selector(Coordinator.changed(_:)), for: .valueChanged)
        return control
    }
    func updateUIView(_ control: UISegmentedControl, context: Context) {
        context.coordinator.selection = $selection
        let index = selection == .normal ? 0 : 1
        if control.selectedSegmentIndex != index { control.selectedSegmentIndex = index }
        let font = UIFont.preferredFont(forTextStyle: .footnote, compatibleWith: control.traitCollection)
        if (control.titleTextAttributes(for: .normal)?[.font] as? UIFont) != font {
            for state in [UIControl.State.normal, .selected] { control.setTitleTextAttributes([.font: font], for: state) }
        }
        let scheme: UIUserInterfaceStyle = context.environment.colorScheme == .dark ? .dark : .light
        if control.overrideUserInterfaceStyle != scheme { control.overrideUserInterfaceStyle = scheme }
    }
    func sizeThatFits(_ proposal: ProposedViewSize, uiView: UISegmentedControl, context: Context) -> CGSize? {
        CGSize(width: proposal.width ?? 132, height: 28)
    }
    func makeCoordinator() -> Coordinator { Coordinator(selection: $selection) }
    final class Coordinator: NSObject {
        var selection: Binding<ChatDisplayMode>
        init(selection: Binding<ChatDisplayMode>) { self.selection = selection }
        @objc func changed(_ sender: UISegmentedControl) { selection.wrappedValue = sender.selectedSegmentIndex == 0 ? .normal : .verbose }
    }
}

extension ChatApprovalMode {
    var icon: String {
        switch self {
        case .supervised: "hand.raised"
        case .autoEdit: "pencil"
        case .full: "bolt.shield"
        case .plan: "list.bullet.rectangle"
        }
    }
}

/// How full the chat's context is: a ring that fills with the share used, the percent beside it. A tap (or a long press, or a usage
/// limit's banner) shows the tokens used of the window, Claude's cost estimate and the provider's usage windows (5-hour, weekly, …).
/// It follows the chat's usage as it comes in. Without a context figure it draws nothing and is only that detail's anchor.
struct ChatUsageRing: View {
    @Environment(\.desktopStyle) private var style
    let meter: ChatUsageMeter?
    var windows: [ChatRateWindow] = []
    var requests = 0
    @State private var detail = false
    /// As the Mac's ring (`ChatUsageMeter.level`): the text color, the warning tint from 70 %, red from 90 %.
    private var tint: Color {
        switch meter?.level ?? .normal {
        case .normal: style.text
        case .warning: style.warning
        case .critical: style.error
        }
    }
    var body: some View {
        Button { detail = true } label: {
            if let meter {
                // About 6 pt between the ring and the number: half the ring's stroke lies outside its frame.
                HStack(spacing: style.pt(6) + 1.25) {
                    ZStack {
                        Circle().stroke(style.divider, lineWidth: 2.5)
                        Circle().trim(from: 0, to: meter.contextFraction ?? 0).stroke(tint, style: StrokeStyle(lineWidth: 2.5, lineCap: .round)).rotationEffect(.degrees(-90))
                    }
                    .frame(width: style.pt(15), height: style.pt(15))
                    .animation(.easeOut(duration: 0.3), value: meter.contextFraction)
                    .chatLayoutProbe("usage-ring")
                    Text(meter.percentText ?? meter.tokensText?.replacingOccurrences(of: " tokens", with: "") ?? "")
                        .font(style.system(.footnote, weight: .medium)).monospacedDigit().foregroundStyle(style.muted).lineLimit(1).fixedSize()
                        .chatLayoutProbe("usage-number")
                }
                .padding(.horizontal, 6).frame(minWidth: style.target, minHeight: style.target).contentShape(Rectangle())
            } else {
                Color.clear.frame(width: 1, height: 1)
            }
        }
        .buttonStyle(.plain)
        .disabled(meter == nil)
        .fixedSize()
        .simultaneousGesture(LongPressGesture(minimumDuration: 0.35).onEnded { _ in if meter != nil { detail = true } })
        .onChange(of: requests) { _, _ in detail = true }
        .popover(isPresented: $detail) {
            VStack(alignment: .leading, spacing: 4) {
                if let meter {
                    Text("Context").font(style.system(.caption, weight: .semibold)).foregroundStyle(style.muted)
                    Text([meter.percentText, meter.tokensText].compactMap { $0 }.joined(separator: " · ")).font(style.system(.subheadline, weight: .semibold)).monospacedDigit().foregroundStyle(style.text)
                    if let cost = meter.costText { Text(cost).font(style.system(.footnote)).foregroundStyle(style.muted) }
                }
                let live = ChatUsageLimits.live(windows)
                if !windows.isEmpty {
                    Text("Usage limits").font(style.system(.caption, weight: .semibold)).foregroundStyle(style.muted).padding(.top, meter == nil ? 0 : 8)
                    Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 6) {
                        ForEach(live) { window in
                            GridRow {
                                Text(window.label).font(style.system(.subheadline)).foregroundStyle(style.text)
                                Text(window.percentText).font(style.system(.subheadline, weight: window.usedPercent >= 90 ? .bold : .semibold)).monospacedDigit()
                                    .foregroundStyle(window.usedPercent >= window.warnAt ? style.gold : style.text)
                                    .gridColumnAlignment(.trailing)
                                Text(window.resetsAt.map { "resets " + ChatUsageLimits.resetTime($0) } ?? "").font(style.system(.footnote)).foregroundStyle(style.muted)
                            }
                            .accessibilityElement(children: .combine)
                        }
                    }
                    if live.isEmpty { Text("No usage windows known.").font(style.system(.footnote)).foregroundStyle(style.muted) }
                }
            }
            .padding(14).fixedSize()
            .presentationCompactAdaptation(.popover)
            .accessibilityElement(children: windows.isEmpty ? .combine : .contain)
        }
        .chatLayoutProbe("usage", visible: meter != nil, action: { detail = true })
        .accessibilityIdentifier("chat-usage")
        .accessibilityLabel("Context usage").accessibilityValue(meter?.spoken ?? "")
        .accessibilityHint(windows.isEmpty ? "Shows the tokens used and the cost estimate" : "Shows the tokens used, the cost estimate and the usage windows")
        .accessibilityHidden(meter == nil)
    }
}

/// The chat's part of the tab row's ⋯ menu, first in it while a chat is on screen. The model and the context/usage are not in it: they
/// are the row under the tab row (`ChatToolbar`).
struct ChatMenuSection: View {
    let model: RemoteModel
    let chat: ChatInfo
    var showNotices: () -> Void = {}
    var body: some View {
        let conversation = model.chatConversations[chat.id] ?? model.conversation(chat.id)
        let info = conversation.transcript.info ?? chat
        let state = model.chatState(info)
        let connected = model.state == .connected
        Section("Chat") {
            Button("Expand all activity", systemImage: "arrow.up.left.and.arrow.down.right") { conversation.discloseAll(true) }
            Button("Collapse all activity", systemImage: "arrow.down.right.and.arrow.up.left") { conversation.discloseAll(false) }
            Button("Compact context", systemImage: "arrow.down.right.and.arrow.up.left") { Task { await model.compactChat(chat.id) } }
                .disabled(!connected || state.isBusy || state == .starting)
            Button("Jump to latest", systemImage: "arrow.down.to.line") { conversation.jumpToEnd() }
            let notices = ChatNotices.all(conversation.transcript.items).count
            if notices > 0 { Button("Notices (\(notices))…", systemImage: "bell") { showNotices() } }
            if let id = info.providerThreadID { Button("Copy session id", systemImage: "doc.on.doc") { UIPasteboard.general.string = id } }
            Button("Stop agent", systemImage: "stop.circle", role: .destructive) { Task { await model.stopChat(chat.id) } }
                .disabled(!connected || state == .stopped)
        }
    }
}

// MARK: - Messages of the moment

/// The one place a chat says something for a moment, directly above the composer: no link (Reconnect), a chat gone from the Mac
/// (Back), Starting, Stopped, Failed (Retry), a transcript that cannot be read, what went wrong with the last command, what an older
/// desktop cannot do, and the provider's notices (rate and usage limits, retries, warnings: the latest of each kind in this turn).
/// Each line has ×; each goes by itself when its cause is resolved, and a recurring one updates its line instead of adding one. At most
/// two lines show; the rest are a tap away, and every provider notice of the chat is in the history.
struct ChatNoticeBanners: View {
    @Environment(\.desktopStyle) private var style
    let model: RemoteModel
    let chat: ChatInfo
    let conversation: ChatConversation
    let state: ChatState
    let showHistory: () -> Void
    /// The link or gone line, and the Starting/Stopped/Failed line, the person closed: each stays closed until it says something else.
    @State private var closedLink: String?
    @State private var closedState: String?
    @State private var expanded = false

    struct Line: Identifiable {
        let id: String
        let level: ChatNoticeLevel
        let icon: String
        let text: String
        var repeats = 1
        var working = false
        var action: (title: String, hint: String, enabled: Bool, run: () -> Void)?
        let close: () -> Void
    }

    private var lines: [Line] {
        var lines: [Line] = []
        let connected = model.state == .connected
        // The chat's state and the link: what it is now, so they go when it changes.
        if conversation.gone {
            let text = "This chat is gone from the Mac."
            if closedLink != text { lines.append(Line(id: "status", level: .warning, icon: "questionmark.folder", text: text, action: ("Back", "Back to the terminals", true, { model.deselectChat() }), close: { closedLink = text })) }
        } else if model.state != .connected {
            let text = model.state == .connecting ? "Connecting…" : "Not connected. Your chat is kept; it carries on when the link is back."
            if closedLink != text {
                lines.append(Line(id: "status", level: .warning, icon: "wifi.slash", text: text, working: model.state == .connecting,
                                  action: model.state == .connecting ? nil : ("Reconnect", "Connects to the Mac again", true, { Task { await model.connect() } }), close: { closedLink = text }))
            }
        } else if let error = conversation.readError, conversation.dismissedReadError != error.message {
            lines.append(Line(id: "read", level: .warning, icon: "arrow.triangle.2.circlepath", text: error.message, close: { conversation.dismissedReadError = error.message }))
        }
        if connected, conversation.catchingUp, conversation.readError == nil, !conversation.gone {
            lines.append(Line(id: "catch-up", level: .info, icon: "arrow.triangle.2.circlepath", text: "Updating chat…", working: true, close: {}))
        }
        if let banner = ChatBanner(state: state, provider: chat.provider, lastMessage: conversation.transcript.lastUserMessage), closedState != banner.text {
            switch banner {
            case .starting: lines.append(Line(id: "state", level: .info, icon: "hourglass", text: banner.text, working: true, close: { closedState = banner.text }))
            case .stopped: lines.append(Line(id: "state", level: .info, icon: "pause.circle", text: banner.text, close: { closedState = banner.text }))
            case .failed(_, let retry):
                lines.append(Line(id: "state", level: .error, icon: "exclamationmark.triangle.fill", text: banner.text,
                                  action: retry.map { retry in ("Retry", "Sends your last message again", connected && !conversation.sending, { Task { await model.sendChatMessage(chat.id, retry) } }) },
                                  close: { closedState = banner.text }))
            }
        }
        // A file that did not reach the Mac, and a dictation that failed (with Open Settings when that is the way out).
        if let activity = model.uploadActivity(for: .chat(chat.id)), case .failed(let message) = activity.phase {
            lines.append(Line(id: "upload", level: .warning, icon: "exclamationmark.triangle.fill", text: message, close: { model.dismissUploadFailure() }))
        }
        if let line = Self.dictationLine(.chat(chat.id)) { lines.append(line) }
        // What the phone itself has to say: one line per source, replaced in place.
        for alert in conversation.alerts.ordered {
            lines.append(Line(id: "alert-\(alert.source.rawValue)", level: alert.level, icon: Self.icon(alert.level), text: alert.text, repeats: alert.repeats,
                              close: { conversation.alerts.clear(alert.source) }))
        }
        // The provider's notices of this turn, the latest of each kind.
        // Sticky ones (a reached usage limit, a sign-in) from any turn; closing one of those closes it on the Mac too.
        for notice in ChatNotices.current(conversation.transcript.items, dismissed: conversation.dismissedNotices, hostKeys: conversation.feed.dismissedNotices) {
            let usage: (title: String, hint: String, enabled: Bool, run: () -> Void)? = notice.isUsageLimit && !conversation.transcript.rateLimits.isEmpty
                ? ("Usage", "Shows the usage windows", true, { conversation.showUsageDetail() }) : nil
            lines.append(Line(id: "notice-\(notice.kind)", level: notice.level, icon: Self.icon(notice.level), text: notice.bannerText(), repeats: notice.count,
                              action: usage, close: { Task { await model.dismissChatNotice(chat.id, notice) } }))
        }
        return lines.sorted { $0.level.rank > $1.level.rank }
    }
    /// A failed dictation of `owner`, as a banner line: its reason, Open Settings when permission is the way out, × to put it away.
    static func dictationLine(_ owner: DictationOwner, controller: DictationController = .shared) -> Line? {
        guard case .failed(let problem) = controller.phase(for: owner) else { return nil }
        let settings: (title: String, hint: String, enabled: Bool, run: () -> Void)? = problem.opensSettings ? ("Open Settings", "Opens RiWork's settings", true, {
            controller.dismiss()
            if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
        }) : nil
        return Line(id: "dictation", level: .warning, icon: "mic.slash", text: problem.message, action: settings, close: { controller.dismiss() })
    }
    static func icon(_ level: ChatNoticeLevel) -> String {
        switch level {
        case .info: "info.circle"
        case .warning: "exclamationmark.triangle.fill"
        case .error: "xmark.octagon.fill"
        }
    }

    /// A small link with a full 44-point target around it.
    private func link(_ title: String, alignment: Alignment) -> some View {
        Text(title).font(style.system(.caption)).foregroundStyle(style.link)
            .padding(.horizontal, 8).frame(minWidth: style.target, minHeight: style.target, alignment: alignment).contentShape(Rectangle())
    }
    var body: some View {
        let lines = lines
        let history = ChatNotices.all(conversation.transcript.items).count
        let shown = expanded ? lines : Array(lines.prefix(2))
        VStack(spacing: 4) {
            // Always one view, at no height when there is nothing to say, so the row's own geometry (and the transcript's beside it)
            // keeps being measured as lines come and go.
            Color.clear.frame(height: 0)
            ForEach(shown) { line in ChatNoticeLine(line: line) }
            if lines.count > 2 || (history > 0 && !lines.isEmpty) {
                HStack(spacing: 12) {
                    if lines.count > 2 {
                        Button { expanded.toggle() } label: { link(expanded ? "Show fewer" : "\(lines.count - 2) more", alignment: .leading) }
                            .accessibilityLabel(expanded ? "Show fewer messages" : "Show \(lines.count - 2) more messages")
                            .chatLayoutProbe("banners-more", action: { expanded.toggle() })
                    }
                    Spacer(minLength: 0)
                    if history > 0 {
                        Button(action: showHistory) { link("\(history) \(history == 1 ? "notice" : "notices")", alignment: .trailing) }
                            .accessibilityHint("Every notice of the provider in this chat")
                            .chatLayoutProbe("notices-history", action: showHistory)
                    }
                }
                .buttonStyle(.plain).padding(.horizontal, 6)
            }
        }
        .onChange(of: model.state) { _, _ in closedLink = nil }
        .onChange(of: state) { _, _ in closedState = nil }
        .onChange(of: conversation.readError?.message) { _, message in if message == nil { conversation.dismissedReadError = nil } }
        .chatLayoutProbe("banners", visible: !lines.isEmpty)
    }
}

/// One line of the banner row: the level's glyph and color, the text, an action when there is one, ×.
struct ChatNoticeLine: View {
    @Environment(\.desktopStyle) private var style
    let line: ChatNoticeBanners.Line
    private var tint: Color {
        switch line.level {
        case .info: style.muted
        case .warning: style.gold
        case .error: style.error
        }
    }
    var body: some View {
        HStack(alignment: .center, spacing: 8) {
            if line.working { ActivityIndicator(activity: .working) } else { Image(systemName: line.icon).foregroundStyle(tint).accessibilityHidden(true) }
            Text(line.text + (line.repeats > 1 ? " ×\(line.repeats)" : "")).font(style.system(.footnote))
                .foregroundStyle(line.level == .info ? style.muted : style.text).lineLimit(4).fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let action = line.action {
                Button(action.title, action: action.run).buttonStyle(DesktopButtonStyle(compact: true)).nativeGlass(style, in: Capsule())
                    .disabled(!action.enabled).accessibilityHint(action.hint)
            }
            Button("Close message", systemImage: "xmark", action: line.close).labelStyle(.iconOnly)
                .font(style.system(.caption, weight: .semibold)).foregroundStyle(style.muted).buttonStyle(TargetButtonStyle())
                .chatLayoutProbe("close-\(line.id)", action: line.close)
        }
        .padding(.leading, 12).padding(.trailing, 2)
        .modifier(StatusLineSurface(tint: tint))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(line.level == .error ? "Error" : (line.level == .warning ? "Warning" : "Note")): \(line.text)")
        .chatLayoutProbe("banner-\(line.id)")
    }
}

/// Every notice of the provider in this chat, newest first, with how often its kind was said.
struct ChatNoticeHistory: View {
    @Environment(\.desktopStyle) private var style
    let notices: [ChatProviderNotice]
    let done: () -> Void
    var body: some View {
        NavigationStack {
            List(notices.reversed()) { notice in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Image(systemName: ChatNoticeBanners.icon(notice.level)).foregroundStyle(notice.level == .error ? style.error : (notice.level == .warning ? style.gold : style.muted))
                        .accessibilityHidden(true)
                    Text(notice.text).font(style.system(.footnote)).foregroundStyle(style.text).textSelection(.enabled)
                    Spacer(minLength: 0)
                    if notice.count > 1 { Text("×\(notice.count)").font(style.system(.caption)).foregroundStyle(style.muted).monospacedDigit() }
                }
                .listRowBackground(style.background)
            }
            .listStyle(.plain).scrollContentBackground(.hidden).background(style.background)
            .overlay { if notices.isEmpty { Text("No notices in this chat.").font(style.system(.footnote)).foregroundStyle(style.muted) } }
            .navigationTitle("Notices").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done", action: done) } }
        }
        .presentationDetents([.medium, .large])
    }
}

/// What a banner line sits on. The terminal look: a band in its signal color with a rule under it. Native: a rounded tinted panel set
/// in from the edges, as the request bars and the upload line are; a quiet one (a note, Starting, Stopped) in the active fill.
struct StatusLineSurface: ViewModifier {
    @Environment(\.desktopStyle) private var style
    let tint: Color
    func body(content: Content) -> some View {
        let quiet = tint == style.muted
        if style.native {
            content.background(quiet ? style.active.opacity(0.6) : tint.opacity(0.12), in: style.block(12)).padding(.horizontal, 8)
        } else {
            content.background(quiet ? style.active.opacity(0.5) : tint.opacity(0.10)).overlay(alignment: .top) { DesktopRule() }
        }
    }
}

// MARK: - Transcript

/// The transcript, newest at the bottom. It follows the bottom while the chat streams and the reader is there; scrolled up, it stays
/// where it is and a pill takes the reader back (`StickyBottom`, the rule the terminal follows too).
/// Geometry bookkeeping does not invalidate the view on every scroll frame.
@MainActor final class ChatHistoryViewport {
    weak var nativeScrollView: UIScrollView?
    let keyboardTap = KeyboardDismissTap()
    var metrics: ScrollMetrics?
    var frames: [String: CGRect] = [:]
    var frameOwners: [String: UUID] = [:]
    var anchor: (id: String, y: CGFloat)? { didSet { anchorGeneration = UUID(); unmeasuredScrolls = 0 } }
    private var anchorGeneration = UUID()
    var correction: Task<Void, Never>?
    var correctionToken: UUID?
    var correctionAgain = false
    /// Scrolls to the anchor's row while it had no measured frame, since the anchor or the rows last changed (`scheduleHistoryCorrection`).
    var unmeasuredScrolls = 0
    var pageInstalled = false
    var waitingForPage = false

    /// The page's explicit alignment must belong to the same capture after layout yields.
    /// A completed gesture, jump or identical recapture invalidates the old lifetime.
    func alignInstalledAnchor(while allowed: () -> Bool,
                              yield: () async -> Void = { await Task.yield() },
                              scroll: (String) -> Void) async -> Bool {
        guard let anchor, pageInstalled, !Task.isCancelled else { return false }
        let generation = anchorGeneration
        await yield()
        guard !Task.isCancelled, allowed(), pageInstalled, anchorGeneration == generation,
              self.anchor?.id == anchor.id, self.anchor?.y == anchor.y else { return false }
        scroll(anchor.id)
        return true
    }
}

/// A row's last measured frame, kept out of SwiftUI state so measuring never invalidates the row. The list can hide a row and show it
/// again where it was (rows changing around it), which brings no new geometry callback: the row reports this frame again as it appears,
/// so a history anchor on it is measured (`ChatHistoryViewport.frames`).
@MainActor final class ChatRowFrame { var value: CGRect? }

/// Reports whether a worker card's row is in view, for the "workers running" chip. Other rows are not watched.
private struct WorkerVisibility: ViewModifier {
    let tracked: Bool
    let changed: @MainActor (Bool) -> Void
    @ViewBuilder func body(content: Content) -> some View {
        if tracked { content.onScrollVisibilityChange(threshold: 0.2, changed) } else { content }
    }
}

/// Capture the concrete scrolling ancestor from a materialized row. List's
/// ScrollPosition does not perform absolute Y corrections on every supported OS.
private struct TranscriptScrollCapture: UIViewRepresentable {
    @Environment(\.chatDropHandler) private var dropHandler
    let viewport: ChatHistoryViewport
    func makeUIView(context: Context) -> Installer {
        let view = Installer(viewport: viewport)
        view.dropPassThrough = dropHandler?.transcript
        return view
    }
    func updateUIView(_ view: Installer, context: Context) {
        view.viewport = viewport; view.dropPassThrough = dropHandler?.transcript; view.capture()
    }

    final class Installer: UIView {
        weak var viewport: ChatHistoryViewport?
        weak var dropPassThrough: TranscriptDropPassThrough?
        init(viewport: ChatHistoryViewport) {
            self.viewport = viewport
            super.init(frame: .zero)
            isUserInteractionEnabled = false
        }
        required init?(coder: NSCoder) { fatalError() }
        override func didMoveToWindow() { super.didMoveToWindow(); capture() }
        override func didMoveToSuperview() { super.didMoveToSuperview(); capture() }
        func capture() {
            var ancestor = superview
            while let current = ancestor, !(current is UIScrollView) { ancestor = current.superview }
            guard let scroll = ancestor as? UIScrollView, let viewport else { return }
            viewport.nativeScrollView = scroll
            dropPassThrough?.capture(scroll)
            if viewport.keyboardTap.view !== scroll {
                viewport.keyboardTap.view?.removeGestureRecognizer(viewport.keyboardTap)
                scroll.addGestureRecognizer(viewport.keyboardTap)
            }
        }
    }
}

final class KeyboardDismissTap: UITapGestureRecognizer, UIGestureRecognizerDelegate {
    init() {
        super.init(target: nil, action: nil)
        addTarget(self, action: #selector(dismiss))
        cancelsTouchesInView = false
        delegate = self
    }
    @objc private func dismiss() { view?.window?.endEditing(true) }
    func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool { true }
}

/// Flatten disclosure into the one outer lazy stack. A nested lazy stack is still a single
/// giant cell to its parent and can measure all 1,200 children during Expand all.
private struct ChatTranscriptVisibleRow: Identifiable {
    let row: ChatProjectionRow
    let depth: Int
    let artifacts: Bool
    let commentary: Bool
    var footer = false
    var nested = false
    var worker: WorkerCard?
    var subagent: ChatSubagent?
    var isCard: Bool { worker != nil || subagent != nil }
    var id: String { footer ? "footer:" + row.id : row.id }

    static func flatten(_ rows: [ChatProjectionRow], mode: ChatDisplayMode, files: ChatFileIndex,
                        timeline: WorkerTimeline, subagents: [String: ChatSubagent]) -> [Self] {
        var result: [Self] = []
        func containsCard(_ rows: [ChatProjectionRow]) -> Bool {
            rows.contains { timeline.card(for: $0.id) != nil || subagents[$0.id] != nil || containsCard($0.children) }
        }
        func walk(_ rows: [ChatProjectionRow], depth: Int, artifacts: Bool, commentary: Bool) {
            for row in rows {
                let worker = timeline.card(for: row.id)
                let subagent = worker == nil ? subagents[row.id] : nil
                // Inline host cards must stay in this renderer even under a deeply
                // nested semantic agent; the generic pushed tree has no inventory.
                let nested = depth > 2 && row.kind == .subagent && worker == nil && subagent == nil && !containsCard(row.children)
                result.append(Self(row: row, depth: depth, artifacts: artifacts, commentary: commentary, nested: nested,
                                   worker: worker, subagent: subagent))
                // A card owns its status/result; disclosed tool children still use the flattened list.
                if nested && worker == nil && subagent == nil { continue }
                if row.open {
                    walk(row.children, depth: depth + 1, artifacts: row.kind == .activity ? false : artifacts,
                         commentary: row.kind == .activity ? false : commentary)
                }
                let disclosureRow = ![ChatProjectionRowKind.user, .final, .commentary, .request, .compaction, .notice].contains(row.kind)
                let strip = artifacts && disclosureRow ? files.resolve(artifacts: row.artifacts) : []
                if !strip.isEmpty || (row.open && row.kind == .subagent && !row.children.isEmpty) {
                    result.append(Self(row: row, depth: depth, artifacts: artifacts, commentary: commentary, footer: true,
                                       worker: worker, subagent: subagent))
                }
                if commentary { walk(row.comments, depth: depth, artifacts: false, commentary: true) }
            }
        }
        walk(rows, depth: 0, artifacts: true, commentary: mode == .verbose)
        return result
    }
}

/// A concrete one-cell view lets the list determine row count without evaluating
/// the conditional footer/call body for every off-screen entry.
private struct ChatTranscriptCell: View, Equatable {
    let entry: ChatTranscriptVisibleRow
    let items: ChatTranscript
    let fileIndex: ChatFileIndex
    let clockMS: UInt64
    let provider: ChatProvider
    let toggle: @MainActor (String) -> Void
    let onFrame: @MainActor (String, UUID, CGRect?) -> Void
    @State private var frameToken = UUID()
    @State private var lastFrame = ChatRowFrame()

    nonisolated static func == (lhs: Self, rhs: Self) -> Bool {
        guard lhs.entry.row == rhs.entry.row, lhs.entry.depth == rhs.entry.depth,
              lhs.entry.artifacts == rhs.entry.artifacts, lhs.entry.commentary == rhs.entry.commentary,
              lhs.entry.footer == rhs.entry.footer, lhs.entry.nested == rhs.entry.nested,
              lhs.entry.worker == rhs.entry.worker, lhs.entry.subagent == rhs.entry.subagent,
              lhs.provider == rhs.provider, !lhs.entry.row.running || lhs.clockMS == rhs.clockMS else { return false }
        let row = lhs.entry.row
        guard lhs.items.item(row.id) == rhs.items.item(row.id), lhs.fileIndex.byItem[row.id] == rhs.fileIndex.byItem[row.id],
              lhs.fileIndex.resolve(artifacts: row.artifacts) == rhs.fileIndex.resolve(artifacts: row.artifacts) else { return false }
        if [.user, .final, .commentary].contains(row.kind), let item = lhs.items.item(row.id) {
            return lhs.fileIndex.byTurn[item.turnID ?? "legacy:\(item.id)"] == rhs.fileIndex.byTurn[item.turnID ?? "legacy:\(item.id)"]
        }
        // Deep subagents retain the existing navigation-link renderer.
        if entryNested(lhs.entry), lhs.items != rhs.items { return false }
        return true
    }
    nonisolated private static func entryNested(_ entry: ChatTranscriptVisibleRow) -> Bool { entry.nested }

    var body: some View {
        let _ = Perf.count("body.ChatTranscriptCell")
        VStack(alignment: .leading, spacing: 0) {
            if entry.footer {
                ChatActivityFooter(row: entry.row, item: items.item(entry.row.id),
                                   files: entry.artifacts ? fileIndex.resolve(artifacts: entry.row.artifacts) : [],
                                   ownedFiles: fileIndex.byItem[entry.row.id] ?? [], provider: provider, showsDetail: !entry.isCard)
            } else if let worker = entry.worker {
                ChatWorkerCard(card: worker, isOpen: entry.row.open, toggle: { toggle(entry.row.id) })
                    .padding(.horizontal, 16).padding(.vertical, 3)
            } else if let subagent = entry.subagent {
                ChatSubagentCard(run: subagent, isOpen: entry.row.open, toggle: { toggle(entry.row.id) })
                    .padding(.horizontal, 16).padding(.vertical, 3)
            } else {
                ChatActivityRows(fileIndex: fileIndex, row: entry.row, items: items, clockMS: clockMS, provider: provider,
                                 toggle: toggle, showArtifacts: entry.artifacts, showCommentary: entry.commentary,
                                 depth: entry.depth, rendersDescendants: entry.nested, onFrame: onFrame).equatable()
            }
        }
        .padding(.leading, CGFloat(entry.depth * 16))
        .id(entry.id)
        .onGeometryChange(for: CGRect.self) { $0.frame(in: .named("chat-transcript-space")) } action: { frame in
            if entry.isCard && !entry.footer { lastFrame.value = frame; onFrame(entry.row.id, frameToken, frame) }
        }
        .onAppear { if entry.isCard && !entry.footer, let frame = lastFrame.value { onFrame(entry.row.id, frameToken, frame) } }
        .onDisappear { if entry.isCard && !entry.footer { onFrame(entry.row.id, frameToken, nil) } }
    }
}

private struct ChatTranscriptList: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.chatFileIndex) private var fileIndex
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let conversation: ChatConversation
    let provider: ChatProvider
    let state: ChatState
    /// The lists the worker cards take their live state from.
    let workers: WorkerInventory
    /// The software keyboard is up (or no hardware keyboard is attached): dragging or tapping the transcript puts it away.
    let dismissesKeyboard: Bool
    var loadOlder: @MainActor (@MainActor () async -> Void) async -> Void = { _ in }
    @State private var paging: Task<Void, Never>?
    @State private var preservingHistory = false
    @State private var preservingPageAnchor = false
    @State private var disclosureAnchorRelease: Task<Void, Never>?
    @State private var viewport = ChatHistoryViewport()
    @State private var position = ScrollPosition(edge: .bottom)
    @State private var sticky = StickyBottom()
    @State private var userDriven = false
    @State private var bottomCorrection: Task<Void, Never>?
    @State private var bottomCorrectionAgain = false
    /// The worker cards in view now, by item id: the running ones that are not are what the chip counts.
    @State private var workersInView: Set<String> = []
    /// The card the chip asked to be scrolled to, taken up (with the reader's current proxy) as soon as it is set.
    @State private var revealRequest: String?
    /// The chip's scroll to a worker card, put right once the rows on the way are measured.
    @State private var revealCorrection: Task<Void, Never>?
    @State private var revealGeneration = UUID()

    @State private var accessibilityPageTarget: Double?

    private var rows: [ChatProjectionRow] {
        conversation.projectionRows
    }

    var body: some View {
        let transcript = conversation.transcript
        let timeline = WorkerTimeline(items: transcript.items, inventory: workers)
        let rows = rows
        let clockMS = conversation.projectionCache.clockMS
        let visibleRows = ChatTranscriptVisibleRow.flatten(rows, mode: conversation.displayMode, files: fileIndex,
                                                           timeline: timeline, subagents: ChatSubagent.cards(in: transcript.items, provider: provider))
        let _ = Perf.count("body.ChatTranscriptList")
        ScrollViewReader { proxy in
        List {
            Group {
                Color.clear.frame(height: 12)
                if !conversation.feed.loaded {
                    placeholder("Loading the conversation…", spinner: true)
                } else if transcript.items.isEmpty {
                    placeholder("Ask \(provider.title) to work in this project.", spinner: false)
                }
                if conversation.feed.hasOlder {
                    Button {
                        pageOlder(proxy)
                    } label: {
                        HStack { if conversation.historyLoading { ProgressView() }; Text(conversation.historyError ?? "Load older messages") }
                            .frame(maxWidth: .infinity, minHeight: 44)
                    }.disabled(conversation.historyLoading).accessibilityLabel("Load older messages").id("chat-history")
                }
                ForEach(visibleRows) { entry in
                    ChatTranscriptCell(entry: entry, items: transcript, fileIndex: fileIndex, clockMS: clockMS, provider: provider,
                                       toggle: { toggle($0, proxy: proxy) }, onFrame: { id, owner, frame in
                        if frame == nil, viewport.frameOwners[id] != owner { return }
                        viewport.frameOwners[id] = frame == nil ? nil : owner
                        viewport.frames[id] = frame
                        if preservingHistory, viewport.pageInstalled || viewport.waitingForPage { scheduleHistoryCorrection(proxy) }
                    }).equatable()
                        .background(TranscriptScrollCapture(viewport: viewport))
                        .modifier(WorkerVisibility(tracked: entry.worker != nil && !entry.footer) { seen in
                            if seen { workersInView.insert(entry.row.id) } else { workersInView.remove(entry.row.id) }
                        })
                        .onDisappear { if !entry.footer { workersInView.remove(entry.row.id) } }
                }
                if (state == .running || state == .waiting || state == .starting) && !rows.contains(where: { $0.running }) { workingRow }
                Color.clear.frame(height: 6).id(Self.end)
                    .background(TranscriptScrollCapture(viewport: viewport))
            }
            .listRowInsets(EdgeInsets())
            .listRowSeparator(.hidden)
            .listRowBackground(Color.clear)
        }
        .listStyle(.plain)
        .buttonStyle(.plain)
        .listRowSpacing(0)
        .environment(\.defaultMinListRowHeight, 0)
        .environment(\.defaultMinListHeaderHeight, 0)
        .scrollContentBackground(.hidden)
        .contentMargins(.all, 0, for: .scrollContent)
        .onChange(of: dismissesKeyboard, initial: true) { _, enabled in viewport.keyboardTap.isEnabled = enabled }
        .chatLayoutProbe("transcript")
        .coordinateSpace(name: Self.space)
        .scrollPosition($position)
        .defaultScrollAnchor(.bottom, for: .initialOffset)
        .defaultScrollAnchor(.top, for: .sizeChanges)
        .accessibilityScrollAction { edge in accessibilityScroll(edge, proxy: proxy) }
        .chatLayoutProbe("transcript-accessibility-previous", action: { accessibilityScroll(.top, proxy: proxy) })
        .chatLayoutProbe("transcript-accessibility-next", action: { accessibilityScroll(.bottom, proxy: proxy) })
        // Dragging the list down takes the software keyboard with the finger, as in a chat, and a tap on the list puts it away. Typing
        // on a hardware keyboard there is none to put away, and the composer must not lose it to a scroll. That goes by the keyboard on
        // screen, not by what GameController reports attached: a paired keyboard or a keyboard case leaves the software one up too.
        .scrollDismissesKeyboard(dismissesKeyboard ? .interactively : .never)
        .onScrollPhaseChange { _, phase in
            userDriven = phase == .interacting || phase == .decelerating
            if userDriven {
                cancelHistoryAnchor()
                sticky.stopFollowing(); bottomCorrection?.cancel(); bottomCorrection = nil
                revealCorrection?.cancel(); revealCorrection = nil
            }
        }
        .onScrollGeometryChange(for: ScrollMetrics.self) { geometry in
            // List reports its full bounds, including the keyboard/composer inset.
            // Following, paging and history anchors use the visible height inside it.
            ScrollMetrics(offset: geometry.contentOffset.y, contentHeight: geometry.contentSize.height, boundsHeight: geometry.containerSize.height,
                          topInset: geometry.contentInsets.top, bottomInset: geometry.contentInsets.bottom)
        } action: { old, new in
            viewport.metrics = new
            if let target = accessibilityPageTarget, abs(new.offset - target) <= 2 {
                accessibilityPageTarget = nil
                UIAccessibility.post(notification: .pageScrolled, argument: nil)
            }
            if userDriven, new.offset + new.topInset < 100, conversation.feed.hasOlder { pageOlder(proxy) }
            if preservingHistory {
                if viewport.pageInstalled || viewport.waitingForPage { scheduleHistoryCorrection(proxy) }
                return
            }
            // Lazy measurements and disclosure animations can move the offset
            // after a bottom correction has finished. Only a reader's gesture
            // should opt out of following; those layout moves are still live.
            let response = sticky.metricsChanged(from: !userDriven && sticky.following ? nil : old, to: new, lineHeight: 24, userDriven: userDriven)
            // An animated jump can finish against a lazy stack's previous height while the approval/composer resizes.
            // StickyBottom treats overscroll as following; it still needs correction to the newly measured content end.
            let bottom = new.bottomOffset
            let settle = sticky.following && !userDriven && new.resized(since: old) && new.distanceFromBottom > 0.5
            if response == .scrollToBottom || settle || (sticky.following && !userDriven && new.offset > bottom + 2) {
                scheduleBottomCorrection(again: new.resized(since: old)) { scrollToEnd(proxy) }
            }
        }
        .onChange(of: conversation.feed.itemArrivals) { _, count in sticky.contentChanged(end: count, epoch: 0) }
        .onChange(of: conversation.jumps) { _, _ in jump(proxy) }
        .onChange(of: revealRequest) { _, id in
            guard let id else { return }
            revealRequest = nil
            reveal(id, proxy)
        }
        .onChange(of: conversation.feed.loaded) { _, loaded in if loaded && !userDriven { jump(proxy) } }
        .onChange(of: rows) { before, after in
            guard visibleStructure(before) != visibleStructure(after), !userDriven, revealCorrection == nil else { return }
            // Incoming rows, requests, and disclosures belong at the live tail
            // while following. A top-row anchor here would disable all later
            // resize/streaming corrections until another gesture.
            if sticky.following {
                cancelHistoryAnchor()
                scheduleBottomCorrection { scrollToEnd(proxy) }
                return
            }
            if !preservingHistory { captureHistoryAnchor() }
            if let anchor = viewport.anchor, let id = anchorID(anchor.id, before: before, after: after) {
                // An unchanged mapping is not a new reader capture. Keep the
                // installed page's pending alignment generation alive.
                if id != anchor.id { viewport.anchor = (id: id, y: max(0, anchor.y)) }
                viewport.pageInstalled = true
                // A correction planned against the previous rows does not run against these; these get their own few scrolls.
                viewport.correction?.cancel(); viewport.correction = nil; viewport.correctionToken = nil; viewport.correctionAgain = false
                viewport.unmeasuredScrolls = 0
                scheduleHistoryCorrection(proxy)
                if !preservingPageAnchor { releaseDisclosureAnchorAfterLayout() }
            } else { cancelHistoryAnchor() }
        }
        .accessibilityLabel("\(provider.chatTitle) conversation")
        .onDisappear {
            cancelHistoryAnchor(); paging?.cancel(); paging = nil; bottomCorrection?.cancel(); bottomCorrection = nil
            revealCorrection?.cancel(); revealCorrection = nil
        }
        // Over the transcript's bottom edge, not in a row of its own: the transcript reaches the composer, and showing or hiding the
        // pill never resizes the viewport. It is shown only while the reader is scrolled up, so it covers nothing being followed.
        .overlay(alignment: .bottomTrailing) { pill(proxy).padding(.bottom, 6) }
        // Beside it, the running workers of this chat whose cards are out of view. Only while some card is live: the clock is read
        // every second then, since a worker the lists have not caught up with counts as starting for its first seconds only.
        .overlay(alignment: .bottomLeading) {
            if timeline.hasLive {
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    let hidden = timeline.running(now: ChatWorkerCard.seconds(context.date)).filter { !workersInView.contains($0.itemID) }
                    if let first = hidden.first, let title = WorkerTimeline.chipTitle(running: hidden.count) {
                        WorkersRunningChip(title: title) { revealRequest = first.itemID }.padding(.bottom, 6)
                    }
                }
            }
        }
        }
    }

    /// Scrolls a worker card into view (the chip). The transcript stops following the bottom, as a scroll by hand does.
    ///
    /// A row the lazy stack has not drawn yet is where its estimate puts it, and the estimates of the rows on the way are right only once
    /// they are drawn: the first scroll can stop short of the card. So the card is looked for after each scroll: drawn and in view, that
    /// is it; drawn, it is scrolled to again (now exactly); not drawn, the transcript moves most of a screen towards it and asks again.
    /// The reader taking over, or the jump to the latest, ends it.
    private func reveal(_ id: String, _ proxy: ScrollViewProxy) {
        cancelHistoryAnchor()
        sticky.stopFollowing(); bottomCorrection?.cancel(); bottomCorrection = nil
        // Verbose may put a worker under a disclosed group/subagent. Open its
        // ancestors before asking List for a concrete, stable cell identity.
        func path(to target: String, in source: [ChatProjectionRow]) -> [String]? {
            for row in source {
                if row.id == target { return [] }
                if let descendants = path(to: target, in: row.children) { return [row.id] + descendants }
            }
            return nil
        }
        for ancestor in path(to: id, in: rows) ?? [] { conversation.disclosure.overrides[ancestor] = true }
        proxy.scrollTo(id, anchor: .top)
        let entries = ChatTranscriptVisibleRow.flatten(rows, mode: conversation.displayMode, files: fileIndex,
                                                      timeline: WorkerTimeline(items: conversation.transcript.items, inventory: workers),
                                                      subagents: ChatSubagent.cards(in: conversation.transcript.items, provider: provider))
        let order = Dictionary(entries.enumerated().map { ($1.id, $0) }, uniquingKeysWith: { first, _ in first })
        revealCorrection?.cancel()
        let generation = UUID(); revealGeneration = generation
        revealCorrection = Task { @MainActor in
            defer { if revealGeneration == generation { revealCorrection = nil } }
            // A change of the rows does not end the reveal (it opens the card's ancestors itself); a card the conversation no longer
            // has (a reset, another snapshot) does, before the list resolves it against rows it is about to drop (`scrollToEnd`).
            for _ in 0..<8 {
                try? await Task.sleep(for: .milliseconds(60))
                guard !Task.isCancelled, !userDriven, isListed(id), let metrics = viewport.metrics else { return }
                let top = CGFloat(metrics.topInset), bottom = top + CGFloat(metrics.viewportHeight)
                if let frame = viewport.frames[id] {
                    if frame.minY >= top - 1, frame.maxY <= bottom + 1 { return }
                    proxy.scrollTo(id, anchor: .top)
                    continue
                }
                let drawn = viewport.frames.filter { $0.value.maxY > top && $0.value.minY < bottom }.compactMap { order[$0.key] }
                guard let target = order[id], let seen = drawn.min() else { return }
                let step = CGFloat(metrics.viewportHeight) * 0.75, offset = CGFloat(metrics.offset)
                scrollToAbsoluteY(target < seen ? max(-top, offset - step) : min(CGFloat(metrics.bottomOffset), offset + step))
                try? await Task.sleep(for: .milliseconds(30))
                guard !Task.isCancelled, !userDriven, isListed(id) else { return }
                proxy.scrollTo(id, anchor: .top)
            }
        }
    }

    private func accessibilityScroll(_ edge: Edge, proxy: ScrollViewProxy) {
        guard let metrics = viewport.metrics, metrics.viewportHeight > 0,
              edge == .top || edge == .bottom else { return }
        // VoiceOver pages toward the requested edge without a dragging phase.
        // Its page is the visible transcript, excluding the floating composer.
        let step = edge == .top ? -metrics.viewportHeight : metrics.viewportHeight
        let target = min(metrics.bottomOffset, max(-metrics.topInset, metrics.offset + step))
        cancelHistoryAnchor()
        bottomCorrection?.cancel(); bottomCorrection = nil
        revealCorrection?.cancel(); revealCorrection = nil
        sticky.stopFollowing()
        accessibilityPageTarget = target
        if target >= metrics.bottomOffset - 0.5 {
            sticky.jumpToBottom()
            scheduleBottomCorrection { scrollToEnd(proxy) }
        } else { scrollToAbsoluteY(CGFloat(target)) }
        if abs(metrics.offset - target) <= 0.5 {
            accessibilityPageTarget = nil
            UIAccessibility.post(notification: .pageScrolled, argument: nil)
        }
    }

    private func pageOlder(_ proxy: ScrollViewProxy) {
        guard paging == nil, !conversation.historyLoading, conversation.feed.hasOlder else { return }
        // Keep a stable row and its visible offset, including a partly visible row.
        // Lazy-stack height estimates cannot preserve the reader's position.
        sticky.stopFollowing()
        preservingPageAnchor = true
        disclosureAnchorRelease?.cancel(); disclosureAnchorRelease = nil
        bottomCorrection?.cancel(); bottomCorrection = nil
        revealCorrection?.cancel(); revealCorrection = nil
        paging = Task { @MainActor in
            defer { paging = nil }
            let requestedBefore = conversation.feed.before, pin = conversation.feed.historyCursor
            while userDriven && !Task.isCancelled { try? await Task.sleep(for: .milliseconds(50)) }
            guard !Task.isCancelled else { return }
            preservingPageAnchor = true
            captureHistoryAnchor()
            let beforeRows = rows
            viewport.waitingForPage = true
            await loadOlder {
                // A response may arrive during a new drag. Wait before mutation, then
                // capture the reader's current point, not the point at request time.
                while userDriven && !Task.isCancelled { try? await Task.sleep(for: .milliseconds(50)) }
                guard !Task.isCancelled else { return }
                preservingPageAnchor = true
                captureHistoryAnchor()
                viewport.waitingForPage = false
                viewport.pageInstalled = false
            }
            guard conversation.feed.historyCursor == pin, conversation.feed.before != requestedBefore else { cancelHistoryAnchor(); return }
            conversation.disclosure = ChatProjection.remapDisclosures(state: conversation.disclosure, before: beforeRows, after: rows)
            if let anchor = viewport.anchor, let remapped = anchorID(anchor.id, before: beforeRows, after: rows) {
                if remapped != anchor.id { viewport.anchor = (id: remapped, y: max(0, anchor.y)) }
            }
            viewport.pageInstalled = true
            // The alignment yields first, and the live read can reset the conversation or install another snapshot meanwhile: then
            // the page is no longer this feed's, and the list would resolve the anchor against rows it is about to drop (`scrollToEnd`).
            let aligned = await viewport.alignInstalledAnchor(while: { preservingHistory && !userDriven && conversation.feed.historyCursor == pin }) { id in
                proxy.scrollTo(id, anchor: .top)
            }
            if aligned { scheduleHistoryCorrection(proxy) }
            // The anchor remains tied to this installed page through later lazy-row
            // measurements. A gesture, jump, next page or disappearance replaces it.
        }
    }

    private func captureHistoryAnchor() {
        let height = CGFloat(viewport.metrics?.boundsHeight ?? 0)
        let visible = viewport.frames.filter { $0.value.maxY > 0 && $0.value.minY < height }
        // An unresolved request whose item is outside the loaded window stays
        // above every page. Pin content when it is visible, so that request's
        // fixed position cannot hide the reader's message after a prepend.
        let content = visible.filter {
            guard let row = findRow($0.key, in: rows) else { return false }
            return row.kind != .request && row.kind != .notice && row.kind != .compaction
        }
        let candidates = content.isEmpty ? visible : content
        let crossingTop = candidates.filter { $0.value.minY <= 0 }
        let anchor = crossingTop.max { $0.value.minY < $1.value.minY }
            ?? candidates.min { $0.value.minY < $1.value.minY }
        viewport.anchor = anchor.map { (id: $0.key, y: $0.value.minY) }
        preservingHistory = viewport.anchor != nil
    }
    private func cancelHistoryAnchor() {
        disclosureAnchorRelease?.cancel(); disclosureAnchorRelease = nil
        preservingPageAnchor = false
        preservingHistory = false; viewport.anchor = nil; viewport.pageInstalled = false; viewport.waitingForPage = false
        viewport.correction?.cancel(); viewport.correction = nil; viewport.correctionToken = nil
        viewport.correctionAgain = false
    }
    private func releaseDisclosureAnchorAfterLayout() {
        disclosureAnchorRelease?.cancel()
        disclosureAnchorRelease = Task { @MainActor in
            // Cover the disclosure animation and deferred lazy measurements,
            // then let future geometry changes use the normal scroll policy.
            try? await Task.sleep(for: .milliseconds(reduceMotion ? 80 : 250))
            guard !Task.isCancelled, !preservingPageAnchor else { return }
            cancelHistoryAnchor()
        }
    }
    private func scheduleHistoryCorrection(_ proxy: ScrollViewProxy) {
        guard viewport.correction == nil else { viewport.correctionAgain = true; return }
        viewport.correctionAgain = false
        let token = UUID(); viewport.correctionToken = token
        viewport.correction = Task { @MainActor in
            // Defer input until after the current layout callback, not as a completion deadline.
            try? await Task.sleep(for: .milliseconds(16))
            defer {
                if viewport.correctionToken == token {
                    viewport.correction = nil; viewport.correctionToken = nil
                    if viewport.correctionAgain && !Task.isCancelled { scheduleHistoryCorrection(proxy) }
                }
            }
            guard !Task.isCancelled, preservingHistory, viewport.pageInstalled || viewport.waitingForPage, !userDriven,
                  let metrics = viewport.metrics, let anchor = viewport.anchor else { return }
            guard let frame = viewport.frames[anchor.id], frame.maxY > 0, frame.minY < CGFloat(metrics.boundsHeight) else {
                // The first layout may not have registered a prepended lazy
                // target yet. Retry after it can load, even without a callback,
                // a few times per change of the rows: List resolves the row's
                // index when asked and scrolls on its next update, so a scroll
                // still pending when the rows change again (Expand all, then
                // Collapse all) points past them. An anchor that is never
                // measured keeps the place its last scroll gave it.
                guard viewport.unmeasuredScrolls < Self.unmeasuredScrollLimit, isListed(anchor.id) else { return }
                viewport.unmeasuredScrolls += 1
                proxy.scrollTo(anchor.id, anchor: .top)
                viewport.correctionAgain = true
                return
            }
            let delta = frame.minY - anchor.y
            if abs(delta) > 0.5 { scrollToAbsoluteY(CGFloat(metrics.offset) + delta) }
        }
    }

    private func scrollToAbsoluteY(_ offset: CGFloat) {
        if let scroll = viewport.nativeScrollView {
            scroll.setContentOffset(CGPoint(x: scroll.contentOffset.x, y: offset), animated: false)
        } else {
            position.scrollTo(y: offset)
        }
    }
    /// To the end marker, the list's last row, by the list's own collection view and at once. `ScrollViewProxy` resolves a row's index
    /// against the rows the list shows when asked and scrolls on its next update: rows the conversation dropped in between (a reset or
    /// a new snapshot after the history expired, the log cut short) left it scrolling past the end of the list, which raises. The
    /// collection view counts the rows it has when it scrolls; the next rows get their own correction once they are in (`rows`).
    private func scrollToEnd(_ proxy: ScrollViewProxy) {
        guard let list = viewport.nativeScrollView as? UICollectionView else { return proxy.scrollTo(Self.end, anchor: .bottom) }
        let section = list.numberOfSections - 1
        guard section >= 0 else { return }
        let count = list.numberOfItems(inSection: section)
        guard count > 0 else { return }
        list.scrollToItem(at: IndexPath(item: count - 1, section: section), at: .bottom, animated: false)
    }

    /// Lazy rows can change their measured height during layout (especially with accessibility text). Scroll after that pass,
    /// rather than feeding a new scroll position back into the geometry callback. A reader's gesture cancels the pending correction.
    private func scheduleBottomCorrection(again: Bool = true, _ scroll: @escaping @MainActor () -> Void) {
        // One already on its way goes once more when it is done if the content or what floats over the bottom grew again meanwhile (a
        // position change alone is no reason).
        guard bottomCorrection == nil else { if again { bottomCorrectionAgain = true }; return }
        bottomCorrectionAgain = false
        bottomCorrection = Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(16))
            guard !Task.isCancelled else { return }
            if sticky.following && !userDriven { scroll() }
            // Keep our own position changes from ending following while the lazy rows settle.
            try? await Task.sleep(for: .milliseconds(32))
            guard !Task.isCancelled else { return }
            if sticky.following && !userDriven {
                scroll()
                // The marker loads lazy rows; the content edge also includes adjusted insets and final measurements.
                position.scrollTo(edge: .bottom)
            }
            bottomCorrection = nil
            if bottomCorrectionAgain { scheduleBottomCorrection(scroll) }
        }
    }
    private static let end = "chat-transcript-end"
    private static let unmeasuredScrollLimit = 2
    nonisolated private static let space = "chat-transcript-space"

    private func findRow(_ id: String, in rows: [ChatProjectionRow]) -> ChatProjectionRow? {
        for row in rows {
            if row.id == id { return row }
            if let found = findRow(id, in: row.children) ?? findRow(id, in: row.comments) { return found }
        }
        return nil
    }
    /// Whether `id` is one of the list's own rows (`ChatTranscriptVisibleRow.flatten`): only those can be scrolled to.
    private func isListed(_ id: String) -> Bool {
        func listed(_ rows: [ChatProjectionRow], commentary: Bool) -> Bool {
            rows.contains { row in
                row.id == id || (row.open && listed(row.children, commentary: row.kind == .activity ? false : commentary))
                    || (commentary && listed(row.comments, commentary: true))
            }
        }
        return listed(rows, commentary: conversation.displayMode == .verbose)
    }
    private func firstItem(in rows: [ChatProjectionRow], containing id: String) -> String? {
        guard let row = findRow(id, in: rows) else { return nil }
        return row.children.first.flatMap { firstItem(in: [$0], containing: $0.id) } ?? row.id
    }
    private func visibleStructure(_ rows: [ChatProjectionRow]) -> [String] {
        rows.flatMap { [$0.id, String($0.open)] + ($0.open ? visibleStructure($0.children) : []) + visibleStructure($0.comments) }
    }
    private func anchorID(_ id: String, before: [ChatProjectionRow], after: [ChatProjectionRow]) -> String? {
        // Keep a surviving group/activity header itself in place when opening it.
        if let same = ChatProjection.visibleAnchor(rows: after, itemID: id) { return same }
        if let item = firstItem(in: before, containing: id), let mapped = ChatProjection.visibleAnchor(rows: after, itemID: item) { return mapped }
        // Commentary disappears in Normal; retain its owning group's stable item anchor.
        if let owner = before.first(where: { findRow(id, in: $0.comments) != nil || findRow(id, in: $0.children) != nil }),
           let item = firstItem(in: [owner], containing: owner.id) {
            return ChatProjection.visibleAnchor(rows: after, itemID: item)
        }
        return nil
    }
    private func toggle(_ key: String, proxy: ScrollViewProxy) {
        revealCorrection?.cancel(); revealCorrection = nil
        // Equatable cells keep their action closure across prepends and streaming.
        // Resolve the current projection through the conversation at tap time.
        let rows = self.rows
        guard let row = findRow(key, in: rows) else { return }
        let followTail = sticky.following
        if !followTail { captureHistoryAnchor() }
        withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.15)) {
            conversation.disclosure.toggle(key, currentlyOpen: row.open)
        }
        if followTail {
            cancelHistoryAnchor()
            scheduleBottomCorrection { scrollToEnd(proxy) }
            return
        }
        if let anchor = viewport.anchor, let id = anchorID(anchor.id, before: rows, after: self.rows) {
            viewport.anchor = (id: id, y: id == anchor.id ? anchor.y : max(0, anchor.y))
            viewport.pageInstalled = true
            scheduleHistoryCorrection(proxy)
            if !preservingPageAnchor { releaseDisclosureAnchorAfterLayout() }
        }
    }
    private func jump(_ proxy: ScrollViewProxy) {
        cancelHistoryAnchor()
        revealCorrection?.cancel(); revealCorrection = nil
        sticky.jumpToBottom()
        scrollToEnd(proxy)
        scheduleBottomCorrection { scrollToEnd(proxy) }
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
    @ViewBuilder private func pill(_ proxy: ScrollViewProxy) -> some View {
        let pill = sticky.pill
        Button { jump(proxy) } label: {
            if style.native {
                // Native: the arrow is a symbol, and the pill is glass on iOS 26, material before.
                Label((pill?.newLines ?? 0) > 0 ? "Latest · \((pill?.newLines ?? 0)) new" : "Latest", systemImage: "arrow.down").labelStyle(.titleAndIcon)
                    .font(style.face(11, bold: true, relativeTo: .caption)).foregroundStyle(style.accent)
                    .padding(.horizontal, 12).frame(minHeight: 44)
                    .background { if !style.glass { Capsule().fill(.ultraThinMaterial) } }
                    .nativeGlass(style, in: Capsule()).contentShape(Capsule())
            } else {
                Text((pill?.newLines ?? 0) > 0 ? "↓ Latest · \((pill?.newLines ?? 0)) new" : "↓ Latest").font(style.mono(11, bold: true, relativeTo: .caption)).foregroundStyle(style.accent)
                    .padding(.horizontal, 12).frame(minHeight: 44).background(.ultraThinMaterial, in: Capsule())
                    .overlay(Capsule().stroke(style.divider, lineWidth: 1)).contentShape(Capsule())
            }
        }
        .buttonStyle(.plain).padding(.horizontal, 10)
        .opacity(pill == nil ? 0 : 1).allowsHitTesting(pill != nil).accessibilityHidden(pill == nil)
        .accessibilityIdentifier("chat-latest-button")
        .chatLayoutProbe("latest", visible: pill != nil)
        .accessibilityLabel((pill?.newLines ?? 0) > 0 ? "Jump to latest, \((pill?.newLines ?? 0)) new" : "Jump to latest")
    }
}
