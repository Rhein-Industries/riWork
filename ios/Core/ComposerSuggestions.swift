import Foundation

// What a chat's composer suggests while a `/` command, an `@` file or a `&` model is typed, and what each suggestion shows and does before
// anything is sent. The phone's half of `crates/riwork-engine/src/chat/suggest.rs`: the desktop owns the catalog and sends it with
// `chat.suggestions` (`ChatSuggestionsReply`); the phone applies the same rules to it, which `Tests/Fixtures/composer-suggestions.json` pins
// for both sides.
//
// - **When the list opens** (`ComposerSuggestions.trigger`): `/` as the first thing in the draft (leading blank space allowed), because a
//   provider reads a slash command only at the start of a message; `@` at the start of the draft or after blank space. The token runs
//   from the sigil to the next blank space, the query from after the sigil to the caret. A sigil inside a word (`a/b`, `me@host`, `a&b`, a
//   URL's `?x=1&y=2`) opens nothing, nor does a token that is not a whole command name (`/usr/bin`). `&` opens as `@` does, wherever the
//   person writes, so one message can name several models (`&opus design it, then &gpt-6.1-sol build it`).
// - **`&` models** (`ComposerModelEntry`): the models the chat's provider listed, with how many of the person's own chats use each, the
//   most used first (`ComposerSuggestions.orderModels`); the chat's own model is marked. Accepting one writes a mention, `&` and the
//   model's id (`ComposerModelEntry.insertion`): a way to tell an orchestrator which model the person would like for the work the message
//   hands out. It is only text: it switches nothing, and the orchestrator still decides.
// - **Accepting** (`ComposerSuggestions.accept`) replaces the token with the entry's text and a space. It never sends: that stays the
//   person's own ⏎ or Send.
// - **Offsets** are UTF-16 code units, what `UITextView.selectedRange` counts, where the desktop counts bytes. Every rule works on Unicode
//   scalars (Rust's `char`), never on Swift's grapheme clusters or its canonical string equality, so the two sides agree on every text.

/// What was typed to open the list.
public enum ComposerSigil: String, Sendable, Equatable, Hashable, Codable {
    /// `/`: a command.
    case slash
    /// `@`: a file or folder of the chat's folder.
    case mention
    /// `&`: a model of the chat's provider, mentioned in the message.
    case model
    public var scalar: Unicode.Scalar {
        switch self {
        case .slash: "/"
        case .mention: "@"
        case .model: "&"
        }
    }
}

/// The token the caret is in that opens the list, in UTF-16 offsets of the draft (so `range` is the text view's own `NSRange`).
public struct ComposerTrigger: Sendable, Equatable, Hashable {
    public let sigil: ComposerSigil
    /// Where the sigil is.
    public let start: Int
    /// Where the token ends: the next blank space after the caret, or the draft's end. Accepting replaces `start..<end`.
    public let end: Int
    /// Where the caret is; always after the sigil and at most `end`.
    public let caret: Int
    /// What is typed between the sigil and the caret.
    public let query: String
    public init(sigil: ComposerSigil, start: Int, end: Int, caret: Int, query: String) {
        self.sigil = sigil; self.start = start; self.end = end; self.caret = caret; self.query = query
    }
    /// The token, as the text view counts it.
    public var range: NSRange { NSRange(location: start, length: end - start) }
}

public enum ComposerSuggestions {
    /// The most files the list holds at once (`SHOWN`).
    public static let shownFiles = 50

    // MARK: When the list opens

    /// The list that `text` with the caret at UTF-16 offset `caret` opens, if any (see the rules at the top of this file). A caret
    /// outside the text or inside a character (between the halves of a surrogate pair) opens nothing.
    public static func trigger(in text: String, caret: Int) -> ComposerTrigger? {
        let scalars = text.unicodeScalars, utf16 = text.utf16
        guard caret >= 0, let at = utf16.index(utf16.startIndex, offsetBy: caret, limitedBy: utf16.endIndex),
              let caretIndex = at.samePosition(in: scalars) else { return nil }
        var start = caretIndex
        while start > scalars.startIndex {
            let before = scalars.index(before: start)
            if scalars[before].properties.isWhitespace { break }
            start = before
        }
        var end = caretIndex
        while end < scalars.endIndex, !scalars[end].properties.isWhitespace { end = scalars.index(after: end) }
        guard start < scalars.endIndex else { return nil }
        let sigil: ComposerSigil
        switch scalars[start] {
        case "/" where scalars[..<start].allSatisfy(\.properties.isWhitespace): sigil = .slash
        case "@": sigil = .mention
        case "&": sigil = .model
        default: return nil
        }
        // The caret is before the sigil: the token is not being typed.
        guard start != caretIndex else { return nil }
        let afterSigil = scalars.index(after: start)
        // A command name is one word: a path typed at the start of a message is just text.
        if sigil == .slash, scalars[afterSigil..<end].contains("/") { return nil }
        return ComposerTrigger(sigil: sigil, start: utf16.distance(from: utf16.startIndex, to: start), end: utf16.distance(from: utf16.startIndex, to: end),
                               caret: caret, query: String(scalars[afterSigil..<caretIndex]))
    }

    /// `text` with the trigger's token replaced by `insert`, and the caret (a UTF-16 offset) after it. A space follows unless one already
    /// does, so the next word starts apart. A trigger that no longer fits the text is held to it, never past its ends.
    public static func accept(_ text: String, trigger: ComposerTrigger, insert: String) -> (text: String, caret: Int) {
        let count = text.utf16.count
        let lower = min(max(trigger.start, 0), count), upper = min(max(trigger.end, lower), count)
        let head = text[..<String.Index(utf16Offset: lower, in: text)], rest = text[String.Index(utf16Offset: upper, in: text)...]
        var out = String(head) + insert
        let caret: Int
        if rest.unicodeScalars.first == " " {
            caret = out.utf16.count + 1
        } else {
            out += " "
            caret = out.utf16.count
        }
        out += rest
        return (out, caret)
    }

    // MARK: Matching

    /// The commands that match `query`, best first: a name that starts with it, then one that holds it, then one that holds its letters
    /// in order; ties in `commands` order. Case is ignored.
    public static func rankCommands(_ query: String, _ commands: [ComposerCommand]) -> [ComposerCommand] {
        let query = lowered(query)
        let ranked = commands.enumerated().compactMap { offset, command -> (tier: Int, offset: Int, command: ComposerCommand)? in
            let name = lowered(command.name)
            let tier: Int
            if name.starts(with: query) { tier = 0 }
            else if holds(name, query[...]) { tier = 1 }
            else if subsequence(query, of: name[...]) { tier = 2 }
            else { return nil }
            return (tier, offset, command)
        }
        return ranked.sorted { ($0.tier, $0.offset) < ($1.tier, $1.offset) }.map(\.command)
    }

    /// The order the `&` list offers `models` in before anything is typed (`order_models`): the most used first; ties, and models never
    /// used, in the provider's order.
    public static func orderModels(_ models: [ComposerModelEntry]) -> [ComposerModelEntry] {
        models.enumerated().sorted { ($1.element.chats, $0.offset) < ($0.element.chats, $1.offset) }.map(\.element)
    }
    /// The models that match `query` by id or name, best first as `rankCommands` ranks (the best tier either reaches), with
    /// `orderModels`' order deciding ties (`rank_models`).
    public static func rankModels(_ query: String, _ models: [ComposerModelEntry]) -> [ComposerModelEntry] {
        let query = lowered(query)
        let ranked = orderModels(models).enumerated().compactMap { offset, model -> (tier: Int, offset: Int, model: ComposerModelEntry)? in
            let tiers = [model.id, model.name].compactMap { name -> Int? in
                let name = lowered(name)
                if name.starts(with: query) { return 0 }
                if holds(name, query[...]) { return 1 }
                if subsequence(query, of: name[...]) { return 2 }
                return nil
            }
            guard let tier = tiers.min() else { return nil }
            return (tier, offset, model)
        }
        return ranked.sorted { ($0.tier, $0.offset) < ($1.tier, $1.offset) }.map(\.model)
    }
    /// The id of the model the chat runs (`current_model`): the one it is set to, else its provider's default. Ids are compared as
    /// Unicode scalars, as the desktop compares them.
    public static func currentModel(_ models: [ComposerModelEntry], chosen: String?) -> String? {
        let chosen = chosen?.trimmingCharacters(in: .whitespacesAndNewlines)
        if let chosen, !chosen.isEmpty { return models.first { sameID($0.id, chosen) }?.id }
        return models.first { $0.isDefault }?.id
    }
    /// Two model ids are the same id: the same Unicode scalars (not merely canonically equal text).
    public static func sameID(_ a: String, _ b: String) -> Bool { a.unicodeScalars.elementsEqual(b.unicodeScalars) }
    /// What a model's use says (`usage_label`): how many of the person's chats of `provider` are set to it.
    public static func usageLabel(chats: UInt64, provider: ChatProvider) -> String {
        switch chats {
        case 0: "None of your \(provider.title) chats use it now"
        case 1: "1 of your \(provider.title) chats uses it now"
        default: "\(chats) of your \(provider.title) chats use it now"
        }
    }

    /// The entries that match `query`, best first and at most `limit`: a name that starts with it, then a path that does or has a part that
    /// does, then a path that holds it, then one that holds its letters in order. Ties go to the shallower path, then the shorter (in
    /// characters), then the first by character. Case is ignored. An empty query keeps every entry, the shallowest first.
    public static func rankFiles(_ query: String, _ entries: [ComposerFileEntry], limit: Int) -> [ComposerFileEntry] {
        ComposerCatalog.rank(query, entries.map(ComposerCatalog.FileKey.init), limit: limit).map { entries[$0] }
    }

    // MARK: Sizes

    /// A size as a preview says it: bytes below 1 KB, whole KB below 1 MB, then MB to a tenth.
    public static func sizeLabel(_ bytes: UInt64) -> String {
        let kb: UInt64 = 1024, mb: UInt64 = 1024 * 1024
        if bytes == 1 { return "1 byte" }
        if bytes < kb { return "\(bytes) bytes" }
        if bytes < mb { return "\(min((bytes + kb / 2) / kb, 1023)) KB" }
        // (bytes * 10 + MB / 2) / MB, without the product: it would overflow for the largest sizes.
        let tenths = bytes / mb * 10 + ((bytes % mb) * 10 + mb / 2) / mb
        return "\(tenths / 10).\(tenths % 10) MB"
    }

    // MARK: Scalars

    /// `text` lowercased, as Unicode scalars: what the matching compares.
    static func lowered(_ text: String) -> [Unicode.Scalar] { Array(text.lowercased().unicodeScalars) }
    /// Whether `needle` appears in `haystack` as one run.
    static func holds(_ haystack: [Unicode.Scalar], _ needle: ArraySlice<Unicode.Scalar>) -> Bool {
        guard !needle.isEmpty else { return true }
        guard needle.count <= haystack.count else { return false }
        for start in 0...(haystack.count - needle.count) where haystack[start] == needle.first! {
            if haystack[start..<(start + needle.count)].elementsEqual(needle) { return true }
        }
        return false
    }
    /// Whether `needle`'s characters appear in `haystack` in order.
    static func subsequence(_ needle: [Unicode.Scalar], of haystack: ArraySlice<Unicode.Scalar>) -> Bool {
        var letters = haystack.makeIterator()
        return needle.allSatisfy { wanted in
            while let letter = letters.next() { if letter == wanted { return true } }
            return false
        }
    }
}

// MARK: - Slash commands

/// Who acts on a command once the message is sent.
public enum ComposerCommandHandler: String, Sendable, Equatable, Hashable, Codable {
    /// The text goes to the provider as typed and the provider runs the command.
    case provider
    /// RiWork acts on it before anything reaches the provider.
    case riwork
}

/// A slash command as a composer shows it (`Command`).
public struct ComposerCommand: Sendable, Equatable, Hashable, Decodable {
    /// Without the slash.
    public let name: String
    /// What it is, in one line.
    public let summary: String
    /// What may follow the name, as the list shows it; nil takes nothing.
    public let argument: String?
    /// What sending a message that starts with it does.
    public let sends: String
    public let handler: ComposerCommandHandler
    public init(name: String, summary: String, argument: String? = nil, sends: String, handler: ComposerCommandHandler) {
        self.name = name; self.summary = summary; self.argument = argument; self.sends = sends; self.handler = handler
    }
    private enum CodingKeys: String, CodingKey { case name, summary, argument, sends, handler }
    /// A command needs its name (one that a `/` can complete: no blank space, no slash), its words and a handler the phone knows; one
    /// without is left out (`Lenient`), never the reply. Only the argument may be missing.
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let name = try c.decode(String.self, forKey: .name)
        guard !name.isEmpty, !name.unicodeScalars.contains(where: { $0.properties.isWhitespace || $0 == "/" }) else {
            throw DecodingError.dataCorruptedError(forKey: .name, in: c, debugDescription: "not a command name")
        }
        self.init(name: name, summary: try c.decode(String.self, forKey: .summary), argument: c.tolerant(String.self, forKey: .argument),
                  sends: try c.decode(String.self, forKey: .sends), handler: try c.decode(ComposerCommandHandler.self, forKey: .handler))
    }
    /// What accepting it puts in the draft.
    public var insertion: String { "/" + name }
}

// MARK: - Files

/// A file or folder of the chat's folder (`FileEntry`).
public struct ComposerFileEntry: Sendable, Equatable, Hashable, Decodable {
    /// Relative to the chat's folder, `/` between its parts, no trailing `/`.
    public let path: String
    public let dir: Bool
    /// A file's size; nil for a folder, or when the desktop could not read it.
    public let bytes: UInt64?
    public init(path: String, dir: Bool = false, bytes: UInt64? = nil) { self.path = path; self.dir = dir; self.bytes = bytes }
    private enum CodingKeys: String, CodingKey { case path, dir, bytes }
    /// An entry needs a path; `dir` and `bytes` default as the desktop's do (an odd value is read as missing).
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let path = try c.decode(String.self, forKey: .path)
        guard !path.isEmpty else { throw DecodingError.dataCorruptedError(forKey: .path, in: c, debugDescription: "an empty path") }
        self.init(path: path, dir: c.tolerant(Bool.self, forKey: .dir) ?? false, bytes: c.tolerant(UInt64.self, forKey: .bytes))
    }
    /// The last part of its path.
    public var name: String { path.split(separator: "/", omittingEmptySubsequences: false).last.map(String.init) ?? path }
    /// The path as the list shows it: a folder's with a trailing `/`.
    public var shownPath: String { dir ? path + "/" : path }
    /// What accepting it puts in the draft: its path, a folder's with a trailing `/`, in backticks when it has blank space so it reads as
    /// one reference.
    public var insertion: String {
        let path = shownPath
        return path.unicodeScalars.contains(where: \.properties.isWhitespace) ? "`\(path)`" : path
    }
    /// What kind of thing it is, with its size for a file: "Folder", "File · 12 KB", "File".
    public var kind: String {
        if dir { return "Folder" }
        return bytes.map { "File · \(ComposerSuggestions.sizeLabel($0))" } ?? "File"
    }
}

/// One walk of a chat's folder (`Listing`).
public struct ComposerFileListing: Sendable, Equatable, Decodable {
    /// Breadth first, each folder's entries by name.
    public let entries: [ComposerFileEntry]
    /// The walk stopped at a bound: there is more than `entries`.
    public let truncated: Bool
    public init(entries: [ComposerFileEntry] = [], truncated: Bool = false) { self.entries = entries; self.truncated = truncated }
    private enum CodingKeys: String, CodingKey { case entries, truncated }
    /// An entry the phone cannot read is left out, never the listing.
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.init(entries: c.leniently([ComposerFileEntry].self, forKey: .entries), truncated: c.tolerant(Bool.self, forKey: .truncated) ?? false)
    }
}

// MARK: - Models

/// A model of the chat's provider as the `&` list shows it, with how much the person uses it (`ModelEntry`).
public struct ComposerModelEntry: Sendable, Equatable, Hashable, Decodable {
    /// What the chat is set to: the provider's model id.
    public let id: String
    public let name: String
    public let description: String
    /// The provider uses it when the chat names none.
    public let isDefault: Bool
    /// How many of the person's own chats of the provider are set to it now.
    public let chats: UInt64
    public init(id: String, name: String, description: String = "", isDefault: Bool = false, chats: UInt64 = 0) {
        self.id = id; self.name = name; self.description = description; self.isDefault = isDefault; self.chats = chats
    }
    private enum CodingKeys: String, CodingKey { case id, name, description, isDefault = "is_default", chats }
    /// A model needs an id that is not blank and has no blank space inside (its mention must read as one word, as the desktop's
    /// `model_entries` has it), and a name; one without is left out (`Lenient`), never the reply. The rest default as the desktop's do.
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let id = try c.decode(String.self, forKey: .id)
        let trimmed = id.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !trimmed.unicodeScalars.contains(where: \.properties.isWhitespace) else {
            throw DecodingError.dataCorruptedError(forKey: .id, in: c, debugDescription: "a blank model id, or one of more than one word")
        }
        self.init(id: id, name: try c.decode(String.self, forKey: .name), description: c.tolerant(String.self, forKey: .description) ?? "",
                  isDefault: c.tolerant(Bool.self, forKey: .isDefault) ?? false, chats: c.tolerant(UInt64.self, forKey: .chats) ?? 0)
    }
    /// What accepting it puts in the draft (`ModelEntry::insertion`): a mention of it, `&` and its id as the provider takes it
    /// (`&gpt-6.1-sol`), never its display name.
    public var insertion: String { "&" + id.trimmingCharacters(in: .whitespacesAndNewlines) }
    /// The row's second line (`model_detail`): whether the chat runs it, and how many chats use it.
    public func detail(current: Bool) -> String {
        let used = switch chats {
        case 0: "not used yet"
        case 1: "used in 1 chat"
        default: "used in \(chats) chats"
        }
        return current ? "Current · \(used)" : used.prefix(1).uppercased() + used.dropFirst()
    }
}

// MARK: - Previews

/// What the card beside the list says about the highlighted entry before it is accepted (`Preview`).
public struct ComposerPreview: Sendable, Equatable {
    /// The entry as it reads: `/compact`, `src/main.rs`.
    public let title: String
    /// What kind of thing it is, with its size for a file: "Claude command", "File · 12 KB".
    public let kind: String
    /// What it is: a command's summary and argument. Empty for a file: the phone does not read files over the relay.
    public let body: [String]
    /// What accepting puts in the draft.
    public let inserts: String
    /// What happens to it: what sending does for a command, that a path is only text.
    public let effect: String
    public init(title: String, kind: String, body: [String], inserts: String, effect: String) {
        self.title = title; self.kind = kind; self.body = body; self.inserts = inserts; self.effect = effect
    }
    /// The preview of a command offered to a chat of `provider` (`command_preview`).
    public init(command: ComposerCommand, provider: ChatProvider) {
        self.init(title: command.insertion,
                  kind: command.handler == .provider ? "\(provider.title) command" : "RiWork command",
                  body: [command.summary, command.argument.map { "Takes \($0)" } ?? "Takes no argument"],
                  inserts: command.insertion, effect: "Sending: \(command.sends)")
    }
    public static let fileEffect = "Puts in its path, relative to the chat's folder; nothing is attached or sent."
    /// The preview of a file or folder (`file_preview` without a peek).
    public init(file: ComposerFileEntry) {
        self.init(title: file.insertion, kind: file.kind, body: [], inserts: file.insertion, effect: Self.fileEffect)
    }
    /// The preview of a model offered to a chat of `provider`, `current` if the chat runs it (`model_preview`): its mention, its name
    /// where that is not its id, what it is, and the use the list is sorted by.
    public init(model: ComposerModelEntry, current: Bool, provider: ChatProvider) {
        var body: [String] = []
        let name = model.name.trimmingCharacters(in: .whitespacesAndNewlines)
        if !name.isEmpty, !ComposerSuggestions.sameID(name, model.id.trimmingCharacters(in: .whitespacesAndNewlines)) { body.append(name) }
        let description = model.description.trimmingCharacters(in: .whitespacesAndNewlines)
        if !description.isEmpty { body.append(description) }
        body.append(ComposerSuggestions.usageLabel(chats: model.chats, provider: provider))
        self.init(title: model.insertion, kind: "\(provider.title) model" + (current ? " · current" : ""), body: body, inserts: model.insertion,
                  effect: "Inserts \(model.insertion) in the message. It does not switch this chat; the orchestrator still decides.")
    }
}

// MARK: - A chat's catalog

/// A chat's commands and its folder's listing, from one `chat.suggestions` reply, prepared once for filtering on every key: each path
/// lowercased and measured ahead. Never changes; a newer reply is a new catalog.
public final class ComposerCatalog: Sendable {
    public let chatID: String
    public let provider: ChatProvider
    /// The chat's folder on the Mac, which the paths are relative to.
    public let root: String
    public let commands: [ComposerCommand]
    public let files: [ComposerFileEntry]
    /// The desktop's walk stopped at a bound: the folder has more than `files`.
    public let filesTruncated: Bool
    /// The most entries one listing holds (`FILE_BOUND`).
    public let fileBound: Int
    /// The models the chat's provider listed, in its order, with their use; none from a desktop before `&`.
    public let models: [ComposerModelEntry]
    private let keys: [FileKey]

    public init(chatID: String, provider: ChatProvider, root: String = "", commands: [ComposerCommand], files: [ComposerFileEntry], filesTruncated: Bool = false, fileBound: Int = 5000,
                models: [ComposerModelEntry] = []) {
        self.chatID = chatID; self.provider = provider; self.root = root; self.commands = commands; self.files = files
        self.filesTruncated = filesTruncated; self.fileBound = fileBound; self.models = models
        keys = files.map(FileKey.init)
    }
    public convenience init(reply: ChatSuggestionsReply) {
        self.init(chatID: reply.chatID, provider: reply.provider, root: reply.root, commands: reply.commands, files: reply.files.entries,
                  filesTruncated: reply.files.truncated, fileBound: reply.fileBound, models: reply.models)
    }

    public func rankCommands(_ query: String) -> [ComposerCommand] { ComposerSuggestions.rankCommands(query, commands) }
    public func rankModels(_ query: String) -> [ComposerModelEntry] { ComposerSuggestions.rankModels(query, models) }
    public func rankFiles(_ query: String, limit: Int = ComposerSuggestions.shownFiles) -> [ComposerFileEntry] {
        Self.rank(query, keys, limit: limit).map { files[$0] }
    }

    /// What a path is matched and ordered by.
    struct FileKey: Sendable {
        /// The path lowercased, and where its last part starts in it.
        let lowered: [Unicode.Scalar]
        let nameStart: Int
        /// Its parts less one, and its length in characters.
        let depth: Int
        let length: Int
        /// The path as it is, for the last tie: compared by Unicode scalar value, as Rust compares strings (by their UTF-8 bytes).
        let scalars: [UInt32]
        init(_ entry: ComposerFileEntry) {
            lowered = ComposerSuggestions.lowered(entry.path)
            nameStart = lowered.lastIndex(of: "/").map { $0 + 1 } ?? 0
            scalars = entry.path.unicodeScalars.map(\.value)
            depth = scalars.reduce(0) { $0 + ($1 == 0x2F ? 1 : 0) }
            length = scalars.count
        }
    }
    /// The positions in `keys` of the matching entries, best first and at most `limit` (see `ComposerSuggestions.rankFiles`).
    static func rank(_ query: String, _ keys: [FileKey], limit: Int) -> [Int] {
        let query = ComposerSuggestions.lowered(query)
        let slashed = ["/"] + query
        let ranked = keys.indices.compactMap { index -> (tier: Int, index: Int)? in
            let key = keys[index]
            let tier: Int
            if query.isEmpty || key.lowered[key.nameStart...].starts(with: query) { tier = 0 }
            else if key.lowered.starts(with: query) || ComposerSuggestions.holds(key.lowered, slashed[...]) { tier = 1 }
            else if ComposerSuggestions.holds(key.lowered, query[...]) { tier = 2 }
            else if ComposerSuggestions.subsequence(query, of: key.lowered[...]) { tier = 3 }
            else { return nil }
            return (tier, index)
        }
        let sorted = ranked.sorted { a, b in
            let x = keys[a.index], y = keys[b.index]
            if (a.tier, x.depth, x.length) != (b.tier, y.depth, y.length) { return (a.tier, x.depth, x.length) < (b.tier, y.depth, y.length) }
            if x.scalars != y.scalars { return x.scalars.lexicographicallyPrecedes(y.scalars) }
            return a.index < b.index
        }
        return sorted.prefix(max(0, limit)).map(\.index)
    }
}

/// What a composer knows of its chat's catalog right now.
public enum ComposerCatalogState: Sendable, Equatable {
    /// The desktop does not answer `chat.suggestions`: no list opens.
    case unsupported
    /// Being read and nothing read before: an `@` list says the files are being listed, a `/` list waits.
    case loading
    /// The read failed (or the link is down) and nothing was read before: no list opens.
    case unavailable
    /// What the desktop said, possibly while a newer read is on its way.
    case ready(ComposerCatalog)

    public static func == (lhs: Self, rhs: Self) -> Bool {
        switch (lhs, rhs) {
        case (.unsupported, .unsupported), (.loading, .loading), (.unavailable, .unavailable): true
        case (.ready(let a), .ready(let b)): a === b
        default: false
        }
    }
}

/// The catalogs of the chats a composer opened a list in, by chat id. A catalog is read once when a list first opens in a chat, is fresh
/// for `timeToLive`, and is read again on the next list after that while the old one is still shown; one read per chat is on the wire at
/// a time. A read that failed is not tried again for `retryDelay`. The `keptChats` chats read most recently are kept (a listing can be
/// 5000 entries). A chat that went on with another provider (`switch`) has the catalog of the one it left: given the chat's provider, that
/// catalog is not offered (its commands are the other provider's) and is read again at once.
public struct ComposerCatalogCache: Sendable {
    public static let timeToLive: Duration = .seconds(30)
    public static let retryDelay: Duration = .seconds(10)
    public static let keptChats = 6

    private struct Entry: Sendable {
        var catalog: ComposerCatalog?
        var readAt: ContinuousClock.Instant?
        var fetching = false
        var failedAt: ContinuousClock.Instant?
        var usedAt: ContinuousClock.Instant
    }
    private var entries: [String: Entry] = [:]
    /// The connection on which a desktop that announced `chat_suggestions` still answered that it does not know the method: no list
    /// opens until the next connection.
    public var refusedConnection: UUID?

    public init() {}

    /// What a composer of `chatID`, a chat of `provider` now (nil: not known), shows from.
    public func state(for chatID: String, provider: ChatProvider? = nil, connected: Bool) -> ComposerCatalogState {
        guard let entry = entries[chatID] else { return connected ? .loading : .unavailable }
        if let catalog = entry.catalog, Self.serves(catalog, provider) { return .ready(catalog) }
        if entry.fetching { return .loading }
        return entry.failedAt == nil && connected ? .loading : .unavailable
    }
    /// The chat's catalog was read less than `timeToLive` before `now`, for the chat's provider (`provider`; nil: not known).
    public func isFresh(_ chatID: String, provider: ChatProvider? = nil, now: ContinuousClock.Instant) -> Bool {
        guard let entry = entries[chatID], let readAt = entry.readAt, let catalog = entry.catalog, Self.serves(catalog, provider) else { return false }
        return now - readAt < Self.timeToLive
    }
    /// Whether a list open in `chatID`, a chat of `provider` now, at `now` should have the catalog read: not while a read is on its way,
    /// while the catalog is fresh, or for `retryDelay` after a read failed. Changes nothing, so asking on every key costs nothing.
    public func wantsFetch(_ chatID: String, provider: ChatProvider? = nil, now: ContinuousClock.Instant) -> Bool {
        guard let entry = entries[chatID] else { return true }
        if entry.fetching || isFresh(chatID, provider: provider, now: now) { return false }
        if let failedAt = entry.failedAt, now - failedAt < Self.retryDelay { return false }
        return true
    }
    /// A list opened in `chatID` at `now`: true when a read should start (`wantsFetch`), which is then counted as on its way.
    public mutating func beginFetch(_ chatID: String, provider: ChatProvider? = nil, now: ContinuousClock.Instant) -> Bool {
        guard wantsFetch(chatID, provider: provider, now: now) else { return false }
        var entry = entries[chatID] ?? Entry(usedAt: now)
        entry.usedAt = now; entry.fetching = true
        entries[chatID] = entry
        return true
    }
    /// The read answered.
    public mutating func store(_ catalog: ComposerCatalog, for chatID: String, now: ContinuousClock.Instant) {
        entries[chatID] = Entry(catalog: catalog, readAt: now, usedAt: entries[chatID]?.usedAt ?? now)
        guard entries.count > Self.keptChats else { return }
        let spare = entries.filter { $0.key != chatID && !$0.value.fetching }.sorted { $0.value.usedAt < $1.value.usedAt }
        for (id, _) in spare.prefix(entries.count - Self.keptChats) { entries[id] = nil }
    }
    /// The read failed; what was read before stays.
    public mutating func fail(_ chatID: String, now: ContinuousClock.Instant) {
        guard var entry = entries[chatID] else { return }
        entry.fetching = false; entry.failedAt = now
        entries[chatID] = entry
    }
    /// The read's answer is of no use (the connection it went on is gone, or the method is not there): nothing failed, nothing changes.
    public mutating func abandon(_ chatID: String) {
        entries[chatID]?.fetching = false
    }
    /// How many chats have an entry.
    public var count: Int { entries.count }
    /// Whether `catalog` is for a chat of `provider`: its commands are that provider's.
    private static func serves(_ catalog: ComposerCatalog, _ provider: ChatProvider?) -> Bool { provider.map { $0 == catalog.provider } ?? true }
}

// MARK: - The list

/// One row of the list.
public enum ComposerSuggestion: Sendable, Equatable, Hashable, Identifiable {
    case command(ComposerCommand)
    case file(ComposerFileEntry)
    /// A model of the chat's provider, and whether the chat runs it now.
    case model(ComposerModelEntry, current: Bool)
    /// Whether it is the same entry as `other`: a model by its id, whatever its use or mark.
    public func isSame(as other: ComposerSuggestion) -> Bool {
        if case .model(let a, _) = self, case .model(let b, _) = other { return ComposerSuggestions.sameID(a.id, b.id) }
        return self == other
    }
    public var id: String {
        switch self {
        case .command(let command): command.insertion
        case .file(let file): "@" + file.shownPath
        case .model(let model, _): "&" + model.id
        }
    }
    /// The row's first line: `/compact`, `src/`, `src/main.rs`, `&gpt-6.1-sol`.
    public var title: String {
        switch self {
        case .command(let command): command.insertion
        case .file(let file): file.shownPath
        case .model(let model, _): model.insertion
        }
    }
    /// The row's second line: a command's summary, a file's kind and size, whether the chat runs a model and how much it is used.
    public var detail: String {
        switch self {
        case .command(let command): command.summary
        case .file(let file): file.kind
        case .model(let model, let current): model.detail(current: current)
        }
    }
    /// What accepting it puts in the draft: a model's mention as for any other row, the chat's own model's too.
    public var insertion: String {
        switch self {
        case .command(let command): command.insertion
        case .file(let file): file.insertion
        case .model(let model, _): model.insertion
        }
    }
}

/// The `/`, `@` and `&` list of a composer, without drawing: which token opened it, what matches, which row is highlighted, and what
/// accepting puts in the draft. The composer calls `update` with every change to its text or caret; filtering is over the catalog it
/// already has, so typing never waits for the Mac.
///
/// - The list shows when a token opens it and something matches; with nothing matching it is hidden, except while an `@` list waits for
///   the folder's first listing ("Listing files…", `listingFiles`).
/// - ↑ and ↓ (`move`) wrap around at the ends, as the New terminal sheet's list does.
/// - Escape or the close button (`dismiss`) closes it for that token: typing on in it keeps it closed, and it opens again only once the
///   token has gone (the caret left it, or it was deleted).
/// - Accepting (`accepted`, or a tap on the highlighted row) gives the text and caret to put in the composer. Nothing is ever sent.
public struct ComposerSuggestionList: Sendable, Equatable {
    public enum Step: Sendable, Equatable { case up, down }

    /// The text the list was last updated with.
    public private(set) var text = ""
    /// The token the list is for; nil when no token opens one, or it was dismissed.
    public private(set) var trigger: ComposerTrigger?
    public private(set) var items: [ComposerSuggestion] = []
    public private(set) var highlighted = 0
    /// An `@` list waits for the folder's first listing.
    public private(set) var listingFiles = false
    /// An `@` list from a listing that stopped at a bound: the folder has more than was searched.
    public private(set) var filesTruncated = false
    /// The chat's provider, as its catalog says (for a command's or a model's preview).
    public private(set) var provider: ChatProvider?
    /// The token Escape closed the list for, by its sigil and where the sigil is.
    private var dismissed: Mark?
    /// The person moved the highlight for this query: a `&` list read again keeps it on their model; one never moved keeps it on the
    /// first row, the most used.
    private var moved = false
    /// What `items` were ranked from, so a caret that moves without changing the query does not rank them again.
    private var ranked: Ranked?

    private struct Mark: Sendable, Equatable { let sigil: ComposerSigil; let start: Int }
    /// The query as the scalars the matching compares, not as a `String`: canonically equal text (`é` and `e` with a combining acute) is
    /// another query.
    private struct Ranked: Sendable, Equatable { let sigil: ComposerSigil; let query: [Unicode.Scalar]; let catalog: ComposerCatalogState; let chosenModel: String? }

    public init() {}

    /// The list is on screen.
    public var isShown: Bool { trigger != nil && (listingFiles || !items.isEmpty) }
    public var highlightedItem: ComposerSuggestion? { items.indices.contains(highlighted) ? items[highlighted] : nil }
    /// The card for the highlighted row.
    public var preview: ComposerPreview? {
        switch highlightedItem {
        case .command(let command)?: provider.map { ComposerPreview(command: command, provider: $0) }
        case .file(let file)?: ComposerPreview(file: file)
        case .model(let model, let current)?: provider.map { ComposerPreview(model: model, current: current, provider: $0) }
        case nil: nil
        }
    }

    /// The text or the caret changed (`caret` is a UTF-16 offset; nil when text is selected rather than a caret placed), or the catalog
    /// did, or the model the chat is set to (`chosenModel`, nil for its default). A new query highlights the best match again; the same
    /// query keeps the highlighted row where it can.
    public mutating func update(text: String, caret: Int?, catalog: ComposerCatalogState, chosenModel: String? = nil) {
        self.text = text
        guard let found = caret.flatMap({ ComposerSuggestions.trigger(in: text, caret: $0) }) else { dismissed = nil; close(); return }
        if let dismissed {
            if dismissed == Mark(sigil: found.sigil, start: found.start) { close(); return }
            self.dismissed = nil
        }
        let previous = trigger, kept = highlightedItem
        trigger = found
        listingFiles = false; filesTruncated = false
        switch catalog {
        case .unsupported, .unavailable:
            items = []; ranked = nil
        case .loading:
            items = []; ranked = nil
            listingFiles = found.sigil == .mention
        case .ready(let catalog):
            provider = catalog.provider
            filesTruncated = found.sigil == .mention && catalog.filesTruncated
            let current = ComposerSuggestions.currentModel(catalog.models, chosen: chosenModel)
            let key = Ranked(sigil: found.sigil, query: Array(found.query.unicodeScalars), catalog: .ready(catalog), chosenModel: current)
            if ranked != key {
                items = switch found.sigil {
                case .slash: catalog.rankCommands(found.query).map { .command($0) }
                case .mention: catalog.rankFiles(found.query).map { .file($0) }
                case .model: catalog.rankModels(found.query).map { model in .model(model, current: current.map { ComposerSuggestions.sameID(model.id, $0) } ?? false) }
                }
                ranked = key
            }
        }
        let sameQuery = previous.map {
            $0.sigil == found.sigil && $0.start == found.start && $0.query.unicodeScalars.elementsEqual(found.query.unicodeScalars)
        } ?? false
        // A new query starts at the best match; the same one keeps the person's row (a model by its id, whatever its use).
        if !sameQuery { moved = false }
        let keep = sameQuery && (moved || found.sigil != .model)
        highlighted = keep ? kept.flatMap { kept in items.firstIndex { $0.isSame(as: kept) } } ?? 0 : 0
    }

    /// ↑ or ↓: the next row up or down, around at the ends.
    public mutating func move(_ step: Step) {
        guard !items.isEmpty else { return }
        highlighted = (highlighted + (step == .down ? 1 : items.count - 1)) % items.count
        moved = true
    }
    public mutating func highlight(_ index: Int) {
        if items.indices.contains(index) { highlighted = index; moved = true }
    }
    /// A tap on a row: one that is not highlighted is highlighted (its card shows); the highlighted one is accepted.
    public mutating func tap(_ index: Int) -> (text: String, caret: Int)? {
        guard items.indices.contains(index) else { return nil }
        if index == highlighted { return accepted() }
        highlighted = index; moved = true
        return nil
    }
    /// Escape or the close button: closed until the token goes.
    public mutating func dismiss() {
        if let trigger { dismissed = Mark(sigil: trigger.sigil, start: trigger.start) }
        close()
    }
    /// The text with the highlighted row put in for the token, and where the caret goes; nil when nothing is highlighted. A model goes
    /// in as its mention; nothing else happens to the chat.
    public func accepted() -> (text: String, caret: Int)? {
        guard let trigger, let item = highlightedItem else { return nil }
        return ComposerSuggestions.accept(text, trigger: trigger, insert: item.insertion)
    }

    private mutating func close() {
        trigger = nil; items = []; highlighted = 0; listingFiles = false; filesTruncated = false; ranked = nil; moved = false
    }
}

// MARK: - A held Return

/// A hardware ⏎ that put a suggestion in may still be held once the list has closed, and a key command repeats while its key is down: a
/// repeat must not send the draft the ⏎ just completed. A ⏎ that comes within `window` of the accepting one (or of a repeat swallowed
/// since) is taken for a repeat and swallowed, until the key is seen to come up or the repeats stop.
public struct HeldReturn: Sendable {
    public static let window: Duration = .milliseconds(600)
    private var last: ContinuousClock.Instant?
    public init() {}
    /// ⏎ put a suggestion in at `now`.
    public mutating func accepted(at now: ContinuousClock.Instant) { last = now }
    /// Whether a ⏎ at `now` is the held key's repeat; the hold goes on if it is.
    public mutating func isRepeat(at now: ContinuousClock.Instant) -> Bool {
        guard let last, now - last < Self.window else {
            self.last = nil
            return false
        }
        self.last = now
        return true
    }
    /// The key came up.
    public mutating func released() { last = nil }
}
