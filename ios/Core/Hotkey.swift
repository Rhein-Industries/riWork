import Foundation

public enum HotkeyError: Error, Equatable, LocalizedError, Sendable {
    case emptyLabel
    case labelTooLong(Int)
    case invalidLabel
    case noSteps
    case tooManySteps(Int)
    /// A step the desktop would refuse; `index` is zero-based.
    case invalidStep(index: Int, reason: String)
    case tooManyHotkeys(Int)
    case duplicate
    case unknown
    case invalidChord(String)
    /// The shortcut already belongs to another hotkey (its name).
    case chordInUse(String)
    public var errorDescription: String? {
        switch self {
        case .emptyLabel: "Give the hotkey a name."
        case .labelTooLong(let limit): "Keep the name to \(limit) characters."
        case .invalidLabel: "The name cannot contain line breaks or control characters."
        case .noSteps: "Add at least one step."
        case .tooManySteps(let limit): "A hotkey has at most \(limit) steps."
        case .invalidStep(let index, let reason): "Step \(index + 1): \(reason)"
        case .tooManyHotkeys(let limit): "You can keep at most \(limit) hotkeys."
        case .duplicate: "A hotkey with this identity already exists."
        case .unknown: "That hotkey no longer exists."
        case .invalidChord(let reason): reason
        case .chordInUse(let name): "That shortcut already runs “\(name)”."
        }
    }
}

/// A button on the key bar that sends a fixed sequence: text and special keys, in order. For example "/clear" then Enter,
/// or Ctrl+C then "exit" then Enter. A step is exactly what `shell.keys` accepts, so a hotkey is validated by the same rules.
public struct Hotkey: Sendable, Hashable, Identifiable {
    public static let maxLabelLength = 12
    public static let maxSteps = KeyItem.maxItems

    public let id: String
    public var label: String
    public var steps: [KeyItem]
    /// A keyboard shortcut that runs this hotkey without opening the hotkey menu.
    public var chord: KeyChord?
    /// Whether the key bar shows a button for it. A hotkey that is only a shortcut (Esc on ⌘E) stays out of the bar's way and
    /// is still there in the hotkey menu.
    public var showsOnBar: Bool
    public init(id: String = UUID().uuidString.lowercased(), label: String, steps: [KeyItem], chord: KeyChord? = nil, showsOnBar: Bool = true) {
        self.id = id; self.label = label; self.steps = steps; self.chord = chord; self.showsOnBar = showsOnBar
    }

    /// Checks the label and every step against the `shell.keys` contract: text without control characters, only whitelisted keys,
    /// at most 64 steps and 4096 bytes of text.
    public func validate() throws {
        let name = label.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty else { throw HotkeyError.emptyLabel }
        guard name.count <= Self.maxLabelLength else { throw HotkeyError.labelTooLong(Self.maxLabelLength) }
        guard !name.unicodeScalars.contains(where: KeyItem.isForbidden) else { throw HotkeyError.invalidLabel }
        guard !steps.isEmpty else { throw HotkeyError.noSteps }
        guard steps.count <= Self.maxSteps else { throw HotkeyError.tooManySteps(Self.maxSteps) }
        for (index, step) in steps.enumerated() {
            do { try step.validate() }
            catch { throw HotkeyError.invalidStep(index: index, reason: Self.reason(for: step)) }
        }
        do { try KeyItem.validate(batch: items) }
        catch { throw HotkeyError.invalidStep(index: steps.count - 1, reason: "there is more text than one send can carry.") }
        if let chord {
            do { try chord.validate() }
            catch { throw HotkeyError.invalidChord((error as? ChordError)?.errorDescription ?? "That shortcut cannot be used.") }
        }
    }
    private static func reason(for step: KeyItem) -> String {
        switch step {
        case .text(let text): text.isEmpty ? "the text is empty." : (text.utf8.count > KeyItem.maxTextBytes ? "the text is too long." : "text cannot contain line breaks or control characters; use the Enter key instead.")
        case .key: "this key cannot be sent."
        }
    }

    /// What the shell receives: the steps, with neighbouring text steps joined into one.
    public var items: [KeyItem] {
        var out: [KeyItem] = []
        for step in steps {
            if case .text(let text) = step, case .text(let previous)? = out.last { out[out.count - 1] = .text(previous + text) }
            else { out.append(step) }
        }
        return out
    }
    /// A short description for lists: the glyphs and text of the steps.
    public var summary: String { steps.map(\.symbol).joined(separator: " ") }

    // MARK: Built in

    private static func control(_ letter: Character, _ label: String) -> Hotkey {
        Hotkey(id: "builtin.c-\(letter)", label: label, steps: [.key(.control(letter))])
    }
    /// Ctrl+C, D, Z, L, R, A, E, U, W and a double Escape. They cannot be edited or removed.
    public static let builtIn: [Hotkey] = [
        control("c", "^C"), control("d", "^D"), control("z", "^Z"), control("l", "^L"), control("r", "^R"),
        control("a", "^A"), control("e", "^E"), control("u", "^U"), control("w", "^W"),
        Hotkey(id: "builtin.esc-esc", label: "Esc Esc", steps: [.key(.escape), .key(.escape)])
    ]
    public var isBuiltIn: Bool { id.hasPrefix("builtin.") }

    // MARK: Storage form

    /// `{"id","label","steps":[{"text":…}|{"key":…}]}`, the steps in the wire form of `shell.keys`.
    public var json: JSONValue {
        var object: [String: JSONValue] = ["id": .string(id), "label": .string(label), "steps": .array(steps.map(\.json))]
        if let chord { object["chord"] = chord.json }
        if !showsOnBar { object["bar"] = .bool(false) }
        return .object(object)
    }
    /// Strict: a hotkey that would not validate is not accepted from storage either.
    public init(json: JSONValue) throws {
        guard case .object = json, let id = json["id"].string, !id.isEmpty, id.utf8.count <= 64, case .array(let raw) = json["steps"], let label = json["label"].string else {
            throw HotkeyError.unknown
        }
        var chord: KeyChord?
        if case .object = json["chord"] {
            do { chord = try KeyChord(json: json["chord"]) }
            // A shortcut the app has taken for itself since (⌘/ opened the help in a later version): the hotkey stays, without it.
            catch ChordError.reserved { chord = nil }
            catch { throw HotkeyError.invalidChord("That shortcut cannot be used.") }
        }
        var onBar = true
        if case .bool(let flag) = json["bar"] { onBar = flag }
        self.init(id: id, label: label, steps: try raw.map { try KeyItem(json: $0) }, chord: chord, showsOnBar: onBar)
        try validate()
    }
}

/// The hotkeys a person has added: an ordered list with editing, kept small, and a stored form that survives bad data.
public struct HotkeyLibrary: Sendable, Equatable {
    /// Enough for a keyboard template (about fifteen) and a person's own on top; the key bar scrolls and the hotkey menu filters.
    public static let maxHotkeys = 64
    public private(set) var hotkeys: [Hotkey]
    public init(hotkeys: [Hotkey] = []) { self.hotkeys = hotkeys }

    /// The hotkey a shortcut runs, if any.
    public func hotkey(for chord: KeyChord) -> Hotkey? { hotkeys.first { $0.chord == chord } }
    private func conflict(for hotkey: Hotkey) -> Hotkey? {
        guard let chord = hotkey.chord else { return nil }
        return hotkeys.first { $0.id != hotkey.id && $0.chord == chord }
    }

    public mutating func add(_ hotkey: Hotkey) throws {
        try hotkey.validate()
        guard !hotkey.isBuiltIn, !hotkeys.contains(where: { $0.id == hotkey.id }) else { throw HotkeyError.duplicate }
        guard hotkeys.count < Self.maxHotkeys else { throw HotkeyError.tooManyHotkeys(Self.maxHotkeys) }
        if let other = conflict(for: hotkey) { throw HotkeyError.chordInUse(other.label) }
        hotkeys.append(hotkey)
    }
    public mutating func update(_ hotkey: Hotkey) throws {
        try hotkey.validate()
        guard let index = hotkeys.firstIndex(where: { $0.id == hotkey.id }) else { throw HotkeyError.unknown }
        if let other = conflict(for: hotkey) { throw HotkeyError.chordInUse(other.label) }
        hotkeys[index] = hotkey
    }
    public mutating func remove(id: String) { hotkeys.removeAll { $0.id == id } }
    public mutating func remove(atOffsets offsets: [Int]) {
        let doomed = Set(offsets)
        hotkeys = hotkeys.enumerated().filter { !doomed.contains($0.offset) }.map(\.element)
    }
    /// The same meaning as SwiftUI's `onMove`: the items at `offsets` end up before the item that is now at `destination`.
    public mutating func move(fromOffsets offsets: [Int], toOffset destination: Int) {
        let moving = Set(offsets).filter { hotkeys.indices.contains($0) }.sorted()
        guard !moving.isEmpty else { return }
        let picked = moving.map { hotkeys[$0] }
        var rest = hotkeys.enumerated().filter { !moving.contains($0.offset) }.map(\.element)
        let removedBefore = moving.filter { $0 < destination }.count
        rest.insert(contentsOf: picked, at: max(0, min(rest.count, destination - removedBefore)))
        hotkeys = rest
    }

    public var encoded: String {
        let value = JSONValue.object(["v": .number(1), "hotkeys": .array(hotkeys.map(\.json))])
        return (try? JSONEncoder().encode(value)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
    }
    /// Reads stored hotkeys. Anything unreadable is dropped rather than trusted: bad entries, repeated ids, built-in ids and
    /// whatever exceeds the limit. Never throws; a wholly unreadable value is an empty library.
    public init(encoded text: String?) {
        guard let text, let value = try? JSONDecoder().decode(JSONValue.self, from: Data(text.utf8)), value["v"] == .number(1) else { self.init(); return }
        var seen = Set<String>()
        var chords = Set<KeyChord>()
        var list: [Hotkey] = []
        for raw in value["hotkeys"].array {
            guard list.count < Self.maxHotkeys, var hotkey = try? Hotkey(json: raw), !hotkey.isBuiltIn, seen.insert(hotkey.id).inserted else { continue }
            // Two hotkeys on one shortcut: the first keeps it, the later one stays but loses it.
            if let chord = hotkey.chord, !chords.insert(chord).inserted { hotkey.chord = nil }
            list.append(hotkey)
        }
        self.init(hotkeys: list)
    }
}
