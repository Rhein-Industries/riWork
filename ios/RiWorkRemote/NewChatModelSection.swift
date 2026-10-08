import SwiftUI
import RiWorkCore

/// The model of a new chat: one list for both providers, Codex and Claude each under its own heading with its default first. The model
/// chosen decides which provider the chat runs (`NewTerminalForm.chatRows`). The lists are the providers' own Models events, read from saved
/// chats (`chat.models`, or the chats of this project on an older Mac); each provider's default stays on offer while its list is not known.
/// Effort and Fast use the shared controls.
struct NewChatModelSection: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var sheet: NewTerminalSheetModel

    var body: some View {
        let choice = sheet.form.chatChoice ?? NewChatChoice()
        let chosen = sheet.form.chosenChatRow
        VStack(alignment: .leading, spacing: 0) {
            Text(style.cased("Model")).font(style.system(.caption, weight: .bold)).foregroundStyle(style.muted)
                .padding(.horizontal, 12).padding(.top, 12).padding(.bottom, 4).accessibilityAddTraits(.isHeader)
            ForEach(ChatProvider.allCases, id: \.self) { provider in
                providerSection(provider, rows: sheet.form.chatRows.filter { $0.provider == provider }, chosen: chosen)
            }
            if sheet.loadingChatModels { ProgressView("Loading models…").padding(12) }
            if !choice.efforts.isEmpty {
                ChatEffortSegments(efforts: choice.efforts, selected: choice.selectedEffort, ringed: ringed(.chatEffort) ? choice.selectedEffort.flatMap { choice.efforts.firstIndex(of: $0) } : nil) { _, effort in
                    sheet.chooseChatEffort(effort)
                }
                .padding(.horizontal, 12).padding(.top, 8)
            }
            if choice.showsFast {
                ChatFastToggle(isOn: Binding(get: { choice.fastIsOn }, set: { sheet.setChatFast($0) }))
                    .padding(.horizontal, 12).padding(.vertical, 6).frame(minHeight: style.pt(48))
                    .overlay { ring(.chatFast) }
                    .padding(.top, 4)
            }
        }
        .task(id: sheet.form.kind.isChat) { if sheet.form.kind.isChat { await sheet.loadChatModels() } }
    }

    @ViewBuilder private func providerSection(_ provider: ChatProvider, rows: [NewChatRow], chosen: NewChatRow?) -> some View {
        HStack(spacing: 6) {
            Image(systemName: provider.glyph).foregroundStyle(style.muted).accessibilityHidden(true)
            Text(provider.title).font(style.system(.caption, weight: .semibold)).foregroundStyle(style.muted)
        }
        .padding(.horizontal, 12).padding(.top, 8).padding(.bottom, 2).accessibilityAddTraits(.isHeader)
        VStack(spacing: 0) {
            ForEach(rows) { row in rowView(row, selected: chosen?.id == row.id) }
        }
        .accessibilityElement(children: .contain).accessibilityLabel("\(provider.title) models")
        if let label = sheet.chatModelsSources[provider]?.label {
            Text(label).font(style.system(.caption)).foregroundStyle(style.warning).padding(.horizontal, 12).padding(.vertical, 4)
        }
        if let error = sheet.chatModelsErrors[provider] {
            Text(error).font(style.system(.caption)).foregroundStyle(style.warning).padding(.horizontal, 12).padding(.vertical, 4)
        }
        if sheet.chatModelsSources[provider] != .live || sheet.chatModelsErrors[provider] != nil {
            Button("Retry live \(provider.title) models") { Task { await sheet.loadChatModels(provider) } }.padding(.horizontal, 12).padding(.vertical, 4)
                .disabled(sheet.loadingProviders.contains(provider) || sheet.busy)
        }
    }

    private func rowView(_ row: NewChatRow, selected: Bool) -> some View {
        let (title, detail, identifier): (String, String, String) = switch row {
        case .providerDefault(let provider): ("Default", "The Mac’s choice", "new-chat-model-\(provider.rawValue)-default")
        case .last(let provider, let option): (option.name, "Last used", "new-chat-model-\(provider.rawValue)-last")
        case .model(let provider, let option): (option.name, option.isDefault ? "Default" : "", "new-chat-model-\(provider.rawValue)-\(option.id)")
        }
        return Button { sheet.chooseChatRow(row) } label: {
            HStack(spacing: 10) {
                Image(systemName: selected ? "largecircle.fill.circle" : "circle").frame(width: style.pt(22)).foregroundStyle(selected ? style.accent : style.muted)
                Text(title).font(style.face(14, bold: selected, relativeTo: .body)).lineLimit(1).truncationMode(.tail)
                Spacer(minLength: 4)
                if !detail.isEmpty { Text(detail).font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted) }
            }
            .padding(.horizontal, 12).frame(maxWidth: .infinity, minHeight: style.pt(48), alignment: .leading)
            .overlay { if selected { ring(.chatModel) } }
            .desktopRowFill(style, selected: selected)
        }
        .buttonStyle(.plain).disabled(sheet.busy)
        .accessibilityIdentifier(identifier)
        .accessibilityLabel("\(row.provider.title), \(title)").accessibilityHint(detail)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func ringed(_ field: NewTerminalForm.Field) -> Bool { sheet.keyboardInUse && sheet.form.focus == field }
    private func ring(_ field: NewTerminalForm.Field) -> some View {
        DesktopRing(shown: ringed(field))
    }
}
