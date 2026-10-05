import SwiftUI
import RiWorkCore

/// New chats use the same provider Models events as the in-chat picker, read from an existing chat in this project.
/// Default remains available when a provider has not published its catalogue yet. Effort and Fast use the shared controls.
struct NewChatModelSection: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var sheet: NewTerminalSheetModel

    var body: some View {
        let choice = sheet.form.chatChoice ?? NewChatChoice()
        VStack(alignment: .leading, spacing: 0) {
            Text(style.cased("Model")).font(style.system(.caption, weight: .bold)).foregroundStyle(style.muted)
                .padding(.horizontal, 12).padding(.top, 12).padding(.bottom, 4).accessibilityAddTraits(.isHeader)
            VStack(spacing: 0) {
                row(title: "Default", detail: "The Mac’s choice", selected: !choice.usesModel, last: false)
                let models = sheet.form.kind.chatProvider.flatMap { sheet.form.chatModels[$0] } ?? []
                ForEach(models) { option in
                    Button { sheet.chooseChatModel(option) } label: {
                        HStack(spacing: 10) {
                            Image(systemName: choice.chosen?.id == option.id ? "largecircle.fill.circle" : "circle")
                            Text(option.name).font(style.face(14, relativeTo: .body))
                            Spacer(minLength: 4)
                            if option.isDefault { Text("Default").font(style.system(.caption)).foregroundStyle(style.muted) }
                        }.padding(12).frame(minHeight: style.pt(48)).contentShape(Rectangle())
                    }.buttonStyle(.plain).disabled(sheet.busy)
                        .overlay { if choice.chosen?.id == option.id { ring(.chatModel) } }
                        .accessibilityIdentifier("new-chat-model-\(option.id)")
                        .accessibilityAddTraits(choice.chosen?.id == option.id ? .isSelected : [])
                }
                if models.isEmpty, let model = choice.model { row(title: model.name, detail: "Last used", selected: choice.usesModel, last: true) }
            }
            .accessibilityElement(children: .contain).accessibilityLabel("Model")
            if let provider = sheet.form.kind.chatProvider, let label = sheet.chatModelsSources[provider]?.label {
                Text(label).font(style.system(.caption)).foregroundStyle(style.warning).padding(12)
            }
            if sheet.loadingChatModels { ProgressView("Loading models…").padding(12) }
            if let error = sheet.chatModelsError {
                Text(error).font(style.system(.caption)).foregroundStyle(style.warning).padding(12)
            }
            if let provider = sheet.form.kind.chatProvider, sheet.chatModelsSources[provider] != .live || sheet.chatModelsError != nil {
                Button("Retry live models") { Task { await sheet.loadChatModels() } }.padding(.horizontal, 12)
                    .disabled(sheet.loadingChatModels || sheet.busy)
            }
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
        .task(id: sheet.form.kind.chatProvider) { await sheet.loadChatModels() }
    }

    private func row(title: String, detail: String, selected: Bool, last: Bool) -> some View {
        Button { sheet.chooseChatModel(last: last) } label: {
            HStack(spacing: 10) {
                Image(systemName: selected ? "largecircle.fill.circle" : "circle").frame(width: style.pt(22)).foregroundStyle(selected ? style.accent : style.muted)
                Text(title).font(style.face(14, bold: selected, relativeTo: .body)).lineLimit(1).truncationMode(.tail)
                Spacer(minLength: 4)
                Text(detail).font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted)
            }
            .padding(.horizontal, 12).frame(maxWidth: .infinity, minHeight: style.pt(48), alignment: .leading)
            .overlay { if selected { ring(.chatModel) } }
            .desktopRowFill(style, selected: selected)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(last ? "new-chat-model-last" : "new-chat-model-default")
        .accessibilityLabel(title).accessibilityHint(detail)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func ringed(_ field: NewTerminalForm.Field) -> Bool { sheet.keyboardInUse && sheet.form.focus == field }
    private func ring(_ field: NewTerminalForm.Field) -> some View {
        DesktopRing(shown: ringed(field))
    }
}
