import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// The chat screen as the person meets it: hosted in the real tab screen, with a scripted desktop behind it. What is checked is what
/// the keyboard does (⏎, ⇧⏎, ⎋, ⌘⌫, ⌘.), that the terminal's key view is not on a chat screen, and that the screen draws.
///
/// A simulator cannot attach a hardware keyboard, so a key is delivered the way UIKit delivers one: as the `UIKeyCommand` the composer
/// declared for it, run through its action.
@MainActor final class ChatViewTests: XCTestCase {
    private let project = ChatTransport.project
    private let chatID = "cccccccc-1111-4111-8111-111111111111"
    private var defaultsNames: [String] = []
    private var windows: [UIWindow] = []

    override func setUp() async throws { executionTimeAllowance = 60 }
    override func tearDown() async throws {
        for window in windows { window.isHidden = true }
        windows = []
        for name in defaultsNames { UserDefaults().removePersistentDomain(forName: name) }
        defaultsNames = []
    }

    private func chat(state: ChatState = .idle, mode: ChatApprovalMode = .supervised) -> ChatInfo {
        ChatInfo(id: chatID, provider: .claude, projectID: project, cwd: "/fixture", title: "Fix the build", createdAtUnix: 10, approvalMode: mode, state: state)
    }
    private func descendants<T: UIView>(_ type: T.Type, in view: UIView) -> [T] {
        view.subviews.compactMap { $0 as? T } + view.subviews.flatMap { descendants(type, in: $0) }
    }
    private func eventually(_ what: String, timeout: Double = 5, file: StaticString = #filePath, line: UInt = #line, _ condition: () async -> Bool) async {
        let end = Date().addingTimeInterval(timeout)
        while await !condition(), Date() < end { try? await Task.sleep(for: .milliseconds(10)) }
        let met = await condition()
        XCTAssertTrue(met, what, file: file, line: line)
    }

    private struct Rig {
        let model: RemoteModel, transport: ChatTransport, window: UIWindow, host: UIHostingController<AnyView>, keychain: KeychainStore
    }
    private func makeRig(chats: [ChatInfo]? = nil, orchestrators: [String] = [], chatFeature: Bool = true, orchestratorCreate: Bool = false, hardwareKeyboard: Bool = true, width: CGFloat = 402, height: CGFloat = 874) async throws -> Rig {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene to show a chat in") }
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = ChatTransport.shell
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        let suite = "com.riwork.tests.chatview.\(UUID().uuidString)"
        defaultsNames.append(suite)
        let transport = ChatTransport(chats: chats ?? [chat()])
        await transport.setFeature(chatFeature)
        await transport.setOrchestratorFeature(orchestratorCreate)
        await transport.setOrchestrators(orchestrators)
        let model = RemoteModel(client: transport, keychain: keychain, defaults: UserDefaults(suiteName: suite)!, chatWaitMilliseconds: 300, chatIdleInterval: .milliseconds(20),
                                hardwareKeyboard: HardwareKeyboardMonitor(probe: { hardwareKeyboard }))
        await model.connect()
        let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))
        let root = AnyView(TerminalTabsView(model: model, project: projectValue, onBack: {}).desktopThemed(model.theme.style))
        let host = UIHostingController(rootView: root)
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: width, height: height)
        window.rootViewController = host
        window.makeKeyAndVisible()
        windows.append(window)
        await eventually("the terminal is on screen") { !self.descendants(KeyCaptureView.self, in: host.view).isEmpty && model.terminalArea != nil }
        return Rig(model: model, transport: transport, window: window, host: host, keychain: keychain)
    }
    private func finish(_ rig: Rig) async {
        rig.window.endEditing(true)
        await rig.model.disconnect()
        rig.window.isHidden = true
        try? rig.keychain.delete()
    }
    private func composer(_ rig: Rig) -> ChatComposerTextView? { descendants(ChatComposerTextView.self, in: rig.host.view).first }
    private func openChat(_ rig: Rig) async throws -> ChatComposerTextView {
        rig.model.selectChat(chatID)
        await eventually("the chat screen is up") { self.composer(rig) != nil && rig.model.conversation(self.chatID).following }
        return try XCTUnwrap(composer(rig))
    }
    private func command(_ view: ChatComposerTextView, _ input: String, _ flags: UIKeyModifierFlags = []) -> UIKeyCommand? {
        view.keyCommands?.first { $0.input == input && $0.modifierFlags == flags }
    }
    private func press(_ view: ChatComposerTextView, _ input: String, _ flags: UIKeyModifierFlags = [], file: StaticString = #filePath, line: UInt = #line) {
        guard let found = command(view, input, flags) else { return XCTFail("the composer does not claim \(input.debugDescription) \(flags.rawValue)", file: file, line: line) }
        view.fired(found)
    }
    private func approval(_ id: String, _ choices: [ChatDecision] = [.accept, .acceptForSession, .decline, .cancel]) -> ChatEvent {
        .approvalRequested(ChatApproval(requestID: id, kind: .command, title: "rm -rf build", choices: choices))
    }
    /// A host that resolves what it is told about.
    private func resolving(_ transport: ChatTransport) async {
        await transport.handleCommands { _, command in
            if case .approve(let id, let decision) = command { return [.approvalResolved(requestID: id, decision: decision)] }
            return []
        }
    }
    private func sentCommands(_ rig: Rig) async -> [JSONValue] { await rig.transport.commands() }

    // MARK: The terminal's key view is not on a chat screen

    func testAChatTakesTheScreenFromTheTerminalAndItsKeyViewGoesWithIt() async throws {
        let rig = try await makeRig()
        XCTAssertFalse(descendants(KeyCaptureView.self, in: rig.host.view).isEmpty)
        XCTAssertTrue(rig.model.terminalVisible)
        _ = try await openChat(rig)
        XCTAssertTrue(descendants(KeyCaptureView.self, in: rig.host.view).isEmpty, "KeyCapture holds ⌘K ⌘, ⌘/, the hotkeys and the Clicks template; none of it is on a chat screen")
        XCTAssertFalse(rig.model.terminalVisible, "and the terminal's long poll and pinned size are released")
        XCTAssertTrue(rig.model.chatIsOnScreen)
        // A terminal tab brings the terminal back, and takes the chat's composer away.
        let shell = try XCTUnwrap(rig.model.openSessions.first)
        await rig.model.chooseSession(shell)
        await eventually("the terminal is back") { !self.descendants(KeyCaptureView.self, in: rig.host.view).isEmpty && self.composer(rig) == nil }
        XCTAssertTrue(rig.model.terminalVisible)
        XCTAssertFalse(rig.model.conversation(chatID).following, "nothing follows a chat that is not on screen")
        await finish(rig)
    }
    func testTheChatKeysAreNotAnyOfTheTerminalsShortcuts() async throws {
        let rig = try await makeRig()
        let view = try await openChat(rig)
        await rig.transport.append(chatID, [approval("r1")])
        await eventually("a request waits") { rig.model.conversation(self.chatID).openApprovals.count == 1 }
        let shortcuts = Set((view.keyCommands ?? []).map { ($0.input ?? "") + "|\($0.modifierFlags.rawValue)" })
        XCTAssertEqual(shortcuts, ["\r|0", "\r|\(UIKeyModifierFlags.shift.rawValue)", "\(UIKeyCommand.inputEscape)|0", "\u{8}|\(UIKeyModifierFlags.command.rawValue)"],
                       "⏎ ⇧⏎ ⎋ and ⌘⌫ are all the composer claims; ⌘. is the screen's")
        // ⌘. is on the hosting controller while there is something to interrupt, and nothing else of the terminal's is.
        func commandKeys() -> [String] { (rig.host.keyCommands ?? []).filter { $0.modifierFlags == .command }.compactMap { $0.input?.lowercased() } }
        XCTAssertFalse(commandKeys().contains("."), "nothing to interrupt yet")
        for taken in ["k", ",", "/", "e", "t", "w", "a", "s", "d", "b", "f", "c", "z", "r", "l"] { XCTAssertFalse(commandKeys().contains(taken), taken) }
        XCTAssertTrue(commandKeys().contains("n"), "⌘N, a new terminal or chat, still works from a chat")
        await rig.transport.append(chatID, [.turnStarted(turnID: "t1"), .state(.running)])
        await eventually("⌘. is there while it works") { commandKeys().contains(".") }
        await rig.transport.append(chatID, [.turnCompleted(turnID: "t1", outcome: .completed), .state(.idle)])
        await eventually("and gone when it is idle") { !commandKeys().contains(".") }
        await finish(rig)
    }

    // MARK: Focus

    func testTheComposerTakesTheKeyboardWhenTheChatOpensWithAHardwareKeyboard() async throws {
        let rig = try await makeRig(hardwareKeyboard: true)
        let view = try await openChat(rig)
        await eventually("focused") { view.isFirstResponder }
        await finish(rig)
    }
    func testWithoutAHardwareKeyboardTheSoftwareKeyboardIsNotPulledUp() async throws {
        let rig = try await makeRig(hardwareKeyboard: false)
        let view = try await openChat(rig)
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertFalse(view.isFirstResponder, "the same rule as the terminal: “with a hardware keyboard” by default")
        await finish(rig)
    }
    func testTheKeyboardCanBeAskedForAlwaysOrNever() async throws {
        let rig = try await makeRig(hardwareKeyboard: false)
        rig.model.keyboard.setFocusSetting(.always)
        let view = try await openChat(rig)
        await eventually("focused") { view.isFirstResponder }
        await finish(rig)
        let never = try await makeRig(hardwareKeyboard: true)
        never.model.keyboard.setFocusSetting(.never)
        let other = try await openChat(never)
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertFalse(other.isFirstResponder)
        await finish(never)
    }

    // MARK: Sending

    func testReturnSendsAndShiftReturnMakesANewLine() async throws {
        let rig = try await makeRig()
        let view = try await openChat(rig)
        view.insertText("fix the build")
        await eventually("the draft follows the text") { rig.model.conversation(self.chatID).draft == "fix the build" }
        press(view, "\r", .shift)
        XCTAssertEqual(view.text, "fix the build\n", "⇧⏎ is a line break, and sends nothing")
        let none = await sentCommands(rig)
        XCTAssertTrue(none.isEmpty)
        view.insertText("then the tests")
        await eventually("the draft has both lines") { rig.model.conversation(self.chatID).draft == "fix the build\nthen the tests" }
        press(view, "\r")
        await eventually("sent") { await !self.sentCommands(rig).isEmpty }
        let sent = await sentCommands(rig)
        XCTAssertEqual(sent, [.object(["command": .string("send"), "text": .string("fix the build\nthen the tests")])])
        await eventually("the composer is empty again") { view.text.isEmpty && rig.model.conversation(self.chatID).draft.isEmpty }
        // On an empty composer, ⏎ does nothing at all.
        press(view, "\r")
        try await Task.sleep(for: .milliseconds(100))
        let after = await sentCommands(rig)
        XCTAssertEqual(after.count, 1)
        await finish(rig)
    }
    func testAWordBeingComposedKeepsReturnForTheInputMethod() async throws {
        let rig = try await makeRig()
        let view = try await openChat(rig)
        view.setMarkedText("にほ", selectedRange: NSRange(location: 2, length: 0))
        XCTAssertNotNil(view.markedTextRange)
        XCTAssertNil(view.keyCommands, "Return confirms the candidate; it is not ours")
        view.unmarkText()
        XCTAssertNotNil(view.keyCommands)
        await finish(rig)
    }

    // MARK: A request waiting

    func testWithARequestWaitingAndNothingTypedTheKeysAnswerIt() async throws {
        let rig = try await makeRig()
        await resolving(rig.transport)
        let view = try await openChat(rig)
        XCTAssertNil(command(view, UIKeyCommand.inputEscape), "Escape is the text's until a request waits")
        await rig.transport.append(chatID, [.turnStarted(turnID: "t1"), .state(.waiting), approval("r1"), approval("r2"), approval("r3"), approval("r4")])
        let conversation = rig.model.conversation(chatID)
        await eventually("four requests wait") { conversation.openApprovals.count == 4 }
        press(view, "\r")
        await eventually("⏎ allowed the first") { conversation.openApprovals.map(\.requestID) == ["r2", "r3", "r4"] }
        press(view, "\r", .shift)
        await eventually("⇧⏎ allowed the second for the session") { conversation.openApprovals.map(\.requestID) == ["r3", "r4"] }
        press(view, UIKeyCommand.inputEscape)
        await eventually("⎋ denied the third") { conversation.openApprovals.map(\.requestID) == ["r4"] }
        press(view, "\u{8}", .command)
        await eventually("⌘⌫ denied the fourth, as Escape does on a keyboard without one") { conversation.openApprovals.isEmpty }
        let sent = await sentCommands(rig)
        func approve(_ id: String, _ decision: String) -> JSONValue { .object(["command": .string("approve"), "request_id": .string(id), "decision": .string(decision)]) }
        XCTAssertEqual(sent, [approve("r1", "accept"), approve("r2", "accept_for_session"), approve("r3", "decline"), approve("r4", "decline")])
        await finish(rig)
    }
    func testTypingTakesTheKeysBackFromTheRequest() async throws {
        let rig = try await makeRig()
        let view = try await openChat(rig)
        await rig.transport.append(chatID, [approval("r1")])
        await eventually("a request waits") { rig.model.conversation(self.chatID).openApprovals.count == 1 }
        XCTAssertNotNil(command(view, UIKeyCommand.inputEscape))
        view.insertText("wait, ")
        XCTAssertNil(command(view, UIKeyCommand.inputEscape), "Escape does not deny while a message is being written")
        XCTAssertNil(command(view, "\u{8}", .command), "and ⌘⌫ is the text view's")
        await eventually("the draft follows the text") { rig.model.conversation(self.chatID).draft == "wait, " }
        press(view, "\r")
        await eventually("Return sent the message, not the answer") { await !self.sentCommands(rig).isEmpty }
        let sent = await sentCommands(rig)
        XCTAssertEqual(sent, [.object(["command": .string("send"), "text": .string("wait, ")])])
        XCTAssertEqual(rig.model.conversation(chatID).openApprovals.count, 1, "the request still waits")
        await finish(rig)
    }
    func testADecisionTheProviderDoesNotOfferIsNotMadeOnAKey() async throws {
        let rig = try await makeRig()
        let view = try await openChat(rig)
        await rig.transport.append(chatID, [approval("r1", [.decline, .cancel])])
        await eventually("a request waits") { rig.model.conversation(self.chatID).openApprovals.count == 1 }
        press(view, "\r")
        press(view, "\r", .shift)
        try await Task.sleep(for: .milliseconds(150))
        let none = await sentCommands(rig)
        XCTAssertTrue(none.isEmpty, "⏎ would be Allow, which this request does not offer; nothing is sent")
        press(view, UIKeyCommand.inputEscape)
        await eventually("⎋ denies") { await !self.sentCommands(rig).isEmpty }
        await finish(rig)
    }

    // MARK: Pictures

    /// Draws the screen at phone size and checks it is not blank. With `RIWORK_CHAT_SNAPSHOTS` set to a directory the pictures are kept
    /// there, for looking at.
    private func snapshot(_ rig: Rig, name: String) throws {
        rig.window.layoutIfNeeded()
        let renderer = UIGraphicsImageRenderer(bounds: rig.window.bounds)
        let image = renderer.image { _ in rig.window.drawHierarchy(in: rig.window.bounds, afterScreenUpdates: true) }
        guard let data = image.pngData() else { return XCTFail("no picture") }
        XCTAssertGreaterThan(data.count, 20_000, "\(name) drew something")
        if let directory = ProcessInfo.processInfo.environment["RIWORK_CHAT_SNAPSHOTS"] {
            try? FileManager.default.createDirectory(atPath: directory, withIntermediateDirectories: true)
            try? data.write(to: URL(fileURLWithPath: directory).appendingPathComponent("\(name).png"))
        }
    }
    private func conversationEvents() -> [ChatEvent] {
        let diff = "@@ -10,6 +10,7 @@ fn main() {\n     let a = 1;\n-    let b = 2;\n+    let b = 3;\n+    let c = 4;\n     println!(\"{a}\");\n"
        let longOutput = (1...40).map { "compiling crate \($0) of 40" }.joined(separator: "\n")
        return [
            .info(chat(state: .running, mode: .autoEdit)),
            .turnStarted(turnID: "t0"),
            .itemCompleted(ChatItem(id: "u1", turnID: "t0", status: .completed, body: .userMessage("The build is red. Can you find out why and fix it?"))),
            .itemCompleted(ChatItem(id: "r1", turnID: "t0", status: .completed, body: .reasoning("The user wants the build fixed. Start by running it."))),
            .itemCompleted(ChatItem(id: "c1", turnID: "t0", status: .completed, body: .command(command: "cargo build --release 2>&1 | tail -n 40", cwd: "/fixture", output: longOutput, exitCode: 101))),
            .itemCompleted(ChatItem(id: "a1", turnID: "t0", status: .completed, body: .agentMessage("""
            # Findings

            The build fails in **`main.rs`**: `b` is *shadowed* and then `c` is unused. Two ways out:

            1. Rename the second binding
            2. Use `c` in the `println!`
               - or drop it
            - [x] reproduced
            - [ ] fixed

            ```rust
            let b = 3;
            println!("{a} {b}");
            ```

            > Note: nothing outside `src/` changes.

            See [the docs](https://doc.rust-lang.org) or [this](javascript:alert(1)).
            """))),
            .itemCompleted(ChatItem(id: "p1", turnID: "t0", status: .completed, body: .plan(explanation: "Fix, then check.", steps: [ChatStep(text: "Reproduce", status: .completed), ChatStep(text: "Edit main.rs", status: .inProgress), ChatStep(text: "Run the tests", status: .pending)]))),
            .itemCompleted(ChatItem(id: "f1", turnID: "t0", status: .completed, body: .fileChange([ChatFileChange(path: "src/main.rs", kind: .modify, diff: diff), ChatFileChange(path: "notes.md", kind: .add)]))),
            .itemCompleted(ChatItem(id: "t1", turnID: "t0", status: .completed, body: .toolCall(server: "cua-driver", tool: "click", input: .object(["x": .number(10), "y": .number(20)]), output: "clicked"))),
            .itemCompleted(ChatItem(id: "w1", turnID: "t0", status: .completed, body: .webSearch("rust shadowed binding warning"))),
            .itemCompleted(ChatItem(id: "k1", turnID: "t0", status: .completed, body: .compaction)),
            .itemCompleted(ChatItem(id: "n1", turnID: "t0", status: .completed, body: .notice(level: .warning, text: "The tests take a while."))),
            .itemStarted(ChatItem(id: "c2", turnID: "t0", status: .inProgress, body: .command(command: "cargo test", cwd: nil, output: "running 12 tests\ntest a ... ok\ntest b ... ok\n", exitCode: nil))),
            .usage(ChatUsage(inputTokens: 12_000, outputTokens: 800, contextWindow: 200_000, contextUsed: 84_000, costUSD: 0.4234)),
            .state(.running)
        ]
    }
    private var mixed: [ChatInfo] {
        [chat(state: .running, mode: .autoEdit), ChatInfo(id: "cccccccc-2222-4222-8222-222222222222", provider: .codex, projectID: project, title: "Codex chat", createdAtUnix: 5, state: .waiting)]
    }
    func testTheScreenDrawsEveryKindOfItem() async throws {
        // Tall enough to hold the whole transcript, so every row is drawn.
        let rig = try await makeRig(chats: mixed, height: 2600)
        await rig.transport.append(chatID, conversationEvents())
        _ = try await openChat(rig)
        await eventually("the transcript is in") { rig.model.conversation(self.chatID).transcript.items.count >= 11 }
        try await Task.sleep(for: .milliseconds(800))
        try snapshot(rig, name: "chat-transcript-collapsed")
        rig.model.conversation(chatID).expanded.formUnion(["c1", "f1", "f1#src/main.rs", "t1", "r1"])
        try await Task.sleep(for: .milliseconds(800))
        try snapshot(rig, name: "chat-transcript-expanded")
        await finish(rig)
    }
    func testARequestAboveTheComposerDraws() async throws {
        let rig = try await makeRig(chats: mixed)
        await rig.transport.append(chatID, [.info(chat(state: .waiting, mode: .supervised)), .itemStarted(ChatItem(id: "c2", turnID: "t0", body: .command(command: "cargo test --all-features", cwd: nil, output: "", exitCode: nil))),
            .state(.waiting), .approvalRequested(ChatApproval(requestID: "r1", itemID: "c2", kind: .command, title: "cargo test --all-features", detail: "Run the whole test suite in /fixture.", choices: [.accept, .acceptForSession, .decline, .cancel]))])
        _ = try await openChat(rig)
        let conversation = rig.model.conversation(chatID)
        await eventually("the bar") { conversation.openApprovals.count == 1 }
        try await Task.sleep(for: .milliseconds(500))
        try snapshot(rig, name: "chat-approval")
        await finish(rig)
    }
    func testAQuestionAndTheStatusLinesDraw() async throws {
        let rig = try await makeRig(chats: [chat(state: .failed("the process exited with status 1"))])
        await rig.transport.append(chatID, [
            .itemCompleted(ChatItem(id: "u1", status: .completed, body: .userMessage("run the tests"))),
            .state(.failed("the process exited with status 1")),
            .questionRequested(ChatQuestion(requestID: "q1", questions: [ChatQuestionPrompt(header: "Scope", question: "Which tests should I run?", options: [ChatQuestionOption(label: "All", description: "The full suite, about a minute"), ChatQuestionOption(label: "Changed", description: "Only what the diff touches")], multiSelect: false)]))
        ])
        _ = try await openChat(rig)
        await eventually("the question and the failure") { rig.model.conversation(self.chatID).openQuestions.count == 1 && rig.model.chatState(self.chat()) == .failed("the process exited with status 1") }
        try await Task.sleep(for: .milliseconds(400))
        try snapshot(rig, name: "chat-question-failed")
        await finish(rig)
    }
    func testTheNewTerminalSheetOffersTheChatKinds() async throws {
        let rig = try await makeRig()
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        let host = UIHostingController(rootView: NewTerminalSheet(sheet: sheet).desktopThemed(rig.model.theme.style).frame(width: 402, height: 560))
        let window = UIWindow(windowScene: try XCTUnwrap(UIApplication.shared.connectedScenes.first as? UIWindowScene))
        window.frame = CGRect(x: 0, y: 0, width: 402, height: 560)
        window.rootViewController = host
        window.makeKeyAndVisible()
        windows.append(window)
        sheet.select(kind: .claudeChat)
        try await Task.sleep(for: .milliseconds(300))
        window.layoutIfNeeded()
        let image = UIGraphicsImageRenderer(bounds: window.bounds).image { _ in window.drawHierarchy(in: window.bounds, afterScreenUpdates: true) }
        if let directory = ProcessInfo.processInfo.environment["RIWORK_CHAT_SNAPSHOTS"], let data = image.pngData() {
            try? FileManager.default.createDirectory(atPath: directory, withIntermediateDirectories: true)
            try? data.write(to: URL(fileURLWithPath: directory).appendingPathComponent("new-chat-sheet.png"))
        }
        XCTAssertEqual(sheet.form.kinds.map(\.title), ["Shell", "Codex", "Claude", "Grok", "Codex chat", "Claude chat"])
        await finish(rig)
    }

    // MARK: An orchestrator that runs as a chat

    private let orchestratorID = "aaaaaaaa-1111-4111-8111-111111111111"
    /// The project's orchestrator as `orchestrators.list` gives it when the Mac runs it as a chat (its id is not the chat's).
    private var chatOrchestrator: String {
        "{\"id\":\"\(orchestratorID)\",\"project_id\":\"\(project)\",\"worktree_id\":null,\"kind\":\"orchestrator\",\"cwd\":\"/fixture\",\"harness\":null,\"alive\":true,\"created_at_unix\":3,\"mode\":\"chat\",\"chat_id\":\"\(chatID)\",\"provider\":\"claude\",\"activity\":\"working\"}"
    }
    func testAChatOrchestratorOpensTheChatScreenForItsChatAndTheTerminalGoesAway() async throws {
        let rig = try await makeRig(chats: [], orchestrators: [chatOrchestrator])
        XCTAssertEqual(rig.model.tabs.map(\.id), [chatID, ChatTransport.shell], "its tab sits in the strip, first")
        XCTAssertFalse(descendants(KeyCaptureView.self, in: rig.host.view).isEmpty)
        await rig.transport.append(chatID, [.state(.running), .itemStarted(ChatItem(id: "a", turnID: "t1", status: .completed, body: .agentMessage("Three workers are running; the build is green.")))])
        rig.model.chooseOrchestrator(rig.model.tabs[0])
        await eventually("the chat screen is up for the chat id") { self.composer(rig) != nil && rig.model.conversation(self.chatID).following }
        XCTAssertTrue(descendants(KeyCaptureView.self, in: rig.host.view).isEmpty, "no terminal behind it")
        XCTAssertFalse(rig.model.terminalVisible)
        try await Task.sleep(for: .milliseconds(300))
        try snapshot(rig, name: "chat-orchestrator-tab")
        let asked = await rig.transport.params(of: "chat.events").compactMap { $0["chat_id"]?.string }
        XCTAssertEqual(Set(asked), [chatID])
        let shells = await rig.transport.params(of: "shell.output").compactMap { $0["shell_id"]?.string }
        XCTAssertFalse(shells.contains(orchestratorID))
        // A terminal tab brings the terminal back, and takes the composer away.
        await rig.model.chooseSession(try XCTUnwrap(rig.model.openSessions.first))
        await eventually("the terminal is back") { !self.descendants(KeyCaptureView.self, in: rig.host.view).isEmpty && self.composer(rig) == nil }
        XCTAssertTrue(rig.model.terminalVisible)
        await finish(rig)
    }
    func testAChatOrchestratorOnAnOlderMacShowsNeitherAChatNorATerminal() async throws {
        let rig = try await makeRig(chats: [], orchestrators: [chatOrchestrator], chatFeature: false)
        rig.model.chooseOrchestrator(rig.model.tabs[0])
        await eventually("the terminal is released") { self.descendants(KeyCaptureView.self, in: rig.host.view).isEmpty && !rig.model.terminalVisible }
        XCTAssertNil(composer(rig), "and there is no chat to type into")
        XCTAssertNotNil(rig.model.selectedBlocked)
        try await Task.sleep(for: .milliseconds(300))
        try snapshot(rig, name: "chat-orchestrator-update-the-mac")
        let calls = await rig.transport.count("chat.events") + rig.transport.count("chats.list")
        XCTAssertEqual(calls, 0)
        await finish(rig)
    }

    func testTheNewTerminalSheetOffersTheOrchestratorsWhereTheMacOpensThemAndTheirRowsHaveNoWorktree() async throws {
        let rig = try await makeRig(orchestratorCreate: true)
        let sheet = try XCTUnwrap(NewTerminalSheetModel(model: rig.model))
        let host = UIHostingController(rootView: NewTerminalSheet(sheet: sheet).desktopThemed(rig.model.theme.style).frame(width: 402, height: 640))
        let window = UIWindow(windowScene: try XCTUnwrap(UIApplication.shared.connectedScenes.first as? UIWindowScene))
        window.frame = CGRect(x: 0, y: 0, width: 402, height: 640)
        window.rootViewController = host
        window.makeKeyAndVisible()
        windows.append(window)
        sheet.select(kind: .projectOrchestrator)
        try await Task.sleep(for: .milliseconds(300))
        window.layoutIfNeeded()
        let image = UIGraphicsImageRenderer(bounds: window.bounds).image { _ in window.drawHierarchy(in: window.bounds, afterScreenUpdates: true) }
        if let directory = ProcessInfo.processInfo.environment["RIWORK_CHAT_SNAPSHOTS"], let data = image.pngData() {
            try? FileManager.default.createDirectory(atPath: directory, withIntermediateDirectories: true)
            try? data.write(to: URL(fileURLWithPath: directory).appendingPathComponent("new-orchestrator-sheet.png"))
        }
        XCTAssertEqual(sheet.form.kinds.map(\.title), ["Shell", "Codex", "Claude", "Grok", "Codex chat", "Claude chat", "Project orchestrator", "Global orchestrator"])
        XCTAssertFalse(sheet.form.fields.contains(.target))
        // The keyboard reaches both rows from the kind list, and Return opens.
        sheet.press(.down)
        XCTAssertEqual(sheet.form.kind, .globalOrchestrator)
        await finish(rig)
    }
}
