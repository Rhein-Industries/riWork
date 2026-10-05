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

    /// How the desktop draws: the terminal look (no palette published), or its Native skin in light or dark.
    enum Look: String, CaseIterable { case terminal, nativeLight = "native-light", nativeDark = "native-dark" }
    /// Native's own palette and flag, as the desktop publishes them (src/theme.rs NATIVE_LIGHT / NATIVE_DARK).
    /// With `mic`, the desktop's mic setting is on too (`"mic": true`); the terminal look then has the Gruvbox Light palette published, as
    /// the built-in style draws, since a setting travels with the colors.
    private func appearance(_ look: Look, mic: Bool = false, updated: Double = 1_790_000_000, published: Bool = false) -> JSONValue? {
        guard look != .terminal || mic || published else { return nil }
        let dark = look == .nativeDark
        let colors = look == .terminal
            ? ["bg": "#fbf1c7", "panel": "#f4ebc2", "panel_active": "#ede3bc", "divider": "#d5ccb6", "cyan": "#427b58", "magenta": "#8f3f71", "gold": "#9d5015", "text": "#3c3836", "muted": "#756f5e"]
            : dark
            ? ["bg": "#000000", "panel": "#1c1c1e", "panel_active": "#2c2c2e", "divider": "#3a3a3c", "cyan": "#ffffff", "magenta": "#c7c7cc", "gold": "#ff9f0a", "text": "#f5f5f7", "muted": "#98989d"]
            : ["bg": "#ffffff", "panel": "#f5f5f7", "panel_active": "#e8e8ed", "divider": "#d2d2d7", "cyan": "#000000", "magenta": "#3a3a3c", "gold": "#b34000", "text": "#1d1d1f", "muted": "#636366"]
        var fields: [String: JSONValue] = ["v": .number(1), "updated_at": .number(updated), "dark": .bool(dark), "palette": .object(colors.mapValues { .string($0) })]
        if look != .terminal { fields["native"] = .bool(true) }
        if mic { fields["mic"] = .bool(true) }
        return .object(fields)
    }

    /// The tab screen in the desktop's current style, read where the app reads it, so a new look or setting reaches it live.
    private struct ThemedTabs: View {
        let model: RemoteModel
        let project: RemoteProject
        var body: some View { TerminalTabsView(model: model, project: project, onBack: {}).desktopThemed(model.theme.style) }
    }
    private struct Rig {
        let model: RemoteModel, transport: ChatTransport, window: UIWindow, host: UIHostingController<AnyView>, keychain: KeychainStore
    }
    private func makeRig(chats: [ChatInfo]? = nil, orchestrators: [String] = [], chatFeature: Bool = true, orchestratorCreate: Bool = false, hardwareKeyboard: Bool = true, width: CGFloat = 402, height: CGFloat = 874, look: Look = .terminal, mic: Bool = false) async throws -> Rig {
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
        let transport = ChatTransport(chats: chats ?? [chat()], appearance: appearance(look, mic: mic))
        await transport.setFeature(chatFeature)
        await transport.setOrchestratorFeature(orchestratorCreate)
        await transport.setOrchestrators(orchestrators)
        let model = RemoteModel(client: transport, keychain: keychain, defaults: UserDefaults(suiteName: suite)!, chatWaitMilliseconds: 300, chatIdleInterval: .milliseconds(20),
                                hardwareKeyboard: HardwareKeyboardMonitor(probe: { hardwareKeyboard }))
        await model.connect()
        if look != .terminal { await eventually("the desktop's Native look is in") { model.theme.style.native } }
        if mic { await eventually("the desktop's mic setting is in") { model.theme.style.mic } }
        let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))
        let root = AnyView(ThemedTabs(model: model, project: projectValue))
        let host = UIHostingController(rootView: root)
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: width, height: height)
        window.rootViewController = host
        if look != .terminal { window.overrideUserInterfaceStyle = look == .nativeDark ? .dark : .light }
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
    /// The picture's name in a look; the terminal look keeps the plain name.
    private func named(_ name: String, _ look: Look) -> String { look == .terminal ? name : "\(name)-\(look.rawValue)" }
    /// Pictures of the paperclip's menu open at each place it is, keyboard up, in Native dark and the terminal look. A system menu is
    /// drawn outside the app's views, so neither this test nor a snapshot can open or draw it: with `RIWORK_ATTACH_SCREENSHOTS` set to a
    /// directory, each state leaves `<name>.ready` and waits for `<name>.png`, which whoever drives the simulator takes after tapping
    /// the paperclip (`xcrun simctl io <udid> screenshot`). Skipped otherwise.
    func testPicturesOfThePaperclipMenu() async throws {
        guard let path = ProcessInfo.processInfo.environment["RIWORK_ATTACH_SCREENSHOTS"] else { throw XCTSkip("Set RIWORK_ATTACH_SCREENSHOTS") }
        executionTimeAllowance = 1800
        let directory = URL(fileURLWithPath: path)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        func hold(_ name: String) async throws {
            let ready = directory.appendingPathComponent(name + ".ready"), shot = directory.appendingPathComponent(name + ".png")
            try? FileManager.default.removeItem(at: shot)
            try Data().write(to: ready)
            let deadline = Date().addingTimeInterval(600)
            while !FileManager.default.fileExists(atPath: shot.path), Date() < deadline { try await Task.sleep(for: .milliseconds(250)) }
            try? FileManager.default.removeItem(at: ready)
            XCTAssertTrue(FileManager.default.fileExists(atPath: shot.path), "no screenshot taken for \(name)")
        }
        let screen = UIScreen.main.bounds.size
        for look in [Look.nativeDark, .terminal] {
            // The chat composer, keyboard up.
            var rig = try await makeRig(chats: mixed, hardwareKeyboard: false, width: screen.width, height: screen.height, look: look, mic: true)
            await rig.transport.append(chatID, conversationEvents())
            let field = try await openChat(rig)
            _ = field.becomeFirstResponder()
            try await Task.sleep(for: .milliseconds(1200))
            try await hold(named("attach-chat", look))
            await finish(rig)

            // The terminal's key bar above the keyboard, scrolled to its paperclip.
            rig = try await makeRig(hardwareKeyboard: false, width: screen.width, height: screen.height, look: look, mic: true)
            let capture = try XCTUnwrap(descendants(KeyCaptureView.self, in: rig.host.view).first)
            _ = capture.becomeFirstResponder()
            try await Task.sleep(for: .milliseconds(1200))
            let paperclip = try XCTUnwrap(capture.bar.buttons[.attach])
            capture.bar.scrollView.scrollRectToVisible(paperclip.frame.insetBy(dx: -60, dy: 0), animated: false)
            try await Task.sleep(for: .milliseconds(300))
            try await hold(named("attach-keybar", look))
            // The bar under the terminal while the keyboard is down.
            _ = capture.resignFirstResponder()
            try await Task.sleep(for: .milliseconds(1200))
            try await hold(named("attach-terminal-bar", look))
            // The line composer, its field holding the keyboard.
            rig.model.preferLineComposer = true
            try await Task.sleep(for: .milliseconds(600))
            if let line = descendants(UITextField.self, in: rig.host.view).first { _ = line.becomeFirstResponder() }
            try await Task.sleep(for: .milliseconds(1200))
            try await hold(named("attach-line-composer", look))
            await finish(rig)
        }
    }
    /// Pictures of the rows that hold a field and its buttons: the chat composer with one line and with several, while dictating and
    /// with a turn running, and the terminal's line composer and the bar under the terminal, in Native dark and the terminal look. As
    /// above, with `RIWORK_ROW_SCREENSHOTS` set each state leaves `<name>.ready` and waits for `<name>.png` from the simulator.
    func testPicturesOfTheComposerRows() async throws {
        guard let path = ProcessInfo.processInfo.environment["RIWORK_ROW_SCREENSHOTS"] else { throw XCTSkip("Set RIWORK_ROW_SCREENSHOTS") }
        executionTimeAllowance = 1800
        let directory = URL(fileURLWithPath: path)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        func hold(_ name: String) async throws {
            try await Task.sleep(for: .milliseconds(800))
            let ready = directory.appendingPathComponent(name + ".ready"), shot = directory.appendingPathComponent(name + ".png")
            try? FileManager.default.removeItem(at: shot)
            try Data().write(to: ready)
            let deadline = Date().addingTimeInterval(120)
            while !FileManager.default.fileExists(atPath: shot.path), Date() < deadline { try await Task.sleep(for: .milliseconds(250)) }
            try? FileManager.default.removeItem(at: ready)
            XCTAssertTrue(FileManager.default.fileExists(atPath: shot.path), "no screenshot taken for \(name)")
        }
        let screen = UIScreen.main.bounds.size
        for look in [Look.nativeDark, .terminal] {
            var rig = try await makeRig(chats: [chat(state: .idle), mixed[1]], hardwareKeyboard: false, width: screen.width, height: screen.height, look: look, mic: true)
            let field = try await openChat(rig)
            _ = field.becomeFirstResponder()
            let conversation = rig.model.conversation(chatID)
            try await hold(named("row-chat-empty", look))
            conversation.draft = "Run the tests"
            try await hold(named("row-chat-one-line", look))
            conversation.draft = "Run the tests\nthen fix what fails\nand tell me what changed"
            try await hold(named("row-chat-three-lines", look))
            await finish(rig)
            // A turn running: Stop joins the row.
            rig = try await makeRig(chats: [chat(state: .running), mixed[1]], hardwareKeyboard: false, width: screen.width, height: screen.height, look: look, mic: true)
            await rig.transport.append(chatID, [.info(chat(state: .running)), .turnStarted(turnID: "t0"), .state(.running)])
            _ = try await openChat(rig).becomeFirstResponder()
            await eventually("the turn is running") { rig.model.chatState(self.chat()).isBusy }
            rig.model.conversation(chatID).draft = "Also check the docs"
            try await hold(named("row-chat-running", look))
            await finish(rig)

            rig = try await makeRig(hardwareKeyboard: false, width: screen.width, height: screen.height, look: look, mic: true)
            try await hold(named("row-terminal-bar", look))
            rig.model.preferLineComposer = true
            try await Task.sleep(for: .milliseconds(600))
            if let line = descendants(UITextField.self, in: rig.host.view).first { _ = line.becomeFirstResponder() }
            try await hold(named("row-line-composer", look))
            await finish(rig)
        }
    }
    func testTheScreenDrawsEveryKindOfItem() async throws {
        for look in Look.allCases {
            // Tall enough to hold the whole transcript, so every row is drawn.
            let rig = try await makeRig(chats: mixed, height: 2600, look: look)
            await rig.transport.append(chatID, conversationEvents())
            _ = try await openChat(rig)
            await eventually("the transcript is in") { rig.model.conversation(self.chatID).transcript.items.count >= 11 }
            try await Task.sleep(for: .milliseconds(800))
            try snapshot(rig, name: named("chat-transcript-collapsed", look))
            rig.model.conversation(chatID).expanded.formUnion(["c1", "f1", "f1#src/main.rs", "t1", "r1"])
            try await Task.sleep(for: .milliseconds(800))
            try snapshot(rig, name: named("chat-transcript-expanded", look))
            await finish(rig)
        }
    }
    func testARequestAboveTheComposerDraws() async throws {
        for look in Look.allCases {
            let rig = try await makeRig(chats: mixed, look: look)
            await rig.transport.append(chatID, [.info(chat(state: .waiting, mode: .supervised)), .itemStarted(ChatItem(id: "c2", turnID: "t0", body: .command(command: "cargo test --all-features", cwd: nil, output: "", exitCode: nil))),
                .state(.waiting), .approvalRequested(ChatApproval(requestID: "r1", itemID: "c2", kind: .command, title: "cargo test --all-features", detail: "Run the whole test suite in /fixture.", choices: [.accept, .acceptForSession, .decline, .cancel]))])
            _ = try await openChat(rig)
            let conversation = rig.model.conversation(chatID)
            await eventually("the bar") { conversation.openApprovals.count == 1 }
            try await Task.sleep(for: .milliseconds(500))
            try snapshot(rig, name: named("chat-approval", look))
            await finish(rig)
        }
    }
    func testAQuestionAndTheStatusLinesDraw() async throws {
        for look in Look.allCases { await finish(try await questionAndStatusLines(look)) }
    }
    /// The same on the room the software keyboard leaves (the screen less the keyboard's 336 points). In Native the question gives up
    /// scrolling room so the last message stays whole above it, and what it scrolls ends between two answers, not through one.
    func testAQuestionAndTheStatusLinesLeaveTheLastMessageWholeOverTheKeyboard() async throws {
        for look in Look.allCases {
            let rig = try await questionAndStatusLines(look, height: 874 - 336, name: "chat-question-failed-keyboard")
            if look != .terminal {
                let transcript = try XCTUnwrap(transcriptScroll(rig))
                XCTAssertGreaterThanOrEqual(transcript.bounds.height, 47, "\(look): the transcript keeps room for its last message")
                XCTAssertLessThanOrEqual(transcript.contentSize.height - (transcript.contentOffset.y + transcript.bounds.height - transcript.adjustedContentInset.bottom), 1,
                                         "\(look): and is at its bottom")
            }
            await finish(rig)
        }
    }
    /// A bar that comes up over a transcript at its bottom (a failure, a question) keeps the transcript at its bottom: the last message is
    /// not left under it.
    func testTheTranscriptStaysAtItsBottomWhenABannerOrAQuestionComesUp() async throws {
        for look in Look.allCases {
            let rig = try await makeRig(chats: [chat(state: .running)], look: look)
            let messages = (1...12).map { ChatItem(id: "m\($0)", turnID: "t0", status: .completed, body: $0 % 2 == 0 ? .agentMessage("Answer \($0), a line or two of it.") : .userMessage("Question \($0)")) }
            await rig.transport.append(chatID, [.info(chat(state: .running)), .turnStarted(turnID: "t0")] + messages.map { .itemCompleted($0) } + [.state(.running)])
            _ = try await openChat(rig)
            await eventually("the transcript is in") { rig.model.conversation(self.chatID).transcript.items.count == 12 }
            func atBottom() -> Bool {
                guard let list = self.transcriptScroll(rig) else { return false }
                return list.contentSize.height - (list.contentOffset.y + list.bounds.height - list.adjustedContentInset.bottom) <= 1
            }
            await eventually("\(look): at the bottom") { atBottom() }
            await rig.transport.append(chatID, [.turnCompleted(turnID: "t0", outcome: .completed), .state(.failed("the process exited with status 1"))])
            await eventually("\(look): the failure") { rig.model.chatState(self.chat()) == .failed("the process exited with status 1") }
            try await Task.sleep(for: .milliseconds(400))
            await eventually("\(look): still at the bottom under the failure", timeout: 1) { atBottom() }
            await rig.transport.append(chatID, [.questionRequested(ChatQuestion(requestID: "q1", questions: [ChatQuestionPrompt(header: "Scope", question: "Which tests should I run?", options: [ChatQuestionOption(label: "All", description: "The full suite"), ChatQuestionOption(label: "Changed", description: "Only what the diff touches")], multiSelect: false)]))])
            await eventually("\(look): the question") { rig.model.conversation(self.chatID).openQuestions.count == 1 }
            try await Task.sleep(for: .milliseconds(400))
            await eventually("\(look): still at the bottom over the question", timeout: 1) { atBottom() }
            if !atBottom(), let list = transcriptScroll(rig) {
                XCTFail("offset \(list.contentOffset.y) height \(list.bounds.height) content \(list.contentSize.height) insets \(list.adjustedContentInset) pill \(rig.model.conversation(chatID).following)")
            }
            try snapshot(rig, name: named("chat-question-arrives", look))
            await finish(rig)
        }
    }
    /// The transcript's scroll view: from the top of the chat screen the wide scroll views are the tab strip, the transcript, and then
    /// those of the bars over the composer. (Over the keyboard the transcript can be shorter than a bar's, so size does not tell.)
    private func transcriptScroll(_ rig: Rig) -> UIScrollView? {
        let wide = descendants(UIScrollView.self, in: rig.host.view).filter { !($0 is UITextView) && $0.bounds.width > 300 && $0.window != nil }
            .sorted { $0.convert($0.bounds, to: nil).minY < $1.convert($1.bounds, to: nil).minY }
        return wide.count >= 2 ? wide[1] : nil
    }
    private func questionAndStatusLines(_ look: Look, height: CGFloat = 874, name: String = "chat-question-failed") async throws -> Rig {
        let rig = try await makeRig(chats: [chat(state: .failed("the process exited with status 1"))], height: height, look: look)
        await rig.transport.append(chatID, [
            .itemCompleted(ChatItem(id: "u1", status: .completed, body: .userMessage("run the tests"))),
            .state(.failed("the process exited with status 1")),
            .questionRequested(ChatQuestion(requestID: "q1", questions: [ChatQuestionPrompt(header: "Scope", question: "Which tests should I run?", options: [ChatQuestionOption(label: "All", description: "The full suite, about a minute"), ChatQuestionOption(label: "Changed", description: "Only what the diff touches")], multiSelect: false)]))
        ])
        _ = try await openChat(rig)
        await eventually("the question and the failure") { rig.model.conversation(self.chatID).openQuestions.count == 1 && rig.model.chatState(self.chat()) == .failed("the process exited with status 1") }
        try await Task.sleep(for: .milliseconds(400))
        try snapshot(rig, name: named(name, look))
        return rig
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

    // MARK: The model controls, the new-chat sheet and the orchestrators, in each look

    private let models = [
        ChatModelOption(id: "claude-opus-4-1", name: "Claude Opus 4.1", description: "The most capable model", efforts: ["low", "medium", "high"], defaultEffort: "medium", supportsFast: true, isDefault: true),
        ChatModelOption(id: "claude-sonnet-4-5", name: "Claude Sonnet 4.5", description: "Fast and capable", efforts: ["low", "medium", "high"], defaultEffort: "medium")
    ]
    private func modelChat(fast: Bool) -> ChatInfo {
        ChatInfo(id: chatID, provider: .claude, projectID: project, cwd: "/fixture", title: "Fix the build", createdAtUnix: 10,
                 model: "claude-opus-4-1", effort: "high", fast: fast, approvalMode: .supervised, state: .idle)
    }
    /// A desktop that does what a Configure says and reports the chat's new info, as the real one does.
    private final class Mirror: @unchecked Sendable {
        private let lock = NSLock()
        private var info: ChatInfo
        init(_ info: ChatInfo) { self.info = info }
        func apply(_ command: ChatCommand) -> [ChatEvent] {
            guard case .configure(let model, let effort, _, let fast) = command else { return [] }
            lock.lock(); defer { lock.unlock() }
            if let model { info.model = model }
            if let effort { info.effort = effort }
            if let fast { info.fast = fast }
            return [.info(info)]
        }
    }
    /// Sends a ⌘ shortcut of the screen up the responder chain, as a key press does.
    private func shortcut(_ rig: Rig, _ input: String) throws {
        let command = try XCTUnwrap((rig.host.keyCommands ?? []).first { $0.input?.lowercased() == input && $0.modifierFlags == .command }, "⌘\(input)")
        // A key press starts at the first responder: the composer, which takes the keyboard with the chat.
        if let composer = composer(rig), !composer.isFirstResponder { _ = composer.becomeFirstResponder() }
        XCTAssertTrue(UIApplication.shared.sendAction(try XCTUnwrap(command.action), to: nil, from: command, for: nil))
    }
    /// With `RIWORK_NATIVE_SCREENSHOTS` set to a directory: leaves `<name>.ready` there and waits for `<name>.png`, which whoever drives
    /// the simulator takes (`xcrun simctl io booted screenshot`), after opening a system menu if the name says so. Nothing otherwise.
    private func hold(_ name: String) async throws {
        guard let path = ProcessInfo.processInfo.environment["RIWORK_NATIVE_SCREENSHOTS"] else { return }
        executionTimeAllowance = 1800
        let directory = URL(fileURLWithPath: path)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try await Task.sleep(for: .milliseconds(600))
        let ready = directory.appendingPathComponent(name + ".ready"), shot = directory.appendingPathComponent(name + ".png")
        try? FileManager.default.removeItem(at: shot)
        try Data().write(to: ready)
        let deadline = Date().addingTimeInterval(300)
        while !FileManager.default.fileExists(atPath: shot.path), Date() < deadline { try await Task.sleep(for: .milliseconds(250)) }
        try? FileManager.default.removeItem(at: ready)
    }
    /// Activates the accessibility element with `label` on the screen, as VoiceOver's double tap does.
    private func activate(_ label: String, in view: UIView) -> Bool {
        // The tree is walked once, each node at most once and only so far: the terminal's views can expose a great many elements.
        var seen = Set<ObjectIdentifier>(), budget = 4000
        func search(_ element: NSObject) -> Bool {
            guard budget > 0, seen.insert(ObjectIdentifier(element)).inserted else { return false }
            budget -= 1
            if (element as? UIView)?.isHidden == true { return false }
            if element.accessibilityLabel == label, element.accessibilityActivate() { return true }
            let children = (element.accessibilityElements as? [NSObject]) ?? []
            if children.contains(where: search) { return true }
            return (element as? UIView)?.subviews.contains(where: search) ?? false
        }
        return search(view)
    }

    // MARK: The desktop's mic setting

    /// The width of a text field on the screen, in points: the mic beside it takes its room, so the field says whether it is there.
    private func width<T: UIView>(of type: T.Type, in rig: Rig) -> CGFloat? {
        descendants(type, in: rig.host.view).first { $0.window != nil }.map { $0.convert($0.bounds, to: nil).width }
    }
    /// The desktop turns its mic setting on or off; the phone picks it up on its next look at the colors (connect, foreground, the
    /// periodic refresh), here asked for at once.
    private func setMic(_ rig: Rig, _ on: Bool, look: Look, updated: Double) async {
        await rig.transport.setAppearance(appearance(look, mic: on, updated: updated, published: true))
        await rig.model.fetchAppearance()
        await eventually("the setting is followed") { rig.model.theme.style.mic == on }
    }

    func testTheChatComposerHasAMicOnlyWhileTheDesktopsSettingIsOnAndFollowsItLive() async throws {
        for look in Look.allCases {
            let rig = try await makeRig(look: look)
            _ = try await openChat(rig)
            try await Task.sleep(for: .milliseconds(300))
            XCTAssertFalse(rig.model.theme.style.mic, "\(look): off by default, as with a Mac that predates the setting")
            let off = try XCTUnwrap(width(of: ChatComposerTextView.self, in: rig))
            try snapshot(rig, name: named("chat-composer-mic-off", look))
            await setMic(rig, true, look: look, updated: 1_790_000_100)
            // The mic is 44 points wide, with the row's 4-point spacing.
            await eventually("\(look): the mic is there") { abs((self.width(of: ChatComposerTextView.self, in: rig) ?? 0) - (off - 48)) < 1 }
            try await Task.sleep(for: .milliseconds(300))
            try snapshot(rig, name: named("chat-composer-mic-on", look))
            await setMic(rig, false, look: look, updated: 1_790_000_200)
            await eventually("\(look): and gone again, the field closing up") { abs((self.width(of: ChatComposerTextView.self, in: rig) ?? 0) - off) < 1 }
            await finish(rig)
        }
    }
    func testTurningTheSettingOffCancelsAChatDictationInProgress() async throws {
        let rig = try await makeRig(look: .nativeLight, mic: true)
        _ = try await openChat(rig)
        let controller = DictationController.shared
        defer { controller.makeEngine = { DictationController.defaultEngine() } }
        XCTAssertTrue(controller.isAllowed, "the model passes the setting on")
        controller.makeEngine = { ScriptedSpeechEngine(script: "run the whole suite", interval: .milliseconds(30)) }
        final class Shown { var texts: [String] = [] }
        let shown = Shown()
        // As the composer's mic does.
        controller.toggle(for: .chat(chatID), live: { shown.texts.append($0) }, deliver: { _ in XCTFail("nothing is delivered") })
        await eventually("listening") { controller.isActive(for: .chat(self.chatID)) && !(shown.texts.last ?? "").isEmpty }
        await setMic(rig, false, look: .nativeLight, updated: 1_790_000_100)
        await eventually("cancelled") { !controller.phase.isActive }
        XCTAssertEqual(shown.texts.last, "", "what was heard is taken out")
        XCTAssertFalse(controller.isAllowed)
        await finish(rig)
    }
    func testTheTerminalsMicsFollowTheSettingTooTheKeyBarsAndTheLineComposers() async throws {
        for look in [Look.terminal, .nativeDark] {
            let rig = try await makeRig(hardwareKeyboard: false, look: look)
            let capture = try XCTUnwrap(descendants(KeyCaptureView.self, in: rig.host.view).first)
            XCTAssertFalse(capture.bar.showsMic, "\(look): no mic key by default")
            await setMic(rig, true, look: look, updated: 1_790_000_100)
            await eventually("\(look): the key bar's mic comes, in place") { capture.bar.showsMic }
            XCTAssertTrue(descendants(KeyCaptureView.self, in: rig.host.view).first === capture, "the bar is not rebuilt")
            await setMic(rig, false, look: look, updated: 1_790_000_200)
            await eventually("\(look): and goes") { !capture.bar.showsMic }
            // The line composer: its mic beside Send likewise.
            rig.model.preferLineComposer = true
            await eventually("\(look): the line composer") { self.width(of: UITextField.self, in: rig) != nil }
            try await Task.sleep(for: .milliseconds(300))
            let off = try XCTUnwrap(width(of: UITextField.self, in: rig))
            await setMic(rig, true, look: look, updated: 1_790_000_300)
            await eventually("\(look): the line composer's mic beside Send") { (self.width(of: UITextField.self, in: rig) ?? off) < off - 40 }
            await finish(rig)
        }
    }
    /// Pictures of the key bar above the keyboard with the setting off and on, in Native and the terminal look. As the other held
    /// pictures, with `RIWORK_NATIVE_SCREENSHOTS` set each state waits for a simulator screenshot (the keyboard is not in the app's
    /// windows); skipped otherwise.
    func testPicturesOfTheKeyBarWithTheMicSettingOffAndOn() async throws {
        guard ProcessInfo.processInfo.environment["RIWORK_NATIVE_SCREENSHOTS"] != nil else { throw XCTSkip("Set RIWORK_NATIVE_SCREENSHOTS") }
        let screen = UIScreen.main.bounds.size
        for look in [Look.nativeDark, .nativeLight, .terminal] {
            let rig = try await makeRig(hardwareKeyboard: false, width: screen.width, height: screen.height, look: look)
            let capture = try XCTUnwrap(descendants(KeyCaptureView.self, in: rig.host.view).first)
            _ = capture.becomeFirstResponder()
            try await Task.sleep(for: .milliseconds(1200))
            func toEnd() { capture.bar.scrollView.setContentOffset(CGPoint(x: max(0, capture.bar.scrollView.contentSize.width - capture.bar.scrollView.bounds.width), y: 0), animated: false) }
            toEnd()
            try await hold(named("keybar-mic-off", look))
            await setMic(rig, true, look: look, updated: 1_790_000_100)
            try await Task.sleep(for: .milliseconds(300))
            toEnd()
            try await hold(named("keybar-mic-on", look))
            await finish(rig)
        }
    }

    func testTheModelControlsDrawInEachLook() async throws {
        let screen = UIScreen.main.bounds.size
        for look in Look.allCases {
            let rig = try await makeRig(chats: [modelChat(fast: true)], width: screen.width, height: screen.height, look: look)
            let mirror = Mirror(modelChat(fast: true))
            await rig.transport.handleCommands { _, command in mirror.apply(command) }
            await rig.transport.append(chatID, [.info(modelChat(fast: true)), .models(models),
                .itemCompleted(ChatItem(id: "u1", turnID: "t0", status: .completed, body: .userMessage("Which model are you?"))),
                .itemCompleted(ChatItem(id: "a1", turnID: "t0", status: .completed, body: .agentMessage("Claude Opus 4.1, with high effort and Fast on."))),
                .usage(ChatUsage(inputTokens: 12_000, outputTokens: 800, contextWindow: 200_000, contextUsed: 42_000))])
            _ = try await openChat(rig)
            let conversation = rig.model.conversation(chatID)
            await eventually("the models are in") { conversation.transcript.models.count == 2 }
            try await Task.sleep(for: .milliseconds(500))
            try snapshot(rig, name: named("chat-model-toolbar", look))
            try await hold(named("chat-model-toolbar", look))
            // The ⋯ menu, opened by hand on the held screen, has Change model.
            try await hold(named("chat-options-menu", look))
            // ⌘M: the picker, Fast on and then off.
            try shortcut(rig, "m")
            await eventually("the picker is up") { rig.host.presentedViewController != nil }
            try await Task.sleep(for: .milliseconds(700))
            try snapshot(rig, name: named("chat-model-sheet-fast-on", look))
            try await hold(named("chat-model-sheet-fast-on", look))
            _ = await rig.model.chooseChatModel(modelChat(fast: true), .fast(false))
            await eventually("Fast is off") { !conversation.modelChoices(fallback: self.modelChat(fast: false)).fastIsOn }
            try await Task.sleep(for: .milliseconds(500))
            try snapshot(rig, name: named("chat-model-sheet-fast-off", look))
            try await hold(named("chat-model-sheet-fast-off", look))
            // The model line opens the text field for a model by name (a system alert, so only a screenshot shows it).
            if ProcessInfo.processInfo.environment["RIWORK_NATIVE_SCREENSHOTS"] != nil, let sheet = rig.host.presentedViewController {
                await withCheckedContinuation { done in sheet.dismiss(animated: false) { done.resume() } }
                try await Task.sleep(for: .milliseconds(500))
                XCTAssertTrue(activate("Change model", in: rig.host.view), "the model line is a button")
                try await hold(named("chat-model-alert", look))
            }
            await finish(rig)
        }
    }
    func testTheNewChatAndOrchestratorChoicesDrawInEachLook() async throws {
        let screen = UIScreen.main.bounds.size
        for look in Look.allCases {
            let rig = try await makeRig(orchestratorCreate: true, width: screen.width, height: screen.height, look: look)
            let opus = models[0]
            rig.model.rememberChatChoice(NewChatChoice(model: opus, usesModel: true, effort: "high", fast: true), for: .claude)
            rig.model.newTerminalRequestedProject = project
            var keys: NewTerminalKeyView?
            await eventually("the New terminal sheet is up") {
                keys = rig.host.presentedViewController.flatMap { self.descendants(NewTerminalKeyView.self, in: $0.view).first }
                return keys != nil
            }
            let keyView = try XCTUnwrap(keys)
            func press(_ input: String, times: Int = 1) {
                for _ in 0..<times { if let command = keyView.keyCommands?.first(where: { $0.input == input && $0.modifierFlags == [] }) { keyView.fired(command) } }
            }
            // Shell, Codex, Claude, Grok, Codex chat, Claude chat.
            press(UIKeyCommand.inputDownArrow, times: 5)
            try await Task.sleep(for: .milliseconds(700))
            // The model section is under the kinds: scrolled to, as a thumb would.
            if let presented = rig.host.presentedViewController,
               let list = descendants(UIScrollView.self, in: presented.view).max(by: { $0.contentSize.height < $1.contentSize.height }) {
                list.setContentOffset(CGPoint(x: 0, y: max(0, list.contentSize.height - list.bounds.height + list.adjustedContentInset.bottom)), animated: false)
                try await Task.sleep(for: .milliseconds(300))
            }
            try snapshot(rig, name: named("new-chat-sheet-model", look))
            try await hold(named("new-chat-sheet-model", look))
            press(UIKeyCommand.inputDownArrow)
            try await Task.sleep(for: .milliseconds(500))
            try snapshot(rig, name: named("new-orchestrator-sheet", look))
            await finish(rig)
        }
    }
    func testTheOrchestratorsDrawInEachLook() async throws {
        for look in Look.allCases {
            var rig = try await makeRig(chats: [], orchestrators: [chatOrchestrator], orchestratorCreate: true, look: look)
            await rig.transport.append(chatID, [.state(.running), .itemStarted(ChatItem(id: "a", turnID: "t1", status: .completed, body: .agentMessage("Three workers are running; the build is green.")))])
            rig.model.chooseOrchestrator(rig.model.tabs[0])
            await eventually("the chat screen is up for the chat id") { self.composer(rig) != nil }
            // Asking for the project's orchestrator again says it is already there, in the note under the tabs.
            _ = await rig.model.createOrchestrator(try NewOrchestratorRequest(projectID: project))
            await eventually("the note") { rig.model.orchestratorNotice != nil }
            try await Task.sleep(for: .milliseconds(400))
            try snapshot(rig, name: named("chat-orchestrator-tab", look))
            // The tabs menu, opened by hand on the held screen, has Global orchestrator.
            try await hold(named("tabs-menu", look))
            await finish(rig)
            rig = try await makeRig(chats: [], orchestrators: [chatOrchestrator], chatFeature: false, look: look)
            rig.model.chooseOrchestrator(rig.model.tabs[0])
            await eventually("the notice") { rig.model.selectedBlocked != nil }
            try await Task.sleep(for: .milliseconds(400))
            try snapshot(rig, name: named("chat-orchestrator-update-the-mac", look))
            await finish(rig)
        }
    }
}
