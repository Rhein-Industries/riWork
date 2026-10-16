import XCTest
import UIKit
import SwiftUI
import RiWorkCore
@testable import RiWorkRemote

@MainActor final class ChatFileViewerTests: XCTestCase {
    func testPagerPutsUserAttachmentsBeforeAgentFilesAndKeepsTurnsSeparate() {
        let items = [
            ChatItem(id: "read", turnID: "one", status: .completed, body: .toolCall(server: nil, tool: "read", input: .null, output: "let a = 1"),
                     presentation: .init(semantics: .init(kind: .read, target: "src/main.swift", range: .init(start: 13)))),
            ChatItem(id: "user", turnID: "one", status: .completed, body: .userMessage("Look at these"), presentation: .init(attachments: [
                .init(id: "one", name: "screen.png", path: "/host/inbox/screen.png", kind: .object(["kind": .string("image")]), bytes: 123),
                .init(id: "two", name: "notes.txt", path: "/host/inbox/notes.txt", bytes: 456)
            ])),
            ChatItem(id: "output", turnID: "one", status: .completed, body: .command(command: "make", cwd: nil, output: "", exitCode: 0),
                     presentation: .init(semantics: .init(kind: .shell, produced: [.init(path: "report.md")]))),
            ChatItem(id: "next", turnID: "two", status: .completed, body: .fileChange([.init(path: "next.swift", kind: .modify)]))
        ]
        let files = ChatFileCatalog.entries(items: items)
        XCTAssertEqual(files.map(\.path), ["/host/inbox/screen.png", "/host/inbox/notes.txt", "src/main.swift", "report.md", "next.swift"])
        XCTAssertEqual(files.filter { $0.turnID == "one" }.count, 4)
        XCTAssertEqual(files[2].line, 13)
        XCTAssertEqual(files[2].textStartLine, 13)
        XCTAssertEqual(files[2].availableText, "let a = 1")
        XCTAssertTrue(files[2].isExcerpt, "a transported excerpt must not claim to be the full host file")
        XCTAssertEqual(files[0].bytes, 123)
    }

    func testCatalogUsesOnlyRecordedProducedFilesAndResolvesImageArtifacts() {
        let item = ChatItem(id: "tool", turnID: "turn", status: .completed, body: .command(command: "echo report.md", cwd: nil, output: "unrecorded.png", exitCode: 0),
                            presentation: .init(images: [.init(label: "screen.png", source: .data(mime: "image/png", base64: ""))],
                                                semantics: .init(kind: .shell, produced: [.init(path: "recorded.md", line: 3)])))
        let files = ChatFileCatalog.entries(items: [item])
        XCTAssertEqual(files.count, 2)
        XCTAssertFalse(files.contains { $0.path == "unrecorded.png" })
        let refs = [ChatArtifactRef(itemID: "tool", path: .init(path: "recorded.md", line: 3)), .init(itemID: "tool", imageIndex: 0)]
        XCTAssertEqual(ChatFileCatalog.entries(for: refs, in: files).map(\.path), ["recorded.md", "screen.png"])
        XCTAssertTrue(files[1].isImage)
    }

    func testFileReferencesPreserveSourceLineAndIgnoreWebLinks() {
        let refs = ChatFileCatalog.references(in: "See `src/chat.swift:13:2` and [model](src/model.rs#L42), [web](https://example.com/a.swift).")
        XCTAssertTrue(refs.contains(.init(path: "src/chat.swift", line: 13)))
        XCTAssertTrue(refs.contains(.init(path: "src/model.rs", line: 42)))
        XCTAssertFalse(refs.contains { $0.path.hasPrefix("http") })
        let item = ChatItem(id: "answer", turnID: "turn", status: .completed, body: .agentMessage("[Code](src/chat.swift:13)"))
        let files = ChatFileCatalog.entries(items: [item])
        XCTAssertEqual(files.first?.line, 13)
        XCTAssertEqual(ChatFileCatalog.resolve(path: "chat.swift", line: 17, files: files)?.line, 17)
    }

    func testLegacySentPathsBecomeAttachmentsAndLeaveTheMessageReadable() {
        let text = "Please review these.\n/host/inbox/photo.png\n/host/inbox/notes.pdf\n"
        let item = ChatItem(id: "user", turnID: "turn", status: .completed, body: .userMessage(text))
        let files = ChatFileCatalog.entries(items: [item])
        XCTAssertEqual(files.count, 2)
        XCTAssertEqual(files.filter(\.isImage).count, 1)
        XCTAssertEqual(ChatFileCatalog.messageText(text, files: files), "Please review these.")
        let afterSpace = "Please review /host/inbox/photo.png\n/host/inbox/notes.pdf\n"
        let inline = ChatItem(id: "inline", turnID: "turn", status: .completed, body: .userMessage(afterSpace))
        let inlineFiles = ChatFileCatalog.entries(items: [inline])
        XCTAssertEqual(inlineFiles.count, 2)
        XCTAssertEqual(inlineFiles.map(\.path), ["/host/inbox/photo.png", "/host/inbox/notes.pdf"])
        XCTAssertEqual(ChatFileCatalog.messageText(afterSpace, files: inlineFiles), "Please review")
    }

    func testSentImageCanUseExistingLocalPreviewWithoutNewMediaTransport() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("viewer-cache-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let images = ChatAttachmentImages(directory: directory)
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: 16, height: 32))
        let data = renderer.pngData { context in UIColor.red.setFill(); context.fill(CGRect(x: 0, y: 0, width: 16, height: 32)) }
        let id = "aaaaaaaa-1111-4111-8111-111111111111", path = "/host/inbox/screen-random.png"
        XCTAssertTrue(images.save(id, data: data))
        images.rememberRemotePath(path, for: id)
        images.remove(id)
        let item = ChatItem(id: "user", turnID: "turn", status: .completed, body: .userMessage(path))
        let file = try XCTUnwrap(ChatFileCatalog.entries(items: [item], images: images).first)
        guard case .local(let local) = file.imageSource else { return XCTFail("the sent image uses the existing local preview") }
        XCTAssertTrue(FileManager.default.fileExists(atPath: local))
        XCTAssertNotEqual(local, path)
        XCTAssertNotNil(UIImage(contentsOfFile: local))
    }

    func testLegacyItemsDoNotShareAPagerWithoutAKnownTurn() {
        let items = [
            ChatItem(id: "first", status: .completed, body: .fileChange([.init(path: "first.swift", kind: .modify)])),
            ChatItem(id: "second", status: .completed, body: .fileChange([.init(path: "second.swift", kind: .modify)]))
        ]
        XCTAssertEqual(ChatFileCatalog.entries(items: items).map(\.turnID), ["legacy:first", "legacy:second"])
    }

    func testAnswerLinksReuseEarlierFileInTheTurnPager() {
        let items = [
            ChatItem(id: "edit", turnID: "one", status: .completed, body: .fileChange([.init(path: "src/main.swift", kind: .modify)])),
            ChatItem(id: "answer", turnID: "one", status: .completed, body: .agentMessage("Updated [main.swift](main.swift)."))
        ]
        let files = ChatFileCatalog.entries(items: items)
        XCTAssertEqual(files.count, 1)
        XCTAssertEqual(files.first?.path, "src/main.swift")
        XCTAssertEqual(ChatFileCatalog.resolve(path: "main.swift", files: files)?.itemID, "edit")
    }

    func testOwnedImageAndPresentationImageAreOneAttachmentAndOneArtifact() {
        let path = "/host/attachments/photo.png"
        let item = ChatItem(id: "user", turnID: "one", status: .completed, body: .userMessage("Look"), presentation: .init(
            images: [.init(label: "photo.png", source: .local(path: path))], attachments: [
                .init(id: "owned", name: "photo.png", path: path, kind: .object(["kind": .string("image")]), bytes: 2048,
                      preview: .object(["kind": .string("image"), "path": .string(path)]))
            ]))
        let files = ChatFileCatalog.entries(items: [item])
        XCTAssertEqual(files.count, 1)
        XCTAssertEqual(files.first?.bytes, 2048)
        XCTAssertEqual(files.first?.imageIndex, 0)
        XCTAssertEqual(ChatFileCatalog.entries(for: [.init(itemID: "user", imageIndex: 0)], in: files).count, 1)
    }

    func testToolImagesUseRecordedNamesAndOnePagerEntry() throws {
        let jpeg = ChatImageSource.data(mime: "image/jpeg", base64: "")
        let png = ChatImageSource.data(mime: "image/png", base64: "")
        let cases: [(ChatItem, String, String?)] = [
            (.init(id: "toolu_target", turnID: "one", status: .completed,
                   body: .toolCall(server: nil, tool: "Read", input: .null, output: "[image]"),
                   presentation: .init(images: [.init(label: "image:toolu_target:0", source: jpeg)], semantics: .init(kind: .read, target: "/tmp/r05surf.jpg"))),
             "r05surf.jpg", "Read tmp/r05surf.jpg"),
            (.init(id: "toolu_input", status: .completed,
                   body: .toolCall(server: nil, tool: "Read", input: .object(["file_path": .string("/tmp/screen.png")]), output: "[image]"),
                   presentation: .init(images: [.init(label: "Image", source: png)])), "screen.png", nil),
            (.init(id: "toolu_unknown", status: .completed,
                   body: .toolCall(server: "simulator", tool: "capture", input: .null, output: "[image]"),
                   presentation: .init(images: [.init(label: "image:toolu_unknown:0", source: png)])), "Image 1.png", "simulator · capture")
        ]
        for (item, name, source) in cases {
            let files = ChatFileCatalog.entries(items: [item])
            XCTAssertEqual(files.count, 1, "the target and returned image share one pager entry")
            let file = try XCTUnwrap(files.first)
            XCTAssertEqual(file.displayName, name)
            if let source { XCTAssertEqual(file.source, source) }
            XCTAssertEqual(file.imageIndex, 0)
            XCTAssertFalse(file.path.contains("toolu_"))
            XCTAssertFalse(file.displayName.contains("toolu_"))
            XCTAssertEqual(ChatFileCatalog.entries(for: [.init(itemID: item.id, imageIndex: 0)], in: files).count, 1)
            guard case .data = file.imageSource else { return XCTFail("the entry retains its transported image bytes") }
        }
    }

    func testOutputCleanupPreservesProgressAndExcerptCopy() {
        let cases = [
            ("10%\r50%\r100%\ndone", "100%\ndone"),
            ("a\r\nb", "a\nb"), ("a\tb", "a    b"),
            ("alpha\rbeta\r\r\n\r\n\tcode\tvalue\n", "beta\n\n    code    value\n"),
            ("\n\n", "\n\n"),
            ("\u{001B}[32m10%\u{001B}[0m\r50%\r100%\ndone\n", "100%\ndone\n")
        ]
        for (input, expected) in cases { XCTAssertEqual(ChatContainerText.clean(input), expected) }
        let progress = (0..<40).map { "\($0)%" }.joined(separator: "\r") + "\ndone\n"
        XCTAssertEqual(ChatContainerText.outputSnapshot(progress).lineCount, 2, "progress updates occupy one display line")
        let output = "  12→    let a = 1\n13\tlet b = 2\nplain text\n"
        let lines = ChatContainerText.excerptSnapshot(output, start: 70).lines
        XCTAssertEqual(lines.map(\.number), [12, 13, 72])
        XCTAssertEqual(lines.map(\.text), ["    let a = 1", "let b = 2", "plain text"])
        XCTAssertEqual(ChatContainerText.excerptCopy(output), "    let a = 1\nlet b = 2\nplain text\n")
        XCTAssertNil(ChatContainerText.excerptSnapshot("unindexed", start: nil).lines.first?.number)
        let numbered = ChatContainerText.excerptSnapshot("12\told\r13\t\tlet\tvalue = 2\n", start: nil).lines
        XCTAssertEqual(numbered, [.init(number: 13, text: "    let    value = 2")])
        XCTAssertEqual(ChatContainerText.excerptCopy("13\t\tlet\tvalue = 2\n"), "    let    value = 2\n")
    }

    func testBoundedOutputSnapshotsKeepUnicodeSourceIndicesAndAccurateOmissions() {
        let source = (0..<600).map { index in
            "old\r\u{001B}[32mline \(index)\u{001B}[0m\t" + String(repeating: "🙂界", count: 90)
        }.joined(separator: "\n") + "\n"
        let clean = ChatContainerText.clean(source)
        let expected = ChatContainerText.lines(clean)
        for (running, expanded) in [(false, false), (false, true), (true, false)] {
            let snapshot = ChatContainerText.outputSnapshot(source, running: running, expanded: expanded)
            let shown = snapshot.output.filter { !$0.2 }
            XCTAssertEqual(snapshot.lineCount, expected.count)
            XCTAssertEqual(snapshot.omitted, expected.count - shown.count)
            XCTAssertLessThanOrEqual(shown.map { $0.1 }.joined(separator: "\n").utf8.count, 32_768)
            XCTAssertTrue(snapshot.bounded)
            XCTAssertFalse(shown.contains { $0.1.contains("�") })
            for (index, line, _) in shown { XCTAssertEqual(line, expected[index]) }
            if running { XCTAssertEqual(shown.map { $0.0 }, [597, 598, 599]) }
            if expanded { XCTAssertEqual(shown.last?.0, 599); XCTAssertLessThan(shown.count, 200) }
        }
        let collapsed = ChatContainerText.outputSnapshot(source)
        XCTAssertEqual(collapsed.output.filter { $0.2 }.first?.1, "… 585 lines omitted")
        let oneHidden = ChatContainerText.outputSnapshot((0..<16).map { "line \($0)" }.joined(separator: "\n"))
        XCTAssertEqual(oneHidden.output.first { $0.2 }?.1, "… 1 line omitted")
        for text in ["", "\n", "\n\n", "one\n\n", "one\r\ntwo\n"] {
            let expected = ChatContainerText.lines(ChatContainerText.clean(text))
            let snapshot = ChatContainerText.outputSnapshot(text, expanded: true)
            XCTAssertEqual(snapshot.lineCount, expected.count)
            XCTAssertEqual(snapshot.output.map { $0.1 }, expected)
            XCTAssertEqual(snapshot.omitted, 0)
        }
        let line = String(repeating: "🙂界", count: 10_000)
        for expanded in [false, true] {
            let snapshot = ChatContainerText.outputSnapshot(line, expanded: expanded)
            let shown = snapshot.output.filter { !$0.2 }.map { $0.1 }.joined()
            XCTAssertLessThanOrEqual(shown.utf8.count, 32_768)
            XCTAssertFalse(shown.contains("�"))
            XCTAssertTrue(snapshot.byteTruncated)
            XCTAssertEqual(snapshot.lineCount, 1)
            XCTAssertEqual(snapshot.omitted, 0)
            XCTAssertTrue(expanded ? line.hasSuffix(shown) : line.hasPrefix(shown))
        }
        let excerptSource = "7\t\t" + line + "\n" + (8..<400).map { "\($0)→value" }.joined(separator: "\n") + "\n"
        let snapshot = ChatContainerText.excerptSnapshot(excerptSource, start: 70, expanded: true)
        XCTAssertEqual(snapshot.lineCount, 393)
        XCTAssertEqual(snapshot.lines.first?.number, 7)
        XCTAssertTrue(snapshot.lines.first?.text.hasPrefix("    🙂界") == true)
        XCTAssertLessThanOrEqual(snapshot.lines.map(\.text).joined(separator: "\n").utf8.count, 32_768)
        XCTAssertTrue(snapshot.byteTruncated)
        XCTAssertFalse(snapshot.lines.contains { $0.text.contains("�") })
        let copy = ChatContainerText.excerptCopy(excerptSource)
        XCTAssertTrue(copy.hasPrefix("    " + line + "\nvalue\n"))
        XCTAssertTrue(copy.hasSuffix("value\n"))
        XCTAssertGreaterThan(copy.utf8.count, 32_768)
    }

    func testFlattenedArtifactFooterSharesTheHeaderAndInlineImageRules() {
        func file(_ id: String, image: Bool = true) -> ChatFileEntry {
            .init(id: id, itemID: "read", path: id, name: id + (image ? ".png" : ".txt"), source: "Read")
        }
        let first = file("first"), second = file("second"), document = file("document", image: false)
        let artifacts = [first, second, document]
        var row = ChatProjectionRow(id: "read", kind: .read)
        XCTAssertEqual(ChatActivityRows.stripArtifacts(row: row, artifacts: artifacts, owned: [first]).map(\.id), [second.id, document.id])
        row.open = true
        XCTAssertEqual(ChatActivityRows.stripArtifacts(row: row, artifacts: artifacts, owned: [first, document]).map(\.id), [second.id, document.id])
        row.kind = .activity
        XCTAssertEqual(ChatActivityRows.stripArtifacts(row: row, artifacts: artifacts, owned: artifacts).map(\.id), artifacts.map(\.id))
    }

    func testFullImagePreviewPreservesSixKPixelsWhileThumbnailDecodeStaysBounded() async throws {
        let format = UIGraphicsImageRendererFormat(); format.scale = 1
        let data = UIGraphicsImageRenderer(size: CGSize(width: 6144, height: 128), format: format).pngData { context in
            UIColor.red.setFill(); context.fill(CGRect(x: 0, y: 0, width: 6144, height: 128))
        }
        let decoded = await Task.detached { (ChatFileBytes.decode(data, maxPixelSize: nil), ChatFileBytes.decode(data)) }.value
        let full = try XCTUnwrap(decoded.0), thumbnail = try XCTUnwrap(decoded.1)
        XCTAssertEqual(full.cgImage?.width, 6144)
        XCTAssertEqual(full.cgImage?.height, 128)
        XCTAssertEqual(thumbnail.cgImage?.width, 2048)
    }

    func testPagingToTextIgnoresAnEarlierImageDecodeThatCompletesAfterCancellation() async throws {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene") }
        let image = ChatFileEntry(id: "slow-image", itemID: "image", path: "image.png", name: "image.png", source: "Read",
                                  imageSource: .data(mime: "image/png", base64: ToolUIPerformanceFixture.imageBase64))
        let text = ChatFileEntry(id: "text", itemID: "text", path: "notes.txt", name: "notes.txt", source: "Read", availableText: "Current text preview")
        let layout = ChatLayoutInspection()
        var continuation: CheckedContinuation<UIImage?, Never>?
        let viewer = ChatFileViewer(files: [image, text], initialID: image.id, decodePreview: { _ in
            await withCheckedContinuation { continuation = $0 }
        })
        let host = UIHostingController(rootView: viewer.environment(\.chatLayoutInspection, layout))
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: 402, height: 874)
        window.rootViewController = host; window.makeKeyAndVisible()
        defer { continuation?.resume(returning: nil); window.isHidden = true }
        func waitUntil(_ predicate: () -> Bool) async throws {
            let deadline = Date().addingTimeInterval(3)
            while !predicate(), Date() < deadline { host.view.layoutIfNeeded(); try await Task.sleep(for: .milliseconds(10)) }
            XCTAssertTrue(predicate())
        }
        try await waitUntil { continuation != nil && layout.actions["file-next"] != nil }
        try XCTUnwrap(layout.actions["file-next"])()
        try await waitUntil { layout.frames["file-preview-text"] != nil }
        let pending = try XCTUnwrap(continuation); continuation = nil
        pending.resume(returning: UIImage(systemName: "photo"))
        try await Task.sleep(for: .milliseconds(100)); host.view.layoutIfNeeded()
        XCTAssertNotNil(layout.frames["file-preview-text"], "a canceled decode must not replace the current text page")
        XCTAssertNil(layout.frames["file-preview-image"])
    }

    func testSearchLocationsCanOpenFilesWithoutExtensions() {
        let item = ChatItem(id: "search", turnID: "one", status: .completed,
                            body: .toolCall(server: nil, tool: "search", input: .null, output: "Makefile:7:test:\n.gitignore:3:build/"),
                            presentation: .init(semantics: .init(kind: .search, target: ".")))
        let files = ChatFileCatalog.entries(items: [item])
        XCTAssertEqual(files.map(\.path), ["Makefile", ".gitignore"])
        XCTAssertEqual(files.map(\.line), [7, 3])
    }

    func testCatalogCacheRebuildsOnlyTheChangedItemAndTurnAndIndexesParagraphReferences() {
        let cache = ChatFileCatalogCache()
        let edit = ChatItem(id: "edit", turnID: "one", status: .completed,
                            body: .fileChange([.init(path: "src/main.swift", kind: .modify)]))
        let command = ChatItem(id: "command", turnID: "one", status: .completed,
                               body: .command(command: "swift test", cwd: "/project", output: "ok", exitCode: 0))
        var answer = ChatItem(id: "answer", turnID: "one", status: .inProgress,
                              body: .agentMessage("See [main](src/main.swift#L13)."))
        let previous = ChatItem(id: "previous", turnID: "older", status: .completed,
                                body: .agentMessage("Earlier [notes](notes.md)."))
        let initial = cache.index(items: [previous, edit, command, answer], revision: 1)
        XCTAssertEqual(cache.rebuiltItemCount, 4)
        XCTAssertEqual(cache.rebuiltTurnCount, 2)
        XCTAssertEqual(initial.byTurn["one"]?.count, 2)
        XCTAssertEqual(initial.byItem["edit"]?.first?.path, "src/main.swift")
        XCTAssertEqual(initial.cwdByTurn["one"], "/project")
        XCTAssertEqual(initial.references(in: "See [main](src/main.swift#L13).", files: initial.byTurn["one"] ?? []).first?.line, 13)
        _ = cache.index(items: [previous, edit, command, answer], revision: 1)
        XCTAssertEqual(cache.rebuiltItemCount, 4, "disclosure and keyboard renders reuse the same catalog")
        XCTAssertEqual(cache.rebuiltTurnCount, 2)
        answer.body = .agentMessage("See [main](src/main.swift#L17).")
        let streamed = cache.index(items: [previous, edit, command, answer], revision: 2)
        XCTAssertEqual(cache.rebuiltItemCount, 5, "a token must not parse historical items again")
        XCTAssertEqual(cache.rebuiltTurnCount, 3, "only the streaming turn is rebuilt")
        XCTAssertEqual(streamed.byTurn["older"], initial.byTurn["older"])
        XCTAssertEqual(streamed.references(in: "See [main](src/main.swift#L17).", files: streamed.byTurn["one"] ?? []).first?.line, 17)
        XCTAssertNil(streamed.markdownReferences["See [main](src/main.swift#L13)."])
    }

    func testFileBytesReadOnlyOnDeviceCacheAndRejectHostPathsAndEscapingSymlinks() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("viewer-bytes-\(UUID().uuidString)")
        let outside = FileManager.default.temporaryDirectory.appendingPathComponent("host-file-\(UUID().uuidString).swift")
        defer { try? FileManager.default.removeItem(at: directory); try? FileManager.default.removeItem(at: outside) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try Data("host bytes".utf8).write(to: outside)
        let images = ChatAttachmentImages(directory: directory)
        let path = "/project/src/code.swift"
        let item = ChatItem(id: "file", turnID: "turn", status: .completed, body: .fileChange([.init(path: path, kind: .modify)]))
        let file = try XCTUnwrap(ChatFileCatalog.entries(items: [item], images: images).first)
        let missing = await ChatFileBytes.data(for: file)
        XCTAssertNil(missing)
        let cached = ChatFileBytes.cachedFileURL(path: path, images: images)
        try Data("transported bytes".utf8).write(to: cached)
        let loaded = await ChatFileBytes.data(for: file)
        XCTAssertEqual(loaded, Data("transported bytes".utf8))
        var host = file
        host.path = outside.path; host.localPreviewPaths = []; host.imageSource = .local(path: outside.path)
        let forbidden = await ChatFileBytes.data(for: host)
        XCTAssertNil(forbidden, "a matching host path on the simulator is not transported media")
        let link = directory.appendingPathComponent("escape.swift")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: outside)
        host.imageSource = .local(path: link.path)
        let escaped = await ChatFileBytes.data(for: host)
        XCTAssertNil(escaped)
    }

    func testSentPreviewAliasRoundTripLoadsBytesAfterDraftPreviewIsRemoved() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("viewer-alias-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let images = ChatAttachmentImages(directory: directory)
        let format = UIGraphicsImageRendererFormat(); format.scale = 1
        let renderer = UIGraphicsImageRenderer(size: CGSize(width: 10, height: 20), format: format)
        let data = renderer.pngData { context in UIColor.blue.setFill(); context.fill(CGRect(x: 0, y: 0, width: 10, height: 20)) }
        let id = "bbbbbbbb-1111-4111-8111-111111111111", path = "/host/inbox/owned.png"
        XCTAssertTrue(images.save(id, data: data))
        images.rememberRemotePath(path, for: id)
        images.remove(id)
        let item = ChatItem(id: "sent", turnID: "turn", status: .completed, body: .userMessage(path))
        let file = try XCTUnwrap(ChatFileCatalog.entries(items: [item], images: images).first)
        let loaded = await ChatFileBytes.image(for: file)
        XCTAssertEqual(loaded?.size.width, 10)
        XCTAssertEqual(loaded?.size.height, 20)
        XCTAssertEqual(file.source, "Your message")
    }

    func testCanonicalAnswerLocationUsesEarlierFilesCachedBytes() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("viewer-canonical-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let images = ChatAttachmentImages(directory: directory)
        let path = "src/main.swift"
        let edit = ChatItem(id: "edit", turnID: "turn", status: .completed, body: .fileChange([.init(path: path, kind: .modify)]))
        let answer = ChatItem(id: "answer", turnID: "turn", status: .completed, body: .agentMessage("Updated [main](main.swift#L13)."))
        let file = try XCTUnwrap(ChatFileCatalog.entries(items: [edit, answer], images: images).first { $0.itemID == "answer" })
        XCTAssertEqual(file.path, path)
        XCTAssertEqual(file.line, 13)
        let data = Data("cached full file".utf8)
        try data.write(to: ChatFileBytes.cachedFileURL(path: path, images: images))
        let loaded = await ChatFileBytes.data(for: file)
        XCTAssertEqual(loaded, data)
    }

}

extension ChatFileViewerTests {
    func testPlainStreamingProseKeepsTheFileEnvironmentIdentityUntilAReferenceArrives() {
        let cache = ChatFileCatalogCache()
        var answer = ChatItem(id: "answer", turnID: "turn", body: .agentMessage("Thinking"))
        let initial = cache.index(items: [answer], revision: 1)
        answer.body = .agentMessage("Thinking about the next token")
        let streamed = cache.index(items: [answer], revision: 2)
        XCTAssertEqual(initial, streamed, "plain tokens must not invalidate every Markdown/file view")
        answer.body = .agentMessage("See [result](src/result.swift#L4).")
        let linked = cache.index(items: [answer], revision: 3)
        XCTAssertNotEqual(streamed, linked, "new file references still reach existing message views")
        XCTAssertEqual(linked.markdownReferences["See [result](src/result.swift#L4)."]?.first?.line, 4)
    }
}

extension ChatFileViewerTests {
    func testRepeatedImageReferencesReuseTheDecodedBitmap() async throws {
        let source = ChatImageSource.data(mime: "image/png", base64: ToolUIPerformanceFixture.imageBase64)
        let first = ChatItem(id: "image-a", body: .toolCall(server: nil, tool: "image", input: .null, output: nil),
                             presentation: .init(images: [.init(label: "a.png", source: source)]))
        let second = ChatItem(id: "image-b", body: first.body, presentation: .init(images: [.init(label: "b.png", source: source)]))
        let files = ChatFileCatalog.entries(items: [first, second])
        let decodedA = await ChatFileBytes.image(for: files[0])
        let decodedB = await ChatFileBytes.image(for: files[1])
        let a = try XCTUnwrap(decodedA)
        let b = try XCTUnwrap(decodedB)
        XCTAssertTrue(a === b, "image identity follows byte source, independently of call/turn IDs")
        XCTAssertEqual(a.size, CGSize(width: 64, height: 64))
    }
}
