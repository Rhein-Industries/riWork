import XCTest
import SwiftUI
import UIKit
import GameController
import RiWorkCore
@testable import RiWorkRemote

/// The terminal taking the keyboard by itself once a shell is ready, and giving it up to sheets, other fields and the person.
@MainActor final class KeyboardFocusFlowTests: XCTestCase {
    private var windows: [UIWindow] = []
    private func eventually(_ what: String, timeout: Double = 4, _ condition: () async -> Bool) async {
        let end = Date().addingTimeInterval(timeout)
        while await !condition(), Date() < end { try? await Task.sleep(for: .milliseconds(20)) }
        let met = await condition()
        XCTAssertTrue(met, what)
    }

    // MARK: The focus object on its own

    private struct Bare { let view: KeyCaptureView, focus: KeyFocus, window: UIWindow }
    private func bare(_ setting: KeyboardFocusSetting, hardware: Bool, canType: Bool = true) -> Bare {
        let view = KeyCaptureView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        let focus = KeyFocus()
        focus.view = view
        focus.shellID = "a"
        view.onUserHide = { focus.noteUserHide() }
        view.onActiveChange = { focus.isActive = $0 }
        let current = (setting: setting, hardware: hardware, canType: canType)
        focus.policy = { current }
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 480))
        window.rootViewController = UIViewController()
        window.rootViewController?.view.addSubview(view)
        window.makeKeyAndVisible()
        windows.append(window)
        return Bare(view: view, focus: focus, window: window)
    }

    func testAShellThatBecomesReadyTakesTheKeyboardUnderAlwaysWithOrWithoutAHardwareKeyboard() {
        for hardware in [false, true] {
            let rig = bare(.always, hardware: hardware)
            XCTAssertFalse(rig.view.isFirstResponder)
            rig.focus.shellReady("a")
            XCTAssertTrue(rig.view.isFirstResponder, "hardware \(hardware)")
            XCTAssertTrue(rig.focus.isActive)
            _ = rig.view.resignFirstResponder()
        }
    }
    func testTheDefaultFocusesOnlyWithAHardwareKeyboard() {
        let without = bare(.hardwareKeyboard, hardware: false)
        without.focus.shellReady("a")
        XCTAssertFalse(without.view.isFirstResponder, "no software keyboard is popped up")
        let with = bare(.hardwareKeyboard, hardware: true)
        with.focus.shellReady("a")
        XCTAssertTrue(with.view.isFirstResponder)
        _ = with.view.resignFirstResponder()
    }
    func testNeverNeverFocuses() {
        let rig = bare(.never, hardware: true)
        rig.focus.shellReady("a")
        XCTAssertFalse(rig.view.isFirstResponder)
        XCTAssertEqual(rig.focus.autoFocus(), .skip(.setting))
    }
    func testAShellThatIsNotReadyOrCannotTakeDirectTypingNeverFocuses() {
        let rig = bare(.always, hardware: true)
        XCTAssertEqual(rig.focus.autoFocus(), .skip(.cannotType), "no live screen yet")
        rig.focus.shellReady(nil)
        XCTAssertFalse(rig.view.isFirstResponder)
        let composer = bare(.always, hardware: true, canType: false)
        composer.focus.shellReady("a")
        XCTAssertFalse(composer.view.isFirstResponder, "line composer or a dead shell")
    }
    func testAnotherShellOrAReconnectIsANewMomentButTheSameShellIsNot() {
        let rig = bare(.always, hardware: true)
        rig.focus.shellReady("a")
        _ = rig.view.resignFirstResponder()
        rig.focus.shellReady("a")
        XCTAssertFalse(rig.view.isFirstResponder, "still the same ready shell: nothing new")
        rig.focus.shellReady(nil)
        rig.focus.shellReady("a")
        XCTAssertTrue(rig.view.isFirstResponder, "back from a dropped connection or the background")
        _ = rig.view.resignFirstResponder()
        rig.focus.shellID = "b"
        rig.focus.shellReady("b")
        XCTAssertTrue(rig.view.isFirstResponder, "another shell chosen")
        _ = rig.view.resignFirstResponder()
    }
    func testAKeyboardThePersonHidStaysHiddenInThatShellUntilTheyTapTheTerminal() {
        let rig = bare(.always, hardware: true)
        rig.focus.shellReady("a")
        XCTAssertTrue(rig.view.isFirstResponder)
        rig.view.bar.tapped(.hide)
        XCTAssertFalse(rig.view.isFirstResponder)
        rig.focus.shellReady(nil)
        rig.focus.shellReady("a")
        XCTAssertFalse(rig.view.isFirstResponder, "reconnecting does not fight the person")
        XCTAssertEqual(rig.focus.autoFocus(), .skip(.dismissedByUser))
        rig.focus.shellID = "b"; rig.focus.shellReady("b")
        XCTAssertTrue(rig.view.isFirstResponder, "another shell is another matter")
        _ = rig.view.resignFirstResponder()
        rig.focus.shellID = "a"; rig.focus.shellReady("a")
        XCTAssertFalse(rig.view.isFirstResponder, "and back in the first one it is still hidden")
        rig.focus.focus()
        XCTAssertTrue(rig.view.isFirstResponder, "a tap asks for it")
        _ = rig.view.resignFirstResponder()
        rig.focus.shellReady(nil); rig.focus.shellReady("a")
        XCTAssertTrue(rig.view.isFirstResponder, "and from then on it is automatic again")
        _ = rig.view.resignFirstResponder()
    }
    func testHidingFromTheBarBelowTheTerminalIsTheSameChoice() {
        let rig = bare(.always, hardware: true)
        rig.focus.shellReady("a")
        rig.focus.userDismiss()
        XCTAssertFalse(rig.view.isFirstResponder)
        XCTAssertEqual(rig.focus.autoFocus(), .skip(.dismissedByUser))
    }
    func testASheetIsNotStolenFromAndTheKeyboardComesBackWhenItGoes() async throws {
        let rig = bare(.always, hardware: true)
        rig.focus.shellReady("a")
        _ = rig.view.resignFirstResponder()
        let sheet = UIViewController()
        let root = try XCTUnwrap(rig.window.rootViewController)
        root.present(sheet, animated: false)
        await eventually("the sheet is up") { root.presentedViewController === sheet }
        XCTAssertEqual(rig.focus.autoFocus(), .skip(.obscured))
        XCTAssertFalse(rig.view.isFirstResponder, "a sheet has the screen")
        root.dismiss(animated: false)
        await eventually("the terminal takes the keyboard once the sheet is gone", timeout: 2) { rig.view.isFirstResponder }
        _ = rig.view.resignFirstResponder()
    }
    func testAnotherTextFieldKeepsTheKeyboard() {
        let rig = bare(.always, hardware: true)
        let field = UITextField(frame: CGRect(x: 0, y: 50, width: 200, height: 30))
        rig.window.rootViewController?.view.addSubview(field)
        XCTAssertTrue(field.becomeFirstResponder())
        rig.focus.shellReady("a")
        XCTAssertTrue(field.isFirstResponder, "a field being typed in is not taken from")
        XCTAssertFalse(rig.view.isFirstResponder)
        XCTAssertEqual(rig.focus.autoFocus(retries: 0), .skip(.obscured))
        _ = field.resignFirstResponder()
    }
    func testAnEditorSheetTakesTheKeyboardAndReturnsItToTheShell() {
        let rig = bare(.never, hardware: true)
        rig.focus.shellReady("a")
        rig.focus.focus()
        XCTAssertTrue(rig.view.isFirstResponder)
        rig.focus.suspendForModal()
        XCTAssertFalse(rig.view.isFirstResponder)
        rig.focus.restoreAfterModalDismissal()
        XCTAssertTrue(rig.view.isFirstResponder, "even under Never: it had the keyboard before the editor")
        _ = rig.view.resignFirstResponder()
        rig.focus.suspendForModal()
        rig.focus.restoreAfterModalDismissal()
        XCTAssertFalse(rig.view.isFirstResponder, "and if it did not have it, the editor does not give it")
    }
    func testAShellThatReplacedAClosedOneWaitsForATap() {
        let rig = bare(.always, hardware: true)
        rig.focus.shellReady("a")
        rig.focus.shellID = "next"
        rig.focus.shellReplacedWithoutTap()
        XCTAssertFalse(rig.view.isFirstResponder, "the keyboard goes away with the closed shell")
        rig.focus.shellReady("next")
        XCTAssertFalse(rig.view.isFirstResponder, "keys meant for one shell must not flow into one nobody picked")
        rig.focus.focus()
        XCTAssertTrue(rig.view.isFirstResponder)
        _ = rig.view.resignFirstResponder()
    }

    // MARK: The hardware keyboard monitor and the settings

    func testTheMonitorFollowsTheKeyboardComingAndGoing() async {
        var attached = false
        let monitor = HardwareKeyboardMonitor(probe: { attached })
        XCTAssertFalse(monitor.isAttached)
        attached = true
        NotificationCenter.default.post(name: .GCKeyboardDidConnect, object: nil)
        await eventually("attached") { monitor.isAttached }
        attached = false
        NotificationCenter.default.post(name: .GCKeyboardDidDisconnect, object: nil)
        await eventually("detached") { !monitor.isAttached }
    }
    func testTheFocusSettingAndTheReadoutAreRememberedAndTheDefaultIsHardwareOnly() {
        let name = "com.riwork.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defer { defaults.removePersistentDomain(forName: name) }
        let prefs = KeyboardPrefs(defaults: defaults, hardware: HardwareKeyboardMonitor(probe: { false }))
        XCTAssertEqual(prefs.focusSetting, .hardwareKeyboard)
        XCTAssertFalse(prefs.showKeyEvents)
        prefs.setFocusSetting(.always); prefs.setShowKeyEvents(true)
        let again = KeyboardPrefs(defaults: defaults, hardware: HardwareKeyboardMonitor(probe: { false }))
        XCTAssertEqual(again.focusSetting, .always)
        XCTAssertTrue(again.showKeyEvents)
        defaults.set("nonsense", forKey: KeyboardPrefs.focusKey)
        XCTAssertEqual(KeyboardPrefs(defaults: defaults).focusSetting, .hardwareKeyboard, "a damaged value is the default")
    }
    func testTheEventLogKeepsTheLastEventAndACount() {
        let log = KeyEventLog()
        XCTAssertNil(log.last)
        log.record(KeyEventRecord(phase: .down, keyCode: 4, modifiers: []))
        log.record(KeyEventRecord(phase: .up, keyCode: 4, modifiers: []))
        XCTAssertEqual(log.count, 2)
        XCTAssertEqual(log.last?.phase, .up)
    }

    // MARK: The real screen

    private struct Rig { let model: RemoteModel, window: UIWindow, host: UIHostingController<AnyView>, capture: KeyCaptureView, keychain: KeychainStore }
    private let project = "11111111-1111-4111-8111-111111111111"
    private let shell = "44444444-4444-4444-8444-444444444444"
    private func descendants<T: UIView>(_ type: T.Type, in view: UIView) -> [T] {
        view.subviews.compactMap { $0 as? T } + view.subviews.flatMap { descendants(type, in: $0) }
    }
    private func screen(setting: KeyboardFocusSetting?, hardware: Bool, connect: Bool = true) async throws -> Rig {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene to show a keyboard in") }
        try XCTSkipIf(UIDevice.current.userInterfaceIdiom == .pad, "the iPhone terminal")
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = shell
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        let suite = "com.riwork.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        if let setting { defaults.set(setting.rawValue, forKey: KeyboardPrefs.focusKey) }
        let model = RemoteModel(client: FixtureTransport(), keychain: keychain, defaults: defaults, hardwareKeyboard: HardwareKeyboardMonitor(probe: { hardware }))
        if connect { await model.connect() }
        let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))
        let host = UIHostingController(rootView: AnyView(TerminalTabsView(model: model, project: projectValue, onBack: {}).desktopThemed(model.theme.style)))
        let window = UIWindow(windowScene: scene)
        window.rootViewController = host
        window.makeKeyAndVisible()
        await eventually("the terminal is on screen") { !self.descendants(KeyCaptureView.self, in: host.view).isEmpty }
        let capture = try XCTUnwrap(descendants(KeyCaptureView.self, in: host.view).first)
        return Rig(model: model, window: window, host: host, capture: capture, keychain: keychain)
    }
    private func finish(_ rig: Rig) async {
        _ = rig.capture.resignFirstResponder()
        await rig.model.disconnect()
        rig.window.isHidden = true
        try? rig.keychain.delete()
    }

    func testOnTheRealScreenAReadyShellTakesTheKeyboardUnderAlwaysAndDefaultsToWaitingForAHardwareKeyboard() async throws {
        let always = try await screen(setting: .always, hardware: false)
        await eventually("the first live screen is shown") { always.model.outputSessionID == self.shell }
        await eventually("typing goes straight into the shell") { always.capture.isFirstResponder }
        await finish(always)

        let byDefault = try await screen(setting: nil, hardware: false)
        await eventually("the first live screen is shown") { byDefault.model.outputSessionID == self.shell }
        try await Task.sleep(for: .milliseconds(600))
        XCTAssertFalse(byDefault.capture.isFirstResponder, "no hardware keyboard: the software keyboard is not popped up by itself")
        await finish(byDefault)

        let hardware = try await screen(setting: nil, hardware: true)
        await eventually("with a hardware keyboard the default focuses") { hardware.capture.isFirstResponder }
        await finish(hardware)

        let never = try await screen(setting: .never, hardware: true)
        await eventually("the first live screen is shown") { never.model.outputSessionID == self.shell }
        try await Task.sleep(for: .milliseconds(600))
        XCTAssertFalse(never.capture.isFirstResponder)
        await finish(never)
    }
    func testOnTheRealScreenAKeyboardHiddenOnPurposeStaysHiddenThroughAReconnect() async throws {
        let rig = try await screen(setting: .always, hardware: true)
        await eventually("focused when ready") { rig.capture.isFirstResponder }
        rig.capture.bar.tapped(.hide)
        XCTAssertFalse(rig.capture.isFirstResponder)
        await rig.model.disconnect()
        await rig.model.connect()
        await eventually("the shell is ready again") { rig.model.outputSessionID == self.shell && rig.model.state == .connected }
        try await Task.sleep(for: .milliseconds(600))
        XCTAssertFalse(rig.capture.isFirstResponder, "the person put it away; a reconnect does not bring it back")
        await finish(rig)
    }
    func testOnTheRealScreenAReconnectBringsTheKeyboardBackWhenItWasNotHidden() async throws {
        let rig = try await screen(setting: .always, hardware: true)
        await eventually("focused when ready") { rig.capture.isFirstResponder }
        _ = rig.capture.resignFirstResponder()   // iOS took it, for instance on leaving the app
        await rig.model.disconnect()
        await rig.model.connect()
        await eventually("focused again when the shell is ready again") { rig.capture.isFirstResponder }
        await finish(rig)
    }
    func testOnTheRealScreenTheEditorTakesTheKeyboardAndGivesItBack() async throws {
        let rig = try await screen(setting: .always, hardware: true)
        await eventually("focused when ready") { rig.capture.isFirstResponder }
        rig.capture.bar.tapped(.editHotkeys)
        await eventually("the editor is up") { rig.host.presentedViewController != nil }
        XCTAssertFalse(rig.capture.isFirstResponder, "the editor has the keyboard")
        rig.host.dismiss(animated: false)
        await eventually("the shell has the keyboard again") { rig.capture.isFirstResponder }
        await finish(rig)
    }
    func testOnTheRealScreenTheMenuOpensFromTheKeyboardAndFocusStaysOnTheShell() async throws {
        let rig = try await screen(setting: .always, hardware: true)
        await eventually("focused when ready") { rig.capture.isFirstResponder }
        let open = try XCTUnwrap(rig.capture.keyCommands?.first { $0.input == "k" && $0.modifierFlags == .command })
        rig.capture.keyCommandFired(open)
        await eventually("the menu is drawn") { !self.descendants(UIView.self, in: rig.host.view).isEmpty && rig.capture.palette.isOpen }
        XCTAssertTrue(rig.capture.isFirstResponder, "the menu is driven by the shell's own keyboard focus")
        rig.capture.insertText("config"); rig.capture.insertText("\n")
        await eventually("the editor is up") { rig.host.presentedViewController != nil }
        rig.host.dismiss(animated: false)
        await eventually("focus returned to the shell") { rig.capture.isFirstResponder }
        XCTAssertFalse(rig.capture.palette.isOpen)
        await finish(rig)
    }
}
