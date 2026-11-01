import Foundation

// MARK: - Contract items

/// A named key the desktop `shell.keys` method accepts, or `C-a` … `C-z`; and, from a desktop whose `ready.features.key_names` is 2
/// (2026-11-01), the second key list: `Insert`, `Space`, `F1` … `F12` and keys with modifiers (`ModifiedKey`).
public enum TerminalKey: Sendable, Hashable {
    case enter, tab, backTab, escape, backspace, delete
    case up, down, left, right, home, end, pageUp, pageDown
    /// A lowercase ASCII letter. Anything else fails validation.
    case control(Character)
    /// The second key list (`key_names` 2). A desktop without it gets the escape sequences instead (`KeyMapper.firstList`).
    case insert, space
    /// F1 … F12.
    case function(Int)
    /// A key with modifiers that the first list has no name for: `C-Up`, `M-x`, `C-\`, `C-M-S-F5`.
    case modified(ModifiedKey)

    private static let named: [(String, TerminalKey)] = [
        ("Enter", .enter), ("Tab", .tab), ("BTab", .backTab), ("Escape", .escape), ("Backspace", .backspace), ("Delete", .delete),
        ("Up", .up), ("Down", .down), ("Left", .left), ("Right", .right), ("Home", .home), ("End", .end),
        ("PageUp", .pageUp), ("PageDown", .pageDown)
    ]
    /// The second list's own named keys.
    private static let secondNamed: [(String, TerminalKey)] = [("Insert", .insert), ("Space", .space)] + (1...12).map { ("F\($0)", .function($0)) }
    public static func isControlLetter(_ character: Character) -> Bool {
        character.unicodeScalars.count == 1 && (97...122).contains(character.unicodeScalars.first!.value)
    }
    /// `C-<letter>` for an ASCII letter of either case; nil for anything else.
    public static func control(forLetter character: Character) -> TerminalKey? {
        guard character.unicodeScalars.count == 1, let scalar = character.unicodeScalars.first,
              (65...90).contains(scalar.value) || (97...122).contains(scalar.value) else { return nil }
        return .control(Character(String(character).lowercased()))
    }
    /// A name of either list. Each key has one: a first-list key by its first-list name (`Enter`, `C-c`), never `ModifiedKey`'s.
    public init?(name: String) {
        if let match = (Self.named + Self.secondNamed).first(where: { $0.0 == name }) { self = match.1; return }
        let scalars = Array(name.unicodeScalars)
        if scalars.count == 3, scalars[0] == "C", scalars[1] == "-", (97...122).contains(scalars[2].value) { self = .control(Character(scalars[2])); return }
        guard let modified = ModifiedKey(name: name) else { return nil }
        self = .modified(modified)
    }
    /// The wire name. Invalid `.control` letters and F-key numbers still render, and fail `KeyItem.validate`.
    public var name: String {
        switch self {
        case .control(let letter): return "C-\(letter)"
        case .function(let number): return "F\(number)"
        case .modified(let key): return key.name
        default: return (Self.named + Self.secondNamed).first(where: { $0.1 == self })!.0
        }
    }
    public var isValid: Bool {
        switch self {
        case .control(let letter): Self.isControlLetter(letter)
        case .function(let number): (1...12).contains(number)
        default: true
        }
    }
    /// Whether only a desktop with the second key list (`key_names` 2) takes this name.
    public var needsSecondList: Bool {
        switch self {
        case .insert, .space, .function, .modified: true
        default: false
        }
    }
    /// Every key a person can pick for a hotkey step: the named keys, Insert and F1 … F12, then `C-a` … `C-z`. A desktop without
    /// the second key list gets Insert and the F-keys as their escape sequences.
    public static let choices: [TerminalKey] = named.map(\.1) + [.insert] + (1...12).map { .function($0) }
        + (97...122).map { .control(Character(Unicode.Scalar(UInt8($0)))) }
    /// A readable name for menus and the key bar's accessibility labels.
    public var title: String {
        switch self {
        case .insert: "Insert"
        case .space: "Space"
        case .function(let number): "F\(number)"
        case .modified(let key): key.title
        case .enter: "Enter"
        case .tab: "Tab"
        case .backTab: "Shift-Tab"
        case .escape: "Esc"
        case .backspace: "Backspace"
        case .delete: "Delete"
        case .up: "Up"
        case .down: "Down"
        case .left: "Left"
        case .right: "Right"
        case .home: "Home"
        case .end: "End"
        case .pageUp: "Page Up"
        case .pageDown: "Page Down"
        case .control(let letter): "Ctrl+\(String(letter).uppercased())"
        }
    }
    /// Compact glyph for the pending-input preview.
    public var symbol: String {
        switch self {
        case .insert: "⎀"
        case .space: "␣"
        case .function(let number): "F\(number)"
        case .modified(let key): key.symbol
        case .enter: "⏎"
        case .tab: "⇥"
        case .backTab: "⇤"
        case .escape: "⎋"
        case .backspace: "⌫"
        case .delete: "⌦"
        case .up: "↑"
        case .down: "↓"
        case .left: "←"
        case .right: "→"
        case .home: "↖"
        case .end: "↘"
        case .pageUp: "⇞"
        case .pageDown: "⇟"
        case .control(let letter): "^\(String(letter).uppercased())"
        }
    }
    /// What modifiers go on to name this key with them: its base, and the modifiers it already has (`C-c` is Ctrl and `c`).
    var modifiable: (ModifiedKey.Base, ChordModifiers)? {
        switch self {
        case .control(let letter): (.character(letter), .control)
        case .modified(let key): (key.base, key.modifiers)
        default: (.named(name), [])
        }
    }
    /// The key `modified` names, by its first-list name when it has one (`C-c`).
    public init(_ modified: ModifiedKey) {
        if modified.modifiers == .control, case .character(let letter) = modified.base, Self.isControlLetter(letter) { self = .control(letter) }
        else { self = .modified(modified) }
    }
}

/// A key with modifiers from the second key list (`ready.features.key_names` 2): `C-`, `M-` (Alt) and `S-`, in that order and each at
/// most once, then a base. Only what tmux turns into the bytes a terminal sends is a name; the desktop checks the same rules
/// (`session_keys::Key::parse`, `valid_key` in the connector):
/// - the cursor and editing keys (Up … PageDown, Insert, Delete) and F1 … F12 take any modifiers (`C-Up`, `S-F5`, `C-M-S-End`);
/// - Enter, Tab, BTab, Escape and Backspace take only `M-`;
/// - Space takes `C-` (NUL) and `M-`;
/// - a printable ASCII character other than space takes `M-` (Escape, then it), and `C-` only when it has a control character
///   (a–z @ [ \ ] ^ _ ?); it needs one of the two and never takes `S-` (the shifted character is sent instead).
public struct ModifiedKey: Sendable, Hashable {
    public enum Base: Sendable, Hashable {
        /// A named key by its wire name: `Up`, `F5`, `Space`.
        case named(String)
        case character(Character)
    }
    /// Control, Alt (Meta) and Shift; at least one.
    public let modifiers: ChordModifiers
    public let base: Base

    private static let anyModifier: Set<String> = Set(["Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown", "Insert", "Delete"]
                                                      + (1...12).map { "F\($0)" })
    private static let altOnly: Set<String> = ["Enter", "Tab", "BTab", "Escape", "Backspace"]
    /// The characters Ctrl turns into a control character.
    public static let controlCharacters: Set<Character> = Set("abcdefghijklmnopqrstuvwxyz@[\\]^_?")

    /// Nil for a combination that has no name, and for none of the three modifiers (a plain key is a `TerminalKey` of its own).
    public init?(modifiers: ChordModifiers, base: Base) {
        let modifiers = modifiers.intersection([.control, .alt, .shift])
        guard !modifiers.isEmpty else { return nil }
        switch base {
        case .named(let name):
            if Self.anyModifier.contains(name) { break }
            if Self.altOnly.contains(name) { guard modifiers == .alt else { return nil }; break }
            guard name == "Space", !modifiers.contains(.shift) else { return nil }
        case .character(let character):
            guard character.unicodeScalars.count == 1, let scalar = character.unicodeScalars.first, (0x21...0x7E).contains(scalar.value),
                  !modifiers.contains(.shift) else { return nil }
            if modifiers.contains(.control), !Self.controlCharacters.contains(character) { return nil }
        }
        self.modifiers = modifiers
        self.base = base
    }
    /// The wire name, modifiers in their one order (`M-C-a` and `C-Enter` are no names).
    public init?(name: String) {
        var rest = Substring(name)
        var modifiers: ChordModifiers = []
        for (prefix, modifier) in [("C-", ChordModifiers.control), ("M-", .alt), ("S-", .shift)] where rest.hasPrefix(prefix) {
            modifiers.insert(modifier)
            rest = rest.dropFirst(2)
        }
        let text = String(rest)
        if Self.anyModifier.contains(text) || Self.altOnly.contains(text) || text == "Space" { self.init(modifiers: modifiers, base: .named(text)) }
        else if text.unicodeScalars.count == 1 { self.init(modifiers: modifiers, base: .character(Character(text))) }
        else { return nil }
    }
    public var name: String {
        var prefix = ""
        if modifiers.contains(.control) { prefix += "C-" }
        if modifiers.contains(.alt) { prefix += "M-" }
        if modifiers.contains(.shift) { prefix += "S-" }
        switch base {
        case .named(let name): return prefix + name
        case .character(let character): return prefix + String(character)
        }
    }
    /// "Ctrl+Alt+Up", "Alt+X".
    public var title: String {
        var parts: [String] = []
        if modifiers.contains(.control) { parts.append("Ctrl") }
        if modifiers.contains(.alt) { parts.append("Alt") }
        if modifiers.contains(.shift) { parts.append("Shift") }
        switch base {
        case .named(let name): parts.append(TerminalKey(name: name)?.title ?? name)
        case .character(let character): parts.append(String(character).uppercased())
        }
        return parts.joined(separator: "+")
    }
    /// "⌃⌥↑", "⌥x".
    public var symbol: String {
        switch base {
        case .named(let name): modifiers.glyphs + (TerminalKey(name: name)?.symbol ?? name)
        case .character(let character): modifiers.glyphs + String(character)
        }
    }
}

/// One element of a `shell.keys` batch: literal text or exactly one key.
public enum KeyItem: Sendable, Hashable {
    case text(String)
    case key(TerminalKey)

    public static let maxTextBytes = 4096
    public static let maxItems = 64

    /// Control characters (Cc) and the Unicode line/paragraph separators can never travel as literal text.
    public static func isForbidden(_ scalar: Unicode.Scalar) -> Bool {
        scalar.properties.generalCategory == .control || scalar.value == 0x2028 || scalar.value == 0x2029
    }

    public func validate() throws {
        switch self {
        case .text(let text):
            guard !text.isEmpty, text.utf8.count <= Self.maxTextBytes else { throw RemoteError.protocolViolation("Text must be 1–4096 bytes.") }
            guard !text.unicodeScalars.contains(where: Self.isForbidden) else { throw RemoteError.protocolViolation("Text must not contain control characters.") }
        case .key(let key):
            guard key.isValid else { throw RemoteError.protocolViolation("Unknown key.") }
        }
    }
    /// 1…64 items, each valid, at most 4096 text bytes in total.
    public static func validate(batch items: [KeyItem]) throws {
        guard (1...maxItems).contains(items.count) else { throw RemoteError.protocolViolation("A key batch holds 1–64 items.") }
        var bytes = 0
        for item in items {
            try item.validate()
            if case .text(let text) = item { bytes += text.utf8.count }
        }
        guard bytes <= maxTextBytes else { throw RemoteError.protocolViolation("A key batch holds at most 4096 text bytes.") }
    }

    public var json: JSONValue {
        switch self {
        case .text(let text): .object(["text": .string(text)])
        case .key(let key): .object(["key": .string(key.name)])
        }
    }
    /// Strict inverse of `json`: exactly one of `text` or `key`, nothing else.
    public init(json: JSONValue) throws {
        guard case .object(let object) = json, object.count == 1 else { throw RemoteError.protocolViolation("A key item has exactly one of text or key.") }
        if let text = object["text"]?.string { self = .text(text) }
        else if let name = object["key"]?.string, let key = TerminalKey(name: name) { self = .key(key) }
        else { throw RemoteError.protocolViolation("Unknown key item.") }
        try validate()
    }
    public var symbol: String {
        switch self {
        case .text(let text): text
        case .key(let key): key.symbol
        }
    }
    /// What the buffer cap counts: one per key, one per text scalar (additive, so merging and splitting text never drifts it).
    var characterCount: Int {
        switch self {
        case .text(let text): text.unicodeScalars.count
        case .key: 1
        }
    }
}

// MARK: - Request and result

public enum KeysStatus: String, Sendable, Equatable {
    case sent, duplicate, uncertain

    public static func parse(_ result: JSONValue, shellID: String, batch: String) throws -> KeysStatus {
        guard result["shell_id"].string == shellID else { throw RemoteError.protocolViolation("Key delivery identity mismatch.") }
        if let echoed = result["batch"].string, echoed != batch { throw RemoteError.protocolViolation("Key delivery batch mismatch.") }
        guard let raw = result["status"].string, let status = KeysStatus(rawValue: raw) else { throw RemoteError.protocolViolation("Unknown key delivery status.") }
        return status
    }
}

extension RemoteError {
    /// An older desktop answers a method it does not know with `invalid_request` "unsupported RPC method".
    public static func isUnsupportedMethod(_ error: any Error) -> Bool {
        guard case RemoteError.rpc(let code, let message) = error, code == "invalid_request" else { return false }
        return message.lowercased().contains("unsupported rpc method")
    }
}

extension RemoteTransport {
    /// Sends one batch. Retrying the same `batch` with a new request `id` is safe: the desktop dedupes by batch.
    public func keys(shellID: String, batch: String, items: [KeyItem], id: String = UUID().uuidString.lowercased()) async throws -> KeysStatus {
        try KeyItem.validate(batch: items)
        let params: [String: JSONValue] = ["shell_id": .string(shellID), "batch": .string(batch), "items": .array(items.map(\.json))]
        return try KeysStatus.parse(try await request(method: "shell.keys", params: params, id: id), shellID: shellID, batch: batch)
    }
}

// MARK: - Outgoing buffer

/// A batch as formed for the wire. Its id and content never change until it succeeds or is dropped.
public struct KeyBatch: Sendable, Equatable {
    public let id: String
    public let items: [KeyItem]
}

/// Ordered, capped keystroke queue for one shell. Pure value type; the model owns the timing.
///
/// `frozen` is the batch that has been formed (and may have reached the desktop). New keystrokes queue behind
/// it and never join it, so a retry with the same batch id always carries the same content.
public struct KeyBuffer: Sendable, Equatable {
    public static let maxCharacters = 4096
    public static let maxItems = 512

    public struct Block: Sendable, Equatable {
        public let reason: String
        public let at: Date
        public init(reason: String, at: Date) { self.reason = reason; self.at = at }
    }
    public private(set) var frozen: KeyBatch?
    public private(set) var queued: [KeyItem] = []
    /// Set when the desktop refused input (disabled, gone). The buffer is kept but not sent.
    public var block: Block?
    private var frozenSince: Date?
    private var queuedSince: Date?
    private var characters = 0

    public init() {}
    public var isEmpty: Bool { frozen == nil && queued.isEmpty }
    public var characterCount: Int { characters }
    public var itemCount: Int { (frozen?.items.count ?? 0) + queued.count }
    public var items: [KeyItem] { (frozen?.items ?? []) + queued }
    /// When the oldest unsent or in-flight item was typed.
    public var oldestPendingAt: Date? { frozen == nil ? queuedSince : frozenSince }

    /// All or nothing. False when the items do not fit the character or item cap.
    @discardableResult
    public mutating func append(_ new: [KeyItem], now: Date = Date()) -> Bool {
        guard !new.isEmpty else { return true }
        var merged = queued
        var added = 0
        for item in new {
            if case .text(let text) = item, text.isEmpty { continue }
            added += item.characterCount
            if case .text(let addition) = item, case .text(let last)? = merged.last { merged[merged.count - 1] = .text(last + addition) }
            else { merged.append(item) }
        }
        guard characters + added <= Self.maxCharacters, (frozen?.items.count ?? 0) + merged.count <= Self.maxItems else { return false }
        if queued.isEmpty { queuedSince = now }
        queued = merged
        characters += added
        return true
    }

    /// The batch to send now: the frozen one if it is still unacknowledged, otherwise a new one formed from the head
    /// of the queue (at most 64 items and 4096 text bytes; long text is split on a scalar boundary; it ends at an Enter).
    public mutating func nextBatch(id: String = UUID().uuidString.lowercased()) -> KeyBatch? {
        if let frozen { return frozen }
        guard !queued.isEmpty else { return nil }
        var items: [KeyItem] = []
        var textBytes = 0
        var consumed = 0
        // A key that follows text makes the desktop pause (~150 ms) so a fast "text, Enter" is not read as a paste.
        // Ending the batch after an Enter (and any Enters right behind it) keeps typical batches to `[text, Enter]`,
        // one pause, a quick answer.
        var afterEnter = false
        formation: for item in queued {
            guard items.count < KeyItem.maxItems else { break }
            if afterEnter, item != .key(.enter) { break }
            switch item {
            case .key(let key):
                items.append(item); consumed += 1
                if key == .enter { afterEnter = true }
            case .text(let text):
                let budget = KeyItem.maxTextBytes - textBytes
                if text.utf8.count <= budget {
                    items.append(item); textBytes += text.utf8.count; consumed += 1
                } else {
                    // An Escape right before it starts a sequence (Alt+key, Ctrl+Up as `ESC [1;5A`): it waits for the next batch too.
                    if items.count > 1, items.last == .key(.escape) { items.removeLast(); consumed -= 1; break formation }
                    var head = String.UnicodeScalarView()
                    var used = 0
                    for scalar in text.unicodeScalars {
                        let width = String(scalar).utf8.count
                        guard used + width <= budget else { break }
                        head.append(scalar); used += width
                    }
                    guard !head.isEmpty else { break formation }
                    items.append(.text(String(head)))
                    queued[consumed] = .text(String(text.unicodeScalars.dropFirst(head.count)))
                    textBytes += used
                    break formation
                }
            }
        }
        // A batch never ends on an Escape that has more behind it: the program would read a lone Esc and then the rest of the
        // sequence as typing.
        if items.count > 1, items.last == .key(.escape), consumed < queued.count { items.removeLast(); consumed -= 1 }
        queued.removeFirst(consumed)
        let batch = KeyBatch(id: id, items: items)
        frozen = batch
        frozenSince = queuedSince
        if queued.isEmpty { queuedSince = nil }
        return batch
    }

    /// The batch reached the desktop (sent, duplicate, uncertain) or is being abandoned. False if it is not the frozen one.
    @discardableResult
    public mutating func finish(_ id: String) -> Bool {
        guard let frozen, frozen.id == id else { return false }
        characters -= frozen.items.reduce(0) { $0 + $1.characterCount }
        self.frozen = nil; frozenSince = nil
        if isEmpty { characters = 0; queuedSince = nil }
        return true
    }
    public mutating func removeAll() {
        frozen = nil; queued = []; frozenSince = nil; queuedSince = nil; characters = 0; block = nil
    }
    /// Text only, for restoring what was typed into the line composer when the desktop cannot take keys.
    public var plainText: String {
        var text = ""
        var afterEscape = false
        for item in items {
            defer { afterEscape = item == .key(.escape) }
            switch item {
            // Text right after an Escape is the rest of a key (Alt+b, Ctrl+Up), not something typed.
            case .text where afterEscape: break
            case .text(let value): text += value
            case .key(.backspace): if !text.isEmpty { text.removeLast() }
            default: break
            }
        }
        return text
    }
    /// One line of pending input with key glyphs, cut at the front with "…" when longer than `limit` characters.
    public func preview(limit: Int = 48) -> String {
        let all = items.map(\.symbol).joined()
        guard limit > 1, all.count > limit else { return all }
        return "…" + String(all.suffix(limit - 1))
    }
}

// MARK: - Event mapping

/// How a modifier key on the key bar is held: not at all, for the next key only, or until it is tapped again.
public enum ModifierLatch: Sendable, Equatable {
    case off, once, locked
}

/// Turns typed, dictated and pasted text and key presses into contract items.
///
/// Ctrl, Alt and Shift on the key bar latch like the Shift key of the iOS keyboard: a tap arms one for the next key, a quick second
/// tap (a double tap) locks it until it is tapped again, and a slower second tap lets it go. Whatever is armed applies to the next
/// single key: a letter, digit or symbol from the software keyboard or the bar, Return, Backspace, a named key of the bar, or a key
/// of a hardware keyboard (its own modifiers add to the bar's). More than one character at once (dictation, a paste, an
/// autocorrection, a word from an input method) is text, not a key press: it goes through unchanged and the arming stays.
public struct KeyMapper: Sendable, Equatable {
    /// A second tap within this many seconds of the first locks the modifier; a later one lets it go.
    public static let doubleTapInterval: TimeInterval = 0.4
    public private(set) var control = ModifierLatch.off
    public private(set) var alt = ModifierLatch.off
    public private(set) var shift = ModifierLatch.off
    /// The modifier tapped last and when, while a second tap could still make a double tap of it.
    private var lastTap: ChordModifiers?
    private var lastTapAt: Date?
    /// The newest key list the desktop takes (`ready.features.key_names`): 1 until it says 2. With 2, F-keys, Insert, modified keys,
    /// Alt plus a character and Ctrl plus a non-letter go by their own names (`F5`, `C-Up`, `M-x`, `C-\`); with 1, as the escape
    /// sequences they stand for, which every desktop with `shell.keys` takes (and Ctrl plus a non-letter as the character itself).
    public var keyNames = 1
    public init() {}

    public var controlArmed: Bool { control != .off }
    public var altArmed: Bool { alt != .off }
    public var shiftArmed: Bool { shift != .off }
    /// What the next key gets.
    public var armed: ChordModifiers {
        var out: ChordModifiers = []
        if controlArmed { out.insert(.control) }
        if altArmed { out.insert(.alt) }
        if shiftArmed { out.insert(.shift) }
        return out
    }
    public func latch(_ modifier: ChordModifiers) -> ModifierLatch {
        switch modifier {
        case .control: control
        case .alt: alt
        case .shift: shift
        default: .off
        }
    }
    private mutating func set(_ modifier: ChordModifiers, _ latch: ModifierLatch) {
        switch modifier {
        case .control: control = latch
        case .alt: alt = latch
        case .shift: shift = latch
        default: break
        }
    }

    /// A tap on Ctrl, Alt or Shift: off arms it for one key, a double tap locks it, any other tap lets it go.
    public mutating func tap(_ modifier: ChordModifiers, at now: Date = Date()) {
        let quick = lastTap == modifier && lastTapAt.map { now.timeIntervalSince($0) < Self.doubleTapInterval } ?? false
        switch latch(modifier) {
        case .off: set(modifier, .once)
        case .once: set(modifier, quick ? .locked : .off)
        case .locked: set(modifier, .off)
        }
        // A tap that locked or released ends the pair: a third quick tap starts over instead of counting as another double.
        if latch(modifier) == .once { lastTap = modifier; lastTapAt = now } else { lastTap = nil; lastTapAt = nil }
    }
    /// Locks or releases a modifier outright (VoiceOver's actions, where a double tap is not a gesture one can make on a key).
    public mutating func setLatch(_ modifier: ChordModifiers, _ latch: ModifierLatch) { set(modifier, latch); lastTap = nil; lastTapAt = nil }
    /// Every modifier, locked ones too, for when the keyboard goes away.
    public mutating func disarmModifiers() { control = .off; alt = .off; shift = .off; lastTap = nil; lastTapAt = nil }
    /// A key was sent: what was armed for one key is used up; a locked modifier stays.
    private mutating func release() {
        if control == .once { control = .off }
        if alt == .once { alt = .off }
        if shift == .once { shift = .off }
        lastTap = nil; lastTapAt = nil
    }

    /// Typed, dictated or pasted text. A single character is a key press and takes the armed modifiers; anything longer is text.
    public mutating func insert(_ text: String) -> [KeyItem] {
        let modifiers = armed
        guard !modifiers.isEmpty, text.count == 1, let character = text.first else { return Self.items(for: text) }
        let items: [KeyItem]
        switch character {
        case "\r", "\n", "\r\n": items = Self.encode(.enter, modifiers: modifiers, keyNames: keyNames)
        case "\t": items = Self.encode(.tab, modifiers: modifiers, keyNames: keyNames)
        default: items = Self.encode(character, modifiers: modifiers, keyNames: keyNames)
        }
        // A character that cannot be sent (a bare control character) is not a key press: the modifiers wait for one that is.
        if !items.isEmpty { release() }
        return items
    }
    public mutating func deleteBackward() -> [KeyItem] { press(.backspace) }
    /// A bar or hardware key, with the modifiers a hardware keyboard held for it. It also takes the armed ones.
    public mutating func press(_ key: TerminalKey, modifiers held: ChordModifiers = []) -> [KeyItem] {
        let items = Self.encode(key, modifiers: armed.union(held), keyNames: keyNames)
        release()
        return items
    }
    /// A character key of a hardware keyboard with the modifiers held for it (Ctrl+Alt+B, say), plus the armed ones.
    public mutating func press(_ character: Character, modifiers held: ChordModifiers) -> [KeyItem] {
        let items = Self.encode(character, modifiers: armed.union(held), keyNames: keyNames)
        if !items.isEmpty { release() }
        return items
    }
    /// A hotkey is complete in itself: it goes out as defined (an F-key step as its escape sequence where the desktop has no name for
    /// it), and uses up what was armed for one key.
    public mutating func run(_ hotkey: Hotkey) -> [KeyItem] {
        release()
        return hotkey.items.flatMap { Self.compatible($0, keyNames: keyNames) }
    }

    // MARK: Encoding

    /// The xterm modifier parameter: 1 plus 1 for Shift, 2 for Alt, 4 for Ctrl (Command is not a terminal modifier).
    public static func xtermParameter(_ modifiers: ChordModifiers) -> Int {
        1 + (modifiers.contains(.shift) ? 1 : 0) + (modifiers.contains(.alt) ? 2 : 0) + (modifiers.contains(.control) ? 4 : 0)
    }

    /// A named key with modifiers. With the second key list (`keyNames` 2) a combination it names goes as that one key (`C-Up`,
    /// `M-Enter`, `S-F5`); Shift+Tab is Back Tab under either list. Anything else goes as `firstList` sends it.
    public static func encode(_ key: TerminalKey, modifiers: ChordModifiers, keyNames: Int = 1) -> [KeyItem] {
        var modifiers = modifiers.intersection([.shift, .control, .alt])
        guard keyNames >= 2 else { return firstList(key, modifiers: modifiers) }
        var key = key
        if key == .tab, modifiers.contains(.shift) { key = .backTab; modifiers.remove(.shift) }
        guard !modifiers.isEmpty else { return [.key(key)] }
        if let (base, own) = key.modifiable, let named = ModifiedKey(modifiers: modifiers.union(own), base: base) { return [.key(TerminalKey(named))] }
        // No name for it (Ctrl+Enter, Ctrl+Backspace): as the first list sends it.
        return firstList(key, modifiers: modifiers)
    }
    /// A key as a desktop with only the first key list takes it, as xterm sends it. The desktop takes named keys and plain text only,
    /// so a modified cursor or editing key travels as `Escape` and the rest of its CSI sequence as text (`ESC [1;5A` for Ctrl+Up),
    /// which reaches the program as the same bytes; so do Insert and the F-keys (`ESC [2~`, `ESC O P`, `ESC [15;2~`). Where xterm has
    /// no modified form the modifier is dropped, except that Alt puts Escape in front.
    public static func firstList(_ key: TerminalKey, modifiers: ChordModifiers) -> [KeyItem] {
        let modifiers = modifiers.intersection([.shift, .control, .alt])
        let parameter = xtermParameter(modifiers)
        func csi(_ body: String) -> [KeyItem] { [.key(.escape), .text("[" + body)] }
        switch key {
        case .insert: return csi(modifiers.isEmpty ? "2~" : "2;\(parameter)~")
        case .function(let number):
            guard (1...12).contains(number) else { return [] }
            if number <= 4 {
                let final = ["P", "Q", "R", "S"][number - 1]
                return modifiers.isEmpty ? [.key(.escape), .text("O" + final)] : csi("1;\(parameter)\(final)")
            }
            let code = [15, 17, 18, 19, 20, 21, 23, 24][number - 5]
            return csi(modifiers.isEmpty ? "\(code)~" : "\(code);\(parameter)~")
        case .space: return encode(" ", modifiers: modifiers)
        case .modified(let named):
            switch named.base {
            case .named(let name): return TerminalKey(name: name).map { firstList($0, modifiers: modifiers.union(named.modifiers)) } ?? []
            case .character(let character): return encode(character, modifiers: modifiers.union(named.modifiers))
            }
        default: break
        }
        guard !modifiers.isEmpty else { return [.key(key)] }
        switch key {
        case .up: return csi("1;\(parameter)A")
        case .down: return csi("1;\(parameter)B")
        case .right: return csi("1;\(parameter)C")
        case .left: return csi("1;\(parameter)D")
        case .home: return csi("1;\(parameter)H")
        case .end: return csi("1;\(parameter)F")
        case .pageUp: return csi("5;\(parameter)~")
        case .pageDown: return csi("6;\(parameter)~")
        case .delete: return csi("3;\(parameter)~")
        case .tab:
            // Shift+Tab is Back Tab; Ctrl+Tab has no legacy form.
            return meta(modifiers, [.key(modifiers.contains(.shift) ? .backTab : .tab)])
        case .backspace:
            // Ctrl+Backspace is ^H, as in xterm (which leaves Backspace itself as DEL).
            return meta(modifiers, [.key(modifiers.contains(.control) ? .control("h") : .backspace)])
        case .backTab, .enter, .escape, .control:
            return meta(modifiers, [.key(key)])
        case .insert, .space, .function, .modified:
            return []  // answered above
        }
    }
    /// A key of either list as a desktop with `keyNames` takes it: a second-list key goes as its escape sequence to one that
    /// has only the first list (`firstList`); everything else as it is.
    public static func compatible(_ item: KeyItem, keyNames: Int) -> [KeyItem] {
        guard keyNames < 2, case .key(let key) = item, key.needsSecondList else { return [item] }
        return firstList(key, modifiers: [])
    }

    /// One character key with modifiers. Shift makes a letter upper case. Ctrl makes a letter its control key, `[` (and `3`)
    /// Escape and `?` (and `8`) DEL; any other character has no control form the desktop can type, and goes as itself. Alt puts
    /// Escape in front (meta sends escape). A character that cannot travel at all (a control character) gives nothing.
    ///
    /// With the second key list (`keyNames` 2) Ctrl also makes `@`, `\`, `]`, `^`, `_` and Space their control characters (and the
    /// digits xterm maps to them: 2 NUL, 4 FS, 5 GS, 6 RS, 7 US, and `/` US; with Shift, `-` is `_` and `/` is `?`, DEL), and Alt
    /// goes with the character as one key (`M-x`, `C-M-x`). A character outside printable ASCII still goes as Escape and the text.
    public static func encode(_ character: Character, modifiers: ChordModifiers, keyNames: Int = 1) -> [KeyItem] {
        if keyNames >= 2, let items = secondList(character, modifiers: modifiers) { return items }
        var character = character
        if modifiers.contains(.shift), TerminalKey.control(forLetter: character) != nil { character = Character(String(character).uppercased()) }
        var items: [KeyItem]
        if modifiers.contains(.control), let key = TerminalKey.control(forLetter: character) { items = [.key(key)] }
        else if modifiers.contains(.control), character == "[" || character == "3" { items = [.key(.escape)] }
        else if modifiers.contains(.control), character == "?" || character == "8" { items = [.key(.backspace)] }
        else { items = Self.items(for: String(character)) }
        return items.isEmpty ? [] : meta(modifiers, items)
    }
    private static func meta(_ modifiers: ChordModifiers, _ items: [KeyItem]) -> [KeyItem] {
        modifiers.contains(.alt) ? [.key(.escape)] + items : items
    }
    /// The second key list's form of a character with Ctrl or Alt, or nil where it has none (the first list's form is used then).
    private static func secondList(_ character: Character, modifiers: ChordModifiers) -> [KeyItem]? {
        let alt = modifiers.contains(.alt)
        var character = character
        if modifiers.contains(.shift), TerminalKey.control(forLetter: character) != nil { character = Character(String(character).uppercased()) }
        if modifiers.contains(.control) {
            // A hardware key command names the key, not what Shift makes of it: Ctrl+Shift+- is Ctrl+_ and Ctrl+Shift+/ is Ctrl+?
            // (DEL), as on a US keyboard.
            if modifiers.contains(.shift) { character = ["-": "_", "/": "?"][character] ?? character }
            let base: Character
            switch character {
            case " ": return encode(.space, modifiers: alt ? [.control, .alt] : .control, keyNames: 2)
            case "2": base = "@"
            case "3", "[": return encode(.escape, modifiers: alt ? .alt : [], keyNames: 2)
            case "8", "?": return encode(.backspace, modifiers: alt ? .alt : [], keyNames: 2)
            case "4": base = "\\"
            case "5": base = "]"
            case "6": base = "^"
            case "7", "/": base = "_"
            default:
                if case .control(let letter)? = TerminalKey.control(forLetter: character) { base = letter }
                else if ModifiedKey.controlCharacters.contains(character) { base = character }
                else { return nil }
            }
            return ModifiedKey(modifiers: alt ? [.control, .alt] : .control, base: .character(base)).map { [.key(TerminalKey($0))] }
        }
        guard alt else { return nil }
        if character == " " { return encode(.space, modifiers: .alt, keyNames: 2) }
        return ModifiedKey(modifiers: .alt, base: .character(character)).map { [.key(.modified($0))] }
    }

    /// CR, LF and CRLF become Enter, tab becomes Tab, other control characters and U+2028/2029 are dropped,
    /// and the text between them stays literal. Runs are capped at the wire limit by `KeyBuffer`, not here.
    public static func items(for text: String) -> [KeyItem] {
        var items: [KeyItem] = []
        var run = String.UnicodeScalarView()
        func flush() { if !run.isEmpty { items.append(.text(String(run))); run = String.UnicodeScalarView() } }
        var previous: Unicode.Scalar?
        for scalar in text.unicodeScalars {
            defer { previous = scalar }
            switch scalar {
            case "\r": flush(); items.append(.key(.enter))
            case "\n": if previous != "\r" { flush(); items.append(.key(.enter)) }
            case "\t": flush(); items.append(.key(.tab))
            default: if !KeyItem.isForbidden(scalar) { run.append(scalar) }
            }
        }
        flush()
        return items
    }
}
