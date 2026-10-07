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

    private func appearance(_ look: Look) -> JSONValue? {
        guard look != .terminal else { return nil }
        let dark = look == .nativeDark
        let colors = dark
            ? ["bg": "#000000", "panel": "#1c1c1e", "panel_active": "#2c2c2e", "divider": "#3a3a3c", "cyan": "#ffffff", "magenta": "#c7c7cc", "gold": "#ff9f0a", "text": "#f5f5f7", "muted": "#98989d"]
            : ["bg": "#ffffff", "panel": "#f5f5f7", "panel_active": "#e8e8ed", "divider": "#d2d2d7", "cyan": "#000000", "magenta": "#3a3a3c", "gold": "#b34000", "text": "#1d1d1f", "muted": "#636366"]
        return .object(["v": .number(1), "updated_at": .number(1_790_000_000), "dark": .bool(dark), "native": .bool(true),
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
}
