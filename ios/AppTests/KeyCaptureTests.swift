import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// The UIKit input surface: traits, event mapping, the key bar, hardware key commands and focus.
@MainActor final class KeyCaptureTests: XCTestCase {
    private var windows: [UIWindow] = []
    private func makeView(accept: Bool = true) -> (KeyCaptureView, Received) {
        let received = Received()
        let view = KeyCaptureView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        view.onItems = { received.items.append(contentsOf: $0); return accept }
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 480))
        window.rootViewController = UIViewController()
        window.rootViewController?.view.addSubview(view)
        window.makeKeyAndVisible()
        windows.append(window)
        return (view, received)
    }
    private final class Received { var items: [KeyItem] = [] }

    func testTraitsKeepEveryAutomaticTextFeatureOff() {
        let (view, _) = makeView()
        XCTAssertEqual(view.autocorrectionType, .no)
        XCTAssertEqual(view.spellCheckingType, .no)
        XCTAssertEqual(view.autocapitalizationType, .none)
        XCTAssertEqual(view.smartQuotesType, .no, "a typed \" must not become “")
        XCTAssertEqual(view.smartDashesType, .no, "-- must not become —")
        XCTAssertEqual(view.smartInsertDeleteType, .no)
        XCTAssertEqual(view.inlinePredictionType, .no)
        XCTAssertEqual(view.mathExpressionCompletionType, .no)
        XCTAssertEqual(view.writingToolsBehavior, .none)
        XCTAssertEqual(view.keyboardType, .asciiCapable)
        XCTAssertEqual(view.returnKeyType, .default, "labelled “return”")
        XCTAssertFalse(view.isSecureTextEntry)
        XCTAssertTrue(view.hasText, "or the delete key goes silent")
    }
    func testTypedDictatedAndPastedTextMapsToItems() {
        let (view, got) = makeView()
        view.insertText("ls -la")
        view.insertText("\n")
        view.insertText("\t")
        view.deleteBackward()
        view.insertText("hello world, this was dictated. ")
        XCTAssertEqual(got.items, [.text("ls -la"), .key(.enter), .key(.tab), .key(.backspace), .text("hello world, this was dictated. ")])
        got.items = []
        view.pasteText("echo one\r\necho two\n\u{7}\u{1b}x\u{2028}y")
        XCTAssertEqual(got.items, [.text("echo one"), .key(.enter), .text("echo two"), .key(.enter), .text("xy")], "multi-line paste splits on newlines; other control characters are dropped")
        got.items = []
        view.insertText("a\nb")
        XCTAssertEqual(got.items, [.text("a"), .key(.enter), .text("b")], "a newline inside inserted text becomes Enter")
    }
    func testStickyCtrlArmsFromTheBarAndTheNextLetterBecomesAControlKey() {
        let (view, got) = makeView()
        let ctrl = view.bar.buttons[.control]!
        XCTAssertFalse(ctrl.isSelected)
        view.bar.tapped(.control)
        XCTAssertTrue(view.mapper.controlArmed)
        XCTAssertTrue(ctrl.isSelected, "the bar shows Ctrl is armed")
        XCTAssertEqual(ctrl.accessibilityValue, "armed")
        view.insertText("c")
        XCTAssertEqual(got.items, [.key(.control("c"))])
        XCTAssertFalse(view.mapper.controlArmed, "one letter only")
        XCTAssertFalse(ctrl.isSelected)
        view.insertText("c")
        XCTAssertEqual(got.items.last, .text("c"))
        view.bar.tapped(.control); view.bar.tapped(.control)
        XCTAssertFalse(view.mapper.controlArmed, "tapping again disarms")
        view.bar.tapped(.control)
        view.bar.tapped(.key(.escape))
        XCTAssertEqual(got.items.last, .key(.escape))
        XCTAssertFalse(view.mapper.controlArmed, "any key press consumes it")
    }
    func testKeyBarHasEscTabCtrlArrowsPasteAndHide() {
        let (view, got) = makeView()
        for key in [TerminalKey.escape, .tab, .left, .up, .down, .right] { XCTAssertNotNil(view.bar.buttons[.key(key)], "\(key)") }
        XCTAssertNotNil(view.bar.buttons[.control]); XCTAssertNotNil(view.bar.buttons[.paste]); XCTAssertNotNil(view.bar.buttons[.hide])
        XCTAssertEqual(view.bar.buttons[.hide]?.accessibilityLabel, "Hide keyboard")
        XCTAssertEqual(view.bar.intrinsicContentSize.height, KeyBarView.height)
        for key in [TerminalKey.escape, .tab, .left, .up, .down, .right] { view.bar.buttons[.key(key)]?.sendActions(for: .touchUpInside) }
        XCTAssertEqual(got.items, [.key(.escape), .key(.tab), .key(.left), .key(.up), .key(.down), .key(.right)])
        XCTAssertTrue(view.inputAccessoryView === view.bar)
    }
    func testHoldingAnArrowRepeatsAndReleaseDoesNotAddAnExtraKey() async {
        let (view, got) = makeView()
        let right = view.bar.buttons[.key(.right)]!
        right.sendActions(for: .touchDown)
        try? await Task.sleep(for: .milliseconds(700))
        let held = got.items.count
        XCTAssertGreaterThanOrEqual(held, 3, "repeats while held")
        right.sendActions(for: .touchUpInside)
        XCTAssertEqual(got.items.count, held, "the release after repeating sends nothing more")
        try? await Task.sleep(for: .milliseconds(200))
        XCTAssertEqual(got.items.count, held, "and the repeat has stopped")
        right.sendActions(for: .touchDown); right.sendActions(for: .touchUpInside)
        XCTAssertEqual(got.items.count, held + 1, "a plain tap sends once")
    }
    func testHardwareKeyboardArrowsEscapeTabAndCtrlLettersTakePriority() throws {
        let (view, got) = makeView()
        let commands = try XCTUnwrap(view.keyCommands)
        XCTAssertTrue(commands.allSatisfy { $0.wantsPriorityOverSystemBehavior })
        func fire(_ input: String, _ flags: UIKeyModifierFlags = []) throws {
            let command = try XCTUnwrap(commands.first { $0.input == input && $0.modifierFlags == flags }, "no command for \(input.debugDescription)")
            view.keyCommandFired(command)
        }
        try fire(UIKeyCommand.inputUpArrow); try fire(UIKeyCommand.inputDownArrow); try fire(UIKeyCommand.inputLeftArrow); try fire(UIKeyCommand.inputRightArrow)
        try fire(UIKeyCommand.inputEscape); try fire("\t"); try fire("\t", .shift)
        try fire(UIKeyCommand.inputHome); try fire(UIKeyCommand.inputEnd); try fire(UIKeyCommand.inputPageUp); try fire(UIKeyCommand.inputPageDown)
        try fire("c", .control); try fire("z", .control)
        XCTAssertEqual(got.items, [.key(.up), .key(.down), .key(.left), .key(.right), .key(.escape), .key(.tab), .key(.backTab),
                                   .key(.home), .key(.end), .key(.pageUp), .key(.pageDown), .key(.control("c")), .key(.control("z"))])
        XCTAssertEqual(commands.filter { $0.modifierFlags == .control && ($0.input ?? "").unicodeScalars.allSatisfy { (97...122).contains($0.value) } }.count, 26, "Ctrl-a … Ctrl-z")
        try fire("[", .control)
        XCTAssertEqual(got.items.last, .key(.escape), "Ctrl-[ is Escape")
    }
    func testARefusedKeystrokeDoesNotBreakTheView() {
        let (view, got) = makeView(accept: false)
        view.insertText("x")
        view.deleteBackward()
        XCTAssertEqual(got.items, [.text("x"), .key(.backspace)], "the view reports; the model decides")
    }
    func testFocusBringsUpTheKeyboardAndHideResigns() {
        let (view, _) = makeView()
        let focus = KeyFocus()
        focus.view = view
        view.onActiveChange = { focus.isActive = $0 }
        focus.focus()
        XCTAssertTrue(view.isFirstResponder)
        XCTAssertTrue(focus.isActive)
        view.bar.tapped(.control)
        view.bar.tapped(.hide)
        XCTAssertFalse(view.isFirstResponder)
        XCTAssertFalse(focus.isActive)
        XCTAssertFalse(view.mapper.controlArmed, "a hidden keyboard does not keep Ctrl armed")
        view.isEnabled = false
        focus.focus()
        XCTAssertFalse(view.isFirstResponder, "a disabled view stays unfocused")
    }
    func testRepresentableWiresTheFocusObject() {
        let focus = KeyFocus()
        let host = UIHostingController(rootView: KeyCapture(focus: focus, isEnabled: true, label: "Terminal input", onItems: { _ in true }).frame(width: 1, height: 1))
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 480))
        window.rootViewController = host
        window.makeKeyAndVisible()
        windows.append(window)
        host.view.layoutIfNeeded()
        XCTAssertNotNil(focus.view)
        XCTAssertEqual(focus.view?.accessibilityLabel, "Terminal input")
    }
}
