import Foundation
import ImageIO
import UniformTypeIdentifiers

// Files a person has given a chat's message and not sent yet: each is on the Mac already (sent the moment it was picked, "File upload
// extension" in docs/remote-protocol.md), and the phone shows it as a card above the composer until the message goes. Sending puts their
// paths into the message as the phone always did; nothing about it is on the wire. Kept with the chat's draft (`ChatDraft.attachments`),
// and the pictures of the images on the phone's disk (`ChatAttachmentImages`).
//
// The desktop has no way to delete one finished upload (`upload.cancel` leaves a complete one in its inbox): a card removed on the phone
// is only dropped here, and its file stays in the chat's inbox until the desktop sweeps it (old uploads, a deleted chat's).

/// One file staged for a chat's next message.
public struct StagedAttachment: Codable, Sendable, Equatable, Identifiable, Hashable {
    public enum Kind: String, Codable, Sendable { case image, file }
    /// The desktop's upload id (unique).
    public var id: String
    public var kind: Kind
    /// The name as the phone knew it.
    public var name: String
    /// Bytes sent.
    public var size: Int
    /// Where it is on the desktop: what goes into the message.
    public var path: String
    public init(id: String, kind: Kind, name: String, size: Int, path: String) {
        self.id = id; self.kind = kind; self.name = name; self.size = size; self.path = path
    }
    /// An image if its media type (else its name's extension) says it is a picture.
    public static func kind(mediaType: String?, name: String) -> Kind {
        let type = mediaType.flatMap { UTType(mimeType: $0) } ?? UTType(filenameExtension: (name as NSString).pathExtension)
        return type?.conforms(to: .image) == true ? .image : .file
    }
    /// The size as Files says it: "840 KB", "2.4 MB" (decimal units).
    public var sizeText: String { Self.sizeText(size) }
    public static func sizeText(_ bytes: Int) -> String {
        if bytes < 1_000_000 { return "\(max(1, Int((Double(bytes) / 1000).rounded()))) KB" }
        let megabytes = Double(bytes) / 1_000_000
        return megabytes < 100 ? String(format: "%.1f MB", megabytes) : "\(Int(megabytes.rounded())) MB"
    }
    /// What the name's extension says the file is, for a file card's icon.
    public var symbol: String {
        guard let type = UTType(filenameExtension: (name as NSString).pathExtension) else { return "doc" }
        if type.conforms(to: .pdf) { return "doc.richtext" }
        if type.conforms(to: .image) { return "photo" }
        if type.conforms(to: .audiovisualContent) { return "film" }
        if type.conforms(to: .archive) { return "doc.zipper" }
        if type.conforms(to: .sourceCode) || type.conforms(to: .json) || type.conforms(to: .xml) { return "chevron.left.forwardslash.chevron.right" }
        if type.conforms(to: .spreadsheet) || type.conforms(to: .commaSeparatedText) { return "tablecells" }
        if type.conforms(to: .text) { return "doc.text" }
        return "doc"
    }
}

public enum ChatAttachmentMessage {
    /// The message sent for `text` and the staged files: the text, then each file's path on its own line, as a path put into the
    /// draft always was.
    public static func compose(_ text: String, _ attachments: [StagedAttachment]) -> String {
        guard !attachments.isEmpty else { return text }
        var message = text
        if !message.isEmpty, !message.hasSuffix("\n"), !message.hasSuffix(" ") { message += "\n" }
        return message + attachments.map(\.path).joined(separator: "\n")
    }
}

/// The pictures of staged images, on the phone's disk: a small one for the card and a larger one for the full-screen preview, by
/// upload id. Made from the bytes that were sent, so they show what the Mac has.
public struct ChatAttachmentImages: Sendable {
    public let directory: URL
    /// Longest side of the card's picture, in pixels (64 points at 3×, with room).
    public static let thumbnailPixels = 256
    /// Longest side of the full-screen preview.
    public static let previewPixels = 2048

    public init(directory: URL) { self.directory = directory }
    public static var standard: ChatAttachmentImages {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first ?? FileManager.default.temporaryDirectory
        return ChatAttachmentImages(directory: base.appendingPathComponent("ChatAttachments", isDirectory: true))
    }

    public func thumbnailURL(_ id: String) -> URL { directory.appendingPathComponent(Self.safe(id) + "-thumb.jpg") }
    public func previewURL(_ id: String) -> URL { directory.appendingPathComponent(Self.safe(id) + ".jpg") }

    /// Writes both pictures of `data`; false when it is not a picture ImageIO reads (the card then shows an icon).
    @discardableResult
    public func save(_ id: String, data: Data) -> Bool {
        guard let thumbnail = Self.jpeg(data, longest: Self.thumbnailPixels), let preview = Self.jpeg(data, longest: Self.previewPixels) else { return false }
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            try thumbnail.write(to: thumbnailURL(id), options: .atomic)
            try preview.write(to: previewURL(id), options: .atomic)
            return true
        } catch { return false }
    }
    public func remove(_ id: String) {
        try? FileManager.default.removeItem(at: thumbnailURL(id))
        try? FileManager.default.removeItem(at: previewURL(id))
    }
    /// Removes the pictures of every id not in `kept` (cards sent or removed while the app was not there to tidy up).
    public func prune(keeping kept: Set<String>) {
        let names = Set(kept.map(Self.safe))
        guard let files = try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil) else { return }
        for file in files {
            let stem = file.deletingPathExtension().lastPathComponent
            let id = stem.hasSuffix("-thumb") ? String(stem.dropLast(6)) : stem
            if !names.contains(id) { try? FileManager.default.removeItem(at: file) }
        }
    }

    /// An id as a file name: the desktop's are UUIDs; anything else is kept to the characters a UUID has.
    private static func safe(_ id: String) -> String {
        let allowed = CharacterSet(charactersIn: "0123456789abcdefABCDEF-")
        let kept = String(id.unicodeScalars.filter(allowed.contains).prefix(64))
        return kept.isEmpty ? "attachment" : kept
    }
    static func jpeg(_ data: Data, longest: Int) -> Data? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil), CGImageSourceGetCount(source) > 0 else { return nil }
        let options: [CFString: Any] = [kCGImageSourceCreateThumbnailFromImageAlways: true, kCGImageSourceCreateThumbnailWithTransform: true,
                                        kCGImageSourceThumbnailMaxPixelSize: longest, kCGImageSourceShouldCacheImmediately: true]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
        let output = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(output, UTType.jpeg.identifier as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: 0.85] as CFDictionary)
        return CGImageDestinationFinalize(destination) ? output as Data : nil
    }
}
