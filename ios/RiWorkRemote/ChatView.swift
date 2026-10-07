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
    /// Counts requests from the tab row's ⋯ menu to open the model picker.
    var openModels = 0
    @State private var focusToken = 0
    /// The model picker is up.
    @State private var showModels = false
    /// The history of the provider's notices is up.
    @State private var showNotices = false
    /// Counts requests from the tab row's ⋯ menu to show the notices.
    var openNotices = 0
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
            ChatTranscriptList(conversation: conversation, provider: info.provider, state: state, hardwareKeyboard: model.keyboard.hardware.isAttached, loadOlder: { beforeInstall in await model.loadOlderChat(chat.id, beforeInstall: beforeInstall) }, viewportChanged: { transcriptChanged($0, barShown: !approvals.isEmpty || !questions.isEmpty) })
                .id(chat.id)
            if let approval = approvals.first {
                BoundedScroll(maxHeight: max(110, height * 0.52 - squeeze)) {
                    ChatApprovalBar(approval: approval, count: approvals.count, keyHints: model.keyboard.hardware.isAttached, detailHeight: max(70, height * 0.2 - squeeze),
                                    busy: !connected || conversation.answered.contains(approval.requestID)) { decision in decide(approval, decision) }
                        .id(approval.requestID)
                }
            } else if let question = questions.first {
                ChatQuestionBar(question: question, scrollHeight: max(leastQuestionScroll, height * 0.3 - squeeze), busy: !connected || conversation.answered.contains(question.requestID)) { form in
                    Task { await model.answerChatQuestion(chat.id, form) }
                }
                .id(question.requestID)
            }
            if let activity = model.uploadActivity(for: .chat(chat.id)) {
                UploadStatusBar(activity: activity, cancel: model.cancelUpload, dismiss: model.dismissUploadFailure)
            }
            // Every message of the moment, in one place: the link, the chat's state, what went wrong, the provider's notices.
            ChatNoticeBanners(model: model, chat: info, conversation: conversation, state: state, showHistory: { showNotices = true })
            ChatComposer(conversation: conversation, provider: info.provider, state: state, approval: approvals.first, connected: connected, focusToken: focusToken,
                         send: { Task { await model.sendChatDraft(chat.id) } }, interrupt: interrupt, decide: { decision in if let approval = approvals.first { decide(approval, decision) } },
                         attach: { picking = $0 }, pasteFiles: pasteFiles)
        }
        .frame(maxWidth: 760)
        .frame(maxWidth: .infinity)
        .background(style.background)
        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height = $0; squeeze = 0 }
        .attachmentPicker($picking, onDone: requestFocus) { model.attach($0, to: .chat(chat.id)) }
        .task(id: chat.id) { await model.followChat(chat.id) }
        .onAppear { requestFocus() }
        .onChange(of: chat.id) { _, _ in requestFocus() }
        .onChange(of: refocus) { _, _ in requestFocus() }
        .onChange(of: openModels) { _, _ in if connected { showModels = true } }
        .onChange(of: openNotices) { _, _ in showNotices = true }
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
    }

    /// The least the questions may scroll in: a question and an answer, at least.
    private var leastQuestionScroll: CGFloat { style.native ? 60 : 100 }
    /// The least of the transcript that stays on screen above a request or question bar: the last message, at one line.
    private var leastTranscript: CGFloat { style.pt(48) }
    /// In Native, where the bars are taller, a bar over a short screen (the keyboard up) gives up scrolling room until the transcript
    /// keeps `leastTranscript`; the terminal look keeps its bars as they were. It only ever gives more up, and starts over when the
    /// screen's height changes: a bar cuts its answers between rows, so its height moves in steps, and taking room back as the
    /// transcript grows would swing between two steps without end.
    private func transcriptChanged(_ transcript: CGFloat, barShown: Bool) {
        guard style.native, barShown else { if squeeze != 0 { squeeze = 0 }; return }
        guard transcript < leastTranscript - 0.5 else { return }
        let next = min(squeeze + leastTranscript - transcript, height * 0.3)
        if next - squeeze >= 1 { squeeze = next }
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
        // The mode gives up its word before anything wraps; the model's name is cut last (`ChatModelChip` gives way first of all).
        ViewThatFits(in: .horizontal) {
            row(modeTitle: true)
            row(modeTitle: false)
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 4) { modelButton; Spacer(minLength: 4); modeButton(title: false) }
                if let meter { ChatUsageRing(meter: meter) }
            }
        }
        .padding(.horizontal, 8)
        .background(style.background)
        .overlay(alignment: .bottom) { if !style.glass { DesktopRule() } }
    }
    private func row(modeTitle: Bool) -> some View {
        HStack(spacing: 2) {
            modelButton
            if let meter { ChatUsageRing(meter: meter) }
            Spacer(minLength: 4)
            modeButton(title: modeTitle)
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

/// How full the chat's context is: a ring that fills with the share used, the percent beside it. A tap (or a long press) shows the
/// tokens used of the window and Claude's cost estimate. It follows the chat's usage as it comes in.
struct ChatUsageRing: View {
    @Environment(\.desktopStyle) private var style
    let meter: ChatUsageMeter
    @State private var detail = false
    /// Gold above 80 % and red above 95 %, as the meter always was: a context that is nearly full is about to be compacted.
    private var tint: Color {
        let fraction = meter.contextFraction ?? 0
        return fraction > 0.95 ? style.error : (fraction > 0.8 ? style.gold : style.text)
    }
    var body: some View {
        Button { detail = true } label: {
            HStack(spacing: 5) {
                ZStack {
                    Circle().stroke(style.divider, lineWidth: 2.5)
                    Circle().trim(from: 0, to: meter.contextFraction ?? 0).stroke(tint, style: StrokeStyle(lineWidth: 2.5, lineCap: .round)).rotationEffect(.degrees(-90))
                }
                .frame(width: style.pt(15), height: style.pt(15))
                .animation(.easeOut(duration: 0.3), value: meter.contextFraction)
                Text(meter.percentText ?? meter.tokensText?.replacingOccurrences(of: " tokens", with: "") ?? "")
                    .font(style.system(.footnote, weight: .medium)).monospacedDigit().foregroundStyle(style.muted).lineLimit(1).fixedSize()
            }
            .padding(.horizontal, 6).frame(minWidth: style.target, minHeight: style.target).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .simultaneousGesture(LongPressGesture(minimumDuration: 0.35).onEnded { _ in detail = true })
        .popover(isPresented: $detail) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Context").font(style.system(.caption, weight: .semibold)).foregroundStyle(style.muted)
                Text([meter.percentText, meter.tokensText].compactMap { $0 }.joined(separator: " · ")).font(style.system(.subheadline, weight: .semibold)).monospacedDigit().foregroundStyle(style.text)
                if let cost = meter.costText { Text(cost).font(style.system(.footnote)).foregroundStyle(style.muted) }
            }
            .padding(14).fixedSize()
            .presentationCompactAdaptation(.popover)
            .accessibilityElement(children: .combine)
        }
        .chatLayoutProbe("usage", action: { detail = true })
        .accessibilityIdentifier("chat-usage")
        .accessibilityLabel("Context usage").accessibilityValue(meter.spoken ?? "")
        .accessibilityHint("Shows the tokens used and the cost estimate")
    }
}

/// The chat's part of the tab row's ⋯ menu, first in it while a chat is on screen.
struct ChatMenuSection: View {
    let model: RemoteModel
    let chat: ChatInfo
    let changeModel: () -> Void
    var showNotices: () -> Void = {}
    var body: some View {
        let conversation = model.chatConversations[chat.id] ?? ChatConversation(id: chat.id)
        let info = conversation.transcript.info ?? chat
        let state = model.chatState(info)
        let connected = model.state == .connected
        Section("Chat") {
            if let meter = conversation.transcript.usage.map(ChatUsageMeter.init), let text = meter.text {
                Text("Usage: \(text)").accessibilityLabel("Usage").accessibilityValue(meter.spoken ?? text)
            }
            Button("Change model…", systemImage: "cpu") { changeModel() }.disabled(!connected || state.isBusy || state == .starting)
            Button("Compact conversation", systemImage: "arrow.down.right.and.arrow.up.left") { Task { await model.compactChat(chat.id) } }
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
        // What the phone itself has to say: one line per source, replaced in place.
        for alert in conversation.alerts.ordered {
            lines.append(Line(id: "alert-\(alert.source.rawValue)", level: alert.level, icon: Self.icon(alert.level), text: alert.text, repeats: alert.repeats,
                              close: { conversation.alerts.clear(alert.source) }))
        }
        // The provider's notices of this turn, the latest of each kind.
        for notice in ChatNotices.current(conversation.transcript.items, dismissed: conversation.dismissedNotices) {
            lines.append(Line(id: "notice-\(notice.kind)", level: notice.level, icon: Self.icon(notice.level), text: notice.text,
                              close: { conversation.dismissedNotices.insert(notice.id) }))
        }
        return lines.sorted { $0.level.rank > $1.level.rank }
    }
    static func icon(_ level: ChatNoticeLevel) -> String {
        switch level {
        case .info: "info.circle"
        case .warning: "exclamationmark.triangle.fill"
        case .error: "xmark.octagon.fill"
        }
    }

    var body: some View {
        let lines = lines
        let history = ChatNotices.all(conversation.transcript.items).count
        let shown = expanded ? lines : Array(lines.prefix(2))
        VStack(spacing: 4) {
            ForEach(shown) { line in ChatNoticeLine(line: line) }
            if lines.count > 2 || (history > 0 && !lines.isEmpty) {
                HStack(spacing: 12) {
                    if lines.count > 2 {
                        Button(expanded ? "Show fewer" : "\(lines.count - 2) more") { expanded.toggle() }
                            .accessibilityLabel(expanded ? "Show fewer messages" : "Show \(lines.count - 2) more messages")
                    }
                    Spacer(minLength: 0)
                    if history > 0 {
                        Button("\(history) \(history == 1 ? "notice" : "notices")", action: showHistory).accessibilityHint("Every notice of the provider in this chat")
                            .chatLayoutProbe("notices-history", action: showHistory)
                    }
                }
                .font(style.system(.caption)).foregroundStyle(style.link).buttonStyle(.plain)
                .padding(.horizontal, 14).frame(minHeight: 32).contentShape(Rectangle())
            }
        }
        .padding(.top, lines.isEmpty ? 0 : 4)
        .animation(.easeInOut(duration: 0.15), value: lines.map(\.id))
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
    var metrics: ScrollMetrics?
    var frames: [String: CGRect] = [:]
    var anchor: (id: String, y: CGFloat)? { didSet { anchorGeneration = UUID() } }
    private var anchorGeneration = UUID()
    var correction: Task<Void, Never>?
    var correctionToken: UUID?
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

private struct ChatTranscriptList: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let conversation: ChatConversation
    let provider: ChatProvider
    let state: ChatState
    let hardwareKeyboard: Bool
    var loadOlder: @MainActor (@MainActor () async -> Void) async -> Void = { _ in }
    var viewportChanged: (CGFloat) -> Void = { _ in }
    @State private var paging: Task<Void, Never>?
    @State private var preservingHistory = false
    @State private var viewport = ChatHistoryViewport()
    @State private var position = ScrollPosition(edge: .bottom)
    @State private var sticky = StickyBottom()
    @State private var userDriven = false
    @State private var bottomCorrection: Task<Void, Never>?

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
                if conversation.feed.hasOlder {
                    Button {
                        pageOlder(proxy)
                    } label: {
                        HStack { if conversation.historyLoading { ProgressView() }; Text(conversation.historyError ?? "Load older messages") }
                            .frame(maxWidth: .infinity, minHeight: 44)
                    }.disabled(conversation.historyLoading).accessibilityLabel("Load older messages").id("chat-history")
                }
                // The provider's notices are the banner row's (and the history's), not rows of the transcript.
                ForEach(transcript.items.filter(ChatNotices.isTranscriptRow)) { item in
                    ChatItemRow(item: item, provider: provider, open: open(for: item), toggle: toggle).equatable()
                        .id(item.id)
                        .onGeometryChange(for: CGRect.self) { $0.frame(in: .named(Self.space)) } action: { frame in
                            viewport.frames[item.id] = frame
                            if preservingHistory, viewport.pageInstalled || viewport.waitingForPage { scheduleHistoryCorrection(proxy) }
                        }
                        .onDisappear { viewport.frames[item.id] = nil }
                }
                if state == .running || state == .waiting || state == .starting { workingRow }
                Color.clear.frame(height: 6).id(Self.end)
            }
            .padding(.top, 12)
        }
        .chatLayoutProbe("transcript")
        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { viewportChanged($0) }
        .coordinateSpace(name: Self.space)
        .scrollPosition($position)
        .defaultScrollAnchor(.bottom, for: .initialOffset)
        .defaultScrollAnchor(.top, for: .sizeChanges)
        // Dragging the list puts the software keyboard away, as in a chat; a hardware keyboard has none to put away, and must not lose
        // the composer to a scroll.
        .scrollDismissesKeyboard(hardwareKeyboard ? .never : .interactively)
        .onScrollPhaseChange { _, phase in
            userDriven = phase == .interacting || phase == .decelerating
            if userDriven {
                cancelHistoryAnchor()
                sticky.stopFollowing(); bottomCorrection?.cancel(); bottomCorrection = nil
            }
        }
        .onScrollGeometryChange(for: ScrollMetrics.self) { geometry in
            ScrollMetrics(offset: geometry.contentOffset.y, contentHeight: geometry.contentSize.height, viewportHeight: geometry.containerSize.height,
                          topInset: geometry.contentInsets.top, bottomInset: geometry.contentInsets.bottom)
        } action: { old, new in
            viewport.metrics = new
            if userDriven, new.offset + new.topInset < 100, conversation.feed.hasOlder { pageOlder(proxy) }
            if preservingHistory {
                if viewport.pageInstalled || viewport.waitingForPage { scheduleHistoryCorrection(proxy) }
                return
            }
            let response = sticky.metricsChanged(from: !userDriven && bottomCorrection != nil ? nil : old, to: new, lineHeight: 24, userDriven: userDriven)
            // An animated jump can finish against a lazy stack's previous height while the approval/composer resizes.
            // StickyBottom treats overscroll as following; it still needs correction to the newly measured content end.
            let bottom = max(-new.topInset, new.contentHeight - new.viewportHeight + new.bottomInset)
            let settle = sticky.following && !userDriven && new.resized(since: old) && new.distanceFromBottom > 0.5
            if response == .scrollToBottom || settle || (sticky.following && !userDriven && new.offset > bottom + 2) {
                scheduleBottomCorrection { proxy.scrollTo(Self.end, anchor: .bottom) }
            }
        }
        .onChange(of: conversation.feed.itemArrivals) { _, count in sticky.contentChanged(end: count, epoch: 0) }
        .onChange(of: conversation.jumps) { _, _ in jump(proxy) }
        .onChange(of: conversation.feed.loaded) { _, loaded in if loaded && !userDriven { jump(proxy) } }
        .accessibilityLabel("\(provider.chatTitle) conversation")
        .onDisappear { cancelHistoryAnchor(); paging?.cancel(); paging = nil; bottomCorrection?.cancel(); bottomCorrection = nil }
        // Over the transcript's bottom edge, not in a row of its own: the transcript reaches the composer, and showing or hiding the
        // pill never resizes the viewport. It is shown only while the reader is scrolled up, so it covers nothing being followed.
        .overlay(alignment: .bottomTrailing) { pill(proxy).padding(.bottom, 6) }
        }
    }

    private func pageOlder(_ proxy: ScrollViewProxy) {
        guard paging == nil, !conversation.historyLoading, conversation.feed.hasOlder else { return }
        // Keep a stable row and its visible offset, including a partly visible row.
        // Lazy-stack height estimates cannot preserve the reader's position.
        sticky.stopFollowing()
        bottomCorrection?.cancel(); bottomCorrection = nil
        paging = Task { @MainActor in
            defer { paging = nil }
            let requestedBefore = conversation.feed.before, pin = conversation.feed.historyCursor
            while userDriven && !Task.isCancelled { try? await Task.sleep(for: .milliseconds(50)) }
            guard !Task.isCancelled else { return }
            captureHistoryAnchor()
            viewport.waitingForPage = true
            await loadOlder {
                // A response may arrive during a new drag. Wait before mutation, then
                // capture the reader's current point, not the point at request time.
                while userDriven && !Task.isCancelled { try? await Task.sleep(for: .milliseconds(50)) }
                guard !Task.isCancelled else { return }
                captureHistoryAnchor()
                viewport.waitingForPage = false
                viewport.pageInstalled = false
            }
            guard conversation.feed.historyCursor == pin, conversation.feed.before != requestedBefore else { cancelHistoryAnchor(); return }
            viewport.pageInstalled = true
            let aligned = await viewport.alignInstalledAnchor(while: { preservingHistory && !userDriven }) { id in
                proxy.scrollTo(id, anchor: .top)
            }
            if aligned { scheduleHistoryCorrection(proxy) }
            // The anchor remains tied to this installed page through later lazy-row
            // measurements. A gesture, jump, next page or disappearance replaces it.
        }
    }

    private func captureHistoryAnchor() {
        let height = CGFloat(viewport.metrics?.viewportHeight ?? 0)
        viewport.anchor = viewport.frames.filter { $0.value.maxY > 0 && $0.value.minY < height }
            .min { $0.value.minY < $1.value.minY }.map { (id: $0.key, y: $0.value.minY) }
        preservingHistory = viewport.anchor != nil
    }
    private func cancelHistoryAnchor() {
        preservingHistory = false; viewport.anchor = nil; viewport.pageInstalled = false; viewport.waitingForPage = false
        viewport.correction?.cancel(); viewport.correction = nil; viewport.correctionToken = nil
    }
    private func scheduleHistoryCorrection(_ proxy: ScrollViewProxy) {
        guard viewport.correction == nil else { return }
        let token = UUID(); viewport.correctionToken = token
        viewport.correction = Task { @MainActor in
            // Defer input until after the current layout callback, not as a completion deadline.
            try? await Task.sleep(for: .milliseconds(16))
            defer { if viewport.correctionToken == token { viewport.correction = nil; viewport.correctionToken = nil } }
            guard !Task.isCancelled, preservingHistory, viewport.pageInstalled || viewport.waitingForPage, !userDriven,
                  let metrics = viewport.metrics, let anchor = viewport.anchor else { return }
            guard let frame = viewport.frames[anchor.id], frame.maxY > 0, frame.minY < CGFloat(metrics.viewportHeight) else {
                proxy.scrollTo(anchor.id, anchor: .top); return
            }
            let delta = frame.minY - anchor.y
            if abs(delta) > 0.5 { position.scrollTo(y: CGFloat(metrics.offset) + delta) }
        }
    }

    /// Lazy rows can change their measured height during layout (especially with accessibility text). Scroll after that pass,
    /// rather than feeding a new scroll position back into the geometry callback. A reader's gesture cancels the pending correction.
    private func scheduleBottomCorrection(_ scroll: @escaping @MainActor () -> Void) {
        guard bottomCorrection == nil else { return }
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
        }
    }
    private static let end = "chat-transcript-end"
    nonisolated private static let space = "chat-transcript-space"

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
    private func jump(_ proxy: ScrollViewProxy) {
        cancelHistoryAnchor()
        sticky.jumpToBottom()
        proxy.scrollTo(Self.end, anchor: .bottom)
        scheduleBottomCorrection { proxy.scrollTo(Self.end, anchor: .bottom) }
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
