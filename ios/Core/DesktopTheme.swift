import Foundation

/// One themed color. Built-in colors differ between light and dark appearance; a synced color is the same in both,
/// because the desktop's `dark` flag, not the phone's setting, decides which look is shown.
public struct ThemeColor: Sendable, Equatable {
    public let light: RGB
    public let dark: RGB
    public init(light: RGB, dark: RGB) { self.light = light; self.dark = dark }
    public init(fixed: RGB) { light = fixed; dark = fixed }
    public var isFixed: Bool { light == dark }
}

/// The colors the app draws with. Resolved from the desktop's `DesktopAppearance` when one is known, otherwise built in
/// (Gruvbox Light in light appearance, RiWork in dark). A pure value: the same input always gives the same theme.
public struct DesktopTheme: Sendable, Equatable {
    public var background: ThemeColor, panel: ThemeColor, active: ThemeColor, divider: ThemeColor
    public var text: ThemeColor, muted: ThemeColor
    /// The primary accent (the desktop's cyan): live, selected, interactive.
    public var accent: ThemeColor
    /// The secondary accent (the desktop's magenta): orchestrators and other secondary marks.
    public var magenta: ThemeColor
    /// Warnings and things that need attention (the desktop's gold).
    public var gold: ThemeColor
    public var error: ThemeColor
    public var terminalBackground: ThemeColor, terminalForeground: ThemeColor
    /// The block cursor. The character under it is drawn in `terminalBackground`.
    public var terminalCursor: ThemeColor
    /// `nil` while built in (follow the phone's appearance); otherwise the desktop's `dark` flag, which also drives the status bar.
    public var dark: Bool?
    public var isSynced: Bool { dark != nil }
    public var warning: ThemeColor { gold }

    /// Below this WCAG contrast ratio a synced foreground is not trusted against its background.
    public static let minimumContrast = 3.0

    /// The two built-in looks. Values from `src/theme.rs`: Gruvbox Light, and RiWork dark.
    struct Scheme {
        let bg: RGB, panel: RGB, active: RGB, divider: RGB, text: RGB, muted: RGB, accent: RGB, magenta: RGB, gold: RGB, error: RGB
    }
    static let lightScheme = Scheme(bg: RGB(0xfbf1c7), panel: RGB(0xf4ebc2), active: RGB(0xede3bc), divider: RGB(0xd5ccb6), text: RGB(0x3c3836), muted: RGB(0x756f5e),
                                    accent: RGB(0x427b58), magenta: RGB(0x8f3f71), gold: RGB(0x9d5015), error: RGB(0x9d0006))
    static let darkScheme = Scheme(bg: RGB(0x090d14), panel: RGB(0x101720), active: RGB(0x14212a), divider: RGB(0x253c45), text: RGB(0xd3e1e6), muted: RGB(0x8fa6ae),
                                   accent: RGB(0x55e6dc), magenta: RGB(0xce78ef), gold: RGB(0xf4bf75), error: RGB(0xf0738b))

    public static let builtIn: DesktopTheme = {
        func pair(_ path: KeyPath<Scheme, RGB>) -> ThemeColor { ThemeColor(light: lightScheme[keyPath: path], dark: darkScheme[keyPath: path]) }
        return DesktopTheme(
            background: pair(\.bg), panel: pair(\.panel), active: pair(\.active), divider: pair(\.divider), text: pair(\.text), muted: pair(\.muted),
            accent: pair(\.accent), magenta: pair(\.magenta), gold: pair(\.gold), error: pair(\.error),
            terminalBackground: pair(\.bg), terminalForeground: pair(\.text), terminalCursor: pair(\.accent), dark: nil)
    }()

    /// The theme for a desktop's appearance, or the built-in one when there is none.
    ///
    /// Contrast guard, so a strange or half-broken desktop palette can never make the app unreadable:
    /// - text must reach 3:1 against the background, the panel and the active row; otherwise the whole base
    ///   (background, panel, active, divider, text) is the built-in one for the desktop's light or dark side;
    /// - muted, accent, magenta and gold must reach 3:1 against those surfaces; otherwise the built-in color of that
    ///   role, and if that is unreadable too, the text color;
    /// - the terminal's own foreground must reach 3:1 against its background; otherwise the terminal uses the base.
    public static func resolve(_ appearance: DesktopAppearance?) -> DesktopTheme {
        guard let appearance else { return .builtIn }
        let builtIn = appearance.dark ? darkScheme : lightScheme
        let p = appearance.palette
        func readable(_ foreground: RGB, on surfaces: [RGB]) -> Bool { surfaces.allSatisfy { foreground.contrast(with: $0) >= minimumContrast } }

        let syncedSurfaces = [p.bg, p.panel, p.panelActive]
        let baseOK = readable(p.text, on: syncedSurfaces)
        let bg = baseOK ? p.bg : builtIn.bg, panel = baseOK ? p.panel : builtIn.panel, active = baseOK ? p.panelActive : builtIn.active
        let divider = baseOK ? p.divider : builtIn.divider, text = baseOK ? p.text : builtIn.text
        let surfaces = [bg, panel, active]
        func guarded(_ synced: RGB, builtIn fallback: RGB) -> RGB {
            if readable(synced, on: surfaces) { return synced }
            return readable(fallback, on: surfaces) ? fallback : text
        }

        var foreground = text, background = bg
        if let terminal = appearance.terminal, terminal.foreground.contrast(with: terminal.background) >= minimumContrast {
            foreground = terminal.foreground; background = terminal.background
        }
        func fixed(_ color: RGB) -> ThemeColor { ThemeColor(fixed: color) }
        return DesktopTheme(
            background: fixed(bg), panel: fixed(panel), active: fixed(active), divider: fixed(divider),
            text: fixed(text), muted: fixed(guarded(p.muted, builtIn: builtIn.muted)),
            accent: fixed(guarded(p.cyan, builtIn: builtIn.accent)), magenta: fixed(guarded(p.magenta, builtIn: builtIn.magenta)),
            gold: fixed(guarded(p.gold, builtIn: builtIn.gold)), error: fixed(guarded(builtIn.error, builtIn: builtIn.error)),
            terminalBackground: fixed(background), terminalForeground: fixed(foreground), terminalCursor: fixed(foreground),
            dark: appearance.dark)
    }
}
