import XCTest
@testable import RiWorkCore

final class KeyboardFocusTests: XCTestCase {
    private func decide(_ setting: KeyboardFocusSetting, hardware: Bool, obscured: Bool = false, focused: Bool = false, dismissed: Bool = false, canType: Bool = true) -> KeyboardFocusDecision {
        KeyboardFocusPolicy.decide(setting: setting, context: KeyboardFocusContext(hardwareKeyboard: hardware, obscured: obscured, alreadyFocused: focused, dismissedHere: dismissed, canType: canType))
    }

    // MARK: Setting

    func testTheDefaultIsWithAHardwareKeyboardAndUnknownValuesFallBackToIt() {
        XCTAssertEqual(KeyboardFocusSetting.default, .hardwareKeyboard)
        XCTAssertEqual(KeyboardFocusSetting(stored: nil), .hardwareKeyboard)
        XCTAssertEqual(KeyboardFocusSetting(stored: "garbage"), .hardwareKeyboard)
        for setting in KeyboardFocusSetting.allCases { XCTAssertEqual(KeyboardFocusSetting(stored: setting.rawValue), setting) }
        XCTAssertEqual(KeyboardFocusSetting.allCases.map(\.title), ["Always", "With a hardware keyboard", "Never"])
    }

    // MARK: Policy

    func testAlwaysFocusesWithOrWithoutAHardwareKeyboard() {
        XCTAssertEqual(decide(.always, hardware: false), .focus)
        XCTAssertEqual(decide(.always, hardware: true), .focus)
    }
    func testTheHardwareSettingFocusesOnlyWhenOneIsAttached() {
        XCTAssertEqual(decide(.hardwareKeyboard, hardware: true), .focus)
        XCTAssertEqual(decide(.hardwareKeyboard, hardware: false), .skip(.noHardwareKeyboard), "focusing would pop the software keyboard up")
    }
    func testNeverNeverFocuses() {
        XCTAssertEqual(decide(.never, hardware: true), .skip(.setting))
        XCTAssertEqual(decide(.never, hardware: false), .skip(.setting))
    }
    func testASheetAnAlertOrAnotherFieldIsNeverStolenFrom() {
        for setting in KeyboardFocusSetting.allCases where setting != .never {
            XCTAssertEqual(decide(setting, hardware: true, obscured: true), .skip(.obscured), "\(setting)")
        }
    }
    func testAKeyboardThePersonPutAwayStaysAwayInThatShell() {
        XCTAssertEqual(decide(.always, hardware: true, dismissed: true), .skip(.dismissedByUser))
        XCTAssertEqual(decide(.hardwareKeyboard, hardware: true, dismissed: true), .skip(.dismissedByUser))
    }
    func testNothingIsDoneWhenTheTerminalAlreadyHasTheKeyboardOrCannotTake() {
        XCTAssertEqual(decide(.always, hardware: true, focused: true), .skip(.alreadyFocused))
        XCTAssertEqual(decide(.always, hardware: true, canType: false), .skip(.cannotType), "line composer, dead shell")
    }
    func testWhatIsOnScreenBeatsWhatWasChosen() {
        XCTAssertEqual(decide(.always, hardware: true, obscured: true, dismissed: true), .skip(.obscured))
        XCTAssertEqual(decide(.never, hardware: true, obscured: true), .skip(.setting))
        XCTAssertTrue(decide(.always, hardware: false).shouldFocus)
        XCTAssertFalse(decide(.never, hardware: false).shouldFocus)
    }

    // MARK: Tracker

    func testAShellBecomingReadyIsAMomentToFocusAndStayingReadyIsNot() {
        var tracker = ShellFocusTracker()
        XCTAssertFalse(tracker.readiness(nil))
        XCTAssertTrue(tracker.readiness("a"), "the first live screen")
        XCTAssertFalse(tracker.readiness("a"), "the same shell going on")
        XCTAssertTrue(tracker.readiness("b"), "another shell chosen")
        XCTAssertTrue(tracker.readiness("a"), "and back")
    }
    func testAReconnectOrAReturnFromTheBackgroundIsANewMoment() {
        var tracker = ShellFocusTracker()
        XCTAssertTrue(tracker.readiness("a"))
        XCTAssertFalse(tracker.readiness(nil), "the link dropped, or the app left: not ready")
        XCTAssertTrue(tracker.readiness("a"), "and ready again")
    }
    func testHidingTheKeyboardIsRememberedPerShellUntilThePersonAsksForItBack() {
        var tracker = ShellFocusTracker()
        tracker.userDismissed("a")
        XCTAssertTrue(tracker.isDismissed("a"))
        XCTAssertFalse(tracker.isDismissed("b"), "another shell is unaffected")
        XCTAssertTrue(tracker.readiness("a"))
        XCTAssertTrue(tracker.isDismissed("a"), "a new ready moment does not forget it")
        tracker.userFocused("a")
        XCTAssertFalse(tracker.isDismissed("a"))
        tracker.userDismissed(nil); tracker.userFocused(nil)
        XCTAssertFalse(tracker.isDismissed(nil))
    }
    func testAShellThatReplacedAClosedOneWaitsForATap() {
        var tracker = ShellFocusTracker()
        tracker.shellReplacedWithoutTap("next")
        XCTAssertTrue(tracker.isDismissed("next"), "keystrokes meant for one shell must not flow into one nobody picked")
        tracker.userFocused("next")
        XCTAssertFalse(tracker.isDismissed("next"))
    }
    func testTheMemoryIsBounded() {
        var tracker = ShellFocusTracker()
        for index in 0..<(ShellFocusTracker.maxRemembered + 10) { tracker.userDismissed("s\(index)") }
        XCTAssertFalse(tracker.isDismissed("s0"), "the oldest are forgotten")
        XCTAssertTrue(tracker.isDismissed("s\(ShellFocusTracker.maxRemembered + 9)"))
        tracker.userDismissed("s\(ShellFocusTracker.maxRemembered + 9)")
        XCTAssertTrue(tracker.isDismissed("s\(ShellFocusTracker.maxRemembered + 9)"), "twice is the same as once")
    }
}
