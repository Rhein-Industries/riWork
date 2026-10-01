import Foundation

/// "Focus keyboard when a shell opens": whether the terminal takes the keyboard by itself once a shell is ready.
public enum KeyboardFocusSetting: String, CaseIterable, Sendable, Identifiable {
    /// Always, which brings up the software keyboard when there is no hardware one.
    case always
    /// Only with a hardware keyboard attached, where focusing costs no screen. The default.
    case hardwareKeyboard
    case never

    public static let `default` = KeyboardFocusSetting.hardwareKeyboard
    public var id: String { rawValue }
    public var title: String {
        switch self {
        case .always: "Always"
        case .hardwareKeyboard: "With a hardware keyboard"
        case .never: "Never"
        }
    }
    /// Anything unknown or missing is the default.
    public init(stored: String?) { self = stored.flatMap(Self.init(rawValue:)) ?? .default }
}

/// What the screen looks like at the moment a shell asks for the keyboard.
public struct KeyboardFocusContext: Sendable, Equatable {
    /// A hardware keyboard is attached (GameController's `GCKeyboard.coalesced`).
    public var hardwareKeyboard: Bool
    /// A sheet, alert, the hotkey editor or another text field has the keyboard or the screen.
    public var obscured: Bool
    /// The terminal already has the keyboard.
    public var alreadyFocused: Bool
    /// The person hid the keyboard on purpose in this shell, and has not asked for it since.
    public var dismissedHere: Bool
    /// Direct typing is on and the shell is alive.
    public var canType: Bool
    public init(hardwareKeyboard: Bool, obscured: Bool = false, alreadyFocused: Bool = false, dismissedHere: Bool = false, canType: Bool = true) {
        self.hardwareKeyboard = hardwareKeyboard; self.obscured = obscured; self.alreadyFocused = alreadyFocused
        self.dismissedHere = dismissedHere; self.canType = canType
    }
}

public enum KeyboardFocusDecision: Sendable, Equatable {
    public enum Reason: Sendable, Equatable { case setting, cannotType, obscured, alreadyFocused, dismissedByUser, noHardwareKeyboard }
    case focus
    case skip(Reason)
    public var shouldFocus: Bool { self == .focus }
}

public enum KeyboardFocusPolicy {
    /// The one place that decides whether the terminal takes the keyboard on its own. The order matters: what is true of the screen
    /// right now (a sheet, a text field) beats what was chosen in general, and a keyboard the person put away stays away.
    public static func decide(setting: KeyboardFocusSetting, context: KeyboardFocusContext) -> KeyboardFocusDecision {
        if setting == .never { return .skip(.setting) }
        if !context.canType { return .skip(.cannotType) }
        if context.obscured { return .skip(.obscured) }
        if context.alreadyFocused { return .skip(.alreadyFocused) }
        if context.dismissedHere { return .skip(.dismissedByUser) }
        if setting == .hardwareKeyboard && !context.hardwareKeyboard { return .skip(.noHardwareKeyboard) }
        return .focus
    }
}

/// Remembers which shell is ready and where the person put the keyboard away. Pure, so the rules are tested without a screen.
///
/// "Ready" is a shell that is connected and shows its first live screen. A new ready shell (another one chosen, a reconnect, the app
/// coming back from the background) is a moment to consider focusing; the same shell staying ready is not.
public struct ShellFocusTracker: Sendable, Equatable {
    public static let maxRemembered = 32
    public private(set) var readyShell: String?
    private var dismissed: [String] = []
    public init() {}

    /// Feed the ready shell (nil when none is). True when a shell has just become ready, so the focus decision should be made.
    public mutating func readiness(_ shell: String?) -> Bool {
        let arrived = shell != nil && shell != readyShell
        readyShell = shell
        return arrived
    }
    public func isDismissed(_ shell: String?) -> Bool { shell.map(dismissed.contains) ?? false }

    /// The person hid the keyboard here (the key bar's Hide, the bar below the terminal). Not a sheet taking the keyboard.
    public mutating func userDismissed(_ shell: String?) {
        guard let shell, !dismissed.contains(shell) else { return }
        dismissed.append(shell)
        if dismissed.count > Self.maxRemembered { dismissed.removeFirst(dismissed.count - Self.maxRemembered) }
    }
    /// The person asked for the keyboard here (tap, Show keyboard): auto-focus may resume for this shell.
    public mutating func userFocused(_ shell: String?) {
        guard let shell else { return }
        dismissed.removeAll { $0 == shell }
    }
    /// The selected shell was replaced without a tap (it closed). Keystrokes meant for one shell must not flow into another one
    /// the person did not pick, so the new shell waits for a tap.
    public mutating func shellReplacedWithoutTap(_ shell: String?) { userDismissed(shell) }
}
