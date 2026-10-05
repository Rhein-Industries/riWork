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
        view.bar.tapped(.control)
        view.bar.tapped(.key(.escape))
        XCTAssertEqual(got.items.last, .key(.escape))
        XCTAssertFalse(view.mapper.controlArmed, "any key press consumes it")
    }
    func testADoubleTapLocksAModifierAndTheBarShowsLockedApartFromArmed() throws {
        let (view, got) = makeView()
        let ctrl = try XCTUnwrap(view.bar.buttons[.control])
        view.bar.tapped(.control)
        let armedBackground = ctrl.configuration?.background.backgroundColor
        XCTAssertEqual(armedBackground, view.bar.style.activeUI)
        view.bar.tapped(.control)
        XCTAssertEqual(view.mapper.control, .locked, "two quick taps lock")
        XCTAssertEqual(ctrl.accessibilityValue, "locked")
        XCTAssertEqual(ctrl.configuration?.background.backgroundColor, view.bar.style.accentUI, "locked is filled with the accent")
        XCTAssertEqual(ctrl.configuration?.baseForegroundColor, view.bar.style.backgroundUI)
        XCTAssertNotEqual(ctrl.configuration?.background.backgroundColor, armedBackground)
        view.insertText("a"); view.insertText("e")
        XCTAssertEqual(got.items, [.key(.control("a")), .key(.control("e"))])
        XCTAssertEqual(ctrl.accessibilityCustomActions?.map(\.name), ["Release"])
        view.bar.tapped(.control)
        XCTAssertEqual(view.mapper.control, .off)
        XCTAssertFalse(ctrl.isSelected)
        XCTAssertEqual(ctrl.configuration?.background.backgroundColor, .clear)
        XCTAssertEqual(ctrl.accessibilityCustomActions?.map(\.name), ["Lock"], "VoiceOver locks with an action")
        view.bar.onAction?(.latch(.alt, .locked))
        XCTAssertEqual(view.mapper.alt, .locked)
    }
    func testEveryModifierCombinationReachesSoftwareKeysAndBarKeys() {
        let (view, got) = makeView()
        view.bar.tapped(.control); view.bar.tapped(.alt)
        view.insertText("r")
        view.bar.tapped(.control); view.bar.tapped(.shift)
        view.bar.tapped(.key(.right))
        view.bar.tapped(.alt)
        view.bar.tapped(.key(.home))
        view.bar.tapped(.shift)
        view.bar.tapped(.key(.tab))
        view.bar.tapped(.control)
        view.bar.tapped(.text("["))
        view.bar.tapped(.alt)
        view.deleteBackward()
        XCTAssertEqual(got.items, [.key(.escape), .key(.control("r")), .key(.escape), .text("[1;6C"), .key(.escape), .text("[1;3H"), .key(.backTab),
                                   .key(.escape), .key(.escape), .key(.backspace)])
        XCTAssertEqual(view.mapper.armed, [], "each was armed for one key")
    }
    func testDictationWhileAModifierIsArmedGoesThroughAndTheModifierWaits() {
        let (view, got) = makeView()
        view.bar.tapped(.control)
        view.insertText("hello there")
        view.pasteText("cd ~")
        XCTAssertEqual(got.items, [.text("hello there"), .text("cd ~")])
        XCTAssertTrue(view.bar.buttons[.control]!.isSelected, "still armed")
        view.insertText("c")
        XCTAssertEqual(got.items.last, .key(.control("c")))
    }
    func testAHeldArrowKeepsTheModifiersItStartedWith() async {
        let (view, got) = makeView()
        view.bar.tapped(.control)
        let right = view.bar.buttons[.key(.right)]!
        right.sendActions(for: .touchDown)
        try? await Task.sleep(for: .milliseconds(650))
        right.sendActions(for: .touchUpInside)
        XCTAssertGreaterThanOrEqual(got.items.count, 6, "at least three repeats")
        XCTAssertEqual(got.items.count % 2, 0)
        for pair in stride(from: 0, to: got.items.count, by: 2) { XCTAssertEqual(Array(got.items[pair...pair + 1]), [.key(.escape), .text("[1;5C")]) }
        XCTAssertFalse(view.mapper.controlArmed, "used up by the key it was armed for")
        view.bar.tapped(.key(.right))
        XCTAssertEqual(got.items.last, .key(.right))
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
    func testHardwareModifierCombinationsAreSentAndAddToTheBarsModifiers() throws {
        let (view, got) = makeView()
        let commands = try XCTUnwrap(view.keyCommands)
        func fire(_ input: String, _ flags: UIKeyModifierFlags) throws {
            let command = try XCTUnwrap(commands.first { $0.input == input && $0.modifierFlags == flags }, "no command for \(input.debugDescription) \(flags.rawValue)")
            view.keyCommandFired(command)
        }
        let arrows = [UIKeyCommand.inputUpArrow, UIKeyCommand.inputDownArrow, UIKeyCommand.inputLeftArrow, UIKeyCommand.inputRightArrow,
                      UIKeyCommand.inputHome, UIKeyCommand.inputEnd, UIKeyCommand.inputPageUp, UIKeyCommand.inputPageDown, UIKeyCommand.inputEscape, "\t", "\r"]
        let combinations: [UIKeyModifierFlags] = [.shift, .control, .alternate, [.shift, .control], [.shift, .alternate], [.control, .alternate], [.shift, .control, .alternate]]
        for input in arrows { for flags in combinations { XCTAssertTrue(commands.contains { $0.input == input && $0.modifierFlags == flags }, "\(input.debugDescription) \(flags.rawValue)") } }
        try fire(UIKeyCommand.inputUpArrow, .control)
        try fire(UIKeyCommand.inputLeftArrow, .alternate)
        try fire(UIKeyCommand.inputRightArrow, [.shift, .control])
        try fire(UIKeyCommand.inputEnd, .shift)
        try fire(UIKeyCommand.inputPageDown, .control)
        try fire("\r", .alternate)
        try fire("\t", .shift)
        try fire("b", [.control, .alternate])
        try fire("a", [.control, .shift])
        XCTAssertEqual(got.items, [.key(.escape), .text("[1;5A"), .key(.escape), .text("[1;3D"), .key(.escape), .text("[1;6C"), .key(.escape), .text("[1;2F"),
                                   .key(.escape), .text("[6;5~"), .key(.escape), .key(.enter), .key(.backTab), .key(.escape), .key(.control("b")), .key(.control("a"))])
        got.items = []
        // The bar's armed modifiers add to what the keyboard holds.
        view.bar.tapped(.alt)
        try fire(UIKeyCommand.inputRightArrow, .control)
        view.bar.tapped(.control)
        try fire(UIKeyCommand.inputDownArrow, [])
        view.bar.tapped(.control)
        view.insertText("x")
        XCTAssertEqual(got.items, [.key(.escape), .text("[1;7C"), .key(.escape), .text("[1;5B"), .key(.control("x"))])
        XCTAssertEqual(view.mapper.armed, [])
    }
    func testForwardDeleteAndModifiedBackspaceFromAHardwareKeyboard() {
        let (view, got) = makeView()
        func press(_ code: Int, _ modifiers: ChordModifiers = []) -> (down: Bool, up: Bool) {
            let down = view.handle(KeyEventRecord(phase: .down, keyCode: code, modifiers: modifiers))
            let up = view.handle(KeyEventRecord(phase: .up, keyCode: code, modifiers: modifiers))
            return (down, up)
        }
        XCTAssertTrue(press(HIDKey.deleteForward) == (true, true), "used, release too")
        XCTAssertTrue(press(HIDKey.deleteForward, .control) == (true, true))
        XCTAssertTrue(press(HIDKey.backspace, .alt) == (true, true))
        XCTAssertTrue(press(HIDKey.backspace, .control) == (true, true))
        XCTAssertTrue(press(HIDKey.backspace) == (false, false), "plain Backspace stays with the text system, which repeats it")
        XCTAssertTrue(press(HIDKey.backspace, .shift) == (false, false))
        XCTAssertTrue(press(HIDKey.backspace, .command) == (false, false), "Command is the system's")
        XCTAssertEqual(got.items, [.key(.delete), .key(.escape), .text("[3;5~"), .key(.escape), .key(.backspace), .key(.control("h"))])
        view.bar.tapped(.alt)
        _ = press(HIDKey.deleteForward)
        XCTAssertEqual(Array(got.items.suffix(2)), [.key(.escape), .text("[3;3~")], "with the bar's Alt")
    }
    func testOptionAloneStillTypesTheLayoutsCharacters() throws {
        // Option+L is @ on a German Mac layout: Option is never made Meta on its own, it reaches the text system untouched.
        let (view, _) = makeView()
        let commands = try XCTUnwrap(view.keyCommands)
        XCTAssertFalse(commands.contains { $0.modifierFlags == .alternate && ($0.input ?? "").count == 1 && $0.input != "\t" && $0.input != "\r" })
    }
    func testTheKeyboardAlwaysSeesTextBeforeTheCaretSoAHeldDeleteRepeats() throws {
        let (view, got) = makeView()
        let caret = try XCTUnwrap(view.selectedTextRange)
        XCTAssertTrue(caret.isEmpty)
        let before = try XCTUnwrap(view.position(from: caret.start, offset: -1), "no text before the caret: the delete key would not repeat")
        XCTAssertEqual(view.text(in: try XCTUnwrap(view.textRange(from: before, to: caret.start))), " ")
        XCTAssertNil(view.markedTextRange)
        for _ in 0..<3 {
            // What the keyboard does on each repeat: select the character before the caret, then delete.
            view.selectedTextRange = view.textRange(from: before, to: caret.start)
            view.deleteBackward()
        }
        view.insertText("x")
        XCTAssertEqual(got.items, [.key(.backspace), .key(.backspace), .key(.backspace), .text("x")], "one backspace per repeat")
        XCTAssertEqual(view.text(in: try XCTUnwrap(view.textRange(from: view.beginningOfDocument, to: view.endOfDocument))), " ", "the document never changes")
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
