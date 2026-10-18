import XCTest
import RiWorkCore
@testable import RiWorkRemote

private actor HostOverviewLedger {
    private var live: Set<String> = []
    private(set) var maximum = 0
    func opened(_ id: String) { live.insert(id); maximum = max(maximum, live.count) }
    func closed(_ id: String) { live.remove(id) }
    var count: Int { live.count }
}

/// No sockets or credentials leave the fixture. A gated reply can arrive even after disconnect, as an adversarial slow host can.
private actor HostOverviewTransport: RemoteTransport {
    private let ledger: HostOverviewLedger?
    private let fixtureID: String
    private var connected = false
    private var failing = false
    private var supported = true
    private var gated = false
    private var waiter: CheckedContinuation<JSONValue, Never>?
    private var reply: JSONValue
    private(set) var connects = 0
    private(set) var disconnects = 0
    private(set) var methods: [String] = []
    private(set) var compression: [Bool] = []

    init(id: String, projects: Int = 1, ledger: HostOverviewLedger? = nil) {
        fixtureID = id; self.ledger = ledger
        reply = .object(["v": .number(1), "chats_known": .bool(true), "totals": .object(["projects": .number(Double(projects))]),
                         "projects": .array([.object(["id": .string("11111111-1111-4111-8111-111111111111"), "name": .string("Same project")])])])
    }
    func setFailing(_ value: Bool) { failing = value }
    func setSupported(_ value: Bool) { supported = value }
    func hold() { gated = true }
    func release() { gated = false; waiter?.resume(returning: reply); waiter = nil }
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing {
        connects += 1
        guard !failing else { throw RemoteError.timeout }
        connected = true; await ledger?.opened(fixtureID)
        return pairing
    }
    func disconnect() async {
        disconnects += 1
        if connected { connected = false; await ledger?.closed(fixtureID) }
    }
    func isConnected() async -> Bool { connected }
    func setCompression(_ enabled: Bool) async { compression.append(enabled) }
    func desktopFeatures() async -> DesktopFeatures {
        DesktopFeatures(ready: .object(["features": .object(["dashboard": .bool(supported)])]))
    }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        guard connected else { throw RemoteError.disconnected }
        try RequestValidation.validate(method: method, params: params, id: id)
        methods.append(method)
        guard method == "dashboard.get" else { throw RemoteError.protocolViolation("Overview attempted a non-dashboard operation.") }
        if gated { return await withCheckedContinuation { waiter = $0 } }
        return reply
    }
}

@MainActor final class HostDashboardStoreTests: XCTestCase {
    private func desktop(_ index: Int, name: String? = nil) -> SavedDesktop {
        let route = String(format: "33333333-3333-4333-8333-%012d", index)
        let pairing = Pairing(v: 1, relay_url: "wss://example.invalid/v1/ws", desktop_id: "11111111-1111-4111-8111-111111111111",
                              device_id: String(format: "22222222-2222-4222-8222-%012d", index), route_id: route, device_name: "Fixture",
                              pairing_secret: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8", relay_token: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8")
        return SavedDesktop(name: name ?? "Host \(index)", pairing: pairing, allowLocalDevelopment: false)
    }
    private func waitFor(_ condition: () async -> Bool, file: StaticString = #filePath, line: UInt = #line) async {
        for _ in 0..<500 {
            if await condition() { return }
            try? await Task.sleep(for: .milliseconds(2))
        }
        XCTFail("Fixture did not reach the expected state", file: file, line: line)
    }

    func testIndependentHostsKeepCollidingIDsAndPartialFailures() async {
        let first = desktop(1), second = desktop(2), third = desktop(3)
        let a = HostOverviewTransport(id: first.id, projects: 2), b = HostOverviewTransport(id: second.id, projects: 3), c = HostOverviewTransport(id: third.id)
        await c.setFailing(true)
        let transports = [first.id: a, second.id: b, third.id: c]
        let store = HostDashboardStore(refreshInterval: .seconds(3600), transportFactory: { transports[$0.id]! })
        store.setVisible(true, desktops: [first, second, third])
        await store.refresh()
        XCTAssertEqual(store.aggregate.projects.value, 5)
        XCTAssertEqual(store.aggregate.projects.unknownHosts, 1)
        XCTAssertEqual(store.aggregate.projectRows.count, 2)
        XCTAssertEqual(Set(store.aggregate.projectRows.map(\.id)).count, 2)
        XCTAssertEqual(store.hosts.map(\.state), [.ready, .ready, .offline])
        await a.setFailing(true)
        await store.refresh()
        XCTAssertEqual(store.aggregate.projects.value, 5, "a failed refresh retains the host's last successful snapshot")
        XCTAssertEqual(store.aggregate.staleHostCount, 1)
        let methodsA = await a.methods, methodsB = await b.methods
        XCTAssertTrue(methodsA.allSatisfy { $0 == "dashboard.get" })
        XCTAssertTrue(methodsB.allSatisfy { $0 == "dashboard.get" })
        await store.stop()
    }

    func testActiveHostIsMirroredAndNeverRegistersASecondMobileSocket() async throws {
        let first = desktop(1), second = desktop(2)
        let a = HostOverviewTransport(id: first.id), b = HostOverviewTransport(id: second.id)
        let transports = [first.id: a, second.id: b]
        let store = HostDashboardStore(refreshInterval: .seconds(3600), transportFactory: { transports[$0.id]! })
        store.setVisible(true, desktops: [first, second], activeHostID: first.id)
        let mirrored = try DashboardSnapshot(wire: .object(["v": .number(1), "totals": .object(["projects": .number(4)])]))
        store.updateActiveHost(HostDashboardEntry(id: first.id, name: "Old name", snapshot: mirrored, readAt: .now, state: .ready))
        await store.refresh()
        XCTAssertEqual(store.aggregate.projects.value, 5)
        XCTAssertEqual(store.hosts.first?.name, first.name)
        let aConnects = await a.connects, bConnects = await b.connects
        XCTAssertEqual(aConnects, 0)
        XCTAssertEqual(bConnects, 1)
        await store.stop()
    }

    func testTheHostBeingOpenedKeepsItsLastSuccessfulRead() async throws {
        let first = desktop(1), second = desktop(2)
        let a = HostOverviewTransport(id: first.id, projects: 2), b = HostOverviewTransport(id: second.id, projects: 3)
        let transports = [first.id: a, second.id: b]
        let store = HostDashboardStore(refreshInterval: .seconds(3600), transportFactory: { transports[$0.id]! })
        store.setVisible(true, desktops: [first, second])
        await store.refresh()
        let saved = try XCTUnwrap(store.hosts.last?.readAt)
        // Opening (or reviewing on) the offline second host: the model has just cleared its dashboard and is connecting.
        store.setVisible(false, desktops: [first, second], activeHostID: second.id)
        store.updateActiveHost(HostDashboardEntry(id: second.id, name: second.name, state: .refreshing))
        XCTAssertEqual(store.hosts.last?.readAt, saved)
        XCTAssertEqual(store.aggregate.projects.value, 5, "All hosts keeps the second host's saved figures")
        store.updateActiveHost(HostDashboardEntry(id: second.id, name: second.name, state: .offline, failure: "Desktop is offline."))
        XCTAssertEqual(store.hosts.last?.state, .offline)
        XCTAssertNotNil(store.hosts.last?.snapshot)
        let newer = try DashboardSnapshot(wire: .object(["v": .number(1), "totals": .object(["projects": .number(7)])]))
        store.updateActiveHost(HostDashboardEntry(id: second.id, name: second.name, snapshot: newer, readAt: saved.addingTimeInterval(1), state: .ready))
        XCTAssertEqual(store.aggregate.projects.value, 9, "a newer read of the model's replaces it")
        let older = try DashboardSnapshot(wire: .object(["v": .number(1), "totals": .object(["projects": .number(1)])]))
        store.updateActiveHost(HostDashboardEntry(id: second.id, name: second.name, snapshot: older, readAt: saved.addingTimeInterval(-60), state: .ready))
        XCTAssertEqual(store.aggregate.projects.value, 9, "an older one does not")
        await store.stop()
    }

    func testOverviewConnectionsAskForTheChosenCompression() async {
        let desktop = desktop(1), transport = HostOverviewTransport(id: "compression")
        let store = HostDashboardStore(refreshInterval: .seconds(3600), transportFactory: { _ in transport })
        store.compressTraffic = { false }
        store.setVisible(true, desktops: [desktop])
        await store.refresh()
        let asked = await transport.compression
        XCTAssertEqual(asked, [false])
        await store.stop()
    }

    func testAReadThatEndsUnansweredDoesNotLeaveTheHostReading() async throws {
        let desktop = desktop(1), transport = HostOverviewTransport(id: "abandoned")
        let store = HostDashboardStore(refreshInterval: .seconds(3600), transportFactory: { _ in transport })
        store.setVisible(true, desktops: [desktop])
        await store.refresh()
        await transport.setFailing(true)
        await store.refresh()
        XCTAssertEqual(store.hosts.first?.state, .offline)
        await transport.setFailing(false); await transport.hold()
        let round = Task { await store.refresh() }
        await waitFor { await transport.methods.count == 2 }
        XCTAssertEqual(store.hosts.first?.state, .refreshing)
        XCTAssertTrue(store.hosts.first?.freshness().stale == true, "an offline host being read again is not current until it answers")
        // The read is overtaken (the interactive model takes this host) before the host answers.
        store.updateDesktops([desktop], activeHostID: desktop.id)
        await transport.release()
        await round.value
        for _ in 0..<20 { await Task.yield() }
        XCTAssertEqual(store.hosts.first?.state, .offline, "not left Reading… for good")
        await store.stop()
    }

    func testUnsupportedHostsAreNotAskedForDashboard() async {
        let desktop = desktop(1), transport = HostOverviewTransport(id: "unsupported")
        await transport.setSupported(false)
        let store = HostDashboardStore(refreshInterval: .seconds(3600), transportFactory: { _ in transport })
        store.setVisible(true, desktops: [desktop])
        await store.refresh()
        XCTAssertEqual(store.hosts.first?.state, .unsupported)
        let methods = await transport.methods, connected = await transport.isConnected()
        XCTAssertTrue(methods.isEmpty)
        XCTAssertFalse(connected)
        await store.stop()
    }

    func testNoConnectionsWhenHiddenOrInBackground() async {
        let desktop = desktop(1), transport = HostOverviewTransport(id: "visible")
        let store = HostDashboardStore(refreshInterval: .seconds(3600), transportFactory: { _ in transport })
        store.updateDesktops([desktop])
        await store.refresh()
        var connects = await transport.connects
        XCTAssertEqual(connects, 0)
        store.setVisible(true, desktops: [desktop], sceneActive: false)
        await store.refresh()
        connects = await transport.connects
        XCTAssertEqual(connects, 0)
        store.setSceneActive(true)
        await store.refresh()
        connects = await transport.connects
        XCTAssertEqual(connects, 1)
        store.setSceneActive(false)
        await store.refresh()
        XCTAssertEqual(store.hosts.first?.state, .paused)
        connects = await transport.connects
        XCTAssertEqual(connects, 1)
        await store.stop()
    }

    func testRemovingHostDiscardsItsLateReply() async {
        let desktop = desktop(1), transport = HostOverviewTransport(id: "late")
        await transport.hold()
        let store = HostDashboardStore(refreshInterval: .seconds(3600), transportFactory: { _ in transport })
        store.setVisible(true, desktops: [desktop])
        await waitFor { await transport.methods.count == 1 }
        store.updateDesktops([])
        await transport.release()
        await store.refresh()
        XCTAssertTrue(store.hosts.isEmpty)
        XCTAssertNil(store.aggregate.projects.value)
        await store.stop()
        let connected = await transport.isConnected()
        XCTAssertFalse(connected)
    }

    func testStopClosesConnectionsAndRejectsLateRepliesBeforeMainHostActivates() async {
        let desktop = desktop(1), transport = HostOverviewTransport(id: "stop")
        await transport.hold()
        let store = HostDashboardStore(refreshInterval: .seconds(3600), transportFactory: { _ in transport })
        store.setVisible(true, desktops: [desktop])
        await waitFor { await transport.methods.count == 1 }
        await store.stop()
        let connected = await transport.isConnected()
        XCTAssertFalse(connected, "stop is a disconnect barrier before RemoteModel can register the same route")
        await transport.release()
        for _ in 0..<10 { await Task.yield() }
        XCTAssertNil(store.hosts.first?.snapshot)
        XCTAssertEqual(store.hosts.first?.state, .paused)
    }

    func testConcurrentConnectionsStayBounded() async {
        let desktops = (1...6).map { desktop($0) }, ledger = HostOverviewLedger()
        let transports = Dictionary(uniqueKeysWithValues: desktops.map { ($0.id, HostOverviewTransport(id: $0.id, ledger: ledger)) })
        for transport in transports.values { await transport.hold() }
        let store = HostDashboardStore(refreshInterval: .seconds(3600), maximumConcurrentHosts: 2, transportFactory: { transports[$0.id]! })
        store.setVisible(true, desktops: desktops)
        await waitFor { await ledger.count == 2 }
        let maximumBefore = await ledger.maximum
        XCTAssertEqual(maximumBefore, 2)
        for transport in transports.values { await transport.release() }
        await store.refresh()
        let maximum = await ledger.maximum, remaining = await ledger.count
        XCTAssertEqual(maximum, 2)
        XCTAssertEqual(remaining, 0)
        XCTAssertEqual(store.aggregate.sampledHostCount, 6)
        await store.stop()
    }

    func testOverviewDoesNotConsumeAPendingSingleUsePairingInvite() async {
        var saved = desktop(1)
        saved.pairing = Pairing(v: 2, relay_url: saved.pairing.relay_url, desktop_id: saved.pairing.desktop_id,
                                device_id: saved.pairing.device_id, route_id: saved.id, device_name: "Pending",
                                pairing_secret: "", relay_token: saved.pairing.relay_token, invite_state: "pending")
        let transport = HostOverviewTransport(id: "pending")
        let store = HostDashboardStore(refreshInterval: .seconds(3600), transportFactory: { _ in transport })
        store.setVisible(true, desktops: [saved])
        await store.refresh()
        let connects = await transport.connects
        XCTAssertEqual(connects, 0)
        XCTAssertTrue(store.hosts.first?.failure?.contains("finish pairing") == true)
        await store.stop()
    }
}
