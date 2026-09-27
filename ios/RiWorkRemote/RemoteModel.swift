import Foundation
import Observation
import RiWorkCore

struct Library: Codable {
    var desktops: [SavedDesktop] = []
    var selectedDesktopID: String?
}

enum ConnectionState: Equatable {
    case disconnected, connecting, connected, suspended, failed
    var label: String { switch self { case .disconnected: "Disconnected"; case .connecting: "Connecting…"; case .connected: "Connected"; case .suspended: "Paused in background"; case .failed: "Connection unavailable" } }
    var symbol: String { switch self { case .connected: "checkmark.shield.fill"; case .connecting: "arrow.triangle.2.circlepath"; default: "wifi.slash" } }
}

@MainActor @Observable final class RemoteModel {
    var desktops: [SavedDesktop] = []
    var selectedDesktopID: String?
    var state: ConnectionState = .disconnected
    var error: String?
    var projects: [RemoteProject] = []
    var worktrees: [RemoteWorktree] = []
    var tasks: [RemoteTask] = []
    var shells: [RemoteSession] = []
    var orchestrators: [RemoteSession] = []
    var output = ""
    var outputSessionID: String?
    var lastOutputAt: Date?
    var snapshotStale = true
    var loading = false
    var sending = false
    var deliveryNotice: String?
    var terminalViewport: TerminalViewport?
    var appliedViewport: TerminalViewport?
    var viewportSessionID: String?
    var viewportError: String?
    var terminalVisible = false
    var draft = ""
    @ObservationIgnored private let keychain: KeychainStore
    @ObservationIgnored private let client: any RemoteTransport
    @ObservationIgnored private var polling: Task<Void, Never>?
    @ObservationIgnored private var generation = UUID()
    @ObservationIgnored private var wantsConnection = false
    // A not_found UUID is excluded until an explicit refresh or fresh connection.
    private var missingSessionIDs: Set<String> = []
    @ObservationIgnored private var viewportBusy = false
    @ObservationIgnored private var viewportWaiters: [CheckedContinuation<Void, Never>] = []
    @ObservationIgnored private var viewportUpdateScheduled = false
    @ObservationIgnored private var failedViewport: ViewportTarget?
    private struct ViewportTarget: Equatable { let shellID: String; let viewport: TerminalViewport }

    init(client: any RemoteTransport = RelayClient(), keychain: KeychainStore = KeychainStore()) {
        self.client = client
        self.keychain = keychain
        do {
            let library = try keychain.read(Library.self) ?? Library()
            desktops = library.desktops
            selectedDesktopID = library.selectedDesktopID ?? desktops.first?.id
        } catch { self.error = error.localizedDescription }
    }
    var desktop: SavedDesktop? { desktops.first(where: { $0.id == selectedDesktopID }) }
    var projectID: String? { desktop?.selectedProjectID }
    var sessionID: String? { desktop?.selectedSessionID }
    var session: RemoteSession? { sessions.first(where: { $0.id == sessionID }) }
    var pendingInput: PendingInput? { desktop?.pendingInput }
    var sessions: [RemoteSession] {
        (orchestrators.filter { $0.project_id == projectID } + shells).sorted {
            if $0.kind != $1.kind { return $0.kind == "orchestrator" }
            return $0.created_at_unix > $1.created_at_unix
        }
    }
    var openSessions: [RemoteSession] { sessions.filter { $0.alive && !missingSessionIDs.contains($0.id) } }
    var viewportReady: Bool { !terminalVisible || (viewportSessionID == sessionID && appliedViewport == terminalViewport && terminalViewport != nil) }
    // Keep focus/keyboard stable while fitting the terminal. Submission still waits for its grid.
    var canEditDraft: Bool { state == .connected && session?.alive == true && !missingSessionIDs.contains(sessionID ?? "") && !sending && pendingInput == nil }
    var canSend: Bool { canEditDraft && !snapshotStale && viewportError == nil && viewportReady }
    func reportViewport(_ viewport: TerminalViewport?) {
        guard let viewport, viewport != terminalViewport else { return }
        terminalViewport = viewport
        failedViewport = nil; viewportError = nil
        scheduleViewportUpdate()
    }
    func setTerminalVisible(_ visible: Bool) {
        terminalVisible = visible
        scheduleViewportUpdate()
    }
    private func scheduleViewportUpdate() {
        guard !viewportUpdateScheduled else { return }
        viewportUpdateScheduled = true
        Task {
            try? await Task.sleep(for: .milliseconds(150))
            viewportUpdateScheduled = false
            let token = generation
            do {
                try await synchronizeViewport(token: token)
                if terminalVisible, state == .connected { await readOutput() }
            } catch { if generation == token { self.error = error.localizedDescription } }
        }
    }
    private func acquireViewport() async {
        if viewportBusy { await withCheckedContinuation { viewportWaiters.append($0) } }
        else { viewportBusy = true }
    }
    private func releaseViewportLock() {
        if viewportWaiters.isEmpty { viewportBusy = false }
        else { viewportWaiters.removeFirst().resume() }
    }
    /// Serializes state operations so late tab changes cannot leave the wrong shell pinned.
    private func synchronizeViewport(token: UUID, forceRelease: Bool = false) async throws {
        await acquireViewport()
        defer { releaseViewportLock() }
        guard generation == token, await client.isConnected() else { return }
        let target: ViewportTarget?
        if !forceRelease, state == .connected, terminalVisible, let id = sessionID, let viewport = terminalViewport {
            target = ViewportTarget(shellID: id, viewport: viewport)
        } else { target = nil }
        if let previous = viewportSessionID, previous != target?.shellID {
            let result = try await rpc("shell.resize.clear", ["shell_id": .string(previous)])
            guard result["shell_id"].string == previous, result["status"].string == "cleared" else { throw RemoteError.protocolViolation("Terminal release identity mismatch.") }
            guard generation == token else { return }
            viewportSessionID = nil; appliedViewport = nil
        }
        guard let target else { viewportError = nil; failedViewport = nil; return }
        if viewportSessionID == target.shellID, appliedViewport == target.viewport { return }
        if failedViewport == target { throw RemoteError.remote(viewportError ?? "Terminal sizing failed. Refresh or reconnect.") }
        do {
            let result = try await rpc("shell.resize", ["shell_id": .string(target.shellID), "columns": .number(Double(target.viewport.columns)), "rows": .number(Double(target.viewport.rows))])
            guard result["shell_id"].string == target.shellID,
                  result["columns"] == .number(Double(target.viewport.columns)),
                  result["rows"] == .number(Double(target.viewport.rows)) else { throw RemoteError.protocolViolation("Desktop terminal dimensions did not match the viewport.") }
            guard generation == token else { return }
            viewportSessionID = target.shellID; appliedViewport = target.viewport
            viewportError = nil; failedViewport = nil
        } catch {
            if generation == token { failedViewport = target; viewportError = error.localizedDescription }
            throw error
        }
    }
    func persist() throws { try keychain.write(Library(desktops: desktops, selectedDesktopID: selectedDesktopID)) }
    private func updateDesktop(_ update: (inout SavedDesktop) -> Void) throws {
        guard let i = desktops.firstIndex(where: { $0.id == selectedDesktopID }) else { return }
        let previous = desktops[i]
        update(&desktops[i])
        do { try persist() } catch { desktops[i] = previous; throw error }
    }
    func add(pairingText: String, name: String, allowLocal: Bool) throws -> String {
        let pairing = try Pairing.parse(pairingText, allowLocalDevelopment: allowLocal)
        guard !desktops.contains(where: { $0.id == pairing.route_id || $0.pairing.device_id == pairing.device_id }) else { throw RemoteError.remote("This device pairing is already saved. Choose it in Desktops.") }
        let cleaned = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let saved = SavedDesktop(name: cleaned.isEmpty ? "RiWork desktop" : String(cleaned.prefix(80)), pairing: pairing, allowLocalDevelopment: allowLocal)
        desktops.append(saved)
        do { try persist() } catch { desktops.removeAll { $0.id == saved.id }; throw error }
        return saved.id
    }
    func rename(id: String, name: String) throws {
        guard let i = desktops.firstIndex(where: { $0.id == id }) else { return }
        let cleaned = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !cleaned.isEmpty else { throw RemoteError.remote("Enter a desktop name.") }
        let old = desktops[i].name; desktops[i].name = String(cleaned.prefix(80))
        do { try persist() } catch { desktops[i].name = old; throw error }
    }
    func remove(id: String) async throws {
        if selectedDesktopID == id { await disconnect(); selectedDesktopID = nil; clearSnapshot() }
        let previous = desktops
        desktops.removeAll { $0.id == id }
        do { try persist() } catch { desktops = previous; throw error }
    }
    func activate(_ id: String) async {
        if selectedDesktopID != id {
            await disconnect()
            selectedDesktopID = id; clearSnapshot(); draft = ""; deliveryNotice = nil
            do { try persist() } catch { self.error = error.localizedDescription; return }
        }
        if state != .connected && state != .connecting { await connect() }
    }
    func connect() async {
        guard let desktop, state != .connecting else { return }
        wantsConnection = true
        polling?.cancel()
        generation = UUID(); let token = generation
        viewportSessionID = nil; appliedViewport = nil; failedViewport = nil; viewportError = nil
        missingSessionIDs = []
        state = .connecting; error = nil; snapshotStale = true
        do {
            try await client.connect(pairing: desktop.pairing, allowLocalDevelopment: desktop.allowLocalDevelopment)
            guard generation == token else { return }
            state = .connected
            try await refresh(token: token)
            guard generation == token else { return }
            startPolling(token: token)
        } catch {
            guard generation == token else { return }
            state = .failed; snapshotStale = true; self.error = error.localizedDescription
            await client.disconnect()
        }
    }
    func disconnect(background: Bool = false) async {
        let token = generation
        if !background { wantsConnection = false }
        state = background ? .suspended : .disconnected
        snapshotStale = true; loading = false
        polling?.cancel(); polling = nil
        // Clear only this authenticated connection's override before closing, when possible.
        try? await synchronizeViewport(token: token, forceRelease: true)
        guard generation == token else { return }
        generation = UUID()
        viewportSessionID = nil; appliedViewport = nil; failedViewport = nil
        await client.disconnect()
    }
    func resume() async { if wantsConnection, state == .suspended { await connect() } }
    private func clearSnapshot() { projects = []; worktrees = []; tasks = []; shells = []; orchestrators = []; output = ""; outputSessionID = nil; lastOutputAt = nil; snapshotStale = true }
    func refresh() async {
        guard state == .connected else { return }
        failedViewport = nil; viewportError = nil; missingSessionIDs = []
        do { try await refresh(token: generation) } catch { handle(error) }
    }
    private func rpc(_ method: String, _ params: [String: JSONValue] = [:], id: String = UUID().uuidString.lowercased()) async throws -> JSONValue {
        try await client.request(method: method, params: params, id: id)
    }
    private func refresh(token: UUID) async throws {
        loading = true
        defer { if generation == token { loading = false } }
        let listed = try await rpc("projects.list")["projects"].decode([RemoteProject].self)
        let managers = try await rpc("orchestrators.list")["orchestrators"].decode([RemoteSession].self)
        guard generation == token else { return }
        projects = listed; orchestrators = managers
        if projectID == nil, let first = listed.first { try updateDesktop { $0.selectedProjectID = first.id } }
        if let project = projectID { try await loadProject(project, token: token) }
        else { snapshotStale = false }
        if sessionID != nil { await readOutput() }
    }
    func chooseProject(_ id: String) async {
        guard projectID != id else { return }
        do {
            if state == .connected { try? await synchronizeViewport(token: generation, forceRelease: true) }
            try updateDesktop {
                if let previousProject = $0.selectedProjectID, let previousSession = $0.selectedSessionID {
                    var selections = $0.projectSessionIDs ?? [:]; selections[previousProject] = previousSession; $0.projectSessionIDs = selections
                }
                $0.selectedProjectID = id
                $0.selectedSessionID = $0.projectSessionIDs?[id]
            }
            output = ""; outputSessionID = nil; lastOutputAt = nil; draft = ""; deliveryNotice = nil
            worktrees = []; tasks = []; shells = []; snapshotStale = true
            if state == .connected {
                try await loadProject(id, token: generation)
                await readOutput()
            }
        } catch { handle(error) }
    }
    private func loadProject(_ id: String, token: UUID) async throws {
        let params: [String: JSONValue] = ["project_id": .string(id)]
        let trees = try await rpc("worktrees.list", params)["worktrees"].decode([RemoteWorktree].self)
        let jobs = try await rpc("tasks.list", params)["tasks"].decode([RemoteTask].self)
        let workers = try await rpc("shells.list", params)["shells"].decode([RemoteSession].self)
        guard generation == token, projectID == id else { return }
        worktrees = trees; tasks = jobs; shells = workers
        try reconcileSelectedSession()
        if sessionID == nil { try await synchronizeViewport(token: token) }
    }
    /// Metadata refresh selects only an existing live tab. Never submits or retries input.
    private func reconcileSelectedSession() throws {
        let selected = sessionID
        if let selected, openSessions.contains(where: { $0.id == selected }) { return }
        let replacement = openSessions.first?.id
        guard selected != replacement else { return }
        try updateDesktop {
            $0.selectedSessionID = replacement
            if let project = $0.selectedProjectID {
                var selections = $0.projectSessionIDs ?? [:]
                selections[project] = replacement
                $0.projectSessionIDs = selections
            }
            // pendingInput belongs to its original shell, even if that tab has closed.
        }
        output = ""; outputSessionID = nil; lastOutputAt = nil; snapshotStale = true
        draft = ""; deliveryNotice = nil; error = nil; viewportError = nil; failedViewport = nil
    }
    func chooseSession(_ session: RemoteSession) async {
        guard openSessions.contains(where: { $0.id == session.id }) else { return }
        do {
            try updateDesktop {
                $0.selectedSessionID = session.id
                if let project = $0.selectedProjectID {
                    var selections = $0.projectSessionIDs ?? [:]; selections[project] = session.id; $0.projectSessionIDs = selections
                }
            }
            if outputSessionID != session.id { output = ""; outputSessionID = nil; lastOutputAt = nil }
            draft = ""; deliveryNotice = nil
            await readOutput()
        } catch { handle(error) }
    }
    func readOutput() async { await readOutput(recoverMissing: true) }
    private func readOutput(recoverMissing: Bool) async {
        guard state == .connected, let id = sessionID, !missingSessionIDs.contains(id) else { return }
        let token = generation
        do {
            try await synchronizeViewport(token: token)
            guard generation == token, sessionID == id else { return }
            let result = try await rpc("shell.output", ["shell_id": .string(id), "lines": .number(500)])
            guard result["shell_id"].string == id, let text = result["output"].string else { throw RemoteError.protocolViolation("Session output identity mismatch.") }
            guard generation == token, sessionID == id else { return }
            output = TerminalText.readable(text); outputSessionID = id; lastOutputAt = Date(); snapshotStale = false
        } catch {
            guard generation == token, sessionID == id else { return }
            if case RemoteError.rpc("not_found", _) = error {
                missingSessionIDs.insert(id); snapshotStale = true
                do {
                    if recoverMissing, let project = projectID {
                        // Refresh project sessions once. The failed UUID cannot be polled again.
                        let managers = try await rpc("orchestrators.list")["orchestrators"].decode([RemoteSession].self)
                        guard generation == token, projectID == project else { return }
                        orchestrators = managers
                        try await loadProject(project, token: token)
                    } else {
                        try reconcileSelectedSession()
                        if sessionID == nil { try await synchronizeViewport(token: token) }
                    }
                    if recoverMissing, sessionID != nil { await readOutput(recoverMissing: false) }
                } catch { handle(error) }
            } else { handle(error) }
        }
    }
    func submit(expectedSessionID: String? = nil, line: String? = nil) async {
        guard canSend, let session, let desktopID = selectedDesktopID,
              expectedSessionID == nil || expectedSessionID == session.id else { return }
        let operation: PendingInput
        do {
            operation = try PendingInput(shellID: session.id, line: line ?? draft)
            // Persist before the first byte leaves this device. Storage failure prevents submission.
            try updateDesktop { $0.pendingInput = operation }
        } catch { self.error = error.localizedDescription; return }
        sending = true; deliveryNotice = nil
        defer { sending = false }
        do {
            let result = try await rpc("shell.input", ["shell_id": .string(operation.shellID), "line": .string(operation.line)], id: operation.id)
            guard result["shell_id"].string == operation.shellID, result["status"].string == "sent" else { throw RemoteError.uncertainDelivery }
            try clearPending(desktopID: desktopID, operationID: operation.id)
            if selectedDesktopID == desktopID { draft = ""; deliveryNotice = "Submitted to \(session.title) · \(session.shortID)."; await readOutput() }
        } catch {
            // Keep UUID/line on any uncertain result, reconnect, background, or app restart.
            if case RemoteError.rpc(let code, _) = error, code != "outcome_unknown" {
                do { try clearPending(desktopID: desktopID, operationID: operation.id) } catch { self.error = error.localizedDescription }
            }
            if selectedDesktopID == desktopID { self.error = error.localizedDescription; deliveryNotice = "Review session output before sending again." }
        }
    }
    private func clearPending(desktopID: String, operationID: String) throws {
        guard let i = desktops.firstIndex(where: { $0.id == desktopID }), desktops[i].pendingInput?.id == operationID else { return }
        let old = desktops[i].pendingInput; desktops[i].pendingInput = nil
        do { try persist() } catch { desktops[i].pendingInput = old; throw error }
    }
    func acknowledgeUncertainInput() throws {
        guard let desktop, let pending = desktop.pendingInput else { return }
        try clearPending(desktopID: desktop.id, operationID: pending.id)
        draft = ""; deliveryNotice = "Unconfirmed input acknowledged after review. Nothing was resent."
    }
    private func handle(_ error: any Error) {
        self.error = error.localizedDescription
        snapshotStale = true
        Task {
            if !(await client.isConnected()), state == .connected { state = .failed; snapshotStale = true; polling?.cancel() }
        }
    }
    private func startPolling(token: UUID) {
        polling = Task { [weak self] in
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(3)) } catch { return }
                guard let self, self.generation == token else { return }
                if !(await self.client.isConnected()) { self.state = .failed; self.snapshotStale = true; self.error = "Desktop disconnected. Reconnect to refresh output."; return }
                await self.readOutput()
            }
        }
    }
}
