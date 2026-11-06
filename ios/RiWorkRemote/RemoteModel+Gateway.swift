import Foundation
import RiWorkCore

extension RemoteModel {
    /// A derived record never carries another Mac's credentials. Its authentication always follows the saved gateway pairing.
    /// This also removes orphan routes when loading an older or interrupted library write.
    func synchronizeGatewayPairings() {
        let gateways = Dictionary(desktops.filter { $0.pairing.gateway == nil }.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        desktops = desktops.compactMap { desktop in
            guard var route = desktop.pairing.gateway else { return desktop }
            guard let gateway = gateways[desktop.pairing.route_id], GatewayRequests.isUUID(route.routeID), GatewayRequests.isUUID(route.hostID) else { return nil }
            route.gatewayName = gateway.name
            var value = desktop
            value.pairing = gateway.pairing.routed(to: route)
            return value
        }
    }

    /// Explicit refresh of the Hosts page also refreshes every established gateway's inventory. No pending invite is consumed.
    func refreshGatewayHosts() async {
        let ids = desktops.filter { $0.pairing.gateway == nil && ($0.pairing.v == 1 || $0.pairing.root_key != nil) }.map(\.id)
        for id in ids { await discoverGatewayHosts(gatewayID: id) }
    }

    func discoverGatewayHosts(gatewayID: String) async {
        guard gatewayDiscoveryEnabled, appActive, !gatewayDiscoveryInFlight.contains(gatewayID),
              let gateway = desktops.first(where: { $0.id == gatewayID && $0.pairing.gateway == nil }),
              gateway.pairing.v == 1 || gateway.pairing.root_key != nil else { return }
        gatewayDiscoveryInFlight.insert(gatewayID)
        defer { gatewayDiscoveryInFlight.remove(gatewayID) }
        var held: HostLease?
        defer { if let held { connections.release(held) } }
        do {
            let lease = try await connections.borrow(gateway)
            held = lease
            let hosts: [GatewayHost]
            if lease.features.hosts {
                let value = try await lease.transport.request(method: "hosts.list", params: [:], id: UUID().uuidString.lowercased(),
                                                              boundTo: UploadConnectionIdentity(pairing: lease.pairing))
                hosts = try GatewayHost.list(value)
            } else { hosts = [] }
            guard connections.isCurrent(lease), desktops.contains(where: { $0.id == gatewayID && $0.pairing == gateway.pairing }) else { return }
            let old = desktops
            let known = Dictionary(desktops.filter { $0.pairing.gateway != nil && $0.pairing.route_id == gatewayID }.map { ($0.pairing.gateway!.routeID, $0) }, uniquingKeysWith: { first, _ in first })
            var changedIdentity = false
            let derived: [SavedDesktop] = hosts.map { host in
                if var saved = known[host.routeID] {
                    guard saved.pairing.gateway?.hostID == host.hostID else { changedIdentity = true; return saved }
                    saved.name = host.label
                    saved.pairing = gateway.pairing.routed(to: GatewayRoute(routeID: host.routeID, hostID: host.hostID, gatewayName: gateway.name))
                    return saved
                }
                return SavedDesktop(name: host.label, pairing: gateway.pairing.routed(to: GatewayRoute(routeID: host.routeID, hostID: host.hostID, gatewayName: gateway.name)),
                                    allowLocalDevelopment: gateway.allowLocalDevelopment)
            }
            let remaining = Set(derived.map(\.id))
            let removed = known.values.filter { !remaining.contains($0.id) }.map(\.id)
            desktops.removeAll { $0.pairing.gateway != nil && $0.pairing.route_id == gatewayID }
            desktops.append(contentsOf: derived)
            do { try persist() } catch { desktops = old; throw error }
            for desktop in derived { connections.remember(desktop.id) }
            for id in removed { connections.forget(id); discardKeyBuffers(forDesktop: id); theme.forget(id) }
            if let selectedDesktopID, removed.contains(selectedDesktopID) {
                cancelUpload(); await disconnect(); self.selectedDesktopID = nil; clearSnapshot()
                try persist()
            }
            gatewayDiscoveryError = changedIdentity ? "A desktop behind \(gateway.name) changed identity. Replace its SSH route on that Mac before using it again." : nil
        } catch {
            if !(error is CancellationError), desktops.contains(where: { $0.id == gatewayID }) {
                gatewayDiscoveryError = "Cannot refresh desktops via \(gateway.name): \(error.localizedDescription)"
            }
        }
        if let held { await connections.release(held)?.value }
    }
}
