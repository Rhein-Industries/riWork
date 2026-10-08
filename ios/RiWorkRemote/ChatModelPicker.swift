import SwiftUI
import UIKit
import RiWorkCore

// Choosing a chat's model, effort and Fast: the chip in the chat toolbar, the sheet it opens, and the controls the New terminal sheet shares
// with it. What each choice means (what is sent, what a key does) is `ChatModelChoices` and `ChatModelCursor` in RiWorkCore.

// MARK: - The chip

/// The model in the toolbar: its short name, a bolt when Fast is on, and a chevron. A tap opens the picker.
struct ChatModelChip: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.dynamicTypeSize) private var typeSize
    let choices: ChatModelChoices
    let enabled: Bool
    var compact = false
    let open: () -> Void

    var body: some View {
        let button = Button(action: open) {
            HStack(spacing: 7) {
                Image(systemName: "cpu").foregroundStyle(style.accent).accessibilityHidden(true)
                Text(choices.chipTitle).font(style.system(compact ? .footnote : .subheadline, weight: .semibold)).lineLimit(typeSize.isAccessibilitySize ? nil : 1).truncationMode(.tail)
                if choices.chipShowsFast { Image(systemName: "bolt.fill").font(.system(size: style.pt(10), weight: .bold)).foregroundStyle(style.gold).accessibilityHidden(true) }
                Image(systemName: "chevron.down").font(style.system(.caption2)).foregroundStyle(style.muted).accessibilityHidden(true)
            }
            .foregroundStyle(style.text).padding(.horizontal, 8)
            .frame(minHeight: style.target).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        Group {
            if compact { button } else { button.nativeGlass(style, in: Capsule()) }
        }
        // The first to give way when the toolbar is full (a long model name, large text): the name is cut, the other controls are not.
        .layoutPriority(-1)
        .disabled(!enabled)
        .accessibilityIdentifier("chat-model-chip")
        .accessibilityLabel("Model").accessibilityValue(choices.spoken)
        .accessibilityHint("Choose the model, the effort and Fast")
    }
}

// MARK: - Controls shared with the New terminal sheet

/// The efforts of a model as one row of segments. The one chosen is filled and underlined; the ring (a keyboard's) is on one segment.
struct ChatEffortSegments: View {
    @Environment(\.desktopStyle) private var style
    let efforts: [String]
    let selected: String?
    /// The segment the keyboard's ring is on, if it is shown.
    var ringed: Int?
    var enabled = true
    let choose: (Int, String) -> Void

    var body: some View {
        if style.native { nativeBody } else { terminalBody }
    }
    /// Native: a segmented control as iOS draws one, the chosen segment a raised capsule in a capsule track.
    private var nativeBody: some View {
        HStack(spacing: 2) {
            ForEach(Array(efforts.enumerated()), id: \.offset) { index, effort in
                let chosen = effort == selected
                Button { choose(index, effort) } label: {
                    Text(ChatEffort.title(effort)).font(style.face(12, bold: chosen, relativeTo: .subheadline)).lineLimit(1).minimumScaleFactor(0.7)
                        .padding(.horizontal, 2).frame(maxWidth: .infinity, minHeight: style.target)
                        .background { if chosen { Capsule().fill(chosenFill).shadow(color: .black.opacity(0.12), radius: 2, y: 1) } }
                        .overlay { if ringed == index { Capsule().stroke(style.accent, lineWidth: 2) } }
                        .contentShape(Capsule())
                }
                .buttonStyle(.plain).disabled(!enabled)
                .accessibilityLabel("\(ChatEffort.spoken(effort)) effort")
                .accessibilityAddTraits(chosen ? .isSelected : [])
            }
        }
        .padding(3).background(style.active, in: Capsule())
        .accessibilityElement(children: .contain).accessibilityLabel("Effort")
    }
    /// The chosen segment stands out of the track: white on light, a lighter gray on dark, as in a system segmented control.
    private var chosenFill: Color { style.colorScheme == .dark ? Color(uiColor: .systemGray3) : style.background }
    private var terminalBody: some View {
        HStack(spacing: 0) {
            ForEach(Array(efforts.enumerated()), id: \.offset) { index, effort in
                let chosen = effort == selected
                Button { choose(index, effort) } label: {
                    Text(ChatEffort.title(effort)).font(style.face(12, bold: chosen, relativeTo: .subheadline)).lineLimit(1).minimumScaleFactor(0.7)
                        .padding(.horizontal, 2).frame(maxWidth: .infinity, minHeight: style.target)
                        .background(chosen ? style.active : .clear)
                        .overlay(alignment: .bottom) { if chosen { Rectangle().fill(style.accent).frame(height: 2) } }
                        .overlay { if ringed == index { Rectangle().stroke(style.accent, lineWidth: 2) } }
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain).disabled(!enabled)
                .accessibilityLabel("\(ChatEffort.spoken(effort)) effort")
                .accessibilityAddTraits(chosen ? .isSelected : [])
                if index < efforts.count - 1 { Rectangle().fill(style.divider).frame(width: 1).padding(.vertical, 6).accessibilityHidden(true) }
            }
        }
        .overlay(Rectangle().stroke(style.divider, lineWidth: 1))
        .accessibilityElement(children: .contain).accessibilityLabel("Effort")
    }
}

/// The Fast switch, with its lightning glyph. In Native an on switch is the system green: Native's accent is black or white, and a
/// switch in it is hard to tell on from off.
struct ChatFastToggle: View {
    @Environment(\.desktopStyle) private var style
    @Binding var isOn: Bool
    var enabled = true
    var body: some View {
        Toggle(isOn: $isOn) {
            VStack(alignment: .leading, spacing: 2) {
                Label("Fast", systemImage: "bolt.fill").foregroundStyle(isOn ? style.gold : style.text)
                Text("Faster replies, at a higher usage rate.").font(style.system(.caption)).foregroundStyle(style.muted).fixedSize(horizontal: false, vertical: true)
            }
        }
        .tint(style.native ? Color(uiColor: .systemGreen) : style.accent)
        .disabled(!enabled)
        .accessibilityIdentifier("chat-fast-toggle")
        .accessibilityLabel("Fast").accessibilityHint("Faster replies, at a higher usage rate")
    }
}

// MARK: - The sheet

/// The picker: the provider's models (name, a line about each, the default marked), the efforts of the one chosen as a row of segments,
/// and Fast when the model has it. Every choice is sent at once as one `Configure`; the sheet stays up so a model and then its effort can
/// be chosen, and goes with Done, a swipe down, ⎋, ⌘. or ⌘M. On a desktop that lets a chat go on with the other provider, that provider's
/// default and models follow: choosing one sends `switch`, which keeps this chat, its tab and its conversation, and the sheet goes.
///
/// Keyboard (a Clicks or any hardware keyboard): ↑ ↓ (⇥ ⇧⇥, ^P ^N) move the ring down the models, then the efforts, then Fast; ← → move
/// along the efforts; ⏎ or space chooses what the ring is on (⏎ on what already is chosen closes the sheet); ⎋, ⌘. and ⌘M close it.
struct ChatModelSheet: View {
    @Environment(\.desktopStyle) private var style
    let model: RemoteModel
    let chat: ChatInfo
    let close: () -> Void
    @State private var cursor: ChatModelCursor
    @State private var keyboardInUse: Bool
    @State private var loadingModels = false
    @State private var modelsError: String?
    @State private var catalogueRequest = UUID()

    init(model: RemoteModel, chat: ChatInfo, close: @escaping () -> Void) {
        self.model = model; self.chat = chat; self.close = close
        let choices = (model.chatConversations[chat.id] ?? ChatConversation(id: chat.id)).modelChoices(fallback: chat, switchable: model.desktopFeatures.chatProviderSwitch)
        _cursor = State(initialValue: ChatModelCursor(for: choices))
        _keyboardInUse = State(initialValue: model.keyboard.hardware.isAttached)
    }

    private var conversation: ChatConversation { model.chatConversations[chat.id] ?? ChatConversation(id: chat.id) }
    /// The chat as it says it is now: after a switch, the other provider's.
    private var info: ChatInfo { conversation.transcript.info ?? chat }
    private var choices: ChatModelChoices { conversation.modelChoices(fallback: chat, switchable: model.desktopFeatures.chatProviderSwitch) }
    private var connected: Bool { model.state == .connected }

    var body: some View {
        let choices = choices
        VStack(spacing: 0) {
            WorkspaceBar(title: style.cased("Model")) { Button("Done", action: close).accessibilityHint("Closes the picker") }
            ScrollView { content(choices) }.scrollBounceBehavior(.basedOnSize).desktopSheetFooter(style) { footer }
        }
        .desktopSheetSurface(style)
        .foregroundStyle(style.text).font(style.face(13, relativeTo: .body)).tint(style.accent)
        .background { KeyCommandHost(active: true, actions: keyActions).frame(width: 1, height: 1).accessibilityHidden(true) }
        .presentationDetents([.medium, .large]).presentationDragIndicator(.visible)
        .onChange(of: choices) { _, fresh in cursor.reconcile(with: fresh) }
        .task {
            await loadModels()
            if model.desktopFeatures.chatProviderSwitch { await model.loadSwitchCatalogue(info) }
        }
        .onChange(of: conversation.modelCatalogueRevision) { _, _ in
            modelsError = nil; loadingModels = false; catalogueRequest = UUID()
        }
        .onDisappear { loadingModels = false; catalogueRequest = UUID() }
    }

    private func content(_ choices: ChatModelChoices) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            if let notice = conversation.notice { messageRow(notice, icon: "exclamationmark.triangle") }
            else if !connected { messageRow("Not connected. Choose again when the link is back.", icon: "wifi.slash") }
            sectionLabel("Model")
            if loadingModels { ProgressView("Loading models…").padding(12) }
            if let label = conversation.modelCatalogueSource.label { messageRow(label, icon: "clock.arrow.circlepath") }
            if let error = modelsError {
                messageRow(error, icon: "exclamationmark.triangle")
            }
            if conversation.modelCatalogueSource != .live || modelsError != nil {
                Button("Retry live models") { Task { await loadModels() } }.buttonStyle(TargetButtonStyle(tinted: true)).padding(.horizontal, 12).disabled(loadingModels || !connected)
            }
            Text("Current model: \(choices.current?.name ?? choices.modelID ?? "Provider default")")
                .font(style.system(.caption)).foregroundStyle(style.muted).padding(.horizontal, 12)
            ForEach(Array(choices.models.enumerated()), id: \.element.id) { index, option in modelRow(index, option, choices) }
            if !choices.efforts.isEmpty {
                sectionLabel("Effort")
                VStack(alignment: .leading, spacing: 6) {
                    ChatEffortSegments(efforts: choices.efforts, selected: choices.selectedEffort, ringed: ringedEffort, enabled: connected) { index, effort in
                        place(.effort, effort: index); send(.effort(effort))
                    }
                    if let current = choices.current, let standard = current.defaultEffortChoice, standard != choices.selectedEffort {
                        Text("\(current.shortName) uses \(ChatEffort.title(standard)) unless you choose another.").font(style.system(.caption)).foregroundStyle(style.muted)
                    }
                }
                .padding(.horizontal, 12).padding(.vertical, 4)
            }
            if choices.showsFast {
                sectionLabel("Speed")
                ChatFastToggle(isOn: Binding(get: { choices.fastIsOn }, set: { on in place(.fast); send(.fast(on)) }), enabled: connected)
                    .padding(.horizontal, 12).padding(.vertical, 6).frame(minHeight: max(style.target, style.pt(52))).overlay { ring(.fast) }
            }
            if let switching = choices.switching { switchSection(switching) }
        }
        .padding(.bottom, 8)
    }

    /// The other provider: its default and its models. Choosing one goes on with it in this chat.
    @ViewBuilder private func switchSection(_ switching: ChatProviderSwitch) -> some View {
        sectionLabel("Continue with \(switching.provider.title)")
        Text("Keeps this chat and its tab. \(switching.provider.title) gets the conversation so far.")
            .font(style.system(.caption)).foregroundStyle(style.muted).padding(.horizontal, 12).padding(.bottom, 4)
            .fixedSize(horizontal: false, vertical: true)
        if let blocked = switching.blocked { messageRow(blocked, icon: "hourglass") }
        if let label = conversation.switchCatalogueSource.label { messageRow(label, icon: "clock.arrow.circlepath") }
        ForEach(Array(switching.rows.enumerated()), id: \.element.id) { index, row in
            Button { place(.other(index)); switchTo(row.model?.id) } label: {
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: switching.provider.glyph).frame(width: style.pt(22), height: style.pt(22)).foregroundStyle(style.muted)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(row.title).font(style.face(14, relativeTo: .body)).lineLimit(1).truncationMode(.tail)
                        if let description = row.model?.description, !description.isEmpty {
                            Text(description).font(style.system(.caption)).foregroundStyle(style.muted).lineLimit(2).fixedSize(horizontal: false, vertical: true)
                        } else if row.model == nil {
                            Text("\(switching.provider.title)’s own choice").font(style.system(.caption)).foregroundStyle(style.muted)
                        }
                    }
                    Spacer(minLength: 4)
                }
                .padding(.horizontal, 12).padding(.vertical, 8).frame(maxWidth: .infinity, minHeight: max(style.target, style.pt(52)), alignment: .leading)
                .overlay { ring(.other(index)) }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain).disabled(!connected || switching.blocked != nil)
            .accessibilityIdentifier("chat-switch-\(switching.provider.rawValue)-\(row.id)")
            .accessibilityLabel("\(switching.provider.title) \(row.title)")
            .accessibilityHint(switching.blocked ?? "Continues this conversation with \(switching.provider.title)")
        }
    }

    private func loadModels() async {
        guard !loadingModels else { return }
        let chat = info
        model.prepareChatCatalogue(chat)
        let token = UUID()
        catalogueRequest = token
        let generation = model.generation
        let project = model.projectID
        let revision = conversation.modelCatalogueRevision
        loadingModels = true; modelsError = nil
        let timeout = Task {
            try? await Task.sleep(for: RemoteModel.catalogueLoadingLimit)
            guard !Task.isCancelled, catalogueRequest == token, model.generation == generation, model.projectID == project else { return }
            catalogueRequest = UUID(); loadingModels = false
            modelsError = "Live model lookup took too long. Use the fallback list or Retry."
        }
        defer { timeout.cancel(); if catalogueRequest == token { loadingModels = false } }
        do {
            let catalogue = try await model.availableChatModels(provider: chat.provider, chat: chat)
            guard catalogueRequest == token, model.generation == generation, model.projectID == project,
                  conversation.modelCatalogueRevision == revision else { return }
            conversation.modelCatalogue = catalogue
            conversation.modelCatalogueSource = .live
            model.rememberModelCatalogue(catalogue, provider: chat.provider, chat: conversation.transcript.info ?? chat)
        }
        catch is CancellationError { }
        catch {
            guard catalogueRequest == token, model.generation == generation, model.projectID == project,
                  conversation.modelCatalogueRevision == revision else { return }
            modelsError = ChatControlError.from(error, operation: .list).message
        }
    }

    private func sectionLabel(_ text: String) -> some View {
        Text(style.cased(text)).font(style.system(.caption, weight: .bold)).foregroundStyle(style.muted)
            .padding(.horizontal, 12).padding(.top, 12).padding(.bottom, 4).accessibilityAddTraits(.isHeader)
    }
    private func messageRow(_ text: String, icon: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: icon).accessibilityHidden(true)
            Text(text).font(style.system(.footnote)).fixedSize(horizontal: false, vertical: true)
        }
        .foregroundStyle(style.warning).padding(12).frame(maxWidth: .infinity, alignment: .leading)
        .background(style.warning.opacity(0.1), in: style.block())
        .padding(.horizontal, style.native ? 8 : 0).padding(.top, style.native ? 8 : 0)
        .accessibilityElement(children: .combine).accessibilityAddTraits(.updatesFrequently)
    }

    private func modelRow(_ index: Int, _ option: ChatModelOption, _ choices: ChatModelChoices) -> some View {
        let selected = option.id == choices.current?.id
        return Button { place(.model(index)); send(.model(option.id)) } label: {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: selected ? "largecircle.fill.circle" : "circle").frame(width: style.pt(22), height: style.pt(22)).foregroundStyle(selected ? style.accent : style.muted)
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(option.name).font(style.face(14, bold: selected, relativeTo: .body)).lineLimit(1).truncationMode(.tail)
                        if option.isDefault { defaultBadge }
                        if option.supportsFast { Image(systemName: "bolt.fill").font(.system(size: style.pt(10))).foregroundStyle(style.muted).accessibilityHidden(true) }
                    }
                    if !option.description.isEmpty {
                        Text(option.description).font(style.system(.caption)).foregroundStyle(style.muted).lineLimit(2).fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 4)
            }
            .padding(.horizontal, 12).padding(.vertical, 8).frame(maxWidth: .infinity, minHeight: max(style.target, style.pt(52)), alignment: .leading)
            .overlay { ring(.model(index)) }
            .desktopRowFill(style, selected: selected)
        }
        .buttonStyle(.plain).disabled(!connected)
        .accessibilityIdentifier("chat-model-\(option.id)")
        .accessibilityLabel(option.name + (option.isDefault ? ", default" : "") + (option.supportsFast ? ", has Fast" : ""))
        .accessibilityHint(option.description)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    /// The default model's tag: a framed label in capitals in the terminal look; a small gray capsule in Native.
    @ViewBuilder private var defaultBadge: some View {
        if style.native {
            Text("Default").font(style.face(9, bold: true, relativeTo: .caption2)).foregroundStyle(style.muted)
                .padding(.horizontal, 6).padding(.vertical, 1).background(style.active, in: Capsule())
        } else {
            Text(style.cased("Default")).font(style.mono(9, bold: true, relativeTo: .caption2)).foregroundStyle(style.muted)
                .padding(.horizontal, 4).padding(.vertical, 1).overlay(Rectangle().stroke(style.divider, lineWidth: 1))
        }
    }

    private var footer: some View {
        VStack(spacing: 6) {
            DesktopRule()
            if model.keyboard.hardware.isAttached || keyboardInUse {
                Text("↑↓ move   ←→ effort   ⏎ choose   ⎋ close").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted)
                    .lineLimit(1).minimumScaleFactor(0.7).padding(.horizontal, 12).accessibilityHidden(true)
            }
            Button(action: close) { Text("Done").font(style.face(14, bold: true, relativeTo: .headline)).frame(maxWidth: .infinity, minHeight: max(style.target, style.pt(48))) }
                .buttonStyle(DesktopButtonStyle(prominent: true)).padding(.horizontal, 12).padding(.bottom, 8)
        }
        .background(style.glass ? style.surface : style.panel)
    }

    // MARK: Ring and keys

    private var ringedEffort: Int? { keyboardInUse && cursor.stop == .effort ? cursor.effortIndex : nil }
    /// The ring around a row, shown while a keyboard drives the sheet. The efforts show theirs on a segment instead.
    private func ring(_ stop: ChatModelCursor.Stop) -> some View {
        DesktopRing(shown: keyboardInUse && cursor.stop == stop)
    }
    /// A tap puts the ring where the finger is, so touch and keyboard agree.
    private func place(_ stop: ChatModelCursor.Stop, effort: Int? = nil) {
        keyboardInUse = false
        cursor.place(stop, effort: effort, in: choices)
    }
    private func send(_ change: ChatModelChoices.Change) {
        if case .provider(let id) = change { switchTo(id); return }
        Task { await model.chooseChatModel(info, change) }
    }
    /// Goes on with the other provider; the sheet goes once the Mac took it, and the chat says the rest.
    private func switchTo(_ id: String?) {
        Task { if await model.switchChatProvider(info, model: id) == nil { close() } }
    }
    private func press(_ key: ChatModelCursor.Key) {
        keyboardInUse = true
        let current = choices
        var next = cursor
        let change = next.handle(key, in: current)
        cursor = next
        guard let change else { return }
        if case .provider = change { send(change); return }
        // Return on what is chosen already is "that's it": the sheet goes. Fast always flips.
        if current.configuration(for: change) != nil { send(change) } else if key == .return { close() }
    }
    private var keyActions: [KeyAction] {
        func key(_ input: String, _ chord: ChatModelCursor.Key, flags: UIKeyModifierFlags = [], title: String? = nil) -> KeyAction {
            KeyAction(input: input, modifiers: flags, title: title) { press(chord) }
        }
        return [
            key(UIKeyCommand.inputUpArrow, .up, title: "Previous"), key(UIKeyCommand.inputDownArrow, .down, title: "Next"),
            key(UIKeyCommand.inputLeftArrow, .left, title: "Previous effort"), key(UIKeyCommand.inputRightArrow, .right, title: "Next effort"),
            key("\t", .tab), key("\t", .backTab, flags: .shift),
            key("p", .up, flags: .control), key("n", .down, flags: .control),
            key("\r", .return, title: "Choose"), key(" ", .space),
            KeyAction(input: UIKeyCommand.inputEscape, title: "Close") { close() },
            // The Clicks keyboard has no Escape: ⌘. closes, and ⌘M, which opened the sheet, closes it again.
            KeyAction(input: ".", modifiers: .command) { close() }, KeyAction(input: "m", modifiers: .command) { close() }
        ]
    }
}
