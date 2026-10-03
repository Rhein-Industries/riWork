import Foundation
import Observation
import RiWorkCore

/// The hotkeys a person added to the key bar, kept on this device (UserDefaults: they are shortcuts, not secrets).
/// Every change is validated by `HotkeyLibrary` and written straight away; what cannot be read back is dropped on load.
@MainActor @Observable final class HotkeyStore {
    static let key = "riwork.hotkeys", shortcutsKey = "riwork.shortcuts"
    private(set) var library: HotkeyLibrary
    /// The extra shortcuts that open the hotkey menu (⌘K always does) and the hotkey help (⌘/ always does).
    private(set) var shortcuts: ShortcutSettings
    @ObservationIgnored private let defaults: UserDefaults
    var custom: [Hotkey] { library.hotkeys }
    /// The ones that get a button on the key bar; the rest are shortcuts and menu entries only.
    var onBar: [Hotkey] { library.hotkeys.filter(\.showsOnBar) }
    /// What each shortcut does.
    var shortcutMap: ShortcutMap { ShortcutMap(hotkeys: library.hotkeys, settings: shortcuts) }

    init(defaults: UserDefaults) {
        self.defaults = defaults
        library = HotkeyLibrary(encoded: defaults.string(forKey: Self.key))
        shortcuts = ShortcutSettings(encoded: defaults.string(forKey: Self.shortcutsKey))
    }
    func add(_ hotkey: Hotkey) throws { try checkShortcut(of: hotkey); var next = library; try next.add(hotkey); commit(next) }
    func update(_ hotkey: Hotkey) throws { try checkShortcut(of: hotkey); var next = library; try next.update(hotkey); commit(next) }
    /// A hotkey cannot take a shortcut that already opens the hotkey menu or the hotkey help.
    private func checkShortcut(of hotkey: Hotkey) throws {
        if let chord = hotkey.chord, shortcuts.paletteChords.contains(chord) { throw HotkeyError.chordInUse("the hotkey menu") }
        if let chord = hotkey.chord, shortcuts.helpChords.contains(chord) { throw HotkeyError.chordInUse("the hotkey help") }
    }
    func remove(id: String) { var next = library; next.remove(id: id); commit(next) }
    func remove(atOffsets offsets: IndexSet) { var next = library; next.remove(atOffsets: Array(offsets)); commit(next) }
    func move(fromOffsets offsets: IndexSet, toOffset destination: Int) { var next = library; next.move(fromOffsets: Array(offsets), toOffset: destination); commit(next) }
    func addPaletteChord(_ chord: KeyChord) throws { var next = shortcuts; try next.addPaletteChord(chord, library: library); commit(next) }
    func removePaletteChord(_ chord: KeyChord) { var next = shortcuts; next.removePaletteChord(chord); commit(next) }
    func addHelpChord(_ chord: KeyChord) throws { var next = shortcuts; try next.addHelpChord(chord, library: library); commit(next) }
    func removeHelpChord(_ chord: KeyChord) { var next = shortcuts; next.removeHelpChord(chord); commit(next) }
    /// Adds a template's hotkeys and menu shortcuts to what is there. Nothing the person has is removed or changed.
    @discardableResult
    func install(_ template: HotkeyTemplate) -> TemplateMerge {
        var nextLibrary = library
        // A shortcut that already opens the hotkey menu or the help is theirs: the template's hotkey on it is left out, not shadowed.
        var taken: [KeyChord: String] = [:]
        for chord in shortcuts.paletteChords { taken[chord] = "the hotkey menu" }
        for chord in shortcuts.helpChords { taken[chord] = "the hotkey help" }
        var result = nextLibrary.merge(template, taken: taken)
        var nextShortcuts = shortcuts
        result.paletteChordsAdded = nextShortcuts.merge(template, library: nextLibrary)
        commit(nextLibrary)
        commit(nextShortcuts)
        return result
    }
    private func commit(_ next: ShortcutSettings) {
        guard next != shortcuts else { return }
        shortcuts = next
        defaults.set(next.encoded, forKey: Self.shortcutsKey)
    }
    private func commit(_ next: HotkeyLibrary) {
        guard next != library else { return }
        library = next
        defaults.set(next.encoded, forKey: Self.key)
    }
}
