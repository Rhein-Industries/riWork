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
    private let stagedAt = Date.now
    private func card(_ n: Int, kind: StagedAttachment.Kind = .image, name: String = "photo.jpg") -> StagedAttachment {
        StagedAttachment(id: "aaaaaaaa-0000-4000-8000-00000000000\(n)", kind: kind, name: name, size: 1000 * n, path: "/Users/me/uploads/\(n)/\(name)", stagedAt: stagedAt)
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

    /// The message is byte for byte what the draft held when paths went into it: the text as typed, a line break unless it ends in one
    /// or a space, then each path on its own line, each ending in a line break.
    func testTheMessageIsTheTextThenEachPathExactlyAsTheDraftWas() {
        let two = [card(1), card(2, kind: .file, name: "notes.pdf")]
        XCTAssertEqual(ChatAttachmentMessage.compose("What is this?", two), "What is this?\n/Users/me/uploads/1/photo.jpg\n/Users/me/uploads/2/notes.pdf\n")
        XCTAssertEqual(ChatAttachmentMessage.compose("", two), "/Users/me/uploads/1/photo.jpg\n/Users/me/uploads/2/notes.pdf\n", "a card alone")
        XCTAssertEqual(ChatAttachmentMessage.compose("line\n", [card(1)]), "line\n/Users/me/uploads/1/photo.jpg\n")
        XCTAssertEqual(ChatAttachmentMessage.compose("ends in a space ", [card(1)]), "ends in a space /Users/me/uploads/1/photo.jpg\n")
        let indented = "    indented first line\n\tand a tab\n\n\n"
        XCTAssertEqual(ChatAttachmentMessage.compose(indented, [card(1)]), indented + "/Users/me/uploads/1/photo.jpg\n", "indentation and blank lines kept")
        XCTAssertEqual(ChatAttachmentMessage.compose(indented, []), indented, "no cards: the text as it is")
    }
    /// A card older than the desktop keeps its file is expired: shown so, and its path is not sent.
    func testAnExpiredCardIsNotSent() {
        let now = Date(timeIntervalSince1970: 2_000_000_000)
        var old = card(1); old.stagedAt = now.addingTimeInterval(-25 * 3600)
        var fresh = card(2, kind: .file, name: "notes.pdf"); fresh.stagedAt = now.addingTimeInterval(-3600)
        XCTAssertTrue(old.isExpired(at: now))
        XCTAssertFalse(fresh.isExpired(at: now))
        var edge = card(3); edge.stagedAt = now.addingTimeInterval(-StagedAttachment.retention)
        XCTAssertTrue(edge.isExpired(at: now), "an hour before the desktop's day is up")
        XCTAssertLessThan(StagedAttachment.retention, 24 * 3600, "never longer than the desktop keeps a file")
        XCTAssertEqual(ChatAttachmentMessage.compose("look", [old, fresh], now: now), "look\n/Users/me/uploads/2/notes.pdf\n")
        XCTAssertEqual(ChatAttachmentMessage.compose("look", [old], now: now), "look", "only expired cards: the text alone")
    }
    func testACardSavedWithoutItsTimeIsTakenForExpired() throws {
        let saved = Data(#"{"id":"aaaaaaaa-0000-4000-8000-000000000001","kind":"image","name":"photo.jpg","size":1000,"path":"/p"}"#.utf8)
        let card = try JSONDecoder().decode(StagedAttachment.self, from: saved)
        XCTAssertTrue(card.isExpired())
        let roundTrip = try JSONDecoder().decode(StagedAttachment.self, from: JSONEncoder().encode(self.card(2)))
        XCTAssertEqual(roundTrip, self.card(2).with(stagedAt: roundTrip.stagedAt))
        XCTAssertFalse(roundTrip.isExpired())
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
    /// A prune that started before a card was staged leaves that card's pictures alone, however late it runs.
    func testAPruneNeverRemovesPicturesWrittenAfterItStarted() {
        let store = images()
        store.save(card(1).id, data: png(width: 10, height: 10))
        let started = Date.now.addingTimeInterval(-60)
        store.prune(keeping: [], writtenBefore: started)
        XCTAssertTrue(FileManager.default.fileExists(atPath: store.thumbnailURL(card(1).id).path), "written after the prune began")
        store.prune(keeping: [])
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.thumbnailURL(card(1).id).path))
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

private extension StagedAttachment {
    func with(stagedAt date: Date) -> StagedAttachment { var copy = self; copy.stagedAt = date; return copy }
}
