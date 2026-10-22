import Foundation
import RiWorkCore

/// Immutable render input: every lookup is indexed, and Markdown references are parsed when its item changes.
struct ChatFileIndex: Equatable {
    private let generation = UUID()
    static func == (lhs: Self, rhs: Self) -> Bool { lhs.generation == rhs.generation }
    var entries: [ChatFileEntry] = []
    var byItem: [String: [ChatFileEntry]] = [:]
    var byTurn: [String: [ChatFileEntry]] = [:]
    var cwdByTurn: [String: String] = [:]
    var markdownReferences: [String: [ChatPathRef]] = [:]

    func resolve(artifacts: [ChatArtifactRef]) -> [ChatFileEntry] {
        var seen = Set<String>()
        return artifacts.compactMap { artifact in
            let candidates = byItem[artifact.itemID] ?? []
            let file: ChatFileEntry?
            if let index = artifact.imageIndex { file = candidates.first { $0.imageIndex == index } }
            else if let path = artifact.path {
                file = candidates.first { $0.path == path.path && $0.line == path.line.map(Int.init(clamping:)) }
                    ?? candidates.first { $0.path == path.path }
            } else { file = nil }
            guard let file, seen.insert(file.id).inserted else { return nil }
            return file
        }
    }

    func references(in source: String, files: [ChatFileEntry]) -> [ChatFileEntry] {
        (markdownReferences[source] ?? []).compactMap { ref in
            ChatFileCatalog.resolve(path: ref.path, line: ref.line.map(Int.init(clamping:)), files: files)
        }
    }
}

/// The file references (`ChatFileCatalog.references(in:)`) of a message that is still arriving. A streamed message grows at its end, so
/// the lines it had complete at the last delta are still there: their references are kept, and only the lines after them are read again.
/// A text that does not start with those lines (an edit, another message) is read from the start.
struct ChatGrowingReferences {
    /// The text read last; the first `complete` UTF-8 bytes of it are whole lines, whose references (by pass) are `head`.
    private var text = ""
    private var complete = 0
    private var head: [[ChatPathRef]] = []

    mutating func references(in source: String) -> [ChatPathRef] {
        let bytes = source.utf8
        if complete > 0, !bytes.starts(with: text.utf8.prefix(complete)) { complete = 0; head = [] }
        let start = bytes.index(bytes.startIndex, offsetBy: complete)
        if let newline = bytes[start...].lastIndex(of: 10) {
            let end = bytes.index(after: newline)
            head = Self.joined(head, ChatFileCatalog.referencePasses(in: String(source[start..<end])))
            complete = bytes.distance(from: bytes.startIndex, to: end)
        }
        text = source
        return ChatFileCatalog.merged(Self.joined(head, ChatFileCatalog.referencePasses(in: String(source[bytes.index(bytes.startIndex, offsetBy: complete)...]))))
    }

    /// The passes of a text followed by those of the text after it.
    private static func joined(_ first: [[ChatPathRef]], _ second: [[ChatPathRef]]) -> [[ChatPathRef]] {
        guard !first.isEmpty else { return second }
        return zip(first, second).map { before, after in before + after.filter { !before.contains($0) } }
    }
}

/// Kept on the chat model, outside SwiftUI observation. A token changes one item, so historical items and turns retain
/// their parsed files and references. State, keyboard and disclosure renders return the same index immediately.
@MainActor final class ChatFileCatalogCache {
    private struct ItemCache {
        var item: ChatItem
        var files: [ChatFileEntry]
        var references: [String: [ChatPathRef]]
        /// The references of every paragraph and heading of the item's text, none included: a delta to a streamed message changes its
        /// last ones, and the others are not read again.
        var paragraphs: [String: [ChatPathRef]] = [:]
        /// The message's own references while it is still arriving.
        var growing = ChatGrowingReferences()
    }
    private struct TurnCache {
        var members: [ChatItem]
        var files: [ChatFileEntry]
        var references: [String: [ChatPathRef]]
        var cwd: String?
    }
    private var revision: UInt64?
    private var imageDirectory: URL?
    private var itemCache: [String: ItemCache] = [:]
    private var turnCache: [String: TurnCache] = [:]
    /// The turns `value` was put together from, in order.
    private var turnOrder: [String] = []
    private var value = ChatFileIndex()
    /// Useful to verify that streaming does not re-parse historical file references.
    private(set) var rebuiltItemCount = 0
    private(set) var rebuiltTurnCount = 0

    func index(items: [ChatItem], revision: UInt64, images: ChatAttachmentImages = .standard) -> ChatFileIndex {
        guard self.revision != revision || imageDirectory != images.directory else { return value }
        if imageDirectory != images.directory { itemCache = [:]; turnCache = [:] }
        self.revision = revision; imageDirectory = images.directory
        var order: [String] = []
        var membersByTurn: [String: [ChatItem]] = [:]
        for item in items {
            let turn = item.turnID ?? "legacy:\(item.id)"
            if membersByTurn[turn] == nil { order.append(turn) }
            membersByTurn[turn, default: []].append(item)
        }
        // Whether a turn's files, references or working directory changed: a delta to an answer's prose changes none.
        var changed = order != turnOrder
        for turn in order {
            let members = membersByTurn[turn] ?? []
            if turnCache[turn]?.members != members {
                var parsedFiles: [String: [ChatFileEntry]] = [:]
                var references: [String: [ChatPathRef]] = [:]
                var cwd: String?
                for item in members {
                    if itemCache[item.id]?.item != item {
                        // What was read of an item that is still arriving is kept for its next delta; a finished one keeps nothing.
                        let arriving = item.status == .inProgress
                        let previous = itemCache[item.id]
                        var growing = previous?.growing ?? ChatGrowingReferences()
                        var messageReferences: [ChatPathRef]?
                        if arriving, case .agentMessage(let text) = item.body { messageReferences = growing.references(in: text) }
                        let files = ChatFileCatalog.entries(item: item, images: images, messageReferences: messageReferences)
                        let source: String?
                        switch item.body {
                        case .agentMessage(let text), .userMessage(let text), .reasoning(let text): source = text
                        default: source = nil
                        }
                        let (references, paragraphs) = source.map { Self.references($0, known: previous?.paragraphs ?? [:]) } ?? ([:], [:])
                        itemCache[item.id] = ItemCache(item: item, files: files, references: references, paragraphs: arriving ? paragraphs : [:],
                                                       growing: arriving ? growing : ChatGrowingReferences())
                        rebuiltItemCount += 1
                    }
                    parsedFiles[item.id] = itemCache[item.id]?.files ?? []
                    references.merge(itemCache[item.id]?.references ?? [:]) { first, _ in first }
                    if case .command(_, let directory, _, _) = item.body, cwd == nil { cwd = directory }
                }
                let rebuilt = TurnCache(members: members, files: ChatFileCatalog.entries(items: members, images: images, itemEntries: parsedFiles),
                                        references: references, cwd: cwd)
                if let before = turnCache[turn], before.files == rebuilt.files, before.references == rebuilt.references, before.cwd == rebuilt.cwd {} else { changed = true }
                turnCache[turn] = rebuilt
                rebuiltTurnCount += 1
            }
        }
        // Every item and turn there is now has its entry, so more entries than those are left over from items that went (a reset, another
        // snapshot): only then are the caches copied without them. Rebuilding both dictionaries with every reply cost more than the rest.
        if itemCache.count > items.count {
            let itemIDs = Set(items.map(\.id))
            itemCache = itemCache.filter { itemIDs.contains($0.key) }
        }
        if turnCache.count > order.count { turnCache = turnCache.filter { membersByTurn[$0.key] != nil } }
        // The same turns with the same files make the same index: it is not put together again.
        guard changed else { return value }
        turnOrder = order
        var result = ChatFileIndex()
        for turn in order {
            guard let cached = turnCache[turn] else { continue }
            result.entries.append(contentsOf: cached.files)
            result.byTurn[turn] = cached.files
            result.cwdByTurn[turn] = cached.cwd
            result.markdownReferences.merge(cached.references) { first, _ in first }
            for file in cached.files { result.byItem[file.itemID, default: []].append(file) }
        }
        // Plain streaming prose does not alter the file environment. Keeping its
        // identity avoids invalidating every cached Markdown/file view per token.
        if result.entries == value.entries, result.cwdByTurn == value.cwdByTurn,
           result.markdownReferences == value.markdownReferences { return value }
        value = result
        return value
    }

    /// The references of each paragraph and heading that has some, and of every one (`known` gives those read before).
    private static func references(_ source: String, known: [String: [ChatPathRef]]) -> (some: [String: [ChatPathRef]], all: [String: [ChatPathRef]]) {
        var result: [String: [ChatPathRef]] = [:]
        var all: [String: [ChatPathRef]] = [:]
        func collect(_ blocks: [ChatMarkdownBlock]) {
            for block in blocks {
                switch block {
                case .paragraph(let text), .heading(_, let text):
                    let references = all[text] ?? known[text] ?? ChatFileCatalog.references(in: text)
                    all[text] = references
                    if !references.isEmpty { result[text] = references }
                case .quote(let blocks): collect(blocks)
                case .list(_, _, let items): for item in items { collect(item.blocks) }
                default: break
                }
            }
        }
        collect(ChatMarkdown.parse(source))
        return (result, all)
    }
}
