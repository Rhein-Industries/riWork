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

/// Kept on the chat model, outside SwiftUI observation. A token changes one item, so historical items and turns retain
/// their parsed files and references. State, keyboard and disclosure renders return the same index immediately.
@MainActor final class ChatFileCatalogCache {
    private struct ItemCache {
        var item: ChatItem
        var files: [ChatFileEntry]
        var references: [String: [ChatPathRef]]
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
        let itemIDs = Set(items.map(\.id))
        itemCache = itemCache.filter { itemIDs.contains($0.key) }
        turnCache = turnCache.filter { membersByTurn[$0.key] != nil }
        var result = ChatFileIndex()
        for turn in order {
            let members = membersByTurn[turn] ?? []
            if turnCache[turn]?.members != members {
                var parsedFiles: [String: [ChatFileEntry]] = [:]
                var references: [String: [ChatPathRef]] = [:]
                var cwd: String?
                for item in members {
                    if itemCache[item.id]?.item != item {
                        let files = ChatFileCatalog.entries(item: item, images: images)
                        let source: String?
                        switch item.body {
                        case .agentMessage(let text), .userMessage(let text), .reasoning(let text): source = text
                        default: source = nil
                        }
                        itemCache[item.id] = ItemCache(item: item, files: files, references: source.map(Self.references) ?? [:])
                        rebuiltItemCount += 1
                    }
                    parsedFiles[item.id] = itemCache[item.id]?.files ?? []
                    references.merge(itemCache[item.id]?.references ?? [:]) { first, _ in first }
                    if case .command(_, let directory, _, _) = item.body, cwd == nil { cwd = directory }
                }
                turnCache[turn] = TurnCache(members: members,
                                            files: ChatFileCatalog.entries(items: members, images: images, itemEntries: parsedFiles),
                                            references: references, cwd: cwd)
                rebuiltTurnCount += 1
            }
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

    private static func references(_ source: String) -> [String: [ChatPathRef]] {
        var result: [String: [ChatPathRef]] = [:]
        func collect(_ blocks: [ChatMarkdownBlock]) {
            for block in blocks {
                switch block {
                case .paragraph(let text), .heading(_, let text):
                    let references = ChatFileCatalog.references(in: text)
                    if !references.isEmpty { result[text] = references }
                case .quote(let blocks): collect(blocks)
                case .list(_, _, let items): for item in items { collect(item.blocks) }
                default: break
                }
            }
        }
        collect(ChatMarkdown.parse(source))
        return result
    }
}
