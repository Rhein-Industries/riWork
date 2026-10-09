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

final class BottomBarGeometryTests: XCTestCase {
    func testGlassRowIsClippedToTheCapsule() {
        let clip = BottomBarGeometry.rowClip(barWidth: 402, barHeight: 44, glass: true)
        XCTAssertEqual(clip, .init(x: 0, y: 2, width: 402, height: 40, cornerRadius: 20), "edge to edge, as the composer's field")
        XCTAssertGreaterThan(BottomBarGeometry.keysInset(glass: true), clip.x, "the first key rests inside the capsule")
        XCTAssertEqual(BottomBarGeometry.rowClip(barWidth: 402, barHeight: 44, glass: false), .init(x: 0, y: 0, width: 402, height: 44, cornerRadius: 0))
    }

    func testOnlyAnEndWithKeysPastItFades() {
        let atStart = BottomBarGeometry.fade(offset: 0, contentWidth: 1200, visibleWidth: 300)
        XCTAssertEqual(atStart.leading, 0); XCTAssertEqual(atStart.trailing, 1)
        let middle = BottomBarGeometry.fade(offset: 8, contentWidth: 1200, visibleWidth: 300)
        XCTAssertEqual(middle.leading, 0.5, accuracy: 0.001); XCTAssertEqual(middle.trailing, 1)
        let atEnd = BottomBarGeometry.fade(offset: 900, contentWidth: 1200, visibleWidth: 300)
        XCTAssertEqual(atEnd.leading, 1); XCTAssertEqual(atEnd.trailing, 0)
        let bounced = BottomBarGeometry.fade(offset: -20, contentWidth: 200, visibleWidth: 300)
        XCTAssertEqual(bounced.leading, 0); XCTAssertEqual(bounced.trailing, 0, "a row that fits never fades")
    }

    func testBounceDoesNotInventHiddenKeys() {
        for offset in [-120.0, -20.0, 0.0, 20.0, 120.0] {
            let fade = BottomBarGeometry.fade(offset: offset, contentWidth: 200, visibleWidth: 300)
            XCTAssertEqual(fade.leading, 0)
            XCTAssertEqual(fade.trailing, 0)
        }
        let left = BottomBarGeometry.fade(offset: -20, contentWidth: 305, visibleWidth: 300)
        XCTAssertEqual(left.leading, 0)
        XCTAssertEqual(left.trailing, 5.0 / 16.0, accuracy: 0.001)
        let right = BottomBarGeometry.fade(offset: 25, contentWidth: 305, visibleWidth: 300)
        XCTAssertEqual(right.leading, 5.0 / 16.0, accuracy: 0.001)
        XCTAssertEqual(right.trailing, 0)
    }

    func testTargetsGrowWithTheInterfaceButNeverShrinkBelow44() {
        XCTAssertEqual(BottomBarGeometry.target(scale: 1), 44)
        XCTAssertEqual(BottomBarGeometry.target(scale: 0.8), 44, "80 % would be 35 points: clamped")
        XCTAssertEqual(BottomBarGeometry.target(scale: 0.5), 44)
        XCTAssertEqual(BottomBarGeometry.target(scale: 1.3), InterfaceScale.scaled(44, by: 1.3))
        XCTAssertGreaterThan(BottomBarGeometry.target(scale: 1.3), 44)
        for scale in stride(from: 0.5, through: 2.0, by: 0.05) {
            XCTAssertGreaterThanOrEqual(BottomBarGeometry.target(scale: scale), 44)
            XCTAssertGreaterThanOrEqual(KeyBarGeometry.height(scale: scale), 44)
        }
    }

    func testComposerLinesUpWithTheKeyBar() {
        for glass in [true, false] {
            let insets = BottomBarGeometry.composerInsets(glass: glass)
            XCTAssertGreaterThanOrEqual(insets.horizontal, 0)
            XCTAssertEqual(insets.horizontal, 0, "no gutter: the field spans the safe area's width")
            XCTAssertEqual(insets.horizontal, BottomBarGeometry.rowClip(barWidth: 402, barHeight: 44, glass: glass).x, "the key bar's edges")
            XCTAssertEqual(insets.bottom, BottomBarGeometry.capsuleEndInset)
        }
        XCTAssertEqual(BottomBarGeometry.minimumTarget, 44)
    }
}
