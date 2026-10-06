import Foundation

// The top of a project's tab screen, decided apart from drawing. Every kind of tab (a terminal, a chat, an orchestrator the phone cannot
// open, or no tab at all) gets the same one row: Back, the tab strip, New terminal and the screen's menu, at the same height, with
// one-line tabs. Switching tabs therefore never adds, removes or resizes a bar, and what is under the row starts at the same place.
// Only what the menu offers depends on the kind (a terminal's own actions: focus mode, text size, copy screen, ...). Focus mode is the
// one exception, and only for a terminal: it drops the row (and the status bar) to give the whole screen to the shell.

/// What the tab screen shows under the row.
public enum TabScreenContent: Sendable, Hashable, CaseIterable {
    /// A terminal's output (a shell, an agent or an orchestrator in a terminal).
    case terminal
    /// A chat (a chat tab, or an orchestrator that runs as a chat).
    case chat
    /// An orchestrator the phone cannot open, in place of a terminal or a chat.
    case unavailable
    /// No tab selected, or none open.
    case none
}

public struct TabScreenChrome: Sendable, Hashable {
    /// Whether the navigation row is drawn at all.
    public enum Header: Sendable, Hashable {
        /// Back, the tab strip, New terminal and the menu, in one row.
        case navigationRow
        /// Nothing above the content (focus mode).
        case hidden
    }
    /// The height of the row and of each (one-line) tab in it at an interface scale: a full touch target, never below 44 points.
    public static func rowHeight(scale: Double) -> Double { BottomBarGeometry.target(scale: scale) }

    public let header: Header
    /// The menu has the terminal's own section (focus mode, jump to latest, display, text size, line composer, copy, refresh output,
    /// close this terminal).
    public let terminalActions: Bool
    /// The status bar is hidden (focus mode).
    public let statusBarHidden: Bool

    /// The chrome for what is on screen. `focusMode` is the person's focus-mode setting; it applies only while a terminal is shown.
    public static func decide(content: TabScreenContent, focusMode: Bool) -> TabScreenChrome {
        let focused = focusMode && content == .terminal
        return TabScreenChrome(header: focused ? .hidden : .navigationRow, terminalActions: content == .terminal, statusBarHidden: focused)
    }
}
