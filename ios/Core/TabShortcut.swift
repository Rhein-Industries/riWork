import Foundation

/// Clicks' physical Tab is an app shortcut prefix. A literal terminal/editor Tab is always Tab, Q.
public enum TabShortcutAction: Sendable, Equatable {
    case literalTab, escape, up, left, down, right, keyMenu, selectTab(Int), newTerminal, projects, modelPicker
}

/// Pure leader state: tapping Tab arms it, holding it reveals help, and an unknown key stays in help without typing into a shell.
public struct TabShortcutState: Sendable, Equatable {
    public private(set) var armed = false
    public private(set) var tabIsHeld = false
    public private(set) var guideVisible = false
    public private(set) var unknownShortcut = false
    public init() {}

    public struct Outcome: Sendable, Equatable {
        public let consumed: Bool
        public let action: TabShortcutAction?
        init(_ consumed: Bool, _ action: TabShortcutAction? = nil) { self.consumed = consumed; self.action = action }
    }

    public mutating func handle(_ event: KeyEventRecord) -> Outcome {
        guard let code = event.keyCode else { return Outcome(false) }
        if event.phase == .cancelled { let wasArmed = armed; cancel(); return Outcome(wasArmed) }
        if event.phase == .up {
            if code == HIDKey.tab, tabIsHeld { tabIsHeld = false; return Outcome(true) }
            return Outcome(false)
        }
        guard event.phase == .down || event.phase == .command else { return Outcome(false) }
        if code == HIDKey.tab, event.modifiers.isEmpty {
            // Only a physical press is held: a key command has no release to wait for, so it can never grow into the hold guide.
            armed = true; tabIsHeld = event.phase == .down; unknownShortcut = false
            return Outcome(true)
        }
        guard armed else { return Outcome(false) }
        if HIDKey.isModifier(code) { return Outcome(false) }
        if code == HIDKey.escape || (code == HIDKey.e && event.modifiers == .command) { cancel(); return Outcome(true) }
        guard event.modifiers.isDisjoint(with: [.command, .control, .alt]) else {
            guideVisible = true; unknownShortcut = true; return Outcome(true)
        }
        let character = (event.characters.isEmpty ? event.charactersIgnoringModifiers : event.characters).lowercased()
        if character == "h" || character == "?" { guideVisible = true; unknownShortcut = false; return Outcome(true) }
        let action: TabShortcutAction?
        switch character {
        case "q": action = .literalTab
        case "e": action = .escape
        case "w": action = .up
        case "a": action = .left
        case "s": action = .down
        case "d": action = .right
        case "k": action = .keyMenu
        case "n": action = .newTerminal
        case "p": action = .projects
        case "m": action = .modelPicker
        case "1", "2", "3", "4", "5", "6", "7", "8", "9": action = Int(character).map(TabShortcutAction.selectTab)
        default: action = nil
        }
        guard let action else { guideVisible = true; unknownShortcut = true; return Outcome(true) }
        cancel()
        return Outcome(true, action)
    }

    /// Called by the adapter's one-second timer only while the same physical Tab remains down.
    public mutating func heldLongEnough() { if armed && tabIsHeld { guideVisible = true } }
    public mutating func cancel() { armed = false; tabIsHeld = false; guideVisible = false; unknownShortcut = false }
}
