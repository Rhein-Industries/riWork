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

    func testCitedFilesJoinTheTurnPagerAndResolveByTheirPath() {
        let path = "/Users/me/out/Bericht März.docx"
        let text = "Done.\n:codex-file-citation{path=\"\(path)\" artifact_kind=\"document\" purpose=\"output\"}\nSee :codex-file-citation{path=\"/Users/me/out/b.pdf\"}."
        XCTAssertEqual(ChatFileCatalog.references(in: text), [.init(path: path), .init(path: "/Users/me/out/b.pdf")])
        XCTAssertEqual(ChatFileCatalog.references(in: ":codex-file-citation{path=\"https://example.com/a.pdf\"}"), [], "a web address is no local file")
        let files = ChatFileCatalog.entries(items: [ChatItem(id: "answer", turnID: "turn", status: .completed, body: .agentMessage(text))])
        XCTAssertEqual(files.map(\.displayName), ["Bericht März.docx", "b.pdf"])
        XCTAssertEqual(ChatFileCatalog.resolve(path: path, files: files)?.source, "Answer")
        // A citation of a file an earlier call of the turn made opens that call's entry.
        let made = ChatItem(id: "make", turnID: "turn", status: .completed, body: .command(command: "pandoc", cwd: nil, output: "", exitCode: 0),
                            presentation: .init(semantics: .init(kind: .shell, produced: [.init(path: path)])))
        let turn = ChatFileCatalog.entries(items: [made, ChatItem(id: "answer", turnID: "turn", status: .completed, body: .agentMessage(text))])
        XCTAssertEqual(ChatFileCatalog.resolve(path: path, files: turn)?.itemID, "make")
    }

    /// An answer with every kind of reference, on lines of their own, inside sentences and at the very start, repeated, with web links,
    /// CRLF line ends and a name that is not ASCII.
    private static let referenceRichAnswer = """
    src/first.swift:3 is where it starts.
    :codex-file-citation{path="/Users/me/out/Bericht März.docx" artifact_kind="document"}
    See `src/chat.swift:13:2` and [model](src/model.rs#L42), [web](https://example.com/a.swift) and [local](file:///tmp/x/notes.md).\r
    Again src/chat.swift:13 and (src/view.swift:120) then :codex-file-citation{path="/tmp/b.pdf"} inline.\r
    - [list](docs/guide.md) item with `Makefile.am:7`
    > quoted lib/quote.rs:9

    ```
    code/in/fence.swift:44
    ```
    Last line names src/first.swift:3 again and src/end.swift:77
    """

    /// The growing read of a streamed answer finds what a full read finds, after every delta, whatever the deltas cut (a path, a line
    /// number, a citation, a CRLF).
    func testGrowingReferencesMatchAFullReadAfterEveryDelta() {
        let answer = Self.referenceRichAnswer
        for step in [1, 2, 5, 11, 29] {
            var growing = ChatGrowingReferences()
            var shown = ""
            var cut = answer.startIndex
            var k = 0
            while cut < answer.endIndex {
                let next = answer.index(cut, offsetBy: [step, 1, step * 2, 3][k % 4], limitedBy: answer.endIndex) ?? answer.endIndex
                shown += answer[cut..<next]; cut = next; k += 1
                XCTAssertEqual(growing.references(in: shown), ChatFileCatalog.references(in: shown), "step \(step), \(shown.count) characters")
            }
            let found = growing.references(in: answer)
            for ref in [ChatPathRef(path: "src/first.swift", line: 3), .init(path: "/Users/me/out/Bericht März.docx"), .init(path: "src/model.rs", line: 42),
                        .init(path: "/tmp/x/notes.md"), .init(path: "src/view.swift", line: 120), .init(path: "src/end.swift", line: 77)] {
                XCTAssertTrue(found.contains(ref), "\(ref) is found")
            }
        }
    }

    /// A text that does not start with what was read (an edit, a shorter text, another message) is read from the start: nothing of the
    /// earlier text stays.
    func testGrowingReferencesForgetTextThatChanged() {
        var growing = ChatGrowingReferences()
        let first = "One src/a.swift:12\nTwo src/b.swift:3\nThree"
        XCTAssertEqual(growing.references(in: first), [.init(path: "src/a.swift", line: 12), .init(path: "src/b.swift", line: 3)])
        let edited = "One src/c.swift:12\nTwo src/b.swift:3\nThree and src/d.swift:4"
        XCTAssertEqual(growing.references(in: edited), ChatFileCatalog.references(in: edited))
        XCTAssertFalse(growing.references(in: edited).contains(.init(path: "src/a.swift", line: 12)))
        let shorter = "One src/c.swift:12\n"
        XCTAssertEqual(growing.references(in: shorter), [.init(path: "src/c.swift", line: 12)])
        XCTAssertEqual(growing.references(in: ""), [])
        XCTAssertEqual(growing.references(in: first), ChatFileCatalog.references(in: first))
    }

    /// The index of a chat whose answer streams in is, after every delta and once the answer is complete, the index a cache that kept
    /// nothing builds; a paragraph whose reference changes as it arrives (`:1` then `:12`) drops the old one.
    func testCatalogOfAStreamedAnswerIsTheCatalogOfAFreshRead() {
        let question = ChatItem(id: "ask", turnID: "turn", status: .completed, body: .userMessage("Where is it?"))
        let cache = ChatFileCatalogCache()
        let answer = Self.referenceRichAnswer + "\n\nAnd last src/tail.swift:1"
        var shown = ""
        var revision: UInt64 = 0
        @discardableResult func check(_ status: ChatItemStatus) -> ChatFileIndex {
            revision += 1
            let items = [question, ChatItem(id: "answer", turnID: "turn", status: status, body: .agentMessage(shown))]
            let kept = cache.index(items: items, revision: revision)
            let fresh = ChatFileCatalogCache().index(items: items, revision: 0)
            XCTAssertEqual(kept.entries, fresh.entries, "\(shown.count) characters, \(status)")
            XCTAssertEqual(kept.markdownReferences, fresh.markdownReferences, "\(shown.count) characters, \(status)")
            return kept
        }
        for character in answer { shown.append(character); check(.inProgress) }
        shown += "2"
        let tail = check(.inProgress).markdownReferences.values.flatMap { $0 }.filter { $0.path == "src/tail.swift" }
        XCTAssertEqual(tail, [.init(path: "src/tail.swift", line: 12)])
        check(.completed)
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
