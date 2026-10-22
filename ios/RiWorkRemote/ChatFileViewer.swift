import SwiftUI
import UIKit
import ImageIO
import UniformTypeIdentifiers
import RiWorkCore

/// A reference to bytes the relay already supplied, or to a host path whose bytes may not be on this device.
/// A transcript excerpt is kept distinct from a complete file; no host path is fetched through a new protocol here.
struct ChatFileEntry: Identifiable, Equatable, Hashable {
    var id: String
    var itemID: String
    var turnID: String?
    var path: String
    var name: String
    var attachment = false
    var line: Int? = nil
    var source: String
    var bytes: UInt64? = nil
    var imageSource: ChatImageSource? = nil
    var availableText: String? = nil
    var textStartLine = 1
    var isExcerpt = false
    var imageIndex: Int? = nil
    /// Only paths under this on-device cache may be read; transcript host paths are display metadata.
    var cacheDirectory: String = ChatAttachmentImages.standard.directory.path
    var localPreviewPaths: [String] = []

    var isImage: Bool { imageSource != nil || StagedAttachment.kind(mediaType: nil, name: name) == .image }
    var typeName: String {
        if case .data(let mime, _) = imageSource, let ext = UTType(mimeType: mime)?.preferredFilenameExtension { return ext.uppercased() }
        let ext = (name as NSString).pathExtension.lowercased()
        let names = ["rs": "Rust", "swift": "Swift", "md": "Markdown", "js": "JavaScript", "ts": "TypeScript", "tsx": "TSX", "py": "Python", "txt": "Text", "json": "JSON", "html": "HTML", "css": "CSS", "pdf": "PDF"]
        return names[ext] ?? (ext.isEmpty ? "File" : ext.uppercased())
    }
    var displayName: String { name.isEmpty ? (path as NSString).lastPathComponent : name }
    /// Attachment paths are host storage keys; only their submitted names belong in the UI.
    var displayPath: String { attachment ? displayName : path }
    var sizeText: String? { bytes.flatMap { $0 > 0 ? ByteCountFormatter.string(fromByteCount: Int64(clamping: $0), countStyle: .file) : nil } }
    func hash(into hasher: inout Hasher) { hasher.combine(id) }
}

extension EnvironmentValues {
    @Entry var chatFileEntries: [ChatFileEntry] = []
    @Entry var chatFileIndex = ChatFileIndex()
    @Entry var chatOpenFile: @MainActor (ChatFileEntry) -> Void = { _ in }
    /// The height of the chat screen to its window's bottom, whatever the keyboard does; it follows the window as it is resized or turned
    /// (Stage Manager, tiling, rotation). Set by `ChatScreen`; the default stands in elsewhere.
    @Entry var chatScreenHeight: CGFloat = 800
}

enum ChatFileCatalog {
    private static let referenceExpressions = [
        #"\]\(([^\s)]+?)(?:#L(\d+))?\)"#,
        #"(?:^|[\s`(])((?:/?[A-Za-z0-9_.@+~-]+/)*[A-Za-z0-9_.@+~-]+\.[A-Za-z0-9]{1,12}):([0-9]+)(?::[0-9]+)?"#
    ].compactMap { try? NSRegularExpression(pattern: $0) }
    private static let legacyUploadExpression = try! NSRegularExpression(pattern: #"(?:^|\s)(/[^\s]*/(?:inbox|uploads)/[^\s]+)"#)
    /// The attachment pager is scoped to a turn. User attachments lead, followed by agent references in transcript order.
    static func entries(items: [ChatItem], images: ChatAttachmentImages = .standard, itemEntries: [String: [ChatFileEntry]]? = nil) -> [ChatFileEntry] {
        var result: [ChatFileEntry] = []
        var turnOrder: [String] = []
        var turns: [String: [ChatItem]] = [:]
        for item in items {
            let turn = item.turnID ?? "legacy:\(item.id)"
            if turns[turn] == nil { turnOrder.append(turn) }
            turns[turn, default: []].append(item)
        }
        for turn in turnOrder {
            let members = turns[turn] ?? []
            let users = members.filter { if case .userMessage = $0.body { true } else { false } }
            let agents = members.filter { if case .userMessage = $0.body { false } else { true } }
            let knownPaths = members.flatMap { item -> [String] in
                var paths = item.presentation.semantics?.produced.map(\.path) ?? []
                if let target = item.presentation.semantics?.target, item.presentation.semantics?.kind == .read { paths.append(target) }
                if case .fileChange(let changes) = item.body { paths.append(contentsOf: changes.map(\.path)) }
                return paths
            }
            var turnFiles: [ChatFileEntry] = []
            for item in users + agents {
                for var file in itemEntries?[item.id] ?? entries(item: item, images: images) {
                    if !file.path.contains("/"), file.imageIndex == nil, !users.contains(where: { $0.id == item.id }) {
                        let canonical = Set(knownPaths.filter { ($0 as NSString).lastPathComponent == file.path })
                        if canonical.count == 1, let path = canonical.first {
                            file.path = path
                            file.localPreviewPaths = [ChatFileBytes.remoteImageURL(path: path, images: images).path,
                                                      ChatFileBytes.cachedFileURL(path: path, images: images).path]
                        }
                    }
                    if case .agentMessage = item.body,
                       turnFiles.contains(where: { $0.path == file.path && $0.line == file.line }) { continue }
                    turnFiles.append(file)
                }
            }
            result.append(contentsOf: turnFiles)
        }
        return result
    }

    static func entries(for artifacts: [ChatArtifactRef], in files: [ChatFileEntry]) -> [ChatFileEntry] {
        var seen = Set<String>()
        return artifacts.compactMap { artifact in
            let candidates = files.filter { $0.itemID == artifact.itemID }
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

    /// Owned snapshots carry the host's exact attachment-summary suffix. Recover only that suffix,
    /// so text the person typed that happens to name a file remains intact.
    static func messageText(_ text: String, files: [ChatFileEntry], attachments: [ChatSubmittedAttachment] = []) -> String {
        if !attachments.isEmpty {
            var typed = text
            for (index, attachment) in attachments.enumerated().reversed() {
                let summary = "[Attached: \(attachment.name) (\(attachment.bytes) bytes)]"
                if index == 0, typed == summary { return "" }
                guard typed.hasSuffix(summary) else { return text }
                typed.removeLast(summary.count)
                guard typed.last == "\n" else { return text }
                typed.removeLast()
                guard !typed.isEmpty else { return text }
            }
            return typed
        }
        // Sent messages from old hosts carry attachment paths rather than owned snapshots.
        let paths = Set(files.map(\.path))
        return text.components(separatedBy: "\n").compactMap { line -> String? in
            if paths.contains(line.trimmingCharacters(in: .whitespaces)) { return nil }
            // Legacy compose preserves a trailing space and appends the first path on that same line.
            if let path = paths.first(where: { line.hasSuffix(" " + $0) || line.hasSuffix("\t" + $0) }) {
                return String(line.dropLast(path.count)).trimmingCharacters(in: .whitespaces)
            }
            return line
        }.joined(separator: "\n").trimmingCharacters(in: .newlines)
    }

    static func references(in text: String) -> [ChatPathRef] { merged(referencePasses(in: text)) }

    /// What `references(in:)` finds, pass by pass: Codex file citations, then each expression (Markdown file links, ordinary file:line
    /// references), each pass in text order without repeats, never treating web URLs as local files. No reference spans a line break
    /// (a citation, a link and a file:line stay on their line), so the passes of a text cut after a line break are those of its two
    /// parts, one after the other (`ChatGrowingReferences`).
    static func referencePasses(in text: String) -> [[ChatPathRef]] {
        var citations: [ChatPathRef] = []
        for (_, citation) in ChatFileCitation.find(in: text) where !citation.path.hasPrefix("http:") && !citation.path.hasPrefix("https:") {
            let ref = ChatPathRef(path: citation.path, line: nil)
            if !citations.contains(ref) { citations.append(ref) }
        }
        return [citations] + referenceExpressions.map { regex in
            var refs: [ChatPathRef] = []
            for match in regex.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
                guard let pathRange = Range(match.range(at: 1), in: text) else { continue }
                var path = String(text[pathRange])
                if path.hasPrefix("http:") || path.hasPrefix("https:") || path.hasPrefix("mailto:") { continue }
                if path.hasPrefix("file://"), let url = URL(string: path) { path = url.path }
                var line = Range(match.range(at: 2), in: text).flatMap { UInt64(text[$0]) }
                if line == nil, let colon = path.lastIndex(of: ":"), let number = UInt64(path[path.index(after: colon)...]) {
                    line = number; path = String(path[..<colon])
                }
                let ref = ChatPathRef(path: path, line: line)
                if !refs.contains(ref) { refs.append(ref) }
            }
            return refs
        }
    }

    /// The references of the passes as one list, in pass order, each once.
    static func merged(_ passes: [[ChatPathRef]]) -> [ChatPathRef] {
        var refs: [ChatPathRef] = []
        for pass in passes { for ref in pass where !refs.contains(ref) { refs.append(ref) } }
        return refs
    }

    static func resolve(path: String, line: Int? = nil, files: [ChatFileEntry]) -> ChatFileEntry? {
        let matches = files.filter { $0.path == path || ($0.path as NSString).lastPathComponent == path }
        guard var match = matches.first(where: { $0.line == line }) ?? matches.first else { return nil }
        if let line { match.line = line }
        return match
    }

    /// `messageReferences` are the agent message's `references(in:)`, when the caller has them already (`ChatGrowingReferences`).
    static func entries(item: ChatItem, images: ChatAttachmentImages, messageReferences: [ChatPathRef]? = nil) -> [ChatFileEntry] {
        var files: [ChatFileEntry] = []
        let user: Bool
        if case .userMessage = item.body { user = true } else { user = false }
        let source = user ? "Your message" : sourceName(item)
        func cachedImage(path: String, id: String? = nil) -> ChatImageSource? {
            // Construct candidates only. Existence and bytes are resolved lazily off the main thread.
            let cached = id.map(images.previewURL) ?? ChatFileBytes.remoteImageURL(path: path, images: images)
            return .local(path: cached.path)
        }
        func add(path: String, name: String? = nil, line: Int? = nil, bytes: UInt64? = nil,
                 image: ChatImageSource? = nil, text: String? = nil, start: Int = 1, excerpt: Bool = false, imageIndex: Int? = nil, attachment: Bool = false) {
            guard !path.isEmpty else { return }
            if files.contains(where: { $0.path == path && $0.line == line && $0.imageIndex == imageIndex }) { return }
            let fileSource: String
            if case .fileChange = item.body { fileSource = "Edited " + path.split(separator: "/").suffix(2).joined(separator: "/") }
            else { fileSource = source }
            files.append(ChatFileEntry(id: "\(item.id)|\(imageIndex.map { "image:\($0)" } ?? path)|\(line ?? 0)", itemID: item.id,
                                       turnID: item.turnID ?? "legacy:\(item.id)", path: path, name: name ?? (path as NSString).lastPathComponent, attachment: attachment, line: line,
                                       source: attachment ? (user ? "Your message" : "Attachment") : fileSource, bytes: bytes, imageSource: image, availableText: text,
                                       textStartLine: start, isExcerpt: excerpt, imageIndex: imageIndex, cacheDirectory: images.directory.path,
                                       localPreviewPaths: [ChatFileBytes.remoteImageURL(path: path, images: images).path,
                                                           ChatFileBytes.cachedFileURL(path: path, images: images).path]))
        }
        for attachment in item.presentation.attachments {
            let isImage = attachment.kind["kind"].string == "image" || StagedAttachment.kind(mediaType: nil, name: attachment.name) == .image
            let previewPath = attachment.preview["path"].string
            let image: ChatImageSource? = isImage ? (cachedImage(path: attachment.path, id: attachment.id)
                ?? previewPath.map { .local(path: $0) } ?? .local(path: attachment.path)) : nil
            add(path: attachment.path, name: attachment.name, bytes: attachment.bytes, image: image,
                text: attachment.preview["excerpt"].string, excerpt: true, attachment: true)
            if let previewPath, let index = files.lastIndex(where: { $0.path == attachment.path }) {
                files[index].localPreviewPaths.insert(previewPath, at: 0)
            }
        }
        if case .userMessage(let text) = item.body, item.presentation.attachments.isEmpty {
            for line in text.components(separatedBy: .newlines) {
                let path = line.trimmingCharacters(in: .whitespaces)
                guard path.hasPrefix("/"), !path.contains("\t"), !files.contains(where: { $0.path == path }) else { continue }
                // An upload path is a whole line. Avoid turning ordinary prose with a slash into an attachment.
                if path.contains("/inbox/") || path.contains("/uploads/") || !(path as NSString).pathExtension.isEmpty {
                    add(path: path, image: StagedAttachment.kind(mediaType: nil, name: path) == .image ? (cachedImage(path: path) ?? .local(path: path)) : nil, attachment: true)
                }
            }
            do {
                for match in legacyUploadExpression.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
                    guard let range = Range(match.range(at: 1), in: text) else { continue }
                    let path = String(text[range])
                    if !files.contains(where: { $0.path == path }) {
                        add(path: path, image: StagedAttachment.kind(mediaType: nil, name: path) == .image ? (cachedImage(path: path) ?? .local(path: path)) : nil, attachment: true)
                    }
                }
            }
            files.sort { (text.range(of: $0.path)?.lowerBound ?? text.endIndex) < (text.range(of: $1.path)?.lowerBound ?? text.endIndex) }
        }
        if !user, let semantics = item.presentation.semantics, let path = semantics.target,
           semantics.kind == .read, !path.hasPrefix("http") {
            let output: String?
            if case .toolCall(_, _, _, let body) = item.body { output = body }
            else if case .command(_, _, let body, _) = item.body { output = body }
            else { output = nil }
            add(path: path, line: semantics.range.map { Int(clamping: $0.start) }, text: output.flatMap { $0.isEmpty ? nil : $0 } ?? semantics.excerpt,
                start: semantics.range.map { Int(clamping: $0.start) } ?? 1, excerpt: true)
        }
        if case .fileChange(let changes) = item.body {
            for change in changes { add(path: change.path) }
        }
        for ref in item.presentation.semantics?.produced ?? [] { add(path: ref.path, line: ref.line.map(Int.init(clamping:))) }
        for (index, image) in item.presentation.images.enumerated() {
            let path: String
            switch image.source {
            case .local(let value): path = value
            case .url(let value): path = value
            case .data(let mime, _):
                path = imageTarget(item) ?? imageLabel(image.label) ?? "Image \(index + 1).\(UTType(mimeType: mime)?.preferredFilenameExtension ?? "png")"
            case .unavailable:
                path = imageTarget(item) ?? imageLabel(image.label) ?? "Image \(index + 1)"
            }
            // New host echoes carry both the owned attachment snapshot and its image presentation.
            // They describe the same file and must occupy only one thumbnail and pager position.
            let attachmentPath = item.presentation.attachments.first { attachment in
                attachment.path == path || attachment.preview["path"].string == path
            }?.path
            if let existing = files.firstIndex(where: { $0.path == (attachmentPath ?? path) && $0.isImage }) {
                files[existing].imageIndex = index
                if case .data = image.source { files[existing].imageSource = image.source }
                else if files[existing].imageSource == nil { files[existing].imageSource = image.source }
                continue
            }
            let name = user ? imageLabel(image.label).map { ($0 as NSString).lastPathComponent } ?? (path as NSString).lastPathComponent : (path as NSString).lastPathComponent
            add(path: path, name: name, image: image.source, imageIndex: index, attachment: user)
        }
        if case .agentMessage(let text) = item.body {
            for ref in messageReferences ?? references(in: text) { add(path: ref.path, line: ref.line.map(Int.init(clamping:))) }
        }
        if item.presentation.semantics?.kind == .search || item.presentation.semantics?.kind == .list {
            let output: String?
            if case .toolCall(_, _, _, let body) = item.body { output = body }
            else if case .command(_, _, let body, _) = item.body { output = body }
            else { output = nil }
            for ref in references(in: output ?? "") { add(path: ref.path, line: ref.line.map(Int.init(clamping:))) }
            // Search output also names extensionless files such as Makefile and .gitignore.
            for line in (output ?? "").split(separator: "\n") {
                let parts = line.split(separator: ":", maxSplits: 2, omittingEmptySubsequences: false)
                guard parts.count >= 2, !parts[0].isEmpty, let number = Int(parts[1]), number > 0 else { continue }
                add(path: String(parts[0]), line: number)
            }
        }
        return files
    }

    /// Tool result images often have a generic label and no path. Their call's real file target
    /// is display metadata; it does not grant the phone permission to read that host path.
    private static func imageTarget(_ item: ChatItem) -> String? {
        guard item.presentation.images.count == 1 else { return nil }
        var candidates = [item.presentation.semantics?.target]
        if case .toolCall(_, _, let input, _) = item.body {
            candidates += ["file_path", "path", "filename", "file"].map { input[$0].string }
        }
        return candidates.compactMap { $0 }.first {
            StagedAttachment.kind(mediaType: nil, name: $0) == .image && imageLabel($0) != nil
        }
    }

    private static func imageLabel(_ value: String) -> String? {
        let label = value.trimmingCharacters(in: .whitespacesAndNewlines)
        let lower = label.lowercased()
        guard !label.isEmpty, !["image", "[image]"].contains(lower),
              !lower.hasPrefix("image:"), !lower.contains("toolu_"), !lower.contains("tool_use") else { return nil }
        return label
    }

    private static func sourceName(_ item: ChatItem) -> String {
        let semantics = item.presentation.semantics
        func target(_ verb: String, _ path: String?) -> String {
            guard let path, imageLabel(path) != nil else { return verb }
            let short = path.split(separator: "/").suffix(2).joined(separator: "/")
            return "\(verb) \(short)"
        }
        switch item.body {
        case .agentMessage: return "Answer"
        case .fileChange(let changes): return target("Edited", changes.first?.path)
        case .command(let command, _, _, _): return "Ran \(command)"
        case .toolCall(let server, let tool, _, _):
            if semantics?.kind == .read { return target("Read", semantics?.target) }
            return server.map { "\($0) · \(tool)" } ?? tool
        default: return "Agent output"
        }
    }
}

/// The neutral chip used for produced files, sent files, and source locations.
struct ChatFileChip: View {
    @Environment(\.chatOpenFile) private var openFile
    let file: ChatFileEntry
    var body: some View {
        Button { openFile(file) } label: {
            HStack(spacing: 6) {
                // The row is as tall as its tallest glyph, and "doc" stands taller than "photo": every chip reserves
                // the "doc" glyph's box, so image chips draw at the file chips' height.
                ZStack {
                    Image(systemName: "doc").hidden()
                    Image(systemName: file.isImage ? "photo" : "doc").foregroundStyle(.secondary)
                }
                Text(file.displayName + (file.line.map { ":\($0)" } ?? "")).lineLimit(1).truncationMode(.middle)
                if let size = file.sizeText { Text(size).foregroundStyle(.secondary).lineLimit(1) }
            }
            .font(file.line == nil ? .system(size: 12) : .system(size: 12, design: .monospaced))
            .foregroundStyle(file.line == nil ? Color.primary : Color(uiColor: .link))
            .padding(.horizontal, 9).padding(.vertical, 4)
            .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 7))
            .overlay(RoundedRectangle(cornerRadius: 7).stroke(Color(uiColor: .separator).opacity(0.35), lineWidth: 0.5))
            .frame(minHeight: 36).contentShape(Rectangle())
        }
        .buttonStyle(.plain).accessibilityLabel("Open \(file.displayName)" + (file.line.map { ", line \($0)" } ?? ""))
        .accessibilityIdentifier("chat-file-chip")
        .chatLayoutProbe("file-chip-\(file.id)")
        .chatLayoutProbe("file-\(file.id)", action: { openFile(file) })
    }
}

struct ChatFileStrip: View {
    let files: [ChatFileEntry]
    var body: some View {
        ChatFileFlowLayout(spacing: 4) { ForEach(files) { ChatFileChip(file: $0) } }
            .accessibilityElement(children: .contain).accessibilityLabel("Files and images")
    }
}

/// Sent files use one catalog and loader: a three-column image grid followed by file chips.
struct ChatSentAttachments: View {
    @Environment(\.chatOpenFile) private var openFile
    let files: [ChatFileEntry]
    private var images: [ChatFileEntry] { files.filter(\.isImage) }
    private var documents: [ChatFileEntry] { files.filter { !$0.isImage } }
    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text("\(images.count) \(images.count == 1 ? "image" : "images"), \(documents.count) \(documents.count == 1 ? "file" : "files")")
                .font(.system(size: 11)).foregroundStyle(.secondary)
            if !images.isEmpty {
                LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 5), count: 3), spacing: 5) {
                    ForEach(images) { file in
                        Button { openFile(file) } label: {
                            ChatFileThumbnail(file: file, showsFileFallback: true).frame(height: 70).clipped()
                                .clipShape(RoundedRectangle(cornerRadius: 9)).contentShape(Rectangle())
                        }.buttonStyle(.plain).accessibilityLabel("Open image, \(file.displayName)")
                            .chatLayoutProbe("file-\(file.id)", action: { openFile(file) })
                    }
                }.frame(maxWidth: 310, alignment: .leading)
            }
            if !documents.isEmpty { ChatFileStrip(files: documents) }
        }
        .accessibilityIdentifier("chat-sent-attachments")
    }
}

struct ChatFileThumbnail: View {
    let file: ChatFileEntry
    var showsFileFallback = false
    @State private var image: UIImage?
    @State private var loading = true
    var body: some View {
        GeometryReader { geometry in
            ZStack {
                Color(uiColor: .tertiarySystemFill)
                if let image {
                    Image(uiImage: image).resizable().scaledToFill().frame(width: geometry.size.width, height: geometry.size.height).clipped()
                } else if showsFileFallback, !loading {
                    VStack(spacing: 3) {
                        Text(file.displayName).lineLimit(1).truncationMode(.middle)
                        Text("Image unavailable").font(.system(size: 10)).foregroundStyle(.secondary)
                    }
                    .font(.system(size: 11)).padding(.horizontal, 5)
                } else { Image(systemName: "photo").foregroundStyle(.secondary) }
            }
        }
        .task(id: file) {
            image = nil
            loading = true
            let decoded = await ChatFileBytes.image(for: file)
            guard !Task.isCancelled else { return }
            image = decoded
            loading = false
        }
    }
}

/// Opened row images keep their real aspect ratio and contain within half of the chat screen's height (its window's, as it is resized).
struct ChatInlineImage: View {
    @Environment(\.chatOpenFile) private var openFile
    @Environment(\.desktopStyle) private var style
    @Environment(\.chatScreenHeight) private var screenHeight
    let file: ChatFileEntry
    @State private var image: UIImage?
    @State private var loading = true
    @State private var copied = false
    @State private var copyResetTask: Task<Void, Never>?

    var body: some View {
        Button { openFile(file) } label: {
            imageContent
                .background(style.panel)
                .clipShape(RoundedRectangle(cornerRadius: 4))
                .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(style.divider, lineWidth: 0.5))
                .contentShape(RoundedRectangle(cornerRadius: 4))
        }
        .buttonStyle(.plain).accessibilityLabel("Open image, \(file.displayName)")
        .overlay(alignment: .topTrailing) {
            HStack(spacing: 2) {
                ChatContainerIconAction(symbol: copied ? "checkmark" : "doc.on.doc", label: "Copy image", translucent: true, action: copyImage)
                    .disabled(image == nil)
                    .chatLayoutProbe("image-copy-\(file.id)", revision: String(copied), action: copyImage)
                ChatContainerIconAction(symbol: "arrow.up.right.square", label: "Open in viewer", translucent: true) { openFile(file) }
                    .chatLayoutProbe("image-open-\(file.id)", action: { openFile(file) })
            }
            .padding(4)
        }
        .accessibilityIdentifier("chat-inline-image")
        .chatLayoutProbe("inline-image-\(file.id)", revision: String(image != nil))
        .chatLayoutProbe("file-\(file.id)", action: { openFile(file) })
        .chatLayoutProbe("image-state-\(file.id)", revision: loading ? "loading" : image == nil ? "error" : "loaded")
        .frame(maxWidth: .infinity, alignment: .leading)
        .task(id: file) {
            image = nil
            loading = true
            copied = false
            copyResetTask?.cancel()
            let loadedImage = await ChatFileBytes.image(for: file)
            guard !Task.isCancelled else { return }
            image = loadedImage
            loading = false
        }
        .onDisappear { copyResetTask?.cancel() }
    }

    @ViewBuilder private var imageContent: some View {
        if let image {
            ChatInlineImageLayout(naturalSize: CGSize(width: image.size.width * image.scale, height: image.size.height * image.scale), maximumHeight: screenHeight * 0.5) {
                Image(uiImage: image).resizable().aspectRatio(contentMode: .fit)
            }
        } else {
            HStack(spacing: 6) {
                if loading { Image(systemName: "photo") }
                Text(loading ? "Loading image…" : imageError).lineLimit(2)
            }
            .font(style.system(.caption)).foregroundStyle(style.muted)
            .frame(maxWidth: .infinity).frame(height: 72)
        }
    }

    private var imageError: String {
        if case .unavailable(let reason) = file.imageSource, !reason.isEmpty { return reason }
        return "Image unavailable"
    }

    private func copyImage() {
        guard let image else { return }
        UIPasteboard.general.image = image
        copyResetTask?.cancel()
        copied = true
        copyResetTask = Task { @MainActor in
            do {
                try await Task.sleep(for: .seconds(1.5))
                copied = false
            } catch { }
        }
    }
}

/// A phone screenshot keeps its own narrow frame; a wide image fits the available transcript column.
private struct ChatInlineImageLayout: Layout {
    let naturalSize: CGSize
    let maximumHeight: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard naturalSize.width > 0, naturalSize.height > 0 else { return .zero }
        let width = max(0, proposal.width ?? naturalSize.width)
        let scale = min(1, width / naturalSize.width, max(0, maximumHeight) / naturalSize.height)
        return CGSize(width: naturalSize.width * scale, height: naturalSize.height * scale)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        for subview in subviews {
            subview.place(at: bounds.origin, proposal: ProposedViewSize(bounds.size))
        }
    }
}

/// Compact chips wrap as a row, preserving whole filenames instead of stacking individual words.
private struct ChatFileFlowLayout: Layout {
    var spacing: CGFloat
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        arrange(width: proposal.width ?? 320, subviews: subviews).size
    }
    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let layout = arrange(width: bounds.width, subviews: subviews)
        for (index, point) in layout.positions.enumerated() {
            subviews[index].place(at: CGPoint(x: bounds.minX + point.x, y: bounds.minY + point.y), proposal: ProposedViewSize(width: min(bounds.width, subviews[index].sizeThatFits(.unspecified).width), height: nil))
        }
    }
    private func arrange(width: CGFloat, subviews: Subviews) -> (size: CGSize, positions: [CGPoint]) {
        var x: CGFloat = 0, y: CGFloat = 0, rowHeight: CGFloat = 0
        var positions: [CGPoint] = []
        for view in subviews {
            let ideal = view.sizeThatFits(ProposedViewSize(width: width, height: nil))
            let size = CGSize(width: min(width, ideal.width), height: ideal.height)
            if x > 0, x + size.width > width { x = 0; y += rowHeight + spacing; rowHeight = 0 }
            positions.append(CGPoint(x: x, y: y)); x += size.width + spacing; rowHeight = max(rowHeight, size.height)
        }
        return (CGSize(width: width, height: y + rowHeight), positions)
    }
}

@MainActor enum ChatFileBytes {
    static let images: NSCache<NSString, UIImage> = {
        let cache = NSCache<NSString, UIImage>()
        cache.totalCostLimit = 64 * 1024 * 1024
        cache.countLimit = 128
        return cache
    }()
    private static var pendingImages: [NSString: Task<UIImage?, Never>] = [:]
    nonisolated static func decode(_ data: Data, maxPixelSize: Int? = 2048) -> UIImage? {
        // ImageIO decodes and applies orientation here, before UIKit displays the bitmap.
        assert(!Thread.isMainThread)
        return Perf.interval("ChatImageDecode") {
            guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
            // Preview keeps source resolution for pixel-accurate zoom. Only thumbnails are capped.
            let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
            let originalSize = max(properties?[kCGImagePropertyPixelWidth] as? Int ?? 0,
                                   properties?[kCGImagePropertyPixelHeight] as? Int ?? 0)
            let limit = maxPixelSize ?? (originalSize > 0 ? originalSize : 65_536)
            guard let cg = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                    kCGImageSourceCreateThumbnailFromImageAlways: true,
                    kCGImageSourceCreateThumbnailWithTransform: true,
                    kCGImageSourceShouldCacheImmediately: true,
                    kCGImageSourceThumbnailMaxPixelSize: limit
                  ] as CFDictionary) else { return nil }
            return UIImage(cgImage: cg)
        }
    }
    private nonisolated static func pathHash(_ path: String) -> String {
        String(path.utf8.reduce(UInt64(14_695_981_039_346_656_037)) { ($0 ^ UInt64($1)) &* 1_099_511_628_211 }, radix: 16)
    }
    /// Matches the existing sent-image alias, without doing a filesystem stat during layout.
    nonisolated static func remoteImageURL(path: String, images: ChatAttachmentImages = .standard) -> URL {
        images.directory.appendingPathComponent("remote-" + pathHash(path) + ".jpg")
    }
    /// On-device full bytes supplied by existing preview/fixture machinery, separate from host display paths.
    nonisolated static func cachedFileURL(path: String, images: ChatAttachmentImages = .standard) -> URL {
        images.directory.appendingPathComponent("file-" + pathHash(path) + "-" + (path as NSString).lastPathComponent)
    }
    static func image(for file: ChatFileEntry) async -> UIImage? {
        let sourceKey: String
        switch file.imageSource {
        case .data(_, let base64): sourceKey = "data:\(base64.hashValue)"
        case .local(let path), .url(let path): sourceKey = path
        case .unavailable(let reason): sourceKey = reason
        case nil: sourceKey = file.path
        }
        // The same produced image may be referenced by many calls/turns.
        let cacheKey = "\(file.cacheDirectory)|\(sourceKey)" as NSString
        if let known = images.object(forKey: cacheKey) { return known }
        if let pending = pendingImages[cacheKey] { return await pending.value }
        let task = Task { () -> UIImage? in
            guard let data = await data(for: file) else { return nil }
            return await Task.detached(priority: .userInitiated) { decode(data) }.value
        }
        pendingImages[cacheKey] = task
        let image = await task.value
        pendingImages[cacheKey] = nil
        if let image { images.setObject(image, forKey: cacheKey, cost: Int(image.size.width * image.size.height) * 4) }
        return image
    }
    static func data(for file: ChatFileEntry) async -> Data? {
        switch file.imageSource {
        case .data(_, let base64): return await Task.detached(priority: .userInitiated) { Data(base64Encoded: base64, options: .ignoreUnknownCharacters) }.value
        case .url(let value):
            guard let url = URL(string: value), ["https", "http"].contains(url.scheme?.lowercased() ?? "") else { return nil }
            guard let (data, response) = try? await URLSession.shared.data(from: url),
                  (response as? HTTPURLResponse).map({ (200..<300).contains($0.statusCode) }) ?? true else { return nil }
            return data
        case .unavailable: return nil
        case .local(let path): return await localData([path] + file.localPreviewPaths, root: file.cacheDirectory)
        case nil: return await localData(file.localPreviewPaths + [file.path], root: file.cacheDirectory)
        }
    }
    private static func localData(_ paths: [String], root: String) async -> Data? {
        await Task.detached(priority: .userInitiated) {
            let rootURL = URL(fileURLWithPath: root, isDirectory: true).resolvingSymlinksInPath().standardizedFileURL
            let prefix = rootURL.path + "/"
            for path in paths {
                let url = path.hasPrefix("file://") ? URL(string: path) : URL(fileURLWithPath: path)
                guard let url, url.isFileURL else { continue }
                let local = url.resolvingSymlinksInPath().standardizedFileURL
                guard local.path.hasPrefix(prefix) else { continue }
                if let data = try? Data(contentsOf: local, options: .mappedIfSafe) { return data }
            }
            return nil
        }.value
    }
}

/// Pushed in the chat's navigation stack, with the same compact toolbar surface as the chat.
struct ChatFileViewer: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.desktopStyle) private var style
    let files: [ChatFileEntry]
    private let decodePreview: @MainActor (Data) async -> UIImage?
    @State private var selectedID: String
    @State private var image: UIImage?
    @State private var text: String?
    @State private var loadedBytes: Int?
    @State private var shareURL: URL?
    @State private var loading = true
    @State private var zoom: CGFloat = 1
    @State private var settledZoom: CGFloat = 1
    @State private var fit = true
    @State private var imageFitScale: CGFloat = 1
    @State private var status = ""
    @State private var openDocument: UIDocumentInteractionController?

    init(files: [ChatFileEntry], initialID: String, initialLine: Int? = nil,
         decodePreview: @escaping @MainActor (Data) async -> UIImage? = { data in
             await Task.detached(priority: .userInitiated) { ChatFileBytes.decode(data, maxPixelSize: nil) }.value
         }) {
        self.decodePreview = decodePreview
        self.files = files.map { file in
            guard file.id == initialID, let initialLine else { return file }
            var selected = file; selected.line = initialLine; return selected
        }
        _selectedID = State(initialValue: files.contains { $0.id == initialID } ? initialID : files.first?.id ?? "")
    }
    private var index: Int { files.firstIndex { $0.id == selectedID } ?? 0 }
    private var file: ChatFileEntry? { files.first { $0.id == selectedID } ?? files.first }
    private var zoomLabel: String { "\(fit ? "Fit · " : "")\(Int(((image == nil ? zoom : imageFitScale * zoom) * 100).rounded()))%" }

    var body: some View {
        Group {
            if let file {
                VStack(spacing: 0) {
                    previewToolbar(file)
                    Text(info(file)).font(.system(size: 11)).foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 16).padding(.top, 3).padding(.bottom, 8)
                        .background(style.background).overlay(alignment: .bottom) { if !style.glass { DesktopRule() } }
                        .accessibilityIdentifier("chat-file-info")
                        .chatLayoutProbe("file-info")
                    viewerContent(file).frame(maxWidth: .infinity, maxHeight: .infinity)
                }
                .background(Color(uiColor: .systemBackground))
                .safeAreaInset(edge: .bottom, spacing: 0) { bottomToolbar }
                .navigationTitle(file.displayName).navigationBarTitleDisplayMode(.inline)
                .navigationBarBackButtonHidden()
                .toolbar(.hidden, for: .navigationBar)
                .task(id: file) { await load(file) }
                .accessibilityIdentifier("chat-file-viewer")
            } else { ContentUnavailableView("No file", systemImage: "doc") }
        }
        .onChange(of: files) { before, after in
            guard !after.contains(where: { $0.id == selectedID }) else { return }
            let previous = before.firstIndex { $0.id == selectedID } ?? 0
            selectedID = after.isEmpty ? "" : after[min(previous, after.count - 1)].id
        }
    }

    private func previewToolbar(_ file: ChatFileEntry) -> some View {
        HStack(spacing: 2) {
            Button { dismiss() } label: { Image(systemName: "chevron.left") }
                .accessibilityLabel("Back to Chat").accessibilityIdentifier("chat-file-back")
                .chatLayoutProbe("file-back", action: { dismiss() })
            Image(systemName: file.isImage ? "photo" : "doc.text").foregroundStyle(style.muted).frame(width: 16)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text(file.displayName).font(style.system(.footnote, weight: .semibold)).foregroundStyle(style.text)
                if file.displayPath != file.displayName {
                    Text(file.displayPath).font(style.system(.caption2)).foregroundStyle(style.muted)
                }
            }
            .lineLimit(1).truncationMode(.middle).frame(maxWidth: .infinity, alignment: .leading)
            if let shareURL {
                ShareLink(item: shareURL) { Image(systemName: "square.and.arrow.up") }.accessibilityLabel("Share file")
                    .chatLayoutProbe("file-share")
            } else {
                Button {} label: { Image(systemName: "square.and.arrow.up") }.disabled(true).accessibilityLabel("Share file unavailable")
            }
            Menu {
                Button("Zoom in", systemImage: "plus.magnifyingglass") { changeZoom(1.2) }
                Button("Zoom out", systemImage: "minus.magnifyingglass") { changeZoom(1 / 1.2) }
                Button("Fit", systemImage: "arrow.down.right.and.arrow.up.left") { resetZoom() }
                Text("Pinch · Double tap fit/100%")
            } label: { Image(systemName: "plus.magnifyingglass") }.accessibilityLabel("Zoom")
                .chatLayoutProbe("file-zoom")
        }
        .font(style.system(.footnote)).foregroundStyle(style.text)
        .buttonStyle(TargetButtonStyle())
        .chatToolbarSurface()
        .accessibilityIdentifier("chat-preview-toolbar")
        .chatLayoutProbe("file-toolbar")
    }

    @ViewBuilder private func viewerContent(_ file: ChatFileEntry) -> some View {
        if loading { ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity) }
        else if let image {
            GeometryReader { geometry in
                ScrollView([.horizontal, .vertical]) {
                    let scale = min(1, geometry.size.width / image.size.width, geometry.size.height / image.size.height)
                    Image(uiImage: image).resizable().interpolation(.high)
                        .frame(width: image.size.width * scale * zoom, height: image.size.height * scale * zoom)
                        .frame(minWidth: geometry.size.width, minHeight: geometry.size.height)
                        .contentShape(Rectangle())
                        .onAppear { imageFitScale = scale }
                        .onChange(of: geometry.size) { _, size in imageFitScale = min(1, size.width / image.size.width, size.height / image.size.height) }
                }
                .background(Color(uiColor: .secondarySystemBackground))
                .simultaneousGesture(zoomGesture)
                .simultaneousGesture(pageGesture)
                .onTapGesture(count: 2, perform: toggleZoom)
            }.chatLayoutProbe("file-preview-image")
        } else if let text {
            ChatCodeFileView(text: text, firstLine: loadedBytes == nil ? file.textStartLine : 1, highlightedLine: file.line, zoom: zoom, fit: fit)
                .simultaneousGesture(zoomGesture).simultaneousGesture(pageGesture)
                .highPriorityGesture(TapGesture(count: 2).onEnded(toggleZoom))
                .chatLayoutProbe("file-preview-text")
        } else {
            VStack(spacing: 10) {
                Image(systemName: file.isImage ? "photo" : "doc").font(.system(size: 32)).foregroundStyle(.tertiary)
                Text(loadedBytes == nil ? "Not available on this device" : "Preview not available").font(.body).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity).contentShape(Rectangle()).gesture(pageGesture)
        }
    }
    private var bottomToolbar: some View {
        HStack(spacing: 4) {
            if files.count > 1 {
                Button { page(-1) } label: { Image(systemName: "chevron.left").frame(width: 44, height: 44) }
                    .disabled(index == 0).accessibilityLabel("Previous file")
                    .chatLayoutProbe("file-previous", action: { page(-1) })
            }
            HStack(spacing: 10) {
                if files.count > 1 { Text("\(index + 1)/\(files.count)").accessibilityLabel("File \(index + 1) of \(files.count)") }
                Text(zoomLabel)
            }.font(.system(size: 12)).monospacedDigit().foregroundStyle(Color(uiColor: .secondaryLabel)).frame(maxWidth: .infinity)
            if files.count > 1 {
                Button { page(1) } label: { Image(systemName: "chevron.right").frame(width: 44, height: 44) }
                    .disabled(index >= files.count - 1).accessibilityLabel("Next file")
                    .chatLayoutProbe("file-next", action: { page(1) })
            }
            Menu {
                if let shareURL {
                    Button("Open in…", systemImage: "arrow.up.forward.app") { openIn(shareURL) }
                    ShareLink(item: shareURL) { Label("Share", systemImage: "square.and.arrow.up") }
                }
                Button(file?.attachment == true ? "Copy filename" : "Copy path", systemImage: "doc.on.doc") {
                    UIPasteboard.general.string = file?.displayPath
                    status = file?.attachment == true ? "Filename copied" : "Path copied"
                }
                Button("Fit", systemImage: "arrow.down.right.and.arrow.up.left", action: resetZoom)
                Button("Close preview", systemImage: "xmark") { dismiss() }
            } label: { Image(systemName: "ellipsis").frame(width: 44, height: 44) }.accessibilityLabel("More file actions")
        }
        .buttonStyle(.plain).foregroundStyle(Color(uiColor: .link))
        .padding(.horizontal, 5).padding(.vertical, 3).modifier(ChatViewerGlass())
        .padding(.horizontal, 10).padding(.top, 8).padding(.bottom, 6)
        .overlay(alignment: .top) { if !status.isEmpty { Text(status).font(.caption2).foregroundStyle(.secondary).offset(y: -12) } }
        .accessibilityIdentifier("chat-file-toolbar")
    }
    private var zoomGesture: some Gesture {
        MagnifyGesture().onChanged { value in
            zoom = min(10, max(0.5, settledZoom * value.magnification)); fit = false
        }.onEnded { _ in settledZoom = zoom }
    }
    private var pageGesture: some Gesture {
        DragGesture(minimumDistance: 35).onEnded { value in
            guard fit, abs(value.translation.width) > 50, abs(value.translation.width) > abs(value.translation.height) * 1.4 else { return }
            page(value.translation.width < 0 ? 1 : -1)
        }
    }
    private func info(_ file: ChatFileEntry) -> String {
        var parts = [file.typeName]
        if let bytes = file.bytes ?? loadedBytes.map(UInt64.init), bytes > 0 { parts.append(ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file)) }
        if let image { parts.append("\(Int(image.size.width * image.scale)) × \(Int(image.size.height * image.scale)) px") }
        else if let text { let count = ChatOutput.lineCount(text); parts.append("\(count) \(count == 1 ? "line" : "lines")\(file.isExcerpt && shareURL == nil ? " available" : "")") }
        if files.count > 1 { parts.append("\(index + 1)/\(files.count)") }
        parts.append("source: \(file.source)")
        return parts.joined(separator: " · ")
    }
    private func load(_ file: ChatFileEntry) async {
        loading = true; image = nil; text = nil; loadedBytes = nil; shareURL = nil; status = ""; resetZoom()
        let data = await ChatFileBytes.data(for: file)
        guard !Task.isCancelled, self.file?.id == file.id else { return }
        loadedBytes = data?.count
        if file.isImage, let data {
            let decoded = await decodePreview(data)
            guard !Task.isCancelled, self.file?.id == file.id else { return }
            image = decoded
        }
        else { text = data.flatMap { String(data: $0, encoding: .utf8) } ?? file.availableText }
        if let data {
            let url = await Task.detached(priority: .userInitiated) { () -> URL? in
                let folder = FileManager.default.temporaryDirectory.appendingPathComponent("ChatFileShares", isDirectory: true).appendingPathComponent(UUID().uuidString, isDirectory: true)
                let url = folder.appendingPathComponent(file.displayName.isEmpty ? "File" : (file.displayName as NSString).lastPathComponent)
                do {
                    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
                    try data.write(to: url, options: .atomic)
                    return url
                } catch { return nil }
            }.value
            guard !Task.isCancelled, self.file?.id == file.id else { return }
            shareURL = url
        }
        loading = false
    }
    private func page(_ delta: Int) {
        let next = index + delta
        guard files.indices.contains(next) else { return }
        selectedID = files[next].id
    }
    private func openIn(_ url: URL) {
        guard let view = UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene })
            .flatMap(\.windows).first(where: \.isKeyWindow)?.rootViewController?.view else { return }
        let controller = UIDocumentInteractionController(url: url)
        openDocument = controller
        if !controller.presentOpenInMenu(from: CGRect(x: view.bounds.midX, y: view.bounds.maxY - 44, width: 1, height: 1), in: view, animated: true) {
            status = "No app available to open this file"
            openDocument = nil
        }
    }
    private func resetZoom() { zoom = 1; settledZoom = 1; fit = true }
    private func changeZoom(_ factor: CGFloat) { zoom = min(10, max(0.5, zoom * factor)); settledZoom = zoom; fit = false }
    private func toggleZoom() {
        if fit { zoom = image == nil ? 1 : min(10, 1 / max(0.05, imageFitScale)); settledZoom = zoom; fit = false }
        else { resetZoom() }
    }
}

private struct ChatViewerGlass: ViewModifier {
    func body(content: Content) -> some View {
        if #available(iOS 26, *) { content.glassEffect(.regular, in: .capsule) }
        else { content.background(.regularMaterial, in: Capsule()).overlay(Capsule().stroke(.white.opacity(0.3), lineWidth: 0.5)) }
    }
}

private struct ChatCodeFileView: View {
    let text: String
    let firstLine: Int
    let highlightedLine: Int?
    let zoom: CGFloat
    let fit: Bool
    private var lines: [Substring] { text.split(separator: "\n", omittingEmptySubsequences: false) }
    private var highlight: Color { Color(uiColor: UIColor { $0.userInterfaceStyle == .dark ? UIColor(red: 0.30, green: 0.26, blue: 0.10, alpha: 1) : UIColor(red: 1, green: 0.95, blue: 0.80, alpha: 1) }) }
    var body: some View {
        ScrollViewReader { proxy in
            ScrollView(fit ? [.vertical] : [.horizontal, .vertical]) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(lines.indices, id: \.self) { index in
                        let number = firstLine + index
                        HStack(alignment: .top, spacing: 0) {
                            Text("\(number)").foregroundStyle(.tertiary).monospacedDigit().frame(width: 30 * zoom, alignment: .trailing)
                                .padding(.trailing, 6 * zoom)
                                .accessibilityHidden(true)
                            Text(verbatim: lines[index].isEmpty ? " " : String(lines[index])).foregroundStyle(.primary)
                                .textSelection(.enabled).padding(.horizontal, 6 * zoom)
                                .frame(maxWidth: .infinity, alignment: .leading).fixedSize(horizontal: !fit, vertical: true)
                        }
                        .font(.system(size: 11 * zoom, design: .monospaced)).lineSpacing(3 * zoom)
                        .frame(minHeight: 20 * zoom, alignment: .top).frame(maxWidth: .infinity, alignment: .leading)
                        .background(alignment: .leading) {
                            (number == highlightedLine ? highlight : Color(uiColor: .secondarySystemBackground)).frame(width: 36 * zoom)
                                .overlay(alignment: .trailing) { Rectangle().fill(Color(uiColor: .separator).opacity(0.4)).frame(width: 0.5) }
                        }
                        .background(number == highlightedLine ? highlight : .clear).id(number)
                    }
                }.padding(.vertical, 8)
            }
            .background(Color(uiColor: .systemBackground))
            .onAppear { if let line = highlightedLine { proxy.scrollTo(max(firstLine, line - 8), anchor: .top) } }
        }
    }
}
