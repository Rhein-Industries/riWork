import XCTest
@testable import RiWorkCore

final class SharedTabStripTests: XCTestCase {
    private func uuid(_ n: Int) -> String { String(format: "00000000-0000-4000-8000-%012d", n) }
    private func tab(_ kind: String, _ n: Int, _ title: String, order: Int, pinned: Bool = false, hidden: Bool = false, worker: Bool = false,
                     parent: String? = nil, status: String = "working", children: [JSONValue] = []) -> JSONValue {
        .object(["key": .string("\(kind):\(uuid(n))"), "kind": .string(kind), "title": .string(title), "status": .string(status), "pinned": .bool(pinned),
                 "hidden": .bool(hidden), "worker": .bool(worker), "order": .number(Double(order)), "parent": parent.map(JSONValue.string) ?? .null,
                 "children": .array(children), "child_count": .number(Double(children.count))])
    }
    /// A pinned orchestrator, a user chat with a hidden worker shell and an opened worker chat, a user shell, a hidden root chat, a
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
        let user = all["User chat"]!, zsh = all["zsh"]!, pinned = all["Project orchestrator"]!, worker = all["Worker chat"]!
        XCTAssertEqual(SharedTabStrip.move(zsh, onto: user, in: reply), .move(zsh.key, before: user.key))
        XCTAssertNil(SharedTabStrip.move(user, onto: zsh, in: reply), "already right before it")
        XCTAssertEqual(SharedTabStrip.move(user, onto: nil, in: reply), .move(user.key, before: nil), "to the end of its group")
        XCTAssertNil(SharedTabStrip.move(zsh, onto: nil, in: reply), "already last")
        XCTAssertNil(SharedTabStrip.move(zsh, onto: pinned, in: reply), "not into the pinned group")
        XCTAssertNil(SharedTabStrip.move(worker, onto: user, in: reply), "not out of its parent")
        XCTAssertNil(SharedTabStrip.move(zsh, onto: zsh, in: reply))
    }
    func testTheEditSheetGroupsAndItsListMoves() throws {
        let reply = try reply()
        let groups = SharedTabStrip.groups(reply)
        XCTAssertEqual(groups.map(\.title), ["Pinned", "Tabs", "In User chat"])
        XCTAssertEqual(groups[1].tabs.map(\.title), ["User chat", "zsh"])
        let tabs = groups[1].tabs
        XCTAssertEqual(SharedTabStrip.move(in: tabs, from: 1, to: 0), .move(tabs[1].key, before: tabs[0].key))
        XCTAssertEqual(SharedTabStrip.move(in: tabs, from: 0, to: 2), .move(tabs[0].key, before: nil))
        XCTAssertNil(SharedTabStrip.move(in: tabs, from: 0, to: 1), "no move")
        XCTAssertNil(SharedTabStrip.move(in: tabs, from: 5, to: 0))
    }
    func testClosingFollowsTheSettingWorkersDetachAndPinnedTabsMustBeUnpinned() throws {
        let reply = try reply()
        let all = Dictionary(reply.allEntries.map { ($0.title, $0) }, uniquingKeysWith: { a, _ in a })
        for setting in TabCloseBehavior.allCases {
            XCTAssertEqual(SharedTabStrip.closePlan(all["Worker chat"]!, setting: setting), .detach, "a worker never asks and never exits")
            XCTAssertEqual(SharedTabStrip.closePlan(all["Project orchestrator"]!, setting: setting), .unpinFirst)
        }
        XCTAssertEqual(SharedTabStrip.closePlan(all["User chat"]!, setting: .ask), .ask)
        XCTAssertEqual(SharedTabStrip.closePlan(all["User chat"]!, setting: .detach), .detach)
        XCTAssertEqual(SharedTabStrip.closePlan(all["zsh"]!, setting: .exit), .exit)
        XCTAssertEqual(SharedTabStrip.actions(all["Project orchestrator"]!), .init(pin: false, unpin: true, rename: true, close: false))
        XCTAssertEqual(SharedTabStrip.actions(all["zsh"]!), .init(pin: true, unpin: false, rename: true, close: true))
        XCTAssertEqual(SharedTabStrip.actions(all["Worker chat"]!).pin, false, "only top-level tabs are pinned")
    }
}
