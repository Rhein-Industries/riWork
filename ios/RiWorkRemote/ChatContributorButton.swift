import SwiftUI
import RiWorkCore

/// Beside the composer, with a full touch target and the host's current mode always in the label.
struct ChatContributorButton: View {
    @Environment(\.desktopStyle) private var style
    let choice: ChatContributorChoice
    let connected: Bool
    let changing: Bool
    let toggle: () -> Void

    var body: some View {
        HStack {
            Button(action: toggle) {
                HStack(spacing: 6) {
                    if changing { ProgressView().controlSize(.small) }
                    else { Image(systemName: choice.mode == .on ? "hand.raised.fill" : "hand.raised").accessibilityHidden(true) }
                    Text(choice.title).font(style.system(.footnote, weight: .medium))
                }
                .foregroundStyle(choice.mode == .on ? style.accent : style.muted)
                .padding(.horizontal, 6).frame(minHeight: style.target).contentShape(Rectangle())
            }
            .buttonStyle(.plain).disabled(!connected || changing)
            .accessibilityIdentifier("chat-contributor-mode")
            .accessibilityLabel(choice.title)
            .accessibilityValue(changing ? "Refreshing from the Mac" : "")
            .accessibilityHint(choice.detail)
            .chatLayoutProbe("contributor")
            Spacer(minLength: 0)
        }
        .padding(.horizontal, ChatComposer.fieldMargin)
        .background(style.background)
    }
}
