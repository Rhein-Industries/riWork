import Foundation

/// The modifier keys a chord can carry. Own bits rather than UIKit's `UIKeyModifierFlags`, so the model is pure and testable; the app
/// maps the flags in one place. Caps Lock and the numeric-pad flag (UIKit sets it on arrow keys) are not modifiers here.
public struct ChordModifiers: OptionSet, Sendable, Hashable {
    public let rawValue: Int
    public init(rawValue: Int) { self.rawValue = rawValue }
    public static let shift = ChordModifiers(rawValue: 1)
    public static let control = ChordModifiers(rawValue: 2)
    public static let alt = ChordModifiers(rawValue: 4)
    public static let command = ChordModifiers(rawValue: 8)
    public static let all: ChordModifiers = [.shift, .control, .alt, .command]

    /// Glyphs in the order macOS writes them: control, option, shift, command.
    public var glyphs: String {
        var text = ""
        if contains(.control) { text += "⌃" }
        if contains(.alt) { text += "⌥" }
        if contains(.shift) { text += "⇧" }
        if contains(.command) { text += "⌘" }
        return text
    }
}

/// Keyboard-page HID usages, which is what `UIKey.keyCode` carries (`UIKeyboardHIDUsage` raw values), and what they are called.
public enum HIDKey {
    public static let a = 0x04, e = 0x08, k = 0x0E, z = 0x1D
    public static let returnKey = 0x28, escape = 0x29, backspace = 0x2A, tab = 0x2B, space = 0x2C
    public static let f1 = 0x3A, f12 = 0x45, f13 = 0x68, f24 = 0x73
    public static let home = 0x4A, pageUp = 0x4B, deleteForward = 0x4C, end = 0x4D, pageDown = 0x4E
    public static let right = 0x4F, left = 0x50, down = 0x51, up = 0x52
    public static let leftControl = 0xE0, leftShift = 0xE1, leftAlt = 0xE2, leftCommand = 0xE3
    public static let rightControl = 0xE4, rightShift = 0xE5, rightAlt = 0xE6, rightCommand = 0xE7

    /// Usages a keyboard can report on the keyboard page: the letters up to the right-hand ⌘.
    public static let valid: ClosedRange<Int> = 0x04...0xE7

    public static func isModifier(_ code: Int) -> Bool { (0xE0...0xE7).contains(code) }
    /// The modifier a modifier key stands for (left and right alike).
    public static func modifier(of code: Int) -> ChordModifiers? {
        switch code {
        case leftControl, rightControl: .control
        case leftShift, rightShift: .shift
        case leftAlt, rightAlt: .alt
        case leftCommand, rightCommand: .command
        default: nil
        }
    }
    public static func isFunctionKey(_ code: Int) -> Bool { (f1...f12).contains(code) || (f13...f24).contains(code) }
    /// Keys that produce characters, or that a terminal already gives a meaning of its own (Return, Esc, Backspace, Tab, Space).
    /// A chord on one of these needs Ctrl, Alt or Command, or it would take that key away from typing.
    public static func isTyping(_ code: Int) -> Bool { (0x04...0x39).contains(code) || (0x54...0x67).contains(code) || code == 0x2A }

    public static func name(of code: Int) -> String {
        if (0x04...0x1D).contains(code) { return String(UnicodeScalar(UInt8(65 + code - 0x04))) }
        if (0x1E...0x26).contains(code) { return String(code - 0x1E + 1) }
        if code == 0x27 { return "0" }
        if (f1...f12).contains(code) { return "F\(code - f1 + 1)" }
        if (f13...f24).contains(code) { return "F\(code - f13 + 13)" }
        if (0x59...0x61).contains(code) { return "Keypad \(code - 0x59 + 1)" }
        switch code {
        case returnKey: return "Return"
        case escape: return "Esc"
        case backspace: return "Backspace"
        case tab: return "Tab"
        case space: return "Space"
        case 0x2D: return "-"
        case 0x2E: return "="
        case 0x2F: return "["
        case 0x30: return "]"
        case 0x31: return "\\"
        case 0x32: return "#"
        case 0x33: return ";"
        case 0x34: return "'"
        case 0x35: return "`"
        case 0x36: return ","
        case 0x37: return "."
        case 0x38: return "/"
        case 0x39: return "Caps Lock"
        case 0x46: return "Print Screen"
        case 0x47: return "Scroll Lock"
        case 0x48: return "Pause"
        case 0x49: return "Insert"
        case home: return "Home"
        case pageUp: return "Page Up"
        case deleteForward: return "Forward Delete"
        case end: return "End"
        case pageDown: return "Page Down"
        case right: return "→"
        case left: return "←"
        case down: return "↓"
        case up: return "↑"
        case 0x53: return "Num Lock"
        case 0x54: return "Keypad /"
        case 0x55: return "Keypad *"
        case 0x56: return "Keypad -"
        case 0x57: return "Keypad +"
        case 0x58: return "Keypad Enter"
        case 0x62: return "Keypad 0"
        case 0x63: return "Keypad ."
        case 0x64: return "\\ (ISO)"
        case 0x65: return "Application"
        case 0x66: return "Power"
        case leftControl: return "Left ⌃"
        case leftShift: return "Left ⇧"
        case leftAlt: return "Left ⌥"
        case leftCommand: return "Left ⌘"
        case rightControl: return "Right ⌃"
        case rightShift: return "Right ⇧"
        case rightAlt: return "Right ⌥"
        case rightCommand: return "Right ⌘"
        default: return "Key 0x" + String(code, radix: 16, uppercase: true)
        }
    }

    /// The character a letter, digit or punctuation key types on a US layout, for `UIKeyCommand.input`. Nil for the rest.
    public static func character(for code: Int) -> String? {
        if (0x04...0x1D).contains(code) { return String(UnicodeScalar(UInt8(97 + code - 0x04))) }
        if (0x1E...0x26).contains(code) { return String(code - 0x1E + 1) }
        if code == 0x27 { return "0" }
        switch code {
        case 0x2D: return "-"
        case 0x2E: return "="
        case 0x2F: return "["
        case 0x30: return "]"
        case 0x31: return "\\"
        case 0x33: return ";"
        case 0x34: return "'"
        case 0x35: return "`"
        case 0x36: return ","
        case 0x37: return "."
        case 0x38: return "/"
        default: return nil
        }
    }
    /// The inverse of `character(for:)` for a single lowercase letter, digit or punctuation character.
    public static func code(forCharacter text: String) -> Int? {
        guard text.unicodeScalars.count == 1 else { return nil }
        let lowered = text.lowercased()
        return (0x04...0x27).first { character(for: $0) == lowered } ?? (0x2D...0x38).first { character(for: $0) == lowered }
    }
}

public enum ChordError: Error, Equatable, LocalizedError, Sendable {
    case unknownKey
    case needsModifier
    case reserved(String)
    public var errorDescription: String? {
        switch self {
        case .unknownKey: "That key cannot be used for a shortcut."
        case .needsModifier: "Add Ctrl, Alt or Command: a bare typing key would stop typing."
        case .reserved(let name): "\(name) is reserved."
        }
    }
}

/// A key and the modifiers held with it: the thing a person presses to run a hotkey or open the hotkey menu.
///
/// It is matched by the key's HID usage and not by the character it types, so Option+E on a layout where it types "´" is still E.
/// A chord whose key is itself a modifier (Command, Option, Control, Shift, or whatever a keyboard's special button reports as one)
/// is a **tap**: it fires when that key is pressed and let go again without another key in between. That is how a lone dedicated
/// button, such as the Clicks keyboard's, can open the hotkey menu while still working as a modifier for other chords.
public struct KeyChord: Sendable, Hashable {
    public let keyCode: Int
    public let modifiers: ChordModifiers

    /// A tap chord carries no modifiers: the key itself is the whole chord.
    public init(keyCode: Int, modifiers: ChordModifiers = []) {
        self.keyCode = keyCode
        self.modifiers = HIDKey.isModifier(keyCode) ? [] : modifiers.intersection(.all)
    }
    public var isTap: Bool { HIDKey.isModifier(keyCode) }

    /// The reserved chords: ⌘K opens the hotkey menu and ⌘, the hotkey settings. They are always there.
    public static let paletteDefault = KeyChord(keyCode: HIDKey.k, modifiers: .command)
    public static let settingsDefault = KeyChord(keyCode: 0x36, modifiers: .command)

    public func validate() throws {
        guard HIDKey.valid.contains(keyCode) else { throw ChordError.unknownKey }
        if isTap { return }
        if self == Self.paletteDefault { throw ChordError.reserved("⌘K") }
        if self == Self.settingsDefault { throw ChordError.reserved("⌘,") }
        if HIDKey.isTyping(keyCode), modifiers.isDisjoint(with: [.control, .alt, .command]) { throw ChordError.needsModifier }
    }
    public var isValid: Bool { (try? validate()) != nil }

    /// "⌃⇧K", "⌥F5", or "Tap Left ⌘".
    public var title: String {
        isTap ? "Tap \(HIDKey.name(of: keyCode))" : modifiers.glyphs + HIDKey.name(of: keyCode)
    }
    /// The key's name alone, for lists where the modifiers are shown apart.
    public var keyName: String { HIDKey.name(of: keyCode) }

    // MARK: Storage form

    public var json: JSONValue { .object(["code": .number(Double(keyCode)), "mods": .number(Double(modifiers.rawValue))]) }
    /// Strict: an unusable chord is not accepted from storage either.
    public init(json: JSONValue) throws {
        guard case .object = json, case .number(let code) = json["code"], case .number(let mods) = json["mods"],
              code == code.rounded(), mods == mods.rounded(), code >= 0, code < 1_000, mods >= 0, mods < 16 else { throw ChordError.unknownKey }
        let chord = KeyChord(keyCode: Int(code), modifiers: ChordModifiers(rawValue: Int(mods)))
        try chord.validate()
        self = chord
    }
}

/// Turns a modifier key pressed and released on its own into a tap. Any other key in between, or a second modifier going down,
/// spoils it: the modifier was being used as a modifier.
public struct ModifierTapDetector: Sendable, Equatable {
    private var candidate: Int?
    private var held: Set<Int> = []
    public init() {}

    public mutating func keyDown(_ keyCode: Int) {
        let alone = held.isEmpty
        held.insert(keyCode)
        candidate = (HIDKey.isModifier(keyCode) && alone) ? keyCode : nil
    }
    /// The tap this release completes, if it does.
    public mutating func keyUp(_ keyCode: Int) -> KeyChord? {
        held.remove(keyCode)
        defer { if candidate == keyCode { candidate = nil } }
        return candidate == keyCode ? KeyChord(keyCode: keyCode) : nil
    }
    /// A key was used some other way while the modifier was held (a key command took it, so no press reached us): not a tap.
    public mutating func spoil() { candidate = nil }
    /// The window lost the keyboard (a sheet came up, the app went away): nothing is held any more.
    public mutating func reset() { candidate = nil; held = [] }
}

extension TerminalKey {
    /// The terminal key a chord stands for, when pressing it is how you would type that key: Esc, Tab, Shift-Tab, Return, the arrows,
    /// Home, End, Page Up and Down, Backspace, Forward Delete, and Ctrl plus a letter. Used to record a key step by pressing it.
    public init?(chord: KeyChord) {
        let code = chord.keyCode
        switch chord.modifiers {
        case []:
            switch code {
            case HIDKey.returnKey: self = .enter
            case HIDKey.tab: self = .tab
            case HIDKey.escape: self = .escape
            case HIDKey.backspace: self = .backspace
            case HIDKey.deleteForward: self = .delete
            case HIDKey.up: self = .up
            case HIDKey.down: self = .down
            case HIDKey.left: self = .left
            case HIDKey.right: self = .right
            case HIDKey.home: self = .home
            case HIDKey.end: self = .end
            case HIDKey.pageUp: self = .pageUp
            case HIDKey.pageDown: self = .pageDown
            default: return nil
            }
        case .shift:
            guard code == HIDKey.tab else { return nil }
            self = .backTab
        case .control:
            guard (HIDKey.a...HIDKey.z).contains(code), let letter = HIDKey.character(for: code)?.first else { return nil }
            self = .control(letter)
        default:
            return nil
        }
    }
}
