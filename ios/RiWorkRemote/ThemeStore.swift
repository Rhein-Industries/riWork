import Foundation
import Observation
import RiWorkCore
import UIKit

/// The colors the app is drawn with, and the last palette known for each paired desktop.
///
/// - What is shown: an independent phone palette (Native by default), with
///   native iOS controls; the selected desktop's palette on iPad.
/// - What is kept: the last valid `appearance.get` result per desktop, in UserDefaults (colors are not secret). It is applied
///   at launch and on connect, before the first fetch returns, and survives a desktop that is offline or has not published.
/// - What changes: only an actually different palette. A fetch that returns the same colors does not touch the theme, the
///   stored copy or any view.
@MainActor @Observable final class ThemeStore {
    /// The theme being drawn. Views read `style` through the environment; setting it re-colors them in place.
    private(set) var style: DesktopStyle
    /// The phone keeps native controls, independent of its chosen color palette.
    let usesNativePhoneAppearance: Bool
    /// Color choices are local to this phone and do not update a desktop setting.
    private(set) var phonePaletteID: String
    let phonePalettes: [PhonePalette]
    let phonePaletteError: String?
    /// The selected desktop's appearance, including functional settings such as
    /// dictation. Its colors apply on iPhone only with "Follow desktop" selected.
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
    static let phonePaletteKey = "riwork.phone.palette"
    static func key(for desktopID: String) -> String { keyPrefix + desktopID }

    init(defaults: UserDefaults, nativePhone: Bool? = nil) {
        self.defaults = defaults
        let followsPhone = nativePhone ?? (UIDevice.current.userInterfaceIdiom == .phone)
        usesNativePhoneAppearance = followsPhone
        let catalog = Self.loadPhonePalettes()
        phonePalettes = catalog.presets
        phonePaletteError = catalog.error
        let storedID = defaults.string(forKey: Self.phonePaletteKey) ?? "native"
        let paletteID = Self.validPhonePalette(storedID, presets: catalog.presets) ? storedID : "native"
        phonePaletteID = paletteID
        style = DesktopStyle(Self.resolvedTheme(nil, nativePhone: followsPhone, paletteID: paletteID, presets: catalog.presets))
    }

    private static func loadPhonePalettes() -> (presets: [PhonePalette], error: String?) {
        do {
            guard let url = Bundle.main.url(forResource: "color-palettes", withExtension: "json") else {
                return ([], "The color palette catalog is missing from this app.")
            }
            return (try PhonePaletteCatalog.decode(data: Data(contentsOf: url)), nil)
        } catch { return ([], "The color palette catalog could not be loaded: \(error.localizedDescription)") }
    }

    private static func validPhonePalette(_ id: String, presets: [PhonePalette]) -> Bool {
        id == "native" || id == "desktop" || presets.contains { $0.id == id }
    }

    private static func resolvedTheme(_ appearance: DesktopAppearance?, nativePhone: Bool, paletteID: String, presets: [PhonePalette]) -> DesktopTheme {
        guard nativePhone else { return DesktopTheme.resolve(appearance) }
        if paletteID == "desktop" { return .phoneDesktop(appearance) }
        if let preset = presets.first(where: { $0.id == paletteID }) { return .phonePalette(preset, mic: appearance?.mic ?? false) }
        return .nativePhone(mic: appearance?.mic ?? false)
    }

    func setPhonePalette(_ id: String) {
        guard usesNativePhoneAppearance, Self.validPhonePalette(id, presets: phonePalettes), id != phonePaletteID else { return }
        defaults.set(id, forKey: Self.phonePaletteKey)
        phonePaletteID = id
        apply(appearance)
    }

    var phonePaletteDetail: String {
        if phonePaletteID == "native" { return "Follows your iPhone’s light or dark appearance." }
        if phonePaletteID == "desktop" { return "Uses the selected desktop’s last known colors, including its Ghostty theme. Native is used until those colors are available." }
        return phonePalettes.first { $0.id == phonePaletteID }?.detail ?? ""
    }

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
        return DesktopStyle(Self.resolvedTheme(stored(for: desktopID), nativePhone: usesNativePhoneAppearance, paletteID: phonePaletteID, presets: phonePalettes), scale: scale)
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
        let next = DesktopStyle(Self.resolvedTheme(appearance, nativePhone: usesNativePhoneAppearance, paletteID: phonePaletteID, presets: phonePalettes), scale: scale)
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
