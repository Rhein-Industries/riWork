import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// Pictures of the top of the tab screen with a shell tab and a chat tab, and of switching between them, in the terminal look and in
/// Native (light and dark), in whatever orientation the simulator is in. Not a test of anything: `scripts/tab-chrome-screenshots.sh
/// <directory> <udid>` runs it and takes the pictures with the simulator itself (so the status bar and Liquid Glass are drawn); skipped
/// otherwise.
@MainActor final class TabChromeScreenshots: XCTestCase {
    private let project = ChatTransport.project
    private let chatID = "cccccccc-1111-4111-8111-111111111111"
    private var directory: URL!
    private var windows: [UIWindow] = []
    private var defaultsNames: [String] = []

    enum Look: String, CaseIterable { case terminal, nativeLight = "native-light", nativeDark = "native-dark" }

    override func setUp() async throws {
        guard let path = ProcessInfo.processInfo.environment["RIWORK_TAB_SCREENSHOTS"] else { throw XCTSkip("Set RIWORK_TAB_SCREENSHOTS") }
        directory = URL(fileURLWithPath: path)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        executionTimeAllowance = 600
    }
    override func tearDown() async throws {
        windows.forEach { $0.isHidden = true }
        windows = []
        for name in defaultsNames { UserDefaults().removePersistentDomain(forName: name) }
        defaultsNames = []
    }

    private func appearance(_ look: Look, mic: Bool = false) -> JSONValue? {
        guard look != .terminal else { return nil }
        let dark = look == .nativeDark
        let colors = dark
            ? ["bg": "#000000", "panel": "#1c1c1e", "panel_active": "#2c2c2e", "divider": "#3a3a3c", "cyan": "#ffffff", "magenta": "#c7c7cc", "gold": "#ff9f0a", "text": "#f5f5f7", "muted": "#98989d"]
            : ["bg": "#ffffff", "panel": "#f5f5f7", "panel_active": "#e8e8ed", "divider": "#d2d2d7", "cyan": "#000000", "magenta": "#3a3a3c", "gold": "#b34000", "text": "#1d1d1f", "muted": "#636366"]
        return .object(["v": .number(1), "updated_at": .number(1_790_000_000), "dark": .bool(dark), "native": .bool(true), "mic": .bool(mic),
                        "palette": .object(colors.mapValues { .string($0) })])
    }
    private struct ThemedTabs: View {
        let model: RemoteModel
        let project: RemoteProject
        var body: some View { TerminalTabsView(model: model, project: project, onBack: {}).desktopThemed(model.theme.style) }
    }
    private func eventually(_ what: String, timeout: Double = 8, _ condition: () async -> Bool) async {
        let end = Date().addingTimeInterval(timeout)
        while await !condition(), Date() < end { try? await Task.sleep(for: .milliseconds(20)) }
        let met = await condition()
        XCTAssertTrue(met, what)
    }
    private var orientation: String {
        let bounds = (UIApplication.shared.connectedScenes.first as? UIWindowScene)?.coordinateSpace.bounds ?? UIScreen.main.bounds
        return bounds.width > bounds.height ? "landscape" : "portrait"
    }

    private func keyBar() -> KeyBarView? {
        func find(_ view: UIView) -> KeyBarView? { (view as? KeyBarView) ?? view.subviews.lazy.compactMap(find).first }
        return UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.flatMap(\.windows).lazy.compactMap { find($0) }.first
    }
    /// Leaves `<name>.ready` and waits for the script to take `<name>.png`.
    private func shot(_ window: UIWindow, _ name: String) async throws {
        window.layoutIfNeeded()
        try? await Task.sleep(for: .milliseconds(600))
        let ready = directory.appendingPathComponent(name + ".ready"), png = directory.appendingPathComponent(name + ".png")
        try? FileManager.default.removeItem(at: png)
        try Data().write(to: ready)
        let deadline = Date().addingTimeInterval(20)
        while !FileManager.default.fileExists(atPath: png.path), Date() < deadline { try await Task.sleep(for: .milliseconds(100)) }
        try? FileManager.default.removeItem(at: ready)
    }
    /// Leaves `<name>.rec` while `body` runs; the script records the screen meanwhile.
    private func record(_ name: String, _ body: () async throws -> Void) async throws {
        let rec = directory.appendingPathComponent(name + ".rec"), started = directory.appendingPathComponent(name + ".recording")
        try Data().write(to: rec)
        defer {
            try? FileManager.default.removeItem(at: rec)
            try? FileManager.default.removeItem(at: started)
        }
        let deadline = Date().addingTimeInterval(10)
        while !FileManager.default.fileExists(atPath: started.path), Date() < deadline { try await Task.sleep(for: .milliseconds(100)) }
        try await Task.sleep(for: .milliseconds(1200))
        try await body()
        try await Task.sleep(for: .milliseconds(800))
        try? FileManager.default.removeItem(at: rec)
        try? FileManager.default.removeItem(at: started)
        try await Task.sleep(for: .milliseconds(1500))
    }

    func testShellAndChatTops() async throws {
        // The iPhone app is portrait only (Info.plist); landscape is asked for on an iPad with RIWORK_TAB_SCREENSHOTS_LANDSCAPE=1.
        if ProcessInfo.processInfo.environment["RIWORK_TAB_SCREENSHOTS_LANDSCAPE"] == "1", let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene {
            scene.requestGeometryUpdate(.iOS(interfaceOrientations: .landscapeRight))
            await eventually("landscape") { self.orientation == "landscape" }
        }
        let rotated = orientation
        for look in Look.allCases {
            guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene") }
            let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
            let pairing = try Pairing.parse("""
            {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
            """)
            var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
            desktop.selectedProjectID = project; desktop.selectedSessionID = ChatTransport.shell
            try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
            let suite = "com.riwork.tests.tabchrome.\(UUID().uuidString)"
            defaultsNames.append(suite)
            let info = ChatInfo(id: chatID, provider: .claude, projectID: project, cwd: "/fixture", title: "Fix the build", createdAtUnix: 10, approvalMode: .supervised, state: .idle)
            let transport = ChatTransport(chats: [info], appearance: appearance(look))
            await transport.setShellOutput((0..<40).map { "\u{1b}[32m~/fixture\u{1b}[0m $ make test  # line \($0)" }.joined(separator: "\r\n"))
            await transport.append(chatID, [.info(info)] + (0..<8).map { index in
                .itemCompleted(ChatItem(id: "m\(index)", status: .completed, body: .agentMessage("Message \(index). A readable paragraph in this fixture conversation, long enough to wrap.")))
            })
            let model = RemoteModel(client: transport, keychain: keychain, defaults: UserDefaults(suiteName: suite)!, chatWaitMilliseconds: 300,
                                    chatIdleInterval: .milliseconds(20), hardwareKeyboard: HardwareKeyboardMonitor(probe: { true }))
            await model.connect()
            if look != .terminal { await eventually("Native look") { model.theme.style.native } }
            let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))
            let host = UIHostingController(rootView: AnyView(ThemedTabs(model: model, project: projectValue)))
            let window = UIWindow(windowScene: scene)
            window.frame = scene.coordinateSpace.bounds
            window.windowLevel = .alert + 1
            window.rootViewController = host
            if look != .terminal { window.overrideUserInterfaceStyle = look == .nativeDark ? .dark : .light } else { window.overrideUserInterfaceStyle = .light }
            window.makeKeyAndVisible()
            windows.forEach { $0.isHidden = true }
            windows.append(window)
            await eventually("terminal up") { model.terminalArea != nil && model.hasOutput }
            let prefix = "\(look.rawValue)-\(rotated)"
            try await shot(window, prefix + "-1-shell")
            // The key bar scrolled part of the way: its keys must stay inside the bar's own shape.
            if let bar = keyBar() {
                bar.scrollView.setContentOffset(CGPoint(x: 137, y: 0), animated: false)
                try await shot(window, prefix + "-1b-shell-keybar-scrolled")
                bar.scrollView.setContentOffset(CGPoint(x: 155, y: 0), animated: false)
                try await shot(window, prefix + "-1c-shell-keybar-scrolled-partial")
                bar.scrollView.setContentOffset(.zero, animated: false)
            }
            model.selectChat(chatID)
            await eventually("chat up") { model.conversation(self.chatID).following }
            window.endEditing(true)
            try await shot(window, prefix + "-2-chat")
            model.deselectChat()
            try await shot(window, prefix + "-3-shell-again")
            try await record(prefix + "-switch") {
                for _ in 0..<3 {
                    model.selectChat(chatID)
                    try await Task.sleep(for: .milliseconds(1200))
                    window.endEditing(true)
                    model.deselectChat()
                    try await Task.sleep(for: .milliseconds(1200))
                }
            }
            window.endEditing(true)
            await model.disconnect()
            window.isHidden = true
            try? keychain.delete()
        }
    }

    // MARK: - The chat screen's design: composer, header, cards

    private final class HeldEngine: SpeechEngine {
        var onEvent: ((SpeechEngineEvent) -> Void)?
        func start(vocabulary: SpeechVocabulary) async { onEvent?(.ready) }
        func finish() {}
        func cancel() {}
    }
    private func views<T: UIView>(_ type: T.Type, in view: UIView) -> [T] {
        ((view as? T).map { [$0] } ?? []) + view.subviews.flatMap { views(type, in: $0) }
    }
    /// Opens the system menu of the button at `point` (a test cannot tap): UIKit's own presentation of a button's menu.
    private func openMenu(at point: CGPoint, in window: UIWindow) async -> Bool {
        var hit = window.hitTest(point, with: nil)
        var chain: [String] = []
        while let view = hit {
            chain.append("\(type(of: view)) interactions=\(view.interactions.map { "\(type(of: $0))" })")
            if let interaction = view.interactions.compactMap({ $0 as? UIContextMenuInteraction }).first {
                let selector = NSSelectorFromString("_presentMenuAtLocation:")
                if interaction.responds(to: selector) {
                    _ = interaction.perform(selector, with: NSValue(cgPoint: view.convert(point, from: window)))
                    try? await Task.sleep(for: .milliseconds(800))
                    return true
                }
            }
            hit = view.superview
        }
        print("MENU-CHAIN", chain.joined(separator: " <- "))
        return false
    }
    private func dismissMenus(_ window: UIWindow) async {
        for view in views(UIView.self, in: window) {
            for interaction in view.interactions.compactMap({ $0 as? UIContextMenuInteraction }) { interaction.dismissMenu() }
        }
        try? await Task.sleep(for: .milliseconds(600))
    }
    private static let diff = (["--- /dev/null", "+++ b/docs/brief.md", "@@ -0,0 +1,25 @@"] + (1...25).map { "+Line \($0) of the brief: what the composer and the header should do." }).joined(separator: "\n")
    private func designItems() -> [ChatEvent] {
        var events: [ChatEvent] = []
        events.append(.itemCompleted(ChatItem(id: "u1", status: .completed, body: .userMessage("The composer on iOS needs a design pass: we lose too much space for the buttons around."))))
        events.append(.itemCompleted(ChatItem(id: "r1", status: .completed, body: .reasoning("Look at the composer row first."))))
        events.append(.itemCompleted(ChatItem(id: "read", status: .completed, body: .toolCall(server: nil, tool: "Read", input: .object(["file_path": .string("/fixture/ios/RiWorkRemote/ChatComposer.swift")]), output: (1...30).map { "\($0)\tline \($0) of the composer" }.joined(separator: "\n")))))
        events.append(.itemCompleted(ChatItem(id: "grep", status: .completed, body: .toolCall(server: nil, tool: "Grep", input: .object(["pattern": .string("ChatToolbar"), "path": .string("ios")]), output: "ios/RiWorkRemote/ChatView.swift:41\nios/RiWorkRemote/ChatView.swift:130"))))
        events.append(.itemCompleted(ChatItem(id: "edit", status: .completed, body: .fileChange([ChatFileChange(path: "/tmp/brief-ios-chat-chrome.md", kind: .add, diff: Self.diff)]))))
        events.append(.itemCompleted(ChatItem(id: "edit2", status: .completed, body: .fileChange([ChatFileChange(path: "ios/RiWorkRemote/ChatComposer.swift", kind: .modify, diff: "@@ -1,3 +1,3 @@\n-old line\n+new line\n context"), ChatFileChange(path: "ios/RiWorkRemote/ChatView.swift", kind: .modify, diff: "@@ -5 +5 @@\n-a\n+b")]))))
        events.append(.itemCompleted(ChatItem(id: "cmd", status: .completed, body: .command(command: "export RIWORK_HOME=/Users/me/.local/share/riwork; R=/Users/me/ocean/riWork; cd $R && cargo test -p remote", cwd: "/fixture", output: (0..<20).map { "test rpc::chat::case_\($0) ... ok" }.joined(separator: "\n"), exitCode: 0))))
        events.append(.itemCompleted(ChatItem(id: "cmd-fail", status: .completed, body: .command(command: "swift test --package-path ios", cwd: "/fixture", output: "error: build failed", exitCode: 1))))
        events.append(.itemCompleted(ChatItem(id: "agent", status: .completed, body: .toolCall(server: nil, tool: "Agent", input: .object(["description": .string("Check staged attachments protocol")]), output: "No: staged draft attachments are view-local on the Mac."))))
        events.append(.itemCompleted(ChatItem(id: "web", status: .completed, body: .webSearch("SwiftUI menu sections iOS 26"))))
        for index in 0..<4 {
            events.append(.itemCompleted(ChatItem(id: "m\(index)", status: .completed, body: .agentMessage("Message \(index). Two workers are running off origin/main: a readable paragraph in this fixture conversation, long enough to wrap."))))
        }
        return events
    }
    private func usage(_ percent: Double) -> ChatUsage {
        ChatUsage(inputTokens: 30_000, outputTokens: 3_000, contextWindow: 1_000_000, contextUsed: UInt64(percent * 10_000), costUSD: percent > 50 ? 14.2 : 0.66)
    }

    /// `RIWORK_TAB_SCREENSHOTS_ONLY=testChatDesign scripts/tab-chrome-screenshots.sh <dir> <udid>`: the chat screen in every state the
    /// design pass is about, with the software keyboard up where the composer is, in Native light and dark and the terminal look.
    func testChatDesign() async throws {
        guard ProcessInfo.processInfo.environment["RIWORK_TAB_SCREENSHOTS_ONLY"] == "testChatDesign" else { throw XCTSkip("Set RIWORK_TAB_SCREENSHOTS_ONLY=testChatDesign") }
        DictationController.shared.isAllowed = true
        defer {
            DictationController.shared.cancel()
            DictationController.shared.isAllowed = false
            DictationController.shared.makeEngine = { DictationController.defaultEngine() }
            DictationController.shared.silenceAfterSpeech = DictationMachine.silenceAfterSpeech
        }
        for look in [Look.nativeDark, .nativeLight, .terminal] {
            guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene") }
            let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
            let pairing = try Pairing.parse("""
            {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
            """)
            var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
            desktop.selectedProjectID = project; desktop.selectedSessionID = ChatTransport.shell
            try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
            let suite = "com.riwork.tests.tabchrome.\(UUID().uuidString)"
            defaultsNames.append(suite)
            let info = ChatInfo(id: chatID, provider: .claude, projectID: project, cwd: "/fixture", title: "Project orchestrator", createdAtUnix: 10,
                                model: "claude-fable-5-1", effort: "high", approvalMode: .full, state: .idle)
            let transport = ChatTransport(chats: [info], appearance: appearance(look, mic: true))
            await transport.setShellOutput((0..<40).map { "\u{1b}[32m~/fixture\u{1b}[0m $ make test  # line \($0)" }.joined(separator: "\r\n"))
            let models = [ChatModelOption(id: "claude-fable-5-1", name: "Claude Fable 5.1", description: "Frontier", efforts: ["low", "medium", "high"], defaultEffort: "medium", supportsFast: true, isDefault: true),
                          ChatModelOption(id: "claude-opus-5-5", name: "Claude Opus 5.5", description: "Strong", efforts: ["low", "medium", "high"], defaultEffort: "medium")]
            await transport.enableSnapshots()
            await transport.append(chatID, [.info(info), .models(models), .usage(usage(3.3))] + designItems())
            let model = RemoteModel(client: transport, keychain: keychain, defaults: UserDefaults(suiteName: suite)!, chatWaitMilliseconds: 300,
                                    chatIdleInterval: .milliseconds(20), hardwareKeyboard: HardwareKeyboardMonitor(probe: { false }))
            await model.connect()
            if look != .terminal { await eventually("Native look") { model.theme.style.native && model.theme.style.mic } }
            let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))
            let layout = ChatLayoutInspection()
            let host = UIHostingController(rootView: AnyView(ThemedTabs(model: model, project: projectValue).environment(\.chatLayoutInspection, layout)))
            let window = UIWindow(windowScene: scene)
            window.frame = scene.coordinateSpace.bounds
            window.windowLevel = .alert + 1
            window.rootViewController = host
            window.overrideUserInterfaceStyle = look == .nativeDark ? .dark : .light
            window.makeKeyAndVisible()
            windows.forEach { $0.isHidden = true }
            windows.append(window)
            await eventually("terminal up") { model.terminalArea != nil && model.hasOutput }
            let prefix = look.rawValue
            try await shot(window, prefix + "-01-shell-header")
            model.selectChat(chatID)
            let conversation = model.conversation(chatID)
            await eventually("chat up") { conversation.following && conversation.transcript.items.count > 5 }
            window.endEditing(true)
            try await Task.sleep(for: .milliseconds(400))
            try await shot(window, prefix + "-02-chat-header")
            if look != .terminal {
                if await openMenu(at: CGPoint(x: window.bounds.maxX - 24, y: (layout.frames["navigation"]?.midY ?? 84)), in: window) {
                    try await shot(window, prefix + "-03-menu-open")
                    await dismissMenus(window)
                }
            }
            // Cards: collapsed (above), then open.
            conversation.expanded = ["edit", "edit#/tmp/brief-ios-chat-chrome.md", "cmd", "read", "edit2"]
            conversation.jumpToEnd()
            try await Task.sleep(for: .milliseconds(500))
            try await shot(window, prefix + "-04-cards-open-bottom")
            if let scroll = views(UIScrollView.self, in: window).filter({ $0.bounds.height > 200 && $0.contentSize.height > $0.bounds.height }).max(by: { $0.bounds.height < $1.bounds.height }) {
                scroll.setContentOffset(CGPoint(x: 0, y: max(0, scroll.contentSize.height * 0.08)), animated: false)
                try await Task.sleep(for: .milliseconds(300))
                try await shot(window, prefix + "-05a-cards-open-top")
                scroll.setContentOffset(CGPoint(x: 0, y: max(0, scroll.contentSize.height * 0.25)), animated: false)
                try await Task.sleep(for: .milliseconds(300))
                try await shot(window, prefix + "-05-cards-open-edit")
                scroll.setContentOffset(CGPoint(x: 0, y: max(0, scroll.contentSize.height * 0.5)), animated: false)
                try await Task.sleep(for: .milliseconds(300))
                try await shot(window, prefix + "-05b-cards-open-middle")
            }
            conversation.expanded = []
            conversation.jumpToEnd()
            try await Task.sleep(for: .milliseconds(400))
            try await shot(window, prefix + "-06-cards-collapsed")
            // The composer with the software keyboard up.
            guard let field = views(ChatComposerTextView.self, in: window).first else { XCTFail("no composer"); continue }
            field.becomeFirstResponder()
            try await Task.sleep(for: .milliseconds(900))
            try await shot(window, prefix + "-10-kb-empty")
            // Scrolled up: Latest shows over the transcript's bottom edge.
            if let scroll = views(UIScrollView.self, in: window).filter({ $0.bounds.height > 60 && $0.contentSize.height > $0.bounds.height }).max(by: { $0.bounds.height < $1.bounds.height }) {
                scroll.delegate?.scrollViewWillBeginDragging?(scroll)
                scroll.setContentOffset(CGPoint(x: 0, y: max(0, scroll.contentSize.height - scroll.bounds.height - 400)), animated: false)
                scroll.delegate?.scrollViewDidEndDragging?(scroll, willDecelerate: false)
                try await Task.sleep(for: .milliseconds(500))
                await transport.append(chatID, [.itemCompleted(ChatItem(id: "live", status: .completed, body: .agentMessage("One new live message.")))])
                try await Task.sleep(for: .milliseconds(600))
                try await shot(window, prefix + "-11-kb-latest")
                conversation.jumpToEnd()
                try await Task.sleep(for: .milliseconds(400))
            }
            conversation.draft = "Make the composer field take the width"
            try await Task.sleep(for: .milliseconds(300))
            try await shot(window, prefix + "-12-kb-text")
            conversation.notice = "Couldn’t send: the link dropped. Your message is kept."
            try await Task.sleep(for: .milliseconds(300))
            try await shot(window, prefix + "-12b-kb-notice")
            conversation.notice = nil
            conversation.draft = (1...9).map { "Line \($0) of a long message that grows the field" }.joined(separator: "\n")
            try await Task.sleep(for: .milliseconds(300))
            try await shot(window, prefix + "-13-kb-multiline")
            conversation.draft = ""
            let engine = HeldEngine()
            DictationController.shared.makeEngine = { engine }
            DictationController.shared.silenceAfterSpeech = .seconds(120)
            DictationController.shared.toggle(for: .chat(chatID), live: { _ in }, deliver: { _ in })
            try await Task.sleep(for: .milliseconds(400))
            engine.onEvent?(.level(0.6))
            engine.onEvent?(.heard("make the composer"))
            try await Task.sleep(for: .milliseconds(300))
            try await shot(window, prefix + "-14-kb-dictating")
            DictationController.shared.cancel()
            await transport.append(chatID, [.state(.running), .usage(usage(87))])
            await eventually("running") { model.chatState(info).isBusy }
            try await Task.sleep(for: .milliseconds(300))
            try await shot(window, prefix + "-15-kb-running")
            conversation.draft = "Also keep Stop reachable"
            try await Task.sleep(for: .milliseconds(300))
            try await shot(window, prefix + "-16-kb-running-text")
            conversation.draft = ""
            window.endEditing(true)
            try await Task.sleep(for: .milliseconds(500))
            try await shot(window, prefix + "-17-usage-high")
            if let action = layout.actions["usage"] {
                action()
                try await Task.sleep(for: .milliseconds(700))
                try await shot(window, prefix + "-18-usage-detail")
                host.presentedViewController?.dismiss(animated: false)
                try await Task.sleep(for: .milliseconds(400))
            }
            await transport.append(chatID, [.usage(usage(3.3))])
            try await Task.sleep(for: .milliseconds(300))
            if let action = layout.actions["usage"] {
                action()
                try await Task.sleep(for: .milliseconds(700))
                try await shot(window, prefix + "-19-usage-low-detail")
                host.presentedViewController?.dismiss(animated: false)
                try await Task.sleep(for: .milliseconds(400))
            }
            // The banner row: the provider's notices of this turn (one per kind) and the phone's own, above the composer.
            await transport.append(chatID, [.state(.idle), .itemCompleted(ChatItem(id: "u-notice", status: .completed, body: .userMessage("Run the suite"))),
                                            .itemCompleted(ChatItem(id: "nr1", status: .completed, body: .notice(level: .warning, text: "Reconnecting… 1/5", kind: "reconnecting"))),
                                            .itemCompleted(ChatItem(id: "nr2", status: .completed, body: .notice(level: .warning, text: "Reconnecting… 2/5", kind: "reconnecting"))),
                                            .itemCompleted(ChatItem(id: "nl", status: .completed, body: .notice(level: .warning, text: "This account is close to the weekly usage limit", kind: "rate_limit:seven_day"))),
                                            .itemCompleted(ChatItem(id: "ne", status: .completed, body: .notice(level: .error, text: "Request failed: overloaded", kind: "turn_failed")))])
            conversation.notice = "Couldn’t send: the Mac is busy. Your message is kept."
            try await Task.sleep(for: .milliseconds(600))
            try await shot(window, prefix + "-20-banners")
            if let more = layout.actions["notices-history"] {
                more()
                try await Task.sleep(for: .milliseconds(800))
                try await shot(window, prefix + "-21-notice-history")
                host.presentedViewController?.dismiss(animated: false)
                try await Task.sleep(for: .milliseconds(400))
            }
            conversation.notice = nil
            await model.disconnect()
            try await Task.sleep(for: .milliseconds(500))
            try await shot(window, prefix + "-22-not-connected")
            window.isHidden = true
            try? keychain.delete()
        }
    }
    /// `RIWORK_TAB_SCREENSHOTS_ONLY=testSharedTabs scripts/tab-chrome-screenshots.sh <dir> <udid>`: the row on the desktop's shared tab
    /// list (a pinned orchestrator and a user chat), the open-worker picker, the close sheet, a reorder in progress (the drop bar and
    /// the Edit tabs sheet) and the setting row, in Native light and dark.
    func testSharedTabs() async throws {
        guard ProcessInfo.processInfo.environment["RIWORK_TAB_SCREENSHOTS_ONLY"] == "testSharedTabs" else { throw XCTSkip("Set RIWORK_TAB_SCREENSHOTS_ONLY=testSharedTabs") }
        let user = "dddddddd-3333-4333-8333-333333333333", worker = "eeeeeeee-4444-4444-8444-444444444444", worker2 = "ffffffff-5555-4555-8555-555555555555"
        for look in [Look.nativeDark, .nativeLight] {
            guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene") }
            let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
            let pairing = try Pairing.parse("""
            {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
            """)
            var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
            desktop.selectedProjectID = project; desktop.selectedSessionID = ChatTransport.shell
            try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
            let suite = "com.riwork.tests.tabchrome.\(UUID().uuidString)"
            defaultsNames.append(suite)
            let chats = [ChatInfo(id: chatID, provider: .claude, projectID: project, cwd: "/fixture", title: "Project orchestrator", createdAtUnix: 10, state: .idle),
                         ChatInfo(id: user, provider: .codex, projectID: project, cwd: "/fixture", title: "Fix the build", createdAtUnix: 20, state: .idle),
                         ChatInfo(id: worker, provider: .codex, projectID: project, cwd: "/fixture", title: "Codex worker", createdAtUnix: 30, state: .running),
                         ChatInfo(id: worker2, provider: .claude, projectID: project, cwd: "/fixture", title: "Review worker", createdAtUnix: 40, state: .idle)]
            let transport = ChatTransport(chats: chats, appearance: appearance(look))
            await transport.setShellOutput((0..<40).map { "\u{1b}[32m~/fixture\u{1b}[0m $ make test  # line \($0)" }.joined(separator: "\r\n"))
            await transport.setSharedTabs([
                .init(key: "chat:\(chatID)", kind: "chat", title: "Project orchestrator", pinned: true),
                .init(key: "chat:\(user)", kind: "chat", title: "Fix the build", status: "waiting"),
                .init(key: "chat:\(worker)", kind: "chat", title: "Codex worker", hidden: true, worker: true, parent: "chat:\(user)"),
                .init(key: "chat:\(worker2)", kind: "chat", title: "Review worker", status: "done", hidden: true, worker: true, parent: "chat:\(chatID)"),
                .init(key: "shell:\(ChatTransport.shell)", kind: "shell", title: "zsh · main")])
            await transport.append(chatID, [.info(chats[0])] + (0..<6).map { .itemCompleted(ChatItem(id: "m\($0)", status: .completed, body: .agentMessage("Message \($0). The orchestrator coordinates the workers of this project."))) })
            await transport.append(user, [.info(chats[1]), .itemCompleted(ChatItem(id: "u", status: .completed, body: .userMessage("Fix the build please")))])
            let model = RemoteModel(client: transport, keychain: keychain, defaults: UserDefaults(suiteName: suite)!, chatWaitMilliseconds: 300,
                                    chatIdleInterval: .milliseconds(20), hardwareKeyboard: HardwareKeyboardMonitor(probe: { true }))
            await model.connect()
            await eventually("Native look") { model.theme.style.native }
            let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))
            let layout = ChatLayoutInspection()
            let host = UIHostingController(rootView: AnyView(ThemedTabs(model: model, project: projectValue).environment(\.chatLayoutInspection, layout)))
            let window = UIWindow(windowScene: scene)
            window.frame = scene.coordinateSpace.bounds
            window.windowLevel = .alert + 1
            window.rootViewController = host
            window.overrideUserInterfaceStyle = look == .nativeDark ? .dark : .light
            window.makeKeyAndVisible()
            windows.forEach { $0.isHidden = true }
            windows.append(window)
            await eventually("shared row") { model.sharedTabs != nil && model.tabs.count == 3 }
            model.selectChat(user)
            await eventually("chat up") { model.conversation(user).following }
            try await Task.sleep(for: .milliseconds(600))
            window.endEditing(true)
            try await Task.sleep(for: .milliseconds(500))
            let prefix = "tabs-" + look.rawValue
            try await shot(window, prefix + "-1-row")
            if let preview = layout.actions["drop-preview"] {
                preview()
                try await Task.sleep(for: .milliseconds(300))
                try await shot(window, prefix + "-2-reorder-drop-bar")
            }
            for (name, action) in [("3-open-worker-picker", "open-workers"), ("4-edit-tabs", "edit-tabs"), ("5-close-sheet", "close-current"), ("6-setting", "display-settings")] {
                guard let run = layout.actions[action] else { XCTFail("no \(action)"); continue }
                run()
                try await Task.sleep(for: .milliseconds(900))
                try await shot(window, prefix + "-" + name)
                host.presentedViewController?.dismiss(animated: false)
                try await Task.sleep(for: .milliseconds(500))
            }
            await model.disconnect()
            window.isHidden = true
            try? keychain.delete()
        }
    }

    /// `RIWORK_TAB_SCREENSHOTS_ONLY=testSimulatorTaps`: real taps on the simulator (by whoever runs it): the tab already on screen, then
    /// the hidden shell's row in the open-worker picker. After each, the main thread must still answer within a second (a layout loop
    /// used to freeze the app here). Leaves `<name>.manual`, waits for `<name>.done`, writes `<name>.result`.
    func testSimulatorTaps() async throws {
        guard ProcessInfo.processInfo.environment["RIWORK_TAB_SCREENSHOTS_ONLY"] == "testSimulatorTaps" else { throw XCTSkip("Set RIWORK_TAB_SCREENSHOTS_ONLY=testSimulatorTaps") }
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene") }
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = ChatTransport.shell
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        let suite = "com.riwork.tests.tabchrome.\(UUID().uuidString)"
        defaultsNames.append(suite)
        let user = "dddddddd-3333-4333-8333-333333333333"
        let transport = ChatTransport(chats: [ChatInfo(id: user, provider: .codex, projectID: project, cwd: "/fixture", title: "User chat", createdAtUnix: 20, state: .idle)], appearance: appearance(.nativeDark))
        await transport.setShellOutput((0..<40).map { "\u{1b}[32m~/fixture\u{1b}[0m $ make test  # line \($0)" }.joined(separator: "\r\n"))
        await transport.setSharedTabs([.init(key: "chat:\(user)", kind: "chat", title: "User chat"),
                                       .init(key: "shell:\(ChatTransport.shell)", kind: "shell", title: "worker zsh", worker: true, parent: "chat:\(user)")])
        let model = RemoteModel(client: transport, keychain: keychain, defaults: UserDefaults(suiteName: suite)!, chatWaitMilliseconds: 300,
                                chatIdleInterval: .milliseconds(20), hardwareKeyboard: HardwareKeyboardMonitor(probe: { true }))
        await model.connect()
        let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))
        let layout = ChatLayoutInspection()
        let host = UIHostingController(rootView: AnyView(ThemedTabs(model: model, project: projectValue).environment(\.chatLayoutInspection, layout)))
        let window = UIWindow(windowScene: scene)
        window.frame = scene.coordinateSpace.bounds
        window.windowLevel = .alert + 1
        window.rootViewController = host
        window.overrideUserInterfaceStyle = .dark
        window.makeKeyAndVisible()
        windows.forEach { $0.isHidden = true }
        windows.append(window)
        await eventually("shell on screen") { model.sharedTabs != nil && model.sessionID == ChatTransport.shell && model.hasOutput }
        func step(_ name: String) async throws {
            let marker = directory.appendingPathComponent(name + ".manual"), done = directory.appendingPathComponent(name + ".done")
            try Data().write(to: marker)
            let deadline = Date().addingTimeInterval(240)
            while !FileManager.default.fileExists(atPath: done.path), Date() < deadline { try await Task.sleep(for: .milliseconds(200)) }
            try? FileManager.default.removeItem(at: marker)
            // Responsive: a short sleep on the main actor comes back in time, several times over.
            var worst: Duration = .zero
            for _ in 0..<10 {
                let start = ContinuousClock.now
                try await Task.sleep(for: .milliseconds(50))
                worst = max(worst, ContinuousClock.now - start)
            }
            let line = "\(name) selected=\(model.sessionID ?? "nil") worst=\(worst) tabs=\(model.tabs.map(\.id))"
            try Data(line.utf8).write(to: directory.appendingPathComponent(name + ".result"))
            XCTAssertLessThan(worst, .seconds(1), line)
        }
        // 1. A tap on the tab already on screen.
        try await step("tap-1-selected-tab")
        // 2. Close it (a worker: detached), then open it again from the picker with a tap on its row.
        _ = try await model.closeTab("shell:\(ChatTransport.shell)", choice: .detach)
        layout.actions["open-workers"]?()
        try await Task.sleep(for: .milliseconds(800))
        try await step("tap-2-picker-row")
        XCTAssertEqual(model.sessionID, ChatTransport.shell, "reopened from the picker")
        await model.disconnect()
        window.isHidden = true
        try? keychain.delete()
    }

    /// A state a person must finish by hand (a system menu, which a test cannot open): leaves `<name>.manual` and waits a while for
    /// `<name>.png`, taken by whoever opened the menu (`xcrun simctl io <udid> screenshot <name>.png`); goes on without it.
    private func manualShot(_ name: String) async throws {
        let marker = directory.appendingPathComponent(name + ".manual"), png = directory.appendingPathComponent(name + ".png")
        try? FileManager.default.removeItem(at: png)
        try Data().write(to: marker)
        let deadline = Date().addingTimeInterval(45)
        while !FileManager.default.fileExists(atPath: png.path), Date() < deadline { try await Task.sleep(for: .milliseconds(200)) }
        try? FileManager.default.removeItem(at: marker)
        try await Task.sleep(for: .milliseconds(1500))
    }
}
