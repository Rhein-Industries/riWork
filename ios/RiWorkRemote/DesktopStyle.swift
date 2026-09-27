import SwiftUI
import UIKit

/// Derived from src/theme.rs: Gruvbox Light in light appearance, RiWork in dark.
/// The v1 relay does not export desktop theme settings.
enum DesktopStyle {
    private static func color(_ light: UInt32, _ dark: UInt32) -> Color {
        Color(uiColor: UIColor { traits in
            let rgb = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(red: CGFloat((rgb >> 16) & 255) / 255,
                           green: CGFloat((rgb >> 8) & 255) / 255,
                           blue: CGFloat(rgb & 255) / 255, alpha: 1)
        })
    }
    static let background = color(0xfbf1c7, 0x090d14)
    static let panel = color(0xf4ebc2, 0x101720)
    static let active = color(0xede3bc, 0x14212a)
    static let divider = color(0xd5ccb6, 0x253c45)
    static let text = color(0x3c3836, 0xd3e1e6)
    static let muted = color(0x756f5e, 0x8fa6ae)
    static let accent = color(0x427b58, 0x55e6dc)
    static let warning = color(0x9d5015, 0xf4bf75)
    static let error = color(0x9d0006, 0xf0738b)
}

struct DesktopButtonStyle: ButtonStyle {
    var prominent = false
    @Environment(\.isEnabled) private var isEnabled
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.custom("Menlo", size: 12, relativeTo: .subheadline))
            .foregroundStyle(prominent ? DesktopStyle.accent : DesktopStyle.text)
            .padding(.horizontal, 10).frame(minWidth: 44, minHeight: 44)
            .background(configuration.isPressed ? DesktopStyle.active : (prominent ? DesktopStyle.active : .clear))
            .overlay(alignment: .bottom) { if prominent { Rectangle().fill(DesktopStyle.accent).frame(height: 1) } }
            .contentShape(Rectangle()).opacity(isEnabled ? 1 : 0.45)
    }
}

struct DesktopRule: View {
    var body: some View { Rectangle().fill(DesktopStyle.divider).frame(height: 1).accessibilityHidden(true) }
}

struct WorkspaceBar<Actions: View>: View {
    let title: String
    var back: (() -> Void)?
    @ViewBuilder var actions: () -> Actions
    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 4) {
                if let back { Button("Back", systemImage: "chevron.left", action: back).labelStyle(.iconOnly).buttonStyle(DesktopButtonStyle()) }
                Text(title).font(.custom("Menlo-Bold", size: 13, relativeTo: .headline)).lineLimit(1).accessibilityAddTraits(.isHeader)
                Spacer(minLength: 4)
                actions()
            }.padding(.horizontal, 8).frame(minHeight: 44).background(DesktopStyle.panel)
            DesktopRule()
        }
    }
}

struct DesktopField: ViewModifier {
    func body(content: Content) -> some View {
        content.textFieldStyle(.plain).padding(8)
            .background(DesktopStyle.background)
            .overlay(RoundedRectangle(cornerRadius: 3).stroke(DesktopStyle.divider, lineWidth: 1))
    }
}
