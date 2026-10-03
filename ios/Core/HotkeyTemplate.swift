import Foundation

/// A ready-made set of hotkeys with shortcuts, installed in one tap. Installing adds to what a person has and never removes or
/// changes anything of theirs.
public struct HotkeyTemplate: Sendable, Equatable, Identifiable {
    public let id: String
    public let name: String
    public let summary: String
    public let hotkeys: [Hotkey]
    /// Extra shortcuts that open the hotkey menu (⌘K always does).
    public let paletteChords: [KeyChord]
    public init(id: String, name: String, summary: String, hotkeys: [Hotkey], paletteChords: [KeyChord] = []) {
        self.id = id; self.name = name; self.summary = summary; self.hotkeys = hotkeys; self.paletteChords = paletteChords
    }
}

/// What installing a template did.
public struct TemplateMerge: Sendable, Equatable {
    public enum Reason: Sendable, Equatable {
        /// The same hotkey (id, or the same steps on the same shortcut) is there already.
        case alreadyThere
        /// Another hotkey of the person's already owns the shortcut; theirs is left alone.
        case shortcutTaken(by: String)
        /// The library is full.
        case libraryFull
    }
    public struct Skip: Sendable, Equatable {
        public let hotkey: Hotkey
        public let reason: Reason
    }
    public var added: [Hotkey] = []
    public var skipped: [Skip] = []
    /// Menu shortcuts that were not set before.
    public var paletteChordsAdded: [KeyChord] = []
    public var changedAnything: Bool { !added.isEmpty || !paletteChordsAdded.isEmpty }

    /// One line for a notice: "Added 14 hotkeys" or "Nothing new: all 14 are already installed".
    public var summary: String {
        func noun(_ n: Int) -> String { n == 1 ? "1 hotkey" : "\(n) hotkeys" }
        if added.isEmpty && skipped.isEmpty { return paletteChordsAdded.isEmpty ? "Nothing to install." : "Added the menu shortcut." }
        if added.isEmpty { return "Nothing new: \(skipped.count == 1 ? "it is" : "all \(skipped.count) are") already there or taken." }
        return skipped.isEmpty ? "Added \(noun(added.count))." : "Added \(noun(added.count)), skipped \(skipped.count) you already have."
    }
}

extension HotkeyLibrary {
    /// Adds the template's hotkeys that are not there yet. A hotkey is skipped when one with its id exists (installing twice changes
    /// nothing, and an edited copy is kept as edited), when the same steps already sit on the same shortcut, when the shortcut belongs
    /// to another hotkey or to something that is not a hotkey (`taken` names the owner of each such shortcut: the extra ones that
    /// open the hotkey menu and the hotkey help), or when the library is full. The person's hotkeys stay, in their order, and the new
    /// ones follow.
    @discardableResult
    public mutating func merge(_ template: HotkeyTemplate, taken: [KeyChord: String] = [:]) -> TemplateMerge {
        var result = TemplateMerge()
        for hotkey in template.hotkeys {
            if hotkeys.contains(where: { $0.id == hotkey.id || ($0.items == hotkey.items && $0.chord == hotkey.chord) }) {
                result.skipped.append(.init(hotkey: hotkey, reason: .alreadyThere)); continue
            }
            if let chord = hotkey.chord, let owner = taken[chord] {
                result.skipped.append(.init(hotkey: hotkey, reason: .shortcutTaken(by: owner))); continue
            }
            do {
                try add(hotkey)
                result.added.append(hotkey)
            } catch HotkeyError.chordInUse(let owner) {
                result.skipped.append(.init(hotkey: hotkey, reason: .shortcutTaken(by: owner)))
            } catch HotkeyError.tooManyHotkeys {
                result.skipped.append(.init(hotkey: hotkey, reason: .libraryFull))
            } catch {
                result.skipped.append(.init(hotkey: hotkey, reason: .alreadyThere))
            }
        }
        return result
    }
}

extension HotkeyTemplate {
    /// The Clicks keyboard for iPhone, which has no Esc, no arrows and, unless its Clicks Key is set to Ctrl, no Ctrl.
    ///
    /// Everything sits on the ⌘ key, which the keyboard has and iOS hands to apps, so it works whichever way the Clicks Key is set
    /// in the Clicks app. The rule is easy to hold in the head: **⌘ plus a letter is a key a terminal needs, ⌘⇧ plus a letter is Ctrl
    /// plus that letter.**
    ///
    /// | ⌘E Esc | ⌘T Tab | ⌘⇧T Shift-Tab | ⌘W ⌘A ⌘S ⌘D ↑ ← ↓ → | ⌘B Page Up | ⌘F Page Down |
    /// | ⌘⇧C ^C | ⌘⇧D ^D | ⌘⇧Z ^Z | ⌘⇧R ^R | ⌘⇧L ^L |
    ///
    /// None of these shortcuts takes a key a terminal types, and none is a shortcut iOS keeps for itself (⌘H, ⌘Space, ⌘Tab). They are
    /// shortcuts only, kept off the key bar, which has those keys already; the hotkey menu lists them with their shortcuts. A tap on
    /// Control alone also opens the hotkey menu: with the Clicks Key set to Ctrl in the Clicks app, that is a tap on the Clicks Key.
    public static let clicks: HotkeyTemplate = {
        func chord(_ letter: String, shift: Bool = false) -> KeyChord {
            KeyChord(keyCode: HIDKey.code(forCharacter: letter) ?? 0, modifiers: shift ? [.command, .shift] : .command)
        }
        func entry(_ slug: String, _ label: String, _ key: TerminalKey, _ chord: KeyChord) -> Hotkey {
            Hotkey(id: "template.clicks.\(slug)", label: label, steps: [.key(key)], chord: chord, showsOnBar: false)
        }
        let hotkeys = [
            entry("esc", "Esc", .escape, chord("e")),
            entry("tab", "Tab", .tab, chord("t")),
            entry("btab", "⇧Tab", .backTab, chord("t", shift: true)),
            entry("up", "↑", .up, chord("w")),
            entry("left", "←", .left, chord("a")),
            entry("down", "↓", .down, chord("s")),
            entry("right", "→", .right, chord("d")),
            entry("pgup", "PgUp", .pageUp, chord("b")),
            entry("pgdn", "PgDn", .pageDown, chord("f")),
            entry("ctrl-c", "^C", .control("c"), chord("c", shift: true)),
            entry("ctrl-d", "^D", .control("d"), chord("d", shift: true)),
            entry("ctrl-z", "^Z", .control("z"), chord("z", shift: true)),
            entry("ctrl-r", "^R", .control("r"), chord("r", shift: true)),
            entry("ctrl-l", "^L", .control("l"), chord("l", shift: true))
        ]
        return HotkeyTemplate(
            id: "clicks", name: "Clicks keyboard",
            summary: "Esc, Tab, Shift-Tab, arrows, Page Up/Down and Ctrl+C D Z R L on ⌘ shortcuts, for a keyboard without them.",
            hotkeys: hotkeys,
            paletteChords: [KeyChord(keyCode: HIDKey.leftControl), KeyChord(keyCode: HIDKey.rightControl)])
    }()

    public static let all: [HotkeyTemplate] = [.clicks]
}
