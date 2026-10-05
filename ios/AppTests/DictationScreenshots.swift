import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// Pictures of the mic in the key bar and in the chat composer, idle and listening, in the terminal look and in Native. Not a test of
/// anything: `scripts/dictation-screenshots.sh <directory>` runs it and writes the PNGs there; skipped otherwise.
@MainActor final class DictationScreenshots: XCTestCase {
    private final class HeldEngine: SpeechEngine {
        var onEvent: ((SpeechEngineEvent) -> Void)?
        func start(vocabulary: SpeechVocabulary) async { onEvent?(.ready) }
        func finish() {}
        func cancel() {}
    }
    private var directory: URL!
    private var windows: [UIWindow] = []

    override func setUp() async throws {
        guard let path = ProcessInfo.processInfo.environment["RIWORK_SPEECH_SCREENSHOTS"] else { throw XCTSkip("Set RIWORK_SPEECH_SCREENSHOTS") }
        directory = URL(fileURLWithPath: path)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        DictationController.shared.isAllowed = true
    }
    override func tearDown() async throws {
        DictationController.shared.cancel()
        DictationController.shared.isAllowed = false
        DictationController.shared.makeEngine = { DictationController.defaultEngine() }
        DictationController.shared.silenceAfterSpeech = DictationMachine.silenceAfterSpeech
        windows.forEach { $0.isHidden = true }
        windows = []
    }

    private func style(native: Bool) -> DesktopStyle {
        var theme = DesktopTheme.builtIn
        theme.native = native
        // The mics are there only while the desktop's mic setting is on.
        theme.mic = true
        return DesktopStyle(theme)
    }
    private func settle() async { try? await Task.sleep(for: .milliseconds(400)) }
    /// The simulator's own screenshot (glass is drawn only on screen): this leaves `<name>.ready` and waits for `<name>.png`, which
    /// `scripts/dictation-screenshots.sh` takes with `simctl io screenshot` while the window is up.
    private func save(_ window: UIWindow, _ name: String) async throws {
        window.layoutIfNeeded()
        let ready = directory.appendingPathComponent(name + ".ready"), shot = directory.appendingPathComponent(name + ".png")
        try? FileManager.default.removeItem(at: shot)
        try Data().write(to: ready)
        let deadline = Date().addingTimeInterval(20)
        while !FileManager.default.fileExists(atPath: shot.path), Date() < deadline { try await Task.sleep(for: .milliseconds(200)) }
        try? FileManager.default.removeItem(at: ready)
        XCTAssertTrue(FileManager.default.fileExists(atPath: shot.path), "no screenshot taken for \(name)")
    }
    private func window(height: CGFloat, dark: Bool) -> UIWindow {
        // In the app's scene and above its window, or it is never on screen.
        let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
        let window = scene.map(UIWindow.init(windowScene:)) ?? UIWindow(frame: UIScreen.main.bounds)
        window.frame = UIScreen.main.bounds
        window.windowLevel = .alert + 1
        window.overrideUserInterfaceStyle = dark ? .dark : .light
        windows.forEach { $0.isHidden = true }
        windows.append(window)
        return window
    }
    /// Starts a dictation that hears `text` and stays listening.
    private func listen(_ owner: DictationOwner, _ text: String, live: @escaping (String) -> Void = { _ in }) async {
        let engine = HeldEngine()
        DictationController.shared.makeEngine = { engine }
        // Taking a picture is slow: the pause after speech must not end the dictation meanwhile.
        DictationController.shared.silenceAfterSpeech = .seconds(120)
        DictationController.shared.toggle(for: owner, live: live, deliver: { _ in })
        await settle()
        engine.onEvent?(.level(0.6))
        engine.onEvent?(.heard(text))
    }

    /// The bottom of a terminal: the dictation panel over the pane, and the key bar under it.
    private func terminal(native: Bool, dark: Bool, review: String?) -> (UIWindow, KeyBarView) {
        let style = style(native: native)
        let window = window(height: 260, dark: dark)
        let root = UIViewController()
        root.view.backgroundColor = style.terminalBackgroundUI
        let top = window.bounds.height - 260 - 34
        let panel = UIHostingController(rootView: TerminalPanelHost(review: review).environment(\.desktopStyle, style))
        panel.view.backgroundColor = .clear
        root.addChild(panel)
        panel.view.frame = CGRect(x: 0, y: top, width: window.bounds.width, height: 260 - KeyBarView.height)
        root.view.addSubview(panel.view)
        let bar = KeyBarView()
        bar.style = style
        bar.frame = CGRect(x: 0, y: top + 260 - KeyBarView.height, width: window.bounds.width, height: KeyBarView.height)
        root.view.addSubview(bar)
        window.rootViewController = root
        window.makeKeyAndVisible()
        bar.applyPlacement(safeArea: KeyBarInsets(left: 0, bottom: 0, right: 0), position: .aboveKeyboard)
        // Scrolled to the end, where the mic and Hide are anyway.
        bar.layoutIfNeeded()
        bar.scrollView.setContentOffset(CGPoint(x: max(0, bar.scrollView.contentSize.width - bar.scrollView.bounds.width), y: 0), animated: false)
        return (window, bar)
    }
    private struct TerminalPanelHost: View {
        @State var review: String?
        var body: some View {
            VStack { Spacer(); TerminalDictationPanel(controller: .shared, review: $review, canType: true, type: { _, _ in }) }
        }
    }

    func testKeyBar() async throws {
        for native in [false, true] {
            let look = native ? "native" : "terminal"
            var (window, bar) = terminal(native: native, dark: true, review: nil)
            await settle()
            try await save(window, "keybar-\(look)-idle")
            await listen(.terminal, "git checkout ios-speech-input")
            bar.setDictation(.listening)
            await settle()
            try await save(window, "keybar-\(look)-listening")
            DictationController.shared.cancel()
            (window, bar) = terminal(native: native, dark: true, review: "git checkout ios-speech-input")
            await settle()
            // The review's field takes the keyboard; this window has no keyboard avoidance, so the picture is without it.
            window.endEditing(true)
            await settle()
            try await save(window, "keybar-\(look)-review")
        }
    }

    private struct ComposerHost: View {
        let conversation: ChatConversation
        var body: some View {
            VStack(spacing: 0) {
                Spacer()
                ChatComposer(conversation: conversation, provider: .claude, state: .idle, approval: nil, connected: true, focusToken: 0,
                             send: {}, interrupt: {}, decide: { _ in })
            }
        }
    }
    func testComposer() async throws {
        for native in [false, true] {
            let look = native ? "native" : "terminal"
            let style = style(native: native)
            let conversation = ChatConversation(id: "shot-\(look)")
            conversation.draft = "Please "
            let window = window(height: 200, dark: false)
            let host = UIHostingController(rootView: ComposerHost(conversation: conversation).environment(\.desktopStyle, style))
            host.view.backgroundColor = style.backgroundUI
            window.rootViewController = host
            window.makeKeyAndVisible()
            await settle()
            try await save(window, "composer-\(look)-idle")
            // The words go into the composer's text view as they are heard, as they do in the app.
            func find(_ view: UIView) -> UITextView? { (view as? UITextView) ?? view.subviews.lazy.compactMap(find).first }
            let insertion = TextInsertion()
            insertion.view = find(host.view)
            await listen(.chat("shot-\(look)"), "fix the failing KeyBarView test") { insertion.show($0) }
            await settle()
            try await save(window, "composer-\(look)-listening")
            DictationController.shared.cancel()
        }
    }
}
