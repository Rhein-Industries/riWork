import Foundation

/// The hotkey help: a cheat sheet over the terminal (⌘/) that lists every hotkey with its shortcut and what it sends, and the app's
/// own shortcuts. It is what a person with a keyboard that has no labels (the Clicks case) looks at to find the right chord.
///
/// Pure data, worked out when the sheet opens, so what it lists is tested without a screen. The hotkeys are the person's (in key-bar
/// order, which includes an installed keyboard template), then the built-in ones; a built-in hotkey that one of the person's already
/// sends is left out, as in the hotkey menu. They fall in two groups: those with a shortcut, then those without.
public struct HotkeyHelp: Sendable, Equatable {
    /// One hotkey.
    public struct Row: Sendable, Equatable, Identifiable {
        /// What a tap on the row sends.
        public let hotkey: Hotkey
        public var id: String { hotkey.id }
        public var label: String { hotkey.label }
        /// Its shortcut in glyphs ("⌘E", "⇧⌘C"), nil when it has none.
        public var shortcut: String? { hotkey.chord?.title }
        /// What it sends, in glyphs: "⎋", "/clear ⏎".
        public var sends: String { hotkey.summary }
    }
    /// One of the app's own shortcuts.
    public struct AppShortcut: Sendable, Equatable, Identifiable {
        /// The keys that do it, the fixed one first and then the person's extra ones: "⌘K · Tap Left ⌃".
        public let keys: String
        public let title: String
        public var id: String { title }
    }

    /// Hotkeys that have a shortcut, in order.
    public let withShortcut: [Row]
    /// Hotkeys that are reached from the key bar, the hotkey menu, or by touch only.
    public let withoutShortcut: [Row]
    public let appShortcuts: [AppShortcut]

    /// Every row, with a shortcut first.
    public var rows: [Row] { withShortcut + withoutShortcut }

    /// `custom` are the person's hotkeys, `shortcuts` their extra shortcuts for the menu and for this help.
    public init(hotkeys custom: [Hotkey], shortcuts: ShortcutSettings = ShortcutSettings()) {
        let sent = Set(custom.map(\.items))
        let all = (custom + Hotkey.builtIn.filter { !sent.contains($0.items) }).map { Row(hotkey: $0) }
        withShortcut = all.filter { $0.hotkey.chord != nil }
        withoutShortcut = all.filter { $0.hotkey.chord == nil }
        func keys(_ fixed: KeyChord, _ extra: [KeyChord]) -> String { ([fixed] + extra).map(\.title).joined(separator: " · ") }
        appShortcuts = [
            AppShortcut(keys: keys(.paletteDefault, shortcuts.paletteChords), title: "Hotkey menu"),
            AppShortcut(keys: KeyChord.settingsDefault.title, title: "Hotkey settings"),
            // A SwiftUI keyboard shortcut of the terminal screen, not one of the capture view's commands.
            AppShortcut(keys: "⌘N", title: "New terminal"),
            AppShortcut(keys: keys(.helpDefault, shortcuts.helpChords), title: "This help")
        ]
    }
}
