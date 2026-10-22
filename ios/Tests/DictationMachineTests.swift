import XCTest
@testable import RiWorkCore

final class DictationMachineTests: XCTestCase {
    private func run(_ events: [DictationMachine.Event]) -> (DictationMachine, [DictationMachine.Effect]) {
        var machine = DictationMachine()
        let effects = events.map { machine.handle($0) }
        return (machine, effects)
    }

    func testTapToStartTapToStopDeliversTheFinalTextOnce() {
        let (machine, effects) = run([.start, .ready, .heard("git"), .heard("git status"), .stop, .heard("git status."), .finished("git status.")])
        XCTAssertEqual(effects, [.beginEngine, .none, .none, .none, .finishEngine, .none, .deliver("git status.")])
        XCTAssertEqual(machine.phase, .idle)
    }
    func testThePhasesCarryTheTextHeardSoFar() {
        var machine = DictationMachine()
        _ = machine.handle(.start); XCTAssertEqual(machine.phase, .preparing(note: nil)); XCTAssertTrue(machine.phase.isActive)
        _ = machine.handle(.note("Installing the speech model…")); XCTAssertEqual(machine.phase, .preparing(note: "Installing the speech model…"))
        _ = machine.handle(.ready); XCTAssertEqual(machine.phase, .listening(text: ""))
        _ = machine.handle(.heard("hello")); XCTAssertEqual(machine.phase.text, "hello")
        _ = machine.handle(.stop); XCTAssertEqual(machine.phase, .finishing(text: "hello"))
    }
    func testCancelDropsEverythingFromAnyActivePhase() {
        for prefix: [DictationMachine.Event] in [[.start], [.start, .ready, .heard("abc")], [.start, .ready, .heard("abc"), .stop]] {
            let (machine, effects) = run(prefix + [.cancel, .finished("abc")])
            XCTAssertEqual(effects.suffix(2), [.cancelEngine, .none], "nothing is delivered after a cancel")
            XCTAssertEqual(machine.phase, .idle)
        }
    }
    func testStoppingBeforeTheMicrophoneIsOpenCancels() {
        XCTAssertEqual(run([.start, .stop]).1.last, .cancelEngine)
    }
    func testNothingHeardDeliversNothing() {
        let (machine, effects) = run([.start, .ready, .stop, .finished("  ")])
        XCTAssertEqual(effects.last, DictationMachine.Effect.none)
        XCTAssertEqual(machine.phase, .idle)
    }
    func testEmptyFinalOrPartialCannotEraseVolatileWordsWhileStopping() {
        let (machine, effects) = run([.start, .ready, .heard("keep the last words"), .stop, .heard(""), .finished("")])
        XCTAssertEqual(effects.last, .deliver("keep the last words"))
        XCTAssertEqual(machine.phase, .idle)
    }
    func testTheRecognizerMayFinishByItself() {
        XCTAssertEqual(run([.start, .ready, .heard("ls"), .finished("ls")]).1.last, .deliver("ls"))
    }
    func testAPermissionRefusedIsAFailureUntilSeen() {
        var (machine, effects) = run([.start, .failed(.microphoneDenied)])
        XCTAssertEqual(effects.last, .cancelEngine)
        XCTAssertEqual(machine.phase, .failed(.microphoneDenied))
        XCTAssertFalse(machine.phase.isActive)
        XCTAssertTrue(DictationProblem.microphoneDenied.opensSettings)
        XCTAssertEqual(machine.handle(.stop), .none, "the mic shows the failure; stop does nothing")
        _ = machine.handle(.dismiss)
        XCTAssertEqual(machine.phase, .idle)
        effects = [machine.handle(.start)]
        XCTAssertEqual(effects, [.beginEngine])
    }
    func testStartingAgainAfterAFailureIsAllowed() {
        var (machine, _) = run([.start, .failed(.modelUnavailable)])
        XCTAssertEqual(machine.handle(.start), .beginEngine)
    }
    func testEveryFailureKeepsWhatWasSaidEvenWhileFinishing() {
        for problem: DictationProblem in [.interrupted, .failed("dead mic"), .modelUnavailable] {
            for ending: [DictationMachine.Event] in [[], [.stop]] {
                let (machine, effects) = run([.start, .ready, .heard("deploy to staging")] + ending + [.failed(problem)])
                XCTAssertEqual(effects.last, .deliver("deploy to staging"))
                XCTAssertEqual(machine.phase, .failed(problem))
                XCTAssertEqual(run([.start, .ready, .failed(problem)]).1.last, .cancelEngine)
            }
        }
    }
    func testRollingRequestsReplaceOnlyTheirOwnTailAndIgnoreLateResults() {
        var transcript = RollingDictationTranscript()
        XCTAssertTrue(transcript.update("open key", generation: 0))
        transcript.update("open KeyBarView", generation: 0)
        transcript.roll()
        XCTAssertFalse(transcript.update("stale final", generation: 0))
        transcript.update("and test", generation: 1)
        XCTAssertEqual(transcript.text, "open KeyBarView and test")
        transcript.update("and test again", generation: 1)
        transcript.update("", generation: 1)
        transcript.roll()
        transcript.update("again", generation: 2)
        XCTAssertEqual(transcript.text, "open KeyBarView and test again again", "repeated words from new audio must survive")
    }
    func testARecognizerResetCommitsTheOldUtteranceAndKeepsCorrectingTheNewOne() {
        var transcript = RollingDictationTranscript()
        transcript.update("run all of the tests before we deploy", generation: 0)
        transcript.update("Then", generation: 0)
        XCTAssertEqual(transcript.text, "run all of the tests before we deploy then")
        transcript.update("Then check logs", generation: 0)
        XCTAssertEqual(transcript.text, "run all of the tests before we deploy then check logs")
        transcript.update("", generation: 0)
        XCTAssertEqual(transcript.text, "run all of the tests before we deploy then check logs")
        var overlapping = RollingDictationTranscript()
        overlapping.update("run all of the tests before deployment", generation: 0)
        overlapping.update("Deployment", generation: 0)
        overlapping.update("Deployment and logs", generation: 0)
        XCTAssertEqual(overlapping.text, "run all of the tests before deployment and logs", "reset overlap must not duplicate words")
        var correction = RollingDictationTranscript()
        correction.update("open the file and change all its callers", generation: 0)
        correction.update("open the file", generation: 0)
        XCTAssertEqual(correction.text, "open the file", "a shorter correction that continues the hypothesis replaces it")
    }
    func testDeadMicRecoversOnceThenAllowsThreeSecondsForActualBuffers() {
        let watch = AudioBufferWatchdog()
        XCTAssertEqual(watch.poll(at: 100), .none)
        watch.start(at: 100)
        XCTAssertEqual(watch.poll(at: 102.99), .none)
        XCTAssertEqual(watch.poll(at: 103), .recover)
        watch.beginRecovery(at: 103.15) // repeated configuration notifications must not extend the grace
        XCTAssertEqual(watch.poll(at: 105.99), .none)
        XCTAssertEqual(watch.poll(at: 106), .lost)
        watch.received(at: 110) // silence from the recovered engine proves the microphone is alive
        XCTAssertEqual(watch.poll(at: 112.99), .none)
        XCTAssertEqual(watch.poll(at: 113), .recover)
        watch.received(at: 114)
        XCTAssertEqual(watch.poll(at: 116.99), .none)
        XCTAssertEqual(watch.poll(at: 117), .recover, "a later independent outage gets its own attempt")
        let changedRoute = AudioBufferWatchdog()
        changedRoute.start(at: 100)
        changedRoute.beginRecovery(at: 101)
        changedRoute.beginRecovery(at: 102)
        XCTAssertEqual(changedRoute.poll(at: 103.99), .none)
        XCTAssertEqual(changedRoute.poll(at: 104), .lost)
    }
    func testEventsOutOfTurnAreIgnored() {
        let (machine, effects) = run([.ready, .heard("x"), .stop, .finished("x"), .cancel, .dismiss])
        XCTAssertEqual(effects, Array(repeating: .none, count: 6))
        XCTAssertEqual(machine.phase, .idle)
        XCTAssertEqual(run([.start, .start]).1, [.beginEngine, .none], "a second start while one runs does nothing")
    }
    func testEveryProblemSaysSomething() {
        for problem: DictationProblem in [.microphoneDenied, .recognitionDenied, .unsupported, .modelUnavailable, .interrupted, .failed("x")] {
            XCTAssertFalse(problem.message.isEmpty)
        }
    }
}
