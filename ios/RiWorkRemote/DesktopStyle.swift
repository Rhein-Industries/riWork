import SwiftUI
import UIKit
import RiWorkCore

/// The colors the app draws with, for SwiftUI (`Color`) and for UIKit controls (`…UI`).
///
/// Built from a `DesktopTheme` (RiWorkCore): the desktop's synced palette when one is known, otherwise the built-in
/// Gruvbox Light / RiWork pair. Views read it from the environment (`@Environment(\.desktopStyle)`), so a new palette
/// re-colors them in place, without rebuilding anything or touching focus. UIKit-backed controls get the dynamic
/// `UIColor` itself; converting a SwiftUI `Color` back would freeze one appearance.
struct DesktopStyle: Equatable, @unchecked Sendable {
    let theme: DesktopTheme
    let textUI, mutedUI, accentUI, magentaUI, goldUI, errorUI: UIColor
    let backgroundUI, panelUI, activeUI, dividerUI: UIColor
    let terminalBackgroundUI, terminalForegroundUI, terminalCursorUI: UIColor

    init(_ theme: DesktopTheme) {
        self.theme = theme
        func ui(_ color: ThemeColor) -> UIColor {
            if color.isFixed { return Self.uiColor(color.light) }
            let light = Self.uiColor(color.light), dark = Self.uiColor(color.dark)
            return UIColor { $0.userInterfaceStyle == .dark ? dark : light }
        }
        textUI = ui(theme.text); mutedUI = ui(theme.muted); accentUI = ui(theme.accent); magentaUI = ui(theme.magenta)
        goldUI = ui(theme.gold); errorUI = ui(theme.error)
        backgroundUI = ui(theme.background); panelUI = ui(theme.panel); activeUI = ui(theme.active); dividerUI = ui(theme.divider)
        terminalBackgroundUI = ui(theme.terminalBackground); terminalForegroundUI = ui(theme.terminalForeground); terminalCursorUI = ui(theme.terminalCursor)
    }
    static func uiColor(_ rgb: RGB) -> UIColor {
        UIColor(red: CGFloat(rgb.red) / 255, green: CGFloat(rgb.green) / 255, blue: CGFloat(rgb.blue) / 255, alpha: 1)
    }
    static func == (lhs: DesktopStyle, rhs: DesktopStyle) -> Bool { lhs.theme == rhs.theme }
    static let builtIn = DesktopStyle(.builtIn)

    var text: Color { Color(uiColor: textUI) }
    var muted: Color { Color(uiColor: mutedUI) }
    /// The desktop's cyan.
    var accent: Color { Color(uiColor: accentUI) }
    /// The desktop's magenta: the secondary accent.
    var magenta: Color { Color(uiColor: magentaUI) }
    /// The desktop's gold: warnings and things that need attention.
    var gold: Color { Color(uiColor: goldUI) }
    var warning: Color { gold }
    var error: Color { Color(uiColor: errorUI) }
    var background: Color { Color(uiColor: backgroundUI) }
    var panel: Color { Color(uiColor: panelUI) }
    var active: Color { Color(uiColor: activeUI) }
    var divider: Color { Color(uiColor: dividerUI) }
    var terminalBackground: Color { Color(uiColor: terminalBackgroundUI) }
    var terminalForeground: Color { Color(uiColor: terminalForegroundUI) }
    var terminalCursor: Color { Color(uiColor: terminalCursorUI) }
    /// The desktop's light or dark side, once synced; nil while built in, so the phone's own setting applies.
    /// It also decides the status bar style and how system controls (menus, alerts, keyboards) draw.
    var colorScheme: ColorScheme? { theme.dark.map { $0 ? .dark : .light } }
}

extension EnvironmentValues {
    @Entry var desktopStyle = DesktopStyle.builtIn
}

/// Applied once at the root: the style in the environment plus the defaults every screen shares.
private struct DesktopThemed: ViewModifier {
    let style: DesktopStyle
    func body(content: Content) -> some View {
        content
            .environment(\.desktopStyle, style)
            .tint(style.accent)
            .foregroundStyle(style.text)
            .font(.custom("Menlo", size: 13, relativeTo: .body))
            .buttonStyle(DesktopButtonStyle())
            .preferredColorScheme(style.colorScheme)
    }
}
extension View {
    func desktopThemed(_ style: DesktopStyle) -> some View { modifier(DesktopThemed(style: style)) }
}

struct DesktopButtonStyle: ButtonStyle {
    @Environment(\.desktopStyle) private var style
    var prominent = false
    /// Terminal chrome: 40-point targets instead of 44 so the shell gets the room.
    var compact = false
    @Environment(\.isEnabled) private var isEnabled
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.custom("Menlo", size: 12, relativeTo: .subheadline))
            .foregroundStyle(prominent ? style.accent : style.text)
            .padding(.horizontal, compact ? 6 : 10).frame(minWidth: compact ? 40 : 44, minHeight: compact ? 40 : 44)
            .background(configuration.isPressed ? style.active : (prominent ? style.active : .clear))
            .overlay(alignment: .bottom) { if prominent { Rectangle().fill(style.accent).frame(height: 1) } }
            .contentShape(Rectangle()).opacity(isEnabled ? 1 : 0.45)
    }
}

struct DesktopRule: View {
    @Environment(\.desktopStyle) private var style
    var body: some View { Rectangle().fill(style.divider).frame(height: 1).accessibilityHidden(true) }
}

struct WorkspaceBar<Actions: View>: View {
    @Environment(\.desktopStyle) private var style
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
            }.padding(.horizontal, compact ? 4 : 8).frame(minHeight: compact ? 40 : 44).background(style.panel)
                .contentShape(Rectangle())
                .simultaneousGesture(TapGesture(count: 2).onEnded { onDoubleTap?() }, including: onDoubleTap == nil ? .none : .all)
            DesktopRule()
        }
    }
}

struct DesktopField: ViewModifier {
    @Environment(\.desktopStyle) private var style
    func body(content: Content) -> some View {
        content.textFieldStyle(.plain).padding(8)
            .background(style.background)
            .overlay(RoundedRectangle(cornerRadius: 3).stroke(style.divider, lineWidth: 1))
    }
}
