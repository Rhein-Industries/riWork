import XCTest
import UIKit
import SwiftUI
import AVFoundation
import RiWorkCore
@testable import RiWorkRemote

/// Dictation without a microphone: the controller with scripted engines, the text going into a text view, and the key bar's mic.
@MainActor final class DictationTests: XCTestCase {
    /// An engine that does only what the test says.
    private final class ManualEngine: SpeechEngine {
        var onEvent: ((SpeechEngineEvent) -> Void)?
        var started = false, finished = false, cancelled = false
        var startProblem: DictationProblem?
        var finalOnFinish: String?
        var finishCount = 0
        func start(vocabulary: SpeechVocabulary) async {
            started = true
            if let startProblem { onEvent?(.failed(startProblem)) } else { onEvent?(.ready) }
        }
        func finish() { finished = true; finishCount += 1; if let finalOnFinish { send(.finished(finalOnFinish)) } }
        func cancel() { cancelled = true }
        func send(_ event: SpeechEngineEvent) { onEvent?(event) }
    }
    private final class Received { var live: [String] = []; var delivered: [String] = [] }

    private func makeController(_ engine: any SpeechEngine) -> DictationController {
        let controller = DictationController()
        controller.makeEngine = { engine }
        controller.sessionSources = { SpeechVocabularySources(names: ["ios-speech-input"], text: "KeyBarView") }
        // The desktop's mic setting is on.
        controller.isAllowed = true
        return controller
    }
    /// The look with the desktop's mic setting on or off.
    private func style(mic: Bool, native: Bool = false) -> DesktopStyle {
        var theme = DesktopTheme.builtIn
        theme.mic = mic
        theme.native = native
        return DesktopStyle(theme)
    }
    private func start(_ controller: DictationController, _ owner: DictationOwner = .terminal) -> Received {
        let received = Received()
        controller.toggle(for: owner, live: { received.live.append($0) }, deliver: { received.delivered.append($0) })
        return received
    }
    private func wait(until condition: @autoclosure () -> Bool, timeout: TimeInterval = 3) async {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline { try? await Task.sleep(for: .milliseconds(20)) }
        XCTAssertTrue(condition(), "condition did not become true")
    }
    override func tearDown() async throws {
        windows.forEach { $0.isHidden = true }
        windows = []
    }

    // MARK: Composer stop / send races, using real mic, Send and hardware command handlers

    @MainActor private final class Deadline {
        var continuation: CheckedContinuation<Void, Never>?
        private var fired = false
        func wait() async {
            guard !fired else { return }
            await withCheckedContinuation { continuation = $0 }
        }
        func fire() { fired = true; let continuation = continuation; self.continuation = nil; continuation?.resume() }
    }
    @MainActor private final class ComposerRig {
        let conversation = ChatConversation(id: "dictation-keys")
        let engine: ManualEngine
        let controller: DictationController
        let layout = ChatLayoutInspection()
        let deadline = Deadline()
        let window: UIWindow
        var host: UIViewController?
        var view: ChatComposerTextView!
        var sent: [String] = []
        var decisions: [ChatDecision] = []
        init(engine: ManualEngine, controller: DictationController, window: UIWindow) {
            self.engine = engine; self.controller = controller; self.window = window
        }
    }
    private func composerRig(draft: String = "", width: CGFloat = 390, native: Bool = false, clearOnSend: Bool = true) async throws -> ComposerRig {
        let engine = ManualEngine()
        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: width, height: 700)
        let controller = makeController(engine)
        let rig = ComposerRig(engine: engine, controller: controller, window: window)
        controller.waitForFinal = { [deadline = rig.deadline] in await deadline.wait() }
        rig.conversation.draft = draft
        let composer = ChatComposer(conversation: rig.conversation, provider: .claude, state: .idle, approval: nil, connected: true, focusToken: 0,
                                    send: { [weak rig] in
                                        guard let rig, !rig.conversation.sending, rig.conversation.hasMessage else { return }
                                        rig.conversation.sending = true
                                        rig.sent.append(rig.conversation.draft)
                                        if clearOnSend { rig.conversation.draft = "" }
                                    }, interrupt: {}, decide: { [weak rig] in rig?.decisions.append($0) }, dictation: controller)
        let host = UIHostingController(rootView: AnyView(VStack { Spacer(); composer }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .desktopThemed(style(mic: true, native: native)).environment(\.chatLayoutInspection, rig.layout)))
        rig.host = host
        host.view.frame = window.bounds
        window.rootViewController = host
        windows.append(window)
        window.makeKeyAndVisible()
        host.view.setNeedsLayout(); host.view.layoutIfNeeded()
        func find(_ view: UIView) -> ChatComposerTextView? { (view as? ChatComposerTextView) ?? view.subviews.lazy.compactMap(find).first }
        await wait(until: find(host.view) != nil && rig.layout.actions["dictation-target"] != nil && rig.layout.actions["send"] != nil && rig.layout.frames["composer"] != nil && rig.layout.frames["composer-field"] != nil)
        if find(host.view) == nil {
            func describe(_ view: UIView) -> String { "\(type(of: view)) \(view.frame) [\(view.subviews.map(describe).joined(separator: ", "))]" }
            print("COMPOSER_FIXTURE \(describe(host.view)) window=\(window.frame) hidden=\(window.isHidden) frames=\(rig.layout.frames)")
        }
        rig.view = try XCTUnwrap(find(host.view))
        return rig
    }
    private func startMic(_ rig: ComposerRig) async throws {
        try XCTUnwrap(rig.layout.actions["dictation-target"])()
        await wait(until: rig.controller.phase == .listening(text: ""))
    }
    private func key(_ rig: ComposerRig, _ input: String, _ flags: UIKeyModifierFlags = []) throws {
        let command = try XCTUnwrap(rig.view.keyCommands?.first { $0.input == input && $0.modifierFlags == flags })
        rig.view.fired(command)
    }

    func testEscapeStopsAndKeepsTypedAndVolatileTextWithoutSending() async throws {
        let rig = try await composerRig(draft: "Typed before")
        try await startMic(rig)
        rig.engine.send(.heard("dictated words"))
        XCTAssertEqual(rig.view.text, "Typed before dictated words")
        // Typing after the live span must survive its final replacement, without duplicating the words.
        rig.view.text += " typed after"
        rig.view.delegate?.textViewDidChange?(rig.view)
        try key(rig, UIKeyCommand.inputEscape)
        XCTAssertTrue(rig.engine.finished)
        XCTAssertEqual(rig.view.text, "Typed before dictated words typed after")
        XCTAssertTrue(rig.sent.isEmpty)
        rig.engine.send(.finished("dictated words"))
        XCTAssertEqual(rig.view.text, "Typed before dictated words typed after")
        XCTAssertEqual(rig.conversation.draft, rig.view.text)
        XCTAssertTrue(rig.sent.isEmpty)
        XCTAssertEqual(rig.controller.phase, .idle)
        rig.deadline.fire()
    }
    func testEscapeWithNoFinalKeepsTheLatestVolatileSegment() async throws {
        let rig = try await composerRig(draft: "Please")
        try await startMic(rig)
        rig.engine.send(.heard("keep every word"))
        try key(rig, UIKeyCommand.inputEscape)
        await wait(until: rig.deadline.continuation != nil)
        rig.engine.send(.heard("keep every last word"))
        rig.deadline.fire()
        await wait(until: rig.controller.phase == .idle)
        XCTAssertEqual(rig.conversation.draft, "Please keep every last word")
        XCTAssertTrue(rig.sent.isEmpty)
        rig.engine.send(.finished("stale result"))
        XCTAssertEqual(rig.conversation.draft, "Please keep every last word")
    }
    func testLeavingAHostWhileFinishingKeepsWordsAndNeverSends() async throws {
        for sendPending in [false, true] {
            let rig = try await composerRig(draft: "Typed")
            try await startMic(rig)
            rig.engine.send(.heard("keep these words"))
            try key(rig, sendPending ? "\r" : UIKeyCommand.inputEscape)
            XCTAssertTrue(rig.controller.phase.isActive)
            rig.window.rootViewController = nil
            rig.host = nil
            await wait(until: !rig.controller.phase.isActive)
            XCTAssertEqual(rig.conversation.draft, "Typed keep these words")
            XCTAssertTrue(rig.sent.isEmpty)
            rig.engine.send(.finished("late words"))
            XCTAssertEqual(rig.conversation.draft, "Typed keep these words")
            rig.deadline.fire()
        }
    }
    func testSwitchingTheComposersOwnerFinishesAgainstTheOldBinding() async throws {
        let rig = try await composerRig(draft: "Old draft")
        try await startMic(rig)
        rig.engine.send(.heard("old words"))
        try key(rig, "\r")
        let next = ChatConversation(id: "next-chat")
        next.draft = "New draft"
        let composer = ChatComposer(conversation: next, provider: .claude, state: .idle, approval: nil, connected: true, focusToken: 0,
                                    send: { XCTFail("navigation must not send") }, interrupt: {}, decide: { _ in }, dictation: rig.controller)
        // Reuse the same representable identity, as the chat screen does when its selection changes.
        let host = try XCTUnwrap(rig.host as? UIHostingController<AnyView>)
        host.rootView = AnyView(VStack { Spacer(); composer }.frame(maxWidth: .infinity, maxHeight: .infinity)
            .desktopThemed(style(mic: true)).environment(\.chatLayoutInspection, rig.layout))
        await wait(until: !rig.controller.phase.isActive)
        XCTAssertEqual(rig.conversation.draft, "Old draft old words")
        XCTAssertEqual(next.draft, "New draft")
        XCTAssertTrue(rig.sent.isEmpty)
        rig.deadline.fire()
    }
    func testRecognizerFailureWhileWaitingKeepsWordsWithoutSending() async throws {
        for problem: DictationProblem in [.interrupted, .failed("recognizer lost"), .modelUnavailable] {
            let rig = try await composerRig(draft: "Typed")
            try await startMic(rig)
            rig.engine.send(.heard("last words"))
            try key(rig, "\r")
            rig.engine.send(.failed(problem))
            XCTAssertEqual(rig.conversation.draft, "Typed last words")
            XCTAssertTrue(rig.sent.isEmpty)
            rig.engine.send(.finished("stale"))
            XCTAssertTrue(rig.sent.isEmpty)
            rig.deadline.fire()
        }
    }
    func testSecondComposerForSameChatReleasesItsFinalizingReservation() async throws {
        let rig = try await composerRig()
        try await startMic(rig)
        rig.engine.send(.heard("words"))
        let first = ComposerSubmission(), second = ComposerSubmission()
        first.request(conversation: rig.conversation, dictation: rig.controller, eligible: { true }, send: { rig.sent.append("first") })
        second.request(conversation: rig.conversation, dictation: rig.controller, eligible: { true }, send: { XCTFail("second send") })
        XCTAssertTrue(first.finalizing)
        XCTAssertFalse(second.finalizing)
        rig.controller.finishNow()
        XCTAssertFalse(first.finalizing)
        XCTAssertTrue(rig.sent.isEmpty)
        rig.deadline.fire()
    }
    func testMicHintOffersKeyboardShortcutsOnlyWithHardwareKeyboard() {
        XCTAssertFalse(DictationButton.hint(active: true, compact: true, hardware: false).contains("Escape"))
        XCTAssertTrue(DictationButton.hint(active: true, compact: true, hardware: true).contains("Escape"))
    }
    func testExplicitMicCancelRestoresTypedTextWithoutChangingTheActionSlots() async throws {
        let rig = try await composerRig(draft: "Typed")
        let frame = try XCTUnwrap(rig.layout.frames["composer"])
        try await startMic(rig)
        rig.engine.send(.heard("discard these words"))
        try XCTUnwrap(rig.layout.actions["dictation-cancel"])()
        XCTAssertEqual(rig.conversation.draft, "Typed")
        XCTAssertTrue(rig.sent.isEmpty)
        XCTAssertEqual(rig.layout.frames["composer"], frame)
    }
    func testSendWaitsForLateFinalAndReturnOrSendRepeatsSubmitOnce() async throws {
        // Both orders: double Return, Send+Return, Return+Send. Hardware Cmd+Return uses the same path.
        for first in ["return", "send", "command"] {
            let rig = try await composerRig(draft: "Please")
            try await startMic(rig)
            rig.engine.send(.heard("run"))
            if first == "send" { try XCTUnwrap(rig.layout.actions["send"])() }
            else { try key(rig, "\r", first == "command" ? .command : []) }
            XCTAssertTrue(rig.engine.finished)
            XCTAssertTrue(rig.sent.isEmpty, "a partial is not sent before the final")
            try key(rig, "\r")
            try XCTUnwrap(rig.layout.actions["send"])()
            XCTAssertEqual(rig.engine.finishCount, 1)
            rig.engine.send(.heard("run every"))
            rig.engine.send(.finished("run every test."))
            XCTAssertEqual(rig.sent, ["Please run every test."])
            rig.engine.send(.finished("late duplicate"))
            try key(rig, "\r")
            XCTAssertEqual(rig.sent.count, 1)
            XCTAssertEqual(rig.conversation.draft, "")
            XCTAssertTrue(rig.decisions.isEmpty)
            rig.deadline.fire()
        }
    }
    func testSendWithSynchronousEarlyFinalDoesNotMissOrDoubleSubmit() async throws {
        let rig = try await composerRig(clearOnSend: false)
        try await startMic(rig)
        rig.engine.send(.heard("last"))
        rig.engine.finalOnFinish = "last words"
        let staleCallback = rig.engine.onEvent
        try key(rig, "\r")
        XCTAssertEqual(rig.sent, ["last words"])
        try key(rig, "\r")
        try XCTUnwrap(rig.layout.actions["send"])()
        staleCallback?(.finished("duplicate"))
        XCTAssertEqual(rig.sent, ["last words"])
        XCTAssertEqual(rig.conversation.draft, "last words", "late engine callbacks cannot overwrite a submitted draft")
        rig.deadline.fire()
    }
    func testSendWithFinalBeforeTheKeyUsesAlreadyCommittedText() async throws {
        let rig = try await composerRig()
        try await startMic(rig)
        rig.engine.send(.heard("early"))
        rig.engine.send(.finished("early final"))
        try key(rig, "\r")
        XCTAssertEqual(rig.sent, ["early final"])
        XCTAssertEqual(rig.engine.finishCount, 0)
    }
    func testSendWithNoFinalUsesLatestVolatileAtControlledDeadline() async throws {
        let rig = try await composerRig()
        try await startMic(rig)
        rig.engine.send(.heard("keep"))
        try key(rig, "\r")
        await wait(until: rig.deadline.continuation != nil)
        rig.engine.send(.heard("keep the last words"))
        XCTAssertTrue(rig.sent.isEmpty)
        rig.deadline.fire()
        await wait(until: !rig.sent.isEmpty)
        XCTAssertEqual(rig.sent, ["keep the last words"])
        rig.engine.send(.finished("too late"))
        try key(rig, "\r")
        XCTAssertEqual(rig.sent.count, 1)
    }
    func testEmptyRecordingOnlyStopsForReturnCommandReturnAndSend() async throws {
        for action in ["return", "command", "send"] {
            let rig = try await composerRig()
            try await startMic(rig)
            if action == "send" { try XCTUnwrap(rig.layout.actions["send"])() }
            else { try key(rig, "\r", action == "command" ? .command : []) }
            XCTAssertTrue(rig.engine.finished)
            rig.engine.send(.finished(""))
            XCTAssertTrue(rig.sent.isEmpty)
            XCTAssertEqual(rig.controller.phase, .idle)
            XCTAssertTrue(rig.decisions.isEmpty)
            rig.deadline.fire()
        }
    }
    func testEmptyRecordingWithNoFinalStopsWithoutSendingAtTheDeadline() async throws {
        let rig = try await composerRig()
        try await startMic(rig)
        try key(rig, "\r")
        await wait(until: rig.deadline.continuation != nil)
        rig.deadline.fire()
        await wait(until: rig.controller.phase == .idle)
        XCTAssertTrue(rig.sent.isEmpty)
        XCTAssertEqual(rig.conversation.draft, "")
    }
    func testEmptyFinalRetainsVolatileAndCancellationDoesNotSend() async throws {
        let rig = try await composerRig()
        try await startMic(rig)
        rig.engine.send(.heard("volatile"))
        try key(rig, "\r")
        rig.engine.send(.heard(""))
        rig.engine.send(.finished(""))
        XCTAssertEqual(rig.sent, ["volatile"])
        let cancelled = try await composerRig()
        try await startMic(cancelled)
        cancelled.engine.send(.heard("cancel me"))
        try key(cancelled, "\r")
        cancelled.controller.cancel()
        cancelled.engine.send(.finished("late"))
        XCTAssertTrue(cancelled.sent.isEmpty)
        cancelled.conversation.draft = "typed later"
        try key(cancelled, "\r")
        XCTAssertEqual(cancelled.sent, ["typed later"], "cancel releases the send reservation")
        rig.deadline.fire(); cancelled.deadline.fire()
    }
    func testEscapeWhenNotRecordingKeepsItsCurrentRoutingAndShiftReturnInsertsNewline() async throws {
        let rig = try await composerRig(draft: "typed")
        XCTAssertFalse(rig.view.keyCommands?.contains { $0.input == UIKeyCommand.inputEscape } == true)
        XCTAssertEqual(rig.view.onKey(.escape), .none)
        XCTAssertEqual(rig.conversation.draft, "typed")
        XCTAssertTrue(rig.sent.isEmpty)
        try await startMic(rig)
        rig.engine.send(.heard("words"))
        try key(rig, "\r", .shift)
        XCTAssertFalse(rig.engine.finished)
        XCTAssertTrue(rig.view.text.contains("\n"))
        rig.controller.cancel()
    }
    func testComposerFrameDoesNotChangeOnStartStreamingOrStopOnPhoneAndPad() async throws {
        for width: CGFloat in [390, 1024] {
            for native in [false, true] {
                let rig = try await composerRig(draft: "Typed\nsecond line\nthird line", width: width, native: native)
                let original = try XCTUnwrap(rig.layout.frames["composer"])
                let field = try XCTUnwrap(rig.layout.frames["composer-field"])
                try await startMic(rig)
                rig.engine.send(.heard(Array(repeating: "words wrapping across many lines", count: 20).joined(separator: " ")))
                await wait(until: rig.view.isScrollEnabled)
                rig.window.layoutIfNeeded()
                XCTAssertEqual(rig.layout.frames["composer"], original, "start/stream at \(width), native \(native)")
                XCTAssertEqual(rig.layout.frames["composer-field"], field)
                try XCTUnwrap(rig.layout.actions["dictation-target"])()
                rig.engine.send(.finished(rig.controller.phase.text))
                await wait(until: rig.controller.phase == .idle)
                // Flush layout after SwiftUI observes the final insertion.
                try await Task.sleep(for: .milliseconds(100))
                rig.window.layoutIfNeeded()
                XCTAssertEqual(rig.layout.frames["composer"], original, "stop at \(width), native \(native)")
                XCTAssertEqual(rig.layout.frames["composer-field"], field)
                XCTAssertTrue(rig.sent.isEmpty)
                await wait(until: !rig.view.hasHeldDictationHeight)
                rig.deadline.fire()
            }
        }
    }
    func testHeightUsesRenderedBoundsAndReleasesAfterStopFocusLossAndDynamicType() async throws {
        let view = ChatComposerTextView(frame: CGRect(x: 0, y: 0, width: 300, height: 87))
        _ = view.composerHeight(fitted: 200, minimum: 44, maximum: 200)
        view.holdDictationHeight()
        XCTAssertEqual(view.composerHeight(fitted: 200, minimum: 44, maximum: 200), 87)
        let rig = try await composerRig(draft: "one\ntwo\nthree")
        try await startMic(rig)
        XCTAssertTrue(rig.view.hasHeldDictationHeight)
        rig.view.delegate?.textViewDidEndEditing?(rig.view)
        XCTAssertFalse(rig.view.hasHeldDictationHeight)
        rig.view.holdDictationHeight()
        rig.host?.traitOverrides.preferredContentSizeCategory = .accessibilityExtraLarge
        await wait(until: !rig.view.hasHeldDictationHeight)
        rig.controller.finishNow()
    }

    // MARK: Controller

    func testTapToStartTapToStopDeliversTheTextOnceWithTheSessionsSpelling() async {
        let engine = ScriptedSpeechEngine(script: "open key bar view", interval: .milliseconds(10))
        let controller = makeController(engine)
        let received = start(controller)
        XCTAssertEqual(controller.phase, .preparing(note: nil))
        XCTAssertEqual(controller.stage, .busy)
        await wait(until: controller.phase.text == "open KeyBarView")
        XCTAssertEqual(controller.phase, .listening(text: "open KeyBarView"), "the session's vocabulary rewrites what was heard")
        XCTAssertEqual(controller.stage, .listening)
        XCTAssertEqual(controller.barState(for: .terminal), .listening)
        XCTAssertEqual(controller.barState(for: .chat("a")), .idle)
        XCTAssertTrue(controller.vocabulary.terms.contains("ios-speech-input"), "built from the session when it started")
        XCTAssertEqual(received.live.last, "open KeyBarView")
        controller.toggle(for: .terminal, deliver: { _ in XCTFail("a second tap stops; it does not start another") })
        XCTAssertTrue(engine.finished)
        await wait(until: controller.phase == .idle)
        XCTAssertEqual(received.delivered, ["open KeyBarView"])
        XCTAssertEqual(controller.stage, .idle)
    }
    func testCancelDeliversNothingAndTakesTheLiveTextOut() async {
        let engine = ManualEngine()
        let controller = makeController(engine)
        let received = start(controller)
        await wait(until: controller.phase == .listening(text: ""))
        engine.send(.heard("rm -rf build"))
        controller.cancel()
        XCTAssertTrue(engine.cancelled)
        XCTAssertEqual(controller.phase, .idle)
        engine.send(.finished("rm -rf build"))
        XCTAssertEqual(received.delivered, [])
        XCTAssertEqual(received.live, ["rm -rf build", ""])
    }
    func testAPermissionRefusedShowsAFailureUntilDismissed() async {
        let engine = ManualEngine()
        engine.startProblem = .microphoneDenied
        let controller = makeController(engine)
        let received = start(controller, .chat("c1"))
        await wait(until: controller.phase == .failed(.microphoneDenied))
        XCTAssertEqual(controller.phase(for: .chat("c1")), .failed(.microphoneDenied))
        XCTAssertEqual(controller.phase(for: .terminal), .idle, "only the mic that was tapped shows it")
        XCTAssertTrue(engine.cancelled)
        controller.dismiss()
        XCTAssertEqual(controller.phase, .idle)
        XCTAssertEqual(received.delivered, [])
    }
    func testTheModelBeingInstalledIsSaid() async {
        let engine = ManualEngine()
        let controller = makeController(engine)
        _ = start(controller)
        engine.send(.note("Installing the speech model…"))
        XCTAssertEqual(controller.phase, .preparing(note: "Installing the speech model…"))
        XCTAssertEqual(TerminalDictationPanel.status(controller.phase), "Installing the speech model…")
    }
    func testAnotherFieldsMicKeepsTheFirstDictation() async {
        let first = ManualEngine(), second = ManualEngine()
        var engines: [ManualEngine] = [first, second]
        let controller = makeController(first)
        controller.makeEngine = { engines.removeFirst() }
        let a = start(controller, .terminal)
        await wait(until: controller.phase == .listening(text: ""))
        first.send(.heard("half a command"))
        let b = start(controller, .chat("c"))
        XCTAssertTrue(first.cancelled)
        XCTAssertEqual(controller.owner, .chat("c"))
        await wait(until: controller.phase == .listening(text: ""))
        second.send(.heard("hello"))
        controller.stop()
        second.send(.finished("hello"))
        XCTAssertEqual(a.delivered, ["half a command"])
        XCTAssertEqual(b.delivered, ["hello"])
    }
    func testSilenceBeforeAndAfterSpeechKeepsListeningUntilATap() async {
        let engine = ManualEngine()
        let controller = makeController(engine)
        let received = start(controller)
        await wait(until: controller.phase == .listening(text: ""))
        try? await Task.sleep(for: .milliseconds(500))
        XCTAssertEqual(controller.phase, .listening(text: ""))
        engine.send(.heard("git status"))
        try? await Task.sleep(for: .milliseconds(500))
        XCTAssertEqual(controller.phase, .listening(text: "git status"))
        XCTAssertFalse(engine.finished)
        XCTAssertEqual(received.delivered, [])
        controller.stop()
        engine.send(.finished("git status"))
        XCTAssertEqual(received.delivered, ["git status"])
    }
    func testEveryFailureKeepsLiveTextAndTearsDownTheEngine() async {
        for problem: DictationProblem in [.interrupted, .failed("dead microphone"), .modelUnavailable] {
            let engine = ManualEngine()
            let controller = makeController(engine)
            let received = start(controller)
            await wait(until: controller.phase == .listening(text: ""))
            engine.send(.heard("keep these words"))
            engine.send(.failed(problem))
            XCTAssertEqual(received.delivered, ["keep these words"])
            XCTAssertEqual(received.live.last, "keep these words")
            XCTAssertEqual(controller.phase, .failed(problem))
            XCTAssertTrue(engine.cancelled)
            engine.send(.finished("late duplicate"))
            XCTAssertEqual(received.delivered, ["keep these words"])
        }
    }
    func testACallKeepsWhatWasSaidBeforeIt() async {
        let engine = ManualEngine()
        let controller = makeController(engine)
        let received = start(controller)
        await wait(until: controller.phase == .listening(text: ""))
        engine.send(.heard("deploy to staging"))
        NotificationCenter.default.post(name: AVAudioSession.interruptionNotification, object: nil,
                                        userInfo: [AVAudioSessionInterruptionTypeKey: AVAudioSession.InterruptionType.began.rawValue])
        await wait(until: !received.delivered.isEmpty)
        XCTAssertEqual(received.delivered, ["deploy to staging"])
        XCTAssertEqual(controller.phase, .failed(.interrupted))
        XCTAssertTrue(engine.cancelled)
    }
    func testLeavingTheAppStopsListeningAndKeepsTheText() async {
        let engine = ManualEngine()
        let controller = makeController(engine)
        _ = start(controller)
        await wait(until: controller.phase == .listening(text: ""))
        engine.send(.heard("ls"))
        NotificationCenter.default.post(name: UIApplication.didEnterBackgroundNotification, object: nil)
        XCTAssertTrue(engine.finished)
        XCTAssertEqual(controller.phase, .finishing(text: "ls"))
    }

    // MARK: The desktop's mic setting

    func testWithTheSettingOffNoDictationStartsAndNoEngineIsMade() {
        var made = 0
        let controller = DictationController()
        controller.makeEngine = { made += 1; return ManualEngine() }
        XCTAssertFalse(controller.isAllowed, "off until the desktop says otherwise")
        controller.toggle(for: .chat("c"), deliver: { _ in XCTFail("nothing is heard") })
        controller.toggle(for: .terminal, deliver: { _ in XCTFail("nothing is heard") })
        XCTAssertEqual(controller.phase, .idle)
        XCTAssertEqual(made, 0, "no engine, so neither the microphone nor speech recognition is asked for")
    }
    func testTurningTheSettingOffCancelsADictationInProgress() async {
        for owner in [DictationOwner.chat("c"), .terminal] {
            let engine = ManualEngine()
            let controller = makeController(engine)
            let received = start(controller, owner)
            await wait(until: controller.phase == .listening(text: ""))
            engine.send(.heard("half a sentence"))
            controller.isAllowed = false
            XCTAssertEqual(controller.phase, .idle, "\(owner)")
            XCTAssertTrue(engine.cancelled)
            XCTAssertEqual(received.delivered, [])
            XCTAssertEqual(received.live.last, "", "what was shown is taken out")
            controller.toggle(for: owner, deliver: { _ in })
            XCTAssertEqual(controller.phase, .idle, "and none starts again while it is off")
        }
    }

    /// Cancelling while the system permission sheet is pending must not open audio when permission resolves later.
    func testLegacyRecognizerDoesNotStartAfterCancellationDuringPermission() async {
        var permission: CheckedContinuation<DictationProblem?, Never>?
        var recognizersMade = 0
        let engine = RecognizerSpeechEngine(authorize: { _ in
            await withCheckedContinuation { permission = $0 }
        }, makeRecognizer: {
            recognizersMade += 1
            return nil
        })
        let start = Task { await engine.start(vocabulary: SpeechVocabulary(terms: [])) }
        await wait(until: permission != nil)
        engine.cancel()
        permission?.resume(returning: nil)
        await start.value
        XCTAssertEqual(recognizersMade, 0, "late permission must not start recognition or microphone capture")
    }
    func testCancelledRecognizerDoesNotAskForPermissionWhenItsQueuedStartRuns() async {
        var asked = 0
        let engine = RecognizerSpeechEngine(authorize: { _ in asked += 1; return .microphoneDenied })
        engine.cancel()
        await engine.start(vocabulary: SpeechVocabulary(terms: []))
        XCTAssertEqual(asked, 0)
    }

    // MARK: Into a text view

    private final class Delegate: NSObject, UITextViewDelegate { var text = ""; func textViewDidChange(_ view: UITextView) { text = view.text } }
    private var windows: [UIWindow] = []
    private func makeTextView(_ text: String, caret: Int) -> (UITextView, Delegate) {
        let view = UITextView(frame: CGRect(x: 0, y: 0, width: 300, height: 100))
        let delegate = Delegate()
        view.delegate = delegate
        view.text = text
        view.selectedRange = NSRange(location: caret, length: 0)
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 200))
        window.addSubview(view)
        windows.append(window)
        return (view, delegate)
    }
    func testWordsAppearAtTheCaretAsTheyAreHeardAndTheFinalTextReplacesThem() {
        let (view, delegate) = makeTextView("Please the tests", caret: 6)
        let insertion = TextInsertion()
        insertion.view = view
        insertion.show("fix")
        XCTAssertEqual(view.text, "Please fix the tests")
        insertion.show("fix all of")
        XCTAssertEqual(view.text, "Please fix all of the tests")
        XCTAssertEqual(delegate.text, view.text, "the binding follows, as if typed")
        insertion.commit("fix all")
        XCTAssertEqual(view.text, "Please fix all the tests")
        XCTAssertEqual(view.selectedRange, NSRange(location: 14, length: 0), "the caret is after the dictated words")
        insertion.show("now")
        insertion.commit("now")
        XCTAssertEqual(view.text, "Please fix all now the tests", "the next dictation goes on from the caret")
    }
    func testTypingInsideTheDictatedSpanTakesOverUntilCommit() {
        let (view, delegate) = makeTextView("Typed", caret: 5)
        let insertion = TextInsertion()
        insertion.view = view
        insertion.show("some words")
        view.selectedRange = NSRange(location: 8, length: 0)
        view.insertText("EDIT")
        let edited = view.text, caret = view.selectedRange
        insertion.show("some different words")
        insertion.commit("some final words")
        XCTAssertEqual(view.text, edited)
        XCTAssertEqual(delegate.text, edited)
        XCTAssertEqual(view.selectedRange, caret)
        insertion.commit("new dictation")
        XCTAssertTrue(view.text.contains("new dictation"), "a new dictation can insert normally")
    }
    func testTypingTwoKeystrokesBetweenPartialsKeepsCaretAndOrder() {
        let (view, _) = makeTextView("Typed", caret: 5)
        let insertion = TextInsertion()
        insertion.view = view
        insertion.show("words")
        view.insertText(" a")
        insertion.show("words more")
        XCTAssertEqual(view.selectedRange.location, (view.text as NSString).length)
        view.insertText("b")
        insertion.commit("words more")
        XCTAssertEqual(view.text, "Typed words more ab")
        XCTAssertEqual(view.selectedRange.location, (view.text as NSString).length)
    }
    func testCancellingTakesTheWordsOutAgain() {
        let (view, delegate) = makeTextView("Hello", caret: 5)
        let insertion = TextInsertion()
        insertion.view = view
        insertion.show("world how")
        XCTAssertEqual(view.text, "Hello world how")
        insertion.discard()
        XCTAssertEqual(view.text, "Hello")
        XCTAssertEqual(delegate.text, "Hello")
        insertion.discard()
        XCTAssertEqual(view.text, "Hello", "nothing to take out")
    }
    func testCancellingLiveInsertionLetsTheNextDictationUseANewCaret() {
        let (view, _) = makeTextView("Hello world", caret: 5)
        let insertion = TextInsertion()
        insertion.view = view
        insertion.show("first")
        insertion.show("") // The controller's cancellation callback.
        XCTAssertEqual(view.text, "Hello world")
        view.selectedRange = NSRange(location: 11, length: 0)
        insertion.commit("again")
        XCTAssertEqual(view.text, "Hello world again")
    }
    func testCancellingLiveInsertionPreservesTypingThatChangedTheDraft() {
        let (view, _) = makeTextView("Hello", caret: 5)
        let insertion = TextInsertion()
        insertion.view = view
        insertion.show("world")
        view.text = "Hello edited"
        insertion.show("")
        XCTAssertEqual(view.text, "Hello edited")
    }

    func testADictationIntoTheComposerEndToEnd() async {
        let (view, delegate) = makeTextView("", caret: 0)
        let insertion = TextInsertion()
        insertion.view = view
        let engine = ScriptedSpeechEngine(script: "rename session vocabulary", interval: .milliseconds(10))
        let controller = makeController(engine)
        controller.sessionSources = { SpeechVocabularySources(text: "SessionVocabulary") }
        controller.toggle(for: .chat("c"), live: { insertion.show($0) }, deliver: { insertion.commit($0) })
        await wait(until: controller.phase.text == "rename SessionVocabulary")
        XCTAssertEqual(view.text, "rename SessionVocabulary", "live, while listening")
        controller.stop()
        await wait(until: controller.phase == .idle)
        XCTAssertEqual(delegate.text, "rename SessionVocabulary")
    }

    // MARK: The key bar's mic

    func testTheMicSitsRightBeforeHideAndStartsADictation() throws {
        let bar = KeyBarView()
        bar.style = style(mic: true)
        bar.frame = CGRect(x: 0, y: 0, width: 402, height: KeyBarView.height)
        bar.setNeedsLayout(); bar.layoutIfNeeded()
        let mic = try XCTUnwrap(bar.buttons[.dictate]), hide = try XCTUnwrap(bar.buttons[.hide])
        let micFrame = mic.convert(mic.bounds, to: bar), hideFrame = hide.convert(hide.bounds, to: bar)
        XCTAssertEqual(micFrame.maxX, hideFrame.minX - 1, accuracy: 0.5, "mic, the divider, then Hide")
        XCTAssertFalse(mic.isDescendant(of: bar.scrollView), "fixed like Hide: never scrolled away")
        XCTAssertLessThanOrEqual(bar.scrollView.frame.maxX, micFrame.minX + 0.5)
        XCTAssertEqual(mic.accessibilityLabel, "Dictate")
        var actions: [KeyBarView.Action] = []
        bar.onAction = { actions.append($0) }
        mic.sendActions(for: .touchUpInside)
        XCTAssertEqual(actions, [.dictate])
    }
    /// Off (the default, and a desktop that predates the setting): no mic, and the scrolling row runs up to the divider before Hide. The
    /// setting flips it in place, both ways, in both looks and on glass, with the layout unambiguous.
    func testTheMicComesAndGoesWithTheSettingAndTheRowClosesUp() throws {
        for native in [false, true] {
            let bar = KeyBarView()
            bar.style = style(mic: false, native: native)
            let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 402, height: 200))
            window.addSubview(bar)
            windows.append(window)
            bar.frame = CGRect(x: 0, y: 100, width: 402, height: KeyBarView.height)
            func layout() { bar.setNeedsLayout(); bar.layoutIfNeeded() }
            layout()
            let mic = try XCTUnwrap(bar.buttons[.dictate]), hide = try XCTUnwrap(bar.buttons[.hide])
            func frame(_ view: UIView) -> CGRect { view.convert(view.bounds, to: bar) }
            XCTAssertFalse(bar.showsMic)
            XCTAssertTrue(mic.isHidden)
            XCTAssertEqual(bar.scrollView.frame.maxX, frame(hide).minX - 1, accuracy: 0.5, "native \(native): the row reaches the divider before Hide")
            XCTAssertFalse(bar.scrollView.hasAmbiguousLayout || hide.hasAmbiguousLayout)
            let offWidth = bar.scrollView.frame.width
            bar.style = style(mic: true, native: native)
            layout()
            XCTAssertTrue(bar.showsMic)
            XCTAssertFalse(mic.isHidden)
            XCTAssertEqual(frame(mic).maxX, frame(hide).minX - 1, accuracy: 0.5, "native \(native): mic, the divider, then Hide")
            XCTAssertEqual(bar.scrollView.frame.maxX, frame(mic).minX, accuracy: 0.5)
            XCTAssertEqual(bar.scrollView.frame.width, offWidth - frame(mic).width, accuracy: 0.5, "the mic takes its room from the row")
            XCTAssertFalse(bar.scrollView.hasAmbiguousLayout || mic.hasAmbiguousLayout)
            bar.style = style(mic: false, native: native)
            layout()
            XCTAssertTrue(mic.isHidden)
            XCTAssertEqual(bar.scrollView.frame.width, offWidth, accuracy: 0.5, "and gives it back")
        }
    }
    func testTheMicShowsListening() throws {
        let bar = KeyBarView()
        bar.style = style(mic: true)
        let mic = try XCTUnwrap(bar.buttons[.dictate])
        bar.setDictation(.listening)
        XCTAssertEqual(mic.accessibilityLabel, "Stop dictation")
        XCTAssertEqual(mic.accessibilityValue, "Listening")
        XCTAssertEqual(mic.configuration?.baseForegroundColor, bar.style.accentUI)
        bar.setDictation(.idle)
        XCTAssertEqual(mic.accessibilityLabel, "Dictate")
        XCTAssertNil(mic.accessibilityValue)
    }
    func testTheBarsMicReachesTheTerminalScreen() {
        let view = KeyCaptureView(frame: .zero)
        var dictated = 0
        view.onDictate = { dictated += 1 }
        view.bar.tapped(.dictate)
        XCTAssertEqual(dictated, 1)
    }
}
