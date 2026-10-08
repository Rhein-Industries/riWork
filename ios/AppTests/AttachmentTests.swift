import XCTest
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// A desktop that takes files the way the connector does (or, `old`, one from before uploads), and records what it was asked.
actor UploadTransport: RemoteTransport {
    var connected = false
    var old = false
    var identity: UploadConnectionIdentity?
    var dropNextUploadBegin = false
    func dropNextBegin() { dropNextUploadBegin = true }
    var calls: [(method: String, params: [String: JSONValue])] = []
    var received: [String: Data] = [:]
    func setOld(_ on: Bool) { old = on }
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { connected = true; identity = UploadConnectionIdentity(pairing: pairing); return pairing }
    func disconnect() async { connected = false }
    func isConnected() async -> Bool { connected }
    func desktopFeatures() async -> DesktopFeatures {
        old ? DesktopFeatures() : DesktopFeatures(ready: .object(["features": .object(["upload": .object([
            "max_bytes": .number(1_000_000), "chunk_bytes": .number(92_160), "quota_bytes": .number(4_000_000), "max_files": .number(16)])])]))
    }
    func methods() -> [String] { calls.map(\.method).filter { $0.hasPrefix("upload.") || $0 == "shell.paste" } }
    func pasted() -> [[String: JSONValue]] { calls.filter { $0.method == "shell.paste" }.map(\.params) }
    func request(method: String, params: [String: JSONValue], id: String, boundTo expected: UploadConnectionIdentity) async throws -> JSONValue {
        guard expected == identity else { throw CancellationError() }
        return try await request(method: method, params: params, id: id)
    }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        guard connected else { throw RemoteError.disconnected }
        try RequestValidation.validate(method: method, params: params, id: id)
        calls.append((method, params))
        let upload = params["upload"]?.string ?? ""
        let path = "/Users/me/.local/share/riwork/uploads/x/photo-0a1b2c3d.jpg"
        switch method {
        case "projects.list", "worktrees.list", "orchestrators.list", "shells.list":
            return .object([String(method.split(separator: ".")[0]): .array([])])
        case "shell.resize.clear": return .object(["shell_id": params["shell_id"] ?? .null, "status": .string("cleared")])
        case "appearance.get": throw RemoteError.rpc(code: "not_found", message: "appearance not published")
        case _ where old: throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")
        case "upload.begin":
            if dropNextUploadBegin { dropNextUploadBegin = false; connected = false; throw RemoteError.disconnected }
            received[upload] = Data()
            return .object(["upload": .string(upload), "status": .string("partial"), "received": .number(0)])
        case "upload.chunk":
            var base64 = params["data"]!.string!.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
            while base64.count % 4 != 0 { base64 += "=" }
            received[upload, default: Data()].append(Data(base64Encoded: base64)!)
            return .object(["upload": .string(upload), "status": .string("partial"), "received": .number(Double(received[upload]!.count))])
        case "upload.finish":
            return .object(["upload": .string(upload), "status": .string("complete"), "received": .number(Double(received[upload]!.count)), "path": .string(path)])
        case "shell.paste":
            return .object(["shell_id": params["shell_id"]!, "batch": params["batch"]!, "status": .string("sent")])
        default: throw RemoteError.protocolViolation("Unknown method \(method)")
        }
    }
}

@MainActor final class AttachmentAppTests: XCTestCase {
    private let shell = "44444444-4444-4444-8444-444444444444"
    private let chat = "55555555-5555-4555-8555-555555555555"
    private var defaultsNames: [String] = []
    private let images = ChatAttachmentImages(directory: FileManager.default.temporaryDirectory.appendingPathComponent("attachment-tests-\(UUID().uuidString)"))

    override func tearDown() async throws {
        for name in defaultsNames { UserDefaults().removePersistentDomain(forName: name) }
        defaultsNames = []
        try? FileManager.default.removeItem(at: images.directory)
    }
    private func connected(old: Bool = false, defaults suite: String? = nil) async throws -> (RemoteModel, UploadTransport) {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        let desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        let name = suite ?? "com.riwork.tests.attach.\(UUID().uuidString)"
        if suite == nil { defaultsNames.append(name) }
        let transport = UploadTransport()
        await transport.setOld(old)
        let model = RemoteModel(client: transport, keychain: keychain, defaults: UserDefaults(suiteName: name)!, attachmentImages: images)
        await model.connect()
        XCTAssertEqual(model.state, .connected)
        await settle { model.desktopFeatures.upload != nil || old }
        return (model, transport)
    }
    private func settle(_ condition: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async {
        let deadline = ContinuousClock.now + .seconds(5)
        while !condition(), ContinuousClock.now < deadline { try? await Task.sleep(for: .milliseconds(5)) }
        XCTAssertTrue(condition(), file: file, line: line)
    }
    private func photo() -> Data {
        UIGraphicsImageRenderer(size: CGSize(width: 30, height: 20)).jpegData(withCompressionQuality: 0.9) { context in
            UIColor.red.setFill(); context.fill(CGRect(x: 0, y: 0, width: 15, height: 20))
        }
    }

    func testAPhotoForATerminalIsSentThenPastedAndTheLineGoesAway() async throws {
        let (model, transport) = try await connected()
        model.attach([.camera(photo())], to: .shell(shell))
        XCTAssertNotNil(model.uploadActivity(for: .shell(shell)), "shown at once")
        XCTAssertNil(model.uploadActivity(for: .chat(chat)), "only where it goes")
        await settle { model.attachments.task == nil }
        XCTAssertNil(model.attachments.activity)
        let methods = await transport.methods()
        XCTAssertEqual(methods, ["upload.begin", "upload.chunk", "upload.finish", "shell.paste"])
        let pasted = await transport.pasted()
        XCTAssertEqual(pasted.first?["shell_id"], .string(shell))
        XCTAssertEqual(pasted.first?["uploads"]?.array.count, 1)
        await model.disconnect()
    }

    private let path = "/Users/me/.local/share/riwork/uploads/x/photo-0a1b2c3d.jpg"
    private func exists(_ url: URL) -> Bool { FileManager.default.fileExists(atPath: url.path) }

    /// A photo for a chat becomes a card above the composer, not text: the draft is as it was, the card knows the path the message will
    /// name, its pictures are on the phone, and it is saved with the draft.
    func testAPhotoForAChatIsStagedAsACard() async throws {
        let (model, transport) = try await connected()
        let conversation = model.conversation(chat)
        conversation.draft = "What is in this picture?"
        model.attach([.camera(photo())], to: .chat(chat))
        await settle { model.attachments.task == nil }
        XCTAssertEqual(conversation.draft, "What is in this picture?", "no path in the text")
        let card = try XCTUnwrap(conversation.attachments.first)
        XCTAssertEqual(conversation.attachments.count, 1)
        XCTAssertEqual(card.kind, .image)
        XCTAssertEqual(card.name, "photo.jpg")
        XCTAssertEqual(card.path, path)
        XCTAssertGreaterThan(card.size, 0)
        XCTAssertTrue(exists(images.thumbnailURL(card.id)) && exists(images.previewURL(card.id)), "pictured from the bytes sent")
        XCTAssertEqual(model.chatDrafts.draft(chat)?.attachments, [card], "kept with the draft")
        let methods = await transport.methods()
        XCTAssertEqual(methods, ["upload.begin", "upload.chunk", "upload.finish"], "sent at once; nothing is typed")
        await model.disconnect()
    }
    func testAFileForAChatIsAFileCardAfterTheOnesStagedAlready() async throws {
        let (model, _) = try await connected()
        let conversation = model.conversation(chat)
        model.attach([.camera(photo())], to: .chat(chat))
        await settle { model.attachments.task == nil }
        model.attachments.loadSource = { _ in UploadFile(name: "notes.pdf", mediaType: "application/pdf", data: Data(repeating: 7, count: 2_400)) }
        model.attach([.camera(Data())], to: .chat(chat))
        await settle { model.attachments.task == nil }
        XCTAssertEqual(conversation.attachments.map(\.kind), [.image, .file])
        let file = try XCTUnwrap(conversation.attachments.last)
        XCTAssertEqual(file.name, "notes.pdf")
        XCTAssertEqual(file.sizeText, "2 KB")
        XCTAssertFalse(exists(images.thumbnailURL(file.id)), "a file has no picture")
        await model.disconnect()
    }
    /// × drops the card and its pictures. The desktop has no way to delete a finished upload, so nothing is asked of it.
    func testRemovingACardDropsItHereOnly() async throws {
        let (model, transport) = try await connected()
        let conversation = model.conversation(chat)
        model.attach([.camera(photo())], to: .chat(chat))
        await settle { model.attachments.task == nil }
        let card = try XCTUnwrap(conversation.attachments.first)
        model.removeStagedAttachment(card.id, from: chat)
        XCTAssertTrue(conversation.attachments.isEmpty)
        XCTAssertNil(model.chatDrafts.draft(chat), "nothing left to keep")
        XCTAssertFalse(exists(images.thumbnailURL(card.id)) || exists(images.previewURL(card.id)))
        let methods = await transport.methods()
        XCTAssertEqual(methods, ["upload.begin", "upload.chunk", "upload.finish"], "no RPC for a removal")
        await model.disconnect()
    }
    /// Cards outlive the app as the text does, and come back into the composer of the chat; pictures no draft holds are tidied away.
    func testCardsComeBackAfterARelaunch() async throws {
        let suite = "com.riwork.tests.attach.\(UUID().uuidString)"
        defaultsNames.append(suite)
        let (model, _) = try await connected(defaults: suite)
        model.conversation(chat).draft = "look"
        model.attach([.camera(photo())], to: .chat(chat))
        await settle { model.attachments.task == nil }
        let card = try XCTUnwrap(model.conversation(chat).attachments.first)
        images.save("bbbbbbbb-0000-4000-8000-000000000000", data: photo())
        await model.disconnect()
        let (relaunched, _) = try await connected(defaults: suite)
        XCTAssertEqual(relaunched.conversation(chat).draft, "look")
        XCTAssertEqual(relaunched.conversation(chat).attachments, [card])
        await settle { !self.exists(self.images.thumbnailURL("bbbbbbbb-0000-4000-8000-000000000000")) }
        XCTAssertTrue(exists(images.thumbnailURL(card.id)), "a staged card keeps its picture")
        await relaunched.disconnect()
    }

    func testAMacTooOldToTakeFilesSaysSoAndNothingIsSent() async throws {
        let (model, transport) = try await connected(old: true)
        model.attach([.camera(photo())], to: .shell(shell))
        let activity = try XCTUnwrap(model.uploadActivity(for: .shell(shell)))
        guard case .failed(let message) = activity.phase else { return XCTFail("\(activity)") }
        XCTAssertTrue(message.contains("Update RiWork on the Mac"), message)
        let methods = await transport.methods()
        XCTAssertTrue(methods.isEmpty)
        model.dismissUploadFailure()
        XCTAssertNil(model.attachments.activity)
        await model.disconnect()
    }

    func testCancellingStopsAndClearsTheLine() async throws {
        let (model, _) = try await connected()
        model.attach([.camera(photo())], to: .shell(shell))
        model.cancelUpload()
        XCTAssertNil(model.attachments.activity)
        XCTAssertNil(model.attachments.task)
        await model.disconnect()
    }

    func testChangingDesktopCancelsPendingSourceBeforeAnyBytesLeave() async throws {
        let (model, transport) = try await connected()
        var load: CheckedContinuation<UploadFile, any Error>?
        model.attachments.loadSource = { _ in try await withCheckedThrowingContinuation { load = $0 } }
        model.attach([.camera(Data())], to: .chat(chat))
        await settle { load != nil }
        let task = try XCTUnwrap(model.attachments.task)
        var other = try XCTUnwrap(model.desktop)
        var fields = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(other.pairing)) as? [String: Any])
        fields["desktop_id"] = "99999999-9999-4999-8999-999999999999"
        fields["route_id"] = "88888888-8888-4888-8888-888888888888"
        other.pairing = try JSONDecoder().decode(Pairing.self, from: JSONSerialization.data(withJSONObject: fields))
        model.desktops.append(other)
        await model.activate(other.id)
        load?.resume(returning: UploadFile(name: "fixture.bin", mediaType: nil, data: Data([1, 2, 3])))
        await task.value
        let methods = await transport.methods()
        XCTAssertTrue(methods.isEmpty, "late source loading must not send to the new desktop")
        XCTAssertNil(model.attachments.task)
        await model.disconnect()
    }
    func testCancelledOldSourceCannotClearOrOverwriteANewerJob() async throws {
        let (model, transport) = try await connected()
        var loads: [CheckedContinuation<UploadFile, any Error>] = []
        model.attachments.loadSource = { _ in try await withCheckedThrowingContinuation { loads.append($0) } }
        model.attach([.camera(Data())], to: .chat(chat))
        await settle { loads.count == 1 }
        let old = try XCTUnwrap(model.attachments.task)
        model.cancelUpload()
        model.attach([.camera(Data())], to: .shell(shell))
        await settle { loads.count == 2 }
        let newJob = model.attachments.job
        loads[0].resume(returning: UploadFile(name: "old.bin", mediaType: nil, data: Data([0])))
        await old.value
        XCTAssertEqual(model.attachments.job, newJob)
        XCTAssertNotNil(model.attachments.task)
        XCTAssertEqual(model.attachments.activity?.target, .shell(shell))
        loads[1].resume(returning: UploadFile(name: "new.bin", mediaType: nil, data: Data([1])))
        await settle { model.attachments.task == nil }
        let methods = await transport.methods()
        XCTAssertEqual(methods, ["upload.begin", "upload.chunk", "upload.finish", "shell.paste"])
        await model.disconnect()
    }

    func testChangingDesktopDuringReconnectWaitNeverRetriesOnTheNewPeer() async throws {
        let (model, transport) = try await connected()
        await transport.dropNextBegin()
        model.attachments.loadSource = { _ in UploadFile(name: "fixture.bin", mediaType: nil, data: Data([1, 2, 3])) }
        model.attach([.camera(Data())], to: .chat(chat))
        await settle { model.attachments.activity?.phase == .sending }
        while await transport.isConnected() { try await Task.sleep(for: .milliseconds(5)) }
        let task = try XCTUnwrap(model.attachments.task)
        let old = try XCTUnwrap(model.desktop)
        var fields = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(old.pairing)) as? [String: Any])
        fields["desktop_id"] = "99999999-9999-4999-8999-999999999999"
        fields["route_id"] = "88888888-8888-4888-8888-888888888888"
        let pairing = try JSONDecoder().decode(Pairing.self, from: JSONSerialization.data(withJSONObject: fields))
        let other = SavedDesktop(name: "Other fixture", pairing: pairing, allowLocalDevelopment: false)
        model.desktops.append(other)
        await model.activate(other.id)
        await task.value
        let methods = await transport.methods()
        XCTAssertEqual(methods, ["upload.begin"], "no chunk, retry, or detached cancellation may reach the new peer")
        await model.disconnect()
    }
    func testAnUploadCanResumeOnTheSameAuthenticatedDesktop() async throws {
        let (model, transport) = try await connected()
        let pairing = try XCTUnwrap(model.desktop).pairing
        await transport.dropNextBegin()
        model.attachments.loadSource = { _ in UploadFile(name: "fixture.bin", mediaType: nil, data: Data([1, 2, 3])) }
        model.attach([.camera(Data())], to: .chat(chat))
        while await transport.isConnected() { try await Task.sleep(for: .milliseconds(5)) }
        _ = try await transport.connect(pairing: pairing, allowLocalDevelopment: false)
        await settle { model.attachments.task == nil }
        let methods = await transport.methods()
        XCTAssertEqual(methods, ["upload.begin", "upload.begin", "upload.chunk", "upload.finish"])
        await model.disconnect()
    }

    // MARK: The paperclip's menu

    private func style(native: Bool) -> DesktopStyle {
        var theme = DesktopTheme.builtIn
        theme.native = native
        return DesktopStyle(theme)
    }

    func testTheChoicesOfferTheCameraOnlyWhereThereIsOne() {
        let hasCamera = UIImagePickerController.isSourceTypeAvailable(.camera)
        XCTAssertEqual(AttachmentChoice.available, hasCamera ? [.photos, .camera, .files] : [.photos, .files])
        XCTAssertEqual(AttachmentChoice.photos.symbol, "photo.on.rectangle")
        XCTAssertEqual(AttachmentChoice.camera.symbol, "camera")
        XCTAssertEqual(AttachmentChoice.files.symbol, "folder")
    }

    /// The key bar's paperclip opens a menu from the key itself, not a dialog over the screen: sentence case under Native, capitals in
    /// the terminal look, and the choice reaches the terminal as it is.
    func testTheKeyBarPaperclipIsAMenuOnTheKey() throws {
        for native in [true, false] {
            let view = KeyCaptureView()
            view.bar.style = style(native: native)
            var chosen: [AttachmentChoice] = []
            view.onAttach = { chosen.append($0) }
            let paperclip = try XCTUnwrap(view.bar.buttons[.attach])
            XCTAssertTrue(paperclip.showsMenuAsPrimaryAction)
            let items = try XCTUnwrap(paperclip.menu).children.compactMap { $0 as? UIAction }
            let titles = AttachmentChoice.available.map { native ? $0.title : $0.title.uppercased() }
            XCTAssertEqual(items.map(\.title), titles)
            XCTAssertEqual(items.first?.title, native ? "Photo library" : "PHOTO LIBRARY")
            XCTAssertFalse(titles.contains { $0.contains("Mac") }, "no explanatory title")
            // Pressing the key itself sends nothing: only a choice does.
            view.bar.tapped(.attach)
            XCTAssertEqual(chosen, [])
            view.bar.tapped(.attachFrom(.files))
            view.bar.tapped(.attachFrom(.photos))
            XCTAssertEqual(chosen, [.files, .photos])
        }
    }
}
