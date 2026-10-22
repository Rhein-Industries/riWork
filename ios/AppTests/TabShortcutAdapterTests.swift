import XCTest
import UIKit
import SwiftUI
import RiWorkCore
@testable import RiWorkRemote

@MainActor final class TabShortcutAdapterTests: XCTestCase {
    private func event(_ text: String, phase: KeyEventRecord.Phase = .down) -> KeyEventRecord {
        KeyEventRecord(phase: phase, keyCode: text == "\t" ? HIDKey.tab : text == "esc" ? HIDKey.escape : HIDKey.code(forCharacter:text) ?? 0x38,
                       modifiers: [], characters: text, charactersIgnoringModifiers: text)
    }
    private func enabled() -> TabShortcutController { let controller = TabShortcutController(); controller.isEnabled = { true }; return controller }
    private func tap(_ key: String, controller: TabShortcutController, perform: (TabShortcutAction) -> Bool = { _ in true }) {
        _ = controller.handle(event(key),perform:perform)
        _ = controller.handle(event(key,phase:.up),perform:perform)
    }

    func testTerminalLeaderProducesOneLiteralTabAndEscCancellationProducesNoInput() {
        let controller = enabled(), terminal = KeyCaptureView()
        terminal.tabShortcuts = controller
        var input: [KeyItem] = []; terminal.onItems = { input += $0; return true }
        XCTAssertFalse(terminal.keyCommands?.contains { $0.input == "\t" && $0.modifierFlags.isEmpty } == true)
        XCTAssertTrue(terminal.handle(event("\t"))); XCTAssertTrue(terminal.handle(event("\t",phase:.up)))
        XCTAssertTrue(input.isEmpty)
        XCTAssertTrue(terminal.handle(event("q")))
        XCTAssertTrue(terminal.handle(event("q",phase:.command)),"a held second key's repeats cannot repeat the action")
        XCTAssertTrue(terminal.handle(event("q",phase:.up)))
        XCTAssertEqual(input,[.key(.tab)])
        XCTAssertFalse(terminal.handle(event("q")),"the next press of Q is the shell's")
        XCTAssertFalse(terminal.handle(event("q",phase:.up)))
        _ = terminal.handle(event("\t")); _ = terminal.handle(event("\t",phase:.up)); _ = terminal.handle(event("esc"))
        XCTAssertEqual(input,[.key(.tab)])
        terminal.insertText("\t")
        XCTAssertEqual(input,[.key(.tab),.key(.tab)],"software/touch Tab still inserts literally")
    }

    func testTerminalArrowsNavigateItsOpenKeyMenuWithoutReachingTheShell() {
        let controller = enabled(), terminal = KeyCaptureView()
        terminal.tabShortcuts = controller
        var input: [KeyItem] = []; terminal.onItems = { input += $0; return true }
        _ = terminal.handle(event("\t")); _ = terminal.handle(event("\t",phase:.up)); _ = terminal.handle(event("k")); _ = terminal.handle(event("k",phase:.up))
        XCTAssertTrue(terminal.palette.isOpen)
        _ = terminal.handle(event("\t")); _ = terminal.handle(event("\t",phase:.up)); _ = terminal.handle(event("s")); _ = terminal.handle(event("s",phase:.up))
        XCTAssertTrue(input.isEmpty)
        _ = terminal.handle(event("\t")); _ = terminal.handle(event("\t",phase:.up)); _ = terminal.handle(event("e"))
        XCTAssertFalse(terminal.palette.isOpen); XCTAssertTrue(input.isEmpty)
    }

    func testClaimedSecondKeyCannotLeakAcrossTerminalComposerAndMenuFocusChanges() throws {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("No test window scene") }
        let controller = enabled(), terminal = KeyCaptureView(), composer = ChatComposerTextView(), menu = KeyCommandHost.HostView()
        terminal.tabShortcuts = controller; composer.tabShortcuts = controller; menu.tabShortcuts = controller
        terminal.inputViewStandIn = UIView(); composer.inputView = UIView()
        let window = UIWindow(windowScene: scene), host = UIViewController()
        window.rootViewController = host; window.frame = CGRect(x:0,y:0,width:402,height:874)
        let responders: [UIView] = [terminal, composer, menu]
        responders.forEach { host.view.addSubview($0) }
        terminal.frame = CGRect(x:0,y:0,width:10,height:10); composer.frame = CGRect(x:0,y:20,width:300,height:100)
        window.makeKeyAndVisible()
        window.layoutIfNeeded(); host.view.layoutIfNeeded()
        defer { window.isHidden = true }
        XCTAssertTrue(terminal.becomeFirstResponder())
        composer.text = "draft"
        var sent: [KeyItem] = [], actions = 0, menuActions = 0
        terminal.onItems = { sent += $0; return true }
        menu.table.actions = [KeyAction(input:"n", run:{menuActions += 1})]
        let owner = UUID()
        controller.setAppHandler(owner: owner) { action in
            XCTAssertEqual(action, .newTerminal)
            actions += 1
            // The originating view resigns and its screen relinquishes its handler
            // before the same physical key has been released.
            XCTAssertTrue(composer.becomeFirstResponder())
            controller.clearAppHandler(owner: owner)
            controller.cancelPrefix() // Navigation path change / guide dismissal.
        }
        _ = terminal.handle(event("\t")); _ = terminal.handle(event("\t",phase:.up))
        XCTAssertTrue(terminal.handle(event("n")))
        XCTAssertEqual(actions, 1); XCTAssertFalse(controller.armed)
        XCTAssertTrue(composer.isFirstResponder)
        // While N is still held, its key-command echoes and repeats reach whichever responder has the keyboard now.
        XCTAssertTrue(composer.handleTabShortcut(event("n",phase:.command)))
        XCTAssertTrue(menu.becomeFirstResponder())
        XCTAssertTrue(menu.handleTabShortcut(event("n",phase:.command)))
        menu.fire(UIKeyCommand(input:"n",modifierFlags:[],action:NSSelectorFromString("unused:")))
        XCTAssertEqual(composer.text, "draft"); XCTAssertEqual(menuActions, 0); XCTAssertTrue(sent.isEmpty)
        XCTAssertTrue(menu.handleTabShortcut(event("n",phase:.up)))
        XCTAssertFalse(menu.handleTabShortcut(event("n")), "a genuinely new press belongs to the next responder")
        menu.fire(UIKeyCommand(input:"n",modifierFlags:[],action:NSSelectorFromString("unused:")))
        XCTAssertEqual(menuActions, 1)
    }

    func testASuggestionListKeepsItsTabThroughCommandTAndTabQ() {
        let controller = enabled(), composer = ChatComposerTextView()
        composer.tabShortcuts = controller; composer.usesClicks = true; composer.suggesting = true
        composer.text = "/comp"
        var keys: [ChatKey] = []; composer.onKey = { keys.append($0); return .none }
        XCTAssertFalse(composer.keyCommands?.contains { $0.input == "\t" && $0.modifierFlags.isEmpty } == true, "Tab is the prefix")
        XCTAssertTrue(composer.keyCommands?.contains { $0.input == "t" && $0.modifierFlags == .command } == true, "⌘T is the list's Tab")
        XCTAssertTrue(composer.handleTabShortcut(event("\t"))); XCTAssertTrue(composer.handleTabShortcut(event("\t",phase:.up)))
        XCTAssertTrue(composer.handleTabShortcut(event("q")))
        XCTAssertEqual(keys, [.tab]); XCTAssertEqual(composer.text, "/comp", "the row goes in, not a tab character")
    }

    func testASheetKeepsTheScreenBehindItFromActing() {
        let controller = enabled(), owner = UUID(), sheet = UUID()
        var actions: [TabShortcutAction] = []
        controller.setAppHandler(owner: owner) { actions.append($0) }
        controller.suspendApp(owner: sheet)
        tap("\t", controller: controller); tap("2", controller: controller, perform: { _ in false })
        XCTAssertTrue(actions.isEmpty, "Tab 2 does not switch tabs under the sheet")
        controller.resumeApp(owner: sheet)
        tap("\t", controller: controller); tap("2", controller: controller, perform: { _ in false })
        XCTAssertEqual(actions, [.selectTab(2)])
    }

    func testAKeyCommandWithoutAPressLeavesNoClaimBehind() {
        let controller = enabled()
        tap("\t", controller: controller)
        // ⌘E and its kind reach no press handler: only the key command arrives, and no release follows it.
        XCTAssertTrue(controller.handle(event("e",phase:.command), perform:{_ in true}))
        XCTAssertFalse(controller.armed)
        XCTAssertFalse(controller.handle(event("e"), perform:{_ in XCTFail("no prefix is armed"); return true}), "the next e is typed")
    }

    func testPrefixDismissalKeepsAHeldTabsReleaseButAFullKeyboardResetDropsIt() {
        let controller = enabled()
        _ = controller.handle(event("\t"), perform:{_ in true})
        controller.cancelPrefix()
        XCTAssertFalse(controller.armed)
        XCTAssertTrue(controller.handle(event("\t",phase:.up), perform:{_ in true}), "the dismissed Tab's release stays out of the next responder")
        XCTAssertFalse(controller.armed, "and does not arm the prefix again")
        _ = controller.handle(event("\t"), perform:{_ in true})
        XCTAssertTrue(controller.armed)
        controller.cancel()
        XCTAssertFalse(controller.handle(event("\t",phase:.up), perform:{_ in true}), "a reset leaves no claim behind")
        XCTAssertFalse(controller.handle(event("n"), perform:{_ in XCTFail("the reset removes the prefix"); return true}))
        XCTAssertFalse(controller.armed)
    }

    func testALostReleaseDoesNotSwallowTheNextPressOfThatKey() {
        let controller = enabled(), terminal = KeyCaptureView()
        terminal.tabShortcuts = controller
        var actions: [TabShortcutAction] = []
        controller.setAppHandler(owner: UUID()) { actions.append($0) }
        tap("\t", controller: controller)
        // Tab 2 switches from a chat to a terminal before 2 comes up: its release reaches no responder that knows the prefix.
        XCTAssertTrue(controller.handle(event("2"), perform: { _ in false }))
        XCTAssertEqual(actions, [.selectTab(2)])
        XCTAssertTrue(controller.handle(event("2",phase:.command), perform: { _ in XCTFail("a repeat acts again"); return false }), "while held, its repeats stay the prefix's")
        XCTAssertFalse(terminal.handle(event("2")), "the next 2 pressed in the terminal is typed")
        XCTAssertFalse(terminal.handle(event("2",phase:.up)))
        XCTAssertEqual(actions, [.selectTab(2)])
    }

    func testHoldingCommandEToCancelThePrefixSendsNoEscape() {
        let controller = enabled()
        tap("\t", controller: controller)
        let commandE = KeyEventRecord(phase: .command, keyCode: HIDKey.e, modifiers: .command, characters: "e", charactersIgnoringModifiers: "e")
        XCTAssertTrue(controller.handle(commandE, perform: { _ in XCTFail("⌘E only cancels"); return true }))
        XCTAssertFalse(controller.armed)
        for _ in 0..<3 {
            XCTAssertTrue(controller.handle(commandE, perform: { _ in XCTFail("no prefix is armed"); return true }), "its repeats are not the screen's Escape")
        }
        // A press or release shows the keys were let go: the next ⌘E is the screen's again.
        XCTAssertFalse(controller.handle(KeyEventRecord(phase: .up, keyCode: HIDKey.leftCommand, modifiers: []), perform: { _ in true }))
        XCTAssertFalse(controller.handle(commandE, perform: { _ in true }))
    }

    func testASheetWithoutTheGuideTurnsThePrefixOff() {
        let controller = enabled()
        var covered = true
        controller.modalPresented = { covered }
        XCTAssertFalse(controller.handle(event("\t"), perform: { _ in true }), "Tab under the Display sheet is not a hidden prefix")
        XCTAssertFalse(controller.armed)
        let sheet = UUID()
        controller.suspendApp(owner: sheet)
        XCTAssertTrue(controller.handle(event("\t"), perform: { _ in true }), "a sheet that shows the guide keeps the prefix")
        XCTAssertTrue(controller.armed)
        controller.resumeApp(owner: sheet)
        XCTAssertFalse(controller.handle(event("q"), perform: { _ in XCTFail("nothing acts under a foreign sheet"); return true }))
        XCTAssertFalse(controller.armed)
        covered = false
        tap("\t", controller: controller)
        XCTAssertTrue(controller.armed)
    }

    func testThePrefixAsksTheKeyWindowWhetherASheetCoversIt() async throws {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("No test window scene") }
        let window = UIWindow(windowScene: scene), root = UIViewController()
        window.rootViewController = root; window.makeKeyAndVisible()
        defer { window.isHidden = true }
        XCTAssertFalse(KeyboardPrefs.keyWindowIsCovered())
        root.present(UIViewController(), animated: false)
        for _ in 0..<100 where root.presentedViewController == nil { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertTrue(KeyboardPrefs.keyWindowIsCovered())
        root.dismiss(animated: false)
        for _ in 0..<100 where root.presentedViewController != nil { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertFalse(KeyboardPrefs.keyWindowIsCovered())
    }

    func testAKeyThePrefixTookIsNotALoneModifierTap() {
        let controller = enabled(), terminal = KeyCaptureView()
        terminal.tabShortcuts = controller
        terminal.shortcutSettings = ShortcutSettings(paletteChords: [KeyChord(keyCode: HIDKey.leftControl)])
        var input: [KeyItem] = []; terminal.onItems = { input += $0; return true }
        _ = terminal.handle(event("\t")); _ = terminal.handle(event("\t",phase:.up))
        let c = HIDKey.code(forCharacter: "c")!
        XCTAssertFalse(terminal.handle(KeyEventRecord(phase: .down, keyCode: HIDKey.leftControl, modifiers: [])))
        XCTAssertTrue(terminal.handle(KeyEventRecord(phase: .down, keyCode: c, modifiers: .control, characters: "c", charactersIgnoringModifiers: "c")))
        XCTAssertTrue(terminal.handle(KeyEventRecord(phase: .up, keyCode: c, modifiers: .control)))
        _ = terminal.handle(KeyEventRecord(phase: .up, keyCode: HIDKey.leftControl, modifiers: []))
        XCTAssertFalse(terminal.palette.isOpen, "Tab, then Ctrl-C is a chord the prefix took, not a tap on Ctrl")
        XCTAssertTrue(input.isEmpty)
    }

    func testATabKeyCommandArmsWithoutAHoldTimer() async {
        let controller = enabled()
        XCTAssertTrue(controller.handle(event("\t",phase:.command), perform:{_ in true}))
        XCTAssertTrue(controller.armed)
        try? await Task.sleep(for:.milliseconds(1050))
        XCTAssertFalse(controller.guideVisible, "no release follows a key command, so it never counts as held")
        controller.cancel()
    }

    func testComposerLeaderDoesNotInsertPrefixOrUnknownLettersAndPreservesMarkedText() {
        let controller = enabled(), composer = ChatComposerTextView()
        composer.tabShortcuts = controller; composer.text = "hello"
        composer.selectedRange = NSRange(location:5,length:0)
        _ = composer.handleTabShortcut(event("\t")); _ = composer.handleTabShortcut(event("\t",phase:.up))
        _ = composer.handleTabShortcut(event("x")); _ = composer.handleTabShortcut(event("x",phase:.up))
        XCTAssertEqual(composer.text,"hello"); XCTAssertTrue(controller.guideVisible)
        _ = composer.handleTabShortcut(event("q")); _ = composer.handleTabShortcut(event("q",phase:.up))
        XCTAssertEqual(composer.text,"hello\t")
        _ = composer.handleTabShortcut(event("\t")); _ = composer.handleTabShortcut(event("\t",phase:.up))
        composer.setMarkedText("に",selectedRange:NSRange(location:1,length:0))
        XCTAssertNotNil(composer.markedTextRange)
        XCTAssertFalse(composer.handleTabShortcut(event("e")))
        XCTAssertFalse(controller.armed)
        composer.unmarkText()
    }

    func testMenuAdapterMovesWithWASDAndTabQUsesItsNormalTabAction() {
        let controller = enabled(), menu = KeyCommandHost.HostView()
        menu.tabShortcuts = controller
        var moves: [String] = []
        menu.table.actions = [KeyAction(input:UIKeyCommand.inputDownArrow,run:{moves.append("down")}), KeyAction(input:"\t",run:{moves.append("tab")})]
        _ = menu.handleTabShortcut(event("\t")); _ = menu.handleTabShortcut(event("\t",phase:.up)); _ = menu.handleTabShortcut(event("s")); _ = menu.handleTabShortcut(event("s",phase:.up))
        _ = menu.handleTabShortcut(event("\t")); _ = menu.handleTabShortcut(event("\t",phase:.up)); _ = menu.handleTabShortcut(event("q"))
        XCTAssertEqual(moves,["down","tab"])
    }

    func testStandardOrSoftwareKeyboardNeverArmsTheLeader() {
        let controller = TabShortcutController(), terminal = KeyCaptureView()
        terminal.tabShortcuts = controller
        XCTAssertTrue(terminal.keyCommands?.contains { $0.input == "\t" && $0.modifierFlags.isEmpty } == true)
        XCTAssertFalse(terminal.handle(event("\t")))
        XCTAssertFalse(controller.armed)
    }

    func testClicksPreferenceEnablesOnlyAnAttachedPhysicalLayout() {
        let suite = "com.riwork.tests.tab-leader.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName:suite)!
        defer { defaults.removePersistentDomain(forName:suite) }
        let hardware = HardwareKeyboardMonitor(probe:{true},nameProbe:{"Clicks Keyboard"})
        let prefs = KeyboardPrefs(defaults:defaults,hardware:hardware)
        XCTAssertTrue(prefs.tabShortcuts.isEnabled())
        prefs.software.update(hiding:false,end:UIScreen.main.bounds)
        XCTAssertFalse(prefs.tabShortcuts.isEnabled(),"showing the software keyboard keeps the touch behavior")
        prefs.software.update(hiding:true,end:.zero)
        XCTAssertTrue(prefs.tabShortcuts.isEnabled())
        prefs.setLayout(.standard)
        XCTAssertFalse(prefs.tabShortcuts.isEnabled())
        prefs.setLayout(.clicks)
        XCTAssertTrue(prefs.tabShortcuts.isEnabled())
    }

    func testNewTerminalAdapterNavigatesWithoutCreatingUntilReturn() {
        let controller = enabled(), view = NewTerminalKeyView()
        view.tabShortcuts = controller
        var actions: [NewTerminalSheetKey] = []
        view.onKey = { actions.append($0) }
        _ = view.handleTabShortcut(event("\t")); _ = view.handleTabShortcut(event("\t",phase:.up)); _ = view.handleTabShortcut(event("s")); _ = view.handleTabShortcut(event("s",phase:.up))
        _ = view.handleTabShortcut(event("\t")); _ = view.handleTabShortcut(event("\t",phase:.up)); _ = view.handleTabShortcut(event("q")); _ = view.handleTabShortcut(event("q",phase:.up))
        XCTAssertEqual(actions,[.form(.down),.form(.tab)])
    }

    func testOnlyTheCurrentScreenOwnsAppActions() {
        let controller = enabled(), old = UUID(), current = UUID()
        var oldActions = 0, actions: [TabShortcutAction] = []
        controller.setAppHandler(owner:old) { _ in oldActions += 1 }
        controller.setAppHandler(owner:current) { actions.append($0) }
        controller.clearAppHandler(owner:old)
        tap("\t",controller:controller); tap("2",controller:controller,perform:{_ in false})
        XCTAssertEqual(oldActions,0); XCTAssertEqual(actions,[.selectTab(2)])
        controller.clearAppHandler(owner:current)
        XCTAssertFalse(controller.armed)
    }

    func testRootKeyHostDoesNotStealAnEditingSearchField() async {
        let window = UIWindow(frame:CGRect(x:0,y:0,width:393,height:852))
        let root = UIViewController(); window.rootViewController = root
        let search = UITextField(frame:CGRect(x:0,y:0,width:200,height:44))
        root.view.addSubview(search)
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        XCTAssertTrue(search.becomeFirstResponder())
        let host = KeyCommandHost.HostView()
        host.active = true; host.avoidTextInput = true
        root.view.addSubview(host)
        host.takeFocus()
        try? await Task.sleep(for:.milliseconds(30))
        XCTAssertTrue(search.isFirstResponder)
        XCTAssertFalse(host.isFirstResponder)
        _ = search.resignFirstResponder()
        host.takeFocus()
        try? await Task.sleep(for:.milliseconds(30))
        XCTAssertTrue(host.isFirstResponder,"the host still captures shortcuts when no text field owns focus")
    }

    func testActualHoldTimerShowsHelpButATapDoesNot() async {
        let controller = enabled()
        tap("\t",controller:controller)
        try? await Task.sleep(for:.milliseconds(1050))
        XCTAssertFalse(controller.guideVisible)
        controller.cancel()
        let pressed = ContinuousClock.now
        _ = controller.handle(event("\t"),perform:{_ in true})
        // The real one-second timer runs on the main actor, which a full run can keep busy past the second: wait for the guide, and
        // check that it came no sooner than the hold.
        let deadline = pressed + .seconds(5)
        while !controller.guideVisible, ContinuousClock.now < deadline { try? await Task.sleep(for:.milliseconds(10)) }
        XCTAssertTrue(controller.guideVisible)
        XCTAssertGreaterThanOrEqual(ContinuousClock.now - pressed, .seconds(1), "a held Tab shows help after one second, not before")
        _ = controller.handle(event("\t",phase:.up),perform:{_ in true})
        XCTAssertTrue(controller.guideVisible)
        controller.cancel()
    }
}
