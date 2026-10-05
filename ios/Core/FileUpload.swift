import Foundation
import CryptoKit
import ImageIO
import UniformTypeIdentifiers

// Files from the phone ("File upload extension" in docs/remote-protocol.md): a photo or a file goes to the desktop in chunks over the
// encrypted link (`upload.begin`, `upload.chunk`, `upload.finish`), lands in the inbox of a shell or a chat, and for a shell is then
// pasted into it as a drop of that file on its terminal would be (`shell.paste`, exactly once per batch). The desktop makes the file's
// name and holds the limits; the phone sends what fits them and says clearly what does not.

/// What `ready.features.upload` says. A desktop without it cannot take files.
public struct UploadFeature: Sendable, Equatable {
    /// The largest file.
    public var maximumBytes: Int
    /// The most data one `upload.chunk` may carry.
    public var chunkBytes: Int
    /// What this phone may keep on the desktop at once.
    public var quotaBytes: Int
    /// Files one paste may hold.
    public var maximumFiles: Int
    public init(maximumBytes: Int, chunkBytes: Int, quotaBytes: Int, maximumFiles: Int) {
        self.maximumBytes = maximumBytes; self.chunkBytes = chunkBytes; self.quotaBytes = quotaBytes; self.maximumFiles = maximumFiles
    }
    /// From `features.upload`; nil when it is missing or cannot be read.
    public init?(_ value: JSONValue) {
        func count(_ key: String) -> Int? {
            guard case .number(let number) = value[key], number.isFinite, number >= 1, number <= 1e12, number.rounded() == number else { return nil }
            return Int(number)
        }
        guard let maximum = count("max_bytes"), let chunk = count("chunk_bytes") else { return nil }
        maximumBytes = maximum
        // Never more than a request frame holds, whatever is announced.
        chunkBytes = min(chunk, UploadLimits.chunkBytes)
        quotaBytes = count("quota_bytes") ?? maximum
        maximumFiles = min(count("max_files") ?? 1, UploadLimits.maximumFiles)
    }
}

public enum UploadLimits {
    /// The most data this phone puts in one chunk: base64 of it and the rest of the request fit one 128 KiB frame.
    public static let chunkBytes = 92_160
    /// Files one paste may hold, at most.
    public static let maximumFiles = 16
    public static let nameMaximumBytes = 255
    /// The longest side of a photo that is re-encoded (a larger one is scaled down; the agents scale further themselves).
    public static let photoMaximumPixels = 4096
}

/// Where a file goes: the inbox of a shell (and then into the shell) or of a chat (and then into its message).
public enum UploadTarget: Sendable, Equatable {
    case shell(String), chat(String)
    var params: [String: JSONValue] {
        switch self {
        case .shell(let id): ["shell_id": .string(id)]
        case .chat(let id): ["chat_id": .string(id)]
        }
    }
}

/// One file ready to send: its name as the phone knows it, its media type and its bytes.
public struct UploadFile: Sendable, Equatable {
    public var name: String
    public var mediaType: String?
    public var data: Data
    public init(name: String, mediaType: String?, data: Data) {
        // A name the desktop would refuse is replaced; the desktop makes its own from it anyway.
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let printable = !trimmed.isEmpty && trimmed.utf8.count <= UploadLimits.nameMaximumBytes
            && !trimmed.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) || $0 == "\u{2028}" || $0 == "\u{2029}" }
        self.name = printable ? trimmed : "file"
        self.mediaType = mediaType.flatMap { UploadFile.validMediaType($0) ? $0 : nil }
        self.data = data
    }
    public var sha256: String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }
    /// `type/subtype` with RFC 6838's restricted names, as the desktop takes it.
    static func validMediaType(_ text: String) -> Bool {
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!#$&^_.+-")
        let parts = text.split(separator: "/", omittingEmptySubsequences: false)
        return text.utf8.count <= 127 && parts.count == 2 && parts.allSatisfy { !$0.isEmpty && $0.unicodeScalars.allSatisfy(allowed.contains) }
    }
}

/// A file the desktop has, complete: its upload id and where it is on the desktop.
public struct UploadedFile: Sendable, Equatable {
    public let upload: String
    public let path: String
    public init(upload: String, path: String) { self.upload = upload; self.path = path }
}

public enum UploadError: Error, LocalizedError, Equatable, Sendable {
    /// The desktop's connector predates uploads (no `features.upload`, or "unsupported RPC method").
    case desktopTooOld
    case tooLarge(name: String, limit: Int)
    case tooMany(limit: Int)
    /// A paste whose outcome is not known: it may have reached the shell. Never repeated by itself.
    case pasteUncertain
    /// The desktop refused, in its own words.
    case refused(String)
    public var errorDescription: String? {
        switch self {
        case .desktopTooOld: "This Mac’s RiWork cannot take files from the phone yet. Update RiWork on the Mac, then try again."
        case .tooLarge(let name, let limit): "\(name) is larger than the \(ByteCountFormatter.string(fromByteCount: Int64(limit), countStyle: .file)) the Mac takes."
        case .tooMany(let limit): "Send at most \(limit) files at once."
        case .pasteUncertain: "The files may already be in the terminal. Look before sending them again."
        case .refused(let message): message
        }
    }
}

/// The params of the upload requests, and the checks `RequestValidation` makes before one leaves the phone.
public enum UploadRequests {
    static let methods: [String: (required: Set<String>, optional: Set<String>)] = [
        "upload.begin": (["upload", "name", "size", "sha256"], ["shell_id", "chat_id", "type"]),
        "upload.chunk": (["upload", "offset", "data"], []),
        "upload.finish": (["upload"], []),
        "upload.cancel": (["upload"], []),
        "shell.paste": (["shell_id", "batch", "uploads"], [])
    ]
    public static func begin(upload: String, file: UploadFile, target: UploadTarget) -> [String: JSONValue] {
        var params = target.params
        params["upload"] = .string(upload); params["name"] = .string(file.name); params["size"] = .number(Double(file.data.count))
        params["sha256"] = .string(file.sha256)
        if let type = file.mediaType { params["type"] = .string(type) }
        return params
    }
    public static func chunk(upload: String, offset: Int, data: Data) -> [String: JSONValue] {
        ["upload": .string(upload), "offset": .number(Double(offset)), "data": .string(base64URL(data))]
    }
    public static func paste(shell: String, batch: String, uploads: [String]) -> [String: JSONValue] {
        ["shell_id": .string(shell), "batch": .string(batch), "uploads": .array(uploads.map(JSONValue.string))]
    }
    /// Unpadded URL-safe base64, as the protocol writes bytes.
    public static func base64URL(_ data: Data) -> String {
        data.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    }
    static func validate(method: String, params: [String: JSONValue], uuid: (String?) throws -> Void) throws {
        func number(_ key: String, _ range: ClosedRange<Double>) throws {
            guard case .number(let value)? = params[key], range.contains(value), value.rounded() == value else { throw RemoteError.protocolViolation("Invalid \(key).") }
        }
        switch method {
        case "upload.begin":
            guard (params["shell_id"] == nil) != (params["chat_id"] == nil) else { throw RemoteError.protocolViolation("An upload goes to one shell or one chat.") }
            guard let name = params["name"]?.string, UploadFile(name: name, mediaType: nil, data: Data()).name == name else { throw RemoteError.protocolViolation("Invalid file name.") }
            if let type = params["type"] { guard let text = type.string, UploadFile.validMediaType(text) else { throw RemoteError.protocolViolation("Invalid media type.") } }
            try number("size", 1...1e12)
            guard let hash = params["sha256"]?.string, hash.count == 64, hash.allSatisfy({ $0.isHexDigit && !$0.isUppercase }) else { throw RemoteError.protocolViolation("Invalid checksum.") }
        case "upload.chunk":
            try number("offset", 0...1e12)
            guard let data = params["data"]?.string, !data.isEmpty, data.count <= (UploadLimits.chunkBytes + 2) / 3 * 4 else { throw RemoteError.protocolViolation("Invalid chunk.") }
        case "shell.paste":
            let uploads = params["uploads"]?.array ?? []
            guard (1...UploadLimits.maximumFiles).contains(uploads.count), Set(uploads.compactMap(\.string)).count == uploads.count else { throw RemoteError.protocolViolation("Paste 1 to \(UploadLimits.maximumFiles) different files.") }
            for upload in uploads { try uuid(upload.string) }
        default: break
        }
    }
}

/// Sending files and pasting them, over any transport. The desktop keeps a partial upload, so a link that drops is picked up where it
/// left off once it is back (`upload.begin` again says how far it got); a paste carries a batch id the desktop dedupes, so asking again
/// after a lost answer never pastes twice.
public enum FileTransfer {
    /// How long a dropped link may take to come back before an upload gives up.
    nonisolated(unsafe) public static var reconnectWait: Duration = .seconds(30)
    nonisolated(unsafe) public static var reconnectPoll: Duration = .milliseconds(500)

    /// Sends `file` to `target` and returns where it is on the desktop. `progress` gets the bytes the desktop has after each step.
    /// Cancelling the task stops it and tells the desktop to drop what it has.
    public static func send(_ file: UploadFile, to target: UploadTarget, feature: UploadFeature, over transport: any RemoteTransport,
                            upload: String = UUID().uuidString.lowercased(),
                            progress: @Sendable (Int) -> Void = { _ in }) async throws -> UploadedFile {
        guard file.data.count <= feature.maximumBytes else { throw UploadError.tooLarge(name: file.name, limit: feature.maximumBytes) }
        let begin = UploadRequests.begin(upload: upload, file: file, target: target)
        let chunk = max(1, min(feature.chunkBytes, UploadLimits.chunkBytes))
        do {
            var attempts = 0
            while true {
                do {
                    var answer = try await call(transport, "upload.begin", begin)
                    var received = try position(answer, upload: upload, size: file.data.count)
                    progress(received)
                    while answer["status"].string != "complete", received < file.data.count {
                        try Task.checkCancellation()
                        let end = min(received + chunk, file.data.count)
                        answer = try await call(transport, "upload.chunk", UploadRequests.chunk(upload: upload, offset: received, data: file.data.subdata(in: received..<end)))
                        received = try position(answer, upload: upload, size: file.data.count)
                        progress(received)
                    }
                    if answer["status"].string != "complete" { answer = try await call(transport, "upload.finish", ["upload": .string(upload)]) }
                    guard answer["status"].string == "complete", let path = answer["path"].string, !path.isEmpty else {
                        throw RemoteError.protocolViolation("The desktop did not say where the file is.")
                    }
                    return UploadedFile(upload: upload, path: path)
                } catch let error where transient(error) && attempts < 5 {
                    attempts += 1
                    try await waitForLink(transport)
                }
            }
        } catch {
            if error is CancellationError || Task.isCancelled {
                // Not the caller's task: that one is cancelled, and this must still leave.
                Task.detached { _ = try? await transport.request(method: "upload.cancel", params: ["upload": .string(upload)], id: UUID().uuidString.lowercased()) }
                throw CancellationError()
            }
            throw error
        }
    }

    /// Pastes finished uploads into their shell, once. A lost answer is asked again with the same batch: `duplicate` means it was
    /// pasted, `uncertain` that it may have been (`UploadError.pasteUncertain`).
    public static func paste(_ uploads: [String], into shell: String, over transport: any RemoteTransport, batch: String = UUID().uuidString.lowercased()) async throws {
        var attempts = 0
        while true {
            do {
                let answer = try await call(transport, "shell.paste", UploadRequests.paste(shell: shell, batch: batch, uploads: uploads))
                switch answer["status"].string {
                case "sent", "duplicate": return
                case "uncertain": throw UploadError.pasteUncertain
                default: throw RemoteError.protocolViolation("Unexpected paste answer.")
                }
            } catch let error where transient(error) && attempts < 5 {
                attempts += 1
                try await waitForLink(transport)
            }
        }
    }

    /// One request, with the desktop's refusals made into what the person should read.
    static func call(_ transport: any RemoteTransport, _ method: String, _ params: [String: JSONValue]) async throws -> JSONValue {
        do { return try await transport.request(method: method, params: params, id: UUID().uuidString.lowercased()) }
        catch RemoteError.rpc(let code, let message) {
            if code == "invalid_request", message == "unsupported RPC method" { throw UploadError.desktopTooOld }
            if code == "cli_error" || code == "upload_limit" || code == "not_found" || code == "invalid_request" || code == "input_unavailable" {
                throw UploadError.refused(message)
            }
            throw RemoteError.rpc(code: code, message: message)
        }
    }
    private static func position(_ answer: JSONValue, upload: String, size: Int) throws -> Int {
        guard answer["upload"].string == upload, case .number(let received) = answer["received"], received >= 0, received <= Double(size), received.rounded() == received else {
            throw RemoteError.protocolViolation("Unexpected upload answer.")
        }
        return Int(received)
    }
    /// A link that dropped or an answer that did not come: worth waiting for the link and asking again.
    static func transient(_ error: any Error) -> Bool {
        switch error {
        case RemoteError.disconnected, RemoteError.timeout, RemoteError.relayClosed: true
        default: false
        }
    }
    private static func waitForLink(_ transport: any RemoteTransport) async throws {
        let deadline = ContinuousClock.now + reconnectWait
        try await Task.sleep(for: reconnectPoll)
        while !(await transport.isConnected()) {
            guard ContinuousClock.now < deadline else { throw RemoteError.disconnected }
            try await Task.sleep(for: reconnectPoll)
        }
    }
}

/// Makes a picture something every agent CLI reads. Claude Code, Codex and Grok attach PNG, JPEG, GIF and WebP; a HEIC photo (what an
/// iPhone takes) is none of those, so it is re-encoded as JPEG. A photo re-encoded here has its orientation applied to its pixels and
/// carries no metadata (no location), is at most `photoMaximumPixels` on its longest side, and is named `.jpg`.
public enum PhotoPreparation {
    static let readable: Set<String> = [UTType.png.identifier, UTType.jpeg.identifier, UTType.gif.identifier, UTType.webP.identifier]

    /// `data` as it should travel. `reencode`: always re-encode a JPEG too (a photo from the library or the camera, so that its
    /// location stays on the phone); a PNG or GIF (a screenshot, an animation) is left alone. A file that is not a picture is untouched.
    public static func prepare(_ data: Data, name: String, typeIdentifier: String?, reencode: Bool) -> UploadFile {
        let type = typeIdentifier.flatMap(UTType.init) ?? UTType(filenameExtension: (name as NSString).pathExtension)
        let mediaType = type?.preferredMIMEType
        guard let type, type.conforms(to: .image) else { return UploadFile(name: name, mediaType: mediaType, data: data) }
        let keep = readable.contains(type.identifier) && (!reencode || type == .png || type == .gif)
        if keep { return UploadFile(name: name, mediaType: mediaType, data: data) }
        guard let jpeg = jpeg(from: data) else { return UploadFile(name: name, mediaType: mediaType, data: data) }
        let stem = (name as NSString).deletingPathExtension
        return UploadFile(name: (stem.isEmpty ? "photo" : stem) + ".jpg", mediaType: "image/jpeg", data: jpeg)
    }

    /// The picture's first frame as JPEG, upright and without metadata. Nil for data that is not a picture ImageIO reads.
    public static func jpeg(from data: Data, quality: Double = 0.85) -> Data? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] else { return nil }
        let width = (properties[kCGImagePropertyPixelWidth] as? Int) ?? 0, height = (properties[kCGImagePropertyPixelHeight] as? Int) ?? 0
        let longest = max(1, min(max(width, height), UploadLimits.photoMaximumPixels))
        let options: [CFString: Any] = [kCGImageSourceCreateThumbnailFromImageAlways: true, kCGImageSourceCreateThumbnailWithTransform: true,
                                        kCGImageSourceThumbnailMaxPixelSize: longest, kCGImageSourceShouldCacheImmediately: true]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
        let output = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(output, UTType.jpeg.identifier as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return output as Data
    }
}
