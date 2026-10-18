import Foundation
import Observation
import RiWorkCore

/// Independent, read-only overview connections. Never takes the active model's transport, input focus, or terminal viewport.
/// The relay allows one mobile socket per pairing: the currently active host is mirrored from RemoteModel, not connected twice.
@MainActor @Observable final class HostDashboardStore {
    typealias TransportFactory = @Sendable (SavedDesktop) -> any RemoteTransport
    /// One URL session for every temporary socket: a session made for each read would never be invalidated.
    nonisolated static let sharedConnector = URLSessionConnector()

    private(set) var hosts: [HostDashboardEntry] = []
    private(set) var visible = false
    private(set) var sceneActive = true
    var aggregate: HostDashboardAggregate { HostDashboardAggregate(hosts: hosts) }
    /// The person's "Compress traffic" choice, read as each temporary connection opens, as the interactive one reads it.
    @ObservationIgnored var compressTraffic: @MainActor () -> Bool = { true }

    @ObservationIgnored private let transportFactory: TransportFactory
    @ObservationIgnored private let refreshInterval: Duration
    @ObservationIgnored private let maximumConcurrentHosts: Int
    @ObservationIgnored private var desktops: [SavedDesktop] = []
    @ObservationIgnored private var activeHostID: String?
    @ObservationIgnored private var generation = UUID()
    @ObservationIgnored private var loop: Task<Void, Never>?
    @ObservationIgnored private var round: Task<Void, Never>?
    @ObservationIgnored private var cleanup: Task<Void, Never>?
    @ObservationIgnored private var clients: [String: (id: UUID, transport: any RemoteTransport)] = [:]

    init(refreshInterval: Duration = .seconds(15), maximumConcurrentHosts: Int = 3,
         transportFactory: @escaping TransportFactory = { _ in RelayClient(connector: HostDashboardStore.sharedConnector) }) {
        self.refreshInterval = max(.milliseconds(100), refreshInterval)
        self.maximumConcurrentHosts = max(1, min(4, maximumConcurrentHosts)); self.transportFactory = transportFactory
    }

    /// Called when the root host overview enters/leaves the screen. No host connects while it is hidden or the scene is inactive.
    func setVisible(_ visible: Bool, desktops: [SavedDesktop], sceneActive: Bool = true, activeHostID: String? = nil) {
        let changed = self.visible != visible || self.sceneActive != sceneActive
        self.visible = visible; self.sceneActive = sceneActive
        updateDesktops(desktops, activeHostID: activeHostID)
        if changed { reconcileLifecycle() }
    }

    /// Passing the active route prevents duplicate registration against the main model's live connection.
    func updateDesktops(_ desktops: [SavedDesktop], activeHostID: String? = nil) {
        // A malformed/duplicated library cannot schedule two clients on the same route.
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

    /// Await before activating a host in RemoteModel: all overview sockets are closed before the main connection registers.
    func stop() async {
        visible = false
        reconcileLifecycle()
        await cleanup?.value
    }

    /// Answers a card over a temporary connection of its own to that exact host, leaving the interactive host alone. The relay allows
    /// one mobile socket per pairing: call after `stop()`, and only for a host the interactive model holds no connection to.
    func respond(to decision: DashboardDecision, on desktop: SavedDesktop, command: ChatCommand) async throws {
        // As for a read: a single-use invite is redeemed only by the explicit connection that saves its key.
        guard desktop.pairing.v == 1 || desktop.pairing.root_key != nil else { throw DashboardResponseError.notSent("Open this desktop once to finish pairing.") }
        await cleanup?.value
        let client = transportFactory(desktop)
        do {
            await client.setCompression(compressTraffic())
            let established: Pairing
            do { established = try await client.connect(pairing: desktop.pairing, allowLocalDevelopment: desktop.allowLocalDevelopment) }
            catch { throw DashboardResponseError.notSent(RemoteModel.dashboardFailureText(error)) }
            guard established == desktop.pairing else { throw DashboardResponseError.notSent("Open this desktop once to finish pairing.") }
            // The connection is this card's alone; the transport still binds the command to the host it authenticated.
            try await DashboardResponder.respond(decision, command: command, client: client, identity: UploadConnectionIdentity(pairing: established)) { true }
        } catch {
            await client.disconnect()
            throw error
        }
        await client.disconnect()
    }

    private func reconcileLifecycle() {
        generation = UUID()
        loop?.cancel(); loop = nil
        round?.cancel(); round = nil
        let previousCleanup = cleanup
        let closing = clients.values.map(\.transport)
        clients.removeAll()
        let closed = Task {
            await previousCleanup?.value
            await withTaskGroup(of: Void.self) { group in
                for client in closing { group.addTask { await client.disconnect() } }
            }
        }
        cleanup = closed
        // On the way to the background iOS may suspend the app before these sockets close, and one left open would still hold this
        // pairing's registration with the relay when the interactive connection comes back.
        if !closing.isEmpty { BackgroundGrace.run("RiWork overview close") { await closed.value } }
        guard visible, sceneActive else {
            for index in hosts.indices where hosts[index].id != activeHostID { hosts[index].state = .paused }
            return
        }
        let token = generation
        loop = Task { [weak self, refreshInterval] in
            while !Task.isCancelled {
                guard let self, self.generation == token else { return }
                await self.refresh()
                do { try await Task.sleep(for: refreshInterval) } catch { return }
            }
        }
    }

    private func readRound(_ pending: [SavedDesktop], token: UUID) async {
        await cleanup?.value
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
    private func checkCurrent(_ token: UUID) throws { guard current(token) else { throw CancellationError() } }

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
        let client = transportFactory(desktop), connectionID = UUID()
        clients[desktop.id] = (connectionID, client)
        do {
            await client.setCompression(compressTraffic())
            let established = try await client.connect(pairing: desktop.pairing, allowLocalDevelopment: desktop.allowLocalDevelopment)
            try checkCurrent(token)
            guard established == desktop.pairing else { throw RemoteError.protocolViolation("Open this desktop once to finish pairing.") }
            let features = await client.desktopFeatures()
            try checkCurrent(token)
            if features.dashboard {
                let wire = try await client.request(method: "dashboard.get", params: [:], id: UUID().uuidString.lowercased())
                let snapshot = try DashboardSnapshot(wire: wire)
                if current(token), let index = hosts.firstIndex(where: { $0.id == desktop.id }) {
                    hosts[index].snapshot = snapshot; hosts[index].readAt = .now
                    hosts[index].state = .ready; hosts[index].failure = nil
                }
            } else {
                install(hostID: desktop.id, token: token, state: .unsupported, failure: "Update RiWork on this host to read its dashboard.")
            }
        } catch is CancellationError {
        } catch {
            let unsupported = RemoteError.isUnsupportedMethod(error)
            install(hostID: desktop.id, token: token, state: unsupported ? .unsupported : .offline,
                    failure: unsupported ? "Update RiWork on this host to read its dashboard." : RemoteModel.dashboardFailureText(error))
        }
        await finish(client, hostID: desktop.id, connectionID: connectionID)
        // A read that ended without an answer (stopped, replaced, cancelled) leaves the host as it was, not "Reading…" for good.
        if clients[desktop.id] == nil, let index = hosts.firstIndex(where: { $0.id == desktop.id }), hosts[index].state == .refreshing {
            hosts[index].state = previous
        }
    }

    private func install(hostID: String, token: UUID, state: HostDashboardEntry.State, failure: String) {
        guard current(token), let index = hosts.firstIndex(where: { $0.id == hostID }) else { return }
        hosts[index].state = state; hosts[index].failure = failure
    }

    private func finish(_ client: any RemoteTransport, hostID: String, connectionID: UUID) async {
        await client.disconnect()
        if clients[hostID]?.id == connectionID { clients.removeValue(forKey: hostID) }
    }
}
