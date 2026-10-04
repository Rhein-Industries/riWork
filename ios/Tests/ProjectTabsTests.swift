import XCTest
@testable import RiWorkCore

/// An orchestrator the Mac runs as a chat: the optional `mode`, `chat_id` and `provider` of `shells.list` / `orchestrators.list` entries
/// are read leniently (a desktop that predates them, or sends something odd, still gives a list), and the strip decides from them what
/// each tab opens.
final class ProjectTabsTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let entryID = "aaaaaaaa-1111-4111-8111-111111111111"
    private let chatID = "cccccccc-1111-4111-8111-111111111111"
    private let shellID = "44444444-4444-4444-8444-444444444444"

    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T { try JSONDecoder().decode(type, from: Data(json.utf8)) }
    private func entry(id: String? = nil, kind: String = "orchestrator", alive: Bool = true, at: Int = 5, _ extra: String = "") throws -> RemoteSession {
        try decode(RemoteSession.self, "{\"id\":\"\(id ?? entryID)\",\"project_id\":\"\(project)\",\"worktree_id\":null,\"kind\":\"\(kind)\",\"cwd\":\"/a\",\"harness\":null,\"alive\":\(alive),\"created_at_unix\":\(at)\(extra.isEmpty ? "" : "," + extra)}")
    }
    private func chatEntry(id: String? = nil, chat: String? = nil, provider: String = "claude", alive: Bool = true, _ extra: String = "") throws -> RemoteSession {
        try entry(id: id, alive: alive, "\"mode\":\"chat\",\"chat_id\":\"\(chat ?? chatID)\",\"provider\":\"\(provider)\"\(extra.isEmpty ? "" : "," + extra)")
    }
    private func chat(_ id: String, at: UInt64 = 10, title: String = "", state: ChatState = .idle) -> ChatInfo {
        ChatInfo(id: id, provider: .codex, projectID: project, cwd: "/a", title: title, createdAtUnix: at, state: state)
    }

    // MARK: Reading the fields

    func testAnEntryFromAnOlderDesktopIsATerminal() throws {
        let old = try entry()
        XCTAssertEqual(old.mode, .terminal)
        XCTAssertNil(old.chat_id)
        XCTAssertNil(old.provider)
        XCTAssertEqual(old.opening(chatsAvailable: true), .terminal)
        XCTAssertEqual(old.opening(chatsAvailable: false), .terminal)
        XCTAssertEqual(old.title, "Project orchestrator")
    }
    func testAChatOrchestratorCarriesItsChatAndAgent() throws {
        let value = try chatEntry()
        XCTAssertEqual(value.mode, .chat)
        XCTAssertEqual(value.chat_id, chatID, "the chat's id, which is not the entry's")
        XCTAssertNotEqual(value.chat_id, value.id)
        XCTAssertEqual(value.provider, .claude)
        XCTAssertEqual(try chatEntry(provider: "codex").provider, .codex)
        XCTAssertEqual(value.title, "Project orchestrator", "an orchestrator keeps its name")
        XCTAssertEqual(value.opening(chatsAvailable: true), .chat(chatID))
    }
    func testAnExplicitTerminalModeIsATerminalAndItsChatFieldsAreNotUsed() throws {
        let value = try entry("\"mode\":\"terminal\",\"chat_id\":\"\(chatID)\",\"provider\":\"codex\"")
        XCTAssertEqual(value.mode, .terminal)
        XCTAssertEqual(value.opening(chatsAvailable: true), .terminal, "only `mode` says it is a chat")
    }
    func testAnUnknownOrOddModeMeansTerminal() throws {
        for bad in ["\"voice\"", "\"\"", "\"chats\"", "7", "true", "null", "[\"chat\"]", "{\"mode\":\"chat\"}"] {
            let value = try entry("\"mode\":\(bad),\"chat_id\":\"\(chatID)\"")
            XCTAssertEqual(value.mode, .terminal, bad)
            XCTAssertEqual(value.opening(chatsAvailable: true), .terminal, bad)
        }
        // The word is read the way the activity words are: case and spaces do not matter.
        XCTAssertEqual(try entry("\"mode\":\" Chat \",\"chat_id\":\"\(chatID)\"").mode, .chat)
    }
    func testABadChatIdOrProviderIsIgnoredOnItsOwnAndTheEntryStillLoads() throws {
        for bad in ["\"not-a-uuid\"", "\"\"", "7", "true", "null", "[1]", "{\"id\":1}", "\"cccccccc-1111-4111-8111-11111111111\"", "\" cccccccc-1111-4111-8111-111111111111\""] {
            let value = try entry("\"mode\":\"chat\",\"chat_id\":\(bad),\"provider\":\"codex\"")
            XCTAssertNil(value.chat_id, bad)
            XCTAssertEqual(value.provider, .codex, "the field beside a bad one is kept: \(bad)")
            XCTAssertEqual(value.id, entryID)
            XCTAssertEqual(value.opening(chatsAvailable: true), .notReady, "it says it is a chat and not which: there is nothing to open (\(bad))")
        }
        for bad in ["\"gemini\"", "\"\"", "3", "null", "{}", "[\"claude\"]"] {
            let value = try entry("\"mode\":\"chat\",\"chat_id\":\"\(chatID)\",\"provider\":\(bad)")
            XCTAssertNil(value.provider, bad)
            XCTAssertEqual(value.chat_id, chatID, "the field beside a bad one is kept: \(bad)")
            XCTAssertEqual(value.opening(chatsAvailable: true), .chat(chatID), "which agent it is comes from the chat itself (\(bad))")
        }
        XCTAssertEqual(try entry("\"mode\":\"chat\",\"provider\":\" Claude \"").provider, .claude, "case and spaces do not matter")
    }
    func testAnUppercaseChatIdIsKeptInTheFormEveryChatRequestNeeds() throws {
        let value = try entry("\"mode\":\"chat\",\"chat_id\":\"\(chatID.uppercased())\"")
        XCTAssertEqual(value.chat_id, chatID)
        XCTAssertNoThrow(try ChatEventsRequest(chatID: try XCTUnwrap(value.chat_id), since: 0, waitMilliseconds: 0))
    }
    func testAListWithOddEntriesStillLoads() throws {
        let list = try decode([RemoteSession].self, """
        [{"id":"a","kind":"orchestrator","cwd":"/a","alive":true,"created_at_unix":1},
         {"id":"b","kind":"orchestrator","cwd":"/b","alive":true,"created_at_unix":2,"mode":"chat","chat_id":"\(chatID)","provider":"codex"},
         {"id":"c","kind":"orchestrator","cwd":"/c","alive":true,"created_at_unix":3,"mode":9,"chat_id":false,"provider":[]}]
        """)
        XCTAssertEqual(list.map(\.mode), [.terminal, .chat, .terminal])
        XCTAssertEqual(list.map(\.chat_id), [nil, chatID, nil])
    }
    func testTheFieldsCarryThroughAnAnswerToShellCreateAndSurviveEncoding() throws {
        let created = try decode(JSONValue.self, "{\"shell\":{\"id\":\"\(shellID)\",\"project_id\":\"\(project)\",\"kind\":\"project\",\"cwd\":\"/a\",\"harness\":null,\"alive\":true,\"created_at_unix\":5,\"mode\":\"chat\",\"chat_id\":\"\(chatID)\",\"provider\":\"codex\"}}")
        let value = try created["shell"].decode(RemoteSession.self)
        XCTAssertEqual(value.chat_id, chatID)
        let again = try JSONDecoder().decode(RemoteSession.self, from: JSONEncoder().encode(value))
        XCTAssertEqual(again, value)
        XCTAssertEqual(value.title, "Codex chat", "a chat that is not an orchestrator is called by its agent")
    }

    // MARK: What opening means

    func testAChatOrchestratorOnADesktopWithoutChatsOpensNeitherAChatNorATerminal() throws {
        let value = try chatEntry()
        XCTAssertEqual(value.opening(chatsAvailable: false), .needsUpdate)
        XCTAssertEqual(value.opening(chatsAvailable: false).notice?.headline, "Update the Mac to open this orchestrator")
        XCTAssertNil(SessionOpening.terminal.notice)
        XCTAssertNil(SessionOpening.chat(chatID).notice)
        XCTAssertNotNil(SessionOpening.notReady.notice)
        // Without a chat id as well, the missing chat support is what is said.
        XCTAssertEqual(try entry("\"mode\":\"chat\"").opening(chatsAvailable: false), .needsUpdate)
        XCTAssertEqual(try entry("\"mode\":\"chat\"").opening(chatsAvailable: true), .notReady)
    }

    // MARK: The strip

    private func tabs(_ sessions: [RemoteSession], chats: [ChatInfo] = [], available: Bool = true, missing: Set<String> = []) -> [ProjectTab] {
        ProjectTabs.tabs(sessions: sessions, chats: chats, chatsAvailable: available, missing: missing)
    }
    func testTerminalsAreTerminalTabsAsBefore() throws {
        let manager = try entry()
        let shell = try entry(id: shellID, kind: "project", at: 9)
        let strip = tabs([manager, shell])
        XCTAssertEqual(strip, [.terminal(manager), .terminal(shell)])
        XCTAssertEqual(strip.map(\.id), [entryID, shellID])
        XCTAssertNil(strip[0].chatInfo)
        XCTAssertEqual(strip[0].session, manager)
    }
    func testATerminalThatIsNotAliveOrIsGoneIsNotATab() throws {
        let dead = try entry(alive: false)
        let gone = try entry(id: shellID, kind: "project")
        XCTAssertEqual(tabs([dead, gone], missing: [shellID]), [])
    }
    func testAChatOrchestratorOpensItsChatByChatIdNotByItsOwnId() throws {
        let value = try chatEntry()
        let strip = tabs([value])
        XCTAssertEqual(strip.count, 1)
        guard case .orchestratorChat(let session, let info) = strip[0] else { return XCTFail("a chat tab, not a terminal: \(strip[0])") }
        XCTAssertEqual(session, value)
        XCTAssertEqual(info.id, chatID, "every chat request uses the chat's id")
        XCTAssertNotEqual(info.id, value.id)
        XCTAssertEqual(strip[0].id, chatID)
        XCTAssertEqual(strip[0].chatInfo?.id, chatID)
        XCTAssertEqual(info.provider, .claude, "the glyph")
        XCTAssertEqual(ChatTabs.title(info), "Project orchestrator", "the tab shows the orchestrator's label")
        XCTAssertEqual(ChatTabs.detail(info, branch: "main"), "Claude chat · main")
        XCTAssertEqual(info.projectID, project)
        XCTAssertEqual(info.cwd, "/a")
        XCTAssertEqual(try tabs([chatEntry(provider: "codex")])[0].chatInfo?.provider, .codex)
    }
    func testTheEntryAndItsChatMayShareAnId() throws {
        let value = try chatEntry(id: chatID)
        XCTAssertEqual(tabs([value]).map(\.id), [chatID])
        XCTAssertEqual(tabs([value])[0].chatInfo?.id, chatID)
    }
    func testAChatOrchestratorIsNotHeldToAlive() throws {
        // A chat at rest is still a chat, and its next message starts its agent again.
        XCTAssertEqual(tabs([try chatEntry(alive: false)]).compactMap(\.chatInfo?.id), [chatID])
        XCTAssertEqual(tabs([try chatEntry()], missing: [entryID]).compactMap(\.chatInfo?.id), [chatID], "`missing` is about terminals the desktop could not find")
    }
    func testWhatTheEntryDoesShowsOnTheTab() throws {
        func state(_ activity: String) throws -> ChatState? { try tabs([chatEntry("\"activity\":\"\(activity)\"")])[0].chatInfo?.state }
        XCTAssertEqual(try state("working"), .running)
        XCTAssertEqual(try state("waiting"), .waiting)
        XCTAssertEqual(try state("done"), .idle)
        XCTAssertEqual(try state("unknown"), .idle)
        XCTAssertEqual(try state("working")?.activity, .working, "the indicator the terminal tabs have")
        XCTAssertEqual(try state("waiting")?.activity, .waiting)
        XCTAssertEqual(try tabs([chatEntry()])[0].chatInfo?.state.activity, .unknown, "an entry that says nothing draws nothing")
    }
    func testTheListedChatIsTheOrchestratorsChatAndIsNotASecondTab() throws {
        let value = try chatEntry()
        let listed = chat(chatID, title: "Some title", state: .waiting)
        let other = chat("cccccccc-2222-4222-8222-222222222222", at: 20)
        let strip = tabs([value], chats: [listed, other])
        XCTAssertEqual(strip.count, 2, "one tab for the orchestrator, one for the other chat")
        guard case .orchestratorChat(_, let info) = strip[0] else { return XCTFail("the orchestrator first") }
        XCTAssertEqual(info.state, .waiting, "what the desktop says about the chat itself")
        XCTAssertEqual(info.provider, .codex)
        XCTAssertEqual(info.title, "Project orchestrator", "and still called by the orchestrator's name")
        XCTAssertEqual(strip[1], .chat(other))
        XCTAssertEqual(strip.map(\.id), [chatID, other.id], "no id twice")
    }
    func testAChatOrchestratorOnADesktopWithoutChatsIsANoticeNeverATerminal() throws {
        let value = try chatEntry()
        let shell = try entry(id: shellID, kind: "project", at: 9)
        let strip = tabs([value, shell], available: false)
        XCTAssertEqual(strip, [.unavailable(value, .needsUpdate), .terminal(shell)])
        XCTAssertEqual(strip[0].id, entryID)
        XCTAssertNil(strip[0].chatInfo)
        XCTAssertEqual(strip[0].session, value)
        // Nor one that did not say which chat it is.
        let unready = try entry("\"mode\":\"chat\"")
        XCTAssertEqual(tabs([unready]), [.unavailable(unready, .notReady)])
    }
    func testTheOrderIsTheEntriesThenTheOtherChatsNewestFirst() throws {
        let manager = try chatEntry()
        let shell = try entry(id: shellID, kind: "project", at: 9)
        let old = chat("cccccccc-2222-4222-8222-222222222222", at: 1)
        let new = chat("cccccccc-3333-4333-8333-333333333333", at: 99)
        XCTAssertEqual(tabs([manager, shell], chats: [old, new]).map(\.id), [chatID, shellID, new.id, old.id])
    }
    func testTwoEntriesForOneChatAreOneTab() throws {
        let first = try chatEntry(id: entryID)
        let second = try chatEntry(id: "aaaaaaaa-2222-4222-8222-222222222222")
        XCTAssertEqual(tabs([first, second]).count, 1)
    }
    func testAShellThatIsAChatIsAChatTabToo() throws {
        let value = try entry(id: shellID, kind: "project", "\"mode\":\"chat\",\"chat_id\":\"\(chatID)\",\"provider\":\"codex\"")
        let strip = tabs([value])
        XCTAssertEqual(strip.compactMap(\.chatInfo?.id), [chatID])
        XCTAssertEqual(strip[0].chatInfo?.title, "Codex chat")
    }
}
