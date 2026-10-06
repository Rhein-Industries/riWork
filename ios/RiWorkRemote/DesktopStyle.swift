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

    // MARK: Native

    /// The desktop uses its Native skin: the interface is set in the system face, in sentence case, on glass where iOS has it.
    /// Otherwise (another theme, a desktop from before the flag, no desktop) the terminal look, exactly as before.
    var native: Bool { theme.native }
    /// Drawn with iOS 26's Liquid Glass: Native, on a system that has it.
    var glass: Bool {
        if #available(iOS 26, *) { return native }
        return false
    }
    /// Points the system face is set larger than Menlo at the same nominal size, so that it reads at the same size.
    private static let nativeLift: CGFloat = 2
    /// The interface face: Menlo in the terminal look (`mono`), SF Pro in Native. Terminal content keeps `mono` whatever the skin.
    func face(_ size: CGFloat, bold: Bool = false, relativeTo textStyle: Font.TextStyle = .body) -> Font {
        guard native else { return mono(size, bold: bold, relativeTo: textStyle) }
        let metrics = UIFontMetrics(forTextStyle: Self.uiTextStyle(textStyle))
        return .system(size: metrics.scaledValue(for: size + Self.nativeLift) * CGFloat(scale), weight: bold ? .semibold : .regular)
    }
    /// `face` for UIKit (key bar, text fields), unscaled for Dynamic Type like `uiFont`.
    func uiFace(size: CGFloat) -> UIFont {
        native ? .systemFont(ofSize: (size + Self.nativeLift) * CGFloat(scale)) : uiFont("Menlo", size: size)
    }
    /// A label or title as the skin writes it. Written in sentence case; the terminal look shows it in capitals, as it always has.
    func cased(_ text: String) -> String { native ? text : text.uppercased() }
    /// The desktop's dictation setting: the mics (chat composer, key bar, line composer) are shown only while it is on. Off by
    /// default and with a desktop that predates the setting.
    var mic: Bool { theme.mic }
    /// What a bar or a sheet is painted with: nothing on glass, so the system's glass shows through, else the background.
    var surface: Color { glass ? .clear : background }

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
            .font(style.face(13))
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
    /// Terminal and chat chrome: less padding beside the label. The target is still at least 44 points either way.
    var compact = false
    @Environment(\.isEnabled) private var isEnabled
    func makeBody(configuration: Configuration) -> some View {
        if style.native { nativeBody(configuration) } else { terminalBody(configuration) }
    }
    /// Native: no fills or underlines. A prominent button is a capsule in the primary color (on glass on iOS 26), and a press dims.
    @ViewBuilder private func nativeBody(_ configuration: Configuration) -> some View {
        let label = configuration.label
            .font(style.face(12, bold: prominent, relativeTo: .subheadline))
            .foregroundStyle(prominent ? style.background : style.text)
            .padding(.horizontal, style.pt(compact ? 6 : (prominent ? 16 : 10))).frame(minWidth: style.pt(44), minHeight: style.pt(44))
        Group {
            if !prominent {
                label
            } else if #available(iOS 26, *) {
                label.glassEffect(.regular.tint(style.accent).interactive(), in: .capsule)
            } else {
                label.background(Capsule().fill(style.accent))
            }
        }
        .contentShape(Capsule()).opacity(isEnabled ? (configuration.isPressed ? 0.6 : 1) : 0.45)
    }
    private func terminalBody(_ configuration: Configuration) -> some View {
        configuration.label
            .font(style.mono(12, relativeTo: .subheadline))
            .foregroundStyle(prominent ? style.accent : style.text)
            .padding(.horizontal, style.pt(compact ? 6 : 10)).frame(minWidth: style.pt(44), minHeight: style.pt(44))
            .background(configuration.isPressed ? style.active : (prominent ? style.active : .clear))
            .overlay(alignment: .bottom) { if prominent { Rectangle().fill(style.accent).frame(height: 1) } }
            .contentShape(Rectangle()).opacity(isEnabled ? 1 : 0.45)
    }
}

/// A plain button whose whole 44-point square (at the interface size) is the target, not only its glyph: a frame put around a plain
/// button from outside makes it bigger without making more of it tappable.
struct TargetButtonStyle: ButtonStyle {
    @Environment(\.desktopStyle) private var style
    @Environment(\.isEnabled) private var isEnabled
    /// In the tint color, as a borderless system button is; otherwise in the surrounding text color, as a plain one is.
    var tinted = false
    /// Dims while disabled; off for a button whose label already draws its disabled look.
    var dims = true
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(tinted ? AnyShapeStyle(.tint) : AnyShapeStyle(.foreground))
            .frame(minWidth: style.pt(44), minHeight: style.pt(44)).contentShape(Rectangle())
            .opacity((isEnabled || !dims ? 1 : 0.45) * (configuration.isPressed ? 0.6 : 1))
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
        let _ = Perf.count("body.WorkspaceBar")
        if style.native { nativeBar } else { terminalBar }
    }
    /// Native: the title in the system face on the screen's own surface, with the controls grouped on glass (iOS 26) as a
    /// navigation bar draws them, and no rule under it while on glass.
    private var nativeBar: some View {
        VStack(spacing: 0) {
            HStack(spacing: compact ? 4 : 8) {
                if let back { Button("Back", systemImage: "chevron.left", action: back).labelStyle(.iconOnly).buttonStyle(DesktopButtonStyle(compact: compact)).nativeGlass(style, in: Circle()) }
                Text(title).font(style.face(13, bold: true, relativeTo: .headline)).lineLimit(1).accessibilityAddTraits(.isHeader)
                Spacer(minLength: 4)
                // Each control on its own glass; a container lets neighbours run together as one group.
                NativeGlassGroup(style: style) { actions().buttonStyle(DesktopButtonStyle(compact: compact)).nativeGlass(style, in: Capsule()) }
            }.padding(.horizontal, compact ? 6 : 10).padding(.vertical, style.glass ? 4 : 0).frame(minHeight: style.pt(compact ? 40 : 44))
                .background(style.glass ? style.surface : style.panel)
                .contentShape(Rectangle())
                .simultaneousGesture(TapGesture(count: 2).onEnded { onDoubleTap?() }, including: onDoubleTap == nil ? .none : .all)
            if !style.glass { DesktopRule() }
        }
    }
    private var terminalBar: some View {
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
        if style.native {
            // A filled rounded field, as iOS draws a search or text field.
            content.textFieldStyle(.plain).padding(.horizontal, 12).padding(.vertical, 9)
                .background(style.panel, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        } else {
            content.textFieldStyle(.plain).padding(8)
                .background(style.background)
                .overlay(RoundedRectangle(cornerRadius: 3).stroke(style.divider, lineWidth: 1))
        }
    }
}

/// Controls side by side; on glass, in a container so their glass blends where they meet.
struct NativeGlassGroup<Content: View>: View {
    let style: DesktopStyle
    @ViewBuilder var content: () -> Content
    var body: some View {
        if #available(iOS 26, *), style.glass {
            GlassEffectContainer(spacing: 6) { HStack(spacing: 2) { content() } }
        } else {
            HStack(spacing: 0) { content() }
        }
    }
}

extension View {
    /// Liquid Glass in `shape` while the style is on glass (Native on iOS 26); otherwise the view as it is. A field to type in is
    /// not `interactive`: its glass stays still under a finger that selects text.
    @ViewBuilder func nativeGlass<S: Shape>(_ style: DesktopStyle, in shape: S, interactive: Bool = true) -> some View {
        if #available(iOS 26, *), style.glass { glassEffect(.regular.interactive(interactive), in: shape) } else { self }
    }
    /// A sheet's surface and corners: the background with small square corners in the terminal look; in Native, the system's
    /// own corners, and on iOS 26 its glass.
    @ViewBuilder func desktopSheetSurface(_ style: DesktopStyle) -> some View {
        if style.native { background(style.surface) } else { background(style.background).presentationCornerRadius(8) }
    }
    /// The fill of a row that can be chosen in a sheet's list (a kind, a model): a band across the sheet in the terminal look; in Native,
    /// a rounded highlight set in from the sheet's edges, as iOS marks the chosen row. The whole width still takes the tap.
    @ViewBuilder func desktopRowFill(_ style: DesktopStyle, selected: Bool) -> some View {
        if style.native {
            background(selected ? style.active : .clear, in: RoundedRectangle(cornerRadius: 10, style: .continuous)).padding(.horizontal, 8).contentShape(Rectangle())
        } else {
            background(selected ? style.active : .clear).contentShape(Rectangle())
        }
    }
    /// A sheet's list with the footer that holds its main button under it, the list clipped at the footer's rule so its last lines
    /// never run under the footer's labels (on glass too, where the footer has no panel of its own).
    @ViewBuilder func desktopSheetFooter<Footer: View>(_ style: DesktopStyle, @ViewBuilder footer: () -> Footer) -> some View {
        if style.native { VStack(spacing: 0) { self.clipped(); footer() } } else { VStack(spacing: 0) { self; footer() } }
    }
}

/// The keyboard's ring around what it is on in a sheet: square in the terminal look, rounded like the row in Native.
struct DesktopRing: View {
    @Environment(\.desktopStyle) private var style
    var shown: Bool
    var capsule = false
    var body: some View {
        Group {
            if capsule && style.native { Capsule().stroke(style.accent, lineWidth: 2) }
            else { RoundedRectangle(cornerRadius: style.native ? 10 : 0, style: .continuous).stroke(style.accent, lineWidth: 2) }
        }
        .opacity(shown ? 1 : 0).allowsHitTesting(false)
    }
}
