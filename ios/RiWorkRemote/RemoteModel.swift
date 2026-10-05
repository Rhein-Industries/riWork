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
    /// How the project list is ordered (Recent unless the person chose otherwise); remembered in UserDefaults.
    var projectSort = ProjectSort.standard
    /// Projects this phone created during this connection, with the desktop's `created_at`: they count as just active until the
    /// desktop has a figure of its own, so a new project is not sorted behind the ones with activity (see `ProjectSorting.sorted`).
    var touchedProjects: [String: UInt64] = [:]
    var worktrees: [RemoteWorktree] = []
    var shells: [RemoteSession] = []
    var orchestrators: [RemoteSession] = []
    var output = "" { didSet { hasOutput = !output.isEmpty } }
    /// Whether `output` holds anything. Views read this rather than `output`, which is a new string with every live answer and would
    /// rebuild whatever reads it each time.
    var hasOutput = false
    /// `output` with its colors and attributes, parsed off the main actor. `outputVersion` changes whenever it does.
    var styledOutput = StyledScreen.empty
    var outputVersion = 0
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
    // MARK: Opening and closing terminals (pipeline in RemoteModel+NewTerminal.swift)
    /// Whether the desktop understands `shell.create` / `shell.close`. Learned from the first call, reset by every new connection.
    var terminalControl: TerminalControlSupport = .unknown
    /// A `shell.create` is on its way: a second one is refused until it answers.
    var creatingTerminal = false
    /// The terminal a `shell.close` is on its way for.
    var closingTerminalID: String?
    /// The project list asks that project's terminal screen to open the "New terminal" sheet once it is up and loaded.
    var newTerminalRequestedProject: String?
    // MARK: Chats (pipeline in RemoteModel+Chat.swift)
    /// Whether the desktop has native chats. Read from `features.chat` when a connection begins, and reset by every new one.
    var chatSupport: ChatSupport = .unknown
    /// The chosen project's chats, from `chats.list`: tabs in the strip beside the terminals.
    var chats: [ChatInfo] = []
    /// The chat on screen; nil while a terminal is. Not remembered across launches.
    var selectedChatID: String?
    /// What has been read of the chats that were opened, by chat id.
    var chatConversations: [String: ChatConversation] = [:]
    /// A `chat.create` is on its way: a second one is refused until it answers.
    var creatingChat = false
    /// How long `chat.events` may hold a request back, and how long the follower rests while the link is down (tests make them short).
    @ObservationIgnored let chatWaitMilliseconds: Int
    @ObservationIgnored let chatIdleInterval: Duration
    // MARK: Agent activity and the project order (RemoteModel+Activity.swift)
    /// How often the lists are read again while a view that shows agent activity is on screen.
    @ObservationIgnored let activityRefreshInterval: Duration
    /// When each list was last read, by a refresh of any kind, so a view that appears right after one does not read it again at once.
    @ObservationIgnored var lastListRead: [ActivityScope: ContinuousClock.Instant] = [:]
    // MARK: Creating projects (pipeline in RemoteModel+NewProject.swift)
    /// Whether the desktop understands `project.create`. Learned from the first call, reset by every new connection.
    var projectCreation: ProjectCreationSupport = .unknown
    /// A `project.create` is on its way: a second one is refused until it answers.
    var creatingProject = false
    var terminalFontSize = TerminalFontSize.standard
    // MARK: Scrolling (history paging and the alternate screen: RemoteModel+Scroll.swift)
    /// The lines shown for the selected shell: scrollback and screen, each under an index that does not change while lines are
    /// added above or below it. `output` and `styledOutput` stay the latest live answer alone.
    var terminal = TerminalBuffer()
    /// A full-screen program (vim, less, htop) is on the alternate screen. It has no scrollback: swipes page the program instead.
    var alternateScreen = false
    /// An older page of scrollback is being fetched, or the last try failed.
    var historyLoading = false
    var historyFailed = false
    /// Whether the desktop understands `shell.history`. Learned from the first call, reset by every new connection.
    var historySupport: HistorySupport = .unknown
    /// Whether the view follows new output, and how many lines arrived while it did not. The view reports its scroll position here.
    var scrollFollow = StickyBottom()
    /// Counts requests to jump to the latest output (sending a line, the menu); typing has `typedCount`.
    var jumpRequests = 0
    @ObservationIgnored var historyTask: Task<Void, Never>?
    @ObservationIgnored var historyRun = UUID()
    /// Per session: lines per page after `response_too_large` halved them. Forgotten on reconnect and refresh.
    @ObservationIgnored var historyPageLines: [String: Int] = [:]
    /// Pages in a row that landed under such a cap: after six it is raised by half, so one big page does not pin a session small.
    @ObservationIgnored var historyCapStreak: [String: Int] = [:]
    /// Whether `shell.history` is asked with `styled`. Cleared for the connection when the desktop rejects the field.
    @ObservationIgnored var historyStyled = true
    @ObservationIgnored var historyRetryAfter: ContinuousClock.Instant?
    /// What pages of history say about the link: how fast it is. Forgotten when the kind of link changes.
    @ObservationIgnored var linkMeter = LinkMeter()
    /// Low Data Mode, a metered link, Low Power Mode, as the path says right now; `linkConditions` is what the phone acts on: a
    /// restriction is lifted only after the path has been free of it for a while, so flags that flap do not make the fetch flap.
    @ObservationIgnored var rawLinkConditions = LinkConditions()
    @ObservationIgnored var conditionHold = ConditionHold()
    @ObservationIgnored var conditionLapse: Task<Void, Never>?
    var linkConditions: LinkConditions { conditionHold.effective(rawLinkConditions, at: ProcessInfo.processInfo.systemUptime) }
    @ObservationIgnored let linkWatcher: (any LinkWatching)?
    @ObservationIgnored var linkInterface = "other"
    /// What the person chose for the history download (Settings). Persisted.
    var historyMode = HistoryMode.automatic
    /// Ask the desktop to compress its replies when it offers to (Settings). Persisted.
    var compressTraffic = true
    /// What the desktop announced when this connection began, and whether it agreed to compress.
    @ObservationIgnored var desktopFeatures = DesktopFeatures()
    /// Files and photos on their way to the Mac (RemoteModel+Upload.swift).
    let attachments = Attachments()
    var compressionAgreed = false
    /// How the latest `shell.output` reply travelled: what a line of this terminal weighs, before any page has said.
    @ObservationIgnored var lastOutputTiming: ReplyTiming?
    /// The most lines one history page may ask for on this connection: what the desktop announced, less if its CLI turned out to take fewer.
    @ObservationIgnored var historyLineLimit = HistoryLimits.legacyMaximumPageLines
    /// Fetch history in the background (the default). Off, a page is fetched only when asked for (`loadOlderHistory`).
    @ObservationIgnored var prefetchEnabled: Bool
    /// The lines of shells that are not on screen, so that going back to one does not fetch them again.
    @ObservationIgnored var terminalCache = TerminalCache()
    /// Where the reader is, from the surface: the first line in view and how many rows fit.
    @ObservationIgnored var readerTop: Int?
    @ObservationIgnored var readerRows = 40
    @ObservationIgnored var lastKickTop = Int.min
    /// History is left alone until then: the pane was just resized or its history shrank, and a program may be drawing it again.
    @ObservationIgnored var settleUntil: ContinuousClock.Instant?
    /// A page was asked for (the retry row, `loadOlderHistory`): no waiting for it.
    @ObservationIgnored var historyDemand = false
    @ObservationIgnored var lastHistoryAnswerAt: ContinuousClock.Instant?
    /// Pages in a row that did not line up or landed inside lines already held. Each doubles the quiet time, silently.
    @ObservationIgnored var historyMisses = 0
    /// The loop's pause between pages, so a reader who reaches the top can end it.
    @ObservationIgnored var historySleeper: Task<Void, Never>?
    /// Scales the quiet times after a resize or a shrinking history (tests make them short).
    @ObservationIgnored var settleScale = 1.0
    @ObservationIgnored weak var surface: TerminalSurfaceView?
    // MARK: Display, live sync and latency (RemoteModel+Display.swift, RemoteModel+Live.swift)
    /// Scale of the app chrome (headers, lists, key bar, buttons), 0.8-1.3. The terminal text size is separate.
    var interfaceScale = InterfaceScale.standard
    var showLatency = false
    /// Draw bold text in the bright variant of ANSI colors 0-7 (Ghostty's `bold-is-bright`). Off, like Ghostty's default.
    var boldIsBright = false
    /// How the screen is followed: a long poll that the desktop answers on change, or interval polling.
    var syncMode: SyncMode = .unknown
    /// Round trips, echo latency and payload size for the overlay. Cheap to keep; only the overlay reads it.
    var latency = LatencyBook()
    /// The app is in the foreground (scene active). The live loop waits while it is not.
    var appActive = true
    /// Sessions in focus mode during this app run.
    var focusedSessionIDs: Set<String> = []
    /// The synced colors: what is drawn now, and the last palette known per desktop (pipeline in RemoteModel+Theme.swift).
    let theme: ThemeStore
    /// The hotkeys added to the key bar.
    let hotkeys: HotkeyStore
    /// When the terminal takes the keyboard by itself, the hardware keyboard, and the key readout (KeyboardSettings.swift).
    let keyboard: KeyboardPrefs
    /// Whether the connected desktop offers `appearance.get`. Learned from the first answer, reset by every new connection.
    @ObservationIgnored var themeSupport: ThemeSyncSupport = .unknown
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
    /// The desktop pane's width in cells, from the last screen: rows that wide are taken to wrap (links across rows).
    var terminalColumns: Int? { lastScreen?.cols }
    /// When the last touch on the terminal came down on a link (system uptime), nil when it did not: its tap opens the link and does not
    /// bring up the keyboard. A touch that became a drag or a press leaves it behind, so it counts only for a second.
    @ObservationIgnored var linkTouchedAt: Double?
    var touchBeganOnLink: Bool {
        guard let at = linkTouchedAt else { return false }
        return ProcessInfo.processInfo.systemUptime - at < 1
    }
    /// The lines of the last screens, parsed. The next answer differs from the last in a line or two, and only those are parsed again.
    @ObservationIgnored private let lineCache = StyledLineCache()
    @ObservationIgnored private var drainingForBackground = false
    @ObservationIgnored private var resumeAfterDrain = false
    /// Counts times the selected terminal was replaced without a tap (it closed); the view drops the keyboard so
    /// keystrokes meant for one shell never continue into another.
    var sessionAutoSwitches = 0
    @ObservationIgnored private var outputReadInFlight = false
    @ObservationIgnored private var outputReadQueued = false
    /// Callers that asked for a read while one was in flight; resumed when that read (and the one folded in for them) is done.
    @ObservationIgnored private var outputReadWaiters: [CheckedContinuation<Void, Never>] = []
    /// The `hash` of the screen on the phone, for the session in `outputSessionID`. Sent back as `if_changed`.
    @ObservationIgnored var outputHash: String?
    /// The `shell.output` request on the wire, so it can be cancelled alone (session change, pause) without touching the loop.
    @ObservationIgnored private var outputFlight: Task<JSONValue, any Error>?
    @ObservationIgnored var outputFlightIsLongPoll = false
    @ObservationIgnored var liveBackoff = LongPollBackoff()
    /// Cancelled long polls that the desktop is still holding (it allows two waiting requests per device).
    @ObservationIgnored var waitSlots = WaitSlots()
    /// Whether the desktop takes `styled`, `if_changed` and `wait_ms`. Cleared for the connection when it refuses them (an older
    /// desktop answers `invalid_request`, unknown field); the app then reads plain text by interval polling.
    @ObservationIgnored var outputExtensions = true
    /// The message a failed screen read put up, so it can be taken down again when reads work.
    @ObservationIgnored private var outputErrorMessage: String?
    @ObservationIgnored let liveWaitMilliseconds: Int
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
    // Theme sync plumbing (RemoteModel+Theme.swift).
    @ObservationIgnored let themeRefreshInterval: Duration
    @ObservationIgnored let themeMinimumGap: Duration
    @ObservationIgnored var themeTask: Task<Void, Never>?
    @ObservationIgnored var appearanceFlight: UUID?
    @ObservationIgnored var appearanceInFlight: Bool { appearanceFlight != nil }
    @ObservationIgnored var lastAppearanceFetch: ContinuousClock.Instant?
    @ObservationIgnored var noticeID = UUID()
    /// The pane size the view reported; the grid is recomputed from it whenever layout, font or focus changes.
    @ObservationIgnored var terminalArea: CGSize?
    // The project whose worktrees and shells are current for this connection; a cancelled load leaves it unset.
    @ObservationIgnored var loadedProjectID: String?
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
         defaults: UserDefaults = .standard, themeRefreshInterval: Duration = .seconds(60), themeMinimumGap: Duration = .seconds(5),
         activityRefreshInterval: Duration = .seconds(4), chatWaitMilliseconds: Int = ChatLimits.waitMilliseconds, chatIdleInterval: Duration = .seconds(1),
         cellMetrics: @escaping @MainActor (Double) -> (width: Double, height: Double) = { TerminalFont.cell(size: $0) },
         keepAwake: @escaping @MainActor (Bool) -> Void = { UIApplication.shared.isIdleTimerDisabled = $0 },
         liveWaitMilliseconds: Int = LiveSync.waitMilliseconds, linkWatcher: (any LinkWatching)? = nil, prefetch: Bool = true,
         hardwareKeyboard: HardwareKeyboardMonitor = HardwareKeyboardMonitor()) {
        self.liveWaitMilliseconds = liveWaitMilliseconds
        self.linkWatcher = linkWatcher
        self.prefetchEnabled = prefetch
        self.client = client
        self.keychain = keychain
        self.pollInterval = pollInterval
        self.keyFlushInterval = keyFlushInterval
        self.previewDelay = previewDelay
        self.reconnectBackoff = reconnectBackoff
        self.defaults = defaults
        self.themeRefreshInterval = themeRefreshInterval
        self.themeMinimumGap = themeMinimumGap
        self.activityRefreshInterval = activityRefreshInterval
        self.chatWaitMilliseconds = chatWaitMilliseconds
        self.chatIdleInterval = chatIdleInterval
        self.theme = ThemeStore(defaults: defaults)
        self.hotkeys = HotkeyStore(defaults: defaults)
        self.keyboard = KeyboardPrefs(defaults: defaults, hardware: hardwareKeyboard)
        self.cellMetrics = cellMetrics
        self.keepAwake = keepAwake
        preferLineComposer = defaults.bool(forKey: Self.lineComposerKey)
        projectSort = ProjectSort.stored(in: defaults)
        terminalFontSize = defaults.object(forKey: Self.fontSizeKey) == nil ? TerminalFontSize.standard : TerminalFontSize.clamped(defaults.double(forKey: Self.fontSizeKey))
        interfaceScale = defaults.object(forKey: Self.interfaceScaleKey) == nil ? InterfaceScale.standard : InterfaceScale.clamped(defaults.double(forKey: Self.interfaceScaleKey))
        showLatency = defaults.bool(forKey: Self.showLatencyKey)
        boldIsBright = defaults.bool(forKey: Self.boldIsBrightKey)
        historyMode = HistoryMode(rawValue: defaults.string(forKey: Self.historyModeKey) ?? "") ?? .automatic
        compressTraffic = defaults.object(forKey: Self.compressTrafficKey) == nil ? true : defaults.bool(forKey: Self.compressTrafficKey)
        theme.setScale(interfaceScale)
        loadLibrary()
        if let linkWatcher {
            rawLinkConditions = linkWatcher.conditions; linkInterface = linkWatcher.interface
            conditionHold.apply(rawLinkConditions, at: ProcessInfo.processInfo.systemUptime)
            linkWatcher.onChange = { [weak self] in self?.linkChanged() }
        }
    }
    static let historyModeKey = "riwork.historyMode", compressTrafficKey = "riwork.compressTraffic"
    static let lineComposerKey = "riwork.lineComposer", fontSizeKey = "riwork.terminalFontSize"
    static let interfaceScaleKey = "riwork.interfaceScale", showLatencyKey = "riwork.showLatency", boldIsBrightKey = "riwork.boldIsBright"
    /// Only "item not found" means an empty library. Any other failure blocks writes so a retry can still succeed.
    func loadLibrary() {
        var chosen: String?
        do {
            let library = try keychain.read(Library.self) ?? Library()
            desktops = library.desktops
            selectedDesktopID = library.selectedDesktopID ?? desktops.first?.id
            chosen = library.selectedDesktopID
            loadFailure = nil
        } catch {
            desktops = []; selectedDesktopID = nil
            loadFailure = error is KeychainError ? error.localizedDescription : "Saved pairings could not be read (\(error.localizedDescription)). They were left untouched."
        }
        // Before anything connects, the desktop list wears the last palette of the desktop that was chosen, or else of the most
        // recently used one that still exists, so the first frame already has it. Never chosen: the built-in style.
        theme.showInitial(selected: chosen, existing: desktops.map(\.id))
    }
    /// Last resort when the stored library can never be decoded; the user confirms first.
    func resetLibrary() throws {
        try keychain.delete()
        for desktop in desktops { theme.forget(desktop.id) }
        desktops = []; selectedDesktopID = nil; loadFailure = nil; error = nil
        theme.showInitial(selected: nil, existing: [])
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
        // The long poll is not renewed while the terminal is off screen (one already out just runs its course), and picks up
        // again, from the hash it has, when the terminal returns.
        if visible { wakeLive(); kickPrefetch() }
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
                // A live desktop answers the long poll on its own when the resize reflows the screen; only interval polling
                // needs a read to see the new size sooner.
                if terminalVisible, state == .connected, syncMode != .live { await readOutput() }
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
    func synchronizeViewport(token: UUID, forceRelease: Bool = false) async throws {
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
            viewportWasApplied()
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
    func updateDesktop(_ update: (inout SavedDesktop) -> Void) throws {
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
        theme.forget(id)
        theme.showInitial(selected: selectedDesktopID, existing: desktops.map(\.id))
    }
    func activate(_ id: String) async {
        if selectedDesktopID != id {
            await disconnect()
            selectedDesktopID = id; clearSnapshot(); draft = ""; deliveryNotice = nil
            // The new desktop's last palette is drawn before it has even connected.
            theme.select(id)
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
        stopThemeSync()
        generation = UUID(); let token = generation
        viewportSessionID = nil; appliedViewport = nil; failedViewport = nil; viewportError = nil
        missingSessionIDs = []; loadedProjectID = nil; outputLines = [:]
        // A fresh connection may reach an upgraded desktop: detect direct typing again. Buffers survive.
        keysSupport = .unknown
        // And for opening terminals.
        terminalControl = .unknown
        // And for creating projects.
        projectCreation = .unknown
        // And for chats (the desktop's `ready` says; what it answers confirms).
        chatSupport = .unknown
        // And for waiting on changes: the first screen tells whether this desktop sends a `hash`.
        syncMode = .unknown; outputHash = nil; liveBackoff = LongPollBackoff(); waitSlots.reset(); outputExtensions = true; latency.reset()
        // And for paging history: the first page tells whether this desktop has `shell.history`.
        historySupport = .unknown; historyStyled = true; historyPageLines = [:]; historyRetryAfter = nil; historyMisses = 0; historyFailed = false; cancelHistory()
        // The new connection may leave on another link: its speed and round trip are measured again (what the desktop and its history
        // are like is not about the link, and is kept).
        linkMeter.pathChanged(); settleUntil = nil
        desktopFeatures = DesktopFeatures(); compressionAgreed = false; historyLineLimit = HistoryLimits.legacyMaximumPageLines
        linkWatcher?.start()
        // The same for its colors, and the last palette for this desktop is on screen while the first fetch is on its way.
        themeSupport = .unknown; appearanceFlight = nil
        theme.select(desktop.id)
        keySender?.cancel(); keySender = nil
        for key in Array(keyBuffers.keys) { keyBuffers[key]?.block = nil }
        state = .connecting; error = nil; snapshotStale = true
        do {
            await client.setCompression(compressTraffic)
            let established = try await client.connect(pairing: desktop.pairing, allowLocalDevelopment: desktop.allowLocalDevelopment)
            guard generation == token else { return }
            if established != desktop.pairing {
                try updateDesktop { $0.pairing = established }
            }
            guard generation == token else { return }
            state = .connected
            await learnDesktopFeatures(token: token)
            startThemeSync(token: token)
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
        stopThemeSync()
        pollSleeper?.cancel()
        // A manual disconnect stays disconnected through backgrounding; only an active connection is "paused".
        state = background && wantsConnection ? .suspended : .disconnected
        snapshotStale = true; loading = false
        polling?.cancel(); polling = nil
        cancelHistory()
        linkWatcher?.stop()
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
    private func clearSnapshot() {
        projects = []; touchedProjects = [:]; worktrees = []; shells = []; orchestrators = []; loadedProjectID = nil; resetOutput(); snapshotStale = true
        chats = []; selectedChatID = nil; chatConversations = [:]
        lastListRead = [:]
        // Another desktop: nothing kept for the last one is of any use.
        terminalCache.removeAll()
    }
    func resetOutput() {
        stashTerminal()
        lineCache.removeAll()
        lastScreen = nil; output = ""; styledOutput = .empty; outputVersion &+= 1; outputHash = nil
        outputCursorOffset = nil; outputInMode = false; outputSessionID = nil; lastOutputAt = nil
        resetTerminal()
    }
    func refresh() async {
        guard state == .connected else { return }
        failedViewport = nil; viewportError = nil; missingSessionIDs = []; outputLines = [:]; historyPageLines = [:]; historyRetryAfter = nil; historyMisses = 0
        do { try await refresh(token: generation) } catch { handle(error) }
    }
    func rpc(_ method: String, _ params: [String: JSONValue] = [:], id: String = UUID().uuidString.lowercased()) async throws -> JSONValue {
        // The lists asked for when a connection opens are small replies: the first samples of the round trip, before any history page.
        let reply = try await client.timedRequest(method: method, params: params, id: id)
        noteReply(reply.timing)
        return reply.value
    }
    private func refresh(token: UUID) async throws {
        loading = true
        defer { if generation == token { loading = false } }
        // One round trip for all of it, not four one after the other: the lists do not depend on each other, and the project whose tabs
        // are wanted is the one already chosen (the replies come back matched to their requests, so they may overlap on the wire).
        let chosen = projectID
        async let projectsReply = rpc("projects.list")
        async let orchestratorsReply = rpc("orchestrators.list")
        async let chosenListing = fetchProject(ifChosen: chosen)
        let listed = try await projectsReply["projects"].decode([RemoteProject].self)
        let managers = try await orchestratorsReply["orchestrators"].decode([RemoteSession].self)
        let early = try await chosenListing
        guard generation == token else { return }
        projects = listed; orchestrators = managers
        noteListsRead()
        if projectID == nil, let first = listed.first { try updateDesktop { $0.selectedProjectID = first.id } }
        if let project = projectID {
            if project == chosen, let early { try await install(early, project: project, token: token) }
            else { try await loadProject(project, token: token) }
        }
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
            chats = []; selectedChatID = nil
            if state == .connected {
                try await loadProject(id, token: generation)
                await readOutput()
            }
        } catch { handle(error) }
    }
    private func loadProject(_ id: String, token: UUID) async throws {
        try await install(try await fetchProject(id), project: id, token: token)
    }
    private typealias ProjectListing = (worktrees: [RemoteWorktree], shells: [RemoteSession], chats: [ChatInfo]?)
    private func fetchProject(ifChosen id: String?) async throws -> ProjectListing? {
        guard let id else { return nil }
        return try await fetchProject(id)
    }
    /// A project's worktrees and shells, asked for together.
    private func fetchProject(_ id: String) async throws -> ProjectListing {
        let params: [String: JSONValue] = ["project_id": .string(id)]
        async let trees = rpc("worktrees.list", params)
        async let workers = rpc("shells.list", params)
        // The desktop's chats come with them, when it has any; a chat list that cannot be read never fails the project.
        async let talks = chatsOfProject(id)
        return (try await trees["worktrees"].decode([RemoteWorktree].self), try await workers["shells"].decode([RemoteSession].self), await talks)
    }
    private func install(_ listing: ProjectListing, project id: String, token: UUID) async throws {
        guard generation == token, projectID == id else { return }
        worktrees = listing.worktrees; shells = listing.shells; loadedProjectID = id
        lastListRead[.sessions] = .now
        if let talks = listing.chats { installChats(talks, project: id) }
        try reconcileSelectedSession()
        if sessionID == nil { try await synchronizeViewport(token: token) }
    }
    /// Metadata refresh selects only an existing live tab. Never submits or retries input.
    func reconcileSelectedSession() throws {
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
        // A terminal tab takes the screen back from a chat.
        selectedChatID = nil
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
            wakeLive()
        } catch { handle(error) }
    }
    /// What one screen read came to.
    enum OutputOutcome: Equatable { case changed, unchanged, skipped, failed, interrupted }

    /// Reads the screen now. Returns once a read that started after this call has finished.
    func readOutput() async { await readOutput(recoverMissing: true, longPoll: false) }

    /// One screen read in flight at a time. A request that arrives meanwhile cuts a long poll short and is folded into one more
    /// (immediate) read afterwards, so a caller that wants fresh output never gets a second concurrent poll on the wire and never
    /// waits behind a request the desktop is holding back.
    @discardableResult
    func readOutput(recoverMissing: Bool, longPoll: Bool) async -> OutputOutcome {
        guard state == .connected, let id = sessionID, !missingSessionIDs.contains(id) else { return .skipped }
        if outputReadInFlight {
            interruptLongPoll()
            outputReadQueued = true
            await withCheckedContinuation { outputReadWaiters.append($0) }
            return .interrupted
        }
        outputReadInFlight = true
        defer {
            outputReadInFlight = false
            let waiting = outputReadWaiters
            outputReadWaiters = []
            for waiter in waiting { waiter.resume() }
        }
        var outcome = OutputOutcome.skipped
        var waits = longPoll
        repeat {
            outputReadQueued = false
            outcome = await readOutputOnce(recoverMissing: recoverMissing, longPoll: waits)
            // Whatever was queued meanwhile wants the screen now.
            waits = false
        } while outputReadQueued && state == .connected && !Task.isCancelled
        return outcome
    }
    private func readOutputOnce(recoverMissing: Bool, longPoll: Bool) async -> OutputOutcome {
        guard state == .connected, let id = sessionID, !missingSessionIDs.contains(id) else { return .skipped }
        let token = generation
        do {
            try await synchronizeViewport(token: token)
            guard generation == token, sessionID == id else { return .skipped }
            // Paused between the decision to wait and the request itself: do not start a wait nobody is looking at.
            // Or a caller arrived meanwhile who wants the screen now: its read comes first.
            if longPoll, !liveWanted || outputReadQueued { return .interrupted }
            // Once the desktop has shown a hash, ask it to answer only when the screen differs from the one we hold.
            let known = syncMode == .live && outputSessionID == id ? outputHash : nil
            let wait = longPoll && known != nil ? liveWaitMilliseconds : 0
            let started = ProcessInfo.processInfo.systemUptime
            let raw = try await fetchOutput(id: id, ifChanged: known, wait: wait)
            let elapsed = ProcessInfo.processInfo.systemUptime - started
            guard generation == token, sessionID == id else { return .skipped }
            switch try OutputReply(result: raw) {
            case .unchanged(let shellID, let hash):
                guard shellID == id else { throw RemoteError.protocolViolation("Session output identity mismatch.") }
                latency.outputAnswered(seconds: elapsed, waited: Double(wait) / 1000, unchanged: true, bytes: nil)
                guard known != nil, hash == known else {
                    // An answer to a question we did not ask: forget the hash so the next read brings the whole screen.
                    outputHash = nil
                    return .unchanged
                }
                outputSessionID = id; lastOutputAt = Date(); snapshotStale = false
                clearOutputError()
                // The same screen, but a full-screen program may have come or gone with it.
                if let alternate = OutputExtras(result: raw).alternate { setAlternateScreen(alternate) }
                kickPrefetch()
                return .unchanged
            case .screen(let screen):
                guard screen.shellID == id else { throw RemoteError.protocolViolation("Session output identity mismatch.") }
                let live = screen.hash != nil
                if live { if syncMode != .live { syncMode = .live; wakeLive() } } else if syncMode != .poll { syncMode = .poll }
                var changed = false
                // An unchanged screen is not re-parsed or re-rendered: reads are frequent while typing and most see the same thing.
                if screen != lastScreen || outputSessionID != id {
                    // Colors, symbols and cursor are worked out off the main actor; a full 500-line screen must not stall touches.
                    let cache = lineCache
                    let styled = await Task.detached(priority: .userInitiated) { Perf.interval("ParseAnswer") { screen.styledScreen(cache: cache) } }.value
                    guard generation == token, sessionID == id else { return .skipped }
                    if let timing = lastOutputTiming { linkMeter.noteLines(wireBytes: timing.wireBytes, jsonBytes: timing.jsonBytes, lines: styled.lines.count) }
                    output = styled.text; styledOutput = styled; outputVersion &+= 1
                    outputCursorOffset = styled.cursorOffset; outputInMode = screen.inMode
                    takeIn(screen: screen, styled: styled)
                    lastScreen = screen
                    latency.screenChanged(at: ProcessInfo.processInfo.systemUptime)
                    changed = true
                }
                latency.outputAnswered(seconds: elapsed, waited: Double(wait) / 1000, unchanged: false, bytes: changed ? screen.text.utf8.count : nil)
                outputHash = screen.hash
                outputSessionID = id; lastOutputAt = Date(); snapshotStale = false
                clearOutputError()
                // The live screen is in: older history is the next step.
                kickPrefetch()
                return changed ? .changed : .unchanged
            }
        } catch {
            guard generation == token, sessionID == id else { return .skipped }
            // A cancelled read (the loop paused, another caller wants the screen now, the view went away) is not a failure.
            if error is CancellationError { return Task.isCancelled ? .skipped : .interrupted }
            if case RemoteError.rpc("not_found", _) = error {
                missingSessionIDs.insert(id); snapshotStale = true
                do {
                    if recoverMissing, let project = projectID {
                        // Refresh project sessions once. The failed UUID cannot be polled again.
                        let managers = try await rpc("orchestrators.list")["orchestrators"].decode([RemoteSession].self)
                        guard generation == token, projectID == project else { return .skipped }
                        orchestrators = managers
                        try await loadProject(project, token: token)
                    } else {
                        try reconcileSelectedSession()
                        if sessionID == nil { try await synchronizeViewport(token: token) }
                    }
                    if recoverMissing, sessionID != nil { return await readOutputOnce(recoverMissing: false, longPoll: false) }
                    return .skipped
                } catch { handle(error); noteOutputError(); return .failed }
            } else { handle(error); noteOutputError(); return .failed }
        }
    }
    private func noteOutputError() { outputErrorMessage = error }
    /// A read worked again: the message an earlier failed read put up is taken down (a different message is left alone).
    private func clearOutputError() {
        if let shown = outputErrorMessage, error == shown { error = nil }
        outputErrorMessage = nil
    }
    /// The desktop refuses a reply over 128 KiB (`response_too_large`, or `cli_error` from the capture), which wide
    /// grids or multibyte scrollback can exceed at 500 lines. Halve until it fits and remember that per session
    /// (until reconnect or an explicit refresh), so a session that keeps failing costs one attempt per poll.
    private func fetchOutput(id: String, ifChanged: String?, wait: Int) async throws -> JSONValue {
        var lines = min(outputLines[id] ?? Self.defaultOutputLines, liveScrollbackLines)
        var wait = wait
        while true {
            let extended = outputExtensions
            let request = OutputRequest(shellID: id, lines: lines, ifChanged: extended ? ifChanged : nil, waitMilliseconds: extended ? wait : 0, styled: extended)
            do { return try await requestOutput(request) }
            catch RemoteError.rpc(let code, let message) where extended && LiveSync.rejectsNewParameters(code: code, message: message) {
                // An older desktop does not know the new parameters: plain reads for the rest of this connection.
                outputExtensions = false
            }
            catch RemoteError.rpc("cli_error", _) where extended && wait > 0 {
                // A wait that failed says nothing about the size of the reply: ask again without waiting before shrinking anything.
                wait = 0
            }
            catch RemoteError.rpc(let code, _) where (code == "cli_error" || code == "response_too_large") && lines > Self.minimumOutputLines {
                lines = max(Self.minimumOutputLines, lines / 2)
                outputLines[id] = lines
            }
        }
    }
    /// The one `shell.output` request. It runs as its own task so a long poll can be cancelled alone: cancelling it detaches only
    /// this request (RelayClient keeps the socket and counters); the desktop's late answer is dropped.
    private func requestOutput(_ request: OutputRequest) async throws -> JSONValue {
        let flight = Task { [client, weak self] in
            let reply = try await client.timedRequest(method: "shell.output", params: request.params, id: UUID().uuidString.lowercased())
            self?.noteReply(reply.timing)
            self?.lastOutputTiming = reply.timing
            return reply.value
        }
        outputFlight = flight; outputFlightIsLongPoll = request.isLongPoll
        let started = ProcessInfo.processInfo.systemUptime
        let token = generation
        defer { if outputFlight == flight { outputFlight = nil; outputFlightIsLongPoll = false } }
        do {
            return try await withTaskCancellationHandler { try await flight.value } onCancel: { flight.cancel() }
        } catch is CancellationError {
            // The desktop does not know: it keeps this wait, and its slot, until the wait runs out (a new connection starts clean).
            if request.isLongPoll, generation == token { waitSlots.abandon(startedAt: started, wait: Double(request.waitMilliseconds) / 1000) }
            throw CancellationError()
        }
    }
    /// Cuts a pending long poll short. Nothing else is touched: the loop sees the read end and decides what comes next.
    func interruptLongPoll() {
        if outputFlightIsLongPoll { outputFlight?.cancel() }
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
        jumpToLatest()
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
                // A desktop that answers on change is followed by a long poll; every other one by the interval below.
                if self.syncMode == .live {
                    guard await self.liveTurn(token: token) else { return }
                    continue
                }
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

    // MARK: Live sync

    /// The long poll runs only for a shell that is on screen, in the foreground, on a live connection.
    var liveWanted: Bool { state == .connected && terminalVisible && appActive && sessionID != nil }

    /// One turn of the live loop: a read that the desktop holds back until the screen differs from the one we hold (or 8 s pass),
    /// re-issued at once whether it came back changed or unchanged. Returns false when the loop should end (the link is gone).
    private func liveTurn(token: UUID) async -> Bool {
        if !(await client.isConnected()) {
            guard !Task.isCancelled, generation == token else { return false }
            state = .failed; snapshotStale = true; error = "Desktop disconnected. Reconnect to refresh output."
            scheduleReconnectIfNeeded()
            return false
        }
        guard liveWanted else { await liveIdle(.seconds(2)); return !Task.isCancelled }
        // Two waiting requests is all the desktop allows; cancelled ones still count until their wait runs out. In the rare burst of
        // cancellations that fills both slots, the screen is followed by short reads until one frees up.
        let canWait = waitSlots.canWait(at: ProcessInfo.processInfo.systemUptime)
        let outcome = await readOutput(recoverMissing: true, longPoll: canWait)
        guard !Task.isCancelled, generation == token else { return false }
        switch outcome {
        case .changed, .unchanged: liveBackoff.success()
        case .failed: await liveIdle(liveBackoff.failure())
        case .skipped: await liveIdle(.seconds(1))
        case .interrupted: break
        }
        if !canWait, outcome != .failed { await liveIdle(.milliseconds(300)) }
        return !Task.isCancelled
    }
    /// A wait that `wakeLive()` ends early (resume, a session switch).
    private func liveIdle(_ delay: Duration) async {
        let sleeper = Task<Void, Never> { _ = try? await Task.sleep(for: delay) }
        pollSleeper = sleeper
        await withTaskCancellationHandler { await sleeper.value } onCancel: { sleeper.cancel() }
        pollSleeper = nil
    }
    /// Ends the loop's idle wait or interval sleep now, so it re-reads what it should be doing (the terminal returned, the app
    /// became active, another session was chosen).
    func wakeLive() { pollSleeper?.cancel() }
    /// The scene became active or left the foreground. No new long poll starts while it is not active (the one out runs its
    /// course, at most one wait; leaving for the background closes the connection anyway), and the loop resumes when it is.
    func setAppActive(_ active: Bool) {
        guard appActive != active else { return }
        appActive = active
        if active { wakeLive(); kickPrefetch() }
    }
}

extension Duration {
    var timeInterval: TimeInterval { Double(components.seconds) + Double(components.attoseconds) / 1e18 }
}
