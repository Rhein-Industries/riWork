import Foundation
import GameController
import Observation
import UIKit
import RiWorkCore

/// Whether a hardware keyboard is attached, as GameController reports it (`GCKeyboard.coalesced`). With one attached iOS hides the
/// software keyboard and shows only the key bar, so focusing the terminal costs no screen; without one it pops the keyboard up.
@MainActor @Observable final class HardwareKeyboardMonitor {
    private(set) var isAttached: Bool
    private(set) var vendorName: String?
    @ObservationIgnored private let probe: () -> Bool
    @ObservationIgnored private let nameProbe: () -> String?
    @ObservationIgnored private var observers: [any NSObjectProtocol] = []

    init(probe: @escaping () -> Bool = { GCKeyboard.coalesced != nil }, nameProbe: @escaping () -> String? = { GCKeyboard.coalesced?.vendorName }) {
        self.probe = probe
        self.nameProbe = nameProbe
        isAttached = probe()
        vendorName = isAttached ? nameProbe() : nil
        for name in [Notification.Name.GCKeyboardDidConnect, .GCKeyboardDidDisconnect, UIApplication.didBecomeActiveNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.refresh() }
            })
        }
    }
    func refresh() {
        isAttached = probe()
        vendorName = isAttached ? nameProbe() : nil
    }
}

/// Whether the software keyboard is on screen, from the keyboard's frame notifications. GameController can report a keyboard (one that
/// is paired, a keyboard case) while the on-screen keyboard is up all the same: what can be put away is what is shown, not what is
/// attached. With a hardware keyboard in use only the shortcut bar shows, well under `shownHeight`.
@MainActor @Observable final class SoftwareKeyboardMonitor {
    static let shownHeight: CGFloat = 120
    private(set) var isShown = false
    @ObservationIgnored private var observers: [any NSObjectProtocol] = []

    init() {
        for name in [UIResponder.keyboardWillChangeFrameNotification, UIResponder.keyboardWillHideNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] note in
                let hiding = note.name == UIResponder.keyboardWillHideNotification
                let end = (note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? NSValue)?.cgRectValue ?? .zero
                MainActor.assumeIsolated { self?.update(hiding: hiding, end: end) }
            })
        }
    }
    func update(hiding: Bool, end: CGRect) {
        let screen = UIScreen.main.bounds
        isShown = !hiding && screen.intersection(end).height >= Self.shownHeight
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
    static let focusKey = "riwork.focusKeyboard", showKeyEventsKey = "riwork.showKeyEvents", layoutKey = "riwork.hardwareKeyboardLayout"
    private(set) var focusSetting: KeyboardFocusSetting
    private(set) var showKeyEvents: Bool
    private(set) var layout: HardwareKeyboardLayout
    var usesClicks: Bool { layout.usesClicks(attached: hardware.isAttached, vendorName: hardware.vendorName) }
    /// The software keyboard can be shown with a case attached. Keep its touch layout until it is put away again.
    var usesHardwareLayout: Bool { hardware.isAttached && !software.isShown }
    let hardware: HardwareKeyboardMonitor
    let software = SoftwareKeyboardMonitor()
    let events = KeyEventLog()
    let tabShortcuts = TabShortcutController()
    private(set) var modelPickerRequests = 0
    @ObservationIgnored private let defaults: UserDefaults

    init(defaults: UserDefaults, hardware: HardwareKeyboardMonitor = HardwareKeyboardMonitor()) {
        self.defaults = defaults
        self.hardware = hardware
        focusSetting = KeyboardFocusSetting(stored: defaults.string(forKey: Self.focusKey))
        layout = HardwareKeyboardLayout(stored: defaults.string(forKey: Self.layoutKey))
        showKeyEvents = defaults.bool(forKey: Self.showKeyEventsKey)
        tabShortcuts.isEnabled = { [weak self] in self?.usesClicks == true && self?.usesHardwareLayout == true }
        tabShortcuts.modalPresented = { Self.keyWindowIsCovered() }
    }
    /// Key presses go to the foreground scene's key window: whether a sheet, alert or popover is up over its screen.
    static func keyWindowIsCovered() -> Bool {
        UIApplication.shared.connectedScenes.contains { scene in
            guard let scene = scene as? UIWindowScene, scene.activationState == .foregroundActive else { return false }
            return scene.keyWindow?.rootViewController?.presentedViewController != nil
        }
    }
    func requestModelPicker() { modelPickerRequests &+= 1 }
    func setFocusSetting(_ setting: KeyboardFocusSetting) {
        guard setting != focusSetting else { return }
        focusSetting = setting
        defaults.set(setting.rawValue, forKey: Self.focusKey)
    }
    func setLayout(_ layout: HardwareKeyboardLayout) {
        guard layout != self.layout else { return }
        self.layout = layout
        tabShortcuts.cancel()
        defaults.set(layout.rawValue, forKey: Self.layoutKey)
    }
    func setShowKeyEvents(_ on: Bool) {
        guard on != showKeyEvents else { return }
        showKeyEvents = on
        defaults.set(on, forKey: Self.showKeyEventsKey)
    }
}

/// One leader shared by terminal input, chat text and keyboard-navigated menus. App handlers belong to the visible screen.
@MainActor @Observable final class TabShortcutController {
    private(set) var state = TabShortcutState()
    var isEnabled: () -> Bool = { false }
    /// A sheet, alert or popover is over the screen. Under one without the guide (`tabShortcutGuide`) the prefix is off: it could not
    /// be seen there, and its Tab Q would reach the shell or draft hidden behind it.
    @ObservationIgnored var modalPresented: () -> Bool = { false }
    @ObservationIgnored private var timer: Task<Void, Never>?
    /// Physical presses the prefix took, kept until their release so neither it nor a key-command echo or repeat of them acts again.
    @ObservationIgnored private var heldCodes: Set<Int> = []
    /// A key command the prefix took (⌘E cancelling it, say). Holding it repeats the command with no press in between: the repeats are
    /// the prefix's too, until a press or release arrives.
    @ObservationIgnored private var repeatingCommand: KeyChord?
    @ObservationIgnored private var appOwner: UUID?
    @ObservationIgnored private var appHandler: ((TabShortcutAction) -> Void)?
    var guideVisible: Bool { state.guideVisible && isEnabled() }
    var armed: Bool { state.armed && isEnabled() }

    @ObservationIgnored private var suspensions: Set<UUID> = []
    func setAppHandler(owner: UUID, handler: @escaping (TabShortcutAction) -> Void) { appOwner = owner; appHandler = handler }
    /// A sheet is over the screen: its handler must not act behind it (switch tabs under an unsaved form, say).
    func suspendApp(owner: UUID) { suspensions.insert(owner) }
    func resumeApp(owner: UUID) { suspensions.remove(owner) }
    func clearAppHandler(owner: UUID) { if appOwner == owner { appOwner = nil; appHandler = nil; cancelPrefix() } }

    @discardableResult
    func handle(_ event: KeyEventRecord, markedText: Bool = false, perform: (TabShortcutAction) -> Bool) -> Bool {
        guard isEnabled(), !markedText, !(suspensions.isEmpty && modalPresented()) else { cancel(); return false }
        guard let code = event.keyCode else { return false }
        if event.phase == .command {
            if repeatingCommand == KeyChord(keyCode: code, modifiers: event.modifiers) { return true }
        } else { repeatingCommand = nil }
        if event.phase == .up {
            let used = heldCodes.remove(code) != nil
            let outcome = update { $0.handle(event) }
            if code == HIDKey.tab { timer?.cancel(); timer = nil }
            return used || outcome.consumed
        }
        if event.phase == .cancelled { let used = heldCodes.contains(code) || state.armed; cancel(); return used }
        if event.phase == .down {
            // UIKit begins each physical press once (a held key repeats as text or key commands): a claim left by a press whose release
            // went to another responder is stale, and this press is a new one.
            heldCodes.remove(code)
        } else if heldCodes.contains(code) {
            // A key-command echo or repeat of a claimed press cannot run an action twice.
            return true
        }
        let outcome = update { $0.handle(event) }
        guard outcome.consumed else { return false }
        // A press is held until its release; a key command has none, so its repeats are caught by the command itself.
        if event.phase == .down { heldCodes.insert(code) } else { repeatingCommand = KeyChord(keyCode: code, modifiers: event.modifiers) }
        if code == HIDKey.tab, state.tabIsHeld {
            timer?.cancel()
            timer = Task { [weak self] in
                do { try await Task.sleep(for: .seconds(1)) } catch { return }
                self?.update { $0.heldLongEnough() }
            }
        }
        if let action = outcome.action {
            timer?.cancel(); timer = nil
            if !perform(action), suspensions.isEmpty { appHandler?(action) }
        }
        return true
    }
    /// Observers hear of the state only when it changes, not on every key that leaves it as it was.
    @discardableResult private func update<Result>(_ change: (inout TabShortcutState) -> Result) -> Result {
        var next = state
        let result = change(&next)
        if next != state { state = next }
        return result
    }
    /// A screen/focus change dismisses the prefix, but a key already claimed by it
    /// remains claimed in the next responder until its physical release arrives.
    func cancelPrefix() { timer?.cancel(); timer = nil; update { $0.cancel() } }
    /// Physical cancellation, IME/disabled routing and keyboard/app lifecycle resets
    /// terminate the event stream, so no future key-up can be relied on.
    func cancel() { cancelPrefix(); heldCodes.removeAll(); repeatingCommand = nil }
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
