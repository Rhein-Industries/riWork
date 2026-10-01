import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// The scrolling key bar: its keys, its ends, and what its buttons send.
@MainActor final class KeyBarTests: XCTestCase {
    private var windows: [UIWindow] = []
    private final class Received { var items: [KeyItem] = []; var edits = 0 }
    private func makeView() -> (KeyCaptureView, Received) {
        let received = Received()
        let view = KeyCaptureView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        view.onItems = { received.items.append(contentsOf: $0); return true }
        view.onEditHotkeys = { received.edits += 1 }
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 402, height: 874))
        window.rootViewController = UIViewController()
        window.rootViewController?.view.addSubview(view)
        window.makeKeyAndVisible()
        windows.append(window)
        return (view, received)
    }
    /// A bar laid out at a phone's width, not in a window: no keyboard is involved.
    private func makeBar(width: CGFloat = 402) -> KeyBarView {
        let bar = KeyBarView()
        bar.frame = CGRect(x: 0, y: 0, width: width, height: KeyBarView.height)
        return bar
    }
    private func layout(_ bar: KeyBarView) { bar.setNeedsLayout(); bar.layoutIfNeeded(); bar.scrollView.layoutIfNeeded() }
    /// A button's frame in the bar's own coordinates.
    private func frame(_ action: KeyBarView.Action, in bar: KeyBarView) throws -> CGRect {
        let button = try XCTUnwrap(bar.buttons[action], "\(action)")
        return button.convert(button.bounds, to: bar)
    }
    private let phone = KeyBarInsets(left: 0, bottom: 34, right: 0)

    // MARK: Keys

    func testTheBarHasTheSpecialKeysSymbolsAndHotkeys() {
        let bar = makeBar()
        let named: [TerminalKey] = [.escape, .tab, .backTab, .left, .up, .down, .right, .home, .end, .pageUp, .pageDown, .delete, .backspace, .enter]
        for key in named { XCTAssertNotNil(bar.buttons[.key(key)], key.name) }
        for action in [KeyBarView.Action.control, .alt, .paste, .hide, .editHotkeys, .palette] { XCTAssertNotNil(bar.buttons[action], "\(action)") }
        for symbol in ["|", "/", "\\", "~", "-", "_", "`", "*", "&", "$", ">", "<", "{", "}", "[", "]", ";", ":", "'", "\""] {
            XCTAssertNotNil(bar.buttons[.text(symbol)], symbol)
        }
        XCTAssertEqual(KeyBarView.symbols.count, 20)
        for hotkey in Hotkey.builtIn { XCTAssertNotNil(bar.buttons[.hotkey(hotkey.id)], hotkey.label) }
        XCTAssertEqual(bar.buttons[.control]?.accessibilityLabel, "Control")
        XCTAssertEqual(bar.buttons[.text("|")]?.accessibilityLabel, "Pipe")
        XCTAssertEqual(bar.buttons[.hide]?.accessibilityLabel, "Hide keyboard")
        XCTAssertEqual(bar.buttons[.editHotkeys]?.accessibilityLabel, "Add or edit hotkeys")
        XCTAssertEqual(bar.buttons[.palette]?.accessibilityLabel, "Hotkey menu", "the key bar opens the hotkey menu too")
    }
    func testTheOrderIsSpecialKeysThenHotkeysThenSymbolsAndTheEditorLast() throws {
        let bar = makeBar()
        layout(bar)
        let order: [KeyBarView.Action] = [.key(.escape), .key(.tab), .control, .alt, .key(.left), .key(.right), .paste, .palette, .hotkey(Hotkey.builtIn[0].id),
                                          .hotkey(Hotkey.builtIn.last!.id), .text("|"), .text("\""), .editHotkeys]
        let xs = try order.map { try frame($0, in: bar).minX }
        XCTAssertEqual(xs, xs.sorted(), "left to right in this order")
        XCTAssertLessThan(try frame(.paste, in: bar).minX, try frame(.hotkey(Hotkey.builtIn[0].id), in: bar).minX)
    }
    func testAddedHotkeysFollowTheBuiltInOnesAndComeAndGoWithTheList() throws {
        let bar = makeBar()
        let first = Hotkey(label: "Clear", steps: [.text("/clear"), .key(.enter)]), second = Hotkey(label: "Bail", steps: [.key(.control("c")), .text("exit"), .key(.enter)])
        bar.hotkeys = [first, second]
        layout(bar)
        let esc = try frame(.hotkey(Hotkey.builtIn.last!.id), in: bar), one = try frame(.hotkey(first.id), in: bar), two = try frame(.hotkey(second.id), in: bar)
        let firstSymbol = try frame(.text("|"), in: bar)
        XCTAssertLessThan(esc.maxX, one.minX + 0.5)
        XCTAssertLessThan(one.maxX, two.minX + 0.5)
        XCTAssertLessThan(two.maxX, firstSymbol.minX, "before the symbols")
        XCTAssertEqual(bar.buttons[.hotkey(first.id)]?.accessibilityLabel, "Hotkey Clear")
        bar.hotkeys = [second]
        XCTAssertNil(bar.buttons[.hotkey(first.id)])
        XCTAssertNotNil(bar.buttons[.hotkey(second.id)])
        bar.hotkeys = []
        XCTAssertNil(bar.buttons[.hotkey(second.id)])
        XCTAssertEqual(bar.buttons.count, makeBar().buttons.count, "back to the plain bar")
    }
    func testTheRowScrollsAndHideStaysWhereItIs() throws {
        let bar = makeBar()
        layout(bar)
        let scroll = bar.scrollView
        XCTAssertGreaterThan(scroll.contentSize.width, 3 * scroll.bounds.width, "far more keys than fit")
        XCTAssertTrue(scroll.alwaysBounceHorizontal)
        XCTAssertFalse(scroll.showsHorizontalScrollIndicator)
        let hideBefore = try frame(.hide, in: bar)
        XCTAssertEqual(hideBefore.maxX, 402, accuracy: 0.5, "Hide sits at the trailing end")
        let editBefore = try frame(.editHotkeys, in: bar)
        XCTAssertGreaterThan(editBefore.minX, 402, "the hotkey editor is scrolled out of view at first")
        scroll.contentOffset = CGPoint(x: scroll.contentSize.width - scroll.bounds.width, y: 0)
        layout(bar)
        XCTAssertEqual(try frame(.hide, in: bar), hideBefore, "Hide does not scroll")
        XCTAssertLessThan(try frame(.editHotkeys, in: bar).maxX, hideBefore.minX, "and the last key comes to rest beside it")
        XCTAssertLessThan(try frame(.key(.escape), in: bar).maxX, 0, "while the first is gone")
    }
    func testAKeyStillReactsToATouchAtOnceButADragScrollsInstead() {
        let bar = makeBar()
        XCTAssertFalse(bar.scrollView.delaysContentTouches, "arrows repeat from touch-down, so no touch is held back")
        XCTAssertTrue(bar.scrollView.touchesShouldCancel(in: bar.buttons[.key(.left)]!), "a finger that moves off a key scrolls the row")
    }

    // MARK: Interface size

    func testTheBarScalesWithTheInterfaceSizeAndStaysTheSameAtTheStandardSize() throws {
        let bar = makeBar()
        layout(bar)
        let standardEscape = try frame(.key(.escape), in: bar)
        XCTAssertEqual(bar.barHeight, 44)
        XCTAssertEqual(bar.intrinsicContentSize.height, 44)
        let standardWidth = bar.scrollView.contentSize.width
        bar.style = DesktopStyle(.builtIn, scale: 1.3)
        bar.frame.size.height = bar.barHeight
        layout(bar)
        XCTAssertEqual(bar.barHeight, 57)
        XCTAssertEqual(bar.intrinsicContentSize.height, 57, "the bar tells iOS its new height")
        XCTAssertGreaterThan(try frame(.key(.escape), in: bar).width, standardEscape.width)
        XCTAssertEqual(try frame(.key(.escape), in: bar).height, 56, accuracy: 1, "keys fill the strip below its 1 pt rule")
        XCTAssertGreaterThan(bar.scrollView.contentSize.width, standardWidth)
        XCTAssertGreaterThanOrEqual(try frame(.hide, in: bar).width, 57 - 1, "Hide keeps a target as large as the bar")
        let big = try XCTUnwrap(bar.buttons[.key(.escape)]?.configuration?.attributedTitle)
        XCTAssertEqual((big.runs.first?.uiKit.font)?.pointSize ?? 0, 13 * 1.3, accuracy: 0.01)
        bar.style = DesktopStyle(.builtIn, scale: 0.8)
        bar.frame.size.height = bar.barHeight
        layout(bar)
        XCTAssertEqual(bar.intrinsicContentSize.height, 35)
        XCTAssertLessThan(bar.scrollView.contentSize.width, standardWidth)
        bar.style = DesktopStyle(.builtIn)
        bar.frame.size.height = bar.barHeight
        layout(bar)
        XCTAssertEqual(bar.intrinsicContentSize.height, 44)
        XCTAssertEqual(try frame(.key(.escape), in: bar).width, standardEscape.width, accuracy: 0.5, "back at 100 % everything is where it was")
        XCTAssertEqual(bar.scrollView.contentSize.width, standardWidth, accuracy: 0.5)
    }

    // MARK: Ends of the row and where the bar is

    func testAboveTheSoftwareKeyboardTheRowStartsAtTheEdge() throws {
        let bar = makeBar()
        bar.applyPlacement(safeArea: phone, position: .aboveKeyboard)
        layout(bar)
        XCTAssertEqual(bar.position, .aboveKeyboard)
        XCTAssertEqual(try frame(.key(.escape), in: bar).minX, 0, accuracy: 0.5)
        XCTAssertEqual(try frame(.hide, in: bar).maxX, 402, accuracy: 0.5)
        XCTAssertEqual(bar.intrinsicContentSize.height, 44)
    }
    func testAloneAtTheBottomTheFirstAndLastKeysClearTheRoundedCorners() throws {
        let bar = makeBar()
        bar.applyPlacement(safeArea: phone, position: .screenBottom)
        layout(bar)
        let clearance = KeyBarGeometry.cornerClearance(safeArea: phone)
        XCTAssertEqual(bar.position, .screenBottom)
        XCTAssertTrue(KeyBarGeometry.clearance.contains(clearance))
        XCTAssertEqual(bar.scrollView.contentOffset.x, 0)
        XCTAssertEqual(try frame(.key(.escape), in: bar).minX, CGFloat(clearance), accuracy: 0.5, "scrolled fully left, the first key is clear of the corner")
        XCTAssertEqual(try frame(.hide, in: bar).maxX, CGFloat(402 - clearance), accuracy: 0.5, "and Hide, the last one, is clear of the other corner")
        XCTAssertEqual(bar.intrinsicContentSize.height, 44, "the bar does not grow or move to do it")
        bar.scrollView.contentOffset.x = bar.scrollView.contentSize.width - bar.scrollView.bounds.width
        layout(bar)
        XCTAssertLessThanOrEqual(try frame(.editHotkeys, in: bar).maxX, try frame(.hide, in: bar).minX, "scrolled fully right, the last scrolling key ends beside Hide")
    }
    func testMovingFromTheKeyboardToTheBottomRepadsWithoutChangingTheBarsSize() throws {
        let bar = makeBar()
        bar.applyPlacement(safeArea: phone, position: .aboveKeyboard)
        layout(bar)
        let size = bar.intrinsicContentSize
        bar.applyPlacement(safeArea: phone, position: .screenBottom)
        layout(bar)
        XCTAssertEqual(bar.intrinsicContentSize, size)
        XCTAssertEqual(bar.bounds.height, 44)
        bar.applyPlacement(safeArea: phone, position: .aboveKeyboard)
        layout(bar)
        XCTAssertEqual(try frame(.key(.escape), in: bar).minX, 0, accuracy: 0.5)
    }
    func testAloneAtTheBottomInFocusModeTheBarIsTheSameStrip() throws {
        let bar = makeBar()
        bar.applyPlacement(safeArea: phone, position: .screenBottom)
        layout(bar)
        XCTAssertNotEqual(bar.backgroundColor, .clear, "the strip has its own background, as above the keyboard")
        XCTAssertNil(bar.subviews.first { $0.layer.cornerRadius > 0 }, "no rounded pill")
        let clearance = KeyBarGeometry.cornerClearance(safeArea: phone)
        XCTAssertGreaterThanOrEqual(try frame(.key(.escape), in: bar).minX, CGFloat(clearance) - 0.5, "the keys are padded clear of the display corners")
    }
    func testTheBarWearsTheStyleItIsGiven() {
        let bar = makeBar()
        let light = DesktopStyle(DesktopTheme.resolve(DesktopAppearance(updatedAt: 1, dark: false,
            palette: DesktopPalette(bg: RGB(0xfbf1c7), panel: RGB(0xf4ebc2), panelActive: RGB(0xede3bc), divider: RGB(0xd5ccb6), cyan: RGB(0x427b58), magenta: RGB(0x8f3f71), gold: RGB(0x9d5015), text: RGB(0x3c3836), muted: RGB(0x756f5e)))))
        bar.style = light
        XCTAssertEqual(bar.backgroundColor, light.panelUI)
        XCTAssertEqual(bar.buttons[.key(.escape)]?.configuration?.baseForegroundColor, light.textUI)
        XCTAssertEqual(bar.buttons[.hotkey(Hotkey.builtIn[0].id)]?.configuration?.baseForegroundColor, light.magentaUI, "hotkeys carry the secondary accent")
        XCTAssertEqual(bar.buttons[.editHotkeys]?.configuration?.baseForegroundColor, light.mutedUI)
        bar.setArmed(control: true, alt: false)
        XCTAssertEqual(bar.buttons[.control]?.configuration?.baseForegroundColor, light.accentUI)
        bar.style = .builtIn
        bar.style = light
        XCTAssertEqual(bar.buttons[.control]?.configuration?.baseForegroundColor, light.accentUI, "a restyle keeps armed keys armed")
    }

    // MARK: What the buttons send

    func testTheNewSpecialKeysSendTheirNamedKeys() {
        let (view, got) = makeView()
        for key in [TerminalKey.backTab, .home, .end, .pageUp, .pageDown, .delete, .backspace, .enter] { view.bar.tapped(.key(key)) }
        XCTAssertEqual(got.items, [.key(.backTab), .key(.home), .key(.end), .key(.pageUp), .key(.pageDown), .key(.delete), .key(.backspace), .key(.enter)])
    }
    func testSymbolKeysSendTheirCharacterAsText() {
        let (view, got) = makeView()
        for symbol in KeyBarView.symbols { view.bar.tapped(.text(symbol)) }
        XCTAssertEqual(got.items, KeyBarView.symbols.map { .text($0) })
        for item in got.items { XCTAssertNoThrow(try item.validate(), "\(item)") }
    }
    func testAltThenAKeySendsEscapeThenTheKeyAndShowsItIsArmed() {
        let (view, got) = makeView()
        let alt = view.bar.buttons[.alt]!
        view.bar.tapped(.alt)
        XCTAssertTrue(view.mapper.altArmed)
        XCTAssertTrue(alt.isSelected)
        XCTAssertEqual(alt.accessibilityValue, "armed")
        view.bar.tapped(.key(.left))
        XCTAssertEqual(got.items, [.key(.escape), .key(.left)])
        XCTAssertFalse(alt.isSelected, "one key only")
        view.bar.tapped(.alt)
        view.insertText("b")
        XCTAssertEqual(Array(got.items.suffix(2)), [.key(.escape), .text("b")], "typed text gets Escape in front too")
        view.bar.tapped(.alt); view.bar.tapped(.text("."))
        XCTAssertEqual(Array(got.items.suffix(2)), [.key(.escape), .text(".")], "and so does a symbol key")
    }
    func testAltAndCtrlTogetherAndTheKeyboardGoingAway() {
        let (view, got) = makeView()
        view.bar.tapped(.control); view.bar.tapped(.alt)
        view.insertText("h")
        XCTAssertEqual(got.items, [.key(.escape), .key(.control("h"))])
        view.bar.tapped(.alt); view.bar.tapped(.control)
        XCTAssertTrue(view.becomeFirstResponder())
        _ = view.resignFirstResponder()
        XCTAssertFalse(view.mapper.altArmed)
        XCTAssertFalse(view.mapper.controlArmed)
        XCTAssertFalse(view.bar.buttons[.alt]!.isSelected)
    }
    func testBuiltInHotkeyButtonsSendTheirKeys() {
        let (view, got) = makeView()
        for hotkey in Hotkey.builtIn { view.bar.tapped(.hotkey(hotkey.id)) }
        XCTAssertEqual(got.items, [.key(.control("c")), .key(.control("d")), .key(.control("z")), .key(.control("l")), .key(.control("r")),
                                   .key(.control("a")), .key(.control("e")), .key(.control("u")), .key(.control("w")), .key(.escape), .key(.escape)])
    }
    func testAnAddedHotkeySendsItsStepsInOrderAndIgnoresArmedModifiers() {
        let (view, got) = makeView()
        let clear = Hotkey(label: "Clear", steps: [.text("/clear"), .key(.enter)])
        let bail = Hotkey(label: "Bail", steps: [.key(.control("c")), .text("exit"), .key(.enter)])
        view.hotkeys = [clear, bail]
        view.bar.tapped(.hotkey(clear.id))
        view.bar.tapped(.alt); view.bar.tapped(.control)
        view.bar.tapped(.hotkey(bail.id))
        XCTAssertEqual(got.items, [.text("/clear"), .key(.enter), .key(.control("c")), .text("exit"), .key(.enter)], "no Escape in front of a hotkey")
        XCTAssertFalse(view.mapper.altArmed)
        XCTAssertFalse(view.mapper.controlArmed)
        view.hotkeys = []
        view.bar.tapped(.hotkey(clear.id))
        XCTAssertEqual(got.items.count, 5, "a removed hotkey sends nothing")
    }
    func testThePlusOpensTheEditorAndSendsNothing() {
        let (view, got) = makeView()
        view.bar.tapped(.editHotkeys)
        XCTAssertEqual(got.edits, 1)
        XCTAssertEqual(got.items, [])
    }
    func testHoldingBackspaceRepeatsLikeTheArrowsDo() async {
        let (view, got) = makeView()
        let backspace = view.bar.buttons[.key(.backspace)]!
        backspace.sendActions(for: .touchDown)
        try? await Task.sleep(for: .milliseconds(650))
        backspace.sendActions(for: .touchUpInside)
        XCTAssertGreaterThanOrEqual(got.items.count, 3)
        XCTAssertTrue(got.items.allSatisfy { $0 == .key(.backspace) })
        let held = got.items.count
        try? await Task.sleep(for: .milliseconds(200))
        XCTAssertEqual(got.items.count, held, "and stops on release")
    }
}
