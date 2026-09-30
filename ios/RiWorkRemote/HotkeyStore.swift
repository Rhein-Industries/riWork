import Foundation
import Observation
import RiWorkCore

/// The hotkeys a person added to the key bar, kept on this device (UserDefaults: they are shortcuts, not secrets).
/// Every change is validated by `HotkeyLibrary` and written straight away; what cannot be read back is dropped on load.
@MainActor @Observable final class HotkeyStore {
    static let key = "riwork.hotkeys"
    private(set) var library: HotkeyLibrary
    @ObservationIgnored private let defaults: UserDefaults
    var custom: [Hotkey] { library.hotkeys }

    init(defaults: UserDefaults) {
        self.defaults = defaults
        library = HotkeyLibrary(encoded: defaults.string(forKey: Self.key))
    }
    func add(_ hotkey: Hotkey) throws { var next = library; try next.add(hotkey); commit(next) }
    func update(_ hotkey: Hotkey) throws { var next = library; try next.update(hotkey); commit(next) }
    func remove(id: String) { var next = library; next.remove(id: id); commit(next) }
    func remove(atOffsets offsets: IndexSet) { var next = library; next.remove(atOffsets: Array(offsets)); commit(next) }
    func move(fromOffsets offsets: IndexSet, toOffset destination: Int) { var next = library; next.move(fromOffsets: Array(offsets), toOffset: destination); commit(next) }
    private func commit(_ next: HotkeyLibrary) {
        guard next != library else { return }
        library = next
        defaults.set(next.encoded, forKey: Self.key)
    }
}
