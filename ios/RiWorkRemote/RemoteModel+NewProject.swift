import Foundation
import Observation
import RiWorkCore

/// Whether the desktop understands `project.create`, learned from the first call.
enum ProjectCreationSupport: Equatable { case unknown, supported, unsupported }

// Creating a project on the desktop. The request is never retried by the app: a lost answer leaves the outcome unknown, and the
// person is told to look at the project list (asking again would only answer "already exists", if it was created).
extension RemoteModel {
    /// The ＋ and ⌘⇧N are offered: a desktop that has not been found too old. (They are dimmed while there is no connection.)
    var offersNewProject: Bool { projectCreation != .unsupported }
    /// The sheet can open and send: connected, and the desktop not known to be too old.
    var canOpenNewProject: Bool { state == .connected && projectCreation != .unsupported }

    /// Creates a project and, on success, puts it in the project list. Returns nil on success, otherwise why not. `onCreated` runs as
    /// soon as the desktop has answered, before the list is read again, so a sheet can go away and the screen move on at once.
    @discardableResult
    func createProject(_ request: NewProjectRequest, onCreated: @MainActor (RemoteProject) -> Void = { _ in }) async -> ProjectCreateError? {
        guard !creatingProject else { return .busy }
        guard state == .connected else { return .notConnected }
        guard projectCreation != .unsupported else { return .unsupported }
        creatingProject = true
        defer { creatingProject = false }
        let token = generation
        let reply: NewProjectReply
        do {
            reply = try await client.createProject(request)
        } catch {
            let failure = ProjectCreateError.from(error)
            if generation == token, failure == .unsupported { projectCreation = .unsupported }
            // Something was made, or may have been: the list shows it if so.
            if generation == token, failure == .unreadableReply { await refreshProjectsQuietly() }
            return failure
        }
        if generation == token { projectCreation = .supported }
        // A reconnect or another desktop in the meantime: the project exists, but this screen is no longer its.
        guard generation == token, state == .connected else { return nil }
        let project = reply.project
        if !projects.contains(where: { $0.id == project.id }) { projects.append(project) }
        // Just made: counts as just active until the desktop has a figure for it, so it is not sorted behind the projects with activity.
        touchedProjects[project.id] = project.created_at
        onCreated(project)
        await refreshProjectsQuietly()
        return nil
    }

    /// Reads the project list again, without the side effects of a full refresh.
    func refreshProjectsQuietly() async {
        guard state == .connected else { return }
        let token = generation
        guard let listed = try? await rpc("projects.list")["projects"].decode([RemoteProject].self) else { return }
        guard generation == token, state == .connected else { return }
        if listed != projects { projects = listed }
        lastListRead[.projects] = .now
    }
}

/// The "New project" sheet's state: the form, what went wrong, and whether a request is on its way. Touch and keyboard both go
/// through here, so they cannot disagree.
@MainActor @Observable final class NewProjectSheetModel: Identifiable {
    let id = UUID()
    var form = NewProjectForm()
    /// What the desktop (or the link) said to the last attempt. Cleared by the next edit.
    var error: ProjectCreateError?
    /// The focus ring shows only once a key was pressed; a finger does not need it.
    var keyboardInUse = false
    /// Return or Create was tried with nothing usable typed: even an empty field is told what it needs.
    private(set) var attempted = false
    @ObservationIgnored let model: RemoteModel
    /// The sheet should go away (a project was created, or Cancel / Esc).
    @ObservationIgnored var dismiss: () -> Void = {}
    /// The desktop made the project: select it and offer a terminal in it. Runs before `dismiss`.
    @ObservationIgnored var onCreated: @MainActor (RemoteProject) -> Void = { _ in }
    /// Between Return / Create and the answer, including the moment before the request itself starts, so a double tap or a held
    /// Return can only ever send one.
    private(set) var submitting = false
    @ObservationIgnored private(set) var pending: Task<Void, Never>?

    init(model: RemoteModel) { self.model = model }

    var busy: Bool { submitting || model.creatingProject }
    var unsupported: Bool { model.projectCreation == .unsupported }
    var canCreate: Bool { !busy && !unsupported && model.state == .connected && form.isReady }
    /// The message shown inline: what the last attempt said, or that the desktop is too old.
    var message: ProjectCreateError? { unsupported ? .unsupported : error }
    /// What is wrong with the name, to show under the field: nothing while it is still empty, until Return asked for it.
    var problem: NewProjectValidationError? {
        let check = form.check
        if let problem = check.problem { return problem }
        return attempted && check == .empty ? .empty : nil
    }

    /// The text field changed.
    func setName(_ text: String) {
        guard text != form.name else { return }
        error = nil; attempted = false
        form.name = text
    }
    /// A key from a hardware keyboard.
    func press(_ key: NewProjectForm.Key) {
        keyboardInUse = true
        if key != .space { error = nil }
        form.handle(key)
        if key == .space, form.focus == .create { create() }
    }
    /// ⌘G, from wherever the keyboard is (also while typing the name).
    func toggleGitFromKeyboard() {
        keyboardInUse = true; error = nil
        form.setGit(!form.git)
    }
    /// A tap on the switch.
    func setGit(_ on: Bool) {
        keyboardInUse = false; error = nil
        form.setGit(on); form.focus = .git
    }
    /// The name field was tapped or took the keyboard.
    func nameFieldFocused() {
        if form.focus != .name { form.focus = .name }
    }

    /// Return, or the Create button. One request at a time; the sheet stays open when it fails.
    func create() {
        guard !busy else { return }
        guard !unsupported else { error = .unsupported; return }
        let request: NewProjectRequest
        do { request = try form.request() } catch {
            // Nothing usable typed: say what is needed and go back to the field.
            attempted = true; form.focus = .name
            return
        }
        error = nil; submitting = true
        // Unstructured on purpose: the sheet going away must not cancel a request that is already on the wire.
        pending = Task { [weak self] in
            guard let self else { return }
            let failure = await model.createProject(request) { project in
                self.onCreated(project)
                self.dismiss()
            }
            submitting = false
            if let failure { error = failure } else { dismiss() }
        }
    }
    func cancel() { dismiss() }
}
