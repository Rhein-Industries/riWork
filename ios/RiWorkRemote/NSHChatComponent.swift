import NativeShell
import Observation
import RiWorkCore
import SwiftUI
import UIKit

/// `riwork.chat` on iPad (docs/nsh-contracts.md §3.10, `apps/riwork-mobile/chat_view.ts`): a project's chat drawn by the TSX page under
/// its tab strip, as the Mac's window draws a chat tab. The transcript, the composer, approvals and questions, worker cards, dictation,
/// file previews and the chat's own keys are the native chat's (`ChatScreen`); JS sees the props, commands and events of the contract.
///
/// - Only a chat of the interactive host is shown; one of another host, or one the Mac no longer lists, says why (`unavailable`).
/// - The view drives the model's one chat selection as the native chat screen (`nav.present` `chat`) did: while it is shown it chooses
///   the chat's project, puts the chat up, reads the project's lists (`keepFresh(.sessions)`) and follows the chat. Hidden (`visible`
///   false, or off the window) it reads nothing and gives the keyboard back, and the chat stays as it was. Going away, or another
///   target, gives the selection back, so the page's terminal gets its shell back.
/// - It is no native screen: the bridge's `nativePresented` stays false and the pages keep their input. While a native screen is up the
///   model is that screen's, and nothing here changes it.
/// - Several components may exist; the one most recently shown, focused or tapped is live, the others say so.
@MainActor final class NSHChatHost {
    let model: RemoteModel
    private weak var bridge: NSHMobileBridge?
    /// Whether the live chat holds the keyboard changed: the root's own Tab prefix, chords and guide stand aside while it does.
    var onKeyboardChange: (() -> Void)?
    /// Pushes a file preview of the chat over the pages (`NSHMobileSurfaces.openFilesOverPage`).
    var presentFiles: (([ChatFileEntry], ChatFileEntry) -> Void)?
    private(set) var holdsKeyboard = false
    private struct Entry { weak var component: NSHChatComponent? }
    /// Chats on screen, the most recently shown or claimed last; that one is live.
    private var candidates: [Entry] = []
    private weak var owner: NSHChatComponent?

    init(model: RemoteModel, bridge: NSHMobileBridge) {
        self.model = model
        self.bridge = bridge
        bridge.observeNativePresented { [weak self] in
            Task { @MainActor [weak self] in self?.nativeChanged() }
        }
    }

    func register(on app: NshAppView) {
        app.register(component: "riwork.chat") { [self] context in NSHChatComponent(context: context, host: self) }
    }

    var nativePresented: Bool { bridge?.nativePresented ?? false }
    /// The host being opened, or the one open.
    var interactiveHostID: String? { bridge?.interactiveHostID ?? model.selectedDesktopID }
    func isOwner(_ component: NSHChatComponent) -> Bool { owner === component }

    /// A chat came on screen (`claim`: was shown, focused or tapped again) or went. Others whose standing changed look again.
    func setCandidate(_ component: NSHChatComponent, _ candidate: Bool, claim: Bool = false) {
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

    /// A component went away (NSH dropped it, its view on screen or not): what it started is let go, and the chat it put up is given
    /// back. Ending a lease twice does nothing the second time.
    func released(_ lease: NSHChatLease) {
        let chat = lease.selected
        lease.selected = nil
        lease.end()
        model.keyboard.tabShortcuts.clearAppHandler(owner: lease.shortcutOwner)
        candidates.removeAll { $0.component == nil }
        owner = candidates.last?.component
        if let chat { giveBack(chat) }
        owner?.refresh()
        keyboardChanged()
    }

    /// Gives the model's chat selection back, as leaving the native chat does (the terminals are on screen again), unless another view
    /// shows that chat. A tab request made for it that no listing has taken yet goes too.
    func giveBack(_ chat: String) {
        guard !candidates.contains(where: { $0.component?.chatID == chat }) else { return }
        if model.requestedTab?.key == "chat:\(chat)" { model.requestedTab = nil }
        // The project's own selection, also while the Global screen is up (`deselectChat` changes nothing there).
        if model.projectChatID == chat { model.projectChatID = nil }
    }
}

/// What a component started on the model that must stop with it, kept apart so it can be ended after the component is gone.
@MainActor final class NSHChatLease {
    let shortcutOwner = UUID()
    var freshness: Task<Void, Never>?
    var resolving: Task<Void, Never>?
    /// The chat this view put up (the model's chat selection), given back when the view goes.
    var selected: String?
    /// The component's hosting controller, a child of the root's controller while its view is in a window.
    weak var controller: UIViewController?
    func end() {
        freshness?.cancel(); freshness = nil
        resolving?.cancel(); resolving = nil
        // Out of the root's children and off the container, so the chat it draws (and what it observes of the model) goes too.
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

@MainActor final class NSHChatComponent: NshComponent {
    struct Target: Equatable {
        let desktopID: String
        let projectID: String
        let chatID: String
    }

    static let unlistedMessage = "This chat is no longer listed on the Mac."
    /// Tries to choose the chat's project and put the chat up, per connection, before saying it could not.
    private static let resolveLimit = 3

    private let context: NshComponentContext
    private let host: NSHChatHost
    private var model: RemoteModel { host.model }
    private let container = NSHChatContainer()
    private let box: NSHChatBox
    private let hosting: UIHostingController<NSHChatRoot>
    private let lease = NSHChatLease()
    var view: UIView { container }
    var chatID: String? { target?.chatID }

    // Props.
    private var target: Target?
    private var visible = false
    private var focusOnAttach = false

    // What was sent, and what this view has set on the model.
    private var sentState: String?
    private var sentMessage: String?
    private var sentTitle: String?
    private(set) var holdsKeyboard = false
    private var readyOnce = false
    /// The chat was hidden after it came up: shown again, the composer takes the keyboard back by the device's setting, as after a sheet.
    private var hiddenSinceReady = false
    private var pendingFocus = false
    private var focusAttempt: Task<Void, Never>?
    private var handlesShortcuts = false
    private var resolveAttempts = 0
    private var resolveEpoch = -1
    private var observation = UUID()

    init(context: NshComponentContext, host: NSHChatHost) {
        self.context = context
        self.host = host
        let box = NSHChatBox()
        self.box = box
        hosting = UIHostingController(rootView: NSHChatRoot(box: box, model: host.model))
        hosting.view.backgroundColor = .clear
        hosting.view.frame = container.bounds
        hosting.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        container.clipsToBounds = true
        container.addSubview(hosting.view)
        lease.controller = hosting
        box.claim = { [weak self] in self?.claim() }
        box.presentFiles = { [weak host] files, selected in host?.presentFiles?(files, selected) }
        container.onWindowChange = { [weak self, weak host, lease] in
            if let self { self.placementChanged() } else { host?.released(lease) }
        }
        container.onEditingChange = { [weak self] in self?.editingChanged() }
        container.commands = { [weak self] in self?.chordCommands() ?? [] }
        container.onChord = { [weak self] in self?.chord($0) }
    }

    deinit {
        // NSH dropped the component, its view in a window or not: what it set on the model is let go.
        Task { @MainActor [host, lease] in host.released(lease) }
    }

    // MARK: Props and commands

    func setProp(_ name: String, _ value: Any?) {
        switch name {
        case "target":
            let next = Self.target(value)
            if next != target {
                target = next
                targetChanged()
                placementChanged()
            }
            if next == nil { send("unavailable", "This chat is unavailable.") }
        case "visible":
            let next = value as? Bool ?? false
            guard next != visible else { return }
            visible = next
            // Shown again: a chat that could not be put up is tried again, as a tab selected again is on the Mac.
            if next { resolveAttempts = 0 }
            placementChanged()
        case "focusOnAttach": focusOnAttach = value as? Bool ?? false
        default: break
        }
    }

    func command(_ name: String, _ args: Any?) {
        switch name {
        case "focus":
            pendingFocus = true
            claim()
        case "blur":
            pendingFocus = false
            focusAttempt?.cancel(); focusAttempt = nil
            container.endEditing(true)
        case "menu": showMenu()
        default: break
        }
    }

    private static func target(_ value: Any?) -> Target? {
        guard let object = value as? [String: Any], let desktop = object["desktopId"] as? String,
              let project = object["projectId"] as? String, let chat = object["chatId"] as? String,
              !desktop.isEmpty, desktop.utf8.count <= 256, isUUID(project), isUUID(chat) else { return nil }
        return Target(desktopID: desktop, projectID: project, chatID: chat)
    }
    private static func isUUID(_ value: String) -> Bool { value.utf8.count == 36 && UUID(uuidString: value)?.uuidString.lowercased() == value }

    /// Another chat, or none: the one this view put up is given back, as when it goes.
    private func targetChanged() {
        sentTitle = nil
        readyOnce = false
        hiddenSinceReady = false
        pendingFocus = false
        lease.resolving?.cancel(); lease.resolving = nil
        resolveAttempts = 0
        if let chat = lease.selected, chat != target?.chatID {
            lease.selected = nil
            host.giveBack(chat)
        }
    }

    private func claim() {
        host.setCandidate(self, isCandidate, claim: true)
        refresh()
    }

    /// A tap into the composer of a chat shown in another pane makes this one live.
    private func editingChanged() {
        if container.editing, isCandidate, !host.isOwner(self) { host.setCandidate(self, true, claim: true) }
        refresh()
    }

    // MARK: Placement

    private var isCandidate: Bool { visible && container.window != nil && target != nil }

    private func placementChanged() {
        attachController()
        host.setCandidate(self, isCandidate)
        refresh()
    }

    /// The hosting controller is a child of the controller around the NSH view while it is in a window, so its sheets (the model
    /// picker, the photo picker, the notices) present from the hierarchy and it gets appearance, safe-area and keyboard updates.
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
        var projectScope = true
        /// The chat's project is the model's, and its lists are in.
        var loaded = false
        /// The chat as the project's tabs list it.
        var chat: ChatInfo?
        /// The model's chat selection (the project's, whatever screen is up) is this chat.
        var selected = false
        var title: String?
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
        seen.editing = container.editing
        guard let target else { return seen }
        seen.loadFailed = model.loadFailed
        guard let desktop = model.desktops.first(where: { $0.id == target.desktopID }) else { return seen }
        seen.paired = true
        seen.hostName = desktop.name
        seen.interactive = model.selectedDesktopID == target.desktopID
        guard seen.interactive else { return seen }
        seen.state = model.state
        seen.error = model.error
        seen.listsReady = model.capabilitiesKnown
        seen.projectScope = model.scope == .project
        _ = model.connectionEpoch
        seen.loaded = model.projectID == target.projectID && model.loadedProjectID == target.projectID
        guard seen.loaded, let tab = model.projectTabs.first(where: { $0.chatInfo?.id == target.chatID }) else { return seen }
        seen.chat = tab.chatInfo
        seen.selected = model.projectChatID == target.chatID
        // As the native chat's title: an orchestrator's name, else the chat's own (its transcript's word is fresher than the list's).
        if case .orchestratorChat(let session, _) = tab { seen.title = session.title }
        else if seen.selected, let chat = seen.chat { seen.title = ChatTabs.title(model.chatConversations[chat.id]?.transcript.info ?? chat) }
        return seen
    }

    private func apply(_ seen: Seen) {
        let candidate = isCandidate, owner = host.isOwner(self), native = host.nativePresented
        let live = candidate && owner
        updateKeyboard(seen, live: live, native: native)
        if container.isUserInteractionEnabled != visible { container.isUserInteractionEnabled = visible }
        guard let target else {
            box.show(.empty)
            deactivate()
            return
        }
        let (state, message) = status(seen, target: target, elsewhere: candidate && !owner)
        switch state {
        case "ready": if let chat = seen.chat { box.show(.chat(chat)) }
        case "loading": box.show(.loading)
        default: box.show(.notice(text: message ?? "", claim: candidate && !owner))
        }
        let shown = live && !native
        if box.follows != (shown && state == "ready") { box.follows = shown && state == "ready" }
        if box.takesFocus != shown { box.takesFocus = shown }
        if shown {
            if hiddenSinceReady, state == "ready" { hiddenSinceReady = false; box.refocus &+= 1 }
        } else if readyOnce, !candidate {
            hiddenSinceReady = true
        }
        if candidate {
            if state == "ready", !readyOnce {
                readyOnce = true
                if focusOnAttach { pendingFocus = true }
            }
            send(state, message)
            if state == "ready", let title = seen.title, title != sentTitle {
                sentTitle = title
                emit("title", ["title": title])
            }
        }
        if live {
            // A native screen (a sheet over the root, a pushed screen): everything stays as it is under it, and the chat's project is
            // not being chosen, so it cannot change the model's project under that screen.
            if native { cancelResolve() } else { activate(seen, target: target) }
        } else {
            deactivate()
        }
        if pendingFocus, shown, state == "ready" { scheduleFocus() }
    }

    /// The contract's state for what the model shows: `ready` (the chat is up; it stays up through a dropped link, saying so itself),
    /// `loading` (connecting, or its project's lists are being read and the chat put up), `unavailable` with why.
    private func status(_ seen: Seen, target: Target, elsewhere: Bool) -> (String, String?) {
        if seen.loadFailed { return ("unavailable", "The saved pairings on this device could not be read.") }
        if !seen.paired { return ("unavailable", "This desktop is not paired with this device.") }
        if !seen.interactive {
            if host.interactiveHostID == target.desktopID { return ("loading", nil) }
            return ("unavailable", "Connect to \(seen.hostName) to see this chat.")
        }
        if elsewhere { return ("unavailable", "Shown in another pane.") }
        if seen.selected, seen.chat != nil { return ("ready", nil) }
        switch seen.state {
        case .connecting, .suspended: return ("loading", nil)
        case .failed: return model.reconnectTask != nil ? ("loading", nil) : ("unavailable", seen.error ?? ConnectionState.failed.label)
        case .disconnected: return model.wantsConnection ? ("loading", nil) : ("unavailable", "Disconnected from \(seen.hostName).")
        case .connected: break
        }
        if seen.loaded, seen.chat == nil, lease.resolving == nil, resolveAttempts > 0 { return ("unavailable", Self.unlistedMessage) }
        if lease.resolving == nil, resolveAttempts >= Self.resolveLimit { return ("unavailable", "This chat could not be opened.") }
        return ("loading", nil)
    }

    /// The live chat while nothing native is up: the project's lists are read while it is shown, and the chat is put up as its tab is
    /// chosen (its project chosen first when the model has another, or its lists read again when the chat is not in them yet).
    private func activate(_ seen: Seen, target: Target) {
        guard seen.interactive else { deactivate(); return }
        if lease.freshness == nil {
            let model = self.model
            lease.freshness = Task { await model.keepFresh(.sessions) }
        }
        if model.connectionEpoch != resolveEpoch {
            resolveEpoch = model.connectionEpoch
            resolveAttempts = 0
        }
        guard seen.state == .connected, seen.listsReady, seen.projectScope else { return }
        if seen.selected { lease.selected = target.chatID; return }
        if seen.chat != nil {
            model.selectChat(target.chatID)
            if model.projectChatID == target.chatID { lease.selected = target.chatID }
            return
        }
        // Not listed: the project is chosen (or its lists read again) once, and then the activity reads find it if it comes.
        guard lease.resolving == nil, resolveAttempts < Self.resolveLimit, !seen.loaded || resolveAttempts == 0 else { return }
        resolve(target)
    }

    /// Not live: nothing is read or put up, and the chat's keys are let go. The chat this view put up stays the model's selection while
    /// the view lasts (hidden, it is still the tab on screen).
    private func deactivate() {
        lease.resolving?.cancel(); lease.resolving = nil
        lease.freshness?.cancel(); lease.freshness = nil
        focusAttempt?.cancel(); focusAttempt = nil
        if handlesShortcuts {
            model.keyboard.tabShortcuts.clearAppHandler(owner: lease.shortcutOwner)
            handlesShortcuts = false
        }
    }

    /// Stops choosing the chat's project without counting the try: it is made again when the native screen goes.
    private func cancelResolve() {
        guard let task = lease.resolving else { return }
        task.cancel()
        lease.resolving = nil
        resolveAttempts = max(0, resolveAttempts - 1)
    }

    /// Chooses the chat's project as a dashboard row's tab request does: once its lists are in (read again when it is the model's
    /// already), the chat is put up (`applyRequestedTab`).
    private func resolve(_ target: Target) {
        resolveAttempts += 1
        let model = self.model
        lease.resolving = Task { [weak self] in
            guard let self, !Task.isCancelled, !self.host.nativePresented else { return }
            model.requestProjectTab(target.projectID, key: "chat:\(target.chatID)")
            await model.chooseProject(target.projectID)
            guard !Task.isCancelled else { return }
            self.lease.resolving = nil
            self.refresh()
        }
    }

    // MARK: Keyboard

    /// The chat holds the keyboard (its composer, or a question's field): the Tab prefix's app actions and the app chords are its, and
    /// the root's stand aside. Hidden, it gives the keyboard back.
    private func updateKeyboard(_ seen: Seen, live: Bool, native: Bool) {
        if seen.editing, !visible || container.window == nil { container.endEditing(true) }
        let holds = live && seen.editing
        if holds != holdsKeyboard {
            holdsKeyboard = holds
            if box.holdsKeyboard != holds { box.holdsKeyboard = holds }
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

    /// The Clicks Tab prefix, as the native chat had it: the page's actions go to the app (`command`), the model picker is the chat's.
    private func tabShortcut(_ action: TabShortcutAction) {
        switch action {
        case .selectTab(let number): emit("command", ["id": "tab.select.\(number)"])
        case .newTerminal: emit("command", ["id": "terminal.new"])
        case .projects: emit("command", ["id": "nav.projects"])
        case .modelPicker: if model.state == .connected { model.keyboard.requestModelPicker() }
        default: break
        }
    }

    /// The project's app chords while the chat holds the keyboard (⌘⇧P is `nav.back`), not while an input method composes a word.
    /// ⌘. (interrupt) and ⌘M (the model picker) stay the chat's own.
    private func chordCommands() -> [UIKeyCommand] {
        guard holdsKeyboard, !host.nativePresented, !container.composing else { return [] }
        return NSHMobileBridge.projectChords.map { chord in
            let command = UIKeyCommand(input: chord.input, modifierFlags: chord.modifiers, action: #selector(NSHChatContainer.nshChatChord(_:)))
            command.wantsPriorityOverSystemBehavior = true
            command.discoverabilityTitle = chord.title
            return command
        }
    }

    private func chord(_ command: UIKeyCommand) {
        guard holdsKeyboard, let chord = NSHMobileBridge.projectChords.first(where: { $0.input == command.input && $0.modifiers == command.modifierFlags }) else { return }
        emit("command", ["id": chord.id])
    }

    /// Focus waits for the composer to be on screen: a few tries, a frame apart.
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
        guard pendingFocus, visible, case .chat = box.face else { return true }
        guard let composer = Self.composer(in: container), composer.window != nil else { return false }
        pendingFocus = false
        // Not editable while the link is down: the device's own focus setting brings the keyboard back with it.
        if composer.isEditable { _ = composer.becomeFirstResponder() }
        return true
    }

    // MARK: The chat's menu

    /// The chat's own actions, as its ⋯ had them on the native screen (`ChatMenuSection`), with its model and approval mode: an action
    /// sheet from the chat's top trailing corner, under the page's ⋯.
    private func showMenu() {
        guard case .chat(let listed) = box.face, container.window != nil, !host.nativePresented else { return }
        let model = self.model, chat = listed.id
        let conversation = model.chatConversations[chat] ?? model.conversation(chat)
        let info = conversation.transcript.info ?? listed
        let state = model.chatState(info)
        let connected = model.state == .connected
        let mode = conversation.pendingMode ?? info.approvalMode
        let sheet = UIAlertController(title: model.orchestrator(ofChat: chat)?.title ?? ChatTabs.title(info), message: nil, preferredStyle: .actionSheet)
        func add(_ title: String, style: UIAlertAction.Style = .default, enabled: Bool = true, _ run: @escaping @MainActor () -> Void) {
            let action = UIAlertAction(title: title, style: style) { _ in run() }
            action.isEnabled = enabled
            sheet.addAction(action)
        }
        add("Choose model…", enabled: connected) { model.keyboard.requestModelPicker() }
        add("Approval mode: \(mode.title)…", enabled: connected) { [weak self] in self?.showModes(chat) }
        add("Expand all activity") { conversation.discloseAll(true) }
        add("Collapse all activity") { conversation.discloseAll(false) }
        add("Compact context", enabled: connected && !state.isBusy && state != .starting) { Task { await model.compactChat(chat) } }
        add("Jump to latest") { conversation.jumpToEnd() }
        let notices = ChatNotices.all(conversation.transcript.items).count
        if notices > 0 { add("Notices (\(notices))…") { [weak self] in self?.box.notices &+= 1 } }
        if let id = info.providerThreadID { add("Copy session id") { UIPasteboard.general.string = id } }
        add("Stop agent", style: .destructive, enabled: connected && state != .stopped) { Task { await model.stopChat(chat) } }
        sheet.addAction(UIAlertAction(title: "Cancel", style: .cancel))
        present(sheet)
    }

    /// The approval modes; the one in force is said and not offered again.
    private func showModes(_ chat: String) {
        guard case .chat(let listed) = box.face, listed.id == chat else { return }
        let model = self.model
        let conversation = model.chatConversations[chat] ?? model.conversation(chat)
        let shown = conversation.pendingMode ?? (conversation.transcript.info ?? listed).approvalMode
        let sheet = UIAlertController(title: "Approval mode", message: "\(shown.title): \(shown.detail)", preferredStyle: .actionSheet)
        for mode in ChatApprovalMode.allCases {
            let action = UIAlertAction(title: "\(mode.title) · \(mode.detail)", style: .default) { _ in Task { await model.setChatMode(chat, mode) } }
            action.isEnabled = mode != shown
            sheet.addAction(action)
        }
        sheet.addAction(UIAlertAction(title: "Cancel", style: .cancel))
        present(sheet)
    }

    /// An action sheet from the chat: on iPad a popover at its top trailing corner, under the page's ⋯. A sheet still going away is
    /// waited for a moment; anything else over the window (a native screen, the chat's own sheets) is left alone.
    private func present(_ sheet: UIAlertController, tries: Int = 5) {
        guard let window = container.window, !host.nativePresented,
              let presenter = hosting.parent != nil ? hosting : Self.controller(of: container) else { return }
        if let covering = window.rootViewController?.presentedViewController {
            guard covering.isBeingDismissed, tries > 0 else { return }
            Task { [weak self] in
                try? await Task.sleep(for: .milliseconds(100))
                self?.present(sheet, tries: tries - 1)
            }
            return
        }
        if let popover = sheet.popoverPresentationController {
            let side: CGFloat = 44
            let rightToLeft = container.effectiveUserInterfaceLayoutDirection == .rightToLeft
            popover.sourceView = container
            popover.sourceRect = CGRect(x: rightToLeft ? 0 : max(0, container.bounds.width - side), y: 0, width: side, height: 1)
            popover.permittedArrowDirections = [.up]
        }
        presenter.present(sheet, animated: true)
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

    private static func composer(in view: UIView) -> ChatComposerTextView? {
        if let composer = view as? ChatComposerTextView { return composer }
        for child in view.subviews { if let composer = composer(in: child) { return composer } }
        return nil
    }
}

/// The component's view: hosts the SwiftUI chat, answers the app chords while the keyboard is inside it, and reports its window and
/// whether a text view inside it (the composer, a question's field) is being edited.
final class NSHChatContainer: UIView {
    var onWindowChange: (() -> Void)?
    var onEditingChange: (() -> Void)?
    var commands: () -> [UIKeyCommand] = { [] }
    var onChord: ((UIKeyCommand) -> Void)?
    /// The text view or field inside that is being edited. Ending is matched by the view itself: one removed while it was edited is no
    /// longer inside when it ends.
    private weak var editingView: UIView?
    var editing: Bool { editingView.map { $0.isFirstResponder && $0.isDescendant(of: self) } ?? false }
    /// An input method is composing a word in it (marked text): its keys are the input method's.
    var composing: Bool { editing && (editingView as? any UITextInput)?.markedTextRange != nil }

    override init(frame: CGRect) {
        super.init(frame: frame)
        let center = NotificationCenter.default
        for name in [UITextView.textDidBeginEditingNotification, UITextField.textDidBeginEditingNotification] {
            center.addObserver(self, selector: #selector(editingBegan(_:)), name: name, object: nil)
        }
        for name in [UITextView.textDidEndEditingNotification, UITextField.textDidEndEditingNotification] {
            center.addObserver(self, selector: #selector(editingEnded(_:)), name: name, object: nil)
        }
    }
    required init?(coder: NSCoder) { nil }

    override var keyCommands: [UIKeyCommand]? { commands() }
    @objc func nshChatChord(_ command: UIKeyCommand) { onChord?(command) }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        onWindowChange?()
    }

    @objc private func editingBegan(_ note: Notification) {
        guard let view = note.object as? UIView, view.isDescendant(of: self) else { return }
        editingView = view
        onEditingChange?()
    }
    @objc private func editingEnded(_ note: Notification) {
        guard let view = note.object as? UIView, view === editingView else { return }
        editingView = nil
        onEditingChange?()
    }
}

/// What the component's SwiftUI shows: the chat, a spinner while it is put up, or a line saying why not.
@MainActor @Observable final class NSHChatBox {
    enum Face: Equatable {
        case empty
        case loading
        case chat(ChatInfo)
        case notice(text: String, claim: Bool)
    }
    private(set) var face = Face.empty
    /// The chat is followed (its long poll): it is live and nothing native is over it.
    var follows = false
    /// The composer may take the keyboard by itself.
    var takesFocus = false
    /// The chat holds the keyboard: the Tab prefix's guide is drawn over it (the root's stands aside).
    var holdsKeyboard = false
    /// Counts the times the chat was shown again after being hidden.
    var refocus = 0
    /// Counts the menu's Notices…: the notice history comes up.
    var notices = 0
    @ObservationIgnored var claim: @MainActor () -> Void = {}
    @ObservationIgnored var presentFiles: @MainActor ([ChatFileEntry], ChatFileEntry) -> Void = { _, _ in }

    func show(_ next: Face) { if face != next { face = next } }
}

private struct NSHChatRoot: View {
    @Bindable var box: NSHChatBox
    let model: RemoteModel

    var body: some View {
        let style = model.theme.style
        Group {
            switch box.face {
            case .chat(let chat):
                ChatScreen(model: model, chat: chat, refocus: box.refocus, openNotices: box.notices, follows: box.follows, takesFocus: box.takesFocus)
                    .id(chat.id)
                    .overlay { if box.holdsKeyboard { TabShortcutGuide(controller: model.keyboard.tabShortcuts) } }
            case .loading:
                ProgressView()
            case .notice(let text, let claim):
                NSHChatNotice(text: text, claim: claim ? { box.claim() } : nil)
            case .empty:
                Color.clear
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(style.background)
        .environment(\.tabShortcuts, model.keyboard.tabShortcuts)
        .environment(\.chatPresentFiles, { files, selected in box.presentFiles(files, selected) })
        .desktopThemed(style)
        .onChange(of: model.appActive) { _, active in if !active { model.keyboard.tabShortcuts.cancel() } }
        .onChange(of: model.keyboard.hardware.isAttached) { _, _ in model.keyboard.tabShortcuts.cancel() }
        .onChange(of: model.keyboard.software.isShown) { _, _ in model.keyboard.tabShortcuts.cancel() }
        .onChange(of: model.keyboard.layout) { _, _ in model.keyboard.tabShortcuts.cancel() }
    }
}

/// Why the chat is not on screen: another host's, no longer listed, shown in another pane.
private struct NSHChatNotice: View {
    @Environment(\.desktopStyle) private var style
    let text: String
    let claim: (() -> Void)?

    var body: some View {
        VStack(spacing: 12) {
            Text(text).font(style.system(.body)).foregroundStyle(style.muted).multilineTextAlignment(.center)
            if let claim { Button("Show here", action: claim).buttonStyle(DesktopButtonStyle(compact: true)) }
        }
        .padding(20)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .contain)
    }
}
