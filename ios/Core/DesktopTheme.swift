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

/// The colors the app draws with. A phone can choose its own palette while
/// retaining native controls; a synced appearance is also available explicitly.
/// A pure value: the same input always gives the same theme.
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
    /// The terminal's 16 ANSI colors, as the shell's SGR 30-37 / 90-97 / 40-47 / 100-107 and palette entries 0-15 name them.
    /// The desktop's own when it published a terminal palette, otherwise a built-in set that suits its light or dark side.
    public var ansi: [ThemeColor]
    /// `nil` while built in (follow the phone's appearance); otherwise the desktop's `dark` flag, which also drives the status bar.
    public var dark: Bool?
    public var isSynced: Bool { dark != nil }
    /// The desktop uses its Native skin, so the interface is drawn the native way. False while built in.
    public var native = false
    /// Native's dynamic semantic colors. A fixed phone palette still uses native
    /// controls, but supplies its own colors and preferred light/dark appearance.
    public var followsSystemAppearance = false
    /// The desktop's setting for dictation: the phone shows its mics (chat composer, key bar, line composer) only while it is on. Off
    /// while built in, as with a desktop that predates the setting.
    public var mic = false
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
            terminalBackground: pair(\.bg), terminalForeground: pair(\.text), terminalCursor: pair(\.accent),
            ansi: (0..<TerminalColors.paletteCount).map { ThemeColor(light: TerminalRenderColors.fallbackAnsi(dark: false)[$0], dark: TerminalRenderColors.fallbackAnsi(dark: true)[$0]) },
            dark: nil)
    }()

    /// iPhone's neutral native palette, independent of the selected desktop's
    /// skin and light/dark setting. Functional settings still come from that
    /// desktop. `dark == nil` lets iOS choose appearance for controls and sheets.
    public static func nativePhone(mic: Bool = false) -> DesktopTheme {
        func pair(_ light: UInt32, _ dark: UInt32) -> ThemeColor {
            ThemeColor(light: RGB(light), dark: RGB(dark))
        }
        let lightBackground = RGB(0xffffff), darkBackground = RGB(0x000000)
        let lightForeground = RGB(0x1c1c1e), darkForeground = RGB(0xf2f2f7)
        // Keep the ANSI roles while choosing legible variants for each native
        // surface. Only the theme's 16 palette entries change: explicit RGB and
        // xterm-256 colors from terminal programs retain their own meaning.
        let lightAnsi: [UInt32] = [
            0x1c1c1e, 0xb42318, 0x216e39, 0x806000, 0x0057b8, 0x8b2c87, 0x006b73, 0x636366,
            0x48484a, 0xc93c2a, 0x287f46, 0x916f00, 0x006ada, 0xa13ba0, 0x007f88, 0x3a3a3c
        ]
        let darkAnsi: [UInt32] = [
            0x636366, 0xff6961, 0x6bd66f, 0xffd60a, 0x64b5ff, 0xe6a1f0, 0x5ed8f4, 0xd1d1d6,
            0x8e8e93, 0xff8a80, 0x8ce99a, 0xffe680, 0x9acbff, 0xf2b7fa, 0x9ce9f9, 0xffffff
        ]
        func legible(_ value: UInt32, background: RGB, foreground: RGB) -> RGB {
            let color = RGB(value)
            if color.contrast(with: background) >= 4.5 { return color }
            // Raise contrast without replacing a colored role with unrelated
            // hue. This also keeps ANSI black readable on a black surface.
            for step in 1...20 {
                let adjusted = foreground.blended(over: color, opacity: Double(step) / 20)
                if adjusted.contrast(with: background) >= 4.5 { return adjusted }
            }
            return foreground
        }
        return DesktopTheme(
            background: pair(0xffffff, 0x000000), panel: pair(0xf2f2f7, 0x1c1c1e),
            active: pair(0xe5e5ea, 0x2c2c2e), divider: pair(0xc6c6c8, 0x38383a),
            text: pair(0x1c1c1e, 0xf2f2f7), muted: pair(0x636366, 0xaeaeb2),
            accent: pair(0x0066cc, 0x64b5ff), magenta: pair(0x8944ab, 0xd6a3ed),
            gold: pair(0x916000, 0xffd60a), error: pair(0xb42318, 0xff6961),
            terminalBackground: ThemeColor(light: lightBackground, dark: darkBackground),
            terminalForeground: ThemeColor(light: lightForeground, dark: darkForeground),
            terminalCursor: ThemeColor(light: lightForeground, dark: darkForeground),
            ansi: (0..<TerminalColors.paletteCount).map {
                ThemeColor(light: legible(lightAnsi[$0], background: lightBackground, foreground: lightForeground),
                           dark: legible(darkAnsi[$0], background: darkBackground, foreground: darkForeground))
            }, dark: nil, native: true, followsSystemAppearance: true, mic: mic)
    }

    /// A local phone color choice. It cannot opt into the desktop's typography,
    /// casing or layout, and only the desktop's functional mic setting is used.
    public static func phonePalette(_ preset: PhonePalette, mic: Bool = false) -> DesktopTheme {
        var result = resolve(DesktopAppearance(updatedAt: 0, dark: preset.dark, palette: preset.palette,
            terminal: preset.terminal, native: true, mic: mic))
        let surfaces = [result.background.light, result.panel.light, result.active.light]
        result.error = ThemeColor(fixed: surfaces.allSatisfy { preset.error.contrast(with: $0) >= minimumContrast }
            ? preset.error : result.text.light)
        return result
    }

    /// Explicit "Follow desktop" on a phone, including the desktop's effective
    /// Ghostty colors. With no cached appearance, Native remains the fallback.
    public static func phoneDesktop(_ appearance: DesktopAppearance?) -> DesktopTheme {
        guard let appearance else { return .nativePhone() }
        var result = resolve(appearance)
        result.native = true
        return result
    }

    /// The colors a screen is drawn with, for the light or the dark side (a synced theme has one side only).
    public func terminalColors(dark: Bool, boldIsBright: Bool = false) -> TerminalRenderColors {
        func pick(_ color: ThemeColor) -> RGB { dark ? color.dark : color.light }
        return TerminalRenderColors(foreground: pick(terminalForeground), background: pick(terminalBackground), ansi: ansi.map(pick), boldIsBright: boldIsBright)
    }

    /// The theme for a desktop's appearance, or the built-in one when there is none.
    ///
    /// Contrast guard, so a strange or half-broken desktop palette can never make the app unreadable:
    /// - text must reach 3:1 against the background, the panel and the active row; otherwise the whole base
    ///   (background, panel, active, divider, text) is the built-in one for the desktop's light or dark side;
    /// - muted, accent, magenta and gold must reach 3:1 against those surfaces; otherwise the built-in color of that
    ///   role, and if that is unreadable too, the text color;
    /// - the terminal's own foreground must reach 3:1 against its background; otherwise the terminal uses the base.
    public static func resolve(_ appearance: DesktopAppearance?, nativePhone: Bool = false) -> DesktopTheme {
        if nativePhone { return .nativePhone(mic: appearance?.mic ?? false) }
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
            ansi: (appearance.terminal?.palette ?? TerminalRenderColors.fallbackAnsi(dark: appearance.dark)).map(fixed),
            dark: appearance.dark, native: appearance.native, mic: appearance.mic)
    }
}
