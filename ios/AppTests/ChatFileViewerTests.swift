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
        guard files.count == 5 else { return }
        XCTAssertEqual(files[2].line, 13)
        XCTAssertEqual(files[2].textStartLine, 13)
        XCTAssertEqual(files[2].availableText, "let a = 1")
        XCTAssertTrue(files[2].isExcerpt, "a transported excerpt must not claim to be the full host file")
        XCTAssertEqual(files[0].bytes, 123)
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

    func testOwnedSnapshotHasOneFileAndRecoversOnlyTheAttachmentSummary() {
        let path = "/host/attachments/photo.png"
        let attachment = ChatSubmittedAttachment(id: "owned", name: "photo.png", path: path,
            kind: .object(["kind": .string("image")]), bytes: 2048,
            preview: .object(["kind": .string("image"), "path": .string(path)]))
        let typed = "Please review this image.\nThe path \(path) is part of my message."
        let logged = typed + "\n[Attached: photo.png (2048 bytes)]"
        let item = ChatItem(id: "user", turnID: "one", status: .completed, body: .userMessage(logged),
            presentation: .init(images: [.init(label: "photo.png", source: .local(path: path))], attachments: [attachment]))
        let files = ChatFileCatalog.entries(items: [item])
        XCTAssertEqual(files.count, 1)
        XCTAssertEqual(files.first?.attachment, true)
        XCTAssertEqual(ChatFileCatalog.entries(for: [.init(itemID: "user", imageIndex: 0)], in: files).count, 1)
        XCTAssertEqual(ChatFileCatalog.messageText(logged, files: files, attachments: [attachment]), typed)
        XCTAssertEqual(ChatFileCatalog.messageText(typed, files: files, attachments: [attachment]), typed)
    }

}
