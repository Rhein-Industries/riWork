import XCTest
@testable import RiWorkCore

/// The rule that decides whether a chat's composer stands up for the keyboard (`KeyboardDocking.isDocked`), from one notification's frame.
final class KeyboardDockingTests: XCTestCase {
    private func docked(_ hiding: Bool = false, x: Double = 0, y: Double, width: Double = 820, height: Double, screen: (Double, Double) = (820, 1180)) -> Bool {
        KeyboardDocking.isDocked(hiding: hiding, x: x, y: y, width: width, height: height, screenWidth: screen.0, screenHeight: screen.1)
    }
    func testOnlyADockedKeyboardOnScreenCounts() {
        XCTAssertTrue(docked(y: 830, height: 350), "docked at the bottom")
        XCTAssertTrue(docked(y: 470, width: 1180, height: 350, screen: (1180, 820)), "docked in landscape")
        XCTAssertFalse(docked(true, y: 830, height: 350), "a hide is never docked, whatever its frame")
        XCTAssertFalse(docked(y: 1180, height: 350), "below the screen: on its way out")
        XCTAssertFalse(docked(y: 1100, height: 350), "mostly below the screen: less than the threshold on it")
        XCTAssertFalse(docked(y: 1125, height: 55), "a hardware keyboard's shortcut bar")
        XCTAssertFalse(docked(x: 40, y: 500, width: 320, height: 260), "floating: does not reach the bottom")
        XCTAssertFalse(docked(y: 600, height: 350), "undocked: raised from the bottom")
        XCTAssertFalse(docked(y: 0, width: 0, height: 0), "the empty frame of a split or floating keyboard")
        XCTAssertFalse(docked(x: 900, y: 830, height: 350), "off the screen to the side")
    }
}
