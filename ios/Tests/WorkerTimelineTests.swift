import XCTest
@testable import RiWorkCore

extension ChatItem {
    /// A worker marker as the desktop's chat host writes it: a `riwork`/`worker` tool call with `presentation.worker`.
    static func workerMarker(_ id: String, session: String, kind: ChatWorker.Kind = .shell, title: String = "Fix polling", notedAt: UInt64 = 1_000,
                             followUp: Bool = false, final: ChatWorker.Final? = nil) -> ChatItem {
        var item = ChatItem(id: id, status: .completed, body: .toolCall(server: "riwork", tool: "worker",
                                                                         input: .object(["session_id": .string(session), "title": .string(title)]), output: final?.tail))
        item.worker = ChatWorker(sessionID: session, kind: kind, title: title, notedAt: notedAt, followUp: followUp, final: final)
        return item
    }
}

final class WorkerTimelineTests: XCTestCase {
    private func decode(_ json: String) throws -> ChatItem { try JSONDecoder().decode(ChatItem.self, from: Data(json.utf8)) }
    private func shell(_ id: String, activity: String = "working", alive: Bool = true) throws -> RemoteSession {
        try JSONValue.object(["id": .string(id), "project_id": .string("p"), "worktree_id": .null, "parent_id": .string("chat"), "kind": .string("project"),
                              "cwd": .string("/fixture"), "harness": .string("claude"), "alive": .bool(alive), "created_at_unix": .number(10),
                              "activity": .string(activity)]).decode(RemoteSession.self)
    }
    private func marker(json worker: String, id: String = "worker:s1") -> String {
        """
        {"id":"\(id)","turn_id":"t1","status":"completed",
         "body":{"type":"tool_call","server":"riwork","tool":"worker","input":{"session_id":"s1","title":"Fix polling"},"output":"ok"},
         "presentation":{"worker":\(worker)}}
        """
    }

    // MARK: Decoding

    func testPresentationWorkerDecodesEveryFieldAndIgnoresUnknownOnes() throws {
        let item = try decode(marker(json: """
            {"session_id":"s1","kind":"chat","title":"Fix polling","harness":"claude","model":"opus","worktree_id":"wt",
             "assignment":"Make the poll adaptive","noted_at":1760400000,"late":true,"follow_up":true,"someday":{"nested":[1,2]},
             "final":{"status":"failed","ended_at":1760400100,"tail":"error: x\\nexit 1","later":"ignored"}}
            """))
        let worker = try XCTUnwrap(item.worker)
        XCTAssertEqual(worker.sessionID, "s1"); XCTAssertEqual(worker.kind, .chat); XCTAssertEqual(worker.title, "Fix polling")
        XCTAssertEqual(worker.harness, "claude"); XCTAssertEqual(worker.model, "opus"); XCTAssertEqual(worker.worktreeID, "wt")
        XCTAssertEqual(worker.assignment, "Make the poll adaptive"); XCTAssertEqual(worker.notedAt, 1_760_400_000)
        XCTAssertTrue(worker.late); XCTAssertTrue(worker.followUp)
        XCTAssertEqual(worker.final, ChatWorker.Final(status: .failed, endedAt: 1_760_400_100, tail: "error: x\nexit 1"))
        XCTAssertEqual(item.id, "worker:s1"); XCTAssertEqual(item.turnID, "t1"); XCTAssertEqual(item.status, .completed)
        XCTAssertEqual(item.body, .toolCall(server: "riwork", tool: "worker", input: .object(["session_id": .string("s1"), "title": .string("Fix polling")]), output: "ok"))
        XCTAssertNil(item.subagent)
        XCTAssertEqual(try JSONDecoder().decode(ChatItem.self, from: JSONEncoder().encode(item)), item, "the phone's own encoding reads back the same")

        // The fields a running worker's marker leaves out.
        let running = try XCTUnwrap(try decode(marker(json: #"{"session_id":"s1","kind":"shell","title":"Fix polling","noted_at":1760400000}"#)).worker)
        XCTAssertNil(running.final); XCTAssertNil(running.assignment); XCTAssertNil(running.harness); XCTAssertNil(running.model)
        XCTAssertFalse(running.late); XCTAssertFalse(running.followUp)
    }

    func testUnknownWordsAreUnknownAndAMalformedWorkerLeavesThePlainToolCall() throws {
        let odd = try XCTUnwrap(try decode(marker(json: #"{"session_id":"s1","kind":"robot","title":"","noted_at":5,"final":{"status":"vanished"}}"#)).worker)
        XCTAssertEqual(odd.kind, .unknown); XCTAssertEqual(odd.title, "Worker")
        XCTAssertEqual(odd.final?.status, .unknown); XCTAssertEqual(odd.final?.status.title, "Status not reported")
        XCTAssertNil(odd.final?.endedAt); XCTAssertEqual(odd.final?.tail, "")

        let toolCall = ChatItemBody.toolCall(server: "riwork", tool: "worker", input: .object(["session_id": .string("s1"), "title": .string("Fix polling")]), output: "ok")
        for malformed in [#""garbage""#, "[]", #"{"session_id":5,"noted_at":1}"#, #"{"session_id":"s1"}"#, #"{"session_id":"","noted_at":1}"#,
                          #"{"noted_at":1}"#, #"{"session_id":"s1","noted_at":"soon"}"#, #"{"session_id":"s1","noted_at":1,"final":"soon"}"#] {
            let item = try decode(marker(json: malformed))
            XCTAssertNil(item.worker, malformed)
            XCTAssertEqual(item.body, toolCall, malformed)
            XCTAssertNil(WorkerTimeline(items: [item], inventory: .unavailable).card(for: item.id), malformed)
        }
    }

    func testAnItemWithoutPresentationWorkerDecodesAsBefore() throws {
        let json = #"{"id":"t","status":"completed","body":{"type":"tool_call","server":"github","tool":"search","input":{"q":"x"},"output":"found"}}"#
        let item = try decode(json)
        XCTAssertEqual(item, ChatItem(id: "t", status: .completed, body: .toolCall(server: "github", tool: "search", input: .object(["q": .string("x")]), output: "found")))
        XCTAssertNil(item.worker)
        let encoded = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(item)) as? [String: Any])
        XCTAssertNil(encoded["presentation"], "nothing is added to an item that had no presentation")

        let subagent = try decode(#"{"id":"m","status":"completed","body":{"type":"tool_call","tool":"reviewer","input":null},"presentation":{"subagent":{"name":"Reviewer","status":"done"}}}"#)
        XCTAssertNil(subagent.worker); XCTAssertEqual(subagent.subagent?.name, "Reviewer")
        let presentation = try XCTUnwrap((JSONSerialization.jsonObject(with: JSONEncoder().encode(subagent)) as? [String: Any])?["presentation"] as? [String: Any])
        XCTAssertEqual(Set(presentation.keys), ["subagent"])
        XCTAssertEqual(try JSONDecoder().decode(ChatItem.self, from: JSONEncoder().encode(subagent)), subagent)
        XCTAssertNil(WorkerTimeline(items: [item, subagent], inventory: .unavailable).card(for: "t"))
    }

    // MARK: Card rules

    func testOnlyAWorkersLatestMarkerIsLiveUnlessAnEarlierOneHasItsFinal() throws {
        let inventory = WorkerInventory(shells: [try shell("a"), try shell("b")], chats: [])
        let items: [ChatItem] = [.workerMarker("worker:a", session: "a"), .workerMarker("worker:b", session: "b"),
                                 ChatItem(id: "said", status: .completed, body: .agentMessage("More work for a")),
                                 .workerMarker("worker:a:2", session: "a", followUp: true)]
        let timeline = WorkerTimeline(items: items, inventory: inventory)
        XCTAssertEqual(timeline.cards.map(\.itemID), ["worker:a", "worker:b", "worker:a:2"])
        XCTAssertEqual(timeline.card(for: "worker:a")?.phase, .continued)
        XCTAssertEqual(timeline.card(for: "worker:a")?.status(now: 1_010), "Continued below")
        XCTAssertEqual(timeline.card(for: "worker:a")?.isLive, false)
        XCTAssertNil(timeline.card(for: "worker:a")?.elapsed(now: 1_010))
        XCTAssertEqual(timeline.card(for: "worker:b")?.phase, .listed(status: "Working", activity: .working), "another worker is not continued")
        XCTAssertEqual(timeline.card(for: "worker:a:2")?.phase, .listed(status: "Working", activity: .working))
        XCTAssertEqual(timeline.card(for: "worker:a:2")?.worker.followUp, true)

        // The first assignment's final snapshot stays what it is, whatever came after.
        var finished = items
        finished[0] = .workerMarker("worker:a", session: "a", final: .init(status: .done, endedAt: 1_060, tail: "ok"))
        XCTAssertEqual(WorkerTimeline(items: finished, inventory: inventory).card(for: "worker:a")?.phase, .finished(.init(status: .done, endedAt: 1_060, tail: "ok")))
    }

    func testAFinalSnapshotIsFinishedWithItsTailAndElapsedTime() {
        let final = ChatWorker.Final(status: .done, endedAt: 1_095, tail: "Compiling\nFinished")
        let card = WorkerTimeline(items: [.workerMarker("w", session: "s", final: final)], inventory: .unavailable).cards[0]
        XCTAssertEqual(card.phase, .finished(final))
        XCTAssertEqual(card.status(now: 9_999), "Done"); XCTAssertEqual(card.elapsed(now: 9_999), 95)
        XCTAssertFalse(card.running(now: 1_001)); XCTAssertFalse(card.isLive)
        XCTAssertEqual(card.worker.final?.tail, "Compiling\nFinished")
        for (status, title) in [(ChatWorker.Final.Status.exited, "Exited"), (.failed, "Failed"), (.stopped, "Stopped"), (.unknown, "Status not reported")] {
            let other = WorkerTimeline(items: [.workerMarker("w", session: "s", final: .init(status: status, endedAt: nil))], inventory: .unavailable).cards[0]
            XCTAssertEqual(other.status(now: 2_000), title); XCTAssertNil(other.elapsed(now: 2_000), "no end time, nothing to count")
        }
    }

    func testALiveShellIsJoinedBySessionIDAndIsStartingOnlyBrieflyWhileUnlisted() throws {
        func card(_ shells: [RemoteSession]?) -> WorkerCard {
            WorkerTimeline(items: [.workerMarker("w", session: "s")], inventory: WorkerInventory(shells: shells, chats: nil)).cards[0]
        }
        // Not listed yet: starting for its first seconds, then ended with nothing more to say.
        let unlisted = card([try shell("someone-else")])
        XCTAssertEqual(unlisted.phase, .unlisted); XCTAssertTrue(unlisted.isLive)
        XCTAssertEqual(unlisted.status(now: 1_010), "Starting"); XCTAssertTrue(unlisted.running(now: 1_010)); XCTAssertEqual(unlisted.elapsed(now: 1_010), 10)
        XCTAssertEqual(unlisted.status(now: 1_029), "Starting")
        XCTAssertEqual(unlisted.status(now: 1_030), "Ended · details unavailable"); XCTAssertFalse(unlisted.running(now: 1_030)); XCTAssertNil(unlisted.elapsed(now: 1_030))
        XCTAssertEqual(unlisted.status(now: 990), "Starting", "a desktop clock ahead of the phone's is not an ended worker"); XCTAssertEqual(unlisted.elapsed(now: 990), 0)

        for (activity, alive, status, running) in [("working", true, "Working", true), ("waiting", true, "Waiting for input", true), ("done", true, "Done", false),
                                                   ("working", false, "Exited", false), ("someNewState", true, "Unknown", true)] {
            let listed = card([try shell("s", activity: activity, alive: alive)])
            XCTAssertEqual(listed.status(now: 1_100), status, activity); XCTAssertEqual(listed.running(now: 1_100), running, activity)
            XCTAssertEqual(listed.elapsed(now: 1_100), 100); XCTAssertTrue(listed.isLive)
        }
        // No list that could have it: neither starting nor ended.
        let unknown = card(nil)
        XCTAssertEqual(unknown.status(now: 1_001), "Status unavailable"); XCTAssertFalse(unknown.running(now: 1_001)); XCTAssertTrue(unknown.isLive)
    }

    func testALiveChatWorkerIsJoinedToTheChatList() {
        func card(_ chats: [ChatInfo]?, shells: [RemoteSession]? = []) -> WorkerCard {
            WorkerTimeline(items: [.workerMarker("w", session: "c", kind: .chat)], inventory: WorkerInventory(shells: shells, chats: chats)).cards[0]
        }
        for (state, status, running) in [(ChatState.running, "Working", true), (.waiting, "Waiting for input", true), (.idle, "Done", false), (.stopped, "Stopped", false),
                                         (.failed("boom"), "Failed", false), (.starting, "Starting", true)] {
            let listed = card([ChatInfo(id: "c", provider: .claude, state: state)])
            XCTAssertEqual(listed.status(now: 1_100), status); XCTAssertEqual(listed.running(now: 1_100), running)
        }
        XCTAssertEqual(card(nil).status(now: 1_001), "Status unavailable", "no chat list, whatever the shells")
        XCTAssertEqual(card([]).status(now: 1_001), "Starting")
        XCTAssertEqual(card([]).status(now: 1_100), "Ended · details unavailable")
    }

    func testTheRunningChipCountsOnlyLiveRunningCards() throws {
        let inventory = WorkerInventory(shells: [try shell("working"), try shell("waiting", activity: "waiting"), try shell("done", activity: "done")], chats: [])
        let items: [ChatItem] = [
            .workerMarker("working", session: "working"),
            .workerMarker("finished", session: "finished", final: .init(status: .done, endedAt: 1_050)),
            .workerMarker("waiting:1", session: "waiting"),
            .workerMarker("ended", session: "gone", notedAt: 900),
            .workerMarker("done", session: "done"),
            .workerMarker("starting", session: "new", notedAt: 1_090),
            .workerMarker("waiting:2", session: "waiting", followUp: true),
        ]
        let timeline = WorkerTimeline(items: items, inventory: inventory)
        XCTAssertEqual(timeline.running(now: 1_100).map(\.itemID), ["working", "starting", "waiting:2"])
        XCTAssertEqual(timeline.running(now: 1_130).map(\.itemID), ["working", "waiting:2"], "starting lasts its first seconds only")
        XCTAssertTrue(timeline.hasLive)
        XCTAssertEqual(WorkerTimeline.chipTitle(running: 3), "3 workers running ↑")
        XCTAssertEqual(WorkerTimeline.chipTitle(running: 1), "1 worker running ↑")
        XCTAssertNil(WorkerTimeline.chipTitle(running: 0))

        // Nothing left that the lists could change: the transcript needs no fresh lists for it.
        let history = WorkerTimeline(items: [items[1], .workerMarker("old", session: "x"), .workerMarker("new", session: "x", final: .init(status: .stopped, endedAt: 1_200))],
                                     inventory: inventory)
        XCTAssertFalse(history.hasLive); XCTAssertTrue(history.running(now: 1_001).isEmpty)
    }

    func testDurations() {
        XCTAssertEqual(WorkerCard.duration(0), "0s"); XCTAssertEqual(WorkerCard.duration(45), "45s")
        XCTAssertEqual(WorkerCard.duration(187), "3m 07s"); XCTAssertEqual(WorkerCard.duration(7_500), "2h 05m")
    }
}
