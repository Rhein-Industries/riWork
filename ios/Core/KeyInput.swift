import Foundation

// MARK: - Contract items

/// A named key the desktop `shell.keys` method accepts, or `C-a` … `C-z`.
public enum TerminalKey: Sendable, Hashable {
    case enter, tab, backTab, escape, backspace, delete
    case up, down, left, right, home, end, pageUp, pageDown
    /// A lowercase ASCII letter. Anything else fails validation.
    case control(Character)

    private static let named: [(String, TerminalKey)] = [
        ("Enter", .enter), ("Tab", .tab), ("BTab", .backTab), ("Escape", .escape), ("Backspace", .backspace), ("Delete", .delete),
        ("Up", .up), ("Down", .down), ("Left", .left), ("Right", .right), ("Home", .home), ("End", .end),
        ("PageUp", .pageUp), ("PageDown", .pageDown)
    ]
    public static func isControlLetter(_ character: Character) -> Bool {
        character.unicodeScalars.count == 1 && (97...122).contains(character.unicodeScalars.first!.value)
    }
    /// `C-<letter>` for an ASCII letter of either case; nil for anything else.
    public static func control(forLetter character: Character) -> TerminalKey? {
        guard character.unicodeScalars.count == 1, let scalar = character.unicodeScalars.first,
              (65...90).contains(scalar.value) || (97...122).contains(scalar.value) else { return nil }
        return .control(Character(String(character).lowercased()))
    }
    public init?(name: String) {
        if let match = Self.named.first(where: { $0.0 == name }) { self = match.1; return }
        let scalars = Array(name.unicodeScalars)
        guard scalars.count == 3, scalars[0] == "C", scalars[1] == "-", (97...122).contains(scalars[2].value) else { return nil }
        self = .control(Character(scalars[2]))
    }
    /// The wire name. Invalid `.control` letters still render, and fail `KeyItem.validate`.
    public var name: String {
        if case .control(let letter) = self { return "C-\(letter)" }
        return Self.named.first(where: { $0.1 == self })!.0
    }
    public var isValid: Bool { if case .control(let letter) = self { Self.isControlLetter(letter) } else { true } }
    /// Every key a person can pick for a hotkey step: the named keys, then `C-a` … `C-z`.
    public static let choices: [TerminalKey] = named.map(\.1) + (97...122).map { .control(Character(Unicode.Scalar(UInt8($0)))) }
    /// A readable name for menus and the key bar's accessibility labels.
    public var title: String {
        switch self {
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
        case "\r", "\n", "\r\n": items = Self.encode(.enter, modifiers: modifiers)
        case "\t": items = Self.encode(.tab, modifiers: modifiers)
        default: items = Self.encode(character, modifiers: modifiers)
        }
        // A character that cannot be sent (a bare control character) is not a key press: the modifiers wait for one that is.
        if !items.isEmpty { release() }
        return items
    }
    public mutating func deleteBackward() -> [KeyItem] { press(.backspace) }
    /// A bar or hardware key, with the modifiers a hardware keyboard held for it. It also takes the armed ones.
    public mutating func press(_ key: TerminalKey, modifiers held: ChordModifiers = []) -> [KeyItem] {
        let items = Self.encode(key, modifiers: armed.union(held))
        release()
        return items
    }
    /// A character key of a hardware keyboard with the modifiers held for it (Ctrl+Alt+B, say), plus the armed ones.
    public mutating func press(_ character: Character, modifiers held: ChordModifiers) -> [KeyItem] {
        let items = Self.encode(character, modifiers: armed.union(held))
        if !items.isEmpty { release() }
        return items
    }
    /// A hotkey is complete in itself: it goes out as defined, and uses up what was armed for one key.
    public mutating func run(_ hotkey: Hotkey) -> [KeyItem] {
        release()
        return hotkey.items
    }

    // MARK: Encoding

    /// The xterm modifier parameter: 1 plus 1 for Shift, 2 for Alt, 4 for Ctrl (Command is not a terminal modifier).
    public static func xtermParameter(_ modifiers: ChordModifiers) -> Int {
        1 + (modifiers.contains(.shift) ? 1 : 0) + (modifiers.contains(.alt) ? 2 : 0) + (modifiers.contains(.control) ? 4 : 0)
    }

    /// A named key with modifiers, as xterm sends it. The desktop takes named keys and plain text only, so a modified cursor or
    /// editing key travels as `Escape` and the rest of its CSI sequence as text (`ESC [1;5A` for Ctrl+Up), which reaches the
    /// program as the same bytes. Where xterm has no modified form the modifier is dropped, except that Alt puts Escape in front.
    public static func encode(_ key: TerminalKey, modifiers: ChordModifiers) -> [KeyItem] {
        let modifiers = modifiers.intersection([.shift, .control, .alt])
        guard !modifiers.isEmpty else { return [.key(key)] }
        let parameter = xtermParameter(modifiers)
        func csi(_ body: String) -> [KeyItem] { [.key(.escape), .text("[" + body)] }
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
        }
    }

    /// One character key with modifiers. Shift makes a letter upper case. Ctrl makes a letter its control key, `[` (and `3`)
    /// Escape and `?` (and `8`) DEL; any other character has no control form the desktop can type, and goes as itself. Alt puts
    /// Escape in front (meta sends escape). A character that cannot travel at all (a control character) gives nothing.
    public static func encode(_ character: Character, modifiers: ChordModifiers) -> [KeyItem] {
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
