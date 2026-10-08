import XCTest
@testable import RiWorkCore

final class SharedTabStripTests: XCTestCase {
    private func uuid(_ n: Int) -> String { String(format: "00000000-0000-4000-8000-%012d", n) }
    private func tab(_ kind: String, _ n: Int, _ title: String, order: Int, pinned: Bool? = nil, hidden: Bool = false, worker: Bool = false,
                     parent: String? = nil, status: String = "working", children: [JSONValue] = []) -> JSONValue {
        // `pinned` is what an older desktop still sends; the phone does not read it.
        var fields: [String: JSONValue] = ["key": .string("\(kind):\(uuid(n))"), "kind": .string(kind), "title": .string(title), "status": .string(status),
                 "hidden": .bool(hidden), "worker": .bool(worker), "order": .number(Double(order)), "parent": parent.map(JSONValue.string) ?? .null,
                 "children": .array(children), "child_count": .number(Double(children.count))]
        if let pinned { fields["pinned"] = .bool(pinned) }
        return .object(fields)
    }
    /// An orchestrator an older desktop still calls pinned, a user chat with a hidden worker shell and an opened worker chat, a user shell, a hidden root chat, a
    /// stopped hidden shell.
    private func reply() throws -> SharedTabsReply {
        let parent = "chat:\(uuid(2))"
        let entries: [JSONValue] = [
            tab("chat", 1, "Project orchestrator", order: 0, pinned: true),
            tab("chat", 2, "User chat", order: 1, children: [
                tab("shell", 3, "Shell · main", order: 2, hidden: true, worker: true, parent: parent),
                tab("chat", 4, "Worker chat", order: 3, worker: true, parent: parent)]),
            tab("shell", 5, "zsh", order: 4),
            tab("chat", 6, "Old chat", order: 5, hidden: true, status: "stopped"),
            tab("shell", 7, "Dead shell", order: 6, hidden: true, status: "stopped")]
        return try JSONValue.object(["revision": .number(3), "entries": .array(entries)]).decode(SharedTabsReply.self)
    }

    func testTheRowIsTheVisibleEntriesInSharedOrderWithoutHiddenWorkers() throws {
        let reply = try reply()
        XCTAssertEqual(reply.visible.map(\.title), ["Project orchestrator", "User chat", "Worker chat", "zsh"], "an opened worker is shown; a hidden one is not")
        XCTAssertEqual(SharedTabStrip.entry(chatID: uuid(2), sessionID: nil, in: reply)?.title, "User chat")
        XCTAssertEqual(SharedTabStrip.entry(chatID: nil, sessionID: uuid(5), in: reply)?.kind, .shell)
        XCTAssertNil(SharedTabStrip.entry(chatID: uuid(99), sessionID: nil, in: reply))
        XCTAssertNil(SharedTabStrip.entry(chatID: uuid(1), sessionID: nil, in: nil), "no shared list: the old strip")
    }
    func testThePickerListsHiddenChatsAndLiveShellsWithTheirParent() throws {
        let rows = SharedTabStrip.openable(try reply())
        XCTAssertEqual(rows.map(\.tab.title), ["Shell · main", "Old chat"], "workers first; a stopped shell cannot be opened, a stopped chat can")
        XCTAssertEqual(rows.first?.parentTitle, "User chat")
        XCTAssertNil(rows.last?.parentTitle)
        XCTAssertTrue(SharedTabStrip.openable(nil).isEmpty)
    }
    func testMovesStayInTheirGroupAndSayNothingWhenNothingMoves() throws {
        let reply = try reply()
        let all = Dictionary(reply.allEntries.map { ($0.title, $0) }, uniquingKeysWith: { a, _ in a })
        let user = all["User chat"]!, zsh = all["zsh"]!, orchestrator = all["Project orchestrator"]!, worker = all["Worker chat"]!
        XCTAssertEqual(SharedTabStrip.move(zsh, onto: user, in: reply), .move(zsh.key, before: user.key))
        XCTAssertNil(SharedTabStrip.move(user, onto: zsh, in: reply), "already right before it")
        XCTAssertEqual(SharedTabStrip.move(user, onto: nil, in: reply), .move(user.key, before: nil), "to the end of its group")
        XCTAssertEqual(SharedTabStrip.move(zsh, onto: nil, in: reply), .move(zsh.key, before: nil), "hidden tabs follow it in its group: a real move")
        XCTAssertEqual(SharedTabStrip.move(zsh, onto: orchestrator, in: reply), .move(zsh.key, before: orchestrator.key), "any top-level tab goes anywhere")
        XCTAssertEqual(SharedTabStrip.move(orchestrator, onto: nil, in: reply), .move(orchestrator.key, before: nil), "the orchestrator moves like any tab")
        XCTAssertNil(SharedTabStrip.move(worker, onto: user, in: reply), "not out of its parent")
        XCTAssertNil(SharedTabStrip.move(zsh, onto: zsh, in: reply))
    }
    func testHiddenMembersOfAGroupCountForMovesAndUnknownKindsAreNotEdited() throws {
        // Shared order [A, B, hidden H] and [A, hidden H, B].
        func reply(_ entries: [JSONValue]) throws -> SharedTabsReply { try JSONValue.object(["revision": .number(1), "entries": .array(entries)]).decode(SharedTabsReply.self) }
        let end = try reply([tab("chat", 1, "A", order: 0), tab("chat", 2, "B", order: 1), tab("chat", 3, "H", order: 2, hidden: true)])
        let b = end.allEntries.first { $0.title == "B" }!
        XCTAssertEqual(SharedTabStrip.move(b, onto: nil, in: end), .move(b.key, before: nil), "past the hidden H: a real move")
        let middle = try reply([tab("chat", 1, "A", order: 0), tab("chat", 3, "H", order: 1, hidden: true), tab("chat", 2, "B", order: 2)])
        let a = middle.allEntries.first { $0.title == "A" }!, b2 = middle.allEntries.first { $0.title == "B" }!
        XCTAssertEqual(SharedTabStrip.move(a, onto: b2, in: middle), .move(a.key, before: b2.key), "H sits between them in the shared order")
        let unknown = try reply([tab("chat", 1, "A", order: 0), tab("future", 4, "Future", order: 1)])
        XCTAssertEqual(SharedTabStrip.groups(unknown).flatMap(\.tabs).map(\.title), ["A"], "an unknown kind is not reordered from here")
    }
    func testMoveLeftAndRightStepAmongVisibleSiblingsAndStopAtTheGroupsEdge() throws {
        // [P, A, B, hidden H, C] in one group (P was pinned on an older desktop).
        let reply = try JSONValue.object(["revision": .number(1), "entries": .array([
            tab("chat", 9, "P", order: 0, pinned: true), tab("chat", 1, "A", order: 1), tab("chat", 2, "B", order: 2),
            tab("chat", 3, "H", order: 3, hidden: true), tab("chat", 4, "C", order: 4)])]).decode(SharedTabsReply.self)
        let all = Dictionary(reply.allEntries.map { ($0.title, $0) }, uniquingKeysWith: { a, _ in a })
        XCTAssertEqual(SharedTabStrip.moveLeft(all["A"]!, in: reply), .move(all["A"]!.key, before: all["P"]!.key), "past P: no pin group")
        XCTAssertEqual(SharedTabStrip.moveLeft(all["B"]!, in: reply), .move(all["B"]!.key, before: all["A"]!.key))
        XCTAssertEqual(SharedTabStrip.moveRight(all["B"]!, in: reply), .move(all["B"]!.key, before: nil), "past C: to the end")
        XCTAssertEqual(SharedTabStrip.moveRight(all["A"]!, in: reply), .move(all["A"]!.key, before: all["H"]!.key), "past B, before what follows it")
        XCTAssertNil(SharedTabStrip.moveRight(all["C"]!, in: reply), "the right edge")
        XCTAssertNil(SharedTabStrip.moveLeft(all["P"]!, in: reply), "the left edge")
        XCTAssertEqual(SharedTabStrip.moveRight(all["P"]!, in: reply), .move(all["P"]!.key, before: all["B"]!.key))
    }
    func testTheEditSheetGroupsAndItsListMoves() throws {
        let reply = try reply()
        let groups = SharedTabStrip.groups(reply)
        XCTAssertEqual(groups.map(\.title), ["Tabs", "In User chat"])
        XCTAssertEqual(groups[0].tabs.map(\.title), ["Project orchestrator", "User chat", "zsh"])
        let tabs = groups[0].tabs
        XCTAssertEqual(SharedTabStrip.move(in: tabs, from: 1, to: 0), .move(tabs[1].key, before: tabs[0].key))
        XCTAssertEqual(SharedTabStrip.move(in: tabs, from: 0, to: 3), .move(tabs[0].key, before: nil))
        XCTAssertNil(SharedTabStrip.move(in: tabs, from: 0, to: 1), "no move")
        XCTAssertNil(SharedTabStrip.move(in: tabs, from: 5, to: 0))
    }
    func testClosingFollowsTheSettingWorkersDetachAndTheOrchestratorIsAnOrdinaryTab() throws {
        let reply = try reply()
        let all = Dictionary(reply.allEntries.map { ($0.title, $0) }, uniquingKeysWith: { a, _ in a })
        for setting in TabCloseBehavior.allCases {
            XCTAssertEqual(SharedTabStrip.closePlan(all["Worker chat"]!, setting: setting), .detach, "a worker never asks and never exits")
            XCTAssertEqual(SharedTabStrip.closePlan(all["Project orchestrator"]!, setting: setting), SharedTabStrip.closePlan(all["User chat"]!, setting: setting))
        }
        XCTAssertEqual(SharedTabStrip.closePlan(all["Project orchestrator"]!, setting: .ask), .ask, "the same Detach / Exit sheet")
        XCTAssertEqual(SharedTabStrip.closePlan(all["User chat"]!, setting: .detach), .detach)
        XCTAssertEqual(SharedTabStrip.closePlan(all["zsh"]!, setting: .exit), .exit)
        for title in ["Project orchestrator", "zsh", "Worker chat"] { XCTAssertEqual(SharedTabStrip.actions(all[title]!), .init(rename: true, close: true)) }
        // A tab the phone cannot end (an orchestrator in a terminal): Ask and Exit are the sheet with Detach only; Detach is Detach.
        XCTAssertEqual(SharedTabStrip.closePlan(all["zsh"]!, setting: .ask, exitable: false), .detachOnly)
        XCTAssertEqual(SharedTabStrip.closePlan(all["zsh"]!, setting: .exit, exitable: false), .detachOnly)
        XCTAssertEqual(SharedTabStrip.closePlan(all["zsh"]!, setting: .detach, exitable: false), .detach)
        XCTAssertEqual(SharedTabStrip.closePlan(all["Worker chat"]!, setting: .exit, exitable: false), .detach, "a worker still just detaches")
    }
    func testEnteringAProjectRestoresItsLastTabWhileTheListStillHasIt() throws {
        let reply = try reply()
        let all = Dictionary(reply.allEntries.map { ($0.title, $0) }, uniquingKeysWith: { a, _ in a })
        let any: (SharedTab) -> Bool = { _ in true }
        XCTAssertEqual(SharedTabStrip.restoredTab(remembered: all["zsh"]!.key, in: reply, opens: any)?.title, "zsh")
        XCTAssertEqual(SharedTabStrip.restoredTab(remembered: all["Worker chat"]!.key, in: reply, opens: any)?.title, "Worker chat")
        XCTAssertEqual(SharedTabStrip.restoredTab(remembered: all["Old chat"]!.key, in: reply, opens: any)?.title, "Project orchestrator", "hidden: the first tab")
        XCTAssertEqual(SharedTabStrip.restoredTab(remembered: "chat:\(uuid(99))", in: reply, opens: any)?.title, "Project orchestrator", "gone: the first tab")
        XCTAssertEqual(SharedTabStrip.restoredTab(remembered: nil, in: reply, opens: any)?.title, "Project orchestrator")
        XCTAssertEqual(SharedTabStrip.restoredTab(remembered: all["zsh"]!.key, in: reply, opens: { $0.kind == .chat })?.title, "Project orchestrator",
                       "a tab the phone cannot open now is passed over")
        XCTAssertNil(SharedTabStrip.restoredTab(remembered: nil, in: reply, opens: { _ in false }))
    }
}
