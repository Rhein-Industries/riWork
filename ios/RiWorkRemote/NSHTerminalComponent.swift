import NativeShell
import Observation
import RiWorkCore
import SwiftUI
import UIKit

/// `riwork.terminal` on iPhone and iPad (docs/nsh-contracts.md §3.8, amendment 1.1): the capture terminal of the native project
/// screen (`SessionConsole`: `PhoneTerminal`/`TerminalSurface` on iPhone, its two-axis view on iPad, `KeyCapture` and the key bar,
/// the line composer and its saved uncertain-outcome block, dictation, the viewport lease) hosted for a TSX screen.
///
/// - Only `{kind: "remote", desktopId, shellId}` targets of the interactive host are shown; a local target says where it runs.
/// - The view drives the one `RemoteModel` terminal the native screens use: it puts the target shell on screen (its project's lists
///   are read when it is in another project), keeps it visible (the long poll and the lease) while it is shown, and lets go when it is
///   hidden. While a native screen is up the model is that screen's, and nothing here changes it.
/// - Several components may exist (iPad panes); the one most recently shown, focused or tapped is live, the others say so.
/// - Keys, IME, dictation, selection and copy stay native; JS sees the props, commands and events of the contract.
@MainActor final class NSHTerminalHost {
    let model: RemoteModel
    private weak var bridge: NSHMobileBridge?
    /// Whether the live terminal holds the keyboard changed: the root's own Tab prefix and guide stand aside while it does.
    var onKeyboardChange: (() -> Void)?
    private(set) var holdsKeyboard = false
    private struct Entry { weak var component: NSHTerminalComponent? }
    /// Terminals on screen, the most recently shown or claimed last; that one is live.
    private var candidates: [Entry] = []
    private weak var owner: NSHTerminalComponent?

    init(model: RemoteModel, bridge: NSHMobileBridge) {
        self.model = model
        self.bridge = bridge
        bridge.onNativePresentedChange = { [weak self] in
            Task { @MainActor [weak self] in self?.nativeChanged() }
        }
    }

    func register(on app: NshAppView) {
        app.register(component: "riwork.terminal") { [self] context in NSHTerminalComponent(context: context, host: self) }
    }

    var nativePresented: Bool { bridge?.nativePresented ?? false }
    /// The host being opened, or the one open.
    var interactiveHostID: String? { bridge?.interactiveHostID ?? model.selectedDesktopID }
    func isOwner(_ component: NSHTerminalComponent) -> Bool { owner === component }
    /// Another terminal is live: it sets what the model's terminal needs, and one that lets go leaves that alone.
    func ownedByOther(_ component: NSHTerminalComponent) -> Bool { owner != nil && owner !== component }

    /// A terminal came on screen (`claim`: was shown, focused or tapped again) or went. Others whose standing changed look again.
    func setCandidate(_ component: NSHTerminalComponent, _ candidate: Bool, claim: Bool = false) {
        let index = candidates.firstIndex { $0.component === component }
        if candidate {
            if let index, claim { candidates.remove(at: index); candidates.append(Entry(component: component)) }
            else if index == nil { candidates.append(Entry(component: component)) }
        } else if let index {
            candidates.remove(at: index)
        }
        candidates.removeAll { $0.component == nil }
        let next = candidates.last?.component
        guard next !== owner else { return }
        let previous = owner
        owner = next
        for other in [previous, next].compactMap({ $0 }) where other !== component {
            Task { @MainActor [weak other] in other?.refresh() }
        }
        keyboardChanged()
    }

    func keyboardChanged() {
        let holds = owner?.holdsKeyboard ?? false
        guard holds != holdsKeyboard else { return }
        holdsKeyboard = holds
        onKeyboardChange?()
    }

    private func nativeChanged() {
        for entry in candidates { entry.component?.refresh() }
    }

    /// A component went away without letting go (NSH dropped it while its view was on screen): what it set is let go here, or the next
    /// terminal on screen takes over.
    func released(_ lease: NSHTerminalLease) {
        lease.end()
        model.keyboard.tabShortcuts.clearAppHandler(owner: lease.shortcutOwner)
        candidates.removeAll { $0.component == nil }
        owner = candidates.last?.component
        if let owner {
            owner.refresh()
        } else if !nativePresented {
            if model.terminalVisible { model.setTerminalVisible(false) }
            model.setTerminalFontScale(1)
        }
        keyboardChanged()
    }
}

/// What a component started on the model that must stop with it, kept apart so its view can end it if the component is gone first.
@MainActor final class NSHTerminalLease {
    let shortcutOwner = UUID()
    var freshness: Task<Void, Never>?
    var resolving: Task<Void, Never>?
    /// The component's hosting controller, a child of the root's controller while its view is in a window.
    weak var controller: UIViewController?
    func end() {
        freshness?.cancel(); freshness = nil
        resolving?.cancel(); resolving = nil
        // Out of the root's children and off the container, so the terminal it draws (and what it observes of the model) goes too.
        if let controller {
            if controller.parent != nil {
                controller.willMove(toParent: nil)
                controller.removeFromParent()
            }
            controller.view.removeFromSuperview()
        }
        controller = nil
    }
}

@MainActor final class NSHTerminalComponent: NshComponent {
    enum Target: Equatable {
        case local
        case remote(desktopID: String, shellID: String)
    }

    static let localMessage = "Local terminals run on the Mac"
    /// The Mac's tab strip is the app's (`tabs.open` through its port); this view never opens a hidden tab.
    static let hiddenMessage = "Open this worker's tab to see it."
    /// Tries to put the target on screen per connection before saying it could not.
    private static let resolveLimit = 3
    /// App chords typed while the terminal holds the keyboard: the TSX shell acts on them (`command`). ⌘K and ⌘/ stay the terminal's
    /// own (its hotkey menu and help), and a custom terminal hotkey on one of these keeps priority, as on the native screen.
    private struct Chord {
        let input: String
        let modifiers: UIKeyModifierFlags
        let id: String
        let title: String
    }
    private static let chords: [Chord] = (1...9).map { Chord(input: "\($0)", modifiers: .command, id: "tab.select.\($0)", title: "Select tab \($0)") } + [
        Chord(input: "[", modifiers: [.command, .shift], id: "tab.prev", title: "Previous tab"),
        Chord(input: "]", modifiers: [.command, .shift], id: "tab.next", title: "Next tab"),
        Chord(input: "n", modifiers: .command, id: "terminal.new", title: "New terminal"),
        Chord(input: "p", modifiers: [.command, .shift], id: "nav.back", title: "Back"),
        Chord(input: ",", modifiers: [.command, .shift], id: "display.open", title: "Display settings"),
        Chord(input: "o", modifiers: [.command, .shift], id: "picker.open", title: "Open a worker or shell"),
    ]

    private let context: NshComponentContext
    private let host: NSHTerminalHost
    private var model: RemoteModel { host.model }
    private let container = NSHTerminalContainer()
    private let box: NSHTerminalBox
    private let hosting: UIHostingController<NSHTerminalRoot>
    private let lease = NSHTerminalLease()
    var view: UIView { container }

    // Props.
    private var target: Target?
    private var visible = false
    private var focusOnAttach = false
    private var frozen = false
    private var fontScale = 1.0

    // What was sent, and what this view has set on the model.
    private var sentState: String?
    private var sentMessage: String?
    private var sentTitle: String?
    private(set) var holdsKeyboard = false
    private var attachedOnce = false
    private var pendingFocus = false
    private var focusAttempt: Task<Void, Never>?
    private var refocusAfterFreeze = false
    private var drivesTerminal = false
    private var scaled = false
    private var handlesShortcuts = false
    private var resolveAttempts = 0
    private var resolveEpoch = -1
    /// The project the target shell was found in, and whether a full search found it nowhere.
    private var shellProject: String?
    private var notFound = false
    private var observation = UUID()

    init(context: NshComponentContext, host: NSHTerminalHost) {
        self.context = context
        self.host = host
        let box = NSHTerminalBox()
        self.box = box
        hosting = UIHostingController(rootView: NSHTerminalRoot(box: box, model: host.model))
        hosting.view.backgroundColor = .clear
        hosting.view.frame = container.bounds
        hosting.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        container.clipsToBounds = true
        container.addSubview(hosting.view)
        lease.controller = hosting
        box.openLink = { [weak self] url in self?.emit("linkOpen", ["action": "open", "kind": "url", "target": url.absoluteString]) }
        box.inputRefused = { [weak self] reason in self?.emit("inputRefused", ["reason": reason]) }
        box.claim = { [weak self] in self?.claim() }
        box.retry = { [weak self] in self?.reattach() }
        container.onWindowChange = { [weak self, weak host, lease] in
            if let self { self.placementChanged() } else { host?.released(lease) }
        }
        container.onEditingChange = { [weak self] in self?.refresh() }
        container.commands = { [weak self] in self?.chordCommands() ?? [] }
        container.onChord = { [weak self] in self?.chord($0) }
    }

    // MARK: Props and commands

    func setProp(_ name: String, _ value: Any?) {
        switch name {
        case "target":
            guard let next = Self.target(value) else {
                target = nil
                placementChanged()
                send("failed", "invalid: terminal target")
                return
            }
            guard next != target else { return }
            target = next
            targetChanged()
            placementChanged()
        case "visible":
            let next = value as? Bool ?? false
            guard next != visible else { return }
            visible = next
            // Shown again: a terminal that could not be opened is tried again, as a tab selected again is on the Mac.
            if next { resolveAttempts = 0 }
            placementChanged()
        case "focusOnAttach": focusOnAttach = value as? Bool ?? false
        case "frozen": setFrozen(value as? Bool ?? false)
        case "fontScale":
            let next = (value as? NSNumber).map(\.doubleValue).flatMap { $0.isFinite && $0 > 0 ? $0 : nil } ?? 1
            guard next != fontScale else { return }
            fontScale = next
            refresh()
        // `pinned`: the model keeps the last screen of every shell it showed (`TerminalCache`). `colors`: the capture terminal draws in
        // the theme `ThemeStore` resolved, the one `platform.theme` pushes.
        default: break
        }
    }

    func command(_ name: String, _ args: Any?) {
        switch name {
        case "focus":
            // As on the Mac, focus also tries again a terminal that could not be put on screen.
            pendingFocus = true
            if lease.resolving == nil { resolveAttempts = 0 }
            claim()
        case "blur":
            pendingFocus = false
            focusAttempt?.cancel()
            box.keyFocus.dismiss()
            container.endEditing(true)
        case "reattach": reattach()
        // The screen and its scrollback, as the native menu's Copy screen text (the capture terminal has no selection to copy).
        case "copy": if box.face == .console { UIPasteboard.general.string = model.screenTextForCopy }
        // Frozen takes no input: neither puts anything in the shell or the composer, nor takes the keyboard.
        case "paste": if !frozen { paste() }
        case "selectAll":
            guard !frozen, box.face == .console, !model.directTyping, let field = Self.textField(in: container) else { return }
            _ = field.becomeFirstResponder()
            field.selectAll(nil)
        // No search over captured screens yet: a search finds nothing.
        case "find", "findNext", "findPrevious", "findClose": emit("findResult", ["total": 0, "current": 0])
        default: break
        }
    }

    private static func target(_ value: Any?) -> Target? {
        guard let object = value as? [String: Any], let kind = object["kind"] as? String else { return nil }
        switch kind {
        case "local": return .local
        case "remote":
            guard let desktop = object["desktopId"] as? String, let shell = object["shellId"] as? String,
                  !desktop.isEmpty, !shell.isEmpty, desktop.utf8.count <= 256, shell.utf8.count <= 256 else { return nil }
            return .remote(desktopID: desktop, shellID: shell)
        default: return nil
        }
    }

    private func targetChanged() {
        sentTitle = nil
        attachedOnce = false
        lease.resolving?.cancel(); lease.resolving = nil
        resolveAttempts = 0
        shellProject = nil
        notFound = false
    }

    /// Frozen: no touches, and the key view takes the keyboard by no path (`KeyFocus.held`: a tap, a ready shell, a hardware keyboard
    /// arriving, the app coming back, a sheet closing); a text field that takes it anyway gives it back (`updateKeyboard`). Thawed:
    /// the keyboard comes back if it was held, else the device's focus setting decides, as after a sheet.
    private func setFrozen(_ value: Bool) {
        guard value != frozen else { return }
        frozen = value
        container.isUserInteractionEnabled = !value
        box.keyFocus.held = value
        if value {
            refocusAfterFreeze = holdsKeyboard
            box.keyFocus.dismiss()
            container.endEditing(true)
        } else if refocusAfterFreeze {
            refocusAfterFreeze = false
            pendingFocus = true
        } else if box.face == .console, model.directTyping, host.isOwner(self), !host.nativePresented {
            box.keyFocus.autoFocus()
        }
        refresh()
    }

    private func claim() {
        host.setCandidate(self, isCandidate, claim: true)
        refresh()
    }

    /// Reconnect, or read the target's lists again and put it on screen: the strip's Reconnect and Retry after `failed`.
    private func reattach() {
        guard case .remote(let desktopID, _) = target else { return }
        lease.resolving?.cancel(); lease.resolving = nil
        resolveAttempts = 0
        shellProject = nil
        notFound = false
        claim()
        guard model.selectedDesktopID == desktopID, !host.nativePresented else { return }
        let model = self.model
        Task { [weak self] in
            switch model.state {
            case .connected: await model.refresh()
            case .connecting: break
            case .disconnected, .failed, .suspended: await model.connect()
            }
            self?.refresh()
        }
    }

    private func paste() {
        guard box.face == .console else { return }
        if model.directTyping, let view = box.keyFocus.view {
            view.paste(nil)
        } else if let field = Self.textField(in: container) {
            _ = field.becomeFirstResponder()
            field.paste(nil)
        }
    }

    // MARK: Placement

    private var isCandidate: Bool {
        guard visible, container.window != nil, case .remote = target else { return false }
        return true
    }

    private func placementChanged() {
        attachController()
        host.setCandidate(self, isCandidate)
        refresh()
    }

    /// The hosting controller is a child of the controller around the NSH view while it is in a window, so its sheets (the hotkey
    /// editor, the photo picker) present from the hierarchy and it gets appearance and safe-area updates.
    private func attachController() {
        if container.window != nil {
            guard hosting.parent == nil, let parent = Self.controller(of: container) else { return }
            parent.addChild(hosting)
            hosting.didMove(toParent: parent)
        } else if hosting.parent != nil {
            hosting.willMove(toParent: nil)
            hosting.removeFromParent()
        }
    }

    // MARK: Following the model

    /// What the view follows of the model, read under observation.
    private struct Seen {
        var loadFailed = false
        var paired = false
        var hostName = ""
        var interactive = false
        var state = ConnectionState.disconnected
        var error: String?
        var listsReady = false
        var selected = false
        var session: RemoteSession?
        /// Its shared tab is hidden on the Mac (a worker): not put on screen until the app opens it.
        var hiddenTab = false
        var missing = false
        var outputIsTarget = false
        var live = false
        var loadedProject: String?
        var title: String?
        var keyActive = false
        var editing = false
    }

    func refresh() {
        let token = UUID()
        observation = token
        let seen = withObservationTracking { read() } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, self.observation == token else { return }
                self.refresh()
            }
        }
        apply(seen)
    }

    private func read() -> Seen {
        var seen = Seen()
        seen.keyActive = box.keyFocus.isActive
        seen.editing = container.editing
        // The scale and visibility another live terminal may have changed under this one.
        _ = model.terminalFontScale
        _ = model.terminalVisible
        guard case .remote(let desktopID, let shellID) = target else { return seen }
        seen.loadFailed = model.loadFailed
        guard let desktop = model.desktops.first(where: { $0.id == desktopID }) else { return seen }
        seen.paired = true
        seen.hostName = desktop.name
        seen.interactive = model.selectedDesktopID == desktopID
        guard seen.interactive else { return seen }
        seen.state = model.state
        seen.error = model.error
        seen.listsReady = model.capabilitiesKnown
        seen.selected = model.sessionID == shellID && !model.terminalCovered
        seen.session = model.sessions.first { $0.id == shellID }
        if model.scope == .project, seen.session != nil, !model.openSessions.contains(where: { $0.id == shellID }) {
            seen.hiddenTab = model.sharedTabs?.allEntries.contains { $0.key == "shell:\(shellID)" && $0.hidden } == true
        }
        seen.missing = model.missingSessionIDs.contains(shellID)
        seen.outputIsTarget = model.outputSessionID == shellID
        seen.live = seen.outputIsTarget && !model.snapshotStale
        if model.scope == .project, let project = model.projectID, model.loadedProjectID == project { seen.loadedProject = project }
        if seen.selected { seen.title = model.session?.title }
        return seen
    }

    private func apply(_ seen: Seen) {
        let candidate = isCandidate, owner = host.isOwner(self), native = host.nativePresented
        updateKeyboard(seen, owner: owner, native: native)
        guard let target else {
            box.show(.empty)
            deactivate()
            return
        }
        guard case .remote(let desktopID, let shellID) = target else {
            box.show(visible ? .notice(text: Self.localMessage, busy: false, claim: false, retry: false) : .empty)
            deactivate()
            send("failed", Self.localMessage)
            return
        }
        let (state, message) = status(seen, desktopID: desktopID, owner: owner)
        let console = candidate && owner && seen.interactive && seen.selected
        if console {
            box.show(.console)
        } else if candidate {
            box.show(.notice(text: message ?? Self.text(for: state), busy: state == "attaching" || state == "reconnecting",
                             claim: state == "detached", retry: state == "failed"))
        } else {
            box.show(.empty)
        }
        if candidate {
            if state == "attached", !attachedOnce {
                attachedOnce = true
                if focusOnAttach { pendingFocus = true }
            }
            send(state, message)
            if console, let title = seen.title, title != sentTitle {
                sentTitle = title
                emit("title", ["title": title])
            }
        }
        if candidate && owner {
            // A native screen (a project pushed, a sheet over the NSH root): everything stays as it is under it, and a shell being put
            // on screen is not, so it cannot change the model's project or tab under that screen.
            if native { cancelResolve() } else { activate(seen, shellID: shellID) }
        } else {
            deactivate()
        }
        if pendingFocus, console, state == "attached", !frozen, !native { scheduleFocus() }
    }

    /// The contract's state for what the model shows: attaching (connecting, or putting the shell on screen), attached (its live
    /// screen), reconnecting (the link went; the last screen stays), ended (its process exited), gone (closed on the Mac), failed.
    private func status(_ seen: Seen, desktopID: String, owner: Bool) -> (String, String?) {
        if seen.loadFailed { return ("failed", "The saved pairings on this device could not be read.") }
        if !seen.paired { return ("failed", "This desktop is not paired with this device.") }
        if !seen.interactive {
            if host.interactiveHostID == desktopID { return ("attaching", nil) }
            return ("failed", "Connect to \(seen.hostName) to see this terminal.")
        }
        if !owner { return ("detached", "Shown in another pane.") }
        switch seen.state {
        case .connecting: return (attachedOnce ? "reconnecting" : "attaching", nil)
        case .suspended: return ("reconnecting", ConnectionState.suspended.label)
        case .failed: return (model.reconnectTask != nil ? "reconnecting" : "failed", seen.error ?? ConnectionState.failed.label)
        case .disconnected: return model.wantsConnection ? ("reconnecting", nil) : ("failed", "Disconnected from \(seen.hostName).")
        case .connected: break
        }
        if seen.selected {
            if seen.session?.alive == false { return ("ended", nil) }
            if seen.live { return ("attached", nil) }
            return (attachedOnce && seen.outputIsTarget ? "reconnecting" : "attaching", nil)
        }
        let gone: (String, String?) = ("gone", "This session was closed on the Mac.")
        if seen.missing { return gone }
        if let session = seen.session {
            if !session.alive { return ("ended", nil) }
            if session.mode == .chat { return ("failed", "This session runs as a chat.") }
            if seen.hiddenTab { return ("failed", Self.hiddenMessage) }
        } else if notFound || (shellProject != nil && seen.loadedProject == shellProject) {
            return gone
        }
        if lease.resolving == nil, resolveAttempts >= Self.resolveLimit { return ("failed", "This terminal could not be opened.") }
        return ("attaching", nil)
    }

    private static func text(for state: String) -> String {
        switch state {
        case "attaching": "Connecting to session…"
        case "reconnecting": "Reconnecting…"
        case "ended": "Session ended."
        case "gone": "This session has ended."
        default: "Could not attach to this session."
        }
    }

    /// The live terminal while nothing native is up: the model shows its shell, at its scale, polled and sized while it is on screen.
    private func activate(_ seen: Seen, shellID: String) {
        scaled = true
        if model.terminalFontScale != fontScale { model.setTerminalFontScale(fontScale) }
        let want = seen.interactive && seen.selected
        if model.terminalVisible != want { model.setTerminalVisible(want) }
        drivesTerminal = want
        guard seen.interactive, seen.state == .connected else {
            lease.freshness?.cancel(); lease.freshness = nil
            return
        }
        // The project's lists, as the native project screen reads them while it is up: a shell that exits or closes is seen.
        if lease.freshness == nil {
            let model = self.model
            lease.freshness = Task { await model.keepFresh(.sessions) }
        }
        if model.connectionEpoch != resolveEpoch {
            resolveEpoch = model.connectionEpoch
            resolveAttempts = 0
            notFound = false
        }
        guard !seen.selected, seen.listsReady, lease.resolving == nil, resolveAttempts < Self.resolveLimit, !seen.missing, !notFound else { return }
        if let session = seen.session {
            guard session.alive, session.mode != .chat, !seen.hiddenTab else { return }
        } else if let project = shellProject, seen.loadedProject == project {
            return
        }
        resolve(shellID)
    }

    /// Lets go of what this view set on the model, unless another live terminal or a native screen has it now.
    private func deactivate() {
        lease.resolving?.cancel(); lease.resolving = nil
        lease.freshness?.cancel(); lease.freshness = nil
        focusAttempt?.cancel(); focusAttempt = nil
        if handlesShortcuts {
            model.keyboard.tabShortcuts.clearAppHandler(owner: lease.shortcutOwner)
            handlesShortcuts = false
        }
        let handedOver = host.ownedByOther(self)
        if drivesTerminal {
            drivesTerminal = false
            if !handedOver, !host.nativePresented, model.terminalVisible { model.setTerminalVisible(false) }
        }
        if scaled {
            scaled = false
            if !handedOver { model.setTerminalFontScale(1) }
        }
    }

    // MARK: Putting the shell on screen

    /// Stops putting the shell on screen without counting the try: it is made again when the native screen goes.
    private func cancelResolve() {
        guard let task = lease.resolving else { return }
        task.cancel()
        lease.resolving = nil
        resolveAttempts = max(0, resolveAttempts - 1)
    }

    /// The resolve may still change the model: not cancelled, no native screen up, still the live terminal, the same connection.
    private func mayResolve(_ epoch: Int) -> Bool {
        !Task.isCancelled && !host.nativePresented && host.isOwner(self) && model.connectionEpoch == epoch && model.state == .connected
    }

    private func resolve(_ shellID: String) {
        resolveAttempts += 1
        let epoch = model.connectionEpoch
        lease.resolving = Task { [weak self] in
            await self?.select(shellID, epoch: epoch)
            guard let self, !Task.isCancelled else { return }
            self.lease.resolving = nil
            self.refresh()
        }
    }

    /// Selects the shell the way its tab is chosen: the global orchestrator's on the Global scope; a project's from its project's lists
    /// (read first when it is another project, or read again when it is this one). Nothing is created, and the Mac's tab strip is not
    /// written: a worker whose shared tab is hidden stays so (`status` says why) until the app opens its tab. After every wait it looks
    /// again whether it still may change the model (`mayResolve`).
    private func select(_ shell: String, epoch: Int) async {
        let model = self.model
        guard mayResolve(epoch) else { return }
        if OrchestratorScope.globalEntries(model.orchestrators).contains(where: { $0.id == shell }) {
            model.enterGlobal()
            if let session = model.openSessions.first(where: { $0.id == shell }) { await model.chooseSession(session) }
            return
        }
        if model.scope == .global { model.leaveGlobal() }
        if let session = model.openSessions.first(where: { $0.id == shell }) {
            await model.chooseSession(session)
            return
        }
        // One of this project's that is no tab on screen (a hidden worker): the app opens its tab, not this view.
        if model.sessions.contains(where: { $0.id == shell }) { return }
        switch await project(of: shell, epoch: epoch) {
        case .found(let project):
            guard mayResolve(epoch) else { return }
            shellProject = project
            if model.projectID == project, model.loadedProjectID == project {
                await model.refreshSessionsQuietly()
            } else {
                await model.chooseProject(project)
            }
            guard mayResolve(epoch), model.scope == .project, model.projectID == project,
                  let session = model.openSessions.first(where: { $0.id == shell }) else { return }
            await model.chooseSession(session)
        case .absent:
            if !Task.isCancelled { notFound = true }
        case .unknown:
            break
        }
    }

    private enum Lookup { case found(String), absent, unknown }

    /// The project a shell belongs to: from the lists already read (orchestrators, the loaded project, the dashboard), else by reading
    /// each project's shells until it turns up. A read that fails leaves the answer open.
    private func project(of shell: String, epoch: Int) async -> Lookup {
        let model = self.model
        if let id = (model.orchestrators + model.shells).first(where: { $0.id == shell })?.project_id { return .found(id) }
        if let dashboard = model.dashboard {
            let sessions = dashboard.attention.rows.compactMap(\.session) + dashboard.working.rows.map(\.session) + dashboard.finished.rows.map(\.session)
            if let id = sessions.first(where: { $0.target == .shell(id: shell) })?.projectID { return .found(id) }
        }
        var complete = true
        for project in model.projects {
            guard mayResolve(epoch) else { return .unknown }
            do {
                let shells = try await model.rpc("shells.list", ["project_id": .string(project.id)])["shells"].decode([RemoteSession].self)
                if shells.contains(where: { $0.id == shell }) { return .found(project.id) }
            } catch {
                complete = false
            }
        }
        return complete ? .absent : .unknown
    }

    // MARK: Keyboard

    /// The terminal holds the keyboard (its key view or its line composer): the Tab prefix's app actions come here, and the root's
    /// stand aside.
    private func updateKeyboard(_ seen: Seen, owner: Bool, native: Bool) {
        // Frozen: whatever took the keyboard inside (the dictation review's field, a responder UIKit put back) gives it back, and the
        // terminal is not said to hold it, so the root's Tab prefix and key host stay the app's.
        if frozen, seen.keyActive || seen.editing {
            box.keyFocus.dismiss()
            container.endEditing(true)
        }
        let holds = owner && !frozen && (seen.keyActive || seen.editing)
        if holds != holdsKeyboard {
            holdsKeyboard = holds
            box.holdsKeyboard = holds
            emit("focusChange", ["focused": holds])
            host.keyboardChanged()
        }
        let wantsShortcuts = holds && !native
        guard wantsShortcuts != handlesShortcuts else { return }
        handlesShortcuts = wantsShortcuts
        if wantsShortcuts {
            model.keyboard.tabShortcuts.setAppHandler(owner: lease.shortcutOwner) { [weak self] action in self?.tabShortcut(action) }
        } else {
            model.keyboard.tabShortcuts.clearAppHandler(owner: lease.shortcutOwner)
        }
    }

    /// The Clicks Tab prefix: what the native project screen did with these, the app now does. Escape, Tab, arrows, the key menu and
    /// the guide stay the terminal's.
    private func tabShortcut(_ action: TabShortcutAction) {
        let id: String? = switch action {
        case .selectTab(let number): "tab.select.\(number)"
        case .newTerminal: "terminal.new"
        case .projects: "nav.projects"
        case .modelPicker: "chat.model"
        default: nil
        }
        if let id { emit("command", ["id": id]) }
    }

    private func chordCommands() -> [UIKeyCommand] {
        guard holdsKeyboard, !frozen, !host.nativePresented else { return [] }
        return Self.chords.compactMap { chord in
            if terminalOwns(chord.input, chord.modifiers) { return nil }
            let command = UIKeyCommand(input: chord.input, modifierFlags: chord.modifiers, action: #selector(NSHTerminalContainer.nshTerminalChord(_:)))
            command.wantsPriorityOverSystemBehavior = true
            command.discoverabilityTitle = chord.title
            return command
        }
    }

    /// A custom terminal hotkey on the chord keeps it, as on the native screen.
    private func terminalOwns(_ input: String, _ modifiers: UIKeyModifierFlags) -> Bool {
        guard model.directTyping, let code = HIDKey.code(forCharacter: input) else { return false }
        return model.hotkeys.shortcutMap.action(for: KeyChord(keyCode: code, modifiers: KeyCaptureView.modifiers(modifiers))) != nil
    }

    private func chord(_ command: UIKeyCommand) {
        guard let chord = Self.chords.first(where: { $0.input == command.input && $0.modifiers == command.modifierFlags }) else { return }
        emit("command", ["id": chord.id])
    }

    /// Focus waits for the key view (or the composer) to be on screen: a few tries, a frame apart.
    private func scheduleFocus() {
        guard focusAttempt == nil else { return }
        focusAttempt = Task { [weak self] in
            for _ in 0..<10 {
                guard let self, !Task.isCancelled else { return }
                if self.focusNow() { break }
                try? await Task.sleep(for: .milliseconds(50))
            }
            self?.focusAttempt = nil
        }
    }

    private func focusNow() -> Bool {
        guard pendingFocus, box.face == .console, !frozen else { return true }
        if model.directTyping {
            guard let view = box.keyFocus.view, view.window != nil else { return false }
            pendingFocus = false
            box.keyFocus.focus()
        } else {
            guard let field = Self.textField(in: container), field.window != nil else { return false }
            pendingFocus = false
            _ = field.becomeFirstResponder()
        }
        return true
    }

    // MARK: Events

    private func emit(_ event: String, _ data: [String: Any]) { context.emit(event, data) }

    private func send(_ state: String, _ message: String?) {
        guard state != sentState || message != sentMessage else { return }
        sentState = state
        sentMessage = message
        var data: [String: Any] = ["state": state]
        if let message { data["message"] = message }
        emit("state", data)
    }

    // MARK: Views

    private static func controller(of view: UIView) -> UIViewController? {
        var responder: UIResponder? = view.next
        while let current = responder {
            if let controller = current as? UIViewController { return controller }
            responder = current.next
        }
        return nil
    }

    private static func textField(in view: UIView) -> UITextField? {
        if let field = view as? UITextField { return field }
        for child in view.subviews { if let field = textField(in: child) { return field } }
        return nil
    }
}

/// The component's view: hosts the SwiftUI terminal, answers the app chords while the keyboard is inside it, and reports its window
/// and the line composer's editing.
final class NSHTerminalContainer: UIView {
    var onWindowChange: (() -> Void)?
    var onEditingChange: (() -> Void)?
    var commands: () -> [UIKeyCommand] = { [] }
    var onChord: ((UIKeyCommand) -> Void)?
    /// The text field inside (the line composer) that is being edited. Ending is matched by the field itself: one removed while it
    /// was edited is no longer inside when it ends.
    private weak var editingField: UIView?
    var editing: Bool { editingField.map { $0.isFirstResponder && $0.isDescendant(of: self) } ?? false }

    override init(frame: CGRect) {
        super.init(frame: frame)
        let center = NotificationCenter.default
        center.addObserver(self, selector: #selector(editingBegan(_:)), name: UITextField.textDidBeginEditingNotification, object: nil)
        center.addObserver(self, selector: #selector(editingEnded(_:)), name: UITextField.textDidEndEditingNotification, object: nil)
    }
    required init?(coder: NSCoder) { nil }

    override var keyCommands: [UIKeyCommand]? { commands() }
    @objc func nshTerminalChord(_ command: UIKeyCommand) { onChord?(command) }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        onWindowChange?()
    }

    @objc private func editingBegan(_ note: Notification) {
        guard let field = note.object as? UIView, field.isDescendant(of: self) else { return }
        editingField = field
        onEditingChange?()
    }
    @objc private func editingEnded(_ note: Notification) {
        guard let field = note.object as? UIView, field === editingField else { return }
        editingField = nil
        onEditingChange?()
    }
}

/// What the component's SwiftUI shows: the terminal, or a line saying why not.
@MainActor @Observable final class NSHTerminalBox {
    enum Face: Equatable {
        case empty
        case console
        case notice(text: String, busy: Bool, claim: Bool, retry: Bool)
    }
    private(set) var face = Face.empty
    /// The iPad view's Follow output.
    var followOutput = true
    /// The terminal holds the keyboard: the Tab prefix's guide is drawn over it (the root's stands aside).
    var holdsKeyboard = false
    @ObservationIgnored let keyFocus = KeyFocus()
    @ObservationIgnored var openLink: @MainActor (URL) -> Void = { _ in }
    @ObservationIgnored var inputRefused: @MainActor (String) -> Void = { _ in }
    @ObservationIgnored var claim: @MainActor () -> Void = {}
    @ObservationIgnored var retry: @MainActor () -> Void = {}

    func show(_ next: Face) { if face != next { face = next } }
}

private struct NSHTerminalRoot: View {
    @Bindable var box: NSHTerminalBox
    let model: RemoteModel

    var body: some View {
        let style = model.theme.style
        Group {
            switch box.face {
            case .console:
                SessionConsole(model: model, followOutput: $box.followOutput, keyFocus: box.keyFocus, onInputRefused: { box.inputRefused($0) })
                    .overlay { if box.holdsKeyboard { TabShortcutGuide(controller: model.keyboard.tabShortcuts) } }
            case .notice(let text, let busy, let claim, let retry):
                NSHTerminalNotice(text: text, busy: busy, claim: claim ? { box.claim() } : nil, retry: retry ? { box.retry() } : nil)
            case .empty:
                Color.clear
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(style.terminalBackground)
        .environment(\.tabShortcuts, model.keyboard.tabShortcuts)
        .environment(\.terminalOpenLink, { box.openLink($0) })
        .desktopThemed(style)
        .onChange(of: model.appActive) { _, active in if !active { model.keyboard.tabShortcuts.cancel() } }
        .onChange(of: model.keyboard.hardware.isAttached) { _, _ in model.keyboard.tabShortcuts.cancel() }
    }
}

/// Why the terminal is not on screen: connecting, gone, in another pane.
private struct NSHTerminalNotice: View {
    @Environment(\.desktopStyle) private var style
    let text: String
    let busy: Bool
    let claim: (() -> Void)?
    let retry: (() -> Void)?

    var body: some View {
        VStack(spacing: 12) {
            if busy { ProgressView() }
            Text(text).font(style.face(12, relativeTo: .body)).foregroundStyle(style.terminalForeground.opacity(0.7))
                .multilineTextAlignment(.center)
            if let claim { Button("Show here", action: claim).buttonStyle(DesktopButtonStyle(compact: true)) }
            if let retry { Button("Try again", action: retry).buttonStyle(DesktopButtonStyle(compact: true)) }
        }
        .padding(20)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .contain)
    }
}
