import XCTest
import CryptoKit
import ImageIO
import UniformTypeIdentifiers
@testable import RiWorkCore

/// Files from the phone: what the requests look like, how an upload resumes after the link drops, that a paste is asked again with
/// the same batch, what an older desktop is told apart by, and what a photo becomes before it travels.
final class FileUploadTests: XCTestCase {
    private let shell = "11111111-1111-4111-8111-111111111111"
    private let chat = "22222222-2222-4222-8222-222222222222"
    private let feature = UploadFeature(maximumBytes: 1000, chunkBytes: 64, quotaBytes: 4000, maximumFiles: 4)

    override func setUp() { FileTransfer.reconnectPoll = .milliseconds(5); FileTransfer.reconnectWait = .seconds(2) }

    /// The desktop's side of an upload, as `remote/src/upload.rs` keeps it: bytes by upload, where each stands, and pastes by batch.
    private actor Desktop: RemoteTransport {
        var files: [String: (size: Int, data: Data, complete: Bool)] = [:]
        var calls: [(method: String, params: [String: JSONValue])] = []
        var pastedBatches: [String: Int] = [:]
        /// Drop the link at this request (counted from 1), once.
        var dropAt: Int?
        var connected = true
        var unsupported = false
        var pasteStatus = "sent"
        var refusal: (code: String, message: String)?
        func set(dropAt: Int?) { self.dropAt = dropAt }
        func set(unsupported: Bool) { self.unsupported = unsupported }
        func set(pasteStatus: String) { self.pasteStatus = pasteStatus }
        func set(refusal: (String, String)?) { self.refusal = refusal.map { (code: $0.0, message: $0.1) } }
        func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { pairing }
        func disconnect() async {}
        func isConnected() async -> Bool { connected }
        func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
            try RequestValidation.validate(method: method, params: params, id: id)
            calls.append((method, params))
            if calls.count == dropAt { dropAt = nil; return try await drop() }
            if unsupported { throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method") }
            if let refusal { throw RemoteError.rpc(code: refusal.code, message: refusal.message) }
            let upload = params["upload"]?.string ?? ""
            func status(_ id: String) -> JSONValue {
                let file = files[id]!
                var answer: [String: JSONValue] = ["upload": .string(id), "received": .number(Double(file.data.count)), "status": .string(file.complete ? "complete" : "partial")]
                if file.complete { answer["path"] = .string("/home/uploads/x/\(id).png") }
                return .object(answer)
            }
            switch method {
            case "upload.begin":
                if files[upload] == nil, case .number(let size)? = params["size"] { files[upload] = (Int(size), Data(), false) }
                return status(upload)
            case "upload.chunk":
                guard case .number(let offset)? = params["offset"], let text = params["data"]?.string, let data = Self.decode(text) else { throw RemoteError.protocolViolation("bad chunk") }
                var file = files[upload]!
                XCTAssertLessThanOrEqual(Int(offset), file.data.count)
                if Int(offset) + data.count > file.data.count { file.data.append(data.suffix(Int(offset) + data.count - file.data.count)) }
                files[upload] = file
                return status(upload)
            case "upload.finish":
                files[upload]!.complete = files[upload]!.data.count == files[upload]!.size
                return status(upload)
            case "upload.cancel":
                files[upload] = nil
                return .object(["upload": .string(upload), "status": .string("cancelled"), "received": .number(0)])
            case "shell.paste":
                let batch = params["batch"]!.string!
                pastedBatches[batch, default: 0] += 1
                let status = pastedBatches[batch]! > 1 ? "duplicate" : pasteStatus
                return .object(["shell_id": params["shell_id"]!, "batch": .string(batch), "status": .string(status)])
            default: throw RemoteError.protocolViolation("unexpected \(method)")
            }
        }
        /// The request reached the desktop and was carried out, but the answer was lost with the link, which comes back shortly.
        private func drop() async throws -> JSONValue {
            let last = calls[calls.count - 1]
            calls.removeLast(); dropAt = nil
            _ = try await request(method: last.method, params: last.params, id: UUID().uuidString.lowercased())
            connected = false
            Task { try? await Task.sleep(for: .milliseconds(30)); self.reconnect() }
            throw RemoteError.disconnected
        }
        private func reconnect() { connected = true }
        static func decode(_ text: String) -> Data? {
            var base64 = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
            while base64.count % 4 != 0 { base64 += "=" }
            return Data(base64Encoded: base64)
        }
        func methods() -> [String] { calls.map(\.method) }
        func params(_ index: Int) -> [String: JSONValue] { calls[index].params }
        func offsets() -> [Double] { calls.filter { $0.method == "upload.chunk" }.compactMap { if case .number(let n)? = $0.params["offset"] { n } else { nil } } }
        func callCount() -> Int { calls.count }
        func data(_ upload: String) -> Data? { files[upload]?.data }
    }

    private func file(_ count: Int, name: String = "a.png") -> UploadFile {
        UploadFile(name: name, mediaType: "image/png", data: Data((0..<count).map { UInt8($0 % 251) }))
    }

    func testAFileGoesInChunksAndComesBackWithItsPath() async throws {
        let desktop = Desktop()
        let upload = "33333333-3333-4333-8333-333333333333"
        let progress = Progress()
        let sent = try await FileTransfer.send(file(150), to: .shell(shell), feature: feature, over: desktop, upload: upload) { progress.note($0) }
        XCTAssertEqual(sent, UploadedFile(upload: upload, path: "/home/uploads/x/\(upload).png"))
        let methods = await desktop.methods()
        XCTAssertEqual(methods, ["upload.begin", "upload.chunk", "upload.chunk", "upload.chunk", "upload.finish"])
        let stored = await desktop.data(upload)
        XCTAssertEqual(stored, file(150).data)
        XCTAssertEqual(progress.values, [0, 64, 128, 150])
        let begin = await desktop.params(0)
        XCTAssertEqual(begin["shell_id"], .string(shell))
        XCTAssertEqual(begin["sha256"], .string(SHA256.hash(data: file(150).data).map { String(format: "%02x", $0) }.joined()))
        XCTAssertEqual(begin["size"], .number(150))
        XCTAssertEqual(begin["type"], .string("image/png"))
    }

    func testALinkThatDropsResumesWhereTheDesktopGotTo() async throws {
        let desktop = Desktop()
        await desktop.set(dropAt: 3)   // The second chunk arrives, its answer does not.
        let upload = "44444444-4444-4444-8444-444444444444"
        _ = try await FileTransfer.send(file(200), to: .chat(chat), feature: feature, over: desktop, upload: upload)
        let methods = await desktop.methods()
        XCTAssertEqual(methods, ["upload.begin", "upload.chunk", "upload.chunk", "upload.begin", "upload.chunk", "upload.chunk", "upload.finish"])
        // The resumed upload asked from 128, never sent the second chunk twice.
        let offsets = await desktop.offsets()
        XCTAssertEqual(offsets, [0, 64, 128, 192])
        let stored = await desktop.data(upload)
        XCTAssertEqual(stored, file(200).data)
    }

    func testAPasteAskedAgainUsesTheSameBatchAndNeverPastesTwice() async throws {
        let desktop = Desktop()
        await desktop.set(dropAt: 1)
        try await FileTransfer.paste(["55555555-5555-4555-8555-555555555555"], into: shell, over: desktop, batch: "66666666-6666-4666-8666-666666666666")
        let pasted = await desktop.pastedBatches
        XCTAssertEqual(pasted, ["66666666-6666-4666-8666-666666666666": 2])   // The second time answered duplicate.
        let uncertain = Desktop()
        await uncertain.set(pasteStatus: "uncertain")
        do { try await FileTransfer.paste(["55555555-5555-4555-8555-555555555555"], into: shell, over: uncertain); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? UploadError, .pasteUncertain) }
    }

    func testAnOlderDesktopAndTheDesktopsRefusalsReadPlainly() async throws {
        let old = Desktop()
        await old.set(unsupported: true)
        do { _ = try await FileTransfer.send(file(10), to: .shell(shell), feature: feature, over: old); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? UploadError, .desktopTooOld) }
        XCTAssertNil(DesktopFeatures(ready: .object(["features": .object(["chat": .bool(true)])])).upload)
        XCTAssertNil(DesktopFeatures(ready: .object([:])).upload)

        let full = Desktop()
        await full.set(refusal: ("upload_limit", "at most 4 uploads may be under way at once"))
        do { _ = try await FileTransfer.send(file(10), to: .shell(shell), feature: feature, over: full); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? UploadError, .refused("at most 4 uploads may be under way at once")) }

        let none = Desktop()
        do { _ = try await FileTransfer.send(file(1001), to: .shell(shell), feature: feature, over: none); XCTFail("should throw") }
        catch { XCTAssertEqual(error as? UploadError, .tooLarge(name: "a.png", limit: 1000)) }
        let calls = await none.callCount()
        XCTAssertEqual(calls, 0, "nothing leaves the phone for a file the desktop would refuse")
        XCTAssertTrue(UploadError.desktopTooOld.localizedDescription.contains("Update RiWork on the Mac"))
    }

    func testTheFeatureIsReadAndBoundedByWhatAFrameHolds() {
        let ready = JSONValue.object(["features": .object(["upload": .object(["max_bytes": .number(52_428_800), "chunk_bytes": .number(1_000_000), "quota_bytes": .number(209_715_200), "max_files": .number(99)])])])
        let upload = DesktopFeatures(ready: ready).upload
        XCTAssertEqual(upload?.maximumBytes, 52_428_800)
        XCTAssertEqual(upload?.chunkBytes, UploadLimits.chunkBytes)
        XCTAssertEqual(upload?.maximumFiles, UploadLimits.maximumFiles)
        XCTAssertNil(UploadFeature(.object(["max_bytes": .string("1"), "chunk_bytes": .number(1)])))
    }

    func testRequestsAreCheckedBeforeTheyLeave() throws {
        let id = "77777777-7777-4777-8777-777777777777"
        func valid(_ method: String, _ params: [String: JSONValue]) -> Bool { (try? RequestValidation.validate(method: method, params: params, id: id)) != nil }
        let begin = UploadRequests.begin(upload: id, file: file(3), target: .shell(shell))
        XCTAssertTrue(valid("upload.begin", begin))
        var both = begin; both["chat_id"] = .string(chat)
        XCTAssertFalse(valid("upload.begin", both))
        var none = begin; none["shell_id"] = nil
        XCTAssertFalse(valid("upload.begin", none))
        var path = begin; path["path"] = .string("/etc/passwd")
        XCTAssertFalse(valid("upload.begin", path))
        var empty = begin; empty["size"] = .number(0)
        XCTAssertFalse(valid("upload.begin", empty))
        var badID = begin; badID["upload"] = .string("x")
        XCTAssertFalse(valid("upload.begin", badID))
        XCTAssertTrue(valid("upload.chunk", UploadRequests.chunk(upload: id, offset: 0, data: Data(count: UploadLimits.chunkBytes))))
        XCTAssertFalse(valid("upload.chunk", UploadRequests.chunk(upload: id, offset: 0, data: Data(count: UploadLimits.chunkBytes + 3))))
        XCTAssertTrue(valid("shell.paste", UploadRequests.paste(shell: shell, batch: id, uploads: [chat])))
        XCTAssertFalse(valid("shell.paste", UploadRequests.paste(shell: shell, batch: id, uploads: [])))
        XCTAssertFalse(valid("shell.paste", UploadRequests.paste(shell: shell, batch: id, uploads: [chat, chat])))
        XCTAssertFalse(valid("shell.paste", UploadRequests.paste(shell: shell, batch: id, uploads: ["../x"])))
        XCTAssertEqual(UploadRequests.base64URL(Data([0xfb, 0xff])), "-_8")
        // A name the desktop would refuse is replaced, the media type dropped.
        XCTAssertEqual(UploadFile(name: "a\nb", mediaType: "image/png; x", data: Data()).name, "file")
        XCTAssertNil(UploadFile(name: "a", mediaType: "image/png; x", data: Data()).mediaType)
    }

    func testAPhotoBecomesAnUprightJPEGWithoutMetadataAndAScreenshotStaysPNG() throws {
        let png = try image(type: .png, width: 40, height: 20)
        let screenshot = PhotoPreparation.prepare(png, name: "Screenshot.png", typeIdentifier: UTType.png.identifier, reencode: true)
        XCTAssertEqual(screenshot.data, png)
        XCTAssertEqual(screenshot.mediaType, "image/png")
        // A rotated photo with a location: re-encoded upright, the location gone.
        let tagged = try image(type: .jpeg, width: 40, height: 20, orientation: 6, gps: true)
        let photo = PhotoPreparation.prepare(tagged, name: "IMG_0001.JPG", typeIdentifier: UTType.jpeg.identifier, reencode: true)
        XCTAssertEqual(photo.name, "IMG_0001.jpg")
        XCTAssertEqual(photo.mediaType, "image/jpeg")
        let properties = try XCTUnwrap(CGImageSourceCopyPropertiesAtIndex(XCTUnwrap(CGImageSourceCreateWithData(photo.data as CFData, nil)), 0, nil) as? [CFString: Any])
        XCTAssertEqual(properties[kCGImagePropertyPixelWidth] as? Int, 20)
        XCTAssertEqual(properties[kCGImagePropertyPixelHeight] as? Int, 40)
        XCTAssertNil(properties[kCGImagePropertyGPSDictionary])
        // A JPEG from Files is left as it is; a file that is not a picture always.
        XCTAssertEqual(PhotoPreparation.prepare(tagged, name: "x.jpg", typeIdentifier: nil, reencode: false).data, tagged)
        let text = Data("hello".utf8)
        let note = PhotoPreparation.prepare(text, name: "notes.txt", typeIdentifier: nil, reencode: true)
        XCTAssertEqual(note, UploadFile(name: "notes.txt", mediaType: "text/plain", data: text))
        // A picture no agent reads (TIFF here, as HEIC is on a phone) becomes JPEG even from Files.
        let tiff = try image(type: .tiff, width: 8, height: 8)
        XCTAssertEqual(PhotoPreparation.prepare(tiff, name: "scan.tiff", typeIdentifier: UTType.tiff.identifier, reencode: false).mediaType, "image/jpeg")
    }

    private func image(type: UTType, width: Int, height: Int, orientation: Int = 1, gps: Bool = false) throws -> Data {
        let context = try XCTUnwrap(CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(),
                                              bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        context.setFillColor(red: 1, green: 0, blue: 0, alpha: 1); context.fill(CGRect(x: 0, y: 0, width: width / 2, height: height))
        let cgImage = try XCTUnwrap(context.makeImage())
        let output = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(output, type.identifier as CFString, 1, nil))
        var properties: [CFString: Any] = [kCGImagePropertyOrientation: orientation]
        if gps { properties[kCGImagePropertyGPSDictionary] = [kCGImagePropertyGPSLatitude: 47.4, kCGImagePropertyGPSLatitudeRef: "N"] }
        CGImageDestinationAddImage(destination, cgImage, properties as CFDictionary)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        return output as Data
    }
}

/// What the progress callback was told, in order.
private final class Progress: @unchecked Sendable {
    private let lock = NSLock()
    private var seen: [Int] = []
    func note(_ value: Int) { lock.withLock { seen.append(value) } }
    var values: [Int] { lock.withLock { seen } }
}
