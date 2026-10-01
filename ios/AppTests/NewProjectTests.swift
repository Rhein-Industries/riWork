import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// A desktop that creates projects the way the connector does, and records what it was asked.
actor ProjectTransport: RemoteTransport {
    enum CreateMode { case ok, unsupported, alreadyExists, cliError(String), timeout, unreadable, wrongName }
    struct Call { let method: String; let params: [String: JSONValue] }
    var connected = false
    var calls: [Call] = []
    var createMode = CreateMode.ok
    var gated = false
    var projects: [RemoteProject]
    var created: [String] = []
    private var counter = 0

    init() {
        projects = [try! JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"11111111-1111-4111-8111-111111111111\",\"name\":\"Fixture\",\"root\":\"/fixture\",\"created_at\":1}".utf8))]
    }

    func setCreateMode(_ mode: CreateMode) { createMode = mode }
    func setGated(_ on: Bool) { gated = on }
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { connected = true; return pairing }
    func disconnect() async { connected = false }
    func isConnected() async -> Bool { connected }
    func count(_ method: String) -> Int { calls.filter { $0.method == method }.count }
    func params(of method: String) -> [[String: JSONValue]] { calls.filter { $0.method == method }.map(\.params) }
    func methods() -> [String] { calls.map(\.method) }

    private func encode<T: Encodable>(_ value: T) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(value)) }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        guard connected else { throw RemoteError.disconnected }
        try RequestValidation.validate(method: method, params: params, id: id)
        if method != "appearance.get" { calls.append(Call(method: method, params: params)) }
        switch method {
        case "projects.list": return .object(["projects": try encode(projects)])
        case "worktrees.list": return .object(["worktrees": .array([])])
        case "orchestrators.list": return .object(["orchestrators": .array([])])
        case "shells.list": return .object(["shells": .array([])])
        case "shell.resize.clear": return .object(["shell_id": params["shell_id"] ?? .null, "status": .string("cleared")])
        case "appearance.get": throw RemoteError.rpc(code: "not_found", message: "appearance not published")
        case "shell.create": throw RemoteError.protocolViolation("A terminal must not be created by a project flow")
        case "project.create":
            while gated { try await Task.sleep(for: .milliseconds(3)) }
            let name = params["name"]?.string ?? ""
            switch createMode {
            case .unsupported: throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")
            case .alreadyExists: throw RemoteError.rpc(code: "already_exists", message: "A project named “\(name)” already exists on the Mac.")
            case .cliError(let text): throw RemoteError.rpc(code: "cli_error", message: text)
            case .timeout: throw RemoteError.timeout
            case .ok, .unreadable, .wrongName:
                counter += 1
                let newID = String(format: "bbbbbbb%d-bbbb-4bbb-8bbb-bbbbbbbbbbbb", counter)
                let project = try JSONDecoder().decode(RemoteProject.self, from: Data("{\"id\":\"\(newID)\",\"name\":\"\(name)\",\"root\":\"/Users/me/Documents/riwork/\(name)\",\"created_at\":\(100 + counter)}".utf8))
                projects.append(project)
                created.append(newID)
                if case .unreadable = createMode { return .object(["status": .string("created")]) }
                var entry = try encode(project)
                if case .wrongName = createMode { entry = .object(["id": .string(newID), "name": .string("Other"), "root": .string("/r"), "created_at": .number(1)]) }
                return .object(["project_id": .string(newID), "project": entry])
            }
        default: throw RemoteError.protocolViolation("Unknown method \(method)")
        }
    }
}

@MainActor final class NewProjectAppTests: XCTestCase {
    private let fixture = "11111111-1111-4111-8111-111111111111"
    private var defaultsNames: [String] = []
    private var retained: [UIWindow] = []
    private static let commandShift: UIKeyModifierFlags = [.command, .shift]

    private func makeStore() throws -> KeychainStore {
        let keychain = KeychainStore(service: "com.riwork.tests.\(UUID().uuidString)")
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"33333333-3333-4333-8333-333333333333","device_name":"Test","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        var desktop = SavedDesktop(name: "Fixture", pairing: pairing, allowLocalDevelopment: false)
        desktop.selectedProjectID = fixture
        try keychain.write(Library(desktops: [desktop], selectedDesktopID: desktop.id))
        return keychain
    }
    private func defaults() -> UserDefaults {
        let name = "com.riwork.tests.newproject.\(UUID().uuidString)"
        defaultsNames.append(name)
        return UserDefaults(suiteName: name)!
    }
    override func tearDown() async throws {
        for name in defaultsNames { UserDefaults().removePersistentDomain(forName: name) }
        defaultsNames = []
    }
    private struct Rig { let model: RemoteModel; let transport: ProjectTransport }
    private func connected() async throws -> Rig {
        let transport = ProjectTransport()
        let model = RemoteModel(client: transport, keychain: try makeStore(), defaults: defaults())
        await model.connect()
        XCTAssertEqual(model.state, .connected)
        return Rig(model: model, transport: transport)
    }
    private func request(_ name: String = "Brand New", git: Bool = true) throws -> NewProjectRequest { try NewProjectRequest(name: name, git: git) }
    private func settle(_ condition: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async {
        let deadline = ContinuousClock.now + .seconds(5)
        while !condition(), ContinuousClock.now < deadline { try? await Task.sleep(for: .milliseconds(5)) }
        XCTAssertTrue(condition(), file: file, line: line)
    }
    private func sheet(_ rig: Rig, name: String? = nil) -> NewProjectSheetModel {
        let sheet = NewProjectSheetModel(model: rig.model)
        if let name { sheet.setName(name) }
        return sheet
    }

    // MARK: Creating

    func testCreatingAProjectSendsOneRequestAndPutsItInTheList() async throws {
        let rig = try await connected()
        let model = rig.model
        XCTAssertEqual(model.projects.map(\.name), ["Fixture"])
        XCTAssertEqual(model.projectCreation, .unknown)
        var created: [RemoteProject] = []
        let failure = await model.createProject(try request()) { created.append($0) }
        XCTAssertNil(failure)
        let sent = await rig.transport.params(of: "project.create")
        XCTAssertEqual(sent, [["name": .string("Brand New")]], "the name and nothing else; git is on by default")
        let ids = await rig.transport.created
        let newID = try XCTUnwrap(ids.first)
        XCTAssertEqual(created.map(\.id), [newID])
        XCTAssertEqual(created.first?.name, "Brand New")
        XCTAssertEqual(model.projects.map(\.id), [fixture, newID])
        XCTAssertEqual(model.projectCreation, .supported)
        XCTAssertFalse(model.creatingProject)
        // The list is read again afterwards, so the desktop's own order and entry win.
        let listed = await rig.transport.methods().filter { $0 == "projects.list" }.count
        XCTAssertGreaterThanOrEqual(listed, 2)
        // Nothing but the project was made: no terminal.
        let shells = await rig.transport.count("shell.create")
        XCTAssertEqual(shells, 0)
        await model.disconnect()
    }
    func testGitIsSentOnlyWhenItIsOff() async throws {
        let rig = try await connected()
        _ = await rig.model.createProject(try request("No Git", git: false))
        _ = await rig.model.createProject(try request("With Git", git: true))
        let sent = await rig.transport.params(of: "project.create")
        XCTAssertEqual(sent, [["name": .string("No Git"), "git": .bool(false)], ["name": .string("With Git")]])
        await rig.model.disconnect()
    }
    func testTheNewProjectIsSelectedLikeAnyOtherAndOffersNoTerminalOfItsOwn() async throws {
        let rig = try await connected()
        let model = rig.model
        var made: RemoteProject?
        let failure = await model.createProject(try request("Chained")) { made = $0 }
        XCTAssertNil(failure)
        let project = try XCTUnwrap(made)
        // What the list does with the project it is handed: open its screen, which chooses it...
        model.newTerminalRequestedProject = project.id
        await model.chooseProject(project.id)
        XCTAssertEqual(model.projectID, project.id)
        let loaded = await rig.transport.params(of: "worktrees.list")
        XCTAssertEqual(loaded.last, ["project_id": .string(project.id)], "its screen was read")
        // ...and the New terminal sheet that comes up is for it, with the usual defaults.
        let terminal = try XCTUnwrap(NewTerminalSheetModel(model: model))
        XCTAssertEqual(terminal.form.targets, [.project(id: project.id, name: "Chained")])
        XCTAssertEqual(terminal.form.kind, .shell)
        XCTAssertFalse(terminal.form.unrestricted)
        // Nobody pressed Create: no terminal was asked for.
        let shells = await rig.transport.count("shell.create")
        XCTAssertEqual(shells, 0)
        XCTAssertEqual(model.shells, [])
        await model.disconnect()
    }
    func testASecondRequestWhileOneIsOnTheWireIsRefusedNotSent() async throws {
        let rig = try await connected()
        await rig.transport.setGated(true)
        let model = rig.model
        let first = try request("One")
        let one = Task { await model.createProject(first) }
        await settle { model.creatingProject }
        let refused = await model.createProject(try request("Two"))
        XCTAssertEqual(refused, .busy)
        await rig.transport.setGated(false)
        let result = await one.value
        XCTAssertNil(result)
        let count = await rig.transport.count("project.create")
        XCTAssertEqual(count, 1)
        await model.disconnect()
    }
    func testNothingIsSentWhileDisconnected() async throws {
        let rig = try await connected()
        let sheet = sheet(rig, name: "Offline")
        await rig.model.disconnect()
        XCTAssertFalse(rig.model.canOpenNewProject)
        XCTAssertTrue(rig.model.offersNewProject, "dimmed, not hidden: it is only the link that is down")
        XCTAssertFalse(sheet.canCreate)
        let failure = await rig.model.createProject(try request())
        XCTAssertEqual(failure, .notConnected)
        let count = await rig.transport.count("project.create")
        XCTAssertEqual(count, 0)
    }

    // MARK: The sheet

    func testTheSheetSendsOnlyOneRequestForAHeldReturnOrADoubleTap() async throws {
        let rig = try await connected()
        let sheet = sheet(rig, name: "Held")
        var dismissed = 0, chained: [String] = []
        sheet.dismiss = { dismissed += 1 }
        sheet.onCreated = { chained.append($0.name) }
        await rig.transport.setGated(true)
        sheet.create(); sheet.create(); sheet.create()
        XCTAssertTrue(sheet.busy)
        XCTAssertFalse(sheet.canCreate, "the button is off while it runs")
        await rig.transport.setGated(false)
        await sheet.pending?.value
        let count = await rig.transport.count("project.create")
        XCTAssertEqual(count, 1)
        XCTAssertEqual(chained, ["Held"], "the screen moves on once")
        XCTAssertGreaterThanOrEqual(dismissed, 1)
        XCTAssertNil(sheet.error)
        XCTAssertFalse(sheet.busy)
        await rig.model.disconnect()
    }
    func testWhatIsTypedIsTrimmedAndTheCreatedProjectIsHandedOnBeforeTheSheetGoes() async throws {
        let rig = try await connected()
        let sheet = sheet(rig, name: "  Brand New \n")
        var order: [String] = []
        sheet.onCreated = { order.append("created \($0.name)") }
        sheet.dismiss = { order.append("dismissed") }
        sheet.create()
        await sheet.pending?.value
        let sent = await rig.transport.params(of: "project.create")
        XCTAssertEqual(sent, [["name": .string("Brand New")]])
        XCTAssertEqual(order.first, "created Brand New")
        XCTAssertEqual(order.dropFirst().first, "dismissed")
        await rig.model.disconnect()
    }
    func testANameTheDesktopWouldRefuseNeverLeavesThePhone() async throws {
        let rig = try await connected()
        let sheet = sheet(rig)
        var dismissed = 0
        sheet.dismiss = { dismissed += 1 }
        // Nothing typed: no complaint yet, but Create is off and Return says what is needed.
        XCTAssertNil(sheet.problem)
        XCTAssertFalse(sheet.canCreate)
        sheet.form.focus = .create
        sheet.create()
        XCTAssertEqual(sheet.problem, .empty)
        XCTAssertEqual(sheet.form.focus, .name, "back to the field")
        sheet.setName("a")
        XCTAssertNil(sheet.problem, "typing takes the complaint away")
        for (name, expected) in [("../x", NewProjectValidationError.separator), (".git", .leadingDot), ("-x", .leadingDash), ("a/b", .separator), ("a\\b", .separator),
                                 (String(repeating: "a", count: 101), .tooLong), ("a\u{1b}b", .controlCharacters)] {
            sheet.setName(name)
            XCTAssertEqual(sheet.problem, expected, name.debugDescription)
            XCTAssertFalse(sheet.canCreate)
            sheet.create()
        }
        let count = await rig.transport.count("project.create")
        XCTAssertEqual(count, 0)
        XCTAssertEqual(dismissed, 0)
        XCTAssertFalse(sheet.busy)
        await rig.model.disconnect()
    }
    func testFailuresKeepTheSheetOpenWithTheReason() async throws {
        let rig = try await connected()
        let sheet = sheet(rig, name: "Taken")
        var dismissed = 0, chained = 0
        sheet.dismiss = { dismissed += 1 }
        sheet.onCreated = { _ in chained += 1 }
        func attempt(_ mode: ProjectTransport.CreateMode) async -> String? {
            await rig.transport.setCreateMode(mode)
            sheet.create()
            await sheet.pending?.value
            return sheet.error?.message
        }
        let exists = await attempt(.alreadyExists)
        XCTAssertEqual(exists, "A project named “Taken” already exists on the Mac.")
        XCTAssertEqual(sheet.error, .alreadyExists("A project named “Taken” already exists on the Mac."))
        let cli = await attempt(.cliError("the installed riwork CLI cannot create projects from the phone; update RiWork"))
        XCTAssertEqual(cli, "the installed riwork CLI cannot create projects from the phone; update RiWork")
        let wrongName = await attempt(.wrongName)
        XCTAssertEqual(wrongName, ProjectCreateError.unreadableReply.message)
        let unreadable = await attempt(.unreadable)
        XCTAssertEqual(unreadable, ProjectCreateError.unreadableReply.message)
        XCTAssertEqual(dismissed, 0, "errors stay in the sheet")
        XCTAssertEqual(chained, 0, "and nothing moves on")
        XCTAssertFalse(sheet.busy)
        // An answer that was not understood may still have made the project: the list was read again, so it shows.
        XCTAssertTrue(rig.model.projects.contains { $0.name == "Taken" })
        // Editing the name takes the old message down.
        sheet.setName("Taken 2")
        XCTAssertNil(sheet.error)
        await rig.model.disconnect()
    }
    func testAnOlderDesktopHidesTheFeatureAndAReconnectTriesAgain() async throws {
        let rig = try await connected()
        let model = rig.model
        XCTAssertTrue(model.canOpenNewProject)
        XCTAssertTrue(model.offersNewProject)
        await rig.transport.setCreateMode(.unsupported)
        let sheet = sheet(rig, name: "Old")
        sheet.create()
        await sheet.pending?.value
        XCTAssertEqual(sheet.error, .unsupported)
        XCTAssertEqual(sheet.message?.message, "Update RiWork on your Mac to create projects from the phone.")
        XCTAssertEqual(model.projectCreation, .unsupported)
        XCTAssertFalse(model.offersNewProject, "the ＋ and ⌘⇧N are gone for this connection")
        XCTAssertFalse(model.canOpenNewProject)
        // No more requests go out, and the open sheet explains instead.
        let again = await model.createProject(try request())
        XCTAssertEqual(again, .unsupported)
        XCTAssertFalse(sheet.canCreate)
        sheet.create()
        let sent = await rig.transport.count("project.create")
        XCTAssertEqual(sent, 1)
        // A new connection may reach an upgraded desktop.
        await model.disconnect()
        await rig.transport.setCreateMode(.ok)
        await model.connect()
        XCTAssertEqual(model.projectCreation, .unknown)
        XCTAssertTrue(model.offersNewProject)
        XCTAssertTrue(model.canOpenNewProject)
        let retried = await model.createProject(try request())
        XCTAssertNil(retried)
        XCTAssertEqual(model.projectCreation, .supported)
        await model.disconnect()
    }
    func testALostAnswerLeavesTheOutcomeUnknownAndIsNeverRetried() async throws {
        let rig = try await connected()
        await rig.transport.setCreateMode(.timeout)
        let sheet = sheet(rig, name: "Maybe")
        sheet.create()
        await sheet.pending?.value
        XCTAssertEqual(sheet.error, .outcomeUnknown)
        XCTAssertEqual(sheet.error?.outcomeIsUncertain, true)
        XCTAssertTrue(sheet.error?.message.contains("Check the project list before trying again") == true)
        // Nothing sends it again by itself: not a reconnect, not time.
        try? await Task.sleep(for: .milliseconds(300))
        let count = await rig.transport.count("project.create")
        XCTAssertEqual(count, 1)
        XCTAssertEqual(rig.model.projects.map(\.name), ["Fixture"])
        // The person can ask again, and the desktop then says it exists (or makes it).
        await rig.transport.setCreateMode(.alreadyExists)
        sheet.create()
        await sheet.pending?.value
        XCTAssertEqual(sheet.error, .alreadyExists("A project named “Maybe” already exists on the Mac."))
        let asked = await rig.transport.count("project.create")
        XCTAssertEqual(asked, 2)
        await rig.model.disconnect()
    }

    // MARK: The keyboard

    func testKeysDriveTheSheetModelEndToEnd() async throws {
        let rig = try await connected()
        let sheet = sheet(rig)
        var chained: [String] = [], dismissed = 0
        sheet.onCreated = { chained.append($0.name) }
        sheet.dismiss = { dismissed += 1 }
        XCTAssertFalse(sheet.keyboardInUse)
        XCTAssertEqual(sheet.form.focus, .name, "typing starts at once")
        XCTAssertTrue(sheet.form.git, "Git is on")
        sheet.setName("keys")
        sheet.press(.tab)                                    // name -> Git switch
        XCTAssertEqual(sheet.form.focus, .git)
        XCTAssertTrue(sheet.keyboardInUse)
        sheet.press(.space)                                  // flips it
        XCTAssertFalse(sheet.form.git)
        sheet.press(.backTab)                                // and back to the name
        XCTAssertEqual(sheet.form.focus, .name)
        sheet.press(.tab); sheet.press(.tab)                 // -> Create
        XCTAssertEqual(sheet.form.focus, .create)
        sheet.press(.space)                                  // space presses the focused Create button
        await sheet.pending?.value
        let sent = await rig.transport.params(of: "project.create")
        XCTAssertEqual(sent, [["name": .string("keys"), "git": .bool(false)]])
        XCTAssertEqual(chained, ["keys"])
        XCTAssertGreaterThanOrEqual(dismissed, 1)
        await rig.model.disconnect()
    }
    func testReturnCreatesFromAnywhereAndCommandGFlipsGitWhileTyping() async throws {
        let rig = try await connected()
        let sheet = sheet(rig, name: "from the switch")
        sheet.toggleGitFromKeyboard()                        // ⌘G, with the ring still on the name
        XCTAssertFalse(sheet.form.git)
        XCTAssertEqual(sheet.form.focus, .name, "typing goes on")
        sheet.toggleGitFromKeyboard()
        XCTAssertTrue(sheet.form.git)
        sheet.press(.tab)                                    // the switch has the ring; Return is create
        sheet.create()
        await sheet.pending?.value
        let sent = await rig.transport.params(of: "project.create")
        XCTAssertEqual(sent, [["name": .string("from the switch")]])
        await rig.model.disconnect()
    }
    func testEscapeCancelsWithoutSendingAnything() async throws {
        let rig = try await connected()
        let sheet = sheet(rig, name: "never sent")
        var closed = false
        sheet.dismiss = { closed = true }
        sheet.cancel()
        XCTAssertTrue(closed)
        let count = await rig.transport.count("project.create")
        XCTAssertEqual(count, 0)
        await rig.model.disconnect()
    }
    func testTouchTakesTheFocusRingAwayAndFlipsLikeTheKeyboard() async throws {
        let rig = try await connected()
        let sheet = sheet(rig)
        sheet.press(.tab)
        XCTAssertTrue(sheet.keyboardInUse)
        sheet.setGit(false)
        XCTAssertFalse(sheet.keyboardInUse)
        XCTAssertFalse(sheet.form.git)
        XCTAssertEqual(sheet.form.focus, .git)
        sheet.nameFieldFocused()
        XCTAssertEqual(sheet.form.focus, .name, "a tap on the field moves the ring to it")
        await rig.model.disconnect()
    }

    private func window(with view: UIView) -> UIWindow {
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 480))
        window.rootViewController = UIViewController()
        window.rootViewController?.view.addSubview(view)
        window.makeKeyAndVisible()
        retained.append(window)
        return window
    }
    private func tuples(_ commands: [UIKeyCommand]) -> [String] { commands.map { "\($0.input?.debugDescription ?? "nil")|\($0.modifierFlags.rawValue)" } }

    func testTheNameFieldKeepsTypingAndTakesTabEscapeAndTheCommandKeys() throws {
        let field = NewProjectNameField(frame: CGRect(x: 0, y: 0, width: 200, height: 30))
        let commands = try XCTUnwrap(field.keyCommands)
        func command(_ input: String, _ flags: UIKeyModifierFlags = []) -> UIKeyCommand? { commands.first { $0.input == input && $0.modifierFlags == flags } }
        let expected: [(String, UIKeyModifierFlags)] = [("\t", []), ("\t", .shift), (UIKeyCommand.inputUpArrow, []), (UIKeyCommand.inputDownArrow, []),
                                                        (UIKeyCommand.inputEscape, []), (".", .command), ("\r", .command), ("g", .command)]
        for (input, flags) in expected {
            let found = try XCTUnwrap(command(input, flags), "\(input.debugDescription) \(flags)")
            XCTAssertTrue(found.wantsPriorityOverSystemBehavior, "the system must not use \(input.debugDescription) first")
        }
        XCTAssertEqual(commands.count, expected.count)
        // Letters, space, backspace, return and the caret keys are the text field's own.
        let typing: [String] = [" ", "\r", UIKeyCommand.inputLeftArrow, UIKeyCommand.inputRightArrow, "a", "\u{8}"]
        for input in typing {
            XCTAssertNil(command(input), "\(input.debugDescription) is typing")
        }
        // The app's and the hotkey menu's shortcuts are not taken.
        XCTAssertFalse(commands.contains { $0.modifierFlags.contains(.command) && ["n", "k", ","].contains($0.input) })
        XCTAssertTrue(field.canBecomeFirstResponder)
    }
    func testEachKeyReachesTheSheetAsWhatItMeans() throws {
        let field = NewProjectNameField(frame: .zero)
        var received: [NewProjectSheetKey] = []
        field.onKey = { received.append($0) }
        let table: [(String, UIKeyModifierFlags, NewProjectSheetKey)] = [
            ("\t", [], .form(.tab)), ("\t", .shift, .form(.backTab)), (UIKeyCommand.inputUpArrow, [], .form(.up)), (UIKeyCommand.inputDownArrow, [], .form(.down)),
            (UIKeyCommand.inputEscape, [], .cancel), (".", .command, .cancel), ("\r", .command, .create), ("g", .command, .toggleGit)
        ]
        for (input, flags, _) in table { field.fired(UIKeyCommand(input: input, modifierFlags: flags, action: #selector(NewProjectNameField.fired(_:)))) }
        XCTAssertEqual(received, table.map(\.2))
        // Anything else is ignored.
        field.fired(UIKeyCommand(input: "x", modifierFlags: .command, action: #selector(NewProjectNameField.fired(_:))))
        XCTAssertEqual(received.count, table.count)
    }
    func testTheControlsKeyViewHasEverythingTheNameFieldHasPlusSpaceReturnAndTheArrows() throws {
        let view = NewProjectKeyView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        let commands = try XCTUnwrap(view.keyCommands)
        func command(_ input: String, _ flags: UIKeyModifierFlags = []) -> UIKeyCommand? { commands.first { $0.input == input && $0.modifierFlags == flags } }
        let expected: [(String, UIKeyModifierFlags)] = [("\t", []), ("\t", .shift), (UIKeyCommand.inputUpArrow, []), (UIKeyCommand.inputDownArrow, []),
                                                        (UIKeyCommand.inputLeftArrow, []), (UIKeyCommand.inputRightArrow, []), (" ", []), ("\r", []),
                                                        (UIKeyCommand.inputEscape, []), (".", .command), ("\r", .command), ("g", .command)]
        for (input, flags) in expected {
            let found = try XCTUnwrap(command(input, flags), "\(input.debugDescription) \(flags)")
            XCTAssertTrue(found.wantsPriorityOverSystemBehavior)
        }
        XCTAssertEqual(commands.count, expected.count)
        XCTAssertFalse(commands.contains { $0.input?.lowercased() == "n" }, "⌘N and ⌘⇧N are left to the app")
        XCTAssertTrue(view.canBecomeFirstResponder)
        XCTAssertFalse(view is UIKeyInput, "no software keyboard comes up for it")
        var received: [NewProjectSheetKey] = []
        view.onKey = { received.append($0) }
        let plain: [String] = [" ", "\r", UIKeyCommand.inputLeftArrow, UIKeyCommand.inputRightArrow]
        for input in plain {
            view.fired(UIKeyCommand(input: input, modifierFlags: [], action: #selector(NewProjectKeyView.fired(_:))))
        }
        XCTAssertEqual(received, [.form(.space), .create, .form(.left), .form(.right)])
    }
    func testTheNameFieldTakesTheKeyboardAsSoonAsItIsOnScreenAndHandsItOverAndBack() async throws {
        let field = NewProjectNameField(frame: CGRect(x: 0, y: 0, width: 200, height: 30))
        let keys = NewProjectKeyView(frame: CGRect(x: 0, y: 40, width: 1, height: 1))
        field.wantsFocus = true
        let host = window(with: field)
        host.rootViewController?.view.addSubview(keys)
        await settle { field.isFirstResponder }
        // The ring moves to the switch: the key view gets the keyboard.
        field.wantsFocus = false
        keys.wantsFocus = true
        keys.takeFocus()
        await settle { keys.isFirstResponder }
        XCTAssertFalse(field.isFirstResponder)
        // And back to the name.
        keys.wantsFocus = false
        field.wantsFocus = true
        field.takeFocus()
        await settle { field.isFirstResponder }
        XCTAssertFalse(keys.isFirstResponder)
    }

    // MARK: The sheet on screen

    private func find<T: UIView>(_ type: T.Type, in view: UIView) -> T? {
        if let match = view as? T { return match }
        for child in view.subviews { if let match = find(type, in: child) { return match } }
        return nil
    }
    private func present(_ sheet: NewProjectSheetModel) async -> UIViewController {
        let host = UIHostingController(rootView: NewProjectSheet(sheet: sheet))
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 390, height: 700))
        window.rootViewController = host
        window.makeKeyAndVisible()
        retained.append(window)
        host.view.layoutIfNeeded()
        return host
    }
    func testTheSheetOnScreenStartsOnTheNameFieldAndTheKeyboardFollowsTheRing() async throws {
        let rig = try await connected()
        let sheet = sheet(rig)
        let host = await present(sheet)
        // Up with no tap: the name field has the keyboard, and it is the one the sheet's commands ride on.
        await settle { self.find(NewProjectNameField.self, in: host.view)?.isFirstResponder == true }
        let field = try XCTUnwrap(find(NewProjectNameField.self, in: host.view))
        let keys = try XCTUnwrap(find(NewProjectKeyView.self, in: host.view))
        XCTAssertFalse(keys.isFirstResponder)
        XCTAssertEqual(field.autocorrectionType, .no)
        XCTAssertEqual(field.autocapitalizationType, .none)
        XCTAssertEqual(field.smartQuotesType, .no)
        XCTAssertEqual(field.smartDashesType, .no)
        // Tab: the Git switch has the ring and the key view the keyboard (space, return, escape reach the sheet).
        field.fired(UIKeyCommand(input: "\t", modifierFlags: [], action: #selector(NewProjectNameField.fired(_:))))
        XCTAssertEqual(sheet.form.focus, .git)
        await settle { keys.isFirstResponder }
        XCTAssertFalse(field.isFirstResponder)
        keys.fired(UIKeyCommand(input: " ", modifierFlags: [], action: #selector(NewProjectKeyView.fired(_:))))
        XCTAssertFalse(sheet.form.git)
        // Shift-Tab: back to the name, typing works at once.
        keys.fired(UIKeyCommand(input: "\t", modifierFlags: .shift, action: #selector(NewProjectKeyView.fired(_:))))
        XCTAssertEqual(sheet.form.focus, .name)
        await settle { field.isFirstResponder }
        XCTAssertFalse(keys.isFirstResponder)
        await rig.model.disconnect()
    }
    func testTypingAndReturnInTheNameFieldReachTheSheet() async throws {
        let rig = try await connected()
        let sheet = sheet(rig)
        var dismissed = 0
        sheet.dismiss = { dismissed += 1 }
        let host = await present(sheet)
        await settle { self.find(NewProjectNameField.self, in: host.view)?.isFirstResponder == true }
        let field = try XCTUnwrap(find(NewProjectNameField.self, in: host.view))
        // Typed text reaches the model, trimmed for the live check.
        field.text = "  My Project "
        field.sendActions(for: .editingChanged)
        XCTAssertEqual(sheet.form.name, "  My Project ")
        XCTAssertEqual(sheet.form.check, .valid("My Project"))
        field.text = "bad/name"
        field.sendActions(for: .editingChanged)
        XCTAssertEqual(sheet.problem, .separator, "shown as it is typed")
        // A pasted line break is dropped, not typed.
        field.text = ""
        let delegate = try XCTUnwrap(field.delegate)
        let allowed = delegate.textField?(field, shouldChangeCharactersIn: NSRange(location: 0, length: 0), replacementString: "one\ntwo") ?? true
        XCTAssertFalse(allowed)
        XCTAssertEqual(field.text, "onetwo")
        field.sendActions(for: .editingChanged)
        XCTAssertEqual(sheet.form.name, "onetwo")
        XCTAssertTrue(delegate.textField?(field, shouldChangeCharactersIn: NSRange(location: 6, length: 0), replacementString: "x") ?? false, "ordinary typing is left alone")
        // Return, from the hardware or the software keyboard, creates.
        field.text = "From Return"
        field.sendActions(for: .editingChanged)
        let returned = delegate.textFieldShouldReturn?(field) ?? true
        XCTAssertFalse(returned)
        await sheet.pending?.value
        let sent = await rig.transport.params(of: "project.create")
        XCTAssertEqual(sent, [["name": .string("From Return")]])
        XCTAssertGreaterThanOrEqual(dismissed, 1)
        await rig.model.disconnect()
    }

    // MARK: ⌘⇧N

    private func hosted(_ rig: Rig, onNewProject: (() -> Void)?, active: Bool = true) async -> UIHostingController<ProjectSelectionView> {
        let view = ProjectSelectionView(model: rig.model, onSelect: { _ in }, onNewTerminal: { _ in }, onNewProject: onNewProject, shortcutsActive: active)
        let host = UIHostingController(rootView: view)
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 480))
        window.rootViewController = host
        window.makeKeyAndVisible()
        retained.append(window)
        host.view.layoutIfNeeded()
        try? await Task.sleep(for: .milliseconds(150))
        return host
    }
    private func hasCommandShiftN(_ host: UIViewController) -> Bool {
        (host.keyCommands ?? []).contains { $0.input?.lowercased() == "n" && $0.modifierFlags == Self.commandShift }
    }
    func testCommandShiftNIsAKeyCommandOfTheProjectListAndNotOfAnythingElse() async throws {
        let rig = try await connected()
        var opened = 0
        let host = await hosted(rig, onNewProject: { opened += 1 })
        let all = (host.keyCommands ?? []).map { "\($0.input ?? "")|\($0.modifierFlags.rawValue)" }
        XCTAssertTrue(hasCommandShiftN(host), "keyCommands: \(all)")
        XCTAssertEqual(opened, 0)
        // ⌘N, the terminal's, is not claimed here.
        XCTAssertFalse((host.keyCommands ?? []).contains { $0.input?.lowercased() == "n" && $0.modifierFlags == .command })
        await rig.model.disconnect()
    }
    func testTheShortcutIsOffForAnOlderDesktopAndWhileAnotherScreenOrSheetIsOnTop() async throws {
        let rig = try await connected()
        // The list does not offer it at all (an older desktop): no command.
        let none = await hosted(rig, onNewProject: nil)
        XCTAssertFalse(hasCommandShiftN(none))
        // Another screen is on top of the list (a terminal), or a sheet is over it.
        let covered = await hosted(rig, onNewProject: {}, active: false)
        XCTAssertFalse(hasCommandShiftN(covered), "keyCommands: \((covered.keyCommands ?? []).map { "\($0.input ?? "")|\($0.modifierFlags.rawValue)" })")
        await rig.model.disconnect()
    }
    func testTheTerminalsKeyViewDoesNotClaimCommandShiftN() throws {
        let view = KeyCaptureView(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        let claims: () -> Bool = { (view.keyCommands ?? []).contains { $0.input?.lowercased() == "n" && $0.modifierFlags == Self.commandShift } }
        XCTAssertFalse(claims())
        view.togglePalette()
        XCTAssertFalse(claims(), "not even with the hotkey menu open")
        // The reserved shortcuts the new one must not collide with.
        let reserved = (view.keyCommands ?? []).filter { $0.modifierFlags == .command }.compactMap(\.input)
        XCTAssertTrue(reserved.contains("k") && reserved.contains(","))
        view.togglePalette()
    }
}
