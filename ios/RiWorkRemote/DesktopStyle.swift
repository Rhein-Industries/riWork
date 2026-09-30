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
    /// The interface size (`InterfaceScale`): a factor for fonts and touch targets, so headers, lists, the key bar and buttons
    /// grow or shrink together. Dynamic Type is not touched; it still applies on top.
    let scale: Double
    let textUI, mutedUI, accentUI, magentaUI, goldUI, errorUI: UIColor
    let backgroundUI, panelUI, activeUI, dividerUI: UIColor
    let terminalBackgroundUI, terminalForegroundUI, terminalCursorUI: UIColor

    init(_ theme: DesktopTheme, scale: Double = InterfaceScale.standard) {
        self.theme = theme
        self.scale = InterfaceScale.clamped(scale)
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
    static func == (lhs: DesktopStyle, rhs: DesktopStyle) -> Bool { lhs.theme == rhs.theme && lhs.scale == rhs.scale }
    static let builtIn = DesktopStyle(.builtIn)

    // MARK: Metrics

    /// A size in points at the current interface scale, on whole points.
    func pt(_ points: CGFloat) -> CGFloat { CGFloat(InterfaceScale.scaled(Double(points), by: scale)) }
    /// Menlo, the app's text face, at the scale. `relativeTo` keeps it following Dynamic Type as before.
    func mono(_ size: CGFloat, bold: Bool = false, relativeTo textStyle: Font.TextStyle = .body) -> Font {
        .custom(bold ? "Menlo-Bold" : "Menlo", size: size * CGFloat(scale), relativeTo: textStyle)
    }
    /// The system face for secondary text (captions, footnotes) at the scale.
    func system(_ textStyle: Font.TextStyle, weight: Font.Weight = .regular) -> Font {
        let base: CGFloat = switch textStyle {
        case .caption2: 11
        case .caption: 12
        case .footnote: 13
        case .subheadline: 15
        case .callout: 16
        case .headline, .body: 17
        default: 17
        }
        let metrics = UIFontMetrics(forTextStyle: Self.uiTextStyle(textStyle))
        return .system(size: metrics.scaledValue(for: base) * CGFloat(scale), weight: weight)
    }
    private static func uiTextStyle(_ style: Font.TextStyle) -> UIFont.TextStyle {
        switch style {
        case .caption2: .caption2
        case .caption: .caption1
        case .footnote: .footnote
        case .subheadline: .subheadline
        case .callout: .callout
        case .headline: .headline
        default: .body
        }
    }
    /// A UIKit font at the scale (key bar, text fields).
    func uiFont(_ name: String, size: CGFloat) -> UIFont {
        UIFont(name: name, size: size * CGFloat(scale)) ?? .monospacedSystemFont(ofSize: size * CGFloat(scale), weight: .regular)
    }

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
            .font(style.mono(13))
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
            .font(style.mono(12, relativeTo: .subheadline))
            .foregroundStyle(prominent ? style.accent : style.text)
            .padding(.horizontal, style.pt(compact ? 6 : 10)).frame(minWidth: style.pt(compact ? 40 : 44), minHeight: style.pt(compact ? 40 : 44))
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
                Text(title).font(style.mono(13, bold: true, relativeTo: .headline)).lineLimit(1).accessibilityAddTraits(.isHeader)
                Spacer(minLength: 4)
                actions().buttonStyle(DesktopButtonStyle(compact: compact))
            }.padding(.horizontal, compact ? 4 : 8).frame(minHeight: style.pt(compact ? 40 : 44)).background(style.panel)
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
