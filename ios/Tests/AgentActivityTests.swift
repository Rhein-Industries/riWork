import XCTest
@testable import RiWorkCore

/// The activity fields of `projects.list`, `shells.list` and `orchestrators.list` are optional and read leniently: a desktop that
/// predates them, or sends something odd, must never make a list fail to load.
final class AgentActivityTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let shell = "44444444-4444-4444-8444-444444444444"

    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T { try JSONDecoder().decode(type, from: Data(json.utf8)) }
    private func project(_ extra: String = "") throws -> RemoteProject {
        try decode(RemoteProject.self, "{\"id\":\"\(project)\",\"name\":\"Alpha\",\"root\":\"/a\",\"created_at\":7\(extra.isEmpty ? "" : ","+extra)}")
    }
    private func session(_ extra: String = "", alive: Bool = true) throws -> RemoteSession {
        try decode(RemoteSession.self, "{\"id\":\"\(shell)\",\"project_id\":\"\(project)\",\"worktree_id\":null,\"kind\":\"project\",\"cwd\":\"/a\",\"harness\":\"claude\",\"alive\":\(alive),\"created_at_unix\":5\(extra.isEmpty ? "" : ","+extra)}")
    }

    // MARK: Projects

    func testAProjectFromAnOlderDesktopHasNoActivity() throws {
        let old = try project()
        XCTAssertNil(old.last_edited_unix)
        XCTAssertNil(old.agents)
        XCTAssertEqual(old.created_at, 7)
    }
    func testAProjectReadsItsEditTimeAndAgentCounts() throws {
        let value = try project("\"last_edited_unix\":1790000123,\"agents\":{\"working\":2,\"waiting\":1}")
        XCTAssertEqual(value.last_edited_unix, 1_790_000_123)
        XCTAssertEqual(value.agents, ProjectAgents(working: 2, waiting: 1))
        XCTAssertEqual(value.agents?.isIdle, false)
    }
    func testABadEditTimeIsIgnoredAndTheProjectStillLoads() throws {
        // Wrong type, negative, zero, not a number at all, too big to be a time, null.
        for bad in ["\"yesterday\"", "\"1790000000\"", "-5", "0", "true", "[1]", "{\"a\":1}", "null", "1e30", "1e400"] {
            XCTAssertNil(try project("\"last_edited_unix\":\(bad)").last_edited_unix, bad)
        }
        // A fraction means the second it starts in.
        XCTAssertEqual(try project("\"last_edited_unix\":1790000000.75").last_edited_unix, 1_790_000_000)
        // The ordinary fields beside a bad one are untouched.
        let odd = try project("\"last_edited_unix\":\"x\"")
        XCTAssertEqual(odd.name, "Alpha")
        XCTAssertEqual(odd.created_at, 7)
    }
    func testBadAgentCountsAreIgnoredOneAtATime() throws {
        XCTAssertEqual(try project("\"agents\":{\"working\":3,\"waiting\":\"two\"}").agents, ProjectAgents(working: 3, waiting: 0))
        XCTAssertEqual(try project("\"agents\":{\"working\":-1,\"waiting\":2}").agents, ProjectAgents(working: 0, waiting: 2))
        XCTAssertEqual(try project("\"agents\":{\"working\":1.5,\"waiting\":null}").agents, ProjectAgents(working: 0, waiting: 0))
        XCTAssertEqual(try project("\"agents\":{\"working\":1e30,\"waiting\":1}").agents, ProjectAgents(working: 0, waiting: 1))
        XCTAssertEqual(try project("\"agents\":{}").agents, ProjectAgents(working: 0, waiting: 0))
        XCTAssertEqual(try project("\"agents\":{\"working\":2,\"extra\":9}").agents, ProjectAgents(working: 2, waiting: 0), "a field this phone does not know is left alone")
        // Not an object: no counts, and no failure.
        for bad in ["\"2\"", "2", "[2,1]", "true", "null"] {
            XCTAssertNil(try project("\"agents\":\(bad)").agents, bad)
        }
    }
    func testAListWithOneOddEntryStillLoads() throws {
        let list = try decode([RemoteProject].self, """
        [{"id":"a","name":"A","root":"/a","created_at":1,"last_edited_unix":10,"agents":{"working":1,"waiting":0}},
         {"id":"b","name":"B","root":"/b","created_at":2,"last_edited_unix":"later","agents":"many"},
         {"id":"c","name":"C","root":"/c","created_at":3}]
        """)
        XCTAssertEqual(list.map(\.last_edited_unix), [10, nil, nil])
        XCTAssertEqual(list.map(\.agents), [ProjectAgents(working: 1, waiting: 0), nil, nil])
    }
    func testTheEntriesOwnFieldsAreStillRequired() {
        XCTAssertThrowsError(try decode(RemoteProject.self, "{\"id\":\"a\",\"root\":\"/a\",\"created_at\":1}"))
        XCTAssertThrowsError(try decode(RemoteProject.self, "{\"id\":\"a\",\"name\":\"A\",\"root\":\"/a\",\"created_at\":\"1\",\"last_edited_unix\":5}"))
        XCTAssertThrowsError(try decode(RemoteSession.self, "{\"id\":\"a\",\"kind\":\"project\",\"cwd\":\"/a\",\"created_at_unix\":5,\"activity\":\"working\"}"))
    }
    func testAProjectSurvivesEncodingAndDecoding() throws {
        let value = try project("\"last_edited_unix\":1790000123,\"agents\":{\"working\":2,\"waiting\":1}")
        let back = try JSONDecoder().decode(RemoteProject.self, from: JSONEncoder().encode(value))
        XCTAssertEqual(back, value)
        let plain = try project()
        XCTAssertEqual(try JSONDecoder().decode(RemoteProject.self, from: JSONEncoder().encode(plain)), plain)
    }
    func testWhatVoiceOverSaysAboutAProjectsAgents() {
        XCTAssertNil(ProjectAgents(working: 0, waiting: 0).spoken)
        XCTAssertTrue(ProjectAgents(working: 0, waiting: 0).isIdle)
        XCTAssertEqual(ProjectAgents(working: 1, waiting: 0).spoken, "1 agent working")
        XCTAssertEqual(ProjectAgents(working: 3, waiting: 0).spoken, "3 agents working")
        XCTAssertEqual(ProjectAgents(working: 0, waiting: 1).spoken, "1 agent waiting for input")
        XCTAssertEqual(ProjectAgents(working: 0, waiting: 2).spoken, "2 agents waiting for input")
        XCTAssertEqual(ProjectAgents(working: 2, waiting: 1).spoken, "2 agents working, 1 waiting for input")
    }

    // MARK: Terminals

    func testATerminalFromAnOlderDesktopHasNoActivity() throws {
        let old = try session()
        XCTAssertEqual(old.activity, .unknown)
        XCTAssertNil(old.activity_since_unix)
        XCTAssertEqual(old.subagents_working, 0)
        XCTAssertNil(old.activitySummary)
        XCTAssertEqual(old.shownActivity, .unknown)
    }
    func testEveryActivityTheDesktopNamesIsRead() throws {
        for (word, expected) in [("working", AgentActivity.working), ("waiting", .waiting), ("done", .done), ("unknown", .unknown), ("exited", .exited)] {
            XCTAssertEqual(try session("\"activity\":\"\(word)\"").activity, expected, word)
        }
        let busy = try session("\"activity\":\"working\",\"activity_since_unix\":1790000000,\"subagents_working\":2")
        XCTAssertEqual(busy.activity_since_unix, 1_790_000_000)
        XCTAssertEqual(busy.subagents_working, 2)
    }
    func testAnActivityThePhoneDoesNotKnowIsUnknown() throws {
        // A word added later, other capitalisation, padding, another type, nothing.
        for bad in ["\"thinking\"", "\"\"", "\"working now\"", "5", "true", "null", "[\"working\"]", "{\"state\":\"working\"}"] {
            XCTAssertEqual(try session("\"activity\":\(bad)").activity, .unknown, bad)
        }
        XCTAssertEqual(try session("\"activity\":\"Working\"").activity, .working)
        XCTAssertEqual(try session("\"activity\":\" waiting \"").activity, .waiting)
    }
    func testBadSubagentCountsAndTimesAreIgnored() throws {
        for bad in ["\"2\"", "-1", "1.5", "true", "null", "[2]", "1e30"] {
            let value = try session("\"activity\":\"working\",\"subagents_working\":\(bad),\"activity_since_unix\":\(bad == "1.5" ? "\"x\"" : bad)")
            XCTAssertEqual(value.subagents_working, 0, bad)
            XCTAssertEqual(value.activity, .working, "the state beside a bad number is kept: \(bad)")
        }
        XCTAssertNil(try session("\"activity_since_unix\":0").activity_since_unix)
        XCTAssertNil(try session("\"activity_since_unix\":-3").activity_since_unix)
        XCTAssertEqual(try session("\"subagents_working\":0").subagents_working, 0)
    }
    func testAListOfTerminalsWithOddEntriesStillLoads() throws {
        let list = try decode([RemoteSession].self, """
        [{"id":"a","kind":"project","cwd":"/a","alive":true,"created_at_unix":1,"activity":"waiting"},
         {"id":"b","kind":"project","cwd":"/b","alive":true,"created_at_unix":2,"activity":42,"subagents_working":"many"},
         {"id":"c","kind":"orchestrator","cwd":"/c","alive":true,"created_at_unix":3}]
        """)
        XCTAssertEqual(list.map(\.activity), [.waiting, .unknown, .unknown])
    }
    func testADeadTerminalShowsNothingWhateverItSaidLast() throws {
        let gone = try session("\"activity\":\"working\",\"subagents_working\":3", alive: false)
        XCTAssertEqual(gone.activity, .working)
        XCTAssertEqual(gone.shownActivity, .exited)
        XCTAssertNil(gone.activitySummary)
        XCTAssertEqual(try session("\"activity\":\"exited\"").shownActivity, .exited)
    }
    func testWhatVoiceOverSaysAboutATerminal() throws {
        XCTAssertEqual(try session("\"activity\":\"working\"").activitySummary, "Working")
        XCTAssertEqual(try session("\"activity\":\"working\",\"subagents_working\":1").activitySummary, "Working, 1 subagent")
        XCTAssertEqual(try session("\"activity\":\"working\",\"subagents_working\":2").activitySummary, "Working, 2 subagents")
        XCTAssertEqual(try session("\"activity\":\"waiting\",\"subagents_working\":2").activitySummary, "Waiting for input")
        XCTAssertEqual(try session("\"activity\":\"done\"").activitySummary, "Done")
        XCTAssertNil(try session("\"activity\":\"unknown\"").activitySummary)
        XCTAssertNil(try session("\"activity\":\"exited\"").activitySummary)
    }
    func testOnlyWorkingWaitingAndDoneAreDrawn() {
        XCTAssertEqual(AgentActivity.allCases.filter(\.isShown), [.working, .waiting, .done])
    }
    func testAnAnswerToShellCreateCarriesTheFieldsToo() throws {
        let created = try decode(JSONValue.self, "{\"shell_id\":\"\(shell)\",\"shell\":{\"id\":\"\(shell)\",\"project_id\":\"\(project)\",\"worktree_id\":null,\"kind\":\"project\",\"cwd\":\"/a\",\"harness\":\"codex\",\"alive\":true,\"created_at_unix\":5,\"activity\":\"working\"}}")
        XCTAssertEqual(try created["shell"].decode(RemoteSession.self).activity, .working)
    }
}
