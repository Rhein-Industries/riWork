import Foundation

/// What a chord does.
public enum ShortcutAction: Sendable, Equatable {
    case openPalette
    case openSettings
    case openHelp
    case hotkey(Hotkey)
}

/// The extra shortcuts that open the hotkey menu, besides ⌘K, and the hotkey help, besides ⌘/. Kept small, validated, and stored as
/// JSON. A chord opens one or the other, never both, and never belongs to a hotkey.
public struct ShortcutSettings: Sendable, Equatable {
    public static let maxPaletteChords = 4
    public static let maxHelpChords = 4
    public private(set) var paletteChords: [KeyChord]
    public private(set) var helpChords: [KeyChord]
    public init(paletteChords: [KeyChord] = [], helpChords: [KeyChord] = []) {
        let palette = Array(Self.clean(paletteChords).prefix(Self.maxPaletteChords))
        self.paletteChords = palette
        self.helpChords = Array(Self.clean(helpChords).filter { !palette.contains($0) }.prefix(Self.maxHelpChords))
    }

    /// Usable and not repeated. The reserved chords (⌘K, ⌘, ⌘/) are not usable, so they are dropped here too.
    private static func clean(_ chords: [KeyChord]) -> [KeyChord] {
        var seen = Set<KeyChord>()
        return chords.filter { ($0.isTap || $0.isValid) && seen.insert($0).inserted }
    }

    /// Adds a chord that opens the hotkey menu. Throws when it is unusable or taken (by a hotkey, in `library`, or by the help).
    public mutating func addPaletteChord(_ chord: KeyChord, library: HotkeyLibrary = HotkeyLibrary()) throws {
        try chord.validate()
        if let owner = library.hotkey(for: chord) { throw HotkeyError.chordInUse(owner.label) }
        if helpChords.contains(chord) { throw HotkeyError.chordInUse("the hotkey help") }
        guard !paletteChords.contains(chord) else { return }
        guard paletteChords.count < Self.maxPaletteChords else { throw HotkeyError.tooManyHotkeys(Self.maxPaletteChords) }
        paletteChords.append(chord)
    }
    public mutating func removePaletteChord(_ chord: KeyChord) { paletteChords.removeAll { $0 == chord } }

    /// Adds a chord that opens the hotkey help. Throws when it is unusable or taken (by a hotkey, in `library`, or by the menu).
    public mutating func addHelpChord(_ chord: KeyChord, library: HotkeyLibrary = HotkeyLibrary()) throws {
        try chord.validate()
        if let owner = library.hotkey(for: chord) { throw HotkeyError.chordInUse(owner.label) }
        if paletteChords.contains(chord) { throw HotkeyError.chordInUse("the hotkey menu") }
        guard !helpChords.contains(chord) else { return }
        guard helpChords.count < Self.maxHelpChords else { throw HotkeyError.tooManyHotkeys(Self.maxHelpChords) }
        helpChords.append(chord)
    }
    public mutating func removeHelpChord(_ chord: KeyChord) { helpChords.removeAll { $0 == chord } }

    /// Adds a template's menu shortcuts that are free; returns those that were new.
    @discardableResult
    public mutating func merge(_ template: HotkeyTemplate, library: HotkeyLibrary) -> [KeyChord] {
        var added: [KeyChord] = []
        for chord in template.paletteChords where !paletteChords.contains(chord) {
            if (try? addPaletteChord(chord, library: library)) != nil { added.append(chord) }
        }
        return added
    }

    /// The help chords are written only when there are some, so what was stored before the help existed reads and writes the same.
    public var encoded: String {
        var object: [String: JSONValue] = ["v": .number(1), "palette": .array(paletteChords.map(\.json))]
        if !helpChords.isEmpty { object["help"] = .array(helpChords.map(\.json)) }
        return (try? JSONEncoder().encode(JSONValue.object(object))).flatMap { String(data: $0, encoding: .utf8) } ?? ""
    }
    /// Never throws: whatever is unreadable is dropped.
    public init(encoded text: String?) {
        guard let text, let value = try? JSONDecoder().decode(JSONValue.self, from: Data(text.utf8)), value["v"] == .number(1) else { self.init(); return }
        self.init(paletteChords: value["palette"].array.compactMap { try? KeyChord(json: $0) },
                  helpChords: value["help"].array.compactMap { try? KeyChord(json: $0) })
    }
}

/// Looks up the action for a chord a keyboard sent. ⌘K, ⌘, and ⌘/ are fixed; then the menu and help shortcuts; then the hotkeys' own.
public struct ShortcutMap: Sendable, Equatable {
    private let paletteChords: Set<KeyChord>
    private let helpChords: Set<KeyChord>
    private let hotkeys: [KeyChord: Hotkey]

    public init(hotkeys: [Hotkey], settings: ShortcutSettings = ShortcutSettings()) {
        paletteChords = Set(settings.paletteChords)
        helpChords = Set(settings.helpChords)
        var map: [KeyChord: Hotkey] = [:]
        for hotkey in hotkeys { if let chord = hotkey.chord, map[chord] == nil { map[chord] = hotkey } }
        self.hotkeys = map
    }
    public func action(for chord: KeyChord) -> ShortcutAction? {
        if chord == .paletteDefault || paletteChords.contains(chord) { return .openPalette }
        if chord == .settingsDefault { return .openSettings }
        if chord == .helpDefault || helpChords.contains(chord) { return .openHelp }
        return hotkeys[chord].map(ShortcutAction.hotkey)
    }
    /// Whether a modifier key is the whole of some shortcut, so a press of it is worth watching for a tap.
    public var hasTapChords: Bool { paletteChords.contains(where: \.isTap) || helpChords.contains(where: \.isTap) || hotkeys.keys.contains(where: \.isTap) }
    /// Every shortcut that is not a fixed one: the menu and help shortcuts and the hotkeys', in no particular order.
    public var chords: [KeyChord] { Array(paletteChords) + Array(helpChords) + Array(hotkeys.keys) }
}

/// What a key event looked like when it reached the app, for the debug readout and for learning a key.
public struct KeyEventRecord: Sendable, Equatable {
    public enum Phase: String, Sendable { case down = "down", up = "up", command = "command", cancelled = "cancelled" }
    public var phase: Phase
    /// The HID usage (keyboard page) of the key, as `UIKey.keyCode` reports it. Nil for a key command, which reports characters only.
    public var keyCode: Int?
    public var modifiers: ChordModifiers
    /// Everything UIKit reported, raw, including Caps Lock and the numeric-pad flag.
    public var rawModifiers: Int
    public var characters: String
    public var charactersIgnoringModifiers: String

    public init(phase: Phase, keyCode: Int?, modifiers: ChordModifiers, rawModifiers: Int = 0, characters: String = "", charactersIgnoringModifiers: String = "") {
        self.phase = phase; self.keyCode = keyCode; self.modifiers = modifiers; self.rawModifiers = rawModifiers
        self.characters = characters; self.charactersIgnoringModifiers = charactersIgnoringModifiers
    }
    public var chord: KeyChord? { keyCode.map { KeyChord(keyCode: $0, modifiers: modifiers) } }

    /// `Text` that shows control characters as visible escapes.
    public static func visible(_ text: String) -> String {
        if text.isEmpty { return "∅" }
        return text.unicodeScalars.map { scalar -> String in
            switch scalar.value {
            case 0x1B: "⎋"
            case 0x09: "⇥"
            case 0x0D, 0x0A: "⏎"
            case 0x20: "␠"
            case 0x7F: "⌦"
            case 0..<0x20: "^" + String(UnicodeScalar(UInt8(scalar.value + 64)))
            default: String(scalar)
            }
        }.joined()
    }
    /// Several short lines for the overlay.
    public var lines: [String] {
        var out: [String] = []
        if let keyCode {
            out.append("\(phase.rawValue)  keyCode \(keyCode) · usage 0x07/0x\(String(keyCode, radix: 16, uppercase: true))")
            out.append(HIDKey.name(of: keyCode))
        } else {
            out.append("\(phase.rawValue)  (no keyCode)")
        }
        out.append("mods \(modifiers.isEmpty ? "none" : modifiers.glyphs)  raw 0x\(String(rawModifiers, radix: 16))")
        out.append("chars \(Self.visible(characters))  plain \(Self.visible(charactersIgnoringModifiers))")
        return out
    }
    public var summary: String { lines.joined(separator: " · ") }
}
