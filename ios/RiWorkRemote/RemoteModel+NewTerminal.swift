import Foundation
import Observation
import RiWorkCore

/// Whether the desktop understands `shell.create` and `shell.close`, learned from the first call.
enum TerminalControlSupport: Equatable { case unknown, supported, unsupported }

// Opening and closing terminals on the desktop. Neither request is ever retried by the app: a lost answer leaves the outcome
// unknown, and the person is told to look at the terminal list.
extension RemoteModel {
    static let newTerminalKindKey = "riwork.newTerminal.kind"

    /// The kind the last terminal was opened with (not whether it was unrestricted: that is chosen on purpose, every time). A chat kind
    /// remembered per provider by an older version is the chat.
    var lastTerminalKind: NewTerminalKind {
        defaults.string(forKey: Self.newTerminalKindKey).flatMap(NewTerminalKind.remembered) ?? .standard
    }
    func rememberTerminalKind(_ kind: NewTerminalKind) { defaults.set(kind.rawValue, forKey: Self.newTerminalKindKey) }

    /// The sheet and the ⌘N shortcut can open: connected, a project chosen, and the desktop not known to be too old.
    var canOpenNewTerminal: Bool { state == .connected && projectID != nil && terminalControl != .unsupported }
    /// Shown instead of the form's Create button when the desktop is too old.
    var terminalControlNotice: String? { terminalControl == .unsupported ? TerminalControlError.unsupportedMessage : nil }
    /// Whether a terminal can be closed from the phone: connected, a desktop that takes the request, nothing else in flight.
    func canClose(_ session: RemoteSession) -> Bool {
        session.kind != "orchestrator" && state == .connected && terminalControl != .unsupported && closingTerminalID == nil
    }
    /// The terminal's own Close (not a shared tab's): only once the connection's capabilities are known, only for a desktop that does
    /// not share tabs, and never for a shell ever seen as a shared worker.
    func legacyCloseAvailable(_ session: RemoteSession) -> Bool {
        capabilitiesKnown && !desktopFeatures.tabs && !everSharedWorkers.contains(session.id) && canClose(session)
    }

    /// The form for the project on screen, with the target and kind a person most likely wants.
    func newTerminalForm() -> NewTerminalForm? {
        guard let project = projectID else { return nil }
        let options = NewTerminalTargets.options(projectID: project, projects: projects, worktrees: worktrees)
        let viewed = session.flatMap { $0.project_id == project ? $0.worktree_id : nil }
        var form = NewTerminalForm(targets: options, targetIndex: NewTerminalTargets.preselected(in: options, selectedWorktreeID: viewed), kind: lastTerminalKind,
                                   kinds: NewTerminalKind.offered(chats: chatsOffered, orchestrators: orchestratorsOffered), chatProvider: lastChatProvider)
        // A desktop whose Settings decide whether its agent terminals are unrestricted: the sheet has no switch for them.
        form.agentsFollowDesktop = desktopFeatures.shellCreateAsSettings
        return form
    }

    /// Opens a terminal and switches to it. Returns nil on success, otherwise why not. `onCreated` runs as soon as the desktop has
    /// answered, before the new terminal's first screen is read, so a sheet can go away at once.
    @discardableResult
    func createTerminal(_ request: NewTerminalRequest, onCreated: @MainActor (RemoteSession) -> Void = { _ in }) async -> TerminalControlError? {
        guard !creatingTerminal else { return .busy }
        guard state == .connected else { return .notConnected }
        guard terminalControl != .unsupported else { return .unsupported }
        creatingTerminal = true
        defer { creatingTerminal = false }
        let token = generation
        let reply: NewTerminalReply
        do {
            reply = try await client.createTerminal(request)
        } catch {
            let failure = TerminalControlError.from(error, operation: .create(request.kind))
            if generation == token, failure == .unsupported { terminalControl = .unsupported }
            return failure
        }
        if generation == token { terminalControl = .supported }
        rememberTerminalKind(request.kind)
        // A reconnect or another desktop in the meantime: the terminal exists, but this screen is no longer its.
        guard generation == token, state == .connected else { return nil }
        let session = reply.session
        if session.project_id == projectID {
            if let index = shells.firstIndex(where: { $0.id == session.id }) { shells[index] = session } else { shells.append(session) }
        }
        if let project = projectID, let list = await sharedTabsOfProject(project), generation == token, projectID == project { acceptSharedTabs(list) }
        guard generation == token, state == .connected else { return nil }
        onCreated(session)
        // Opened like any tab: selected, resized to the phone, then read.
        await chooseSession(session)
        await refreshShellsQuietly()
        return nil
    }

    /// Closes a project terminal on the desktop (the process in it ends), after the person confirmed. Returns nil on success.
    @discardableResult
    /// Ends a terminal (`shell.close`). `viaSharedTab`: called by a shared tab's Exit, after its Hide (`closeTab`); any other caller is
    /// the terminal's own Close, which exists only for a desktop known not to share tabs.
    func closeTerminal(_ session: RemoteSession, viaSharedTab: Bool = false) async -> TerminalControlError? {
        // Only a project terminal can be closed (the relay refuses anything else); a shared tab's Exit is refused before its Hide.
        guard session.kind == "project" else { return .failed("Orchestrators are closed on the Mac.") }
        // A shell ever seen as a shared worker is only ever detached, on any connection: nothing here ends it.
        if everSharedWorkers.contains(session.id) || sharedEntry(ofSession: session.id)?.isWorker == true {
            return .failed("A worker's shell is detached, not closed, from the phone.")
        }
        guard closingTerminalID == nil else { return .busy }
        guard state == .connected else { return .notConnected }
        guard terminalControl != .unsupported else { return .unsupported }
        if !viaSharedTab {
            // The connection's capabilities are not known yet (just connected), or the desktop shares tabs: closing goes by the tab
            // (Ask / Detach / Exit, Hide before Exit), never straight to shell.close. A confirmation from before a reconnect ends here.
            guard capabilitiesKnown else { return .failed("Still reading what the Mac offers. Try again in a moment.") }
            guard !desktopFeatures.tabs else { return .failed("Close it from its tab.") }
        }
        let request: CloseTerminalRequest
        do { request = try CloseTerminalRequest(shellID: session.id) } catch { return .failed(error.localizedDescription) }
        closingTerminalID = session.id
        defer { closingTerminalID = nil }
        let token = generation
        // Leave the terminal first when it is the one on screen: its viewport is released and the neighbour pinned in the usual way,
        // and a last read of a shell that is about to end cannot put up an error.
        let wasSelected = sessionID == session.id
        let neighbour = wasSelected ? neighbour(of: session) : nil
        if let neighbour { await chooseSession(neighbour) }
        do {
            try await client.closeTerminal(request)
            if generation == token { terminalControl = .supported }
        } catch {
            let failure = TerminalControlError.from(error, operation: .close)
            if generation == token, failure == .unsupported { terminalControl = .unsupported }
            // Already gone is the state that was asked for.
            if case .notFound = failure {} else {
                if wasSelected, neighbour != nil, generation == token, sessionID == neighbour?.id, openSessions.contains(where: { $0.id == session.id }) { await chooseSession(session) }
                return failure
            }
        }
        guard generation == token else { return nil }
        shells.removeAll { $0.id == session.id }
        if let desktopID = selectedDesktopID {
            let key = KeyBufferKey(desktopID: desktopID, shellID: session.id)
            keyBuffers[key] = nil; keysFull.remove(key)
        }
        if sessionID == session.id {
            // The only terminal: the selection falls to nothing (or to a live one the list still has).
            if viewportSessionID == session.id { viewportSessionID = nil; appliedViewport = nil }
            try? reconcileSelectedSession()
            if sessionID == nil { try? await synchronizeViewport(token: token) }
        }
        await refreshShellsQuietly()
        return nil
    }

    /// The tab to land on when `session` goes: the one after it, else the one before.
    func neighbour(of session: RemoteSession) -> RemoteSession? {
        let open = openSessions
        guard let index = open.firstIndex(where: { $0.id == session.id }) else { return nil }
        if open.indices.contains(index + 1) { return open[index + 1] }
        return index > 0 ? open[index - 1] : nil
    }

    /// Reads the project's terminal list again, without the side effects of a full refresh.
    func refreshShellsQuietly() async {
        guard state == .connected, let project = projectID else { return }
        let token = generation
        guard let listed = try? await rpc("shells.list", ["project_id": .string(project)])["shells"].decode([RemoteSession].self) else { return }
        guard generation == token, projectID == project, state == .connected else { return }
        if listed != shells { shells = listed }
    }
}

/// The "New terminal" sheet's state: the form, what went wrong, and whether a request is on its way. Touch and keyboard both go
/// through here, so they cannot disagree.
/// What the sheet says when a request did not go through: the words, and whether it may have happened anyway.
struct NewTabProblem: Equatable {
    let message: String
    let outcomeIsUncertain: Bool
    init(_ error: TerminalControlError) { message = error.message; outcomeIsUncertain = error.outcomeIsUncertain }
    init(_ error: ChatControlError) { message = error.message; outcomeIsUncertain = error.outcomeIsUncertain }
    init(_ error: OrchestratorControlError) { message = error.message; outcomeIsUncertain = error.outcomeIsUncertain }
}

@MainActor @Observable final class NewTerminalSheetModel: Identifiable {
    let id = UUID()
    var form: NewTerminalForm
    /// The providers whose models are being read for a new chat.
    var loadingProviders: Set<ChatProvider> = []
    var loadingChatModels: Bool { !loadingProviders.isEmpty }
    var chatModelsSources: [ChatProvider: ChatCatalogueSource] = [:]
    /// Why a provider's live list could not be read, per provider: one provider's trouble is said under its own rows.
    var chatModelsErrors: [ChatProvider: String] = [:]
    /// The error of the provider that is chosen.
    var chatModelsError: String? { chatModelsErrors[form.chatProvider] }
    var catalogueRequests: [ChatProvider: UUID] = [:]
    var error: TerminalControlError?
    /// The same for a chat, which has its own errors.
    var chatError: ChatControlError?
    /// And for an orchestrator.
    var orchestratorError: OrchestratorControlError?
    /// The focus ring shows only once a key was pressed; a finger does not need it.
    var keyboardInUse = false
    @ObservationIgnored let model: RemoteModel
    /// The sheet should go away (a terminal was created, or Cancel / Esc).
    @ObservationIgnored var dismiss: () -> Void = {}
    /// Between Return / Create and the answer, including the moment before the request itself starts, so a double tap or a held
    /// Return can only ever send one.
    private(set) var submitting = false
    @ObservationIgnored private(set) var pending: Task<Void, Never>?

    init?(model: RemoteModel) {
        guard var form = model.newTerminalForm() else { return nil }
        form.chatChoices = model.rememberedChatChoices()
        self.form = form
        self.model = model
    }

    var busy: Bool { submitting || model.creatingTerminal || model.creatingChat || model.creatingOrchestrator }
    /// The desktop is too old for what is chosen: terminals (`shell.create`), chats or orchestrators.
    var unsupported: Bool {
        if form.kind.isOrchestrator { return model.orchestratorCreateSupport == .unsupported }
        return form.kind.isChat ? model.chatSupport == .unsupported : model.terminalControl == .unsupported
    }
    var canCreate: Bool { !busy && !unsupported && model.state == .connected && (form.target != nil || form.kind == .globalOrchestrator) }
    /// The message shown inline: what the last attempt said, or that the desktop is too old.
    var message: TerminalControlError? { unsupported && !form.kind.isChat && !form.kind.isOrchestrator ? .unsupported : (form.kind.isChat || form.kind.isOrchestrator ? nil : error) }
    /// The same, for any kind of request.
    var problem: NewTabProblem? {
        if form.kind.isOrchestrator { return (unsupported ? OrchestratorControlError.unsupported : orchestratorError).map(NewTabProblem.init) }
        if form.kind.isChat { return (unsupported ? ChatControlError.unsupported : chatError).map(NewTabProblem.init) }
        return message.map(NewTabProblem.init)
    }
    /// What the Create button's hint says when it is off because the desktop is too old.
    var unsupportedMessage: String {
        if form.kind.isOrchestrator { return OrchestratorControlError.unsupportedMessage }
        return form.kind.isChat ? ChatControlError.unsupportedMessage : TerminalControlError.unsupportedMessage
    }

    /// A key from a hardware keyboard.
    func press(_ key: NewTerminalForm.Key) {
        keyboardInUse = true
        if key != .space { error = nil; chatError = nil; orchestratorError = nil }
        form.handle(key)
        if key == .space, form.focus == .create { create() }
    }
    func select(kind: NewTerminalKind) {
        keyboardInUse = false; error = nil; chatError = nil; orchestratorError = nil
        form.select(kind: kind); form.focus = .kind
    }
    /// A chat with one of `provider`'s models (its remembered one, or its default).
    func selectChat(_ provider: ChatProvider) {
        select(kind: .chat)
        form.selectChatProvider(provider)
    }
    func select(targetAt index: Int) {
        keyboardInUse = false; error = nil; chatError = nil; orchestratorError = nil
        form.select(targetAt: index); form.focus = .target
    }
    func setOrchestratorMode(_ mode: NewOrchestratorMode) {
        guard !busy else { return }
        keyboardInUse = false; orchestratorError = nil
        form.setOrchestratorMode(mode); form.focus = .orchestratorMode
    }
    func setUnrestricted(_ on: Bool) {
        keyboardInUse = false
        form.setUnrestricted(on); form.focus = .unrestricted
    }
    /// The worktrees arrived or changed under the open sheet: keep what was chosen if it is still there.
    func refreshTargets() {
        guard let fresh = model.newTerminalForm() else { return }
        let chosen = form.target?.id
        form.targets = fresh.targets
        form.select(targetAt: fresh.targets.firstIndex { $0.id == chosen } ?? fresh.targetIndex)
    }

    /// Return, or the Create button. One request at a time; the sheet stays open when it fails. A chat is made by `chat.create`, a
    /// terminal by `shell.create`, an orchestrator is opened by `orchestrator.create`; none is ever sent twice.
    func create() {
        guard !busy else { return }
        let submission: NewTabRequest
        do { submission = try form.submission() } catch {
            if form.kind.isOrchestrator { orchestratorError = .failed(error.localizedDescription) }
            else if form.kind.isChat { chatError = .failed(error.localizedDescription) } else { self.error = .failed(error.localizedDescription) }
            return
        }
        guard !unsupported else {
            if form.kind.isOrchestrator { orchestratorError = .unsupported }
            else if form.kind.isChat { chatError = .unsupported } else { error = .unsupported }
            return
        }
        error = nil; chatError = nil; orchestratorError = nil; submitting = true
        // Unstructured on purpose: the sheet going away must not cancel a request that is already on the wire.
        switch submission {
        case .terminal(let request):
            pending = Task { [weak self] in
                guard let self else { return }
                let failure = await model.createTerminal(request) { _ in self.dismiss() }
                submitting = false
                if let failure { error = failure } else { dismiss() }
            }
        case .chat(let request):
            model.rememberChatChoice(form.chatChoices[request.provider] ?? NewChatChoice(), for: request.provider)
            model.rememberChatProvider(request.provider)
            pending = Task { [weak self] in
                guard let self else { return }
                let failure = await model.createChat(request) { _ in self.dismiss() }
                submitting = false
                if let failure { chatError = failure } else { dismiss() }
            }
        case .orchestrator(let request):
            pending = Task { [weak self] in
                guard let self else { return }
                let failure = await model.createOrchestrator(request) { _ in self.dismiss() }
                submitting = false
                if let failure { orchestratorError = failure } else { dismiss() }
            }
        }
    }
    func cancel() { dismiss() }
}
