import Foundation
import Observation
import RiWorkCore

/// The colors the app is drawn with, and the last palette known for each paired desktop.
///
/// - What is shown: the palette of the desktop that is selected (or, before any is, the most recently used one), and the
///   built-in style while there is none.
/// - What is kept: the last valid `appearance.get` result per desktop, in UserDefaults (colors are not secret). It is applied
///   at launch and on connect, before the first fetch returns, and survives a desktop that is offline or has not published.
/// - What changes: only an actually different palette. A fetch that returns the same colors does not touch the theme, the
///   stored copy or any view.
@MainActor @Observable final class ThemeStore {
    /// The theme being drawn. Views read `style` through the environment; setting it re-colors them in place.
    private(set) var style = DesktopStyle.builtIn
    /// The palette behind `style`, if the shown desktop has one.
    private(set) var appearance: DesktopAppearance?
    /// The desktop whose palette is shown.
    private(set) var shownDesktopID: String?
    /// The shown desktop's mic setting turned on or off (`DesktopStyle.mic`).
    @ObservationIgnored var onMicChange: ((Bool) -> Void)?
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var cache: [String: DesktopAppearance] = [:]
    /// The interface size every style is drawn at (see `InterfaceScale`).
    private(set) var scale = InterfaceScale.standard

    static let keyPrefix = "riwork.appearance."
    static let recentKey = "riwork.appearance.recent"
    static let recentLimit = 8
    static func key(for desktopID: String) -> String { keyPrefix + desktopID }

    init(defaults: UserDefaults) { self.defaults = defaults }

    // MARK: Persistence

    /// The stored palette for a desktop. Anything that no longer passes the parser is discarded, not trusted.
    func stored(for desktopID: String) -> DesktopAppearance? {
        if let cached = cache[desktopID] { return cached }
        guard let text = defaults.string(forKey: Self.key(for: desktopID)) else { return nil }
        guard let value = try? JSONDecoder().decode(JSONValue.self, from: Data(text.utf8)), let parsed = try? DesktopAppearance(json: value) else {
            defaults.removeObject(forKey: Self.key(for: desktopID))
            return nil
        }
        cache[desktopID] = parsed
        return parsed
    }
    private func save(_ appearance: DesktopAppearance, for desktopID: String) {
        cache[desktopID] = appearance
        if let data = try? JSONEncoder().encode(appearance.json), let text = String(data: data, encoding: .utf8) { defaults.set(text, forKey: Self.key(for: desktopID)) }
    }
    var recentDesktopIDs: [String] { defaults.stringArray(forKey: Self.recentKey) ?? [] }
    private func markUsed(_ desktopID: String) {
        defaults.set(Array(([desktopID] + recentDesktopIDs.filter { $0 != desktopID }).prefix(Self.recentLimit)), forKey: Self.recentKey)
    }

    // MARK: What is shown

    /// The style for any desktop: the shown one, or its stored palette. Used for screens that belong to one desktop
    /// (renaming it) while another one is shown.
    func style(for desktopID: String) -> DesktopStyle {
        if desktopID == shownDesktopID { return style }
        return stored(for: desktopID).map { DesktopStyle(DesktopTheme.resolve($0), scale: scale) } ?? DesktopStyle(.builtIn, scale: scale)
    }
    /// A new interface size: every style is redrawn at it.
    func setScale(_ value: Double) {
        let next = InterfaceScale.clamped(value)
        guard next != scale else { return }
        scale = next
        apply(appearance)
    }
    /// Launch, or the library changed: show the selected desktop, else the most recently used one that still exists.
    func showInitial(selected: String?, existing: [String]) {
        show(selected ?? recentDesktopIDs.first(where: existing.contains))
    }
    /// A desktop was chosen. It becomes the most recently used one and its stored palette is applied at once.
    func select(_ desktopID: String) {
        if recentDesktopIDs.first != desktopID { markUsed(desktopID) }
        if shownDesktopID != desktopID { show(desktopID) }
    }
    private func show(_ desktopID: String?) {
        shownDesktopID = desktopID
        apply(desktopID.flatMap(stored(for:)))
    }
    private func apply(_ appearance: DesktopAppearance?) {
        let next = DesktopStyle(DesktopTheme.resolve(appearance), scale: scale)
        self.appearance = appearance
        if next.mic != style.mic { onMicChange?(next.mic) }
        // An equal theme is not assigned: observers of `style` are not woken for nothing.
        if next != style { style = next }
    }

    // MARK: Updates

    /// A fresh `appearance.get` result. Returns true when the colors changed (and were stored and, if shown, applied).
    @discardableResult
    func receive(_ fetched: DesktopAppearance, for desktopID: String) -> Bool {
        if let known = stored(for: desktopID), known.sameLook(as: fetched) { return false }
        save(fetched, for: desktopID)
        if desktopID == shownDesktopID { apply(fetched) }
        return true
    }
    /// The desktop no longer offers colors (it was downgraded): forget its palette and go back to the built-in style.
    func clear(_ desktopID: String) {
        cache[desktopID] = nil
        defaults.removeObject(forKey: Self.key(for: desktopID))
        if desktopID == shownDesktopID { apply(nil) }
    }
    /// The pairing was removed.
    func forget(_ desktopID: String) {
        clear(desktopID)
        defaults.set(recentDesktopIDs.filter { $0 != desktopID }, forKey: Self.recentKey)
        if desktopID == shownDesktopID { shownDesktopID = nil }
    }
}
