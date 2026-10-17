import Foundation

/// Whether the keyboard is a software keyboard docked at the bottom of its screen, the only one a chat's composer stands up for. A
/// floating, split or undocked keyboard does not reach the screen's bottom (or is announced as hidden), a hardware keyboard shows only its
/// shortcut bar, which is well under `dockedMinimumHeight`, and a keyboard frame below the screen (one on its way out) is no keyboard.
public enum KeyboardDocking {
    /// The least height of a docked software keyboard on screen; the shortcut bar of a hardware keyboard is about half of this.
    public static let dockedMinimumHeight: Double = 120

    /// From one keyboard notification alone, so their order does not matter: its end frame on its screen (`x`, `y`, `width`, `height`)
    /// and that screen's size. A hide (will or did) is never docked; otherwise the part of the frame on the screen must be at least
    /// `dockedMinimumHeight` tall and reach the screen's bottom edge.
    public static func isDocked(hiding: Bool, x: Double, y: Double, width: Double, height: Double, screenWidth: Double, screenHeight: Double) -> Bool {
        guard !hiding, width > 0, height > 0, screenWidth > 0, screenHeight > 0 else { return false }
        let top = max(y, 0), bottom = min(y + height, screenHeight)
        let left = max(x, 0), right = min(x + width, screenWidth)
        guard right - left >= 1, bottom - top >= dockedMinimumHeight else { return false }
        return y + height >= screenHeight - 1
    }
}
