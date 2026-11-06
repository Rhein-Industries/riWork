import XCTest
import RiWorkCore
@testable import RiWorkRemote

/// An authenticated gateway with two isolated SSH targets; no real network or SSH is used.
@MainActor private final class GatewayFixtureTransport: RemoteTransport {
    struct Call { let route: String; let method: String; let requestID: String }
    var connected = false
    var pairing: Pairing?
    var supportsHosts = true
    var descriptors: [JSONValue] = []
    var sessions: [String: (route: String, host: String)] = [:]
    var calls: [Call] = []
    var connects = 0
    var nextCallFailure: String?
    var mismatchedIdentity = false
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing {
        XCTAssertNil(pairing.gateway, "Only the gateway may open an authenticated socket")
        self.pairing = pairing; connected = true; connects += 1; return pairing
    }
    func disconnect() async { connected = false; pairing = nil; sessions.removeAll() }
    func isConnected() async -> Bool { connected }
    func desktopFeatures() async -> DesktopFeatures {
        DesktopFeatures(ready: .object(["features": .object(["hosts": .number(supportsHosts ? 1 : 0), "dashboard": .bool(true)])]))
    }
    func request(method: String, params: [String: JSONValue], id: String, boundTo identity: UploadConnectionIdentity) async throws -> JSONValue {
        guard let pairing, identity == UploadConnectionIdentity(pairing: pairing) else { throw CancellationError() }
        return try await request(method: method, params: params, id: id)
    }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        guard connected else { throw RemoteError.disconnected }
        try RequestValidation.validate(method: method, params: params, id: id)
        switch method {
        case "hosts.list": return .object(["v": .number(1), "hosts": .array(descriptors)])
        case "hosts.open":
            let session = UUID().uuidString.lowercased(), route = params["route_id"]!.string!, host = params["host_id"]!.string!
            sessions[session] = (route, host)
            return .object(["session": .string(session), "route_id": .string(route), "host_id": .string(mismatchedIdentity ? UUID().uuidString.lowercased() : host),
                            "features": .object(["dashboard": .bool(true)])])
        case "hosts.close": sessions.removeValue(forKey: params["session"]!.string!); return .object([:])
        case "hosts.call":
            guard let target = sessions[params["session"]!.string!] else { throw RemoteError.rpc(code: "host_session_closed", message: "Route closed") }
            let inner = params["method"]!.string!
            calls.append(Call(route: target.route, method: inner, requestID: params["request_id"]!.string!))
            if let failure = nextCallFailure { nextCallFailure = nil; throw RemoteError.rpc(code: failure, message: "Fixture route failure") }
            return try result(inner)
        default: return try result(method)
        }
    }
    private func result(_ method: String) throws -> JSONValue {
        switch method {
        case "projects.list": return .object(["projects": .array([])])
        case "orchestrators.list": return .object(["orchestrators": .array([])])
        case "dashboard.get": return .object(["v": .number(1), "chats_known": .bool(true), "totals": .object(["projects": .number(1)]), "projects": .array([])])
        case "upload.cancel", "chat.command", "shell.input": return .object(["status": .string("applied")])
        default: throw RemoteError.rpc(code: "invalid_request", message: "unsupported RPC method")
        }
    }
}

@MainActor final class GatewayConnectionTests: XCTestCase {
    private let routeA = "44444444-4444-4444-8444-444444444444"
    private let routeB = "55555555-5555-4555-8555-555555555555"
    private let hostA = "66666666-6666-4666-8666-666666666666"
    private let hostB = "77777777-7777-4777-8777-777777777777"
    private func root() -> SavedDesktop {
        SavedDesktop(name: "Gateway", pairing: Pairing(v: 1, relay_url: "wss://example.invalid/v1/ws", desktop_id: "11111111-1111-4111-8111-111111111111",
                                                      device_id: "22222222-2222-4222-8222-222222222222", route_id: "33333333-3333-4333-8333-333333333333", device_name: "Fixture",
                                                      pairing_secret: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8", relay_token: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"), allowLocalDevelopment: false)
    }
    private func peer(_ root: SavedDesktop, route: String, host: String) -> SavedDesktop {
        SavedDesktop(name: route == routeA ? "Studio" : "Laptop", pairing: root.pairing.routed(to: .init(routeID: route, hostID: host, gatewayName: root.name)), allowLocalDevelopment: false)
    }
    private func descriptor(route: String, host: String) -> JSONValue {
        .object(["route_id": .string(route), "host_id": .string(host), "label": .string(route == routeA ? "Studio" : "Laptop"), "transport": .string("ssh")])
    }
    private func fixture() -> GatewayFixtureTransport {
        let value = GatewayFixtureTransport()
        value.descriptors = [descriptor(route: routeA, host: hostA), descriptor(route: routeB, host: hostB)]
        return value
    }

    func testRoutesShareGatewaySocketAndBindDecisionsAndUploadsToExactTarget() async throws {
        let gateway = root(), transport = fixture()
        var factories = 0
        let connections = HostConnections(idleTimeout: .zero, factory: { id in XCTAssertEqual(id, gateway.id); factories += 1; return transport })
        let rootLease = try await connections.borrow(gateway)
        let a = try await connections.borrow(peer(gateway, route: routeA, host: hostA))
        let b = try await connections.borrow(peer(gateway, route: routeB, host: hostB))
        XCTAssertEqual(factories, 1); XCTAssertEqual(transport.connects, 1); XCTAssertEqual(transport.sessions.count, 2)
        let requestID = UUID().uuidString.lowercased(), upload = UUID().uuidString.lowercased()
        do {
            _ = try await a.transport.request(method: "upload.cancel", params: ["upload": .string(upload)], id: requestID,
                                              boundTo: UploadConnectionIdentity(pairing: b.pairing))
            XCTFail("Cross-target upload was sent")
        } catch is CancellationError {} catch { XCTFail("Unexpected error: \(error)") }
        XCTAssertTrue(transport.calls.isEmpty)
        _ = try await a.transport.request(method: "upload.cancel", params: ["upload": .string(upload)], id: requestID,
                                          boundTo: UploadConnectionIdentity(pairing: a.pairing))
        let command = try ChatCommandRequest(chatID: hostA, command: .interrupt)
        _ = try await b.transport.request(method: "chat.command", params: command.params, id: UUID().uuidString.lowercased(),
                                          boundTo: UploadConnectionIdentity(pairing: b.pairing))
        XCTAssertEqual(transport.calls.map(\.route), [routeA, routeB])
        XCTAssertEqual(transport.calls.first?.requestID, requestID)
        await connections.release(a)?.value
        XCTAssertEqual(transport.sessions.count, 1)
        XCTAssertTrue(connections.isCurrent(b)); XCTAssertTrue(transport.connected)
        connections.forget(gateway.id)
        let stillConnected = await b.transport.isConnected()
        XCTAssertFalse(stillConnected)
        await connections.release(b)?.value; await connections.release(rootLease)?.value
        do { _ = try await connections.borrow(gateway); XCTFail("Forgotten gateway reopened from stale credentials") } catch {}
    }

    func testTargetFaultInvalidatesSessionAndUncertainMutationIsNotRetried() async throws {
        let gateway = root(), transport = fixture()
        let connections = HostConnections(idleTimeout: .zero, factory: { _ in transport })
        let held = try await connections.borrow(peer(gateway, route: routeA, host: hostA))
        transport.nextCallFailure = "delivery_uncertain"
        do { _ = try await held.transport.request(method: "shell.input", params: ["shell_id": .string(hostA), "line": .string("echo once")], id: UUID().uuidString.lowercased()); XCTFail("Expected uncertain delivery") }
        catch RemoteError.uncertainDelivery {} catch { XCTFail("Unexpected error: \(error)") }
        XCTAssertEqual(transport.calls.count, 1)
        let connected = await held.transport.isConnected()
        XCTAssertFalse(connected)
        await connections.release(held)?.value
    }

    func testMismatchedOpenIdentityClosesTokenAndNeverForwards() async throws {
        let gateway = root(), transport = fixture(); transport.mismatchedIdentity = true
        let connections = HostConnections(idleTimeout: .zero, factory: { _ in transport })
        do { _ = try await connections.borrow(peer(gateway, route: routeA, host: hostA)); XCTFail("Accepted changed host identity") } catch {}
        XCTAssertTrue(transport.sessions.isEmpty); XCTAssertTrue(transport.calls.isEmpty)
    }

    func testInteractiveModelDiscoversSwitchesRefreshesAndForgetsDerivedHosts() async throws {
        let suite = "somtus.gateway.tests.\(UUID().uuidString)", gateway = root(), transport = fixture()
        let keychain = KeychainStore(service: suite), defaults = UserDefaults(suiteName: suite)!
        defer { try? keychain.delete(); defaults.removePersistentDomain(forName: suite) }
        let connections = HostConnections(idleTimeout: .zero, factory: { _ in transport })
        let model = RemoteModel(connections: connections, keychain: keychain, pollInterval: .seconds(3600), defaults: defaults, keepAwake: { _ in }, prefetch: false)
        model.desktops = [gateway]
        await model.activate(gateway.id)
        XCTAssertEqual(model.state, .connected); XCTAssertEqual(model.desktops.count, 3)
        let a = try XCTUnwrap(model.desktops.first { $0.pairing.gateway?.routeID == routeA })
        await model.activate(a.id)
        XCTAssertEqual(model.state, .connected); XCTAssertEqual(model.selectedDesktopID, a.id)
        XCTAssertTrue(transport.calls.contains { $0.route == routeA && $0.method == "projects.list" })
        let overview = HostDashboardStore(connections: connections, refreshInterval: .seconds(3600))
        overview.setVisible(true, desktops: model.desktops, activeHostID: a.id)
        await overview.refresh()
        XCTAssertEqual(overview.hosts.count, 3)
        XCTAssertTrue(transport.calls.contains { $0.route == routeB && $0.method == "dashboard.get" })
        await overview.stop()
        transport.descriptors = [descriptor(route: routeB, host: hostB)]
        await model.refreshGatewayHosts()
        XCTAssertFalse(model.desktops.contains { $0.id == a.id }); XCTAssertNil(model.selectedDesktopID)
        try await model.remove(id: gateway.id)
        XCTAssertTrue(model.desktops.isEmpty)
    }

    func testOldGatewayRemovesCachedRoutesAndRootPairingUpdatesPropagate() async throws {
        let suite = "somtus.gateway.old.\(UUID().uuidString)", gateway = root(), transport = fixture()
        transport.supportsHosts = false
        let keychain = KeychainStore(service: suite), defaults = UserDefaults(suiteName: suite)!
        defer { try? keychain.delete(); defaults.removePersistentDomain(forName: suite) }
        let model = RemoteModel(connections: HostConnections(idleTimeout: .zero, factory: { _ in transport }), keychain: keychain, defaults: defaults, keepAwake: { _ in }, prefetch: false)
        model.desktops = [gateway, peer(gateway, route: routeA, host: hostA)]
        try model.rename(id: gateway.id, name: "Renamed gateway")
        XCTAssertEqual(model.desktops.last?.pairing.gateway?.gatewayName, "Renamed gateway")
        model.selectedDesktopID = gateway.id
        var stored = try JSONDecoder().decode([String: JSONValue].self, from: JSONEncoder().encode(gateway.pairing))
        stored["relay_token"] = .string("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
        let refreshed = try JSONDecoder().decode(Pairing.self, from: JSONEncoder().encode(stored))
        try model.updateDesktop { $0.pairing = refreshed }
        XCTAssertEqual(model.desktops.last?.pairing.direct, refreshed)
        await model.refreshGatewayHosts()
        XCTAssertEqual(model.desktops.count, 1); XCTAssertTrue(transport.calls.isEmpty)
        try await model.remove(id: gateway.id)
    }

    func testDiscoveryNeverRetargetsAnExistingRouteWhenPinnedIdentityChanges() async throws {
        let suite = "somtus.gateway.pin.\(UUID().uuidString)", gateway = root(), transport = fixture()
        let keychain = KeychainStore(service: suite), defaults = UserDefaults(suiteName: suite)!
        defer { try? keychain.delete(); defaults.removePersistentDomain(forName: suite) }
        let model = RemoteModel(connections: HostConnections(idleTimeout: .zero, factory: { _ in transport }), keychain: keychain, defaults: defaults, keepAwake: { _ in }, prefetch: false)
        let original = peer(gateway, route: routeA, host: hostA)
        model.desktops = [gateway, original]
        transport.descriptors = [descriptor(route: routeA, host: hostB)]
        await model.refreshGatewayHosts()
        XCTAssertEqual(model.desktops.last?.id, original.id)
        XCTAssertEqual(model.desktops.last?.pairing.executionHostID, hostA)
        XCTAssertTrue(model.gatewayDiscoveryError?.contains("changed identity") == true)
        try await model.remove(id: gateway.id)
    }
}
