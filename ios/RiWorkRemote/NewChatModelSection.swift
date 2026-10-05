import SwiftUI
import RiWorkCore

/// The model of a new chat, in the New terminal sheet. The phone has no list of models before a chat exists, so it offers what it can know:
/// **Default** (the Mac's own choice) and the model last used with this provider, with the effort and Fast it was used with. Choosing the
/// last model shows its efforts and Fast; what is chosen is sent in `chat.create` and remembered for the next chat. The keyboard's ring is
/// the sheet's own (`NewTerminalForm`): ↑ ↓ choose on the model rows and on the efforts, space flips Fast.
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
                if let model = choice.model { row(title: model.shortName, detail: "Last used", selected: choice.usesModel, last: true) }
            }
            .accessibilityElement(children: .contain).accessibilityLabel("Model")
            if choice.model == nil {
                Text("Choose a model inside the chat; it is offered here next time.").font(style.system(.caption)).foregroundStyle(style.muted)
                    .padding(.horizontal, 12).padding(.top, 4).fixedSize(horizontal: false, vertical: true)
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
