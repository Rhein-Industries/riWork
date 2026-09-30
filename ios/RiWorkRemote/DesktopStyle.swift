import SwiftUI
import UIKit

/// Derived from src/theme.rs: Gruvbox Light in light appearance, RiWork in dark.
/// The v1 relay does not export desktop theme settings.
enum DesktopStyle {
    private static func dynamic(_ light: UInt32, _ dark: UInt32) -> UIColor {
        UIColor { traits in
            let rgb = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(red: CGFloat((rgb >> 16) & 255) / 255,
                           green: CGFloat((rgb >> 8) & 255) / 255,
                           blue: CGFloat(rgb & 255) / 255, alpha: 1)
        }
    }
    private static func color(_ light: UInt32, _ dark: UInt32) -> Color { Color(uiColor: dynamic(light, dark)) }
    // UIKit-backed controls need the dynamic UIColor itself; converting a SwiftUI Color back would freeze one appearance.
    static let textUI = dynamic(0x3c3836, 0xd3e1e6)
    static let mutedUI = dynamic(0x756f5e, 0x8fa6ae)
    static let accentUI = dynamic(0x427b58, 0x55e6dc)
    static let backgroundUI = dynamic(0xfbf1c7, 0x090d14)
    static let panelUI = dynamic(0xf4ebc2, 0x101720)
    static let activeUI = dynamic(0xede3bc, 0x14212a)
    static let dividerUI = dynamic(0xd5ccb6, 0x253c45)
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
    /// Terminal chrome: 40-point targets instead of 44 so the shell gets the room.
    var compact = false
    @Environment(\.isEnabled) private var isEnabled
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.custom("Menlo", size: 12, relativeTo: .subheadline))
            .foregroundStyle(prominent ? DesktopStyle.accent : DesktopStyle.text)
            .padding(.horizontal, compact ? 6 : 10).frame(minWidth: compact ? 40 : 44, minHeight: compact ? 40 : 44)
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
    /// The terminal screen's header: shorter, with 40-point controls.
    var compact = false
    var onDoubleTap: (() -> Void)?
    @ViewBuilder var actions: () -> Actions
    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: compact ? 2 : 4) {
                if let back { Button("Back", systemImage: "chevron.left", action: back).labelStyle(.iconOnly).buttonStyle(DesktopButtonStyle(compact: compact)) }
                Text(title).font(.custom("Menlo-Bold", size: 13, relativeTo: .headline)).lineLimit(1).accessibilityAddTraits(.isHeader)
                Spacer(minLength: 4)
                actions().buttonStyle(DesktopButtonStyle(compact: compact))
            }.padding(.horizontal, compact ? 4 : 8).frame(minHeight: compact ? 40 : 44).background(DesktopStyle.panel)
                .contentShape(Rectangle())
                .simultaneousGesture(TapGesture(count: 2).onEnded { onDoubleTap?() }, including: onDoubleTap == nil ? .none : .all)
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
