import XCTest
@testable import RiWorkCore

final class DesktopThemeTests: XCTestCase {
    static func palette(bg: UInt32 = 0x090d14, panel: UInt32 = 0x101720, active: UInt32 = 0x14212a, divider: UInt32 = 0x253c45, cyan: UInt32 = 0x55e6dc,
                        magenta: UInt32 = 0xce78ef, gold: UInt32 = 0xf4bf75, text: UInt32 = 0xd3e1e6, muted: UInt32 = 0x708993) -> DesktopPalette {
        DesktopPalette(bg: RGB(bg), panel: RGB(panel), panelActive: RGB(active), divider: RGB(divider), cyan: RGB(cyan), magenta: RGB(magenta),
                       gold: RGB(gold), text: RGB(text), muted: RGB(muted))
    }
    static func appearance(_ palette: DesktopPalette = DesktopThemeTests.palette(), dark: Bool = true, terminal: TerminalColors? = nil) -> DesktopAppearance {
        DesktopAppearance(updatedAt: 1, dark: dark, palette: palette, terminal: terminal)
    }
    static func terminal(background: UInt32, foreground: UInt32) -> TerminalColors {
        TerminalColors(background: RGB(background), foreground: RGB(foreground), palette: (0..<16).map { RGB(UInt32($0) * 0x111111) })
    }

    func testNoAppearanceIsTheBuiltInTheme() {
        let theme = DesktopTheme.resolve(nil)
        XCTAssertEqual(theme, .builtIn)
        XCTAssertFalse(theme.isSynced)
        XCTAssertNil(theme.dark)
        XCTAssertEqual(theme.background, ThemeColor(light: RGB(0xfbf1c7), dark: RGB(0x090d14)), "Gruvbox Light / RiWork, as before syncing existed")
        XCTAssertEqual(theme.accent, ThemeColor(light: RGB(0x427b58), dark: RGB(0x55e6dc)))
        XCTAssertEqual(theme.gold, ThemeColor(light: RGB(0x9d5015), dark: RGB(0xf4bf75)))
        XCTAssertEqual(theme.terminalBackground, theme.background)
        XCTAssertEqual(theme.terminalForeground, theme.text)
        XCTAssertEqual(theme.terminalCursor, theme.accent, "the built-in cursor keeps its accent")
    }
    func testSyncedPaletteMapsRolesTheWayTheDesktopUsesThem() {
        let theme = DesktopTheme.resolve(Self.appearance(Self.palette(cyan: 0x40e0d0, magenta: 0xd070e0, gold: 0xe0b060), dark: true))
        XCTAssertTrue(theme.isSynced)
        XCTAssertEqual(theme.dark, true)
        XCTAssertEqual(theme.background, ThemeColor(fixed: RGB(0x090d14)))
        XCTAssertEqual(theme.panel, ThemeColor(fixed: RGB(0x101720)))
        XCTAssertEqual(theme.active, ThemeColor(fixed: RGB(0x14212a)))
        XCTAssertEqual(theme.divider, ThemeColor(fixed: RGB(0x253c45)))
        XCTAssertEqual(theme.text, ThemeColor(fixed: RGB(0xd3e1e6)))
        XCTAssertEqual(theme.muted, ThemeColor(fixed: RGB(0x708993)))
        XCTAssertEqual(theme.accent, ThemeColor(fixed: RGB(0x40e0d0)), "cyan is the accent")
        XCTAssertEqual(theme.magenta, ThemeColor(fixed: RGB(0xd070e0)), "magenta is the secondary accent")
        XCTAssertEqual(theme.gold, ThemeColor(fixed: RGB(0xe0b060)))
        XCTAssertEqual(theme.warning, theme.gold, "gold is for warnings")
        XCTAssertTrue(theme.accent.isFixed && theme.background.isFixed, "the desktop's dark flag, not the phone's, picks the look")
    }
    func testOnlyANativeDesktopMakesTheThemeNative() {
        XCTAssertFalse(DesktopTheme.builtIn.native, "no desktop: the terminal look")
        XCTAssertFalse(DesktopTheme.resolve(Self.appearance()).native, "a desktop without the flag: the terminal look")
        // Native's own light and dark palettes (src/theme.rs NATIVE_LIGHT / NATIVE_DARK).
        let light = Self.palette(bg: 0xffffff, panel: 0xf5f5f7, active: 0xe8e8ed, divider: 0xd2d2d7, cyan: 0x000000, magenta: 0x3a3a3c, gold: 0xb34000, text: 0x1d1d1f, muted: 0x636366)
        let dark = Self.palette(bg: 0x000000, panel: 0x1c1c1e, active: 0x2c2c2e, divider: 0x3a3a3c, cyan: 0xffffff, magenta: 0xc7c7cc, gold: 0xff9f0a, text: 0xf5f5f7, muted: 0x98989d)
        for (palette, isDark) in [(light, false), (dark, true)] {
            let theme = DesktopTheme.resolve(DesktopAppearance(updatedAt: 1, dark: isDark, palette: palette, native: true))
            XCTAssertTrue(theme.native)
            XCTAssertEqual(theme.dark, isDark)
            XCTAssertEqual(theme.background, ThemeColor(fixed: palette.bg), "Native's colors pass the contrast guard unchanged")
            XCTAssertEqual(theme.accent, ThemeColor(fixed: palette.cyan))
            XCTAssertEqual(theme.gold, ThemeColor(fixed: palette.gold))
            XCTAssertEqual(theme.muted, ThemeColor(fixed: palette.muted))
        }
    }
    func testTheMicSettingComesOnlyFromTheDesktop() {
        XCTAssertFalse(DesktopTheme.builtIn.mic, "no desktop: no mics")
        XCTAssertFalse(DesktopTheme.resolve(Self.appearance()).mic, "a desktop without the setting: no mics")
        let on = DesktopTheme.resolve(DesktopAppearance(updatedAt: 1, dark: true, palette: Self.appearance().palette, mic: true))
        XCTAssertTrue(on.mic)
        XCTAssertFalse(on.native, "the setting does not change the skin")
    }
    func testTerminalColorsAreUsedWhenPresentAndPaletteWhenNot() {
        let with = DesktopTheme.resolve(Self.appearance(terminal: Self.terminal(background: 0x101010, foreground: 0xeeeeee)))
        XCTAssertEqual(with.terminalBackground, ThemeColor(fixed: RGB(0x101010)))
        XCTAssertEqual(with.terminalForeground, ThemeColor(fixed: RGB(0xeeeeee)))
        XCTAssertEqual(with.terminalCursor, ThemeColor(fixed: RGB(0xeeeeee)))
        let without = DesktopTheme.resolve(Self.appearance())
        XCTAssertEqual(without.terminalBackground, without.background, "falls back to palette.bg")
        XCTAssertEqual(without.terminalForeground, without.text, "falls back to palette.text")
        XCTAssertEqual(without.terminalCursor, without.text)
    }
    func testUnreadableTextFallsBackToTheBuiltInBaseForThatSide() {
        // Dark desktop whose text is nearly its background.
        let dark = DesktopTheme.resolve(Self.appearance(Self.palette(text: 0x0b1018), dark: true))
        XCTAssertEqual(dark.text, ThemeColor(fixed: RGB(0xd3e1e6)))
        XCTAssertEqual(dark.background, ThemeColor(fixed: RGB(0x090d14)))
        XCTAssertGreaterThanOrEqual(dark.text.dark.contrast(with: dark.background.dark), 3)
        // Light desktop with unreadable text: the built-in light pair, not the dark one.
        let light = DesktopTheme.resolve(Self.appearance(Self.palette(bg: 0xffffff, panel: 0xf8f8f8, active: 0xf0f0f0, text: 0xfafafa), dark: false))
        XCTAssertEqual(light.text, ThemeColor(fixed: RGB(0x3c3836)))
        XCTAssertEqual(light.background, ThemeColor(fixed: RGB(0xfbf1c7)))
        XCTAssertEqual(light.dark, false, "still the desktop's side, so the status bar keeps following it")
    }
    func testTextThatFailsOnlyAgainstThePanelStillTriggersTheBaseFallback() {
        // Readable on bg (light text on a dark bg) but the panel is nearly the text color.
        let theme = DesktopTheme.resolve(Self.appearance(Self.palette(panel: 0xd0dde2), dark: true))
        XCTAssertEqual(theme.panel, ThemeColor(fixed: RGB(0x101720)))
        XCTAssertEqual(theme.background, ThemeColor(fixed: RGB(0x090d14)))
    }
    func testUnreadableSecondaryColorsFallBackOneRoleAtATime() {
        let theme = DesktopTheme.resolve(Self.appearance(Self.palette(cyan: 0x0a0e15, magenta: 0x0a0e15, gold: 0x0b0f16, muted: 0x0a0e15), dark: true))
        XCTAssertEqual(theme.accent, ThemeColor(fixed: RGB(0x55e6dc)), "built-in accent for the dark side")
        XCTAssertEqual(theme.magenta, ThemeColor(fixed: RGB(0xce78ef)))
        XCTAssertEqual(theme.gold, ThemeColor(fixed: RGB(0xf4bf75)))
        XCTAssertEqual(theme.muted, ThemeColor(fixed: RGB(0x8fa6ae)))
        XCTAssertEqual(theme.text, ThemeColor(fixed: RGB(0xd3e1e6)), "the healthy text is kept")
        // If even the built-in role is unreadable against a synced background, the text color is used.
        let odd = DesktopTheme.resolve(Self.appearance(Self.palette(bg: 0x55e6dc, panel: 0x4fdcd2, active: 0x48d0c8, cyan: 0x55e6dc, text: 0x001010, muted: 0x55e6dc), dark: true))
        XCTAssertEqual(odd.accent, odd.text)
        XCTAssertEqual(odd.muted, odd.text)
    }
    func testEveryColorThatIsUsedAsTextIsReadableOnTheSurfaces() {
        let hostile = DesktopTheme.resolve(Self.appearance(Self.palette(bg: 0x808080, panel: 0x848484, active: 0x7c7c7c, cyan: 0x858585, magenta: 0x7e7e7e, gold: 0x828282, text: 0x8a8a8a, muted: 0x878787), dark: false))
        for color in [hostile.text, hostile.muted, hostile.accent, hostile.magenta, hostile.gold, hostile.error] {
            for surface in [hostile.background, hostile.panel, hostile.active] {
                XCTAssertGreaterThanOrEqual(color.dark.contrast(with: surface.dark), 3)
            }
        }
    }
    func testUnreadableTerminalPairFallsBackToThePaletteBase() {
        let theme = DesktopTheme.resolve(Self.appearance(terminal: Self.terminal(background: 0x202020, foreground: 0x262626)))
        XCTAssertEqual(theme.terminalBackground, theme.background)
        XCTAssertEqual(theme.terminalForeground, theme.text)
        XCTAssertEqual(theme.terminalCursor, theme.text)
        // The exact boundary: 3:1 passes.
        let edge = RGB(0x767676)
        XCTAssertGreaterThan(edge.contrast(with: RGB(0xffffff)), 4.5)
        let ok = DesktopTheme.resolve(Self.appearance(terminal: Self.terminal(background: 0x000000, foreground: 0x767676)))
        XCTAssertEqual(ok.terminalForeground, ThemeColor(fixed: RGB(0x767676)))
    }
    func testResolveIsDeterministic() {
        let a = Self.appearance(terminal: Self.terminal(background: 0x101010, foreground: 0xeeeeee))
        XCTAssertEqual(DesktopTheme.resolve(a), DesktopTheme.resolve(a))
    }
}
