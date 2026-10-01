import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// The hotkey menu and the keyboard shortcuts on the capture view: opened with ⌘K, driven by the keyboard alone, and shortcuts that
/// send a hotkey without opening it.
@MainActor final class HotkeyMenuTests: XCTestCase {
    private var windows: [UIWindow] = []
    private final class Received { var items: [KeyItem] = []; var events: [KeyEventRecord] = []; var edited: [String] = [] }
    private func makeView(hotkeys: [Hotkey] = [], palette: [KeyChord] = []) -> (KeyCaptureView, Received) {
        let received = Received()
        let view = KeyCaptureView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        view.onItems = { received.items.append(contentsOf: $0); return true }
        view.onKeyEvent = { received.events.append($0) }
        view.onEditHotkeys = { received.edited.append("list") }
        view.onNewHotkey = { received.edited.append("new") }
        view.onEditHotkey = { received.edited.append("edit \($0.label)") }
        view.hotkeys = hotkeys
        view.shortcutSettings = ShortcutSettings(paletteChords: palette)
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 480))
        window.rootViewController = UIViewController()
        window.rootViewController?.view.addSubview(view)
        window.makeKeyAndVisible()
        windows.append(window)
        return (view, received)
    }
    private func hotkey(_ label: String, _ steps: [KeyItem] = [.text("x")], chord: KeyChord? = nil, onBar: Bool = true) -> Hotkey {
        Hotkey(id: label.lowercased(), label: label, steps: steps, chord: chord, showsOnBar: onBar)
    }
    private func command(_ view: KeyCaptureView, _ input: String, _ flags: UIKeyModifierFlags = []) throws -> UIKeyCommand {
        try XCTUnwrap(view.keyCommands?.first { $0.input == input && $0.modifierFlags == flags }, "no command for \(input.debugDescription) \(flags)")
    }
    private func fire(_ view: KeyCaptureView, _ input: String, _ flags: UIKeyModifierFlags = []) throws {
        view.keyCommandFired(try command(view, input, flags))
    }
    private func chord(_ letter: String, _ modifiers: ChordModifiers = .command) -> KeyChord { KeyChord(keyCode: HIDKey.code(forCharacter: letter)!, modifiers: modifiers) }
    private func type(_ view: KeyCaptureView, _ text: String) { for character in text { view.insertText(String(character)) } }
    private func down(_ code: Int, _ modifiers: ChordModifiers = []) -> KeyEventRecord { KeyEventRecord(phase: .down, keyCode: code, modifiers: modifiers) }
    private func up(_ code: Int) -> KeyEventRecord { KeyEventRecord(phase: .up, keyCode: code, modifiers: []) }

    // MARK: Opening and closing

    func testCommandKOpensTheMenuAndAgainClosesIt() throws {
        let (view, got) = makeView()
        let open = try command(view, "k", .command)
        XCTAssertTrue(open.wantsPriorityOverSystemBehavior)
        XCTAssertEqual(open.discoverabilityTitle, "Hotkey menu")
        view.keyCommandFired(open)
        XCTAssertTrue(view.palette.isOpen)
        view.keyCommandFired(open)
        XCTAssertFalse(view.palette.isOpen)
        XCTAssertEqual(got.items, [], "the menu shortcut is not typed into the shell")
    }
    func testTheKeyBarButtonAndAMenuShortcutOpenItToo() throws {
        let tap = KeyChord(keyCode: HIDKey.leftControl)
        let (view, _) = makeView(palette: [tap])
        XCTAssertEqual(view.bar.buttons[.palette]?.accessibilityLabel, "Hotkey menu")
        view.bar.tapped(.palette)
        XCTAssertTrue(view.palette.isOpen)
        view.bar.tapped(.palette)
        XCTAssertFalse(view.palette.isOpen)
        XCTAssertFalse(view.handle(down(HIDKey.leftControl)), "a modifier going down is never used up")
        XCTAssertFalse(view.palette.isOpen, "not until it is let go")
        XCTAssertFalse(view.handle(up(HIDKey.leftControl)))
        XCTAssertTrue(view.palette.isOpen, "a tap on the key opens the menu")
        _ = view.handle(down(HIDKey.leftControl)); _ = view.handle(up(HIDKey.leftControl))
        XCTAssertFalse(view.palette.isOpen, "and the next tap closes it")
    }
    func testACommandThatTookTheKeyBehindOurBackIsNotATap() throws {
        let (view, got) = makeView(palette: [KeyChord(keyCode: HIDKey.leftControl)])
        _ = view.handle(down(HIDKey.leftControl))
        try fire(view, "e", .control)   // the key command takes the E: no press for it ever reaches the view
        _ = view.handle(up(HIDKey.leftControl))
        XCTAssertFalse(view.palette.isOpen, "Ctrl-E is Ctrl-E, not a tap on Ctrl")
        XCTAssertEqual(got.items, [.key(.control("e"))])
    }
    func testAModifierUsedWithAnotherKeyIsNotATap() {
        let (view, got) = makeView(palette: [KeyChord(keyCode: HIDKey.leftControl)])
        _ = view.handle(down(HIDKey.leftControl))
        _ = view.handle(down(HIDKey.a, .control)); _ = view.handle(up(HIDKey.a))
        _ = view.handle(up(HIDKey.leftControl))
        XCTAssertFalse(view.palette.isOpen, "Ctrl-A is a chord, not a tap on Ctrl")
        XCTAssertEqual(got.items, [])
    }
    func testEscapeClosesTheMenuInsteadOfBeingSentAndSendsEscapeOtherwise() throws {
        let (view, got) = makeView()
        view.togglePalette()
        try fire(view, UIKeyCommand.inputEscape)
        XCTAssertFalse(view.palette.isOpen)
        XCTAssertEqual(got.items, [], "Escape closed the menu")
        try fire(view, UIKeyCommand.inputEscape)
        XCTAssertEqual(got.items, [.key(.escape)], "with the menu closed it is the shell's again")
    }
    func testCommandPeriodIsTheSystemsCancelAndClosesTheMenu() throws {
        let (view, got) = makeView()
        view.togglePalette()
        try fire(view, ".", .command)
        XCTAssertFalse(view.palette.isOpen)
        XCTAssertNil(view.keyCommands?.first { $0.input == "." && $0.modifierFlags == .command }, "with the menu closed it is not claimed")
        XCTAssertEqual(got.items, [], "and does nothing to the shell")
    }
    func testTheMenuDoesNotOpenOutOfSightUnderASheet() async throws {
        let (view, _) = makeView()
        let root = try XCTUnwrap(view.window?.rootViewController)
        root.present(UIViewController(), animated: false)
        view.togglePalette()
        XCTAssertFalse(view.palette.isOpen)
        root.dismiss(animated: false)
        for _ in 0..<100 where root.presentedViewController != nil { try await Task.sleep(for: .milliseconds(20)) }
        view.togglePalette()
        XCTAssertTrue(view.palette.isOpen)
    }
    func testHidingTheKeyboardOrLosingItClosesTheMenu() {
        let (view, _) = makeView()
        XCTAssertTrue(view.becomeFirstResponder())
        view.togglePalette()
        XCTAssertTrue(view.palette.isOpen)
        _ = view.resignFirstResponder()
        XCTAssertFalse(view.palette.isOpen)
    }

    // MARK: Driving it from the keyboard

    func testTypingFiltersAndReturnSendsTheChosenHotkeyAndClosesTheMenu() throws {
        let (view, got) = makeView(hotkeys: [hotkey("Compact", [.text("/compact")]), hotkey("Clear", [.text("/clear"), .key(.enter)])])
        view.togglePalette()
        type(view, "cle")
        XCTAssertEqual(view.palette.state?.query, "cle")
        XCTAssertEqual(view.palette.state?.selected?.title, "Clear")
        XCTAssertEqual(got.items, [], "what is typed goes to the filter, not the shell")
        view.insertText("\n")
        XCTAssertEqual(got.items, [.text("/clear"), .key(.enter)])
        XCTAssertFalse(view.palette.isOpen)
        view.insertText("a")
        XCTAssertEqual(got.items.last, .text("a"), "and typing is the shell's again")
    }
    func testBackspaceEditsTheFilter() {
        let (view, got) = makeView()
        view.togglePalette()
        type(view, "ab")
        view.deleteBackward()
        XCTAssertEqual(view.palette.state?.query, "a")
        XCTAssertEqual(got.items, [])
    }
    func testUpDownTabAndTheEmacsKeysMoveTheSelection() throws {
        let (view, got) = makeView(hotkeys: [hotkey("One"), hotkey("Two"), hotkey("Three")])
        view.togglePalette()
        func selected() -> String? { view.palette.state?.selected?.title }
        try fire(view, UIKeyCommand.inputDownArrow); XCTAssertEqual(selected(), "Two")
        try fire(view, "\t"); XCTAssertEqual(selected(), "Three")
        try fire(view, "n", .control); XCTAssertEqual(view.palette.state?.selection, 3)
        try fire(view, UIKeyCommand.inputUpArrow); XCTAssertEqual(selected(), "Three")
        try fire(view, "\t", .shift); XCTAssertEqual(selected(), "Two")
        try fire(view, "p", .control); XCTAssertEqual(selected(), "One")
        try fire(view, "j", .control); try fire(view, "k", .control); XCTAssertEqual(selected(), "One")
        try fire(view, UIKeyCommand.inputPageDown); XCTAssertEqual(view.palette.state?.selection, HotkeyPalette.pageSize)
        try fire(view, UIKeyCommand.inputPageUp); XCTAssertEqual(view.palette.state?.selection, 0)
        XCTAssertEqual(got.items, [], "none of these reached the shell")
        XCTAssertTrue(view.palette.isOpen)
    }
    func testControlMIsReturnInTheMenu() throws {
        let (view, got) = makeView(hotkeys: [hotkey("One", [.text("1")])])
        view.togglePalette()
        try fire(view, "m", .control)
        XCTAssertEqual(got.items, [.text("1")])
        XCTAssertFalse(view.palette.isOpen)
        try fire(view, "m", .control)
        XCTAssertEqual(got.items.last, .key(.control("m")), "with the menu closed it is the shell's Ctrl-M")
    }
    func testControlCAndControlGCloseTheMenuWithoutInterruptingTheShell() throws {
        let (view, got) = makeView()
        view.togglePalette()
        try fire(view, "c", .control)
        XCTAssertFalse(view.palette.isOpen)
        view.togglePalette(); try fire(view, "g", .control)
        XCTAssertFalse(view.palette.isOpen)
        XCTAssertEqual(got.items, [])
        view.togglePalette(); type(view, "ab"); try fire(view, "u", .control)
        XCTAssertEqual(view.palette.state?.query, "", "Ctrl-U clears the filter")
    }
    func testAnyOtherKeyIsSwallowedWhileTheMenuIsOpenSoNothingLeaksToTheShell() throws {
        let (view, got) = makeView()
        view.togglePalette()
        try fire(view, "a", .control)
        try fire(view, UIKeyCommand.inputLeftArrow)
        try fire(view, UIKeyCommand.inputHome)
        XCTAssertEqual(got.items, [])
        XCTAssertTrue(view.palette.isOpen)
    }
    func testASoftwareKeyboardWorksToo() {
        let (view, got) = makeView(hotkeys: [hotkey("One"), hotkey("Two")])
        view.togglePalette()
        view.insertText("\t")
        XCTAssertEqual(view.palette.state?.selected?.title, "Two", "Tab moves on")
        view.insertText("\r")
        XCTAssertEqual(got.items, [.text("x")])
    }
    func testAKeyBarKeyMovesTheMenuAndAnythingElseClosesIt() {
        let (view, got) = makeView(hotkeys: [hotkey("One"), hotkey("Two")])
        view.togglePalette()
        view.bar.tapped(.key(.down))
        XCTAssertEqual(view.palette.state?.selected?.title, "Two")
        view.bar.tapped(.text("|"))
        XCTAssertEqual(view.palette.state?.query, "|", "a symbol key types into the filter")
        view.bar.tapped(.key(.escape))
        XCTAssertFalse(view.palette.isOpen)
        XCTAssertEqual(got.items, [])
        view.togglePalette()
        view.bar.tapped(.key(.left))
        XCTAssertFalse(view.palette.isOpen, "a key the menu has no use for closes it and goes to the shell")
        XCTAssertEqual(got.items, [.key(.left)])
        view.togglePalette()
        view.bar.tapped(.hotkey(Hotkey.builtIn[0].id))
        XCTAssertFalse(view.palette.isOpen)
        XCTAssertEqual(got.items.last, .key(.control("c")))
    }
    func testTappingARowSendsItLikeReturn() {
        let (view, got) = makeView(hotkeys: [hotkey("One", [.text("1")]), hotkey("Two", [.text("2")])])
        view.togglePalette()
        view.palette.choose(index: 1)
        XCTAssertEqual(got.items, [.text("2")])
        XCTAssertFalse(view.palette.isOpen)
    }

    // MARK: Reaching the editor

    func testTheMenuLeadsToTheConfiguration() throws {
        let (view, got) = makeView(hotkeys: [hotkey("Clear")])
        view.togglePalette(); type(view, "config"); view.insertText("\n")
        XCTAssertEqual(got.edited, ["list"])
        XCTAssertFalse(view.palette.isOpen)
        view.togglePalette(); type(view, "new hotkey"); view.insertText("\n")
        XCTAssertEqual(got.edited, ["list", "new"])
        view.togglePalette()
        try fire(view, "n", .command)
        XCTAssertEqual(got.edited, ["list", "new", "new"], "⌘N in the menu adds a hotkey")
        view.togglePalette()
        try fire(view, "\r", .shift)
        XCTAssertEqual(got.edited.last, "edit Clear", "Shift-Return edits the chosen one")
        XCTAssertEqual(got.items, [])
        try fire(view, ",", .command)
        XCTAssertEqual(got.edited.last, "list", "⌘, opens the hotkey settings from anywhere")
    }
    func testShiftReturnAndCommandNDoNothingToTheShellWhenTheMenuIsClosed() throws {
        let (view, got) = makeView()
        try fire(view, "\r", .shift)
        XCTAssertEqual(got.items, [.key(.enter)], "Shift-Return was Enter before and still is")
        XCTAssertNil(view.keyCommands?.first { $0.input == "n" && $0.modifierFlags == .command }, "⌘N is the app's (a new terminal) while the menu is closed")
        XCTAssertEqual(got.items, [.key(.enter)])
        XCTAssertEqual(got.edited, [])
    }

    // MARK: Shortcuts for hotkeys

    func testAShortcutRunsItsHotkeyWithoutOpeningTheMenu() throws {
        let esc = hotkey("Esc", [.key(.escape)], chord: chord("e"), onBar: false)
        let (view, got) = makeView(hotkeys: [esc])
        let command = try command(view, "e", .command)
        XCTAssertTrue(command.wantsPriorityOverSystemBehavior, "it beats the system's own use of the keys")
        view.keyCommandFired(command)
        XCTAssertEqual(got.items, [.key(.escape)])
        XCTAssertFalse(view.palette.isOpen)
    }
    func testShortcutsOnShiftedAndPunctuationKeysAreCommandsToo() throws {
        let (view, got) = makeView(hotkeys: [hotkey("A", [.text("a")], chord: chord("t", [.command, .shift])), hotkey("B", [.text("b")], chord: KeyChord(keyCode: 0x38, modifiers: .alt))])
        try fire(view, "t", [.command, .shift]); try fire(view, "/", .alternate)
        XCTAssertEqual(got.items, [.text("a"), .text("b")])
    }
    func testAShortcutThatTheKeyCommandsCannotNameIsUsedFromTheKeyPress() {
        let insert = KeyChord(keyCode: 0x49, modifiers: .control)
        let (view, got) = makeView(hotkeys: [hotkey("Ins", [.key(.escape)], chord: insert)])
        XCTAssertFalse(view.handle(KeyEventRecord(phase: .down, keyCode: 0x49, modifiers: .alt)), "other modifiers: not ours")
        XCTAssertTrue(view.handle(down(0x49, .control)), "used up, so the text system does not also see it")
        XCTAssertEqual(got.items, [.key(.escape)])
        XCTAssertFalse(view.handle(up(0x49)))
    }
    func testAShortcutWithAKeyCommandIsLeftToItsCommandSoItFiresOnce() throws {
        let (view, got) = makeView(hotkeys: [hotkey("Esc", [.key(.escape)], chord: chord("e"))])
        XCTAssertFalse(view.handle(down(HIDKey.e, .command)), "not used up by the press…")
        XCTAssertEqual(got.items, [], "…and not sent by it")
        try fire(view, "e", .command)
        XCTAssertEqual(got.items, [.key(.escape)], "the command sends it, once")
    }
    func testAFunctionKeyShortcutIsACommand() throws {
        let f5 = KeyChord(keyCode: HIDKey.f1 + 4)
        let (view, got) = makeView(hotkeys: [hotkey("Run", [.text("make")], chord: f5)])
        try fire(view, UIKeyCommand.f5)
        XCTAssertEqual(got.items, [.text("make")])
    }
    func testAUserShortcutBeatsTheBuiltInMeaningOfTheSameKeys() throws {
        let (view, got) = makeView(hotkeys: [hotkey("Mine", [.text("mine")], chord: chord("e", .control))])
        XCTAssertEqual(view.keyCommands?.filter { $0.input == "e" && $0.modifierFlags == .control }.count, 1, "not registered twice")
        try fire(view, "e", .control)
        XCTAssertEqual(got.items, [.text("mine")], "instead of Ctrl-E")
    }
    func testControlLetterStillGoesToTheShellWithoutAShortcut() throws {
        let (view, got) = makeView()
        try fire(view, "e", .control)
        XCTAssertEqual(got.items, [.key(.control("e"))])
    }
    func testControlBracketIsEscape() throws {
        let (view, got) = makeView()
        try fire(view, "[", .control)
        XCTAssertEqual(got.items, [.key(.escape)], "the way to Esc on a keyboard whose Ctrl is its only modifier")
    }
    func testShortcutCommandsComeAndGoWithTheHotkeys() throws {
        let (view, _) = makeView(hotkeys: [hotkey("Esc", [.key(.escape)], chord: chord("e"))])
        XCTAssertNotNil(view.keyCommands?.first { $0.input == "e" && $0.modifierFlags == .command })
        view.hotkeys = []
        XCTAssertNil(view.keyCommands?.first { $0.input == "e" && $0.modifierFlags == .command })
        XCTAssertTrue(view.keyCommands?.allSatisfy(\.wantsPriorityOverSystemBehavior) ?? false)
    }
    func testUpdatingWithTheSameHotkeysKeepsTheKeyCommandsAsTheyAre() throws {
        // SwiftUI pushes the hotkeys on every update, also in the middle of a key press; commands rebuilt then are lost to that press.
        let esc = hotkey("Esc", [.key(.escape)], chord: chord("e"))
        let (view, _) = makeView(hotkeys: [esc])
        let before = try XCTUnwrap(view.keyCommands)
        view.hotkeys = [esc]
        view.shortcutSettings = ShortcutSettings()
        let after = try XCTUnwrap(view.keyCommands)
        XCTAssertEqual(before.count, after.count)
        XCTAssertTrue(zip(before, after).allSatisfy { $0 === $1 }, "the same command objects")
        view.hotkeys = [esc, hotkey("Other", chord: chord("o"))]
        XCTAssertEqual(try XCTUnwrap(view.keyCommands).count, before.count + 1, "a real change does rebuild them")
    }
    func testTheMenuControllerReportsWhatWasChosenAndNothingWhenClosed() {
        let controller = PaletteController()
        var outcomes: [HotkeyPalette.Outcome] = []
        controller.onOutcome = { outcomes.append($0) }
        controller.apply(.activate)
        controller.choose(index: 0)
        XCTAssertEqual(outcomes, [], "closed: nothing")
        let one = hotkey("One")
        controller.open(hotkeys: [one])
        XCTAssertTrue(controller.isOpen)
        controller.apply(.down)
        XCTAssertEqual(outcomes, [], "moving is not a choice")
        controller.choose(index: 0)
        XCTAssertEqual(outcomes, [.hotkey(one)])
        controller.close()
        controller.toggle(hotkeys: [one])
        XCTAssertTrue(controller.isOpen)
        controller.toggle(hotkeys: [one])
        XCTAssertFalse(controller.isOpen)
        controller.open(hotkeys: []); controller.close()
        XCTAssertNil(controller.state)
    }
    func testTheKeyBarShowsOnlyTheHotkeysThatWantAButton() {
        let shown = hotkey("Shown"), hidden = hotkey("Hidden", chord: chord("h", .control), onBar: false)
        let (view, _) = makeView(hotkeys: [shown, hidden])
        XCTAssertNotNil(view.bar.buttons[.hotkey(shown.id)])
        XCTAssertNil(view.bar.buttons[.hotkey(hidden.id)])
    }

    // MARK: Inside the menu, a shortcut stands in for the key it sends

    func testTheClicksTemplateWorksInsideTheMenuWithoutArrowKeys() throws {
        var library = HotkeyLibrary()
        library.merge(.clicks)
        let (view, got) = makeView(hotkeys: library.hotkeys + [hotkey("One"), hotkey("Two"), hotkey("Three")])
        view.togglePalette()
        func selected() -> String? { view.palette.state?.selected?.title }
        let first = selected()
        try fire(view, "s", .command)
        XCTAssertNotEqual(selected(), first, "⌘S is ↓ in the shell, and moves down in the menu")
        try fire(view, "w", .command)
        XCTAssertEqual(selected(), first, "⌘W is ↑")
        try fire(view, "f", .command)
        XCTAssertEqual(view.palette.state?.selection, HotkeyPalette.pageSize, "⌘F is Page Down")
        try fire(view, "c", [.command, .shift])
        XCTAssertFalse(view.palette.isOpen, "⌘⇧C is Ctrl-C, which cancels the menu")
        view.togglePalette()
        try fire(view, "e", .command)
        XCTAssertFalse(view.palette.isOpen, "⌘E is Esc")
        XCTAssertEqual(got.items, [], "none of it reached the shell")
        try fire(view, "w", .command)
        XCTAssertEqual(got.items, [.key(.up)], "with the menu closed ⌘W sends the arrow")
    }

    // MARK: The readout

    func testEveryKeyEventIsReportedWithItsUsageAndModifiers() throws {
        let (view, got) = makeView()
        try fire(view, "k", .command)
        let event = try XCTUnwrap(got.events.last)
        XCTAssertEqual(event.phase, .command)
        XCTAssertEqual(event.keyCode, HIDKey.k)
        XCTAssertEqual(event.modifiers, .command)
        XCTAssertEqual(event.characters, "k")
        _ = view.handle(down(HIDKey.leftCommand))
        XCTAssertEqual(got.events.last?.keyCode, HIDKey.leftCommand, "a lone modifier press is reported too")
        XCTAssertEqual(got.events.count, 2)
    }
}
