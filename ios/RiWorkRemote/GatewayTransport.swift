import Foundation
import RiWorkCore

/// One immutable execution target behind a paired Mac. It holds a lease on the gateway's existing socket;
/// all routes, the interactive desktop and the overview share that socket and its request budget.
@MainActor final class GatewayTransport: RemoteTransport {
    private weak var connections: HostConnections?
    private var lease: HostLease?
    private var pairing: Pairing?
    private var session: String?
    private var features = DesktopFeatures()
    private var generation = UUID()

    init(connections: HostConnections) { self.connections = connections }

    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing {
        await disconnect()
        guard let connections, let route = pairing.gateway,
              GatewayRequests.isUUID(route.routeID), GatewayRequests.isUUID(route.hostID) else {
            throw RemoteError.protocolViolation("A gateway route needs its pinned host identity.")
        }
        let token = generation
        let gateway = SavedDesktop(name: route.gatewayName, pairing: pairing.direct, allowLocalDevelopment: allowLocalDevelopment)
        let held = try await connections.borrow(gateway)
        guard generation == token else { connections.release(held); throw CancellationError() }
        lease = held
        do {
            guard held.features.hosts else { throw RemoteError.remote("Update Somtus on the gateway Mac to open its connected desktops.") }
            let opened = try await held.transport.request(method: "hosts.open", params: ["route_id": .string(route.routeID), "host_id": .string(route.hostID)],
                                                          id: UUID().uuidString.lowercased(), boundTo: UploadConnectionIdentity(pairing: held.pairing))
            guard let openedSession = opened["session"].string, GatewayRequests.isUUID(openedSession) else {
                throw RemoteError.protocolViolation("Gateway did not confirm the requested host identity.")
            }
            guard opened["route_id"] == .string(route.routeID), opened["host_id"] == .string(route.hostID), case .object = opened["features"] else {
                _ = try? await held.transport.request(method: "hosts.close", params: ["session": .string(openedSession)], id: UUID().uuidString.lowercased())
                throw RemoteError.protocolViolation("Gateway did not confirm the requested host identity.")
            }
            guard generation == token, connections.isCurrent(held) else {
                _ = try? await held.transport.request(method: "hosts.close", params: ["session": .string(openedSession)], id: UUID().uuidString.lowercased())
                throw CancellationError()
            }
            session = openedSession
            self.pairing = held.pairing.routed(to: route)
            features = DesktopFeatures(ready: .object(["features": opened["features"]]))
            // A route cannot become another gateway: discovery and compression are for the outer connection.
            features.hosts = false
            return self.pairing!
        } catch {
            if generation == token { await disconnect() }
            throw error
        }
    }

    func disconnect() async {
        generation = UUID()
        let held = lease, opened = session
        lease = nil; pairing = nil; session = nil; features = DesktopFeatures()
        if let held {
            if let opened, connections?.isCurrent(held) == true {
                _ = try? await held.transport.request(method: "hosts.close", params: ["session": .string(opened)], id: UUID().uuidString.lowercased(),
                                                      boundTo: UploadConnectionIdentity(pairing: held.pairing))
            }
            await connections?.release(held)?.value
        }
    }

    func isConnected() async -> Bool {
        guard session != nil, let held = lease, connections?.isCurrent(held) == true else { return false }
        return await held.transport.isConnected()
    }
    func desktopFeatures() async -> DesktopFeatures { features }
    func setCompression(_ enabled: Bool) async { await lease?.transport.setCompression(enabled) }
    func compressionActive() async -> Bool { await lease?.transport.compressionActive() ?? false }

    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        guard let pairing else { throw RemoteError.disconnected }
        return try await request(method: method, params: params, id: id, boundTo: UploadConnectionIdentity(pairing: pairing))
    }
    func request(method: String, params: [String: JSONValue], id: String, boundTo identity: UploadConnectionIdentity) async throws -> JSONValue {
        guard let pairing, let held = lease, let session, connections?.isCurrent(held) == true else { throw RemoteError.disconnected }
        guard identity == UploadConnectionIdentity(pairing: pairing) else { throw CancellationError() }
        try RequestValidation.validate(method: method, params: params, id: id)
        guard !method.hasPrefix("hosts."), method != "link.configure", !method.hasPrefix("pty.") else {
            throw RemoteError.protocolViolation("Gateway routes cannot forward another gateway.")
        }
        let token = generation
        let mutation = GatewayRequests.isMutation(method: "hosts.call", params: ["method": .string(method), "params": .object(params)])
        do {
            let value = try await held.transport.request(method: "hosts.call", params: ["session": .string(session), "request_id": .string(id),
                                                                                      "method": .string(method), "params": .object(params)],
                                                         id: UUID().uuidString.lowercased(), boundTo: UploadConnectionIdentity(pairing: held.pairing))
            guard generation == token else {
                if mutation { throw RemoteError.uncertainDelivery }
                throw CancellationError()
            }
            return value
        } catch {
            if case RemoteError.rpc(let code, _) = error,
               ["host_not_found", "host_changed", "host_session_closed", "host_unavailable", "host_unsupported", "delivery_uncertain"].contains(code) {
                if generation == token { await disconnect() }
                if code == "delivery_uncertain" { throw RemoteError.uncertainDelivery }
            }
            throw error
        }
    }
}
