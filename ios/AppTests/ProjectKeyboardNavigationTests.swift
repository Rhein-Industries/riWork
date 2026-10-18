import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

@MainActor final class ProjectKeyboardNavigationTests: XCTestCase {
    private var windows: [UIWindow] = []
    private var defaultsNames: [String] = []

    override func tearDown() async throws {
        windows.forEach { $0.isHidden = true }; windows = []
        defaultsNames.forEach { UserDefaults().removePersistentDomain(forName: $0) }; defaultsNames = []
    }
    private func eventually(_ message: String, _ condition: () -> Bool) async {
        for _ in 0..<100 {
            if condition() { return }
            try? await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertTrue(condition(), message)
    }
    private func descendants<T: UIView>(_ type: T.Type, in view: UIView) -> [T] {
        view.subviews.compactMap { $0 as? T } + view.subviews.flatMap { descendants(type, in: $0) }
    }
    private func project(_ number: Int, name: String) throws -> RemoteProject {
        let wire: JSONValue = .object(["id": .string(String(format: "11111111-1111-4111-8111-%012d", number)),
                                      "name": .string(name), "root": .string("/fixture/\(name)"), "created_at": .number(Double(number))])
        return try wire.decode(RemoteProject.self)
    }
    private final class Opened { var projects: [String] = []; var terminals: [String] = [] }
    private struct Screen {
        let model: RemoteModel
        let host: UIHostingController<AnyView>
        let keys: KeyCommandHost.HostView
        let opened: Opened
    }
    private func screen(clicks: Bool = false, hardware: Bool = true, active: Bool = true) async throws -> Screen {
        let suite = "com.riwork.tests.project-keys.\(UUID().uuidString)"
        defaultsNames.append(suite)
        let prefs = UserDefaults(suiteName: suite)!
        let monitor = HardwareKeyboardMonitor(probe: { hardware }, nameProbe: { clicks ? "Clicks Keyboard" : "Keyboard" })
        let model = RemoteModel(client: ActivityTransport(), keychain: KeychainStore(service: suite), defaults: prefs,
                                activityRefreshInterval: .seconds(3600), hardwareKeyboard: monitor)
        model.appActive = false // No desktop is contacted by a view fixture.
        model.state = .connected
        model.projects = try [project(1, name: "Alpha"), project(2, name: "Bravo"), project(3, name: "Charlie")]
        model.setProjectSort(.name)
        let opened = Opened()
        let view = ProjectSelectionView(model: model, onSelect: { opened.projects.append($0.id) },
                                        onNewTerminal: { opened.terminals.append($0.id) }, onNewProject: nil, shortcutsActive: active)
            .environment(\.tabShortcuts, model.keyboard.tabShortcuts)
            .desktopThemed(model.theme.style)
        let host = UIHostingController(rootView: AnyView(view))
        let window = UIWindow(windowScene: try XCTUnwrap(UIApplication.shared.connectedScenes.first as? UIWindowScene))
        window.frame = CGRect(x: 0, y: 0, width: 402, height: 852)
        window.rootViewController = host; window.makeKeyAndVisible(); windows.append(window)
        host.view.layoutIfNeeded()
        var keys: KeyCommandHost.HostView?
        await eventually("Project navigation key host is mounted") { keys = self.descendants(KeyCommandHost.HostView.self, in: host.view).first; return keys != nil }
        return Screen(model: model, host: host, keys: try XCTUnwrap(keys), opened: opened)
    }
    private func press(_ screen: Screen, _ input: String, _ modifiers: UIKeyModifierFlags = [], file: StaticString = #filePath, line: UInt = #line) async {
        guard let command = screen.keys.keyCommands?.first(where: { $0.input == input && $0.modifierFlags == modifiers }) else {
            return XCTFail("Project list does not offer \(input.debugDescription) \(modifiers.rawValue)", file: file, line: line)
        }
        screen.keys.fire(command)
        try? await Task.sleep(for: .milliseconds(20))
    }
    private func event(_ input: String, phase: KeyEventRecord.Phase = .down) -> KeyEventRecord {
        KeyEventRecord(phase: phase, keyCode: input == "\t" ? HIDKey.tab : HIDKey.code(forCharacter: input),
                       modifiers: [], characters: input, charactersIgnoringModifiers: input)
    }
    private func leader(_ screen: Screen, _ input: String) async {
        _ = screen.keys.handleTabShortcut(event("\t")); _ = screen.keys.handleTabShortcut(event("\t", phase: .up))
        _ = screen.keys.handleTabShortcut(event(input)); _ = screen.keys.handleTabShortcut(event(input, phase: .up))
        try? await Task.sleep(for: .milliseconds(20))
    }

    func testNativeArrowsTabAndReturnOpenTheHighlightedProjectAndKeepItsIdentityOnRefresh() async throws {
        let screen = try await screen()
        let first = screen.model.projects[0], second = screen.model.projects[1]
        await press(screen, UIKeyCommand.inputDownArrow)
        await press(screen, "\r")
        XCTAssertEqual(screen.opened.projects, [second.id])
        screen.model.projects = try [project(1, name: "Alpha refreshed"), project(2, name: "Bravo refreshed"), project(3, name: "Charlie")]
        try? await Task.sleep(for: .milliseconds(30))
        await press(screen, "\r")
        XCTAssertEqual(screen.opened.projects, [second.id, second.id])
        await press(screen, "\t", .shift)
        await press(screen, "\r")
        XCTAssertEqual(screen.opened.projects.last, first.id)
        await press(screen, "\t")
        await press(screen, "\r")
        XCTAssertEqual(screen.opened.projects.last, second.id)
    }

    func testClicksAliasesAndLeaderUseLocalProjectNavigationAndNewTerminalTarget() async throws {
        let screen = try await screen(clicks: true)
        let first = screen.model.projects[0], second = screen.model.projects[1], third = screen.model.projects[2]
        await press(screen, "s", .command)
        await press(screen, "\r")
        XCTAssertEqual(screen.opened.projects.last, second.id)
        await press(screen, "w", .command)
        await press(screen, "\r")
        XCTAssertEqual(screen.opened.projects.last, first.id)
        await leader(screen, "s")
        await leader(screen, "q")
        await leader(screen, "n")
        XCTAssertEqual(screen.opened.terminals, [third.id], "New terminal targets the highlighted project, not a stale model selection")
        await leader(screen, "1"); await leader(screen, "p"); await leader(screen, "m")
        XCTAssertEqual(screen.opened.terminals, [third.id], "Terminal/chat-only leader actions do not fall through to another screen")
    }

    func testCoveredOrTouchProjectListsDoNotClaimNavigationKeys() async throws {
        let covered = try await screen(clicks: true, active: false)
        XCTAssertFalse(covered.keys.active); XCTAssertTrue(covered.keys.table.actions.isEmpty)
        await leader(covered, "n")
        XCTAssertTrue(covered.opened.terminals.isEmpty)
        let touch = try await screen(hardware: false)
        XCTAssertFalse(touch.keys.active); XCTAssertTrue(touch.keys.table.actions.isEmpty)
    }

    func testCommandFFocusesSearchAndListRefreshKeepsTextAndComposition() async throws {
        let screen = try await screen(clicks: true)
        await eventually("Project key host owns the responder chain") { screen.keys.isFirstResponder }
        let command = try XCTUnwrap(screen.keys.keyCommands?.first { $0.input?.lowercased() == "f" && $0.modifierFlags == .command })
        XCTAssertTrue(UIApplication.shared.sendAction(try XCTUnwrap(command.action), to: nil, from: command, for: nil))
        var field: UITextField?
        await eventually("Command-F focuses project search") {
            field = self.descendants(UITextField.self, in: screen.host.view).first(where: \.isFirstResponder)
            return field != nil
        }
        let search = try XCTUnwrap(field)
        search.text = "br"; search.sendActions(for: .editingChanged)
        search.setMarkedText("に", selectedRange: NSRange(location: 1, length: 0))
        screen.model.projects = try [project(1, name: "Alpha refreshed"), project(2, name: "Bravo refreshed"), project(3, name: "Charlie")]
        try? await Task.sleep(for: .milliseconds(60))
        XCTAssertTrue(search.isFirstResponder)
        XCTAssertNotNil(search.markedTextRange)
        XCTAssertTrue(search.text?.contains("br") == true)
        XCTAssertFalse(screen.keys.isFirstResponder)
        XCTAssertTrue(screen.keys.avoidTextInput)
        search.unmarkText()
    }
}
