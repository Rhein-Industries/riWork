import Foundation
import GameController
import Observation
import UIKit
import RiWorkCore

/// Whether a hardware keyboard is attached, as GameController reports it (`GCKeyboard.coalesced`). With one attached iOS hides the
/// software keyboard and shows only the key bar, so focusing the terminal costs no screen; without one it pops the keyboard up.
@MainActor @Observable final class HardwareKeyboardMonitor {
    private(set) var isAttached: Bool
    @ObservationIgnored private let probe: () -> Bool
    @ObservationIgnored private var observers: [any NSObjectProtocol] = []

    init(probe: @escaping () -> Bool = { GCKeyboard.coalesced != nil }) {
        self.probe = probe
        isAttached = probe()
        for name in [Notification.Name.GCKeyboardDidConnect, .GCKeyboardDidDisconnect] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.refresh() }
            })
        }
    }
    func refresh() { isAttached = probe() }
}

/// Whether the software keyboard is on screen, from the keyboard's frame notifications. GameController can report a keyboard (one that
/// is paired, a keyboard case) while the on-screen keyboard is up all the same: what can be put away is what is shown, not what is
/// attached. With a hardware keyboard in use only the shortcut bar shows, well under `shownHeight`.
///
/// The frame is the keyboard's on its screen; what counts is how much of the app's window it covers: in Stage Manager the window need
/// not reach the screen's bottom, and it may be on another display.
///
/// It also says whether that keyboard is **docked** at the bottom of the screen (`KeyboardDocking.isDocked`), the only keyboard a
/// chat's composer stands up for: a floating, split or undocked one, and a hardware keyboard's shortcut bar, leave it at the bottom of
/// the window. The monitor lives as long as the app, so a chat screen that opens with the keyboard already up knows at once.
@MainActor @Observable final class SoftwareKeyboardMonitor {
    static let shownHeight = CGFloat(KeyboardDocking.dockedMinimumHeight)
    private(set) var isShown = false
    private(set) var isDocked = false
    @ObservationIgnored private var observers: [any NSObjectProtocol] = []

    init() {
        // Every notification is judged on its own frame, so their order (rotation, a move to another display, floating and splitting)
        // never matters; the did-notifications settle it on the frame the keyboard ended at.
        let hides: Set<Notification.Name> = [UIResponder.keyboardWillHideNotification, UIResponder.keyboardDidHideNotification]
        for name in [UIResponder.keyboardWillShowNotification, UIResponder.keyboardWillChangeFrameNotification, UIResponder.keyboardWillHideNotification,
                     UIResponder.keyboardDidShowNotification, UIResponder.keyboardDidChangeFrameNotification, UIResponder.keyboardDidHideNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] note in
                let hiding = hides.contains(note.name)
                let end = (note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? NSValue)?.cgRectValue ?? .zero
                // The keyboard's screen, by identity only: it is looked up again among the scenes on the main actor.
                let screen = (note.object as? UIScreen).map(ObjectIdentifier.init)
                let screenSize = (note.object as? UIScreen)?.bounds.size
                MainActor.assumeIsolated { self?.update(hiding: hiding, end: end, screen: screen, screenSize: screenSize) }
            })
        }
    }
    func update(hiding: Bool, end: CGRect, screen: ObjectIdentifier? = nil, screenSize: CGSize? = nil) {
        isShown = !hiding && Self.windowOverlap(of: end, on: screen) >= Self.shownHeight
        let size = screenSize ?? UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first?.screen.bounds.size ?? .zero
        let docked = KeyboardDocking.isDocked(hiding: hiding, x: Double(end.minX), y: Double(end.minY), width: Double(end.width), height: Double(end.height),
                                              screenWidth: Double(size.width), screenHeight: Double(size.height))
        if docked != isDocked { isDocked = docked }
    }
    /// How much of the app's window (the key one, on `screen` when it is known) a keyboard at `frame` on its screen covers.
    static func windowOverlap(of frame: CGRect, on screen: ObjectIdentifier?) -> CGFloat {
        let windows = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .filter { screen == nil || ObjectIdentifier($0.screen) == screen }.flatMap(\.windows)
        guard let window = windows.first(where: \.isKeyWindow) ?? windows.first, let space = window.windowScene?.screen.coordinateSpace else { return 0 }
        return window.bounds.intersection(window.convert(frame, from: space)).height
    }
}

/// The last key events the app received, for the readout that shows what a key (the Clicks button, say) really sends.
@MainActor @Observable final class KeyEventLog {
    private(set) var last: KeyEventRecord?
    private(set) var count = 0
    func record(_ event: KeyEventRecord) { last = event; count &+= 1 }
}

/// Keyboard preferences: when the terminal takes the keyboard by itself, and the key readout. Kept in UserDefaults.
@MainActor @Observable final class KeyboardPrefs {
    static let focusKey = "riwork.focusKeyboard", showKeyEventsKey = "riwork.showKeyEvents"
    private(set) var focusSetting: KeyboardFocusSetting
    private(set) var showKeyEvents: Bool
    let hardware: HardwareKeyboardMonitor
    let software = SoftwareKeyboardMonitor()
    let events = KeyEventLog()
    @ObservationIgnored private let defaults: UserDefaults

    init(defaults: UserDefaults, hardware: HardwareKeyboardMonitor = HardwareKeyboardMonitor()) {
        self.defaults = defaults
        self.hardware = hardware
        focusSetting = KeyboardFocusSetting(stored: defaults.string(forKey: Self.focusKey))
        showKeyEvents = defaults.bool(forKey: Self.showKeyEventsKey)
    }
    func setFocusSetting(_ setting: KeyboardFocusSetting) {
        guard setting != focusSetting else { return }
        focusSetting = setting
        defaults.set(setting.rawValue, forKey: Self.focusKey)
    }
    func setShowKeyEvents(_ on: Bool) {
        guard on != showKeyEvents else { return }
        showKeyEvents = on
        defaults.set(on, forKey: Self.showKeyEventsKey)
    }
}

/// The open hotkey menu, if there is one. The capture view drives it from the keyboard and the SwiftUI overlay draws it and takes
/// taps, so the menu works with a hardware keyboard, the software one, and touch alike.
@MainActor @Observable final class PaletteController {
    private(set) var state: HotkeyPalette?
    var isOpen: Bool { state != nil }
    /// What a choice came to; the capture view sends the keys or opens the editor.
    @ObservationIgnored var onOutcome: ((HotkeyPalette.Outcome) -> Void)?

    func open(hotkeys: [Hotkey]) { state = HotkeyPalette(hotkeys: hotkeys) }
    func close() { state = nil }
    func toggle(hotkeys: [Hotkey]) { if isOpen { close() } else { open(hotkeys: hotkeys) } }
    func apply(_ input: HotkeyPalette.Input) {
        guard var current = state else { return }
        let outcome = current.apply(input)
        state = current
        if outcome != .none { onOutcome?(outcome) }
    }
    func choose(index: Int) {
        guard var current = state else { return }
        let outcome = current.choose(index: index)
        state = current
        if outcome != .none { onOutcome?(outcome) }
    }
}

/// The open hotkey help (⌘/), if there is one. It is not modal: the capture view keeps the keyboard, so typing, arrows and chords go on
/// working under it, and a chord or a tap on a row sends its hotkey and leaves the help up for the next one.
@MainActor @Observable final class HelpController {
    private(set) var state: HotkeyHelp?
    var isOpen: Bool { state != nil }
    /// A tap on a row; the capture view sends the hotkey.
    @ObservationIgnored var onFire: ((Hotkey) -> Void)?

    func open(hotkeys: [Hotkey], shortcuts: ShortcutSettings) { state = HotkeyHelp(hotkeys: hotkeys, shortcuts: shortcuts) }
    func close() { state = nil }
    func fire(_ hotkey: Hotkey) {
        guard isOpen else { return }
        onFire?(hotkey)
    }
}
