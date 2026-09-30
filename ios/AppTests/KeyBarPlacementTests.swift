import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// The real screen with a real keyboard state: the bar where iOS puts it, its ends padded, and the terminal never behind it.
/// A simulator cannot attach a hardware keyboard, so that state is emulated the way iOS lays it out: no keys, the bar alone.
@MainActor final class KeyBarPlacementTests: XCTestCase {
    private let project = "11111111-1111-4111-8111-111111111111"
    private let shell = "44444444-4444-4444-8444-444444444444"
    /// The strip under the terminal while the keyboard is down: `directBar` is one compact 40 pt control tall.
    private let directBarHeight = 40.0

    private func descendants<T: UIView>(_ type: T.Type, in view: UIView) -> [T] {
        view.subviews.compactMap { $0 as? T } + view.subviews.flatMap { descendants(type, in: $0) }
    }
    private func eventually(_ what: String, timeout: Double = 4, _ condition: () async -> Bool) async {
        let end = Date().addingTimeInterval(timeout)
        while await !condition(), Date() < end { try? await Task.sleep(for: .milliseconds(20)) }
        let met = await condition()
        XCTAssertTrue(met, what)
    }
    private struct Rig {
        let model: RemoteModel, window: UIWindow, host: UIHostingController<AnyView>, capture: KeyCaptureView, keychain: KeychainStore
    }
    private func makeRig() async throws -> Rig {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("no window scene to show a keyboard in") }
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = project; desktop.selectedSessionID = shell
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        let suite = "com.riwork.tests.\(UUID().uuidString)"
        let model = RemoteModel(client: FixtureTransport(), keychain: keychain, defaults: UserDefaults(suiteName: suite)!)
        await model.connect()
        let projectValue = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(project)\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))
        let root = AnyView(TerminalTabsView(model: model, project: projectValue, onBack: {}).desktopThemed(model.theme.style))
        let host = UIHostingController(rootView: root)
        let window = UIWindow(windowScene: scene)
        window.rootViewController = host
        window.makeKeyAndVisible()
        await eventually("the terminal is on screen") { !self.descendants(KeyCaptureView.self, in: host.view).isEmpty && model.terminalArea != nil }
        let capture = try XCTUnwrap(descendants(KeyCaptureView.self, in: host.view).first)
        return Rig(model: model, window: window, host: host, capture: capture, keychain: keychain)
    }
    private func tearDown(_ rig: Rig) async {
        _ = rig.capture.resignFirstResponder()
        await rig.model.disconnect()
        rig.window.isHidden = true
        try? rig.keychain.delete()
    }
    private func barFrame(_ rig: Rig) -> CGRect { rig.capture.bar.convert(rig.capture.bar.bounds, to: rig.window) }
    /// Where the terminal pane's bottom edge is, worked out from how much its height changed. Before the keyboard the pane ends
    /// above the bottom safe area and the "tap to type" strip.
    private func paneBottom(_ rig: Rig, baseline: CGSize) -> Double {
        let before = rig.window.bounds.height - rig.window.safeAreaInsets.bottom - directBarHeight
        return before + (rig.model.terminalArea?.height ?? 0) - baseline.height
    }

    func testAloneAtTheBottomTheBarKeepsItsPlaceItsEndsClearTheCornersAndTheTerminalStaysAboveIt() async throws {
        let rig = try await makeRig()
        let baseline = try XCTUnwrap(rig.model.terminalArea)
        // A simulator that has a hardware keyboard connected shows exactly this state by itself; otherwise it is emulated by
        // replacing the software keyboard with an empty view.
        XCTAssertTrue(rig.capture.becomeFirstResponder())
        try await Task.sleep(for: .milliseconds(800))
        if barFrame(rig).maxY < rig.window.bounds.height - 1 {
            _ = rig.capture.resignFirstResponder()
            try await Task.sleep(for: .milliseconds(500))
            rig.capture.inputViewStandIn = UIView(frame: CGRect(x: 0, y: 0, width: 1, height: 0.001))
            XCTAssertTrue(rig.capture.becomeFirstResponder())
        }
        let bar = rig.capture.bar
        await eventually("the bar settled at the bottom") { self.barFrame(rig).maxY >= rig.window.bounds.height - rig.window.safeAreaInsets.bottom && bar.position == .screenBottom }
        try await Task.sleep(for: .milliseconds(400))
        let frame = barFrame(rig)
        XCTAssertEqual(frame.height, 44, "the bar is not taller than it is above a keyboard")
        XCTAssertGreaterThanOrEqual(frame.maxY, rig.window.bounds.height - rig.window.safeAreaInsets.bottom, "and it is where iOS puts it: at the bottom, in the home-indicator zone")
        let clearance = KeyBarGeometry.cornerClearance(safeArea: KeyBarInsets(left: 0, bottom: rig.window.safeAreaInsets.bottom, right: 0))
        XCTAssertGreaterThan(rig.window.safeAreaInsets.bottom, 0, "the simulator is an iPhone with a home indicator")
        XCTAssertEqual(bar.padding.left, clearance)
        XCTAssertEqual(bar.padding.right, clearance)
        let esc = try XCTUnwrap(bar.buttons[.key(.escape)]), hide = try XCTUnwrap(bar.buttons[.hide])
        XCTAssertEqual(esc.convert(esc.bounds, to: rig.window).minX, CGFloat(clearance), accuracy: 0.5)
        XCTAssertEqual(hide.convert(hide.bounds, to: rig.window).maxX, rig.window.bounds.width - CGFloat(clearance), accuracy: 0.5)
        // The terminal ends where the bar begins: nothing of it is hidden behind the bar, and no room is wasted above it.
        await eventually("the pane made room for the bar") { self.paneBottom(rig, baseline: baseline) <= frame.minY + 1 }
        XCTAssertEqual(paneBottom(rig, baseline: baseline), frame.minY, accuracy: 3)
        await tearDown(rig)
    }
    func testAboveTheSoftwareKeyboardTheRowStartsAtTheEdgeAndTheTerminalStaysAboveTheBar() async throws {
        let rig = try await makeRig()
        let baseline = try XCTUnwrap(rig.model.terminalArea)
        var keyboardHeight = 0.0
        let token = NotificationCenter.default.addObserver(forName: UIResponder.keyboardWillChangeFrameNotification, object: nil, queue: .main) { note in
            keyboardHeight = Double((note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? CGRect)?.height ?? 0)
        }
        defer { NotificationCenter.default.removeObserver(token) }
        XCTAssertTrue(rig.capture.becomeFirstResponder())
        await eventually("a keyboard appeared") { keyboardHeight > 0 }
        try await Task.sleep(for: .milliseconds(700))
        let bar = rig.capture.bar
        if barFrame(rig).maxY >= rig.window.bounds.height - 1 {
            await tearDown(rig)
            throw XCTSkip("this simulator has a hardware keyboard connected, so there is no software keyboard to sit on")
        }
        XCTAssertEqual(bar.position, .aboveKeyboard)
        XCTAssertEqual(bar.padding, .zero)
        XCTAssertEqual(barFrame(rig).height, 44)
        XCTAssertEqual(barFrame(rig).minY, rig.window.bounds.height - keyboardHeight, accuracy: 0.5, "the bar tops the keyboard")
        let esc = try XCTUnwrap(bar.buttons[.key(.escape)])
        XCTAssertEqual(esc.convert(esc.bounds, to: rig.window).minX, 0, accuracy: 0.5)
        XCTAssertEqual(paneBottom(rig, baseline: baseline), barFrame(rig).minY, accuracy: 3)
        await tearDown(rig)
    }
}
