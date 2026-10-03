import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// The hotkey help (⌘/): opened and closed from the keyboard and the key bar, a reference that leaves the keyboard alone, and
/// chords that go on working under it.
@MainActor final class HotkeyHelpTests: XCTestCase {
    private var windows: [UIWindow] = []
    private final class Received { var items: [KeyItem] = []; var edited: [String] = [] }
    private func makeView(hotkeys: [Hotkey] = [], settings: ShortcutSettings = ShortcutSettings()) -> (KeyCaptureView, Received) {
        let received = Received()
        let view = KeyCaptureView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        view.onItems = { received.items.append(contentsOf: $0); return true }
        view.onEditHotkeys = { received.edited.append("list") }
        view.hotkeys = hotkeys
        view.shortcutSettings = settings
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 480))
        window.rootViewController = UIViewController()
        window.rootViewController?.view.addSubview(view)
        window.makeKeyAndVisible()
        windows.append(window)
        return (view, received)
    }
    private func clicksHotkeys() -> [Hotkey] { var library = HotkeyLibrary(); library.merge(.clicks); return library.hotkeys }
    private func command(_ view: KeyCaptureView, _ input: String, _ flags: UIKeyModifierFlags = []) throws -> UIKeyCommand {
        try XCTUnwrap(view.keyCommands?.first { $0.input == input && $0.modifierFlags == flags }, "no command for \(input.debugDescription) \(flags)")
    }
    private func fire(_ view: KeyCaptureView, _ input: String, _ flags: UIKeyModifierFlags = []) throws {
        view.keyCommandFired(try command(view, input, flags))
    }
    private func chord(_ letter: String, _ modifiers: ChordModifiers = .command) -> KeyChord { KeyChord(keyCode: HIDKey.code(forCharacter: letter)!, modifiers: modifiers) }
    private func down(_ code: Int, _ modifiers: ChordModifiers = []) -> KeyEventRecord { KeyEventRecord(phase: .down, keyCode: code, modifiers: modifiers) }
    private func up(_ code: Int) -> KeyEventRecord { KeyEventRecord(phase: .up, keyCode: code, modifiers: []) }

    // MARK: Opening and closing

    func testCommandSlashIsAlwaysOnAndTogglesTheHelp() throws {
        let (view, got) = makeView()
        let slash = try command(view, "/", .command)
        XCTAssertTrue(slash.wantsPriorityOverSystemBehavior, "it beats the system's own use of the keys")
        XCTAssertEqual(slash.discoverabilityTitle, "Hotkey help")
        XCTAssertNotNil(try command(view, "k", .command), "next to ⌘K")
        XCTAssertNotNil(try command(view, ",", .command), "and ⌘,")
        XCTAssertFalse(view.help.isOpen)
        view.keyCommandFired(slash)
        XCTAssertTrue(view.help.isOpen)
        view.keyCommandFired(slash)
        XCTAssertFalse(view.help.isOpen)
        XCTAssertEqual(got.items, [], "the shortcut is not typed into the shell")
    }
    func testTheKeyBarButtonTogglesItToo() {
        let (view, got) = makeView()
        XCTAssertEqual(view.bar.buttons[.help]?.accessibilityLabel, "Hotkey help")
        view.bar.tapped(.help)
        XCTAssertTrue(view.help.isOpen)
        view.bar.tapped(.help)
        XCTAssertFalse(view.help.isOpen)
        view.bar.buttons[.help]?.sendActions(for: .touchUpInside)
        XCTAssertTrue(view.help.isOpen, "a real tap on the button")
        XCTAssertEqual(got.items, [])
    }
    func testEscapeAndCommandPeriodCloseItAndEscapeIsNotSent() throws {
        let (view, got) = makeView()
        view.toggleHelp()
        XCTAssertNotNil(view.keyCommands?.first { $0.input == "." && $0.modifierFlags == .command }, "⌘. is claimed while it is open")
        try fire(view, UIKeyCommand.inputEscape)
        XCTAssertFalse(view.help.isOpen)
        XCTAssertEqual(got.items, [], "Escape closed the help")
        view.toggleHelp()
        try fire(view, ".", .command)
        XCTAssertFalse(view.help.isOpen)
        XCTAssertEqual(got.items, [], "⌘. is the system's cancel and does nothing to the shell")
        XCTAssertNil(view.keyCommands?.first { $0.input == "." && $0.modifierFlags == .command }, "not claimed once it is closed")
        try fire(view, UIKeyCommand.inputEscape)
        XCTAssertEqual(got.items, [.key(.escape)], "with the help closed Escape is the shell's again")
    }
    func testTheHelpOpensAndClosesWithoutTakingTheKeyboard() throws {
        let (view, _) = makeView()
        XCTAssertTrue(view.becomeFirstResponder())
        try fire(view, "/", .command)
        XCTAssertTrue(view.help.isOpen)
        XCTAssertTrue(view.isFirstResponder, "the capture view still has the keyboard, so typing goes on")
        view.help.close()
        XCTAssertTrue(view.isFirstResponder)
        try fire(view, "/", .command)
        _ = view.resignFirstResponder()
        XCTAssertFalse(view.help.isOpen, "it goes with the keyboard: nothing could drive it any more")
    }
    func testItDoesNotOpenOutOfSightUnderASheet() async throws {
        let (view, _) = makeView()
        let root = try XCTUnwrap(view.window?.rootViewController)
        root.present(UIViewController(), animated: false)
        view.toggleHelp()
        XCTAssertFalse(view.help.isOpen)
        root.dismiss(animated: false)
        for _ in 0..<100 where root.presentedViewController != nil { try await Task.sleep(for: .milliseconds(20)) }
        view.toggleHelp()
        XCTAssertTrue(view.help.isOpen)
    }
    func testTheHelpAndTheMenuTakeTurns() throws {
        let (view, got) = makeView()
        try fire(view, "/", .command)
        XCTAssertTrue(view.help.isOpen)
        try fire(view, "k", .command)
        XCTAssertTrue(view.palette.isOpen)
        XCTAssertFalse(view.help.isOpen, "⌘K puts the menu where the help was")
        try fire(view, "/", .command)
        XCTAssertTrue(view.help.isOpen)
        XCTAssertFalse(view.palette.isOpen, "and ⌘/ the other way")
        view.bar.tapped(.palette)
        XCTAssertTrue(view.palette.isOpen); XCTAssertFalse(view.help.isOpen)
        view.bar.tapped(.help)
        XCTAssertTrue(view.help.isOpen); XCTAssertFalse(view.palette.isOpen, "the key bar's buttons do the same")
        try fire(view, ",", .command)
        XCTAssertFalse(view.help.isOpen, "the settings open over a clear terminal")
        XCTAssertEqual(got.edited, ["list"])
        XCTAssertEqual(got.items, [])
    }

    // MARK: What it shows

    func testItListsTheHotkeysWithAShortcutFirstThenTheOthersWithTheirGlyphs() throws {
        let mine = Hotkey(id: "mine", label: "Clear", steps: [.text("/clear"), .key(.enter)])
        let (view, _) = makeView(hotkeys: clicksHotkeys() + [mine])
        view.toggleHelp()
        let help = try XCTUnwrap(view.help.state)
        XCTAssertEqual(help.withShortcut.count, HotkeyTemplate.clicks.hotkeys.count)
        XCTAssertEqual(help.withShortcut.first?.label, "Esc")
        XCTAssertEqual(help.withShortcut.first?.shortcut, "⌘E")
        XCTAssertEqual(help.withShortcut.first { $0.label == "⇧Tab" }?.shortcut, "⇧⌘T")
        XCTAssertEqual(help.withShortcut.first { $0.label == "^C" }?.sends, "^C")
        XCTAssertEqual(help.withoutShortcut.map(\.label), ["Clear", "^A", "^E", "^U", "^W", "Esc Esc"], "the built-in ones the template covers are not listed twice")
        XCTAssertEqual(help.withoutShortcut.first?.shortcut, nil, "shown as a dash")
        XCTAssertEqual(help.withoutShortcut.first?.sends, "/clear ⏎")
        XCTAssertEqual(help.appShortcuts.map(\.keys), ["⌘K", "⌘,", "⌘N", "⌘/"])
    }
    func testItIsWorkedOutWhenItOpensFromTheHotkeysAndShortcutsOfTheMoment() throws {
        let (view, _) = makeView()
        view.toggleHelp()
        XCTAssertEqual(view.help.state?.withShortcut, [])
        view.toggleHelp()
        view.hotkeys = clicksHotkeys()
        view.shortcutSettings = ShortcutSettings(helpChords: [chord("j")])
        view.toggleHelp()
        XCTAssertEqual(view.help.state?.withShortcut.count, 14)
        XCTAssertEqual(view.help.state?.appShortcuts.last?.keys, "⌘/ · ⌘J")
    }

    // MARK: Using the keyboard under it

    func testAChordFiresItsHotkeyWhileTheHelpIsOpenAndTheHelpStaysForTheNextOne() throws {
        let (view, got) = makeView(hotkeys: clicksHotkeys())
        view.toggleHelp()
        try fire(view, "e", .command)
        XCTAssertEqual(got.items, [.key(.escape)], "⌘E is Esc, as always")
        XCTAssertTrue(view.help.isOpen, "and the help is still there")
        try fire(view, "t", .command)
        try fire(view, "t", [.command, .shift])
        try fire(view, "c", [.command, .shift])
        XCTAssertEqual(got.items, [.key(.escape), .key(.tab), .key(.backTab), .key(.control("c"))])
        XCTAssertTrue(view.help.isOpen, "as many as wanted in a row")
    }
    func testAChordTheCommandsCannotNameIsFiredFromTheKeyPressToo() {
        let insert = KeyChord(keyCode: 0x49, modifiers: .control)
        let (view, got) = makeView(hotkeys: [Hotkey(id: "ins", label: "Ins", steps: [.key(.escape)], chord: insert)])
        view.toggleHelp()
        XCTAssertTrue(view.handle(down(0x49, .control)), "used up, as with the help closed")
        XCTAssertEqual(got.items, [.key(.escape)])
        XCTAssertTrue(view.help.isOpen)
    }
    func testTypingArrowsAndControlKeysStayTheShellsWhileItIsOpen() throws {
        let (view, got) = makeView()
        view.toggleHelp()
        view.insertText("ls")
        try fire(view, UIKeyCommand.inputDownArrow)
        try fire(view, UIKeyCommand.inputUpArrow)
        try fire(view, "\t")
        try fire(view, "c", .control)
        view.deleteBackward()
        XCTAssertEqual(got.items, [.text("ls"), .key(.down), .key(.up), .key(.tab), .key(.control("c")), .key(.backspace)])
        XCTAssertTrue(view.help.isOpen, "none of them closes it or is taken by it")
    }
    func testATapOnARowSendsItsHotkeyAndTheHelpStays() throws {
        let one = Hotkey(id: "one", label: "One", steps: [.text("1")]), two = Hotkey(id: "two", label: "Two", steps: [.text("2")], chord: chord("y"))
        let (view, got) = makeView(hotkeys: [one, two])
        view.help.fire(one)
        XCTAssertEqual(got.items, [], "nothing while the help is closed")
        view.toggleHelp()
        view.help.fire(one); view.help.fire(two)
        XCTAssertEqual(got.items, [.text("1"), .text("2")])
        XCTAssertTrue(view.help.isOpen)
        view.bar.tapped(.control)
        view.help.fire(one)
        XCTAssertFalse(view.mapper.controlArmed, "a hotkey is complete in itself, as from the menu")
    }

    // MARK: Its shortcut is the person's to set

    func testTheHelpOpensOnTheExtraShortcutsThePersonAdded() throws {
        let (view, got) = makeView(settings: ShortcutSettings(helpChords: [chord("j"), KeyChord(keyCode: HIDKey.rightControl)]))
        XCTAssertNotNil(try command(view, "j", .command), "a command for it, with priority")
        XCTAssertTrue(try command(view, "j", .command).wantsPriorityOverSystemBehavior)
        try fire(view, "j", .command)
        XCTAssertTrue(view.help.isOpen)
        try fire(view, "/", .command)
        XCTAssertFalse(view.help.isOpen, "⌘/ still works")
        _ = view.handle(down(HIDKey.rightControl)); _ = view.handle(up(HIDKey.rightControl))
        XCTAssertTrue(view.help.isOpen, "a tap on a modifier opens it too")
        _ = view.handle(down(HIDKey.rightControl)); _ = view.handle(up(HIDKey.rightControl))
        XCTAssertFalse(view.help.isOpen, "and the next tap closes it")
        XCTAssertFalse(view.palette.isOpen)
        view.shortcutSettings = ShortcutSettings()
        XCTAssertNil(view.keyCommands?.first { $0.input == "j" && $0.modifierFlags == .command }, "removed with the shortcut")
        XCTAssertEqual(got.items, [])
    }
    func testNoHotkeyCanTakeTheShortcutsOfTheHelp() throws {
        let defaults = UserDefaults(suiteName: "com.riwork.tests.\(UUID().uuidString)")!
        let store = HotkeyStore(defaults: defaults)
        XCTAssertThrowsError(try store.add(Hotkey(id: "a", label: "A", steps: [.text("a")], chord: .helpDefault)), "⌘/ is the help's")
        try store.addHelpChord(chord("j"))
        XCTAssertThrowsError(try store.add(Hotkey(id: "b", label: "B", steps: [.text("b")], chord: chord("j")))) { XCTAssertEqual($0 as? HotkeyError, .chordInUse("the hotkey help")) }
        try store.add(Hotkey(id: "c", label: "C", steps: [.text("c")], chord: chord("y")))
        XCTAssertThrowsError(try store.addHelpChord(chord("y"))) { XCTAssertEqual($0 as? HotkeyError, .chordInUse("C")) }
        XCTAssertThrowsError(try store.addHelpChord(.helpDefault), "⌘/ is always there")
        XCTAssertThrowsError(try store.addPaletteChord(chord("j"))) { XCTAssertEqual($0 as? HotkeyError, .chordInUse("the hotkey help")) }
        XCTAssertEqual(store.shortcutMap.action(for: .helpDefault), .openHelp)
        XCTAssertEqual(store.shortcutMap.action(for: chord("j")), .openHelp)
        let launched = HotkeyStore(defaults: defaults)
        XCTAssertEqual(launched.shortcuts.helpChords, [chord("j")], "kept on the device")
        launched.removeHelpChord(chord("j"))
        XCTAssertEqual(HotkeyStore(defaults: defaults).shortcuts.helpChords, [])
    }
    func testTheClicksTemplateAndTheirOwnChordsLeaveCommandSlashAlone() throws {
        let (view, got) = makeView(hotkeys: clicksHotkeys())
        let taken = Set(view.hotkeys.compactMap(\.chord))
        XCTAssertFalse(taken.contains(.helpDefault))
        XCTAssertEqual(view.shortcuts.action(for: .helpDefault), .openHelp, "the template cannot shadow it")
        try fire(view, "/", .command)
        XCTAssertTrue(view.help.isOpen)
        XCTAssertEqual(got.items, [])
        XCTAssertEqual(view.keyCommands?.filter { $0.input == "/" && $0.modifierFlags == .command }.count, 1, "one command for it, not two")
    }

    // MARK: What does not change

    func testTabAndALoneModifierTapAreAsTheyWere() async throws {
        let tap = KeyChord(keyCode: HIDKey.leftControl)
        let (view, got) = makeView(settings: ShortcutSettings(paletteChords: [tap]))
        try fire(view, "\t"); try fire(view, "\t", .shift)
        XCTAssertEqual(got.items, [.key(.tab), .key(.backTab)], "Tab is a Tab and Shift-Tab a Shift-Tab, at once")
        XCTAssertFalse(view.handle(down(HIDKey.tab)), "the press is not used up")
        XCTAssertFalse(view.handle(up(HIDKey.tab)))
        try await Task.sleep(for: .milliseconds(600))
        XCTAssertEqual(got.items, [.key(.tab), .key(.backTab)], "a Tab held down for a while does nothing more")
        XCTAssertFalse(view.help.isOpen); XCTAssertFalse(view.palette.isOpen)
        // A modifier held on its own, however long, does nothing new; a quick tap on one that is a shortcut still opens the menu.
        XCTAssertFalse(view.handle(down(HIDKey.leftCommand)))
        try await Task.sleep(for: .milliseconds(600))
        XCTAssertFalse(view.handle(up(HIDKey.leftCommand)))
        XCTAssertFalse(view.help.isOpen); XCTAssertFalse(view.palette.isOpen)
        _ = view.handle(down(HIDKey.leftControl)); _ = view.handle(up(HIDKey.leftControl))
        XCTAssertTrue(view.palette.isOpen, "a tap on the key chosen for it opens the menu")
        XCTAssertFalse(view.help.isOpen)
        XCTAssertEqual(got.items, [.key(.tab), .key(.backTab)])
    }
}
