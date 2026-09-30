import Foundation
import Observation
import UIKit
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
    var shells: [RemoteSession] = []
    var orchestrators: [RemoteSession] = []
    var output = ""
    /// Where the desktop's cursor is in `output` (Character offset), when it reported one.
    var outputCursorOffset: Int?
    /// The desktop pane is in copy mode.
    var outputInMode = false
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
    // MARK: Direct typing, layout and focus (pipeline in RemoteModel+Keys.swift)
    /// Whether the desktop understands `shell.keys`. Learned from the first call, reset by every new connection.
    var keysSupport: KeysSupport = .unknown
    /// Unsent keystrokes per (desktop, shell). Memory only: typed text is never persisted.
    var keyBuffers: [KeyBufferKey: KeyBuffer] = [:]
    var keysFull: Set<KeyBufferKey> = []
    /// Bumped when the pending-input preview may need to appear because time passed.
    var keyRevealTick = 0
    /// Counts accepted keystroke groups; the view keeps the screen scrolled to the bottom while it changes.
    var typedCount = 0
    var preferLineComposer = false
    var terminalFontSize = TerminalFontSize.standard
    /// Sessions in focus mode during this app run.
    var focusedSessionIDs: Set<String> = []
    /// Set when the Keychain library could not be read. Saved pairings are then untouched and never overwritten.
    var loadFailure: String?
    var loadFailed: Bool { loadFailure != nil }
    @ObservationIgnored private let keychain: KeychainStore
    @ObservationIgnored let client: any RemoteTransport
    @ObservationIgnored private let pollInterval: Duration
    @ObservationIgnored var polling: Task<Void, Never>?
    @ObservationIgnored var generation = UUID()
    @ObservationIgnored var wantsConnection = false
    // A not_found UUID is excluded until an explicit refresh or fresh connection.
    var missingSessionIDs: Set<String> = []
    @ObservationIgnored private var lastScreen: ShellOutput?
    @ObservationIgnored private var drainingForBackground = false
    @ObservationIgnored private var resumeAfterDrain = false
    /// Counts times the selected terminal was replaced without a tap (it closed); the view drops the keyboard so
    /// keystrokes meant for one shell never continue into another.
    var sessionAutoSwitches = 0
    @ObservationIgnored private var outputReadInFlight = false
    @ObservationIgnored private var outputReadQueued = false
    @ObservationIgnored var pollSleeper: Task<Void, Never>?
    @ObservationIgnored private var pollDueAt: Date?
    @ObservationIgnored var lastKeyActivity: Date?
    // Direct-typing plumbing (RemoteModel+Keys.swift).
    @ObservationIgnored let keyFlushInterval: Duration
    @ObservationIgnored let previewDelay: Duration
    @ObservationIgnored let reconnectBackoff: Duration
    @ObservationIgnored let defaults: UserDefaults
    @ObservationIgnored let cellMetrics: @MainActor (Double) -> (width: Double, height: Double)
    @ObservationIgnored let keepAwake: @MainActor (Bool) -> Void
    @ObservationIgnored var keySender: Task<Void, Never>?
    @ObservationIgnored var keySenderID = UUID()
    @ObservationIgnored var lastKeyBatchStart: ContinuousClock.Instant?
    @ObservationIgnored var revealTask: Task<Void, Never>?
    @ObservationIgnored var reconnectTask: Task<Void, Never>?
    @ObservationIgnored var reconnectID = UUID()
    @ObservationIgnored var noticeID = UUID()
    /// The pane size the view reported; the grid is recomputed from it whenever layout, font or focus changes.
    @ObservationIgnored var terminalArea: CGSize?
    // The project whose worktrees and shells are current for this connection; a cancelled load leaves it unset.
    @ObservationIgnored private var loadedProjectID: String?
    // Per-session `lines` that fit the desktop's 128 KiB reply cap; wide grids or multibyte scrollback need fewer.
    @ObservationIgnored private var outputLines: [String: Int] = [:]
    private static let defaultOutputLines = 500, minimumOutputLines = 20
    @ObservationIgnored private var viewportBusy = false
    @ObservationIgnored private var viewportWaiters: [CheckedContinuation<Void, Never>] = []
    @ObservationIgnored private var viewportUpdateScheduled = false
    @ObservationIgnored private var failedViewport: ViewportTarget?
    private struct ViewportTarget: Equatable { let shellID: String; let viewport: TerminalViewport }

    init(client: any RemoteTransport = RelayClient(), keychain: KeychainStore = KeychainStore(), pollInterval: Duration = .seconds(3),
         keyFlushInterval: Duration = .milliseconds(40), previewDelay: Duration = .milliseconds(300), reconnectBackoff: Duration = .seconds(1),
         defaults: UserDefaults = .standard,
         cellMetrics: @escaping @MainActor (Double) -> (width: Double, height: Double) = { TerminalFont.cell(size: $0) },
         keepAwake: @escaping @MainActor (Bool) -> Void = { UIApplication.shared.isIdleTimerDisabled = $0 }) {
        self.client = client
        self.keychain = keychain
        self.pollInterval = pollInterval
        self.keyFlushInterval = keyFlushInterval
        self.previewDelay = previewDelay
        self.reconnectBackoff = reconnectBackoff
        self.defaults = defaults
        self.cellMetrics = cellMetrics
        self.keepAwake = keepAwake
        preferLineComposer = defaults.bool(forKey: Self.lineComposerKey)
        terminalFontSize = defaults.object(forKey: Self.fontSizeKey) == nil ? TerminalFontSize.standard : TerminalFontSize.clamped(defaults.double(forKey: Self.fontSizeKey))
        loadLibrary()
    }
    static let lineComposerKey = "riwork.lineComposer", fontSizeKey = "riwork.terminalFontSize"
    /// Only "item not found" means an empty library. Any other failure blocks writes so a retry can still succeed.
    func loadLibrary() {
        do {
            let library = try keychain.read(Library.self) ?? Library()
            desktops = library.desktops
            selectedDesktopID = library.selectedDesktopID ?? desktops.first?.id
            loadFailure = nil
        } catch {
            desktops = []; selectedDesktopID = nil
            loadFailure = error is KeychainError ? error.localizedDescription : "Saved pairings could not be read (\(error.localizedDescription)). They were left untouched."
        }
    }
    /// Last resort when the stored library can never be decoded; the user confirms first.
    func resetLibrary() throws {
        try keychain.delete()
        desktops = []; selectedDesktopID = nil; loadFailure = nil; error = nil
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
        updateKeepAwake()
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
            if generation == token {
                if !(error is CancellationError) { failedViewport = target; viewportError = error.localizedDescription }
                // A cancelled, timed-out or mismatched resize may still have pinned the shell; keep it releasable.
                if case RemoteError.rpc = error {} else { viewportSessionID = target.shellID; appliedViewport = nil }
            }
            throw error
        }
    }
    func persist() throws {
        guard !loadFailed else { throw RemoteError.remote("Saved pairings could not be loaded, so nothing was saved. Retry loading them first.") }
        try keychain.write(Library(desktops: desktops, selectedDesktopID: selectedDesktopID))
    }
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
        discardKeyBuffers(forDesktop: id)
        let previous = desktops
        desktops.removeAll { $0.id == id }
        do { try persist() } catch { desktops = previous; throw error }
    }
    func activate(_ id: String) async {
        if selectedDesktopID != id {
            await disconnect()
            selectedDesktopID = id; clearSnapshot(); draft = ""; deliveryNotice = nil
            // Typed-but-unsent input belongs to the desktop it was typed for; leaving it does not queue it for later.
            discardKeyBuffers(exceptDesktop: id)
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
        missingSessionIDs = []; loadedProjectID = nil; outputLines = [:]
        // A fresh connection may reach an upgraded desktop: detect direct typing again. Buffers survive.
        keysSupport = .unknown
        keySender?.cancel(); keySender = nil
        for key in Array(keyBuffers.keys) { keyBuffers[key]?.block = nil }
        state = .connecting; error = nil; snapshotStale = true
        do {
            let established = try await client.connect(pairing: desktop.pairing, allowLocalDevelopment: desktop.allowLocalDevelopment)
            guard generation == token else { return }
            if established != desktop.pairing {
                try updateDesktop { $0.pairing = established }
            }
            guard generation == token else { return }
            state = .connected
            try await refresh(token: token)
            guard generation == token else { return }
            startPolling(token: token)
            kickKeySender()
        } catch {
            guard generation == token else { return }
            state = .failed; snapshotStale = true; self.error = error.localizedDescription
            await client.disconnect()
            scheduleReconnectIfNeeded()
        }
    }
    func disconnect(background: Bool = false) async {
        // Leaving the app right after typing: give what is queued a moment to reach the desktop. Coming back during
        // that moment must still reconnect afterwards, which `resume()` records instead of acting on a live connection.
        if background, state == .connected {
            drainingForBackground = true
            await drainKeys(timeout: .seconds(1))
            drainingForBackground = false
        }
        await performDisconnect(background: background)
        if resumeAfterDrain { resumeAfterDrain = false; await resume() }
    }
    private func performDisconnect(background: Bool) async {
        let token = generation
        if !background { wantsConnection = false }
        reconnectTask?.cancel(); reconnectTask = nil
        keySender?.cancel(); keySender = nil
        pollSleeper?.cancel()
        // A manual disconnect stays disconnected through backgrounding; only an active connection is "paused".
        state = background && wantsConnection ? .suspended : .disconnected
        snapshotStale = true; loading = false
        polling?.cancel(); polling = nil
        // Clear only this authenticated connection's override before closing, when possible. Unstructured so a
        // cancelled caller cannot skip it; the cancelled poll it waits behind only detaches its own request.
        await Task { try? await self.synchronizeViewport(token: token, forceRelease: true) }.value
        guard generation == token else { return }
        generation = UUID()
        viewportSessionID = nil; appliedViewport = nil; failedViewport = nil
        await client.disconnect()
    }
    func resume() async {
        if drainingForBackground { resumeAfterDrain = true; return }
        if wantsConnection, state == .suspended { await connect() }
    }
    private func clearSnapshot() { projects = []; worktrees = []; shells = []; orchestrators = []; loadedProjectID = nil; resetOutput(); snapshotStale = true }
    private func resetOutput() { lastScreen = nil; output = ""; outputCursorOffset = nil; outputInMode = false; outputSessionID = nil; lastOutputAt = nil }
    func refresh() async {
        guard state == .connected else { return }
        failedViewport = nil; viewportError = nil; missingSessionIDs = []; outputLines = [:]
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
        // Re-entering a project whose load was cancelled (Back during loading) must load it again.
        guard projectID != id || (state == .connected && !loading && loadedProjectID != id) else { return }
        do {
            if state == .connected { try? await synchronizeViewport(token: generation, forceRelease: true) }
            if projectID != id {
                try updateDesktop {
                    if let previousProject = $0.selectedProjectID, let previousSession = $0.selectedSessionID {
                        var selections = $0.projectSessionIDs ?? [:]; selections[previousProject] = previousSession; $0.projectSessionIDs = selections
                    }
                    $0.selectedProjectID = id
                    $0.selectedSessionID = $0.projectSessionIDs?[id]
                }
            }
            resetOutput(); draft = ""; deliveryNotice = nil
            worktrees = []; shells = []; loadedProjectID = nil; snapshotStale = true
            if state == .connected {
                try await loadProject(id, token: generation)
                await readOutput()
            }
        } catch { handle(error) }
    }
    private func loadProject(_ id: String, token: UUID) async throws {
        let params: [String: JSONValue] = ["project_id": .string(id)]
        let trees = try await rpc("worktrees.list", params)["worktrees"].decode([RemoteWorktree].self)
        let workers = try await rpc("shells.list", params)["shells"].decode([RemoteSession].self)
        guard generation == token, projectID == id else { return }
        worktrees = trees; shells = workers; loadedProjectID = id
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
        resetOutput(); snapshotStale = true
        draft = ""; deliveryNotice = nil; error = nil; viewportError = nil; failedViewport = nil
        if selected != nil { sessionAutoSwitches &+= 1 }
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
            if outputSessionID != session.id { resetOutput() }
            draft = ""; deliveryNotice = nil
            await readOutput()
        } catch { handle(error) }
    }
    func readOutput() async { await readOutput(recoverMissing: true) }
    /// One screen read in flight at a time. A request that arrives meanwhile is folded into one more read afterwards,
    /// so a caller that wants fresh output never gets a second concurrent poll on the wire.
    private func readOutput(recoverMissing: Bool) async {
        guard state == .connected, let id = sessionID, !missingSessionIDs.contains(id) else { return }
        if outputReadInFlight { outputReadQueued = true; return }
        outputReadInFlight = true
        defer { outputReadInFlight = false }
        repeat {
            outputReadQueued = false
            await readOutputOnce(recoverMissing: recoverMissing)
        } while outputReadQueued && state == .connected && !Task.isCancelled
    }
    private func readOutputOnce(recoverMissing: Bool) async {
        guard state == .connected, let id = sessionID, !missingSessionIDs.contains(id) else { return }
        let token = generation
        do {
            try await synchronizeViewport(token: token)
            guard generation == token, sessionID == id else { return }
            let screen = try ShellOutput(result: try await fetchOutput(id: id))
            guard screen.shellID == id else { throw RemoteError.protocolViolation("Session output identity mismatch.") }
            guard generation == token, sessionID == id else { return }
            // An unchanged screen is not re-rendered: polling is fast while typing and most polls see the same thing.
            if screen != lastScreen || outputSessionID != id {
                let rendered = screen.screen
                output = rendered.text; outputCursorOffset = rendered.cursorOffset; outputInMode = screen.inMode
                lastScreen = screen
            }
            outputSessionID = id; lastOutputAt = Date(); snapshotStale = false
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
                    if recoverMissing, sessionID != nil { await readOutputOnce(recoverMissing: false) }
                } catch { handle(error) }
            } else { handle(error) }
        }
    }
    /// The desktop refuses a reply over 128 KiB (`response_too_large`, or `cli_error` from the capture), which wide
    /// grids or multibyte scrollback can exceed at 500 lines. Halve until it fits and remember that per session
    /// (until reconnect or an explicit refresh), so a session that keeps failing costs one attempt per poll.
    private func fetchOutput(id: String) async throws -> JSONValue {
        var lines = outputLines[id] ?? Self.defaultOutputLines
        while true {
            do { return try await rpc("shell.output", ["shell_id": .string(id), "lines": .number(Double(lines))]) }
            catch RemoteError.rpc(let code, _) where (code == "cli_error" || code == "response_too_large") && lines > Self.minimumOutputLines {
                lines = max(Self.minimumOutputLines, lines / 2)
                outputLines[id] = lines
            }
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
        // Cancelled callers (a view going away, disconnect stopping the poll) are not failures.
        if error is CancellationError { return }
        self.error = error.localizedDescription
        snapshotStale = true
        Task {
            if !(await client.isConnected()), state == .connected { state = .failed; snapshotStale = true; polling?.cancel(); scheduleReconnectIfNeeded() }
        }
    }
    /// How long to wait before the next screen read: quick right after keys were typed or sent, then slower, then the resting interval.
    func pollDelay(now: Date = Date()) -> Duration {
        PollCadence(resting: pollInterval).interval(sinceKeyActivity: lastKeyActivity.map { now.timeIntervalSince($0) })
    }
    /// Typing or sending just happened: note it, and cut a long idle wait short so the echo shows up quickly.
    func noteKeyActivity(now: Date = Date()) {
        lastKeyActivity = now
        let fast = PollCadence(resting: pollInterval).fast
        if let due = pollDueAt, due.timeIntervalSince(now) > fast.timeInterval + 0.05 { pollSleeper?.cancel() }
    }
    /// True when the full wait elapsed; false when key activity cut it short (the caller then recomputes the delay).
    private func pollSleep() async -> Bool {
        let delay = pollDelay()
        pollDueAt = Date().addingTimeInterval(delay.timeInterval)
        let sleeper = Task<Void, Never> { _ = try? await Task.sleep(for: delay) }
        pollSleeper = sleeper
        await withTaskCancellationHandler { await sleeper.value } onCancel: { sleeper.cancel() }
        pollSleeper = nil; pollDueAt = nil
        return !sleeper.isCancelled
    }
    private func startPolling(token: UUID) {
        polling = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, self.generation == token else { return }
                guard await self.pollSleep() else { continue }
                guard !Task.isCancelled, self.generation == token else { return }
                if !(await self.client.isConnected()) {
                    self.state = .failed; self.snapshotStale = true; self.error = "Desktop disconnected. Reconnect to refresh output."
                    self.scheduleReconnectIfNeeded()
                    return
                }
                await self.readOutput()
            }
        }
    }
}

extension Duration {
    var timeInterval: TimeInterval { Double(components.seconds) + Double(components.attoseconds) / 1e18 }
}
