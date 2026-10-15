import XCTest
@testable import RiWorkCore

final class SubshellCardsTests: XCTestCase {
    private func session(_ id: String = "shell", activity: String = "working", alive: Bool = true, parent: String? = "parent", project: String = "p", worktree: String? = "w", kind: String = "project") throws -> RemoteSession {
        try JSONValue.object(["id":.string(id),"project_id":.string(project),"worktree_id":worktree.map(JSONValue.string) ?? .null,
            "parent_id":parent.map(JSONValue.string) ?? .null,"kind":.string(kind),"cwd":.string("/fixture"),"harness":.string("claude"),"alive":.bool(alive),
            "created_at_unix":.number(10),"activity_since_unix":.number(20),"activity":.string(activity)]).decode(RemoteSession.self)
    }
    func testRealStatusesUnknownModelAndDoneElapsed() throws {
        for (wire,label,finished) in [("working","Working",false),("waiting","Waiting for input",false),("done","Done",true),("futureState","Unknown",false)] {
            let card = WorkerShellCard(session: try session(activity: wire))
            XCTAssertEqual(card.status,label); XCTAssertEqual(card.finished,finished); XCTAssertNil(card.model)
            XCTAssertEqual(card.elapsed(now: 30),finished ? 10 : 20)
        }
        XCTAssertEqual(WorkerShellCard(session: try session(alive:false)).status,"Exited")
        XCTAssertEqual(WorkerShellCard(session: try session()).elapsed(now:0),0)
    }
    /// The scope is the chat, not the project: a card exists only for a session a marker of this chat names, joined to the list by that
    /// session id. A worker of another chat of the project, an unparented shell, an orchestrator and another project's shell never appear.
    func testChatScopeIsTheMarkersSessionIDsJoinedToTheInventory() throws {
        let sessions = try [session("mine"), session("sibling", activity:"waiting", parent:"other-chat"), session("loose", parent:nil),
                            session("boss", kind:"orchestrator"), session("foreign", project:"other")]
        let inventory = WorkerInventory(shells:sessions, chats:[])
        let said = ChatItem(id:"said", status:.completed, body:.agentMessage("Started a worker"))
        let timeline = WorkerTimeline(items:[said, .workerMarker("w", session:"mine")], inventory:inventory)
        XCTAssertEqual(timeline.cards.map(\.worker.sessionID), ["mine"])
        XCTAssertEqual(timeline.card(for:"w")?.phase, .listed(status:"Working", activity:.working))
        XCTAssertNil(timeline.card(for:"said"))
        // A chat that marked no worker shows none, whatever else the project runs.
        XCTAssertTrue(WorkerTimeline(items:[said], inventory:inventory).cards.isEmpty)
    }
    func testClaudeBackgroundLaunchIsNotClaimedDoneAndOrdinaryToolsAreNotAgents() {
        var item = ChatItem(id:"a",status:.completed,body:.toolCall(server:nil,tool:"Agent",input:.object(["subagent_type":.string("Explore"),"description":.string("Find handler"),"run_in_background":.bool(true)]),output:"Launched"))
        let run = ChatSubagent.of(item,provider:.claude)
        XCTAssertEqual(run?.status,.unknown); XCTAssertEqual(run?.purpose,"Find handler"); XCTAssertEqual(run?.summary,"Launched")
        item.status = .inProgress
        XCTAssertEqual(ChatSubagent.of(item,provider:.claude)?.status,.running)
        XCTAssertNil(ChatSubagent.of(item,provider:.muse))
        item.body = .toolCall(server:nil,tool:"Read",input:.object(["description":.string("subagent")]),output:nil)
        XCTAssertNil(ChatSubagent.of(item,provider:.claude))
    }
    func testCodexChildLifecycleIsIndependentOfCompletedSpawnOperation() {
        let item = { (states: JSONValue) in ChatItem(id:"c",status:.completed,body:.toolCall(server:nil,tool:"collabAgentToolCall",input:.object(["tool":.string("spawnAgent"),"agentsStates":states]),output:nil)) }
        XCTAssertEqual(ChatSubagent.of(item(.null),provider:.codex)?.status,.unknown)
        XCTAssertEqual(ChatSubagent.of(item(.object(["child":.object(["status":.string("running")])])),provider:.codex)?.status,.running)
        let run = ChatSubagent.of(item(.object(["child":.object(["status":.string("completed"),"message":.string("Found")])])),provider:.codex)
        XCTAssertEqual(run?.status,.done); XCTAssertEqual(run?.result,"child · Done\nFound")
        XCTAssertEqual(ChatSubagent.of(item(.object(["child":.object(["status":.string("futureStatus")])])),provider:.codex)?.status,.unknown)
    }
    func testALaterReportUpdatesEarlierCardsOnlyForTheSameChild() {
        let item = { (id: String, state: String) in ChatItem(id:id,status:.completed,body:.toolCall(server:nil,tool:"collabAgentToolCall",input:.object(["agentsStates":.object(["one":.object(["status":.string(state),"message":.string("Found")])])]),output:nil)) }
        let spawn = item("spawn","running"), wait = item("wait","completed")
        let run = ChatSubagent.inTranscript(spawn,items:[spawn,wait],provider:.codex)
        XCTAssertEqual(run?.status,.done); XCTAssertEqual(run?.children.count,1); XCTAssertEqual(run?.result,"one · Done\nFound")
        XCTAssertEqual(ChatSubagent.inTranscript(spawn,items:[spawn,item("interrupt","interrupted")],provider:.codex)?.status,.interrupted)
    }

    func testAdditiveMusePresentationSurvivesWireRoundTripAndUnknownStatus() throws {
        let item = try JSONValue.object(["id":.string("m"),"status":.string("completed"),"body":.object(["type":.string("tool_call"),"tool":.string("reviewer"),"input":.null]),
            "presentation":.object(["subagent":.object(["name":.string("Reviewer"),"purpose":.string("Review"),"status":.string("futureStatus"),"summary":.string("x".repeating(5000))])])]).decode(ChatItem.self)
        XCTAssertEqual(ChatSubagent.of(item,provider:.muse)?.status,.unknown)
        XCTAssertEqual(item.subagent?.summary?.count,4096)
        XCTAssertEqual(try JSONDecoder().decode(ChatItem.self,from:JSONEncoder().encode(item)),item)
        let legacy = ChatItem(id:"m",status:.completed,body:.toolCall(server:nil,tool:"reviewer",input:.object(["objective":.string("delegate")]),output:"done"))
        XCTAssertNil(ChatSubagent.of(legacy,provider:.muse),"a generic tool is insufficient evidence of a run")
    }
}

private extension String { func repeating(_ n: Int) -> String { String(repeating:self,count:n) } }
