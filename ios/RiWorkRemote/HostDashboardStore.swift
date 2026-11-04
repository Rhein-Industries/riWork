import Foundation
import Observation
import RiWorkCore

/// The All hosts overview: each paired host's dashboard, read over that host's one socket, borrowed from `HostConnections` (it opens none
/// of its own). Never takes the interactive model's input focus or terminal viewport, and never reads the interactive host, whose
/// dashboard is mirrored from RemoteModel.
@MainActor @Observable final class HostDashboardStore {
    private(set) var hosts: [HostDashboardEntry] = []
    private(set) var visible = false
    private(set) var sceneActive = true
    var aggregate: HostDashboardAggregate { HostDashboardAggregate(hosts: hosts) }

    @ObservationIgnored private let connections: HostConnections
    @ObservationIgnored private let refreshInterval: Duration
    @ObservationIgnored private let maximumConcurrentHosts: Int
    @ObservationIgnored private var desktops: [SavedDesktop] = []
    @ObservationIgnored private var activeHostID: String?
    @ObservationIgnored private var generation = UUID()
    @ObservationIgnored private var loop: Task<Void, Never>?
    @ObservationIgnored private var round: Task<Void, Never>?
    /// The sockets this overview's reads hold, by host: given back after each read, and all at once whenever it stops.
    @ObservationIgnored private var leases: [String: HostLease] = [:]

    init(connections: HostConnections, refreshInterval: Duration = .seconds(15), maximumConcurrentHosts: Int = HostConnections.maximumOtherSockets) {
        self.connections = connections
        self.refreshInterval = max(.milliseconds(100), refreshInterval)
        self.maximumConcurrentHosts = max(1, min(4, maximumConcurrentHosts))
    }

    /// Called when the root host overview enters/leaves the screen. No host is read while it is hidden or the scene is inactive.
    func setVisible(_ visible: Bool, desktops: [SavedDesktop], sceneActive: Bool = true, activeHostID: String? = nil) {
        let changed = self.visible != visible || self.sceneActive != sceneActive
        self.visible = visible; self.sceneActive = sceneActive
        updateDesktops(desktops, activeHostID: activeHostID)
        if changed { reconcileLifecycle() }
    }

    /// The active host is the interactive model's: it is mirrored (`updateActiveHost`), never read here.
    func updateDesktops(_ desktops: [SavedDesktop], activeHostID: String? = nil) {
        // A malformed/duplicated library cannot schedule two reads of the same host.
        var seen: Set<String> = []
        let unique = desktops.filter { seen.insert($0.id).inserted }
        let changed = unique.map(\.id) != self.desktops.map(\.id)
            || zip(unique, self.desktops).contains { $0.pairing != $1.pairing || $0.allowLocalDevelopment != $1.allowLocalDevelopment }
            || self.activeHostID != activeHostID
        self.desktops = unique; self.activeHostID = activeHostID
        let previous = Dictionary(uniqueKeysWithValues: hosts.map { ($0.id, $0) })
        hosts = unique.map { desktop in
            var entry = previous[desktop.id] ?? HostDashboardEntry(id: desktop.id, name: desktop.name)
            entry.name = desktop.name; return entry
        }
        if changed { reconcileLifecycle() }
    }

    func updateActiveHost(_ entry: HostDashboardEntry) {
        guard entry.id == activeHostID, let index = hosts.firstIndex(where: { $0.id == entry.id }) else { return }
        var entry = entry
        entry.name = hosts[index].name
        // A host just opened has no read of its own yet (`activate` clears it): the last successful one stays, marked by its age.
        if let saved = hosts[index].readAt, entry.readAt.map({ $0 < saved }) ?? true {
            entry.snapshot = hosts[index].snapshot; entry.readAt = saved
        }
        if hosts[index] != entry { hosts[index] = entry }
    }

    func setSceneActive(_ active: Bool) {
        guard sceneActive != active else { return }
        sceneActive = active; reconcileLifecycle()
    }

    /// A pull-to-refresh shares an already-running round rather than stacking requests.
    func refresh() async {
        guard visible, sceneActive else { return }
        if let round { await round.value; return }
        let token = generation
        let pending = desktops.filter { $0.id != activeHostID }
        let task = Task<Void, Never> { [weak self] in
            guard let self else { return }
            await self.readRound(pending, token: token)
        }
        round = task
        await withTaskCancellationHandler { await task.value } onCancel: { task.cancel() }
        if generation == token { round = nil }
    }

    /// Stops reading and gives back every socket the overview holds; returns once those that close now (nobody else holds them) are closed.
    /// A socket another part of the app uses stays open for it.
    func stop() async {
        visible = false
        for closing in reconcileLifecycle() { await closing.value }
    }

    /// Answers a card over its host's socket (opened for it if no one has it open), leaving the interactive host alone. The transport
    /// binds the command to the host the socket authenticated.
    func respond(to decision: DashboardDecision, on desktop: SavedDesktop, command: ChatCommand) async throws {
        // As for a read: a single-use invite is redeemed only by the explicit connection that saves its key.
        guard desktop.pairing.v == 1 || desktop.pairing.root_key != nil else { throw DashboardResponseError.notSent("Open this desktop once to finish pairing.") }
        let lease: HostLease
        do { lease = try await connections.borrow(desktop) }
        catch { throw DashboardResponseError.notSent(RemoteModel.dashboardFailureText(error)) }
        do {
            guard lease.pairing == desktop.pairing else { throw DashboardResponseError.notSent("Open this desktop once to finish pairing.") }
            try await DashboardResponder.respond(decision, command: command, client: lease.transport, identity: UploadConnectionIdentity(pairing: lease.pairing)) {
                connections.isCurrent(lease)
            }
        } catch {
            await connections.release(lease)?.value
            throw error
        }
        await connections.release(lease)?.value
    }

    /// Starts or stops the reads; returns the closes of the sockets it gave back that nobody else held.
    @discardableResult private func reconcileLifecycle() -> [Task<Void, Never>] {
        generation = UUID()
        loop?.cancel(); loop = nil
        round?.cancel(); round = nil
        // A read cut short gives its socket back here, even one whose host never answers.
        let closing = leases.values.compactMap { connections.release($0) }
        leases.removeAll()
        guard visible, sceneActive else {
            for index in hosts.indices where hosts[index].id != activeHostID { hosts[index].state = .paused }
            return closing
        }
        let token = generation
        loop = Task { [weak self, refreshInterval] in
            while !Task.isCancelled {
                guard let self, self.generation == token else { return }
                await self.refresh()
                do { try await Task.sleep(for: refreshInterval) } catch { return }
            }
        }
        return closing
    }

    private func readRound(_ pending: [SavedDesktop], token: UUID) async {
        guard current(token) else { return }
        await withTaskGroup(of: Void.self) { group in
            var next = 0
            @MainActor func enqueue() {
                guard next < pending.count, current(token) else { return }
                let desktop = pending[next]; next += 1
                group.addTask { [weak self] in await self?.read(desktop, token: token) }
            }
            for _ in 0..<min(maximumConcurrentHosts, pending.count) { enqueue() }
            while await group.next() != nil {
                if !current(token) { group.cancelAll(); return }
                enqueue()
            }
        }
    }

    private func current(_ token: UUID) -> Bool { generation == token && visible && sceneActive && !Task.isCancelled }

    private func read(_ desktop: SavedDesktop, token: UUID) async {
        guard current(token), let index = hosts.firstIndex(where: { $0.id == desktop.id }) else { return }
        // An overview must not consume a single-use invite without saving the established key. The normal explicit connection does that.
        guard desktop.pairing.v == 1 || desktop.pairing.root_key != nil else {
            hosts[index].state = .offline; hosts[index].failure = "Open this desktop once to finish pairing."
            return
        }
        // A host whose last read worked stays current while it is read again; any other shows that it is being read.
        let previous = hosts[index].state
        if previous != .ready { hosts[index].state = .refreshing }
        var held: HostLease?
        do {
            let lease = try await connections.borrow(desktop)
            held = lease
            guard current(token) else { throw CancellationError() }
            leases[desktop.id] = lease
            guard lease.pairing == desktop.pairing else { throw RemoteError.protocolViolation("Open this desktop once to finish pairing.") }
            if lease.features.dashboard {
                let wire = try await lease.transport.request(method: "dashboard.get", params: [:], id: UUID().uuidString.lowercased())
                let snapshot = try DashboardSnapshot(wire: wire)
                if current(token), let index = hosts.firstIndex(where: { $0.id == desktop.id }) {
                    hosts[index].snapshot = snapshot; hosts[index].readAt = .now
                    hosts[index].state = .ready; hosts[index].failure = nil
                }
            } else {
                install(hostID: desktop.id, token: token, state: .unsupported, failure: "Update Somtus on this host to read its dashboard.")
            }
        } catch is CancellationError {
        } catch {
            let unsupported = RemoteError.isUnsupportedMethod(error)
            // Not asked again on this socket's session (by this overview or `relay.call`), and left out of the host's features.
            if unsupported, let held { connections.dashboardRefused(held) }
            install(hostID: desktop.id, token: token, state: unsupported ? .unsupported : .offline,
                    failure: unsupported ? "Update Somtus on this host to read its dashboard." : RemoteModel.dashboardFailureText(error))
        }
        // Given back (unless stopping already did); a socket nobody else holds closes as idle sockets do.
        if let held {
            if leases[desktop.id]?.id == held.id { leases.removeValue(forKey: desktop.id) }
            await connections.release(held)?.value
        }
        // A read that ended without an answer (stopped, replaced, cancelled) leaves the host as it was, not "Reading…" for good.
        if let index = hosts.firstIndex(where: { $0.id == desktop.id }), hosts[index].state == .refreshing {
            hosts[index].state = previous
        }
    }

    private func install(hostID: String, token: UUID, state: HostDashboardEntry.State, failure: String) {
        guard current(token), let index = hosts.firstIndex(where: { $0.id == hostID }) else { return }
        hosts[index].state = state; hosts[index].failure = failure
    }
}
