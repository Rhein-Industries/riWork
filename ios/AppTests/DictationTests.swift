import XCTest
import UIKit
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
        func start(vocabulary: SpeechVocabulary) async {
            started = true
            if let startProblem { onEvent?(.failed(startProblem)) } else { onEvent?(.ready) }
        }
        func finish() { finished = true }
        func cancel() { cancelled = true }
        func send(_ event: SpeechEngineEvent) { onEvent?(event) }
    }
    private final class Received { var live: [String] = []; var delivered: [String] = [] }

    private func makeController(_ engine: any SpeechEngine, silence: Duration = .seconds(60)) -> DictationController {
        let controller = DictationController(silenceAfterSpeech: silence, silenceBeforeSpeech: .seconds(60))
        controller.makeEngine = { engine }
        controller.sessionSources = { SpeechVocabularySources(names: ["ios-speech-input"], text: "KeyBarView") }
        return controller
    }
    private func start(_ controller: DictationController, _ owner: DictationOwner = .terminal) -> Received {
        let received = Received()
        controller.toggle(for: owner, live: { received.live.append($0) }, deliver: { received.delivered.append($0) })
        return received
    }
    private func wait(until condition: @autoclosure () -> Bool, timeout: TimeInterval = 3) async {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition(), Date() < deadline { try? await Task.sleep(for: .milliseconds(20)) }
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
    func testAnotherFieldsMicCancelsTheFirstDictation() async {
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
        XCTAssertEqual(a.delivered, [])
        XCTAssertEqual(b.delivered, ["hello"])
    }
    func testSilenceAfterSpeechStopsByItself() async {
        let engine = ScriptedSpeechEngine(script: "git status", interval: .milliseconds(10))
        let controller = makeController(engine, silence: .milliseconds(300))
        let received = start(controller)
        await wait(until: received.delivered == ["git status"], timeout: 5)
        XCTAssertTrue(engine.finished, "the pause after speech stopped it, without a tap")
        XCTAssertEqual(received.delivered, ["git status"])
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
    func testTheMicShowsListening() throws {
        let bar = KeyBarView()
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
