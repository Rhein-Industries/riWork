import XCTest
import SwiftUI
import UIKit
import Vision
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
    /// The navigation stack's path, for a test to go back and forth.
    @MainActor @Observable final class StackPath { var items = [1] }
    /// The tab screen pushed on a navigation stack with its bar hidden, as the app shows it.
    private struct PushedTabs: View {
        let model: RemoteModel
        let project: RemoteProject
        @Bindable var stack: StackPath
        var body: some View {
            NavigationStack(path: $stack.items) {
                Text("Projects").toolbar(.hidden, for: .navigationBar)
                    .navigationDestination(for: Int.self) { _ in
                        TerminalTabsView(model: model, project: project, onBack: { stack.items.removeAll() }).desktopThemed(model.theme.style)
                            .toolbar(.hidden, for: .navigationBar).edgeSwipeBack()
                    }
            }
        }
    }
    private struct Rig {
        let model: RemoteModel, transport: ChatTransport, window: UIWindow, host: UIHostingController<AnyView>, keychain: KeychainStore
        let layout: ChatLayoutInspection
        let defaults: UserDefaults
        let stack: StackPath
    }
    private func makeRig(chats: [ChatInfo]? = nil, orchestrators: [String] = [], chatFeature: Bool = true, orchestratorCreate: Bool = false, hardwareKeyboard: Bool = true, width: CGFloat = 402, height: CGFloat = 874, look: Look = .terminal, mic: Bool = false, pushed: Bool = false, defaults suiteName: String? = nil) async throws -> Rig {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene to show a chat in") }
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = ChatTransport.shell
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        let suite = suiteName ?? "com.riwork.tests.chatview.\(UUID().uuidString)"
        if suiteName == nil { defaultsNames.append(suite) }
        let transport = ChatTransport(chats: chats ?? [chat()], appearance: appearance(look, mic: mic))
        await transport.setFeature(chatFeature)
        await transport.setOrchestratorFeature(orchestratorCreate)
        await transport.setOrchestrators(orchestrators)
        let defaults = UserDefaults(suiteName: suite)!
        let model = RemoteModel(client: transport, keychain: keychain, defaults: defaults, chatWaitMilliseconds: 300, chatIdleInterval: .milliseconds(20),
                                hardwareKeyboard: HardwareKeyboardMonitor(probe: { hardwareKeyboard }))
        await model.connect()
        if look != .terminal { await eventually("the desktop's Native look is in") { model.theme.style.native } }
        if mic { await eventually("the desktop's mic setting is in") { model.theme.style.mic } }
        let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))
        let layout = ChatLayoutInspection()
        let stack = StackPath()
        let root = pushed ? AnyView(PushedTabs(model: model, project: projectValue, stack: stack).environment(\.chatLayoutInspection, layout))
            : AnyView(ThemedTabs(model: model, project: projectValue).environment(\.chatLayoutInspection, layout))
        let host = UIHostingController(rootView: root)
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: width, height: height)
        window.rootViewController = host
        if look != .terminal { window.overrideUserInterfaceStyle = look == .nativeDark ? .dark : .light }
        window.makeKeyAndVisible()
        windows.append(window)
        await eventually("the terminal is on screen") { !self.descendants(KeyCaptureView.self, in: host.view).isEmpty && model.terminalArea != nil }
        return Rig(model: model, transport: transport, window: window, host: host, keychain: keychain, layout: layout, defaults: defaults, stack: stack)
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

    func testLatestFirstOpensBoundedFullItemsAndPreservesReaderAcrossHistoryAndLiveEdits() async throws {
        let screen = UIScreen.main.bounds.size
        let rig = try await makeRig(width: screen.width, height: screen.height, look: .nativeLight)
        await rig.transport.enableSnapshots()
        var events: [ChatEvent] = [.info(chat()), approval("older-request"), .models(models), .usage(ChatUsage(inputTokens: 1234))]
        events += (0..<1500).map { .itemCompleted(ChatItem(id: "recent-\($0)", status: .completed, body: .agentMessage($0 == 1499 ? "Latest message 1499. Complete." : "Message \($0). " + String(repeating: "A complete paragraph with varying row height. ", count: $0 % 4 + 1)))) }
        await rig.transport.append(chatID, events)
        let started = ContinuousClock.now
        let field = try await openChat(rig)
        let conversation = rig.model.conversation(chatID)
        await eventually("only the newest 50 full messages are loaded") { conversation.feed.loaded && conversation.transcript.items.count == 50 }
        XCTAssertEqual(conversation.transcript.items.first?.id, "recent-1450")
        XCTAssertEqual(conversation.transcript.items.last?.id, "recent-1499")
        XCTAssertEqual(conversation.openApprovals.first?.requestID, "older-request")
        XCTAssertEqual(conversation.transcript.models.count, 2)
        XCTAssertEqual(conversation.transcript.usage?.inputTokens, 1234)
        let polls = await rig.transport.params(of: "chat.events")
        XCTAssertTrue(polls.allSatisfy { $0["since"] == .number(Double(events.count)) }, "open must not replay old event pages")
        print("LATEST_FIRST open latency \(started.duration(to: .now)), 1500 rows -> 50 rows, cursor \(conversation.feed.next)")
        conversation.draft = "unsent draft"
        try await Task.sleep(for: .milliseconds(450))
        try snapshot(rig, name: "latest-first-initial")
        try await hold("latest-first-initial")
        let list = try XCTUnwrap(descendants(UIScrollView.self, in: rig.host.view).filter { $0 !== field && $0.bounds.height > 20 && $0.contentSize.height > $0.bounds.height + 500 }.max { $0.contentSize.height < $1.contentSize.height })
        assertValidBottom(list)
        XCTAssertTrue(try renderedTranscriptText(rig, scroll: list).contains("1499"), "the latest message is visible on initial open")
        // Move to a partly visible row; the toolbar's history button stays reachable to VoiceOver.
        list.delegate?.scrollViewWillBeginDragging?(list)
        try await Task.sleep(for: .milliseconds(50))
        list.setContentOffset(CGPoint(x: 0, y: 20), animated: false)
        try await Task.sleep(for: .milliseconds(150))
        let oldHeight = list.contentSize.height, oldOffset = list.contentOffset.y
        let anchorTop = try renderedMessageTop("1450", rig: rig, scroll: list)
        try snapshot(rig, name: "latest-first-before-page")
        print("LATEST_FIRST reader offset \(oldOffset) height \(oldHeight) viewport \(list.bounds.height)")
        list.delegate?.scrollViewDidEndDragging?(list, willDecelerate: false)
        try await Task.sleep(for: .milliseconds(300))
        if conversation.transcript.items.count == 50 { XCTAssertTrue(activate("Load older messages", in: rig.host.view)) }
        await eventually("history prepended") { conversation.transcript.items.count == 100 }
        try await Task.sleep(for: .milliseconds(500))
        XCTAssertEqual(try renderedMessageTop("1450", rig: rig, scroll: list), anchorTop, accuracy: 4, "prepending preserves the actual visible message, independent of lazy height estimates")
        let reader = list.contentOffset.y
        await rig.transport.append(chatID, [.itemDelta(itemID: "recent-1470", delta: .text(" live edit")), .approvalResolved(requestID: "older-request", decision: .accept)])
        await eventually("live controls arrive") { conversation.openApprovals.isEmpty }
        if case .agentMessage(let text) = conversation.transcript.item("recent-1470")?.body { XCTAssertTrue(text.hasSuffix(" live edit")) } else { XCTFail("missing full live row") }
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(list.contentOffset.y, reader, accuracy: 4, "an active reader opts out of bottom following")
        XCTAssertEqual(conversation.draft, "unsent draft")
        try snapshot(rig, name: "latest-first-history-reader")
        try await hold("latest-first-history-reader")
        await rig.transport.append(chatID, [.itemDelta(itemID: "recent-2", delta: .text(" old stream"))])
        await eventually("preexisting old item hydrated with its base") { conversation.feed.hasItem("recent-2") }
        XCTAssertEqual(conversation.transcript.items.first?.id, "recent-1400", "out-of-window live edits must not insert rows above the reader")
        await finish(rig)
    }

    func testLatestFirstCancelledBootstrapIsIsolatedAndOlderDesktopFallsBackExplicitly() async throws {
        let otherID = "dddddddd-1111-4111-8111-111111111111"
        let other = ChatInfo(id: otherID, provider: .claude, projectID: project, cwd: "/fixture", title: "Other chat", createdAtUnix: 11, approvalMode: .supervised, state: .idle)
        let rig = try await makeRig(chats: [chat(), other])
        await rig.transport.enableSnapshots(); await rig.transport.gateSnapshots(true)
        await rig.transport.append(otherID, [.info(other), .itemCompleted(ChatItem(id: "other-row", status: .completed, body: .agentMessage("Other chat only")))])
        rig.model.selectChat(chatID)
        await eventually("snapshot request started") { await rig.transport.count("chat.snapshot") > 0 }
        rig.model.conversation(chatID).draft = "kept while switching"
        rig.model.selectChat(otherID)
        await eventually("follower cancelled on direct chat switch") { !rig.model.conversation(self.chatID).following && rig.model.conversation(otherID).following }
        await rig.transport.gateSnapshots(false)
        await eventually("only the second tab installs its snapshot") { rig.model.conversation(otherID).transcript.item("other-row") != nil }
        XCTAssertEqual(rig.model.conversation(otherID).transcript.items.map(\.id), ["other-row"])
        XCTAssertFalse(rig.model.conversation(chatID).feed.loaded)
        XCTAssertEqual(rig.model.conversation(chatID).draft, "kept while switching")
        await rig.transport.enableSnapshots(false)
        await rig.transport.append(chatID, [.info(chat()), .itemCompleted(ChatItem(id: "legacy", status: .completed, body: .agentMessage("legacy complete")))])
        _ = try await openChat(rig)
        await eventually("legacy replay explicitly selected") { rig.model.conversation(self.chatID).transcript.item("legacy") != nil }
        // The note about the older desktop goes by itself once its history is in.
        await eventually("the recent-first note has done its work") { !rig.model.conversation(self.chatID).legacyLoading && rig.model.conversation(self.chatID).alerts.text(.desktop) == nil }
        XCTAssertNil(rig.model.conversation(chatID).feed.historyCursor)
        await finish(rig)
    }

    func testLatestFirstHistoryKeepsInterleavedLiveChangesAndCancelledPagesOutOfAnotherTab() async throws {
        let rig = try await makeRig()
        await rig.transport.enableSnapshots()
        let rows: [ChatEvent] = (0..<300).map { .itemCompleted(ChatItem(id: "row-\($0)", status: .completed, body: .agentMessage("base \($0)"))) }
        await rig.transport.append(chatID, [.info(chat()), approval("old-request")] + rows)
        _ = try await openChat(rig)
        let conversation = rig.model.conversation(chatID)
        await eventually("snapshot loaded") { conversation.feed.loaded }
        await rig.transport.gateSnapshots(true)
        let history = Task { await rig.model.loadOlderChat(self.chatID) }
        await eventually("history waits") { conversation.historyLoading }
        await rig.transport.append(chatID, [.itemDelta(itemID: "row-270", delta: .text(" LIVE")), .approvalResolved(requestID: "old-request", decision: .accept), .state(.running)])
        await eventually("live update while history waits") { conversation.transcript.state == .running && conversation.openApprovals.isEmpty }
        let liveNext = conversation.feed.next
        await rig.transport.gateSnapshots(false)
        await history.value
        XCTAssertEqual(conversation.feed.next, liveNext)
        XCTAssertEqual(conversation.transcript.item("row-270")?.body, .agentMessage("base 270 LIVE"))
        XCTAssertEqual(conversation.transcript.items.count, 100)
        await rig.transport.drop()
        await rig.model.connect()
        _ = try await openChat(rig)
        await rig.transport.append(chatID, [.itemDelta(itemID: "row-270", delta: .text(" RECONNECTED"))])
        await eventually("reconnect resumes the cursor without replay or duplicates") { conversation.transcript.item("row-270")?.body == .agentMessage("base 270 LIVE RECONNECTED") }
        XCTAssertEqual(Set(conversation.transcript.items.map(\.id)).count, 100)
        let snapshotCalls = await rig.transport.params(of: "chat.snapshot")
        XCTAssertEqual(snapshotCalls.filter { $0["cursor"] == nil }.count, 1)
        let before = conversation.feed.before
        await rig.transport.gateSnapshots(true)
        let cancelled = Task { await rig.model.loadOlderChat(self.chatID) }
        await eventually("second history waits") { conversation.historyLoading }
        rig.model.deselectChat(); cancelled.cancel()
        await cancelled.value
        await rig.transport.gateSnapshots(false)
        XCTAssertEqual(conversation.feed.before, before)
        XCTAssertEqual(conversation.transcript.items.count, 100)
        await finish(rig)
    }

    func testLatestFirstGatedPageIgnoresLiveBottomGrowthAndFollowsReaderAndLateLayout() async throws {
        let screen = UIScreen.main.bounds.size
        let rig = try await makeRig(width: screen.width, height: screen.height, look: .nativeLight)
        rig.host.traitOverrides.preferredContentSizeCategory = .accessibilityExtraLarge
        await rig.transport.enableSnapshots()
        let rows: [ChatEvent] = (0..<300).map { n in
            let body: ChatItemBody = n % 5 == 4
                ? .command(command: "synthetic command \(n)", cwd: "/fixture", output: String(repeating: "Expanded output \(n)\n", count: 12), exitCode: 0)
                : .agentMessage("Message \(n). " + String(repeating: "A paragraph with heterogeneous height. ", count: n % 3 + 1))
            return .itemCompleted(ChatItem(id: "hetero-\(n)", status: .completed, body: body))
        }
        await rig.transport.append(chatID, [.info(chat())] + rows)
        let field = try await openChat(rig), conversation = rig.model.conversation(chatID)
        await eventually("recent window loaded") { conversation.transcript.items.count == 50 }
        conversation.draft = "gated draft"; conversation.expanded.insert("hetero-254")
        rig.window.endEditing(true)
        try await Task.sleep(for: .milliseconds(350))
        let list = try XCTUnwrap(descendants(UIScrollView.self, in: rig.host.view).filter { $0 !== field && $0.bounds.height > 100 && $0.contentSize.height > $0.bounds.height + 500 }.max { $0.contentSize.height < $1.contentSize.height })
        await rig.transport.gateHistory(true)
        list.delegate?.scrollViewWillBeginDragging?(list)
        try await Task.sleep(for: .milliseconds(100))
        list.setContentOffset(CGPoint(x: 0, y: 20), animated: false)
        try await Task.sleep(for: .milliseconds(150))
        list.delegate?.scrollViewDidEndDragging?(list, willDecelerate: false)
        await eventually("history request gated") { conversation.historyLoading }
        let readerMessage = try firstRenderedMessage(rig, scroll: list)
        let beforeGrowth = try renderedMessageTop(readerMessage, rig: rig, scroll: list)
        await rig.transport.append(chatID, [.itemCompleted(ChatItem(id: "bottom-live", status: .completed, body: .agentMessage(String(repeating: "New bottom paragraph. ", count: 30))))])
        await eventually("one independent live arrival") { conversation.feed.itemArrivals == 51 }
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(try renderedMessageTop(readerMessage, rig: rig, scroll: list), beforeGrowth, accuracy: 4, "bottom growth while the page waits must not move the reader")
        list.delegate?.scrollViewWillBeginDragging?(list)
        list.setContentOffset(CGPoint(x: 0, y: 28), animated: false)
        try await Task.sleep(for: .milliseconds(120))
        let movedMessage = try firstRenderedMessage(rig, scroll: list)
        let movedAnchor = try renderedMessageTop(movedMessage, rig: rig, scroll: list)
        await rig.transport.gateHistory(false)
        try await Task.sleep(for: .milliseconds(120))
        XCTAssertEqual(conversation.transcript.items.count, 51, "page installation waits for this new gesture")
        list.delegate?.scrollViewDidEndDragging?(list, willDecelerate: false)
        await eventually("page installed with the live row retained") { conversation.transcript.items.count == 101 }
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(try renderedMessageTop(movedMessage, rig: rig, scroll: list), movedAnchor, accuracy: 4)
        XCTAssertEqual(conversation.feed.itemArrivals, 51, "the 50 older rows add no unread arrivals")
        // A newly prepended card measures again well after the former timer deadline.
        conversation.expanded.insert("hetero-249")
        try await Task.sleep(for: .milliseconds(400))
        XCTAssertEqual(try renderedMessageTop(movedMessage, rig: rig, scroll: list), movedAnchor, accuracy: 4, "late expansion above the anchor preserves its within-row position")
        XCTAssertEqual(conversation.draft, "gated draft")
        try assertLatestOverTranscriptBottom(rig, scroll: list)
        try snapshot(rig, name: "latest-first-gated-large-text")
        await finish(rig)
    }

    func testLatestFirstCachedChatSwitchCancelsPagingAndResetsScrollWithoutLosingDrafts() async throws {
        let otherID = "dddddddd-1111-4111-8111-111111111111"
        let other = ChatInfo(id: otherID, provider: .claude, projectID: project, cwd: "/fixture", title: "Cached other", createdAtUnix: 11, approvalMode: .supervised, state: .idle)
        let screen = UIScreen.main.bounds.size
        let rig = try await makeRig(chats: [chat(), other], width: screen.width, height: screen.height, look: .nativeLight)
        await rig.transport.enableSnapshots()
        await rig.transport.append(chatID, [.info(chat())] + (0..<300).map { .itemCompleted(ChatItem(id: "cached-\($0)", status: .completed, body: .agentMessage("Message \($0). A complete cached message."))) })
        await rig.transport.append(otherID, [.info(other), .itemCompleted(ChatItem(id: "other-cached", status: .completed, body: .agentMessage("Other latest 999.")))])
        _ = try await openChat(rig)
        let first = rig.model.conversation(chatID); first.draft = "draft A"
        await eventually("A cached") { first.feed.loaded }
        rig.model.selectChat(otherID)
        let second = rig.model.conversation(otherID); second.draft = "draft B"
        await eventually("B cached") { second.feed.loaded && !first.following }
        _ = try await openChat(rig)
        let field = try XCTUnwrap(composer(rig))
        try await Task.sleep(for: .milliseconds(250))
        let list = try XCTUnwrap(descendants(UIScrollView.self, in: rig.host.view).filter { $0 !== field && $0.bounds.height > 20 && $0.contentSize.height > $0.bounds.height + 500 }.max { $0.contentSize.height < $1.contentSize.height })
        await rig.transport.gateHistory(true)
        list.delegate?.scrollViewWillBeginDragging?(list); list.setContentOffset(CGPoint(x: 0, y: 20), animated: false)
        try await Task.sleep(for: .milliseconds(100)); list.delegate?.scrollViewDidEndDragging?(list, willDecelerate: false)
        await eventually("A has a gated page") { first.historyLoading }
        rig.model.selectChat(otherID)
        await eventually("cached B owns the follower") { second.following && !first.following }
        await rig.transport.gateHistory(false)
        await eventually("A page cancellation settles") { !first.historyLoading }
        XCTAssertEqual(first.transcript.items.count, 50)
        XCTAssertEqual(second.transcript.items.map(\.id), ["other-cached"])
        _ = try await openChat(rig)
        try await Task.sleep(for: .milliseconds(300))
        let reopenedField = try XCTUnwrap(composer(rig))
        let reopened = try XCTUnwrap(descendants(UIScrollView.self, in: rig.host.view).filter { $0 !== reopenedField && $0.bounds.height > 20 && $0.contentSize.height > $0.bounds.height + 500 }.max { $0.contentSize.height < $1.contentSize.height })
        assertValidBottom(reopened)
        XCTAssertTrue(try renderedTranscriptText(rig, scroll: reopened).contains("299"))
        XCTAssertEqual(reopenedField.text, "draft A"); XCTAssertEqual(second.draft, "draft B")
        let bootstraps = await rig.transport.params(of: "chat.snapshot").filter { $0["cursor"] == nil }
        XCTAssertEqual(bootstraps.count, 2, "cached switches must not replay or bootstrap again")
        await finish(rig)
    }

    func testLatestFirstResourceRecoveryIsOnceBoundedAndKeepsActionableState() async throws {
        let screen = UIScreen.main.bounds.size
        for scenario in ["newest", "controls", "capped", "live", "hydration"] {
            let rig = try await makeRig(width: screen.width, height: screen.height, look: .nativeLight)
            await rig.transport.enableSnapshots(); await rig.transport.enforceResourceLimits(cappedLog: scenario == "capped")
            let large = ChatItem(id: "large", status: .completed, body: .agentMessage(String(repeating: "x", count: 200_000)))
            var initial: [ChatEvent] = [.info(chat()), approval("older-action"), .models(models), .usage(ChatUsage(inputTokens: 42))]
            if scenario == "newest" { initial.append(.itemCompleted(large)) }
            if scenario == "controls" {
                initial += (0..<200).map { .approvalRequested(ChatApproval(requestID: "action-\($0)", kind: .command, title: String(repeating: "a", count: 1000), choices: [.accept, .decline])) }
            }
            if scenario == "hydration" {
                initial += ["old-a", "old-b"].map { .itemCompleted(ChatItem(id: $0, status: .completed, body: .agentMessage(String(repeating: "b", count: 70_000)))) }
            }
            if scenario != "newest" { initial += (0..<(scenario == "live" ? 180 : 60)).map { .itemCompleted(ChatItem(id: "small-\($0)", status: .completed, body: .agentMessage("Complete small \($0)"))) } }
            await rig.transport.append(chatID, initial)
            let conversation = rig.model.conversation(chatID); conversation.draft = "resource draft"
            _ = try await openChat(rig)
            await eventually("\(scenario): initial load") { conversation.feed.loaded }
            if scenario == "live" {
                await rig.transport.gateBoundedReplay(after: 1)
                await rig.transport.append(chatID, [.models([models[1]])])
                await eventually("current catalogue received before recovery") { conversation.modelCatalogue == [self.models[1]] }
                await rig.transport.append(chatID, [.itemCompleted(large), approval("behind-large"), .state(.waiting)])
            } else if scenario == "hydration" {
                await rig.transport.append(chatID, [.itemDelta(itemID: "old-a", delta: .text(" LIVE A")), .itemDelta(itemID: "old-b", delta: .text(" LIVE B")), approval("behind-large"), .state(.waiting)])
            }
            if scenario == "live" {
                await eventually("historical first replay page is held at its checkpoint") { conversation.feed.degradedReplay && conversation.feed.next == 100 }
                XCTAssertEqual(conversation.modelCatalogue, [models[1]], "historical Models must not replace the authoritative live catalogue")
                XCTAssertEqual(conversation.transcript.models, [models[1]])
                XCTAssertEqual(conversation.openApprovals.first?.requestID, "older-action")
                await rig.transport.gateBoundedReplay(after: nil)
            }
            await eventually("\(scenario): exceptional recovery completes") { conversation.feed.degradedReplay && !conversation.legacyLoading }
            XCTAssertEqual(conversation.draft, "resource draft")
            XCTAssertEqual(conversation.openApprovals.first?.requestID, "older-action")
            XCTAssertEqual(conversation.modelCatalogue.count, scenario == "live" ? 1 : 2); XCTAssertEqual(conversation.transcript.usage?.inputTokens, 42)
            XCTAssertTrue(conversation.alerts.text(.desktop)?.contains("shortened") == true, "said in the banner row while the replay is degraded")
            let calls = await rig.transport.params(of: "chat.snapshot")
            XCTAssertEqual(calls.filter { $0["cursor"] == nil }.count, 1, "\(scenario): no unchanged bootstrap retries")
            if scenario == "controls" { XCTAssertEqual(conversation.openApprovals.count, 201) }
            if scenario == "newest" || scenario == "live" {
                if case .agentMessage(let body) = conversation.transcript.item("large")?.body { XCTAssertLessThan(body.utf8.count, 120_000); XCTAssertTrue(body.contains("shortened")) } else { XCTFail("represented newest row missing") }
            }
            if scenario == "live" || scenario == "hydration" {
                XCTAssertTrue(conversation.openApprovals.contains { $0.requestID == "behind-large" }); XCTAssertEqual(conversation.transcript.state, .waiting)
            }
            if scenario == "hydration" {
                XCTAssertEqual(calls.filter { $0["item_ids"] != nil }.count, 1)
                XCTAssertEqual(conversation.transcript.item("old-a")?.body, .agentMessage(String(repeating: "b", count: 70_000) + " LIVE A"))
            }
            let polls = await rig.transport.params(of: "chat.events")
            XCTAssertTrue(polls.contains { $0["bounded"] == .bool(true) })
            if scenario == "live" {
                let rejections = await rig.transport.rejectedCompleteEvents()
                XCTAssertEqual(rejections, 1, "only one deterministic complete-event size failure, distinct from idle polls")
            }
            if scenario == "capped" {
                await rig.transport.truncate(chatID, to: 1)
                await eventually("shortening during degraded replay retains its safe policy") { conversation.feed.loaded && conversation.feed.next == 1 && conversation.feed.degradedReplay }
                XCTAssertEqual(conversation.draft, "resource draft")
            }
            if scenario == "newest" {
                rig.window.endEditing(true)
                let field = try XCTUnwrap(composer(rig))
                let list = try XCTUnwrap(descendants(UIScrollView.self, in: rig.host.view).filter { $0 !== field && $0.bounds.height > 20 }.max { $0.contentSize.height < $1.contentSize.height })
                await eventually("represented body has settled at the bottom") { abs(list.contentOffset.y - self.bottomOffset(list)) < 2 && list.contentSize.height > 0 }
                try await Task.sleep(for: .milliseconds(350))
                await eventually("resource viewport remains at measured bottom after keyboard layout") { abs(list.contentOffset.y - self.bottomOffset(list)) < 2 }
                print("LATEST_FIRST_RESOURCE_VIEWPORT frame=\(list.convert(list.bounds, to: rig.window)) offset=\(list.contentOffset) content=\(list.contentSize) window=\(rig.window.bounds)")
                try snapshot(rig, name: "latest-first-resource-recovery")
            }
            await finish(rig)
        }
        // An individually unrepresentable control stops once, preserving its cursor.
        let rig = try await makeRig()
        await rig.transport.enableSnapshots(); await rig.transport.enforceResourceLimits()
        await rig.transport.append(chatID, [.info(chat()), .approvalRequested(ChatApproval(requestID: "unrepresented", kind: .command, title: String(repeating: "c", count: 200_000), choices: [.accept]))])
        _ = try await openChat(rig)
        let conversation = rig.model.conversation(chatID)
        await eventually("bounded control failure stops its follower") { conversation.readError != nil && !conversation.following }
        let count = await rig.transport.count("chat.events"), cursor = conversation.feed.next
        try await Task.sleep(for: .milliseconds(350))
        let afterWait = await rig.transport.count("chat.events")
        XCTAssertEqual(afterWait, count)
        XCTAssertEqual(cursor, 1, "never acknowledge the unrepresented approval")
        rig.model.deselectChat(); rig.model.selectChat(chatID)
        try await Task.sleep(for: .milliseconds(100))
        let afterReopen = await rig.transport.count("chat.events")
        XCTAssertEqual(afterReopen, count)
        await finish(rig)
    }

    func testLatestFirstMalformedDegradedRepliesBlockReopenButTransientFailuresRecover() async throws {
        for failure in [ChatTransport.BoundedReplyFailure.advancingEmptyPage, .connectorInvalidPage, .transientCLI, .transientNetwork] {
            let rig = try await makeRig()
            await rig.transport.enableSnapshots(); await rig.transport.enforceResourceLimits(cappedLog: true)
            await rig.transport.failBoundedReplies(failure)
            let initial: [ChatEvent] = [.info(chat()), approval("current-control"), .state(.waiting), .usage(ChatUsage(inputTokens: 42)), .models(models)]
            await rig.transport.append(chatID, initial)
            let conversation = rig.model.conversation(chatID); conversation.draft = "protocol draft"
            _ = try await openChat(rig)
            await eventually("represented controls reach the recovery cursor") { conversation.feed.next == UInt64(initial.count) }
            switch failure {
            case .advancingEmptyPage, .connectorInvalidPage:
                await eventually("malformed recovery blocks this connection") { conversation.readError == .unreadableReply && !conversation.following }
                XCTAssertNotNil(conversation.resourceBlockedGeneration)
                let failures = await rig.transport.boundedFailures()
                XCTAssertEqual(failures, 1)
                let count = await rig.transport.count("chat.events")
                try await Task.sleep(for: .milliseconds(350))
                let afterWait = await rig.transport.count("chat.events")
                XCTAssertEqual(afterWait, count, "no unchanged deterministic retry")
                rig.model.deselectChat(); rig.model.selectChat(chatID)
                try await Task.sleep(for: .milliseconds(150))
                let afterReopen = await rig.transport.count("chat.events")
                XCTAssertEqual(afterReopen, count, "same connection cannot repeat the malformed page")
                XCTAssertEqual(conversation.feed.next, UInt64(initial.count), "never advance over unrepresented state")
                XCTAssertEqual(conversation.transcript.state, .waiting)
            case .transientCLI, .transientNetwork:
                await eventually("transient failure was delivered") { await rig.transport.boundedFailures() == 1 }
                await rig.transport.append(chatID, [.state(.running)])
                await eventually("backoff retries and applies the next live state") { conversation.feed.next == UInt64(initial.count + 1) && conversation.readError == nil }
                XCTAssertNil(conversation.resourceBlockedGeneration)
                XCTAssertTrue(conversation.following)
                XCTAssertEqual(conversation.transcript.state, .running)
            }
            XCTAssertEqual(conversation.openApprovals.first?.requestID, "current-control")
            XCTAssertEqual(conversation.transcript.usage?.inputTokens, 42)
            XCTAssertEqual(conversation.modelCatalogue, models)
            XCTAssertEqual(conversation.draft, "protocol draft")
            await finish(rig)
        }
    }

    func testLatestFirstExpiryAndTruncationStillRebootstrap() async throws {
        let rig = try await makeRig(); await rig.transport.enableSnapshots()
        await rig.transport.append(chatID, [.info(chat())] + (0..<60).map { .itemCompleted(ChatItem(id: "expire-\($0)", status: .completed, body: .agentMessage("Full \($0)"))) })
        _ = try await openChat(rig)
        let conversation = rig.model.conversation(chatID); conversation.draft = "expiry draft"
        await eventually("loaded before expiry") { conversation.feed.next == 61 }
        await rig.transport.expireHistory(); await rig.model.loadOlderChat(chatID)
        await eventually("expired pin installs a fresh snapshot") { await rig.transport.params(of: "chat.snapshot").filter { $0["cursor"] == nil }.count == 2 && conversation.feed.loaded }
        await rig.transport.truncate(chatID, to: 2)
        await eventually("shortened log installs its current prefix") { conversation.feed.next == 2 && conversation.feed.loaded }
        XCTAssertEqual(conversation.transcript.items.map(\.id), ["expire-0"])
        XCTAssertEqual(conversation.draft, "expiry draft")
        XCTAssertFalse(conversation.feed.degradedReplay)
        await finish(rig)
    }

    /// A shell and a chat share one navigation row: switching between them neither moves nor resizes it, in any look.
    func testShellAndChatShareTheSameNavigationRow() async throws {
        let screen = UIScreen.main.bounds.size
        for look in Look.allCases {
            let rig = try await makeRig(width: screen.width, height: screen.height, look: look)
            await rig.transport.append(chatID, [.info(chat())])
            await eventually("the shell's row has completed layout") { rig.layout.frames["navigation"] != nil }
            rig.window.layoutIfNeeded()
            let shell = try XCTUnwrap(rig.layout.frames["navigation"])
            XCTAssertEqual(shell.minY, rig.host.view.safeAreaInsets.top, accuracy: 1, "the row sits right under the status bar")
            XCTAssertGreaterThanOrEqual(shell.height, 44)
            XCTAssertLessThanOrEqual(shell.height, 48, "one compact row over a shell too")
            _ = try await openChat(rig)
            rig.window.layoutIfNeeded()
            let chat = try XCTUnwrap(rig.layout.frames["navigation"])
            XCTAssertEqual(chat.minY, shell.minY, accuracy: 0.5, "\(look): the row does not move")
            XCTAssertEqual(chat.height, shell.height, accuracy: 0.5, "\(look): the row keeps its height")
            XCTAssertEqual(chat.width, shell.width, accuracy: 0.5)
            rig.model.deselectChat()
            await eventually("the shell is back") { rig.model.selectedChat == nil }
            rig.window.layoutIfNeeded()
            let again = try XCTUnwrap(rig.layout.frames["navigation"])
            XCTAssertEqual(again, shell, "\(look): back on the shell, the row is where it was")
            // The smallest interface size makes the row's glyphs smaller, never its 44-point targets.
            rig.model.setInterfaceScale(0.8)
            try await Task.sleep(for: .milliseconds(200))
            rig.window.layoutIfNeeded()
            let small = try XCTUnwrap(rig.layout.frames["navigation"])
            XCTAssertGreaterThanOrEqual(small.height, 44, "\(look): the row stays a 44-point target at 80 %")
            await finish(rig)
        }
    }

    func testCompactSelectedTabPaintStaysInsideNavigationAndOutOfTopSafeArea() async throws {
        let screen = UIScreen.main.bounds.size
        for look in [Look.nativeLight, .terminal] {
            let rig = try await makeRig(width: screen.width, height: screen.height, look: look)
            await rig.transport.append(chatID, [.info(chat()), .itemCompleted(ChatItem(id: "paint", status: .completed, body: .agentMessage("A fixture conversation.")))])
            _ = try await openChat(rig)
            await eventually("navigation has completed layout") { rig.layout.frames["navigation"] != nil }
            rig.window.layoutIfNeeded()
            let navigation = try XCTUnwrap(rig.layout.frames["navigation"])
            let safeTop = rig.host.view.safeAreaInsets.top
            XCTAssertGreaterThan(safeTop, 0, "exercise the actual device top safe area")
            XCTAssertEqual(navigation.minY, safeTop, accuracy: 1)
            let image = UIGraphicsImageRenderer(bounds: rig.window.bounds).image { _ in
                rig.window.drawHierarchy(in: rig.window.bounds, afterScreenUpdates: true)
            }
            let cg = try XCTUnwrap(image.cgImage)
            let width = cg.width, height = cg.height, stride = width * 4
            let scale = CGFloat(height) / rig.window.bounds.height
            let topEnd = Int(floor(navigation.minY * scale))
            let rowEnd = Int(floor(navigation.maxY * scale))
            let style = rig.model.theme.style
            func rgb(_ color: UIColor) -> [Int] {
                var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, alpha: CGFloat = 0
                color.resolvedColor(with: rig.host.traitCollection).getRed(&r, green: &g, blue: &b, alpha: &alpha)
                return [r, g, b].map { Int(($0 * 255).rounded()) }
            }
            let background = rgb(style.backgroundUI), selected = rgb(style.activeUI)
            XCTAssertNotEqual(background, selected, "the selected paint must be distinguishable")
            var pixels = [UInt8](repeating: 0, count: stride * height)
            let counts = try pixels.withUnsafeMutableBytes { bytes -> (above: Int, inside: Int) in
                let context = try XCTUnwrap(CGContext(data: bytes.baseAddress, width: width, height: height, bitsPerComponent: 8, bytesPerRow: stride,
                                                     space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
                context.draw(cg, in: CGRect(x: 0, y: 0, width: width, height: height))
                let rgba = bytes.bindMemory(to: UInt8.self)
                func matches(_ offset: Int, _ color: [Int]) -> Bool {
                    (0..<3).allSatisfy { abs(Int(rgba[offset + $0]) - color[$0]) <= 3 } && rgba[offset + 3] == 255
                }
                var above = 0, inside = 0
                for y in 0..<rowEnd {
                    for x in 0..<width {
                        let offset = y * stride + x * 4
                        if y < topEnd { if !matches(offset, background) { above += 1 } }
                        else if matches(offset, selected) { inside += 1 }
                    }
                }
                return (above, inside)
            }
            XCTAssertGreaterThan(counts.inside, 100, "positive control: the selected tab is actually painted within the row")
            XCTAssertEqual(counts.above, 0, "no tab paint may bleed above navigation or into the status-bar safe area")
            print("COMPACT_SAFE_AREA look=\(look.rawValue) size=\(screen) safeTop=\(safeTop) navigation=\(navigation) scale=\(scale) aboveMismatchPixels=\(counts.above) selectedInsidePixels=\(counts.inside)")
            try compactSnapshot(rig, name: "compact-safe-area-" + look.rawValue)
            await finish(rig)
        }
    }

    func testCompactChatChromeReservesLatestAndKeepsLargeOutputAndControlsAboveKeyboard() async throws {
        let screen = UIScreen.main.bounds.size
        for category in [UIContentSizeCategory.large, .accessibilityExtraLarge] {
            let rig = try await makeRig(hardwareKeyboard: false, width: screen.width, height: screen.height, look: .nativeLight)
            rig.host.traitOverrides.preferredContentSizeCategory = category
            await rig.transport.enableSnapshots()
            let output = (0..<24).map { "output line \($0): compile fixture" }.joined(separator: "\n")
            var events: [ChatEvent] = [.info(chat()), .models(models), .usage(ChatUsage(inputTokens: 42))]
            events += (0..<60).map { .itemCompleted(ChatItem(id: "layout-\($0)", status: .completed, body: .agentMessage("Message \($0). A readable paragraph in this conversation."))) }
            events.append(.itemCompleted(ChatItem(id: "layout-command", status: .completed, body: .command(command: "swift test --filter fixture", cwd: "/fixture", output: output, exitCode: 0))))
            await rig.transport.append(chatID, events)
            let field = try await openChat(rig), conversation = rig.model.conversation(chatID)
            conversation.expanded.insert("layout-command"); conversation.draft = "Layout draft"
            await eventually("recent full rows loaded") { conversation.transcript.items.count == 50 }
            try await Task.sleep(for: .milliseconds(350))
            let scroll = try XCTUnwrap(transcriptScroll(rig))
            let modelFrame = try XCTUnwrap(rig.layout.frames["model"])
            let permissions = try XCTUnwrap(rig.layout.frames["permissions"])
            let navigation = try XCTUnwrap(rig.layout.frames["navigation"])
            XCTAssertGreaterThanOrEqual(modelFrame.height, 44)
            XCTAssertGreaterThanOrEqual(permissions.height, 44)
            XCTAssertLessThanOrEqual(navigation.height, 48, "project and tab navigation share one compact row")
            if category == .large {
                XCTAssertEqual(modelFrame.midY, permissions.midY, accuracy: 2, "routine model and permission controls fit one compact row")
                XCTAssertLessThanOrEqual(modelFrame.union(permissions).height, 48)
            }
            XCTAssertTrue(try renderedTranscriptText(rig, scroll: scroll).contains("output line 23"))
            XCTAssertFalse(try renderedTranscriptText(rig, scroll: scroll).contains("output line 0:"), "expanded cards begin with a short output preview")
            let expand = try XCTUnwrap(rig.layout.actions["output-toggle"])
            expand()
            await eventually("the visible output expands") { (try? self.renderedText(rig, in: rig.layout.frames["output-toggle"] ?? .zero).contains("Show less output")) == true }
            await eventually("expanded output settles at the newest bottom") { self.isAtValidBottom(scroll) }
            let copy = try XCTUnwrap(rig.layout.actions["copy-Copy output"])
            copy()
            XCTAssertEqual(UIPasteboard.general.string, output, "folded output still copies the full text held locally")
            try XCTUnwrap(rig.layout.actions["output-toggle"])()
            await eventually("the visible output folds again") { (try? self.renderedText(rig, in: rig.layout.frames["output-toggle"] ?? .zero).contains("Show more output")) == true }
            await eventually("folded output settles before reading begins") { self.isAtValidBottom(scroll) }
            if category == .accessibilityExtraLarge { try compactSnapshot(rig, name: "compact-large-text-output") }
            listStartReading(scroll)
            try await Task.sleep(for: .milliseconds(50))
            scroll.setContentOffset(CGPoint(x: 0, y: max(20, bottomOffset(scroll) - scroll.bounds.height - 200)), animated: false)
            try await Task.sleep(for: .milliseconds(150))
            scroll.delegate?.scrollViewDidEndDragging?(scroll, willDecelerate: false)
            await eventually("Latest shows while reading") { rig.layout.visible["latest"] == true }
            try await Task.sleep(for: .milliseconds(100))
            try assertLatestOverTranscriptBottom(rig, scroll: scroll)
            await rig.transport.append(chatID, [.itemCompleted(ChatItem(id: "layout-live", status: .completed, body: .agentMessage("One new live message.")))])
            await eventually("only the live row counts as new") { (try? self.renderedText(rig, in: rig.layout.frames["latest"] ?? .zero).contains("1 new")) == true }
            try assertLatestOverTranscriptBottom(rig, scroll: scroll)
            if category == .accessibilityExtraLarge { try compactSnapshot(rig, name: "compact-large-text-reader") }
            await rig.transport.append(chatID, [approval("layout-approval"), .state(.waiting)])
            await eventually("approval displayed") { conversation.openApprovals.count == 1 }
            var keyboardTop = screen.height
            let keyboard = NotificationCenter.default.addObserver(forName: UIResponder.keyboardWillChangeFrameNotification, object: nil, queue: .main) { note in
                if let frame = note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? CGRect { keyboardTop = frame.minY }
            }
            defer { NotificationCenter.default.removeObserver(keyboard) }
            field.becomeFirstResponder()
            await eventually("software keyboard is presented") { keyboardTop < screen.height - 100 && field.isFirstResponder }
            try await Task.sleep(for: .milliseconds(350))
            let composerFrame = field.convert(field.bounds, to: rig.window)
            XCTAssertLessThanOrEqual(composerFrame.maxY, keyboardTop + 2, "draft remains above the actual keyboard frame")
            XCTAssertEqual(field.text, "Layout draft")
            let allow = try XCTUnwrap(rig.layout.frames["approval-accept"])
            XCTAssertLessThanOrEqual(allow.maxY, composerFrame.minY + 2)
            XCTAssertGreaterThan(allow.height, 40)
            XCTAssertTrue(try renderedText(rig, in: CGRect(x: 0, y: scroll.convert(scroll.bounds, to: rig.window).maxY, width: screen.width, height: composerFrame.minY - scroll.convert(scroll.bounds, to: rig.window).maxY)).contains("Allow"), "approval action is visible above the keyboard")
            XCTAssertGreaterThan(scroll.bounds.height, 24, "conversation retains visible room above approvals")
            try assertLatestOverTranscriptBottom(rig, scroll: scroll)
            print("COMPACT_LAYOUT category=\(category.rawValue) size=\(screen) navigation=\(navigation.height) model=\(modelFrame.height) transcript=\(scroll.convert(scroll.bounds, to: rig.window)) latest=\(rig.layout.frames["latest"] ?? .zero) approval=\(allow) draft=\(composerFrame) keyboardTop=\(keyboardTop)")
            if category == .accessibilityExtraLarge { try compactSnapshot(rig, name: "compact-large-text-approval-keyboard") }
            await rig.transport.append(chatID, [.approvalResolved(requestID: "layout-approval", decision: .accept), .questionRequested(ChatQuestion(requestID: "layout-question", questions: [ChatQuestionPrompt(header: "Scope", question: "Which tests?", options: [ChatQuestionOption(label: "Changed", description: "Focused checks")], multiSelect: false)]))])
            await eventually("question displayed") { conversation.openQuestions.count == 1 }
            try await Task.sleep(for: .milliseconds(150))
            let answer = try XCTUnwrap(rig.layout.frames["question-send"])
            XCTAssertLessThanOrEqual(answer.maxY, field.convert(field.bounds, to: rig.window).minY + 2)
            XCTAssertEqual(conversation.draft, "Layout draft")
            await finish(rig)
        }
    }

    /// The transcript reaches down to the composer, with the software keyboard up and down: no band between them (Latest used to keep
    /// a row of its own there), no keyboard inset left inside the transcript, and Latest over the transcript's bottom edge.
    func testTranscriptReachesTheComposerWithTheKeyboardUpAndDown() async throws {
        let screen = UIScreen.main.bounds.size
        for look in [Look.nativeDark, .terminal] {
            let rig = try await makeRig(hardwareKeyboard: false, width: screen.width, height: screen.height, look: look, mic: true)
            await rig.transport.enableSnapshots()
            await rig.transport.append(chatID, [.info(chat()), .models(models), .state(.running)] + (0..<40).map {
                .itemCompleted(ChatItem(id: "band-\($0)", status: .completed, body: .agentMessage("Message \($0). A readable paragraph in this conversation, long enough to wrap.")))
            })
            let field = try await openChat(rig)
            await eventually("rows loaded") { rig.model.conversation(self.chatID).transcript.items.count >= 40 }
            try await Task.sleep(for: .milliseconds(350))
            func assertReaches(_ when: String) throws {
                let scroll = try XCTUnwrap(transcriptScroll(rig), when)
                let transcript = scroll.convert(scroll.bounds, to: rig.window)
                let fieldFrame = field.convert(field.bounds, to: rig.window)
                XCTAssertLessThanOrEqual(fieldFrame.minY - transcript.maxY, 14, "\(look) \(when): the transcript reaches the composer")
                XCTAssertGreaterThanOrEqual(fieldFrame.minY, transcript.maxY, "\(look) \(when): the composer is under the transcript")
                XCTAssertLessThanOrEqual(scroll.adjustedContentInset.bottom, 1, "\(look) \(when): no keyboard or bar inset counted inside the transcript")
            }
            try assertReaches("keyboard down")
            var keyboardTop = screen.height
            let keyboard = NotificationCenter.default.addObserver(forName: UIResponder.keyboardWillChangeFrameNotification, object: nil, queue: .main) { note in
                if let frame = note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? CGRect { keyboardTop = frame.minY }
            }
            defer { NotificationCenter.default.removeObserver(keyboard) }
            field.becomeFirstResponder()
            await eventually("software keyboard is presented") { keyboardTop < screen.height - 100 && field.isFirstResponder }
            try await Task.sleep(for: .milliseconds(400))
            try assertReaches("keyboard up")
            let fieldFrame = field.convert(field.bounds, to: rig.window)
            XCTAssertLessThanOrEqual(fieldFrame.maxY, keyboardTop + 2, "\(look): the composer sits on the keyboard")
            XCTAssertGreaterThanOrEqual(fieldFrame.maxY, keyboardTop - 16, "\(look): and close to it")
            let scroll = try XCTUnwrap(transcriptScroll(rig))
            listStartReading(scroll)
            scroll.setContentOffset(CGPoint(x: 0, y: max(20, bottomOffset(scroll) - 600)), animated: false)
            try await Task.sleep(for: .milliseconds(150))
            scroll.delegate?.scrollViewDidEndDragging?(scroll, willDecelerate: false)
            await eventually("Latest shows while reading") { rig.layout.visible["latest"] == true }
            try await Task.sleep(for: .milliseconds(100))
            try assertLatestOverTranscriptBottom(rig, scroll: scroll)
            try assertReaches("reading, keyboard up")
            await finish(rig)
        }
    }

    /// A swipe from the left edge goes back to the projects from a shell tab and from a chat, as on any pushed screen, although the
    /// screen hides the navigation bar for its own header (which turns UIKit's swipe off unless it is turned back on).
    func testTheEdgeSwipeGoesBackFromAShellAndFromAChat() async throws {
        let rig = try await makeRig(pushed: true)
        func find(_ controller: UIViewController) -> UINavigationController? {
            (controller as? UINavigationController) ?? controller.children.lazy.compactMap(find).first
        }
        let navigation = try XCTUnwrap(find(rig.host), "the screen is pushed on a navigation controller")
        func assertSwipeBack(_ screen: String) throws {
            let gesture = try XCTUnwrap(navigation.interactivePopGestureRecognizer)
            XCTAssertTrue(gesture.isEnabled, "\(screen): the edge swipe is on")
            XCTAssertTrue(gesture.delegate === EdgeSwipeBackDelegate.shared, "\(screen): with the delegate that allows it without a bar")
            XCTAssertEqual(navigation.viewControllers.count, 2, "\(screen): pushed over the projects")
            XCTAssertTrue(EdgeSwipeBackDelegate.shared.gestureRecognizerShouldBegin(gesture), "\(screen): it may begin")
            let scroll = UIScrollView(), pan = scroll.panGestureRecognizer
            XCTAssertTrue(EdgeSwipeBackDelegate.shared.gestureRecognizer(gesture, shouldBeRequiredToFailBy: pan), "\(screen): a scroll view at the edge waits for it")
        }
        try assertSwipeBack("shell")
        let field = try await openChat(rig)
        XCTAssertNotNil(field)
        try await Task.sleep(for: .milliseconds(300))
        try assertSwipeBack("chat")
        // Back at the first screen there is nothing to go back to.
        navigation.popViewController(animated: false)
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertFalse(EdgeSwipeBackDelegate.shared.gestureRecognizerShouldBegin(try XCTUnwrap(navigation.interactivePopGestureRecognizer)), "none on the first screen")
        await finish(rig)
    }

    // MARK: Drafts

    /// Types into the composer as a person does: through the text view, so the binding and the saving run as they do on a device.
    private func type(_ text: String, into field: ChatComposerTextView) {
        field.text = text
        field.delegate?.textViewDidChange?(field)
    }
    private func secondChat() -> ChatInfo {
        ChatInfo(id: "dddddddd-2222-4222-8222-222222222222", provider: .codex, projectID: project, cwd: "/fixture", title: "Second", createdAtUnix: 20, state: .idle)
    }

    func testADraftSurvivesSwitchingTabsAndTwoChatsKeepTheirOwn() async throws {
        let rig = try await makeRig(chats: [chat(), secondChat()])
        var field = try await openChat(rig)
        type("half a thought for the first chat", into: field)
        rig.model.deselectChat()
        await eventually("the shell is back") { self.composer(rig) == nil }
        rig.model.selectChat(secondChat().id)
        await eventually("the second chat is up") { self.composer(rig) != nil }
        field = try XCTUnwrap(composer(rig))
        XCTAssertEqual(field.text, "", "the second chat has its own, empty, composer")
        type("and one for the second", into: field)
        rig.model.selectChat(chatID)
        await eventually("the first chat's draft is back") { self.composer(rig)?.text == "half a thought for the first chat" }
        rig.model.selectChat(secondChat().id)
        await eventually("the second chat's draft is back") { self.composer(rig)?.text == "and one for the second" }
        await finish(rig)
    }

    func testADraftSurvivesGoingBackToTheProjectsAReconnectAndARelaunch() async throws {
        let rig = try await makeRig(pushed: true)
        let field = try await openChat(rig)
        type("kept across everything", into: field)
        // Back to the projects and in again.
        rig.stack.items = []
        await eventually("the projects are up") { self.composer(rig) == nil }
        rig.model.deselectChat()
        rig.stack.items = [1]
        await eventually("the tab screen is back") { !self.descendants(KeyCaptureView.self, in: rig.host.view).isEmpty }
        rig.model.selectChat(chatID)
        await eventually("the draft is back after leaving the project") { self.composer(rig)?.text == "kept across everything" }
        // The link drops and comes back; the conversations the desktop gave are let go and made again.
        await rig.model.disconnect()
        rig.model.chatConversations = [:]
        await rig.model.connect()
        rig.model.selectChat(chatID)
        await eventually("the draft is back after a reconnect") { self.composer(rig)?.text == "kept across everything" }
        let suite = try XCTUnwrap(defaultsNames.last)
        await finish(rig)
        // A new start of the app, on the same saved settings.
        let relaunched = try await makeRig(defaults: suite)
        let again = try await openChat(relaunched)
        XCTAssertEqual(again.text, "kept across everything", "a relaunch restores it")
        XCTAssertFalse(relaunched.model.conversation(chatID).notice?.contains("may not have reached") == true, "nothing was on its way")
        await finish(relaunched)
    }

    func testADraftIsClearedOnlyWhenSentAndAFailedSendKeepsIt() async throws {
        let rig = try await makeRig()
        let field = try await openChat(rig)
        type("ship it", into: field)
        await rig.transport.failCommand(.rpc(code: "unavailable", message: "The desktop is busy"))
        await rig.model.sendChatDraft(chatID)
        XCTAssertEqual(rig.model.conversation(chatID).draft, "ship it", "a refused message comes back")
        XCTAssertEqual(rig.model.chatDrafts.draft(chatID)?.text, "ship it")
        XCTAssertNil(rig.model.chatDrafts.draft(chatID)?.sending)
        await rig.transport.failCommand(nil)
        await rig.model.sendChatDraft(chatID)
        await eventually("the composer is empty") { self.composer(rig)?.text == "" }
        XCTAssertNil(rig.model.chatDrafts.draft(chatID), "sent: nothing kept")
        XCTAssertNil(ChatDraftStore(defaults: rig.defaults).draft(chatID), "and nothing saved for the next start")
        await finish(rig)
    }

    func testAMessageWhoseSendingWasNeverAnsweredComesBackWithAWarning() async throws {
        let suite = "com.riwork.tests.chatview.\(UUID().uuidString)"
        defaultsNames.append(suite)
        // The app ended while a message was on its way.
        let store = ChatDraftStore(defaults: UserDefaults(suiteName: suite)!)
        store.beginSending("deploy to staging", for: chatID)
        store.setText("then tell me", for: chatID)
        let rig = try await makeRig(defaults: suite)
        await rig.transport.enableSnapshots()
        let field = try await openChat(rig)
        XCTAssertEqual(field.text, "deploy to staging\nthen tell me")
        XCTAssertTrue(rig.model.conversation(chatID).notice?.contains("may not have reached the Mac") == true, "said, not sent again")
        let sent = await rig.transport.commands().count
        XCTAssertEqual(sent, 0, "nothing is sent by itself")
        XCTAssertNil(rig.model.chatDrafts.draft(chatID)?.sending, "now an ordinary draft")
        await finish(rig)
    }

    // MARK: Messages of the moment

    /// The provider's notices are not transcript rows: the latest of each kind of this turn is one banner line above the composer,
    /// replaced in place when it recurs, closed with ×, back only when said again, and all of them are in the history.
    func testProviderNoticesAreOneBannerPerKindAboveTheComposer() async throws {
        let rig = try await makeRig(look: .nativeDark)
        await rig.transport.enableSnapshots()
        let notice = { (id: String, text: String, level: ChatNoticeLevel) in ChatEvent.itemCompleted(ChatItem(id: id, status: .completed, body: .notice(level: level, text: text))) }
        await rig.transport.append(chatID, [.info(chat()), .itemCompleted(ChatItem(id: "u1", status: .completed, body: .userMessage("go"))),
                                            notice("n1", "Reconnecting… 1/5", .warning), notice("n2", "This account is close to the weekly usage limit", .warning)])
        let field = try await openChat(rig)
        let conversation = rig.model.conversation(chatID)
        await eventually("two notice lines") { rig.layout.frames.keys.filter { $0.hasPrefix("banner-notice-") }.count == 2 }
        let scroll = try XCTUnwrap(transcriptScroll(rig))
        XCTAssertFalse(try renderedTranscriptText(rig, scroll: scroll).contains("Reconnecting"), "not a transcript row")
        let banners = try XCTUnwrap(rig.layout.frames["banners"])
        XCTAssertLessThanOrEqual(banners.maxY, field.convert(field.bounds, to: rig.window).minY + 1, "directly above the composer")
        XCTAssertGreaterThanOrEqual(banners.minY, scroll.convert(scroll.bounds, to: rig.window).maxY - 1, "under the transcript")
        // It recurs: the same line says the new text.
        await rig.transport.append(chatID, [notice("n3", "Reconnecting… 2/5", .warning)])
        await eventually("replaced in place") { (try? self.renderedText(rig, in: rig.layout.frames.first { $0.key.hasPrefix("banner-notice-reconnecting") }?.value ?? .zero).contains("2/5")) == true }
        XCTAssertEqual(rig.layout.frames.keys.filter { $0.hasPrefix("banner-notice-") }.count, 2, "not stacked")
        // ×: closed, until the provider says it again.
        let closeKey = try XCTUnwrap(rig.layout.actions.keys.first { $0.hasPrefix("close-notice-reconnecting") })
        rig.layout.actions[closeKey]?()
        await eventually("closed") { !rig.layout.frames.keys.contains { $0.hasPrefix("banner-notice-reconnecting") } }
        XCTAssertGreaterThanOrEqual(rig.layout.frames["close-notice-this account is close to the weekly usage limit"]?.width ?? 0, 44, "a full target")
        await rig.transport.append(chatID, [notice("n4", "Reconnecting… 3/5", .warning)])
        await eventually("back when said again") { rig.layout.frames.keys.contains { $0.hasPrefix("banner-notice-reconnecting") } }
        // A new turn: last turn's notices go to the history.
        await rig.transport.append(chatID, [.itemCompleted(ChatItem(id: "u2", status: .completed, body: .userMessage("again")))])
        await eventually("history only") { !rig.layout.frames.keys.contains { $0.hasPrefix("banner-notice-") } }
        XCTAssertEqual(ChatNotices.all(conversation.transcript.items).count, 4)
        await finish(rig)
    }

    /// The phone's own messages: one line per source, replaced in place, with ×, gone by themselves when the cause is resolved.
    func testThePhonesMessagesReplaceInPlaceCloseAndGoWhenResolved() async throws {
        let rig = try await makeRig()
        _ = try await openChat(rig)
        let conversation = rig.model.conversation(chatID)
        // An older desktop (no snapshots): the loading note goes when its history is in.
        await eventually("the legacy note goes once the history is loaded") { conversation.feed.loaded && !conversation.legacyLoading && conversation.alerts.text(.desktop) == nil }
        conversation.notice = "Couldn’t send: busy"
        conversation.notice = "Couldn’t send: busy"
        await eventually("one line") { rig.layout.frames["banner-alert-action"] != nil }
        XCTAssertEqual(conversation.alerts.items.count, 1)
        XCTAssertEqual(conversation.alerts.items.first?.repeats, 2, "a recurring error counts up in its one line")
        try XCTUnwrap(rig.layout.actions["close-alert-action"])()
        await eventually("closed with ×") { rig.layout.frames["banner-alert-action"] == nil && conversation.notice == nil }
        // The link: said while down, gone when back, without being closed.
        await rig.model.disconnect()
        await eventually("not connected is said") { rig.layout.frames["banner-status"] != nil }
        await rig.model.connect()
        await eventually("and goes when the link is back") { rig.layout.frames["banner-status"] == nil }
        await finish(rig)
    }

    /// A shell's error is said in the same line, above its input, with ×, in focus mode too.
    func testAShellErrorIsABannerLineAboveTheInputWithClose() async throws {
        let rig = try await makeRig()
        rig.model.error = "Desktop disconnected. Reconnect to refresh output."
        await eventually("the line is there") { rig.layout.frames["banner-shell-error"] != nil }
        rig.model.setFocusMode(true)
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertNotNil(rig.layout.frames["banner-shell-error"], "in focus mode too")
        XCTAssertGreaterThanOrEqual(rig.layout.frames["close-shell-error"]?.width ?? 0, 44)
        try XCTUnwrap(rig.layout.actions["close-shell-error"])()
        await eventually("closed") { rig.model.error == nil && rig.layout.frames["banner-shell-error"] == nil }
        rig.model.setFocusMode(false)
        await finish(rig)
    }

    private func listStartReading(_ scroll: UIScrollView) { scroll.delegate?.scrollViewWillBeginDragging?(scroll) }
    /// Latest floats over the transcript's bottom edge (it has no row of its own, which left a dead band above the composer), and the
    /// transcript reaches down to what is under it.
    private func assertLatestOverTranscriptBottom(_ rig: Rig, scroll: UIScrollView, file: StaticString = #filePath, line: UInt = #line) throws {
        let latest = try XCTUnwrap(rig.layout.frames["latest"], file: file, line: line)
        XCTAssertEqual(rig.layout.visible["latest"], true, file: file, line: line)
        let transcript = scroll.convert(scroll.bounds, to: rig.window)
        XCTAssertGreaterThanOrEqual(latest.minY, transcript.minY, "Latest is over the transcript", file: file, line: line)
        XCTAssertLessThanOrEqual(latest.maxY, transcript.maxY + 0.5, "Latest stays inside the transcript", file: file, line: line)
        XCTAssertLessThanOrEqual(transcript.maxY - latest.maxY, 12, "Latest sits on the transcript's bottom edge", file: file, line: line)
        let field = try XCTUnwrap(composer(rig), file: file, line: line)
        XCTAssertLessThanOrEqual(latest.maxY, field.convert(field.bounds, to: rig.window).minY + 1, file: file, line: line)
    }
    private func compactSnapshot(_ rig: Rig, name: String) throws {
        guard let directory = ProcessInfo.processInfo.environment["RIWORK_COMPACT_CHAT_SNAPSHOTS"] else { return }
        rig.window.layoutIfNeeded()
        let image = UIGraphicsImageRenderer(bounds: rig.window.bounds).image { _ in rig.window.drawHierarchy(in: rig.window.bounds, afterScreenUpdates: true) }
        try FileManager.default.createDirectory(atPath: directory, withIntermediateDirectories: true)
        try XCTUnwrap(image.pngData()).write(to: URL(fileURLWithPath: directory).appendingPathComponent(name + ".png"))
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
    func testStreamingLeavesScrolledUpReaderInPlaceAndJumpSettlesWithLargeText() async throws {
        let rig = try await makeRig(hardwareKeyboard: false, width: 375, height: 812)
        rig.host.traitOverrides.preferredContentSizeCategory = .accessibilityExtraLarge
        let events = (0..<16).map { index in
            ChatEvent.itemCompleted(ChatItem(id: "message-\(index)", status: .completed, body: .agentMessage("Message \(index). A paragraph that wraps across several lines at accessibility text sizes.")))
        }
        await rig.transport.append(chatID, [.info(chat())] + events)
        _ = try await openChat(rig)
        await eventually("all messages loaded") { rig.model.conversation(self.chatID).transcript.items.count == 16 }
        try await Task.sleep(for: .milliseconds(400))
        let scroll = try XCTUnwrap(descendants(UIScrollView.self, in: rig.host.view).filter { $0.contentSize.height > $0.bounds.height + 100 }.max { $0.bounds.height < $1.bounds.height })
        scroll.setContentOffset(CGPoint(x: 0, y: -scroll.adjustedContentInset.top), animated: false)
        try await Task.sleep(for: .milliseconds(300))
        let offset = scroll.contentOffset.y
        await rig.transport.append(chatID, [.itemDelta(itemID: "message-15", delta: .text(" More streamed text arrives below the reader."))])
        await eventually("stream accepted") { rig.model.conversation(self.chatID).transcript.items.last?.body == .agentMessage("Message 15. A paragraph that wraps across several lines at accessibility text sizes. More streamed text arrives below the reader.") }
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(scroll.contentOffset.y, offset, accuracy: 2, "streaming below does not pull a reader to the bottom")
        rig.model.conversation(chatID).jumpToEnd()
        await eventually("jump converges after lazy rows are measured", timeout: 3) {
            self.isAtValidBottom(scroll)
        }
        assertValidBottom(scroll)
        let text = try renderedTranscriptText(rig, scroll: scroll)
        XCTAssertTrue(text.lowercased().contains("reader"), "the streamed tail is actually rendered, not an empty overscrolled viewport: \(text)")
        await finish(rig)
    }

    private func bottomOffset(_ scroll: UIScrollView) -> CGFloat {
        max(-scroll.adjustedContentInset.top, scroll.contentSize.height - scroll.bounds.height + scroll.adjustedContentInset.bottom)
    }
    private func isAtValidBottom(_ scroll: UIScrollView) -> Bool {
        let offset = scroll.contentOffset.y
        return scroll.bounds.height > 0 && offset >= -scroll.adjustedContentInset.top - 2
            && offset <= bottomOffset(scroll) + 2 && abs(bottomOffset(scroll) - offset) <= 2
    }
    private func assertValidBottom(_ scroll: UIScrollView, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertGreaterThan(scroll.bounds.height, 0, file: file, line: line)
        XCTAssertGreaterThanOrEqual(scroll.contentOffset.y, -scroll.adjustedContentInset.top - 2, file: file, line: line)
        XCTAssertLessThanOrEqual(scroll.contentOffset.y, bottomOffset(scroll) + 2, "no overscroll beyond the content end", file: file, line: line)
        XCTAssertEqual(scroll.contentOffset.y, bottomOffset(scroll), accuracy: 2, "absolute bottom gap, including adjusted insets", file: file, line: line)
    }
    private func firstRenderedMessage(_ rig: Rig, scroll: UIScrollView) throws -> String {
        let text = try renderedTranscriptText(rig, scroll: scroll)
        let range = try XCTUnwrap(text.range(of: "Message [0-9]+", options: .regularExpression), "a stable message is actually visible")
        return String(text[range])
    }

    /// OCR the actual viewport pixels, not the model or off-screen accessibility nodes: blank content cannot pass this check.
    private func renderedMessageTop(_ message: String, rig: Rig, scroll: UIScrollView) throws -> CGFloat {
        rig.window.layoutIfNeeded()
        let visible = scroll.convert(scroll.bounds, to: rig.window).intersection(rig.window.bounds)
        let image = UIGraphicsImageRenderer(size: visible.size).image { context in
            context.cgContext.translateBy(x: -visible.minX, y: -visible.minY)
            rig.window.drawHierarchy(in: rig.window.bounds, afterScreenUpdates: true)
        }
        let request = VNRecognizeTextRequest(); request.recognitionLevel = .accurate
        try VNImageRequestHandler(cgImage: try XCTUnwrap(image.cgImage), options: [:]).perform([request])
        let observation = request.results?.first { $0.topCandidates(1).first?.string.contains(message) == true }
        if observation == nil { print("LATEST_FIRST_OCR_MISSING message=\(message) viewport=\(visible) offset=\(scroll.contentOffset) size=\(scroll.contentSize) text=\(request.results?.compactMap { $0.topCandidates(1).first?.string } ?? [])") }
        let match = try XCTUnwrap(observation, "message \(message) is actually visible")
        return (1 - match.boundingBox.maxY) * visible.height
    }

    private func renderedTranscriptText(_ rig: Rig, scroll: UIScrollView) throws -> String {
        rig.window.layoutIfNeeded()
        let visible = scroll.convert(scroll.bounds, to: rig.window).intersection(rig.window.bounds)
        XCTAssertFalse(visible.isEmpty, "transcript has a visible viewport")
        return try renderedText(rig, in: visible)
    }
    private func renderedText(_ rig: Rig, in rect: CGRect) throws -> String {
        rig.window.layoutIfNeeded()
        let visible = rect.intersection(rig.window.bounds)
        guard !visible.isEmpty else { return "" }
        let image = UIGraphicsImageRenderer(size: visible.size).image { context in
            context.cgContext.translateBy(x: -visible.minX, y: -visible.minY)
            rig.window.drawHierarchy(in: rig.window.bounds, afterScreenUpdates: true)
        }
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.recognitionLanguages = ["en-US"]
        request.usesLanguageCorrection = false
        try VNImageRequestHandler(cgImage: XCTUnwrap(image.cgImage)).perform([request])
        return (request.results ?? []).compactMap { $0.topCandidates(1).first?.string }.joined(separator: " ")
    }
    func testApprovalArrivalKeepsLoadedChatTailVisible() async throws {
        try await checkApprovalResize(jumpFirst: false)
    }
    func testJumpImmediatelyFollowedByApprovalResizeKeepsTailVisible() async throws {
        try await checkApprovalResize(jumpFirst: true)
    }
    private func checkApprovalResize(jumpFirst: Bool, look: Look = .terminal, mic: Bool = false) async throws {
        let rig = try await makeRig(hardwareKeyboard: false, width: 375, height: 812, look: look, mic: mic)
        let messages = (0..<12).map { index in
            ChatEvent.itemCompleted(ChatItem(id: "reply-\(index)", status: .completed, body: .agentMessage(index == 11 ? "Latest reply remains visible." : "Reply \(index). This conversation has enough history to scroll.")))
        }
        await rig.transport.append(chatID, [.info(chat())] + messages)
        _ = try await openChat(rig)
        await eventually("fresh chat loaded") { rig.model.conversation(self.chatID).transcript.items.count == 12 }
        try await Task.sleep(for: .milliseconds(400))
        let scroll = try XCTUnwrap(transcriptScroll(rig))
        await eventually("fresh loaded chat at valid bottom") { self.isAtValidBottom(scroll) }
        if jumpFirst {
            scroll.setContentOffset(CGPoint(x: 0, y: -scroll.adjustedContentInset.top), animated: false)
            try await Task.sleep(for: .milliseconds(300))
            rig.model.conversation(chatID).jumpToEnd()
        }
        await rig.transport.append(chatID, [.approvalRequested(ChatApproval(requestID: "resize", kind: .command, title: "swift test --package-path ios", detail: "Run focused tests.", choices: [.accept, .acceptForSession, .decline, .cancel])), .state(.waiting)])
        rig.model.conversation(chatID).draft = "Keep my draft\nand check the chat tail."
        await eventually("approval arrives") { !rig.model.conversation(self.chatID).openApprovals.isEmpty }
        try await Task.sleep(for: .milliseconds(500))
        await eventually("approval and composer resize leave a valid bottom") { self.isAtValidBottom(scroll) }
        assertValidBottom(scroll)
        let text = try renderedTranscriptText(rig, scroll: scroll).lowercased()
        XCTAssertTrue(text.contains("latest reply"), "rendered reply tail must remain visible: \(text)")
        XCTAssertTrue(text.contains("waiting for you"), "rendered waiting status must remain visible: \(text)")
        try reviewSnapshot(rig, name: jumpFirst ? "phone-jump-approval-resize" : "phone-fresh-approval-resize")
        await finish(rig)
    }

    func testCombinedNativeControlsAndApprovalResizeKeepLoadedTailVisible() async throws {
        for look in [Look.nativeLight, .nativeDark] {
            try await checkApprovalResize(jumpFirst: true, look: look, mic: true)
        }
    }

    func testComposerTracksDynamicTypeWithoutLosingDraftOrKeyboardCommands() async throws {
        let rig = try await makeRig(hardwareKeyboard: false, width: 375, height: 812)
        let field = try await openChat(rig)
        rig.model.conversation(chatID).draft = "A draft that must survive text scaling."
        try await Task.sleep(for: .milliseconds(100))
        let initialFont = try XCTUnwrap(field.font).pointSize
        rig.host.traitOverrides.preferredContentSizeCategory = .accessibilityExtraLarge
        await eventually("composer font follows accessibility text size") { (field.font?.pointSize ?? 0) > initialFont }
        XCTAssertEqual(field.text, "A draft that must survive text scaling.")
        XCTAssertEqual(field.keyCommands?.count, 2, "Return routing is unchanged by layout")
        let frame = field.convert(field.bounds, to: rig.window)
        XCTAssertGreaterThanOrEqual(frame.minX, 0)
        XCTAssertLessThanOrEqual(frame.maxX, rig.window.bounds.width)
        XCTAssertLessThanOrEqual(frame.maxY, rig.window.bounds.height)
        await finish(rig)
    }

    /// Reproducible design review using the real screen and scripted transport, never production fake messages.
    func testChatLayoutReviewCaptures() async throws {
        let tablet = UIDevice.current.userInterfaceIdiom == .pad
        let screenSize = try XCTUnwrap((UIApplication.shared.connectedScenes.first as? UIWindowScene)?.screen.bounds.size)
        let width: CGFloat = tablet ? screenSize.width : 375
        let height: CGFloat = tablet ? screenSize.height : 812
        for (name, appearance, category) in [("light", UIUserInterfaceStyle.light, UIContentSizeCategory.large),
                                             ("dark", .dark, .large), ("large-text", .light, .accessibilityExtraLarge)] {
            let rig = try await makeRig(hardwareKeyboard: false, width: width, height: height)
            rig.window.overrideUserInterfaceStyle = appearance
            rig.host.traitOverrides.preferredContentSizeCategory = category
            let conversation = rig.model.conversation(chatID)
            let events: [ChatEvent] = [
                .info(chat()), .models(ChatBundledModels.models(for: .claude)),
                .itemCompleted(ChatItem(id: "user", status: .completed, body: .userMessage("Can you make this screen easier to read on my phone and iPad?"))),
                .itemCompleted(ChatItem(id: "assistant", status: .completed, body: .agentMessage("""
                ## A calmer conversation

                The messages now have room to breathe. **Your prompts** and the assistant’s response should be easy to tell apart, while the model stays within reach.

                - Keep the reading column comfortable on iPad.
                - Preserve drafts, keyboard shortcuts and approvals.

                ```swift
                let title = "A long line stays inside the code surface, with horizontal scrolling when needed"
                print(title)
                ```
                """))),
                .itemCompleted(ChatItem(id: "tool", status: .completed, body: .toolCall(server: "Files", tool: "read", input: .object(["path": .string("ios/RiWorkRemote/ChatView.swift")]), output: "Read 328 lines.\nThe toolbar, transcript and composer share the same theme.")))
            ]
            await rig.transport.append(chatID, events)
            let field = try await openChat(rig)
            await eventually("fixture messages") { conversation.transcript.items.count == 3 }
            conversation.expanded.insert("tool")
            try await Task.sleep(for: .milliseconds(600))
            try reviewSnapshot(rig, name: "\(tablet ? "tablet" : "phone")-\(name)-messages")
            let transcriptScroll = descendants(UIScrollView.self, in: rig.host.view)
                // The whole conversation can fit on iPad before an approval reduces its viewport.
                .filter { $0.bounds.height > 100 }
                .max { $0.bounds.height < $1.bounds.height }
            let scrolledToStart = transcriptScroll.map { $0.contentSize.height > $0.bounds.height + 20 } ?? false
            if scrolledToStart {
                transcriptScroll?.setContentOffset(CGPoint(x: 0, y: -(transcriptScroll?.adjustedContentInset.top ?? 0)), animated: false)
                try await Task.sleep(for: .milliseconds(250))
            }
            try reviewSnapshot(rig, name: "\(tablet ? "tablet" : "phone")-\(name)-messages-start")
            if scrolledToStart { conversation.jumpToEnd() }
            await rig.transport.append(chatID, [.approvalRequested(ChatApproval(requestID: "review", kind: .command, title: "swift test --package-path ios", detail: "Run the focused chat tests.", choices: [.accept, .acceptForSession, .decline, .cancel])), .state(.waiting)])
            await eventually("approval shown") { !conversation.openApprovals.isEmpty }
            conversation.draft = "Please keep the current model\nand run the focused checks."
            try await Task.sleep(for: .milliseconds(400))
            let scroll = try XCTUnwrap(transcriptScroll)
            await eventually("review approval settles at valid bottom") { self.isAtValidBottom(scroll) }
            assertValidBottom(scroll)
            var rendered = ""
            await eventually("review viewport paints its reply tail") {
                rendered = (try? self.renderedTranscriptText(rig, scroll: scroll).lowercased()) ?? ""
                return rendered.contains("waiting for you") && (category != .large || rendered.contains("read 328 lines"))
            }
            XCTAssertTrue(rendered.contains("waiting for you"), "review viewport renders its tail: \(rendered)")
            if category == .large {
                XCTAssertTrue(rendered.contains("read 328 lines"), "expanded tool tail remains visible: \(rendered)")
            }
            try reviewSnapshot(rig, name: "\(tablet ? "tablet" : "phone")-\(name)-approval")
            if name == "dark" {
                _ = field.becomeFirstResponder()
                try await Task.sleep(for: .milliseconds(700))
                // SDK capture includes the real software keyboard, which lives in its own UIKit window.
                let image = UIGraphicsImageRenderer(bounds: rig.window.screen.bounds).image { _ in
                    for window in (rig.window.windowScene?.windows ?? []) where !window.isHidden {
                        window.drawHierarchy(in: window.frame, afterScreenUpdates: true)
                    }
                }
                try reviewImage(image, name: "\(tablet ? "tablet" : "phone")-dark-keyboard")
                // Allows SDK simctl capture of the keyboard's separate system window during a design review.
                try await Task.sleep(for: .seconds(5))
            }
            await finish(rig)
        }
    }
    private func reviewSnapshot(_ rig: Rig, name: String) throws {
        rig.window.layoutIfNeeded()
        let image = UIGraphicsImageRenderer(bounds: rig.window.bounds).image { _ in rig.window.drawHierarchy(in: rig.window.bounds, afterScreenUpdates: true) }
        try reviewImage(image, name: name)
    }
    private func reviewImage(_ image: UIImage, name: String) throws {
        let attachment = XCTAttachment(image: image); attachment.name = name; attachment.lifetime = .keepAlways; add(attachment)
        guard let path = ProcessInfo.processInfo.environment["RIWORK_CHAT_SNAPSHOTS"] else { return }
        let directory = URL(fileURLWithPath: path)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try XCTUnwrap(image.pngData()).write(to: directory.appendingPathComponent(name + ".png"))
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
        guard let frame = rig.layout.frames["transcript"] else { return nil }
        // Match the actual transcript viewport; compact tabs and nested horizontal output also use UIScrollView.
        return descendants(UIScrollView.self, in: rig.host.view).first { scroll in
            guard !(scroll is UITextView), scroll.window != nil else { return false }
            let actual = scroll.convert(scroll.bounds, to: rig.window)
            return abs(actual.minY - frame.minY) < 1 && abs(actual.width - frame.width) < 1 && abs(actual.height - frame.height) < 1
        }
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
            XCTAssertNil(rig.layout.frames["mic"], "\(look): no mic")
            XCTAssertNotNil(rig.layout.frames["send"], "\(look): Send, greyed, holds the field's action slot")
            try snapshot(rig, name: named("chat-composer-mic-off", look))
            await setMic(rig, true, look: look, updated: 1_790_000_100)
            // With nothing typed the mic takes Send's slot: the field keeps its width.
            await eventually("\(look): the mic is there") { rig.layout.frames["mic"] != nil && rig.layout.frames["send"] == nil }
            XCTAssertEqual(try XCTUnwrap(width(of: ChatComposerTextView.self, in: rig)), off, accuracy: 1, "\(look): in the same slot")
            XCTAssertGreaterThanOrEqual(rig.layout.frames["mic"]?.width ?? 0, 44, "\(look): a full target")
            rig.model.conversation(chatID).draft = "typed"
            await eventually("\(look): typing brings Send back in its place") { rig.layout.frames["mic"] == nil && rig.layout.frames["send"] != nil }
            rig.model.conversation(chatID).draft = ""
            try await Task.sleep(for: .milliseconds(300))
            try snapshot(rig, name: named("chat-composer-mic-on", look))
            await setMic(rig, false, look: look, updated: 1_790_000_200)
            await eventually("\(look): and gone again") { rig.layout.frames["mic"] == nil && rig.layout.frames["send"] != nil }
            XCTAssertEqual(try XCTUnwrap(width(of: ChatComposerTextView.self, in: rig)), off, accuracy: 1)
            await finish(rig)
        }
    }
    /// The composer's one action slot: what it holds in every state, Interrupt always reachable while the agent works.
    func testTheComposerActionSlot() {
        func slot(typed: Bool, busy: Bool, mic: Bool, dictating: Bool = false) -> [String] {
            let actions = ComposerActions(typed: typed, busy: busy, mic: mic, dictating: dictating)
            return [actions.stop ? "stop" : nil, actions.mic ? "mic" : nil, actions.send ? "send" : nil].compactMap { $0 }
        }
        XCTAssertEqual(slot(typed: false, busy: false, mic: true), ["mic"])
        XCTAssertEqual(slot(typed: false, busy: false, mic: false), ["send"], "greyed, holding the slot")
        XCTAssertEqual(slot(typed: true, busy: false, mic: true), ["send"])
        XCTAssertEqual(slot(typed: false, busy: true, mic: true), ["stop"])
        XCTAssertEqual(slot(typed: true, busy: true, mic: true), ["stop", "send"], "Interrupt stays beside Send")
        XCTAssertEqual(slot(typed: true, busy: false, mic: true, dictating: true), ["mic"], "the mic stays while it listens, whatever is heard")
        XCTAssertEqual(slot(typed: false, busy: true, mic: true, dictating: true), ["stop", "mic"])
        XCTAssertEqual(slot(typed: false, busy: false, mic: false, dictating: true), ["mic"], "a dictation the setting has not ended yet keeps its button")
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

/// Exercises the production alignment helper at its real yield, without copying view methods.
@MainActor final class HistoryAnchorAlignmentTests: XCTestCase {
    func testCompletedGestureDuringYieldCannotAlignCapturedRow() async {
        let viewport = ChatHistoryViewport()
        viewport.anchor = ("reader", -18); viewport.pageInstalled = true
        var preserving = true, userDriven = false
        var calls: [String] = []
        let aligned = await viewport.alignInstalledAnchor(while: { preserving && !userDriven }, yield: {
            let input = Task { @MainActor in
                userDriven = true; preserving = false
                viewport.anchor = nil; viewport.pageInstalled = false
                userDriven = false // the gesture has completed before alignment resumes
            }
            await Task.yield()
            await input.value
        }, scroll: { calls.append($0) })
        XCTAssertFalse(userDriven); XCTAssertFalse(aligned); XCTAssertTrue(calls.isEmpty)
    }

    func testJumpDuringYieldRemainsTheFinalAlignment() async {
        let viewport = ChatHistoryViewport()
        viewport.anchor = ("reader", -18); viewport.pageInstalled = true
        var preserving = true
        var calls: [String] = []
        let aligned = await viewport.alignInstalledAnchor(while: { preserving }, yield: {
            let input = Task { @MainActor in
                preserving = false; viewport.anchor = nil; viewport.pageInstalled = false
                calls.append("latest")
            }
            await Task.yield()
            await input.value
        }, scroll: { calls.append($0) })
        XCTAssertFalse(aligned); XCTAssertEqual(calls, ["latest"])
    }

    func testIdenticalRecaptureInvalidatesLifetimeAndCurrentCaptureCanAlign() async {
        let viewport = ChatHistoryViewport()
        viewport.anchor = ("reader", -18); viewport.pageInstalled = true
        var calls: [String] = []
        let stale = await viewport.alignInstalledAnchor(while: { true }, yield: {
            let input = Task { @MainActor in viewport.anchor = ("reader", -18) }
            await Task.yield()
            await input.value
        }, scroll: { calls.append($0) })
        XCTAssertFalse(stale); XCTAssertTrue(calls.isEmpty)
        let current = await viewport.alignInstalledAnchor(while: { true }, scroll: { calls.append($0) })
        XCTAssertTrue(current); XCTAssertEqual(calls, ["reader"])
    }
}
