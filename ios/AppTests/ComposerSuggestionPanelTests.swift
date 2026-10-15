import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// The composer's `/`, `@` and `&` list in the real chat screen, with a scripted desktop behind it: the catalog is read once when a list
/// opens, the list shows above the field, the keys of a hardware keyboard drive it (delivered as the `UIKeyCommand`s the composer
/// declares, as `ChatViewTests` does), accepting puts text in and never sends nor changes the chat, and an older desktop shows nothing.
@MainActor final class ComposerSuggestionPanelTests: XCTestCase {
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

    private func chat() -> ChatInfo {
        ChatInfo(id: chatID, provider: .claude, projectID: project, cwd: "/fixture", title: "Fix the build", createdAtUnix: 10, state: .idle)
    }
    /// The desktop's answer for the chat: Claude's `/compact`, a small folder and Claude's models (opus the most used).
    private func reply(provider: String = "claude") throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: Data("""
        {"chat_id":"\(chatID)","provider":"\(provider)","root":"/fixture",
         "commands":[{"name":"compact","summary":"Summarize the conversation so far to free up context","argument":null,
                      "sends":"Goes to Claude as typed. Claude compacts the chat's context.","handler":"provider"}],
         "files":{"entries":[{"path":"README.md","dir":false,"bytes":2048},{"path":"src","dir":true,"bytes":null},
                             {"path":"src/main.rs","dir":false,"bytes":900},{"path":"src/chat","dir":true,"bytes":null}],"truncated":false},
         "file_bound":5000,
         "models":[{"id":"default","name":"Default (recommended)","description":"","is_default":true,"chats":2},
                   {"id":"opus","name":"Opus","description":"For complex tasks","is_default":false,"chats":9},
                   {"id":"haiku","name":"Haiku","description":"","is_default":false,"chats":0}]}
        """.utf8))
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

    private struct ThemedTabs: View {
        let model: RemoteModel
        let project: RemoteProject
        var body: some View { TerminalTabsView(model: model, project: project, onBack: {}).desktopThemed(model.theme.style) }
    }
    private struct Rig {
        let model: RemoteModel, transport: ChatTransport, window: UIWindow, host: UIHostingController<AnyView>, keychain: KeychainStore
        let layout: ChatLayoutInspection
    }
    private func makeRig(feature: Bool = true, refused: Bool = false) async throws -> Rig {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene to show a chat in") }
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = ChatTransport.shell
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        let suite = "com.riwork.tests.suggestions.\(UUID().uuidString)"
        defaultsNames.append(suite)
        let transport = ChatTransport(chats: [chat()])
        await transport.setSuggestions(feature: feature, replies: [chatID: try reply()], refused: refused)
        let model = RemoteModel(client: transport, keychain: keychain, defaults: UserDefaults(suiteName: suite)!, chatWaitMilliseconds: 300, chatIdleInterval: .milliseconds(20),
                                hardwareKeyboard: HardwareKeyboardMonitor(probe: { true }))
        await model.connect()
        let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))
        let layout = ChatLayoutInspection()
        let host = UIHostingController(rootView: AnyView(ThemedTabs(model: model, project: projectValue).environment(\.chatLayoutInspection, layout)))
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: 402, height: 874)
        window.rootViewController = host
        window.makeKeyAndVisible()
        windows.append(window)
        await eventually("the terminal is on screen") { !self.descendants(KeyCaptureView.self, in: host.view).isEmpty && model.terminalArea != nil }
        return Rig(model: model, transport: transport, window: window, host: host, keychain: keychain, layout: layout)
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
    private func listShown(_ rig: Rig) -> Bool { rig.layout.frames["suggestions"] != nil }
    private func sent(_ rig: Rig) async -> [JSONValue] { await rig.transport.commands() }

    // MARK: Commands

    func testASlashOpensTheListAboveTheFieldAndReturnPutsTheCommandInWithoutSending() async throws {
        let rig = try await makeRig()
        let view = try await openChat(rig)
        XCTAssertNil(command(view, UIKeyCommand.inputUpArrow), "the arrows are the text view's while no list is open")
        view.insertText("/co")
        await eventually("the list is up") { self.listShown(rig) && rig.layout.frames["suggestion-preview"] != nil }
        let list = try XCTUnwrap(rig.layout.frames["suggestions"]), field = try XCTUnwrap(rig.layout.frames["composer"])
        XCTAssertLessThanOrEqual(list.maxY, field.minY + 1, "the list stands above the field")
        XCTAssertNotNil(rig.layout.frames["suggestion-row-0"])
        await eventually("the list's keys are claimed") { self.command(view, UIKeyCommand.inputUpArrow) != nil && self.command(view, "\t") != nil }
        XCTAssertNotNil(command(view, UIKeyCommand.inputEscape))
        let reads = await rig.transport.count("chat.suggestions")
        XCTAssertEqual(reads, 1, "the catalog is read once, however many keys are typed")
        press(view, "\r")
        XCTAssertEqual(view.text, "/compact ", "⏎ puts the command in, with a space after it")
        XCTAssertEqual(view.selectedRange, NSRange(location: 9, length: 0))
        await eventually("the draft follows and the list goes") { rig.model.conversation(self.chatID).draft == "/compact " && !self.listShown(rig) }
        try await Task.sleep(for: .milliseconds(150))
        let none = await sent(rig)
        XCTAssertTrue(none.isEmpty, "accepting never sends")
        await eventually("the list's keys are given back") { self.command(view, UIKeyCommand.inputUpArrow) == nil }
        // The key comes up (a held one's repeats send nothing: `HeldReturn`), and is pressed again.
        view.returnReleased()
        press(view, "\r")
        await eventually("now ⏎ sends, as ever") { await !self.sent(rig).isEmpty }
        let after = await sent(rig)
        XCTAssertEqual(after.count, 1)
        XCTAssertEqual(after.first?["command"], .string("send"))
        XCTAssertEqual(after.first?["text"].string?.trimmingCharacters(in: .whitespaces), "/compact", "the person's own ⏎ sent what the list put in")
        await finish(rig)
    }
    func testEscapeClosesTheListAndTheTokenKeepsItClosed() async throws {
        let rig = try await makeRig()
        let view = try await openChat(rig)
        view.insertText("/c")
        await eventually("the list is up") { self.listShown(rig) }
        await eventually("Escape is the list's") { self.command(view, UIKeyCommand.inputEscape) != nil }
        press(view, UIKeyCommand.inputEscape)
        await eventually("closed") { !self.listShown(rig) }
        view.insertText("o")
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertFalse(listShown(rig), "typing on in the same token keeps it closed")
        XCTAssertNil(command(view, UIKeyCommand.inputEscape))
        await finish(rig)
    }

    // MARK: Files

    func testAnAtListMovesWithTheArrowsAndTabPutsTheFileIn() async throws {
        let rig = try await makeRig()
        let view = try await openChat(rig)
        view.insertText("see @src")
        await eventually("the files are up") { self.listShown(rig) && rig.layout.frames["suggestion-row-2"] != nil }
        await eventually("the list's keys are claimed") { self.command(view, UIKeyCommand.inputDownArrow) != nil }
        press(view, UIKeyCommand.inputDownArrow)
        press(view, "\t")
        XCTAssertEqual(view.text, "see src/chat/ ", "↓ moved from src/ to src/chat/, and Tab put it in")
        await eventually("the draft follows") { rig.model.conversation(self.chatID).draft == "see src/chat/ " }
        let none = await sent(rig)
        XCTAssertTrue(none.isEmpty)
        await finish(rig)
    }
    func testATapHighlightsARowAndASecondTapPutsItIn() async throws {
        let rig = try await makeRig()
        let view = try await openChat(rig)
        view.insertText("@")
        // Shallowest first: src/, README.md, src/chat/, src/main.rs.
        await eventually("the files are up") { rig.layout.actions["suggestion-row-1"] != nil && rig.layout.frames["suggestion-preview"] != nil }
        rig.layout.actions["suggestion-row-1"]?()
        XCTAssertEqual(view.text, "@", "the first tap only highlights")
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertNotNil(rig.layout.frames["suggestion-preview"], "the card stays, for the tapped row")
        rig.layout.actions["suggestion-row-1"]?()
        XCTAssertEqual(view.text, "README.md ", "a tap on the highlighted row puts it in")
        await eventually("the draft follows and the list goes") { rig.model.conversation(self.chatID).draft == "README.md " && !self.listShown(rig) }
        let none = await sent(rig)
        XCTAssertTrue(none.isEmpty)
        await finish(rig)
    }

    // MARK: Models

    func testAnAmpersandPutsAModelsMentionInAndAsksTheChatForNothing() async throws {
        let rig = try await makeRig()
        // The chat's own model list has opus and haiku, so the model picker could switch to either: `&` must not.
        let option = { (id: String, isDefault: Bool) in
            ChatModelOption(id: id, name: id.capitalized, description: "", efforts: ["low", "high"], defaultEffort: "low", supportsFast: false, isDefault: isDefault)
        }
        await rig.transport.append(chatID, [.info(chat()), .models([option("default", true), option("opus", false), option("haiku", false)])])
        let view = try await openChat(rig)
        await eventually("the chat's models are in") { rig.model.conversation(self.chatID).transcript.models.count == 3 }
        XCTAssertNotNil(rig.model.conversation(chatID).modelChoices(fallback: chat()).configuration(for: .model("opus")),
                        "the picker would switch to opus: what follows tells a mention from a switch")
        view.insertText("&op")
        await eventually("the models are up") { self.listShown(rig) && rig.layout.frames["suggestion-preview"] != nil }
        await eventually("the list's keys are claimed") { self.command(view, "\t") != nil }
        press(view, "\r")
        XCTAssertEqual(view.text, "&opus ", "⏎ puts the model's mention in, with a space after it")
        // A second mention later in the same message.
        view.returnReleased()
        view.insertText("designs it, then &ha")
        await eventually("a list again, for the second mention") { self.listShown(rig) && self.command(view, "\t") != nil }
        press(view, "\t")
        XCTAssertEqual(view.text, "&opus designs it, then &haiku ")
        await eventually("the draft follows and the list goes") {
            rig.model.conversation(self.chatID).draft == "&opus designs it, then &haiku " && !self.listShown(rig)
        }
        try await Task.sleep(for: .milliseconds(150))
        let commands = await rig.transport.count("chat.command")
        XCTAssertEqual(commands, 0, "no configure, no send: the chat is asked for nothing")
        XCTAssertNil(rig.model.chats.first { $0.id == self.chatID }?.model, "the chat's model is as it was")
        XCTAssertNil(rig.model.conversation(chatID).notice, "nothing to say about a model change")
        await finish(rig)
    }

    // MARK: The catalog

    func testTheCatalogIsReadOncePerChatAndKept() async throws {
        let rig = try await makeRig()
        XCTAssertEqual(rig.model.composerCatalog(for: chatID), .loading)
        rig.model.wantComposerSuggestions(chatID)
        rig.model.wantComposerSuggestions(chatID)
        await eventually("read") { if case .ready = rig.model.composerCatalog(for: self.chatID) { true } else { false } }
        rig.model.wantComposerSuggestions(chatID)
        let reads = await rig.transport.count("chat.suggestions")
        XCTAssertEqual(reads, 1, "one read on the wire, then fresh for 30 s")
        guard case .ready(let catalog) = rig.model.composerCatalog(for: chatID) else { return XCTFail("no catalog") }
        XCTAssertEqual(catalog.commands.map(\.name), ["compact"])
        XCTAssertEqual(catalog.rankFiles("main").map(\.path), ["src/main.rs"])
        await finish(rig)
    }
    func testACatalogOfTheProviderTheChatLeftIsNotOfferedAndIsReadAgain() async throws {
        let rig = try await makeRig()
        rig.model.wantComposerSuggestions(chatID)
        await eventually("read") { if case .ready = rig.model.composerCatalog(for: self.chatID) { true } else { false } }
        // The chat goes on with Codex: Claude's /compact would reach Codex as words.
        var codex = chat()
        codex.provider = .codex
        rig.model.chats = [codex]
        XCTAssertEqual(rig.model.composerCatalog(for: chatID), .loading, "Claude's commands are not offered")
        await rig.transport.setSuggestions(feature: true, replies: [chatID: try reply(provider: "codex")])
        rig.model.wantComposerSuggestions(chatID)
        await eventually("read again at once") {
            if case .ready(let catalog) = rig.model.composerCatalog(for: self.chatID) { catalog.provider == .codex } else { false }
        }
        let reads = await rig.transport.count("chat.suggestions")
        XCTAssertEqual(reads, 2)
        await finish(rig)
    }
    func testWithoutTheFeatureNothingIsAskedAndNoListOpens() async throws {
        let rig = try await makeRig(feature: false)
        let view = try await openChat(rig)
        XCTAssertEqual(rig.model.composerCatalog(for: chatID), .unsupported)
        view.insertText("/co")
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertFalse(listShown(rig))
        XCTAssertNil(command(view, UIKeyCommand.inputUpArrow))
        let reads = await rig.transport.count("chat.suggestions")
        XCTAssertEqual(reads, 0)
        await finish(rig)
    }
    func testADesktopThatAnnouncesTheFeatureButRefusesTheMethodIsNotAskedAgain() async throws {
        let rig = try await makeRig(refused: true)
        rig.model.wantComposerSuggestions(chatID)
        await eventually("refused") { rig.model.composerCatalog(for: self.chatID) == .unsupported }
        rig.model.wantComposerSuggestions(chatID)
        try await Task.sleep(for: .milliseconds(100))
        let reads = await rig.transport.count("chat.suggestions")
        XCTAssertEqual(reads, 1)
        await finish(rig)
    }
}
