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
    func testAnInterruptionKeepsWhatWasSaid() {
        let (machine, effects) = run([.start, .ready, .heard("deploy to staging"), .failed(.interrupted)])
        XCTAssertEqual(effects.last, .deliver("deploy to staging"))
        XCTAssertEqual(machine.phase, .failed(.interrupted))
        XCTAssertEqual(run([.start, .ready, .failed(.interrupted)]).1.last, .cancelEngine)
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
