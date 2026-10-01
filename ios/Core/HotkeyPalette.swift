import Foundation

/// One row of the hotkey menu.
public struct PaletteEntry: Sendable, Equatable, Identifiable {
    public enum Kind: Sendable, Equatable {
        case hotkey(Hotkey)
        case key(TerminalKey)
        case newHotkey
        case configure
    }
    public let id: String
    public let kind: Kind
    public let title: String
    /// What it sends, in glyphs; or what the action does.
    public let detail: String
    /// Its keyboard shortcut, if it has one: "⌘E".
    public let shortcut: String?
    /// Extra words the filter also matches ("Page Up", "Ctrl+C").
    let keywords: String
    var isAction: Bool { if case .newHotkey = kind { true } else if case .configure = kind { true } else { false } }
    var searchText: String { [title, detail, shortcut ?? "", keywords].joined(separator: " ") }
}

extension Hotkey {
    /// The steps in words, for the filter: "Ctrl+C", "Page Up", or the text typed.
    var spokenSteps: String { steps.map { step -> String in if case .key(let key) = step { key.title } else { step.symbol } }.joined(separator: " ") }
}

/// The hotkey menu: type to filter, move with ↑ and ↓, send with Return. Pure state, driven by `apply`, so the keyboard handling is
/// tested without a screen.
///
/// The rows are the person's hotkeys first (in key-bar order), then the built-in ones, then plain keys (Esc, Tab, arrows … for a
/// keyboard that lacks them), then "New hotkey…" and "Configure hotkeys…". A built-in hotkey or plain key that one of the
/// person's hotkeys already sends is left out: theirs, with its shortcut, stands in for it.
public struct HotkeyPalette: Sendable, Equatable {
    public enum Input: Sendable, Equatable {
        case text(String), backspace, clearQuery, up, down, pageUp, pageDown, activate, editSelected, newHotkey
    }
    public enum Outcome: Sendable, Equatable {
        case none
        case hotkey(Hotkey)
        case key(TerminalKey)
        case configure
        case newHotkey
        case edit(Hotkey)
    }
    public static let maxQueryLength = 40
    /// How far Page Up and Page Down jump.
    public static let pageSize = 5
    public static let plainKeys: [TerminalKey] = [.escape, .tab, .backTab, .up, .down, .left, .right, .pageUp, .pageDown, .home, .end, .enter, .backspace, .delete]

    public let entries: [PaletteEntry]
    public private(set) var query = ""
    public private(set) var selection = 0
    public private(set) var results: [PaletteEntry]

    public init(hotkeys custom: [Hotkey]) {
        func entry(_ hotkey: Hotkey) -> PaletteEntry {
            PaletteEntry(id: "hotkey.\(hotkey.id)", kind: .hotkey(hotkey), title: hotkey.label, detail: hotkey.summary, shortcut: hotkey.chord?.title, keywords: hotkey.spokenSteps)
        }
        let sent = Set(custom.map(\.items))
        var list = custom.map(entry)
        list += Hotkey.builtIn.filter { !sent.contains($0.items) }.map(entry)
        for key in Self.plainKeys where !sent.contains([.key(key)]) {
            list.append(PaletteEntry(id: "key.\(key.name)", kind: .key(key), title: key.title, detail: key.symbol, shortcut: nil, keywords: key.name))
        }
        list.append(PaletteEntry(id: "action.new", kind: .newHotkey, title: "New hotkey…", detail: "add one to the key bar", shortcut: nil, keywords: "add create hotkey"))
        list.append(PaletteEntry(id: "action.configure", kind: .configure, title: "Configure hotkeys…", detail: "edit, reorder, delete, shortcuts, Clicks template", shortcut: "⌘,", keywords: "settings edit reorder delete template clicks keyboard learn key shortcut"))
        entries = list
        results = list
    }

    public var selected: PaletteEntry? { results.indices.contains(selection) ? results[selection] : nil }

    @discardableResult
    public mutating func apply(_ input: Input) -> Outcome {
        switch input {
        case .text(let text):
            let kept = String(String.UnicodeScalarView(text.unicodeScalars.filter { !KeyItem.isForbidden($0) }))
            guard !kept.isEmpty else { return .none }
            setQuery(String((query + kept).prefix(Self.maxQueryLength)))
        case .backspace:
            if !query.isEmpty { setQuery(String(query.dropLast())) }
        case .clearQuery:
            setQuery("")
        case .up:
            if !results.isEmpty { selection = (selection - 1 + results.count) % results.count }
        case .down:
            if !results.isEmpty { selection = (selection + 1) % results.count }
        case .pageUp:
            if !results.isEmpty { selection = max(0, selection - Self.pageSize) }
        case .pageDown:
            if !results.isEmpty { selection = min(results.count - 1, selection + Self.pageSize) }
        case .activate:
            guard let selected else { return .none }
            switch selected.kind {
            case .hotkey(let hotkey): return .hotkey(hotkey)
            case .key(let key): return .key(key)
            case .newHotkey: return .newHotkey
            case .configure: return .configure
            }
        case .editSelected:
            if case .hotkey(let hotkey)? = selected?.kind, !hotkey.isBuiltIn { return .edit(hotkey) }
        case .newHotkey:
            return .newHotkey
        }
        return .none
    }

    /// A tap on a row: select it and choose it.
    @discardableResult
    public mutating func choose(index: Int) -> Outcome {
        guard results.indices.contains(index) else { return .none }
        selection = index
        return apply(.activate)
    }

    private mutating func setQuery(_ text: String) {
        query = text.drop(while: \.isWhitespace).description
        results = Self.filter(entries, by: query)
        selection = 0
    }

    // MARK: Filter

    /// All the words of the query must match; entries rank by where they match (the title's start, a word start, anywhere in the
    /// title, letters in order, then the rest of the text). Equal scores keep their order.
    static func filter(_ entries: [PaletteEntry], by query: String) -> [PaletteEntry] {
        let words = query.lowercased().split(whereSeparator: \.isWhitespace).map(String.init)
        guard !words.isEmpty else { return entries }
        var scored: [(entry: PaletteEntry, score: Int, order: Int)] = []
        for (order, entry) in entries.enumerated() {
            var total = 0
            var matched = true
            for word in words {
                guard let score = score(word, in: entry) else { matched = false; break }
                total += score
            }
            if matched { scored.append((entry, total, order)) }
        }
        return scored.sorted { $0.score != $1.score ? $0.score > $1.score : $0.order < $1.order }.map(\.entry)
    }
    private static func score(_ word: String, in entry: PaletteEntry) -> Int? {
        let title = entry.title.lowercased()
        if title == word { return 120 }
        if title.hasPrefix(word) { return 100 }
        let titleWords = title.split(whereSeparator: { !$0.isLetter && !$0.isNumber })
        if titleWords.contains(where: { $0.hasPrefix(word) }) { return 80 }
        if title.contains(word) { return 60 }
        if isSubsequence(word, of: title) { return 40 }
        let rest = [entry.detail, entry.shortcut ?? "", entry.keywords].joined(separator: " ").lowercased()
        let restWords = rest.split(whereSeparator: { !$0.isLetter && !$0.isNumber })
        if restWords.contains(where: { $0.hasPrefix(word) }) { return 30 }
        if rest.contains(word) { return 20 }
        return nil
    }
    private static func isSubsequence(_ word: String, of text: String) -> Bool {
        var remaining = word[...]
        for character in text { if character == remaining.first { remaining = remaining.dropFirst() } }
        return remaining.isEmpty
    }
}
