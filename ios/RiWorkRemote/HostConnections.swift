import Foundation
import Observation
import RiWorkCore

/// A host's socket, lent by `HostConnections` for some work and given back with `release`.
struct HostLease: Sendable {
    let id = UUID()
    let hostID: String
    let transport: any RemoteTransport
    /// The pairing the socket's session authenticated with (what a bound request is checked against).
    let pairing: Pairing
    /// What the desktop announced when the session began.
    let features: DesktopFeatures
    /// The session it was lent in; a socket opened again starts another.
    let session: Int
}

enum HostConnectionError: LocalizedError, Equatable {
    /// A v2 pairing whose invite is still pending: only the explicit connection, which saves the key it establishes, may redeem it.
    case unfinishedPairing
    /// The app is in the background, where no socket opens.
    case background
    var errorDescription: String? {
        switch self {
        case .unfinishedPairing: "Open this desktop once to finish pairing."
        case .background: ConnectionState.suspended.label
        }
    }
}

/// The phone's relay sockets: one per paired host, and nothing else opens one. The relay allows one mobile socket per pairing, so the
/// interactive model (`RemoteModel`, through `interactive`), the All hosts overview, the decision responder and the shared screens'
/// `relay.call` all borrow the host's one socket; none of them opens a second one or closes another's. Every request on a socket
/// counts against its `RequestBudget` (`RelayClient`).
///
/// - The interactive host's socket is held for as long as the model is connected to it. Opening it reuses the socket when the host
///   already has one, and only that connection may redeem a v2 invite (the model saves the key it establishes); any other borrow of a
///   pairing whose invite is pending is refused.
/// - A socket nobody holds closes after `idleTimeout`; the one the model lets go closes at once.
/// - At most `maximumOtherSockets` sockets are open to hosts other than the interactive one. Opening one more closes the one idle the
///   longest, or waits until one is let go.
/// - In the background nothing opens, idle sockets close at once, and the rest as soon as they are let go.
@MainActor @Observable final class HostConnections {
    typealias TransportFactory = @MainActor (String) -> any RemoteTransport
    static let maximumOtherSockets = 3
    static let idleTimeout: Duration = .seconds(20)

    enum Link: Equatable { case connecting, connected, failed(String) }
    /// Each host's socket, while it is opening or open, or after its last attempt failed. A host without one is absent.
    private(set) var links: [String: Link] = [:]
    /// Hosts whose open socket's session refused `dashboard.get` although the desktop announced it (`dashboardRefused`).
    private var refusingDashboard: Set<String> = []
    /// The person's "Compress traffic" choice, read as each socket opens.
    @ObservationIgnored var compressTraffic: @MainActor () -> Bool = { true }
    /// `RemoteModel`'s connection: the interactive host's socket.
    let interactive = InteractiveConnection()

    @ObservationIgnored private let factory: TransportFactory
    @ObservationIgnored private let idle: Duration
    @ObservationIgnored private let maximumOthers: Int
    @ObservationIgnored private var sockets: [String: Socket] = [:]
    @ObservationIgnored private(set) var interactiveHostID: String?
    @ObservationIgnored private var lent: [UUID: String] = [:]
    @ObservationIgnored private var active = true
    @ObservationIgnored private var roomWaiters: [(id: UUID, continuation: CheckedContinuation<Void, any Error>)] = []

    /// One host's socket. Its transport is kept for the host's lifetime and opened again as needed.
    @MainActor private final class Socket {
        let transport: any RemoteTransport
        var open = false
        var session = 0
        /// The pairing it was opened with, and the one its session established (they differ after a v2 invite is redeemed).
        var openedWith: Pairing?
        var established: Pairing?
        var features = DesktopFeatures()
        /// The attempt under way, and its id: an attempt overtaken by a close is ignored when it ends.
        var opening: Task<Void, any Error>?
        var attempt: UUID?
        /// Opening and closing run in the order asked.
        var tail: Task<Void, Never>?
        var holds = 0
        var lastUse = ContinuousClock.now
        var idleClose: Task<Void, Never>?
        init(transport: any RemoteTransport) { self.transport = transport }
        func matches(_ pairing: Pairing) -> Bool { openedWith == pairing || established == pairing }
        var busy: Bool { open || opening != nil }
    }

    init(idleTimeout: Duration = HostConnections.idleTimeout, maximumOtherSockets: Int = HostConnections.maximumOtherSockets,
         factory: @escaping TransportFactory = { _ in RelayClient() }) {
        idle = idleTimeout; maximumOthers = max(1, maximumOtherSockets); self.factory = factory
        interactive.owner = self
    }

    // MARK: Borrowing

    /// Lends `desktop`'s socket, opening it if it is not open. This is never the explicit connection: a pairing whose v2 invite is still
    /// pending is refused. Give the lease back with `release`.
    func borrow(_ desktop: SavedDesktop) async throws -> HostLease {
        guard desktop.pairing.v == 1 || desktop.pairing.root_key != nil else { throw HostConnectionError.unfinishedPairing }
        guard active else { throw HostConnectionError.background }
        let hostID = desktop.id, socket = socket(for: hostID)
        hold(socket)
        do {
            try await open(socket, hostID: hostID, pairing: desktop.pairing, allowLocalDevelopment: desktop.allowLocalDevelopment, explicit: false)
            guard active else { throw HostConnectionError.background }
            guard let established = socket.established else { throw RemoteError.disconnected }
            let lease = HostLease(hostID: hostID, transport: socket.transport, pairing: established, features: announced(socket, hostID: hostID), session: socket.session)
            lent[lease.id] = hostID
            return lease
        } catch {
            letGo(socket, hostID: hostID)
            throw error
        }
    }

    /// Gives a lease back; returns the socket's close when that was its last holder and it closes now. Giving it back twice does nothing.
    @discardableResult func release(_ lease: HostLease) -> Task<Void, Never>? {
        guard let hostID = lent.removeValue(forKey: lease.id), let socket = sockets[hostID] else { return nil }
        return letGo(socket, hostID: hostID)
    }

    /// Whether the session a lease was lent in is still the socket's.
    func isCurrent(_ lease: HostLease) -> Bool {
        guard lent[lease.id] != nil, let socket = sockets[lease.hostID] else { return false }
        return socket.open && socket.session == lease.session
    }

    /// What the host announced when its open socket's session began, less a `dashboard` it has refused since; nil while it has none.
    func features(of hostID: String) -> DesktopFeatures? {
        guard let socket = sockets[hostID], socket.open else { return nil }
        return announced(socket, hostID: hostID)
    }

    /// The host refused `dashboard.get` in the lease's session although it announced it (a Mac whose connector is newer than its Somtus).
    /// For the rest of that session its features leave `dashboard` out, so nobody asks again, as `RemoteModel.dashboardSupport` does for
    /// the interactive host.
    func dashboardRefused(_ lease: HostLease) {
        guard isCurrent(lease) else { return }
        refusingDashboard.insert(lease.hostID)
    }

    /// The host was removed: its socket closes and its transport is let go.
    func forget(_ hostID: String) {
        shut(hostID)
        if interactiveHostID == hostID { interactiveHostID = nil }
        sockets[hostID] = nil
        links[hostID] = nil
    }

    /// The app entered the background (false) or came back. In the background no socket opens; the idle ones close now, under a background
    /// task so they are gone before iOS suspends the app, and the rest as soon as they are let go.
    func setActive(_ value: Bool) {
        guard active != value else { return }
        active = value
        wakeRoomWaiters()
        guard !value else { return }
        let closing = sockets.filter { $0.value.holds == 0 && $0.value.busy }.compactMap { shut($0.key) }
        if !closing.isEmpty { BackgroundGrace.run("Somtus sockets close") { for task in closing { await task.value } } }
    }

    /// Asks every open socket for compressed replies or not (`RemoteModel.setCompressTraffic`); the next ones open asking the same.
    func setCompression(_ enabled: Bool) async {
        for socket in sockets.values where socket.busy { await socket.transport.setCompression(enabled) }
    }

    // MARK: The interactive host

    /// Opens the interactive host's socket (`InteractiveConnection.connect`), reusing the one it has; returns what the session established
    /// and the session's number.
    fileprivate func openInteractive(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> (Pairing, Int) {
        let hostID = pairing.route_id
        if let previous = interactiveHostID, previous != hostID { await closeInteractive() }
        let socket = socket(for: hostID)
        if interactiveHostID != hostID {
            interactiveHostID = hostID
            hold(socket)
            // That socket no longer counts against the others.
            wakeRoomWaiters()
        }
        try await open(socket, hostID: hostID, pairing: pairing, allowLocalDevelopment: allowLocalDevelopment, explicit: true)
        guard interactiveHostID == hostID, sockets[hostID] === socket, let established = socket.established else { throw RemoteError.disconnected }
        // A socket that was already open keeps the compression it was asked for; ask again (nothing is sent when it is the same).
        await socket.transport.setCompression(compressTraffic())
        return (established, socket.session)
    }

    /// The model let the interactive host go. Its socket closes now unless someone else holds it.
    fileprivate func closeInteractive() async {
        guard let hostID = interactiveHostID else { return }
        interactiveHostID = nil
        guard let socket = sockets[hostID] else { return }
        socket.holds = max(0, socket.holds - 1)
        socket.lastUse = .now
        if socket.holds == 0 { await shut(hostID)?.value }
        wakeRoomWaiters()
    }

    /// The interactive host's transport while its session is still `session`.
    fileprivate func interactiveTransport(host: String, session: Int) -> (any RemoteTransport)? {
        guard interactiveHostID == host, let socket = sockets[host], socket.open, socket.session == session else { return nil }
        return socket.transport
    }

    // MARK: Sockets

    private func announced(_ socket: Socket, hostID: String) -> DesktopFeatures {
        var features = socket.features
        if refusingDashboard.contains(hostID) { features.dashboard = false }
        return features
    }

    private func socket(for hostID: String) -> Socket {
        if let socket = sockets[hostID] { return socket }
        let socket = Socket(transport: factory(hostID))
        sockets[hostID] = socket
        return socket
    }

    private func hold(_ socket: Socket) {
        socket.holds += 1
        socket.idleClose?.cancel(); socket.idleClose = nil
        socket.lastUse = .now
    }

    @discardableResult private func letGo(_ socket: Socket, hostID: String) -> Task<Void, Never>? {
        socket.holds = max(0, socket.holds - 1)
        socket.lastUse = .now
        guard socket.holds == 0, sockets[hostID] === socket else { return nil }
        defer { wakeRoomWaiters() }
        if idle <= .zero { return shut(hostID) }
        guard active else {
            let closing = shut(hostID)
            // On the way to the background: closed before iOS suspends the app, so no registration outlives it at the relay.
            if let closing { BackgroundGrace.run("Somtus socket close") { await closing.value } }
            return closing
        }
        closeWhenIdle(socket, hostID: hostID)
        return nil
    }

    private func closeWhenIdle(_ socket: Socket, hostID: String) {
        socket.idleClose?.cancel()
        socket.idleClose = Task { [weak self, idle] in
            do { try await Task.sleep(for: idle) } catch { return }
            guard let self, self.sockets[hostID] === socket, socket.holds == 0 else { return }
            self.shut(hostID)
        }
    }

    /// Makes sure `socket` has a session for `pairing`: the one it has when it is open for that pairing and still up, else a new one (shared
    /// with an attempt already under way). The caller holds the socket meanwhile.
    private func open(_ socket: Socket, hostID: String, pairing: Pairing, allowLocalDevelopment: Bool, explicit: Bool) async throws {
        while true {
            guard sockets[hostID] === socket else { throw RemoteError.disconnected }
            if let opening = socket.opening {
                // Somebody is opening it: that attempt is this one's too, its failure included.
                try await Self.outcome(of: opening)
                continue
            }
            if socket.open, socket.matches(pairing) {
                let session = socket.session
                if await socket.transport.isConnected() { return }
                // The relay or the desktop ended it (a timeout, a close): noted, and opened again below.
                if socket.session == session, socket.open, socket.opening == nil { socket.open = false; links[hostID] = nil }
                continue
            }
            if !explicit, interactiveHostID != hostID {
                try await makeRoom(for: hostID)
                if socket.opening != nil || (socket.open && socket.matches(pairing)) { continue }
            }
            try await Self.outcome(of: startOpening(socket, hostID: hostID, pairing: pairing, allowLocalDevelopment: allowLocalDevelopment))
        }
    }

    private func startOpening(_ socket: Socket, hostID: String, pairing: Pairing, allowLocalDevelopment: Bool) -> Task<Void, any Error> {
        let attempt = UUID(), transport = socket.transport, compress = compressTraffic(), previous = socket.tail
        socket.open = false
        let task = Task { @MainActor [weak self] in
            await previous?.value
            do {
                guard self?.sockets[hostID] === socket, socket.attempt == attempt else { throw RemoteError.disconnected }
                await transport.setCompression(compress)
                let established = try await transport.connect(pairing: pairing, allowLocalDevelopment: allowLocalDevelopment)
                let features = await transport.desktopFeatures()
                guard socket.attempt == attempt else { throw RemoteError.disconnected }
                self?.opened(socket, hostID: hostID, pairing: pairing, established: established, features: features)
            } catch {
                if socket.attempt == attempt { self?.failed(socket, hostID: hostID, error: error) }
                throw error
            }
        }
        socket.attempt = attempt
        socket.opening = task
        socket.tail = Task { _ = try? await task.value }
        links[hostID] = .connecting
        return task
    }

    private func opened(_ socket: Socket, hostID: String, pairing: Pairing, established: Pairing, features: DesktopFeatures) {
        socket.attempt = nil; socket.opening = nil
        socket.open = true; socket.session &+= 1
        socket.openedWith = pairing; socket.established = established; socket.features = features
        refusingDashboard.remove(hostID)
        if sockets[hostID] === socket { links[hostID] = .connected }
        // Everyone who wanted it may have let go while it opened.
        if socket.holds == 0 { if active, idle > .zero { closeWhenIdle(socket, hostID: hostID) } else { shut(hostID) } }
    }

    private func failed(_ socket: Socket, hostID: String, error: any Error) {
        socket.attempt = nil; socket.opening = nil; socket.open = false
        if sockets[hostID] === socket { links[hostID] = .failed(error.localizedDescription) }
        wakeRoomWaiters()
    }

    /// Closes a socket: an attempt under way is dropped, every request on it fails, and its next opening waits for the close.
    @discardableResult private func shut(_ hostID: String) -> Task<Void, Never>? {
        guard let socket = sockets[hostID] else { return nil }
        socket.idleClose?.cancel(); socket.idleClose = nil
        socket.attempt = nil; socket.opening = nil
        socket.open = false; socket.openedWith = nil; socket.established = nil; socket.features = DesktopFeatures()
        links[hostID] = nil; refusingDashboard.remove(hostID)
        let transport = socket.transport, previous = socket.tail
        // Disconnecting first ends an attempt already connecting; once that attempt is over, once more for one that connected anyway.
        let task = Task { await transport.disconnect(); await previous?.value; await transport.disconnect() }
        socket.tail = task
        wakeRoomWaiters()
        return task
    }

    // MARK: The other hosts' limit

    /// Room for one more socket to a host other than the interactive one: the socket idle the longest closes, or this waits for one to be
    /// let go.
    private func makeRoom(for hostID: String) async throws {
        while true {
            guard active else { throw HostConnectionError.background }
            let others = sockets.filter { $0.key != hostID && $0.key != interactiveHostID && $0.value.busy }
            if others.count < maximumOthers { return }
            if let oldest = others.filter({ $0.value.holds == 0 }).min(by: { $0.value.lastUse < $1.value.lastUse }) {
                shut(oldest.key)
                continue
            }
            try await waitForRoom()
        }
    }

    private func waitForRoom() async throws {
        let id = UUID()
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
                if Task.isCancelled { continuation.resume(throwing: CancellationError()); return }
                roomWaiters.append((id, continuation))
            }
        } onCancel: {
            Task { @MainActor [weak self] in
                guard let self, let index = self.roomWaiters.firstIndex(where: { $0.id == id }) else { return }
                self.roomWaiters.remove(at: index).continuation.resume(throwing: CancellationError())
            }
        }
    }

    /// Something may have made room: every waiter looks again.
    private func wakeRoomWaiters() {
        let waiting = roomWaiters
        roomWaiters.removeAll()
        for waiter in waiting { waiter.continuation.resume() }
    }

    /// Waits for an attempt's outcome, but lets go when the caller is cancelled (the attempt goes on for whoever else waits for it).
    private static func outcome(of task: Task<Void, any Error>) async throws {
        let gate = Gate()
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
                gate.install(continuation)
                Task {
                    do { try await task.value; gate.finish(nil) } catch { gate.finish(error) }
                }
            }
        } onCancel: { gate.finish(CancellationError()) }
    }

    /// Resumes a continuation once, whichever comes first: the outcome or the caller's cancellation.
    private final class Gate: @unchecked Sendable {
        private let lock = NSLock()
        private var continuation: CheckedContinuation<Void, any Error>?
        private var result: Result<Void, any Error>?
        private var done = false
        func install(_ continuation: CheckedContinuation<Void, any Error>) {
            lock.lock()
            if let result { done = true; lock.unlock(); continuation.resume(with: result); return }
            self.continuation = continuation
            lock.unlock()
        }
        func finish(_ error: (any Error)?) {
            lock.lock()
            guard !done, result == nil else { lock.unlock(); return }
            let outcome: Result<Void, any Error> = error.map { .failure($0) } ?? .success(())
            if let continuation { done = true; self.continuation = nil; lock.unlock(); continuation.resume(with: outcome); return }
            result = outcome
            lock.unlock()
        }
    }
}

/// `RemoteModel`'s connection: the interactive host's socket, borrowed from `HostConnections` for as long as the model is connected to
/// that host. Requests go to the session the model connected in; once that session has ended (even if the socket was opened again
/// since), this connection reports itself disconnected so the model connects afresh.
@MainActor final class InteractiveConnection: RemoteTransport {
    fileprivate weak var owner: HostConnections?
    private var session: (host: String, number: Int)?

    @discardableResult
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing {
        guard let owner else { throw RemoteError.disconnected }
        session = nil
        let (established, number) = try await owner.openInteractive(pairing: pairing, allowLocalDevelopment: allowLocalDevelopment)
        session = (pairing.route_id, number)
        return established
    }
    func disconnect() async {
        session = nil
        await owner?.closeInteractive()
    }
    func isConnected() async -> Bool {
        guard let transport = current() else { return false }
        return await transport.isConnected()
    }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        try await required().request(method: method, params: params, id: id)
    }
    func request(method: String, params: [String: JSONValue], id: String, boundTo identity: UploadConnectionIdentity) async throws -> JSONValue {
        try await required().request(method: method, params: params, id: id, boundTo: identity)
    }
    func timedRequest(method: String, params: [String: JSONValue], id: String) async throws -> TimedReply {
        try await required().timedRequest(method: method, params: params, id: id)
    }
    func desktopFeatures() async -> DesktopFeatures {
        guard let transport = current() else { return DesktopFeatures() }
        return await transport.desktopFeatures()
    }
    func setCompression(_ enabled: Bool) async { await owner?.setCompression(enabled) }
    func compressionActive() async -> Bool {
        guard let transport = current() else { return false }
        return await transport.compressionActive()
    }

    private func current() -> (any RemoteTransport)? {
        guard let session, let owner else { return nil }
        return owner.interactiveTransport(host: session.host, session: session.number)
    }
    private func required() throws -> any RemoteTransport {
        guard let transport = current() else { throw RemoteError.disconnected }
        return transport
    }
}
