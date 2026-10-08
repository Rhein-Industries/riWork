import XCTest
import ImageIO
import UniformTypeIdentifiers
@testable import RiWorkCore

@MainActor final class ChatAttachmentsTests: XCTestCase {
    private var suites: [String] = []
    private var directories: [URL] = []
    override func tearDown() async throws {
        for name in suites { UserDefaults().removePersistentDomain(forName: name) }
        for url in directories { try? FileManager.default.removeItem(at: url) }
        suites = []; directories = []
    }
    private func defaults() -> UserDefaults {
        let name = "com.riwork.tests.attachments.\(UUID().uuidString)"
        suites.append(name)
        return UserDefaults(suiteName: name)!
    }
    private func images() -> ChatAttachmentImages {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("chat-attachments-\(UUID().uuidString)")
        directories.append(url)
        return ChatAttachmentImages(directory: url)
    }
    private func card(_ n: Int, kind: StagedAttachment.Kind = .image, name: String = "photo.jpg") -> StagedAttachment {
        StagedAttachment(id: "aaaaaaaa-0000-4000-8000-00000000000\(n)", kind: kind, name: name, size: 1000 * n, path: "/Users/me/uploads/\(n)/\(name)")
    }
    private func png(width: Int, height: Int) -> Data {
        let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(),
                                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.setFillColor(red: 1, green: 0, blue: 0, alpha: 1)
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        let output = NSMutableData()
        let destination = CGImageDestinationCreateWithData(output, UTType.png.identifier as CFString, 1, nil)!
        CGImageDestinationAddImage(destination, context.makeImage()!, nil)
        CGImageDestinationFinalize(destination)
        return output as Data
    }
    private func pixels(_ url: URL) -> Int? {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] else { return nil }
        return max(properties[kCGImagePropertyPixelWidth] as? Int ?? 0, properties[kCGImagePropertyPixelHeight] as? Int ?? 0)
    }

    // MARK: The card

    func testTheSizeReadsInKilobytesOrMegabytes() {
        XCTAssertEqual(StagedAttachment.sizeText(0), "1 KB")
        XCTAssertEqual(StagedAttachment.sizeText(840_400), "840 KB")
        XCTAssertEqual(StagedAttachment.sizeText(999_499), "999 KB")
        XCTAssertEqual(StagedAttachment.sizeText(2_400_000), "2.4 MB")
        XCTAssertEqual(StagedAttachment.sizeText(120_000_000), "120 MB")
    }
    func testAnImageIsAnImageByItsTypeOrItsName() {
        XCTAssertEqual(StagedAttachment.kind(mediaType: "image/jpeg", name: "photo.jpg"), .image)
        XCTAssertEqual(StagedAttachment.kind(mediaType: nil, name: "Screenshot.PNG"), .image)
        XCTAssertEqual(StagedAttachment.kind(mediaType: "application/pdf", name: "notes.pdf"), .file)
        XCTAssertEqual(StagedAttachment.kind(mediaType: nil, name: "Makefile"), .file)
        XCTAssertEqual(card(1, kind: .file, name: "report.pdf").symbol, "doc.richtext")
        XCTAssertEqual(card(1, kind: .file, name: "build.log").symbol, "doc.text")
        XCTAssertEqual(card(1, kind: .file, name: "archive.zip").symbol, "doc.zipper")
        XCTAssertEqual(card(1, kind: .file, name: "no-extension").symbol, "doc")
    }

    /// The message names the files as the draft used to: the text, then each path on its own line.
    func testTheMessageIsTheTextThenEachPath() {
        let two = [card(1), card(2, kind: .file, name: "notes.pdf")]
        XCTAssertEqual(ChatAttachmentMessage.compose("What is this?", two), "What is this?\n/Users/me/uploads/1/photo.jpg\n/Users/me/uploads/2/notes.pdf")
        XCTAssertEqual(ChatAttachmentMessage.compose("", two), "/Users/me/uploads/1/photo.jpg\n/Users/me/uploads/2/notes.pdf", "a card alone")
        XCTAssertEqual(ChatAttachmentMessage.compose("line\n", [card(1)]), "line\n/Users/me/uploads/1/photo.jpg")
        XCTAssertEqual(ChatAttachmentMessage.compose("plain", []), "plain", "no cards: the text as it is")
    }

    // MARK: Pictures on disk

    func testAnImageGetsASmallAndALargePictureAndBothGoWithIt() throws {
        let store = images()
        XCTAssertTrue(store.save(card(1).id, data: png(width: 3000, height: 1500)))
        XCTAssertEqual(pixels(store.thumbnailURL(card(1).id)), ChatAttachmentImages.thumbnailPixels)
        XCTAssertEqual(pixels(store.previewURL(card(1).id)), ChatAttachmentImages.previewPixels)
        store.remove(card(1).id)
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.thumbnailURL(card(1).id).path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.previewURL(card(1).id).path))
        XCTAssertFalse(store.save(card(2).id, data: Data("not a picture".utf8)), "a file card shows an icon instead")
    }
    func testPruningKeepsOnlyThePicturesOfCardsStillStaged() {
        let store = images()
        for n in 1...3 { store.save(card(n).id, data: png(width: 10, height: 10)) }
        store.prune(keeping: [card(2).id])
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.thumbnailURL(card(1).id).path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: store.thumbnailURL(card(2).id).path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: store.previewURL(card(2).id).path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.previewURL(card(3).id).path))
    }

    // MARK: Kept with the draft

    func testCardsAreKeptWithTheDraftAcrossARelaunch() {
        let defaults = defaults()
        let store = ChatDraftStore(defaults: defaults)
        store.setAttachments([card(1), card(2, kind: .file, name: "notes.pdf")], for: "a")
        let again = ChatDraftStore(defaults: defaults)
        XCTAssertEqual(again.draft("a")?.attachments, [card(1), card(2, kind: .file, name: "notes.pdf")])
        XCTAssertEqual(again.draft("a")?.text, "", "cards alone are a draft")
        XCTAssertEqual(again.attachmentIDs, [card(1).id, card(2).id])
        again.setAttachments([], for: "a")
        XCTAssertNil(again.draft("a"), "no text and no cards: nothing kept")
        XCTAssertNil(defaults.data(forKey: ChatDraftStore.key))
    }
    func testADraftSavedBeforeCardsStillReads() throws {
        let defaults = defaults()
        defaults.set(Data(#"{"a":{"text":"old draft","uncertain":false,"updatedAt":\#(Date.now.timeIntervalSinceReferenceDate)}}"#.utf8), forKey: ChatDraftStore.key)
        let store = ChatDraftStore(defaults: defaults)
        XCTAssertEqual(store.draft("a")?.text, "old draft")
        XCTAssertEqual(store.draft("a")?.attachments, [])
    }
    /// A send that fails puts its cards back before any staged since; one never answered brings them back after a relaunch.
    func testCardsOnTheirWayComeBackWhenTheMessageDoesNot() {
        let defaults = defaults()
        let store = ChatDraftStore(defaults: defaults)
        let token = store.beginSending("look", attachments: [card(1)], for: "a")
        store.setAttachments([card(2)], for: "a")
        XCTAssertEqual(store.attachmentIDs, [card(1).id, card(2).id], "pictures of a card on its way are kept")
        let back = store.returnUnsent("look", attachments: [card(1)], token: token, uncertain: false, for: "a")
        XCTAssertEqual(back?.text, "look")
        XCTAssertEqual(back?.attachments, [card(1), card(2)])
        XCTAssertEqual(store.draft("a")?.sendingAttachments, [])

        let sent = store.beginSending("", attachments: [card(3)], for: "b")
        XCTAssertNotNil(store.draft("b"), "a message of cards alone is held too")
        let relaunched = ChatDraftStore(defaults: defaults).draft("b")?.restored
        XCTAssertEqual(relaunched?.text, "")
        XCTAssertEqual(relaunched?.attachments, [card(3)])
        XCTAssertEqual(relaunched?.uncertain, true)
        store.endSending(for: "b", token: sent)
        XCTAssertNil(store.draft("b"), "answered: nothing kept")
    }
}
