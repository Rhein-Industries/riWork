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
    private func appearance(_ look: Look) -> JSONValue? {
        guard look != .terminal else { return nil }
        let dark = look == .nativeDark
        let colors = dark
            ? ["bg": "#000000", "panel": "#1c1c1e", "panel_active": "#2c2c2e", "divider": "#3a3a3c", "cyan": "#ffffff", "magenta": "#c7c7cc", "gold": "#ff9f0a", "text": "#f5f5f7", "muted": "#98989d"]
            : ["bg": "#ffffff", "panel": "#f5f5f7", "panel_active": "#e8e8ed", "divider": "#d2d2d7", "cyan": "#000000", "magenta": "#3a3a3c", "gold": "#b34000", "text": "#1d1d1f", "muted": "#636366"]
        return .object(["v": .number(1), "updated_at": .number(1_790_000_000), "dark": .bool(dark), "native": .bool(true), "palette": .object(colors.mapValues { .string($0) })])
    }

    private struct Rig {
        let model: RemoteModel, transport: ChatTransport, window: UIWindow, host: UIHostingController<AnyView>, keychain: KeychainStore
    }
    private func makeRig(chats: [ChatInfo]? = nil, hardwareKeyboard: Bool = true, width: CGFloat = 402, height: CGFloat = 874, look: Look = .terminal, options: Bool = true) async throws -> Rig {
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
        let transport = ChatTransport(chats: chats ?? [chat()], appearance: appearance(look))
        if !options { await transport.setOptions(nil) }
        let model = RemoteModel(client: transport, keychain: keychain, defaults: UserDefaults(suiteName: suite)!, chatWaitMilliseconds: 300, chatIdleInterval: .milliseconds(20),
                                hardwareKeyboard: HardwareKeyboardMonitor(probe: { hardwareKeyboard }))
        await model.connect()
        if look != .terminal { await eventually("the desktop's Native look is in") { model.theme.style.native } }
        let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))
        let root = AnyView(TerminalTabsView(model: model, project: projectValue, onBack: {}).desktopThemed(model.theme.style))
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
            var rig = try await makeRig(chats: mixed, hardwareKeyboard: false, width: screen.width, height: screen.height, look: look)
            await rig.transport.append(chatID, conversationEvents())
            let field = try await openChat(rig)
            _ = field.becomeFirstResponder()
            try await Task.sleep(for: .milliseconds(1200))
            try await hold(named("attach-chat", look))
            await finish(rig)

            // The terminal's key bar above the keyboard, scrolled to its paperclip.
            rig = try await makeRig(hardwareKeyboard: false, width: screen.width, height: screen.height, look: look)
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
            var rig = try await makeRig(chats: [chat(state: .idle), mixed[1]], hardwareKeyboard: false, width: screen.width, height: screen.height, look: look)
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
            rig = try await makeRig(chats: [chat(state: .running), mixed[1]], hardwareKeyboard: false, width: screen.width, height: screen.height, look: look)
            await rig.transport.append(chatID, [.info(chat(state: .running)), .turnStarted(turnID: "t0"), .state(.running)])
            _ = try await openChat(rig).becomeFirstResponder()
            await eventually("the turn is running") { rig.model.chatState(self.chat()).isBusy }
            rig.model.conversation(chatID).draft = "Also check the docs"
            try await hold(named("row-chat-running", look))
            await finish(rig)

            rig = try await makeRig(hardwareKeyboard: false, width: screen.width, height: screen.height, look: look)
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
        for look in Look.allCases { try await questionAndStatusLines(look) }
    }
    private func questionAndStatusLines(_ look: Look) async throws {
        let rig = try await makeRig(chats: [chat(state: .failed("the process exited with status 1"))], look: look)
        await rig.transport.append(chatID, [
            .itemCompleted(ChatItem(id: "u1", status: .completed, body: .userMessage("run the tests"))),
            .state(.failed("the process exited with status 1")),
            .questionRequested(ChatQuestion(requestID: "q1", questions: [ChatQuestionPrompt(header: "Scope", question: "Which tests should I run?", options: [ChatQuestionOption(label: "All", description: "The full suite, about a minute"), ChatQuestionOption(label: "Changed", description: "Only what the diff touches")], multiSelect: false)]))
        ])
        _ = try await openChat(rig)
        await eventually("the question and the failure") { rig.model.conversation(self.chatID).openQuestions.count == 1 && rig.model.chatState(self.chat()) == .failed("the process exited with status 1") }
        try await Task.sleep(for: .milliseconds(400))
        try snapshot(rig, name: named("chat-question-failed", look))
        await finish(rig)
    }
    // MARK: The Model menu

    /// The pull-downs of the chat's toolbar, left to right: the views SwiftUI gives a context menu that opens on a tap. Their spoken
    /// labels are not on those views, so the toolbar is found as the first row from the top that has more than one of them (the tab
    /// strip above it has one).
    private func toolbarMenus(_ rig: Rig) -> [(view: UIView, menu: UIContextMenuInteraction, frame: CGRect)] {
        func all(_ view: UIView) -> [(view: UIView, menu: UIContextMenuInteraction, frame: CGRect)] {
            let own = view.interactions.compactMap { $0 as? UIContextMenuInteraction }.first.map { [(view: view, menu: $0, frame: view.convert(view.bounds, to: rig.window))] } ?? []
            return (view is UITextView || view === rig.host.view ? [] : own) + view.subviews.flatMap(all)
        }
        let rows = Dictionary(grouping: all(rig.host.view)) { Int($0.frame.minY.rounded()) }
        return rows.keys.sorted().lazy.compactMap { rows[$0] }.first { $0.count > 1 }?.sorted { $0.frame.minX < $1.frame.minX } ?? []
    }
    private func modelChat(_ model: String?, effort: String? = "high") -> ChatInfo {
        var info = chat(state: .running, mode: .autoEdit)
        info.model = model; info.effort = effort
        return info
    }

    func testTheModelMenuIsBetweenModeAndCompactAndAnOlderMacHasNone() async throws {
        let rig = try await makeRig(chats: [modelChat("claude-sonnet-4-5-20250929")])
        await rig.transport.append(chatID, [.info(modelChat("claude-sonnet-4-5-20250929"))])
        _ = try await openChat(rig)
        await eventually("the options are in") { rig.model.chatOptionsSupport == .supported }
        await eventually("mode, model and ⋯ are on the toolbar") { self.toolbarMenus(rig).count == 3 }
        // A long name is cut, not the controls: the mode menu keeps its width, and Compact and ⋯ their place at the end.
        let menus = toolbarMenus(rig)
        if menus.count == 3 {
            let (mode, model, more) = (menus[0].frame, menus[1].frame, menus[2].frame)
            XCTAssertGreaterThan(model.width, 80, "the model's name has room to be read")
            XCTAssertGreaterThanOrEqual(model.minX, mode.maxX)
            XCTAssertLessThanOrEqual(model.maxX, more.minX - 80, "Compact keeps its room between the model and ⋯")
            XCTAssertEqual(more.maxX, rig.window.bounds.width, accuracy: 8)
        }
        await finish(rig)

        let older = try await makeRig(chats: [modelChat("opus")], options: false)
        _ = try await openChat(older)
        await eventually("the Mac says it has no options") { older.model.chatOptionsSupport == .unsupported }
        await eventually("mode and ⋯ are on the toolbar") { self.toolbarMenus(older).count == 2 }
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(toolbarMenus(older).count, 2, "no Model menu for a Mac that offers no models")
        await finish(older)
    }

    /// Pictures of the toolbar and of the open Model menu in the three looks, taken by the simulator itself (a system menu and Liquid
    /// Glass are drawn only on screen): with `RIWORK_CHAT_MODEL_SCREENSHOTS` set to a directory, this leaves `<name>.ready` while a state
    /// is on screen and waits for `<name>.png`, which a loop running `xcrun simctl io <udid> screenshot` beside the test takes.
    func testPicturesOfTheModelMenu() async throws {
        guard let path = ProcessInfo.processInfo.environment["RIWORK_CHAT_MODEL_SCREENSHOTS"] else { throw XCTSkip("Set RIWORK_CHAT_MODEL_SCREENSHOTS") }
        let directory = URL(fileURLWithPath: path)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        func save(_ name: String) async throws {
            let ready = directory.appendingPathComponent(name + ".ready"), shot = directory.appendingPathComponent(name + ".png")
            try? FileManager.default.removeItem(at: shot)
            try Data().write(to: ready)
            let deadline = Date().addingTimeInterval(20)
            while !FileManager.default.fileExists(atPath: shot.path), Date() < deadline { try await Task.sleep(for: .milliseconds(200)) }
            try? FileManager.default.removeItem(at: ready)
            XCTAssertTrue(FileManager.default.fileExists(atPath: shot.path), "no screenshot taken for \(name)")
        }
        let bounds = UIScreen.main.bounds
        for look in Look.allCases {
            for (label, model) in [("short", "opus"), ("long", "claude-sonnet-4-5-20250929")] {
                let rig = try await makeRig(chats: [modelChat(model)], hardwareKeyboard: false, width: bounds.width, height: bounds.height, look: look)
                rig.window.windowLevel = .alert + 1
                await rig.transport.append(chatID, conversationEvents() + [.info(modelChat(model))])
                _ = try await openChat(rig)
                rig.window.endEditing(true)
                await eventually("the menu is on the toolbar") { self.toolbarMenus(rig).count == 3 }
                try await Task.sleep(for: .milliseconds(600))
                try await save(named("chat-model-toolbar-\(label)", look))
                if label == "long", let (view, menu, _) = toolbarMenus(rig).dropFirst().first {
                    // The tap that opens a pull-down, as UIKit's own button does it.
                    let open = NSSelectorFromString("_presentMenuAtLocation:")
                    if menu.responds(to: open) { menu.perform(open, with: NSValue(cgPoint: CGPoint(x: view.bounds.midX, y: view.bounds.midY))) }
                    try await Task.sleep(for: .milliseconds(900))
                    try await save(named("chat-model-menu", look))
                    menu.dismissMenu()
                    try await Task.sleep(for: .milliseconds(400))
                }
                await finish(rig)
            }
        }
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
}
