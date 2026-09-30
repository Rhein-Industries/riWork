import XCTest
@testable import RiWorkCore

/// The key row's end padding: portrait iPhone, hardware keyboard (bar alone at the bottom) and software keyboard (bar above it).
final class KeyBarGeometryTests: XCTestCase {
    // Safe areas as iOS reports them on an iPhone 17 (also 15/16 Pro Max class) in portrait: home indicator below, nothing at the sides.
    private let portrait = KeyBarInsets(left: 0, bottom: 34, right: 0)

    func testAboveTheSoftwareKeyboardTheRowKeepsTheSideSafeAreaOnly() {
        XCTAssertEqual(KeyBarGeometry.padding(safeArea: portrait, position: .aboveKeyboard), .zero,
                       "the keyboard below already lines up with the display; no corner is near")
    }
    func testAloneAtTheBottomTheEndsClearTheRoundedCorners() {
        let padding = KeyBarGeometry.padding(safeArea: portrait, position: .screenBottom)
        XCTAssertEqual(padding.left, padding.right)
        XCTAssertTrue(KeyBarGeometry.clearance.contains(padding.left), "20-28 pt beyond a zero side safe area, got \(padding.left)")
        XCTAssertEqual(padding.left, 34 / 0.62 / 2, accuracy: 0.001, "half the corner radius, estimated from the home-indicator inset")
    }
    func testTheBarItselfDoesNotGrowOrMove() {
        // Only the ends are padded: there is no vertical component at all, and the bar is the same height in both places.
        XCTAssertEqual(KeyBarGeometry.height, 44)
        XCTAssertEqual(KeyBarGeometry.padding(safeArea: portrait, position: .screenBottom), KeyBarPadding(left: 27.419_354_838_709_68, right: 27.419_354_838_709_68))
    }
    func testASideSafeAreaIsKeptAndGrowsToTheClearanceWhereItIsSmaller() {
        // Not used on a phone in portrait; the function still behaves for whatever the system reports.
        let wide = KeyBarInsets(left: 60, bottom: 34, right: 60)
        XCTAssertEqual(KeyBarGeometry.padding(safeArea: wide, position: .aboveKeyboard), KeyBarPadding(left: 60, right: 60))
        XCTAssertEqual(KeyBarGeometry.padding(safeArea: wide, position: .screenBottom), KeyBarPadding(left: 60, right: 60), "already past the corner")
        let narrow = KeyBarInsets(left: 8, bottom: 34, right: 0)
        let padding = KeyBarGeometry.padding(safeArea: narrow, position: .screenBottom)
        XCTAssertEqual(padding.left, KeyBarGeometry.cornerClearance(safeArea: narrow))
        XCTAssertEqual(padding.right, KeyBarGeometry.cornerClearance(safeArea: narrow))
    }
    func testASquareDisplayNeedsNoExtraPadding() {
        // iPhone SE: no safe area apart from the status bar.
        for position in [KeyBarPosition.aboveKeyboard, .screenBottom] {
            XCTAssertEqual(KeyBarGeometry.padding(safeArea: .zero, position: position), .zero)
        }
        XCTAssertEqual(KeyBarGeometry.cornerClearance(safeArea: .zero), 0)
    }
    func testAnySafeAreaWithAHomeIndicatorGetsBetweenTwentyAndTwentyEight() {
        for bottom in stride(from: 1.0, through: 80, by: 3) {
            let value = KeyBarGeometry.cornerClearance(safeArea: KeyBarInsets(left: 0, bottom: bottom, right: 0))
            XCTAssertTrue(KeyBarGeometry.clearance.contains(value), "bottom \(bottom): \(value)")
        }
        XCTAssertEqual(KeyBarGeometry.padding(safeArea: KeyBarInsets(left: 0, bottom: 20, right: 0), position: .screenBottom), KeyBarPadding(left: 20, right: 20))
    }
    func testNegativeInsetsNeverShrinkThePadding() {
        let padding = KeyBarGeometry.padding(safeArea: KeyBarInsets(left: -5, bottom: -3, right: -1), position: .screenBottom)
        XCTAssertGreaterThanOrEqual(padding.left, 0)
        XCTAssertGreaterThanOrEqual(padding.right, 0)
    }
    func testPositionIsWhereTheBarEndsOnTheScreen() {
        // Hardware keyboard: the "keyboard" is the accessory alone, 44 pt tall at the very bottom of an 874 pt screen.
        XCTAssertEqual(KeyBarGeometry.position(barMaxY: 874, screenHeight: 874), .screenBottom)
        XCTAssertEqual(KeyBarGeometry.position(barMaxY: 873.5, screenHeight: 874), .screenBottom, "within a point")
        // Software keyboard: 318 pt of keys below a 44 pt bar.
        XCTAssertEqual(KeyBarGeometry.position(barMaxY: 556, screenHeight: 874), .aboveKeyboard)
        XCTAssertEqual(KeyBarGeometry.position(barMaxY: 872, screenHeight: 874), .aboveKeyboard)
        XCTAssertEqual(KeyBarGeometry.position(barMaxY: 860, screenHeight: 874, tolerance: 35), .screenBottom, "a bar a little above the edge, inside the safe area")
        XCTAssertEqual(KeyBarGeometry.position(barMaxY: 0, screenHeight: 0), .aboveKeyboard, "no window yet")
    }
}
