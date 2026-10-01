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
