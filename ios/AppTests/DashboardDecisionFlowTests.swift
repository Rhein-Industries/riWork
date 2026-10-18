import XCTest
import SwiftUI
import UIKit
import RiWorkCore
@testable import RiWorkRemote

/// A scripted transport whose final write binds and records on one actor turn.
/// Reads reuse the shared chat fixture; no socket, key or real desktop is involved.
private actor DashboardDecisionTransport: RemoteTransport {
    let base: ChatTransport
    private var identity: UploadConnectionIdentity?
    private var replaceBeforeSend: UploadConnectionIdentity?
    private var commandFailure: RemoteError?
    private var unreadable = false
    private var closeAfterSnapshot = false
    private(set) var writes: [(UploadConnectionIdentity, [String: JSONValue])] = []
    private(set) var unboundWrites = 0
    init(_ base: ChatTransport) { self.base = base }
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing {
        identity = UploadConnectionIdentity(pairing: pairing)
        return try await base.connect(pairing: pairing, allowLocalDevelopment: allowLocalDevelopment)
    }
    func disconnect() async { identity = nil; await base.disconnect() }
    func isConnected() async -> Bool { identity != nil }
    func desktopFeatures() async -> DesktopFeatures { await base.desktopFeatures() }
    func swapAtSend(to pairing: Pairing) { replaceBeforeSend = UploadConnectionIdentity(pairing: pairing) }
    func failCommand(_ error: RemoteError) { commandFailure = error }
    func unreadableCommandReply() { unreadable = true }
    func closeConnectionAfterSnapshot() { closeAfterSnapshot = true }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        if method == "chat.command" { unboundWrites += 1; throw RemoteError.protocolViolation("A dashboard response must bind its host.") }
        let reply = try await base.request(method: method, params: params, id: id)
        if method == "chat.snapshot", closeAfterSnapshot { identity = nil }
        return reply
    }
    func request(method: String, params: [String: JSONValue], id: String, boundTo expected: UploadConnectionIdentity) async throws -> JSONValue {
        try RequestValidation.validate(method: method, params: params, id: id)
        if let replacement = replaceBeforeSend { identity = replacement; replaceBeforeSend = nil }
        guard identity == expected else { throw CancellationError() }
        // No await between checking the identity and recording the sealed request.
        writes.append((expected, params))
        if let failure = commandFailure { commandFailure = nil; throw failure }
        return .object(["status": .string(unreadable ? "unexpected" : "ok")])
    }
}

private actor DashboardLandingTransport: RemoteTransport {
    private let reply: JSONValue
    private var connected = false, offline = false
    private(set) var methods: [String] = []
    init(reply: JSONValue) { self.reply = reply }
    func setOffline() { offline = true }
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing {
        guard !offline else { throw RemoteError.timeout }; connected = true; return pairing
    }
    func disconnect() async { connected = false }
    func isConnected() async -> Bool { connected }
    func desktopFeatures() async -> DesktopFeatures { DesktopFeatures(ready: .object(["features": .object(["dashboard": .bool(true)])])) }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        guard connected, !offline else { throw RemoteError.disconnected }
        methods.append(method)
        guard method == "dashboard.get" else { throw RemoteError.protocolViolation("The landing overview may only read dashboards.") }
        return reply
    }
}

@MainActor final class DashboardDecisionFlowTests: XCTestCase {
    private let chatID = "cccccccc-1111-4111-8111-111111111111"
    private let projectID = ChatTransport.project
    private var suites: [String] = []
    private var windows: [UIWindow] = []

    override func setUp() async throws { executionTimeAllowance = 60 }
    override func tearDown() async throws {
        windows.forEach { $0.isHidden = true }; windows = []
        suites.forEach { UserDefaults().removePersistentDomain(forName: $0) }; suites = []
    }
    private func desktop(_ n: Int, name: String) throws -> SavedDesktop {
        let route = String(format: "33333333-3333-4333-8333-%012d", n)
        let pairing = try Pairing.parse("""
        {"v":1,"relay_url":"wss://example.com/v1/ws","desktop_id":"11111111-1111-4111-8111-111111111111","device_id":"22222222-2222-4222-8222-222222222222","route_id":"\(route)","device_name":"Fixture","pairing_secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","relay_token":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}
        """)
        return SavedDesktop(name: name, pairing: pairing, allowLocalDevelopment: false)
    }
    private var approval: ChatApproval { ChatApproval(requestID: "approve-build", kind: .command, title: "Run the simulator tests?", detail: "xcodebuild test -scheme RiWorkRemote", choices: [.accept, .decline]) }
    private var question: ChatQuestion {
        ChatQuestion(requestID: "choose-preview", questions: [ChatQuestionPrompt(header: "Preview", question: "Which preview should the agent test?", options: [ChatQuestionOption(label: "iPhone preview", description: "Test touch input in the simulator"), ChatQuestionOption(label: "Desktop preview", description: "Test the GPUI app")])])
    }
    private func json<T: Encodable>(_ value: T) throws -> JSONValue { try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(value)) }
    private func dashboard(question: Bool = false) throws -> JSONValue {
        let request = question ? self.question.requestID : approval.requestID
        let session: JSONValue = .object(["target": .object(["type": .string("chat"), "id": .string(chatID), "project_id": .string(projectID)]), "agent": .string("Claude"), "title": .string("Preview pipeline"), "project_name": .string("RiWork"), "is_chat": .bool(true)])
        let decision: JSONValue = .object(["kind": .string(question ? "question" : "approval"), "chat_id": .string(chatID), "request_id": .string(request), "observed_at": .number(1_800_000_000), "session": session,
            "source": .object(["chat_id": .string(chatID), "event_seq": .number(3), "event_id": .string("chat:\(chatID):seq:3")]),
            question ? "question" : "approval": question ? try json(self.question) : try json(approval)])
        return .object(["v": .number(1), "generated_at": .number(1_800_000_000), "chats_known": .bool(true),
            "totals": .object(["projects": .number(1), "attention": .number(1), "agents": .object(["working": .number(1), "waiting": .number(1)]), "tasks": .object(["todo": .number(2), "in_progress": .number(1)])]),
            "projects": .array([.object(["id": .string(projectID), "name": .string("RiWork"), "agents": .object(["working": .number(1)]), "tasks": .object(["todo": .number(2), "in_progress": .number(1)])])]),
            "working": .array([.object(["session": session, "activity": .string("working")])]),
            "decisions": .array([decision]), "decisions_total": .number(1), "decision_sources": .object(["state": .string("read")])])
    }
    private func decision(question: Bool = false) throws -> DashboardDecision {
        try XCTUnwrap(DashboardSnapshot(wire: dashboard(question: question)).decisions.rows.first)
    }
    private struct Rig { let model: RemoteModel; let transport: DashboardDecisionTransport; let base: ChatTransport; let first: SavedDesktop; let second: SavedDesktop }
    private func rig(question: Bool = false) async throws -> Rig {
        let first = try desktop(1, name: "MacBook Pro"), second = try desktop(2, name: "Mac Studio")
        let base = ChatTransport()
        await base.enableSnapshots()
        let transport = DashboardDecisionTransport(base)
        _ = try await transport.connect(pairing: first.pairing, allowLocalDevelopment: false)
        let suite = "com.riwork.tests.decisions.\(UUID().uuidString)"; suites.append(suite)
        let model = RemoteModel(client: transport, keychain: KeychainStore(service: suite), defaults: UserDefaults(suiteName: suite)!)
        model.desktops = [first, second]; model.selectedDesktopID = first.id; model.state = .connected
        let info = ChatInfo(id: chatID, provider: .claude, projectID: projectID, cwd: "/fixture", title: "Preview pipeline", createdAtUnix: 1, state: .waiting)
        await base.append(chatID, [.info(info), question ? .questionRequested(self.question) : .approvalRequested(approval), .state(.waiting)])
        return Rig(model: model, transport: transport, base: base, first: first, second: second)
    }
    private func eventually(_ condition: () async -> Bool, file: StaticString = #filePath, line: UInt = #line) async {
        let until = Date().addingTimeInterval(4)
        while !(await condition()), Date() < until { try? await Task.sleep(for: .milliseconds(5)) }
        let value = await condition(); XCTAssertTrue(value, file: file, line: line)
    }

    func testFreshWaitingDecisionSendsExactlyOneCommandBoundToItsHost() async throws {
        let rig = try await rig(), decision = try decision()
        try await rig.model.respondToDashboardDecision(decision, hostID: rig.first.id, command: .approve(requestID: decision.requestID, decision: .accept))
        let writes = await rig.transport.writes, unbound = await rig.transport.unboundWrites
        XCTAssertEqual(writes.count, 1); XCTAssertEqual(unbound, 0)
        XCTAssertEqual(writes.first?.0, UploadConnectionIdentity(pairing: rig.first.pairing))
        XCTAssertEqual(writes.first?.1["chat_id"], .string(chatID))
        let command = try writes.first?.1["command"]?.decode(ChatCommand.self)
        XCTAssertEqual(command, .approve(requestID: decision.requestID, decision: .accept))
        let reads = await rig.base.count("chat.snapshot"); XCTAssertEqual(reads, 1)
    }

    func testWrongHostResolvedRequestAndChangedProviderBodySendNothing() async throws {
        let rig = try await rig(), decision = try decision()
        do { try await rig.model.respondToDashboardDecision(decision, hostID: rig.second.id, command: .approve(requestID: decision.requestID, decision: .accept)); XCTFail("wrong host") }
        catch { XCTAssertEqual(error as? DashboardResponseError, .hostUnavailable) }
        let before = await rig.base.count("chat.snapshot"); XCTAssertEqual(before, 0)
        var changed = approval; changed.title = "A different command with the same request ID"
        await rig.base.append(chatID, [.approvalRequested(changed), .state(.waiting)])
        do { try await rig.model.respondToDashboardDecision(decision, hostID: rig.first.id, command: .approve(requestID: decision.requestID, decision: .accept)); XCTFail("changed body") }
        catch { XCTAssertEqual(error as? DashboardResponseError, .staleRequest) }
        await rig.base.append(chatID, [.approvalResolved(requestID: decision.requestID, decision: .decline), .state(.idle)])
        do { try await rig.model.respondToDashboardDecision(decision, hostID: rig.first.id, command: .approve(requestID: decision.requestID, decision: .accept)); XCTFail("resolved request") }
        catch { XCTAssertEqual(error as? DashboardResponseError, .staleRequest) }
        let writes = await rig.transport.writes; XCTAssertTrue(writes.isEmpty)
    }

    func testHostGenerationChangesDuringHeldSnapshotSendNothing() async throws {
        let rig = try await rig(), decision = try decision()
        await rig.base.gateSnapshots(true)
        let response = Task { try await rig.model.respondToDashboardDecision(decision, hostID: rig.first.id, command: .approve(requestID: decision.requestID, decision: .accept)) }
        await eventually { await rig.base.count("chat.snapshot") == 1 }
        rig.model.generation = UUID(); rig.model.selectedDesktopID = rig.second.id
        _ = try await rig.transport.connect(pairing: rig.second.pairing, allowLocalDevelopment: false)
        await rig.base.gateSnapshots(false)
        do { try await response.value; XCTFail("changed host") }
        catch { XCTAssertEqual(error as? DashboardResponseError, .changedHost) }
        let writes = await rig.transport.writes; XCTAssertTrue(writes.isEmpty)
    }

    func testTransportRejectsARouteSwapAfterSnapshotValidation() async throws {
        let rig = try await rig(), decision = try decision()
        await rig.transport.swapAtSend(to: rig.second.pairing)
        do { try await rig.model.respondToDashboardDecision(decision, hostID: rig.first.id, command: .approve(requestID: decision.requestID, decision: .accept)); XCTFail("transport route changed") }
        catch {
            // The transport refused before sealing: nothing went out, so this is no "may have gone through".
            XCTAssertEqual(error as? DashboardResponseError, .changedHost)
            XCTAssertNotEqual((error as? ChatControlError)?.outcomeIsUncertain, true)
        }
        let writes = await rig.transport.writes; XCTAssertTrue(writes.isEmpty)
    }

    func testFailuresBeforeTheCommandIsHandedOverAreDefinite() async throws {
        // The snapshot check cannot be read: nothing was sent.
        let unreadable = try await rig(), decision = try decision()
        await unreadable.base.enableSnapshots(false)
        do { try await unreadable.model.respondToDashboardDecision(decision, hostID: unreadable.first.id, command: .approve(requestID: decision.requestID, decision: .accept)); XCTFail("no snapshot") }
        catch {
            guard case .notSent? = error as? DashboardResponseError else { return XCTFail("\(error)") }
            XCTAssertTrue(error.localizedDescription.hasPrefix("Nothing was sent."))
        }
        // The connection closes after the check but before the command is handed to it.
        let closed = try await rig()
        await closed.transport.closeConnectionAfterSnapshot()
        do { try await closed.model.respondToDashboardDecision(decision, hostID: closed.first.id, command: .approve(requestID: decision.requestID, decision: .accept)); XCTFail("closed") }
        catch { guard case .notSent? = error as? DashboardResponseError else { return XCTFail("\(error)") } }
        let first = await unreadable.transport.writes, second = await closed.transport.writes
        XCTAssertTrue(first.isEmpty); XCTAssertTrue(second.isEmpty)
    }

    func testAnotherHostIsAnsweredOverItsOwnConnectionAndTheHostInUseIsLeftAlone() async throws {
        let rig = try await rig(), decision = try decision()
        rig.model.draft = "Unsent draft for the host in use"
        // The second host's chat, reached only over the overview's kind of temporary connection.
        let otherBase = ChatTransport()
        await otherBase.enableSnapshots()
        let info = ChatInfo(id: chatID, provider: .claude, projectID: projectID, cwd: "/fixture", title: "Preview pipeline", createdAtUnix: 1, state: .waiting)
        await otherBase.append(chatID, [.info(info), .approvalRequested(approval), .state(.waiting)])
        let other = DashboardDecisionTransport(otherBase)
        let store = HostDashboardStore(refreshInterval: .seconds(3600), transportFactory: { _ in other })
        try await store.respond(to: decision, on: rig.second, command: .approve(requestID: decision.requestID, decision: .accept))
        let writes = await other.writes, connected = await other.isConnected(), interactive = await rig.transport.writes
        XCTAssertEqual(writes.count, 1)
        XCTAssertEqual(writes.first?.0, UploadConnectionIdentity(pairing: rig.second.pairing), "bound to the card's own host")
        XCTAssertFalse(connected, "the temporary connection is closed afterwards")
        XCTAssertTrue(interactive.isEmpty)
        XCTAssertEqual(rig.model.selectedDesktopID, rig.first.id)
        XCTAssertEqual(rig.model.state, .connected)
        XCTAssertEqual(rig.model.draft, "Unsent draft for the host in use")
        let snapshots = await rig.base.count("chat.snapshot"); XCTAssertEqual(snapshots, 0, "the interactive host is not even read")
        // The same checks hold over it: a request answered meanwhile is refused, and nothing more goes out.
        await otherBase.append(chatID, [.approvalResolved(requestID: decision.requestID, decision: .accept), .state(.idle)])
        do { try await store.respond(to: decision, on: rig.second, command: .approve(requestID: decision.requestID, decision: .accept)); XCTFail("resolved") }
        catch { XCTAssertEqual(error as? DashboardResponseError, .staleRequest) }
        let after = await other.writes; XCTAssertEqual(after.count, 1)
    }

    func testUncertainAndUnreadableRepliesAreNotRetried() async throws {
        for unreadable in [false, true] {
            let rig = try await rig(), decision = try decision()
            if unreadable { await rig.transport.unreadableCommandReply() } else { await rig.transport.failCommand(.uncertainDelivery) }
            do { try await rig.model.respondToDashboardDecision(decision, hostID: rig.first.id, command: .approve(requestID: decision.requestID, decision: .accept)); XCTFail("uncertain result") }
            catch { XCTAssertTrue((error as? ChatControlError)?.outcomeIsUncertain == true) }
            for _ in 0..<10 { await Task.yield() }
            let writes = await rig.transport.writes, reads = await rig.base.count("chat.snapshot")
            XCTAssertEqual(writes.count, 1); XCTAssertEqual(reads, 1)
        }
    }

    // MARK: Real SwiftUI screens, hosted with the Native palette

    private func show<V: View>(_ view: V, dark: Bool = false) throws -> UIHostingController<AnyView> {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene else { throw XCTSkip("No test window scene") }
        let palette = DesktopPalette(
            bg: RGB(dark ? 0x000000 : 0xffffff), panel: RGB(dark ? 0x1c1c1e : 0xf5f5f7),
            panelActive: RGB(dark ? 0x2c2c2e : 0xe8e8ed), divider: RGB(dark ? 0x3a3a3c : 0xd2d2d7),
            cyan: RGB(dark ? 0xffffff : 0x000000), magenta: RGB(dark ? 0xc7c7cc : 0x3a3a3c),
            gold: RGB(dark ? 0xff9f0a : 0xb34000), text: RGB(dark ? 0xf5f5f7 : 0x1d1d1f), muted: RGB(dark ? 0x98989d : 0x636366))
        let theme = DesktopTheme.resolve(DesktopAppearance(updatedAt: 1_800_000_000, dark: dark, palette: palette, native: true))
        let host = UIHostingController(rootView: AnyView(view.desktopThemed(DesktopStyle(theme))))
        let window = UIWindow(windowScene: scene); window.frame = CGRect(x: 0, y: 0, width: 402, height: 874)
        host.overrideUserInterfaceStyle = dark ? .dark : .light; window.rootViewController = host; window.makeKeyAndVisible()
        host.view.frame = window.bounds; host.view.layoutIfNeeded(); windows.append(window)
        return host
    }
    private func picture(_ host: UIViewController, _ name: String) throws {
        host.view.layoutIfNeeded()
        let image = UIGraphicsImageRenderer(bounds: host.view.bounds).image { _ in host.view.drawHierarchy(in: host.view.bounds, afterScreenUpdates: true) }
        let data = try XCTUnwrap(image.pngData()); XCTAssertGreaterThan(data.count, 10_000)
        let directory = URL(fileURLWithPath: "/tmp/riwork-hosts-ui", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try data.write(to: directory.appendingPathComponent(name + ".png"))
        let attachment = XCTAttachment(image: image); attachment.name = name; attachment.lifetime = .keepAlways; add(attachment)
    }
    private func elements(in view: UIView) -> [NSObject] {
        var seen: Set<ObjectIdentifier> = [], remaining = 4_000, result: [NSObject] = []
        func visit(_ element: NSObject) {
            guard remaining > 0, seen.insert(ObjectIdentifier(element)).inserted, (element as? UIView)?.isHidden != true else { return }
            remaining -= 1; result.append(element)
            ((element.accessibilityElements as? [NSObject]) ?? []).forEach(visit)
            (element as? UIView)?.subviews.forEach(visit)
        }
        visit(view); return result
    }
    private func activate(_ label: String, in view: UIView) -> Bool {
        elements(in: view).first { $0.accessibilityLabel == label && $0.accessibilityTraits.contains(.button) }?.accessibilityActivate() ?? false
    }
    private func enableAccessibility() -> () -> Void {
        typealias Get = @convention(c) () -> Bool
        typealias Set = @convention(c) (Bool) -> Void
        let library = dlopen("/usr/lib/libAccessibility.dylib", RTLD_NOW)
        let get = dlsym(library, "_AXSApplicationAccessibilityEnabled").map { unsafeBitCast($0, to: Get.self) }
        let set = dlsym(library, "_AXSApplicationAccessibilitySetEnabled").map { unsafeBitCast($0, to: Set.self) }
        let previous = get?() ?? false; set?(true)
        return { set?(previous) }
    }

    func testDecisionFormsShowProviderChoicesAndOnlySubmitAfterExplicitPress() async throws {
        let restore = enableAccessibility(); defer { restore() }
        for isQuestion in [false, true] {
            let rig = try await rig(question: isQuestion), decision = try decision(question: isQuestion)
            let item = HostedDashboardDecision(hostID: rig.first.id, hostName: rig.first.name, decision: decision)
            let host = try show(DashboardDecisionSheet(item: item) { command in
                try await rig.model.respondToDashboardDecision(decision, hostID: rig.first.id, command: command)
            })
            try await Task.sleep(for: .milliseconds(150))
            let before = await rig.transport.writes; XCTAssertTrue(before.isEmpty)
            try picture(host, isQuestion ? "decision-question-native" : "decision-approval-native")
            let labels = elements(in: host.view).compactMap(\.accessibilityLabel).joined(separator: " ")
            XCTAssertTrue(labels.contains(isQuestion ? "Which preview should the agent test?" : "Run the simulator tests?"))
            if isQuestion {
                XCTAssertTrue(labels.contains("iPhone preview")); XCTAssertTrue(labels.contains("Desktop preview"))
                XCTAssertTrue(activate("iPhone preview", in: host.view))
                await eventually { self.elements(in: host.view).contains { $0.accessibilityLabel == "Send answer" && !$0.accessibilityTraits.contains(.notEnabled) } }
                let selected = await rig.transport.writes; XCTAssertTrue(selected.isEmpty, "choosing an option is not submitting")
                XCTAssertTrue(activate("Send answer", in: host.view))
            } else {
                XCTAssertTrue(labels.contains("Deny")); XCTAssertTrue(activate("Allow", in: host.view))
            }
            await eventually { await rig.transport.writes.count == 1 }
            await eventually { self.elements(in: host.view).contains { $0.accessibilityLabel?.contains("Response sent") == true } }
            let writes = await rig.transport.writes; XCTAssertEqual(writes.count, 1)
            let command = try XCTUnwrap(writes.first?.1["command"]).decode(ChatCommand.self)
            if isQuestion { XCTAssertEqual(command, .answer(requestID: decision.requestID, answers: [["iPhone preview"]])) }
            else { XCTAssertEqual(command, .approve(requestID: decision.requestID, decision: .accept)) }
            try picture(host, isQuestion ? "decision-question-sent" : "decision-approval-sent")
            windows.last?.isHidden = true
        }
    }

    func testUncertainDecisionSheetDisablesResubmission() async throws {
        let restore = enableAccessibility(); defer { restore() }
        let rig = try await rig(), decision = try decision()
        await rig.transport.failCommand(.uncertainDelivery)
        let item = HostedDashboardDecision(hostID: rig.first.id, hostName: rig.first.name, decision: decision)
        let host = try show(DashboardDecisionSheet(item: item) { command in
            try await rig.model.respondToDashboardDecision(decision, hostID: rig.first.id, command: command)
        })
        try await Task.sleep(for: .milliseconds(150))
        XCTAssertTrue(activate("Allow", in: host.view))
        await eventually { await rig.transport.writes.count == 1 }
        await eventually { self.elements(in: host.view).contains { $0.accessibilityLabel == "Allow" && $0.accessibilityTraits.contains(.notEnabled) } }
        _ = activate("Allow", in: host.view)
        for _ in 0..<10 { await Task.yield() }
        let writes = await rig.transport.writes, snapshots = await rig.base.count("chat.snapshot")
        XCTAssertEqual(writes.count, 1); XCTAssertEqual(snapshots, 1, "uncertainty never triggers another validation or response")
        try picture(host, "decision-uncertain-native")
    }

    func testACardAlreadyAnsweredFromThisPhoneCannotBeSentAgain() async throws {
        let restore = enableAccessibility(); defer { restore() }
        let rig = try await rig(), decision = try decision()
        let item = HostedDashboardDecision(hostID: rig.first.id, hostName: rig.first.name, decision: decision)
        let host = try show(DashboardDecisionSheet(item: item, answered: true) { command in
            try await rig.model.respondToDashboardDecision(decision, hostID: rig.first.id, command: command)
        })
        try await Task.sleep(for: .milliseconds(150))
        let labels = elements(in: host.view).compactMap(\.accessibilityLabel).joined(separator: " ")
        XCTAssertTrue(labels.contains("already went out from this phone"))
        _ = activate("Allow", in: host.view)
        for _ in 0..<10 { await Task.yield() }
        let writes = await rig.transport.writes, snapshots = await rig.base.count("chat.snapshot")
        XCTAssertTrue(writes.isEmpty); XCTAssertEqual(snapshots, 0)
    }

    func testNativeHostLandingKeepsCollidingIDsAndOfflineSavedDataWithoutSending() async throws {
        let first = try desktop(1, name: "MacBook Pro"), second = try desktop(2, name: "Mac Studio")
        let reply = try dashboard(), a = DashboardLandingTransport(reply: reply), b = DashboardLandingTransport(reply: reply)
        let transports = [first.id: a, second.id: b]
        let store = HostDashboardStore(refreshInterval: .seconds(3600), transportFactory: { transports[$0.id]! })
        store.setVisible(true, desktops: [first, second]); await store.refresh()
        await b.setOffline(); await store.refresh()
        XCTAssertEqual(store.hosts.map(\.state), [.ready, .offline])
        XCTAssertEqual(store.aggregate.projectRows.count, 2); XCTAssertEqual(Set(store.aggregate.projectRows.map(\.id)).count, 2)
        let decisions = store.hosts.flatMap { host in (host.snapshot?.decisions.rows ?? []).map { HostedDashboardDecision(hostID: host.id, hostName: host.name, decision: $0) } }
        XCTAssertEqual(decisions.count, 2); XCTAssertEqual(Set(decisions.map(\.id)).count, 2)
        var navigated = false
        for dark in [false, true] {
            let host = try show(List {
                HostDashboardSections(store: store, openHost: { _ in navigated = true }, openTarget: { _, _ in navigated = true }, review: { _ in navigated = true })
            }.listStyle(.insetGrouped), dark: dark)
            try await Task.sleep(for: .milliseconds(150))
            try picture(host, dark ? "all-hosts-native-dark" : "all-hosts-native-light")
            XCTAssertFalse(navigated, "drawing and refreshing cannot choose a host or answer a request")
            windows.last?.isHidden = true
        }
        let aMethods = await a.methods, bMethods = await b.methods
        XCTAssertTrue(aMethods.allSatisfy { $0 == "dashboard.get" }); XCTAssertTrue(bMethods.allSatisfy { $0 == "dashboard.get" })
        await store.stop()
    }
}
