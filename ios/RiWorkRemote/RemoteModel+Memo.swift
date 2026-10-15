import Foundation
import RiWorkCore

/// `/memo`'s preview over a chat: what it shows and the chat it is for.
struct MemoPreviewState: Identifiable, Equatable {
    let id = UUID()
    let chatID: String
    var preview: ProjectMemoPreview
}

/// A project's memo on the phone (`ProjectMemo.swift`): the editor's reading and saving, text kept on the phone until the Mac has it,
/// and `/memo` in a chat. A save replaces the whole memo; the last one to reach the Mac wins. A save, once on its way, is never
/// cancelled (the frame goes out regardless, so cancelling would only lose its answer): only the wait for typing to pause is.
@MainActor extension RemoteModel {
    var memoDrafts: ProjectMemoDraftStore { ProjectMemoDraftStore(defaults: defaults) }
    var memoSupported: Bool { desktopFeatures.projectMemo }

    // MARK: The editor

    /// Puts the memo editor of the project up and reads its memo. Text from earlier that never reached the Mac comes back with it.
    func openMemoEditor(projectID: String, name: String) {
        memoDebounce?.cancel(); memoDebounce = nil
        let kept = desktop.flatMap { memoDrafts.entry(desktop: $0.id, project: projectID) }
        memoEditor = ProjectMemoEditor(projectID: projectID, projectName: name, restoredText: kept?.text, restoredBase: kept?.base)
        Task { await loadMemo() }
    }

    /// Takes the editor down. What the Mac does not have yet stays kept on the phone and is saved now if it can be; a save on its
    /// way finishes by itself.
    func closeMemoEditor() {
        memoDebounce?.cancel(); memoDebounce = nil
        guard let editor = memoEditor else { return }
        keepMemoText(editor)
        memoEditor = nil
        guard editor.pendingSave != nil, state == .connected, memoSupported, let desktop else { return }
        let desktopID = desktop.id
        Task { await replayKept(projectID: editor.projectID, desktopID: desktopID) }
    }

    /// Reads the memo of the editor that is up. A read that cannot happen says why, and text kept from earlier stays queued.
    func loadMemo() async {
        guard let session = memoEditor?.session, let projectID = memoEditor?.projectID else { return }
        let failure: ProjectMemoError? = state != .connected ? .notConnected : memoSupported ? nil : .unsupported
        if let failure { memoEditor?.loadFailed(failure); return }
        do {
            let memo = try await client.readProjectMemo(projectID: projectID)
            guard memoEditor?.session == session else { return }
            memoEditor?.loaded(memo)
        } catch {
            guard memoEditor?.session == session else { return }
            memoEditor?.loadFailed(ProjectMemoError.from(error))
        }
        if let editor = memoEditor { keepMemoText(editor) }
        if memoEditor?.pendingSave != nil { saveMemoSoon(after: .zero) }
    }

    /// What the person typed. Kept on the phone at once; saved once typing pauses.
    func editMemo(_ text: String) {
        guard var editor = memoEditor, editor.text != text else { return }
        editor.edit(text)
        memoEditor = editor
        keepMemoText(editor)
        if editor.pendingSave != nil { saveMemoSoon(after: memoSaveDelay) } else { memoDebounce?.cancel(); memoDebounce = nil }
    }

    /// Retry after a refusal, or a read that failed or never happened.
    func retryMemo() async {
        if memoEditor?.saved == nil { await loadMemo() } else { await saveMemoNow() }
    }

    /// The Mac's memo changed since the kept text was written: keep it (it is saved over the Mac's) or take the Mac's.
    func resolveMemoConflict(keepMine: Bool) {
        guard var editor = memoEditor else { return }
        if keepMine { editor.keepMine() } else { editor.useMacVersion() }
        memoEditor = editor
        keepMemoText(editor)
        if editor.pendingSave != nil { saveMemoSoon(after: .zero) }
    }

    /// The wait before a save. Cancelling it never cancels a save: the save runs in a task of its own.
    func saveMemoSoon(after delay: Duration) {
        memoDebounce?.cancel()
        memoDebounce = Task { [weak self] in
            if delay > .zero { try? await Task.sleep(for: delay) }
            guard !Task.isCancelled, let self else { return }
            self.memoDebounce = nil
            Task { await self.saveMemoNow() }
        }
    }

    /// Sends what the editor holds, if a save is due. Nothing is called saved before the Mac answers with the memo it keeps.
    func saveMemoNow() async {
        guard let editor = memoEditor, let text = editor.pendingSave else { return }
        let (session, projectID) = (editor.session, editor.projectID)
        guard state == .connected else { memoEditor?.saveFailed(.notConnected); return }
        guard memoSupported else { memoEditor?.saveFailed(.unsupported); return }
        memoEditor?.beginSave()
        let result: Result<ProjectMemo, ProjectMemoError>
        do { result = .success(try await client.saveProjectMemo(projectID: projectID, text: text)) }
        catch { result = .failure(ProjectMemoError.from(error)) }
        guard memoEditor?.session == session else {
            if case .success(let memo) = result { savedAfterClose(memo) }
            return
        }
        switch result {
        case .success(let memo): memoEditor?.saveSucceeded(memo)
        case .failure(let error): memoEditor?.saveFailed(error)
        }
        guard let editor = memoEditor else { return }
        keepMemoText(editor)
        // Typed while it was on its way: that text is due like any edit.
        if editor.phase == .unsaved, editor.pendingSave != nil { saveMemoSoon(after: memoSaveDelay) }
    }

    /// A save that answered after its editor went. Kept text it covers goes; newer kept text now builds on this save and is
    /// saved too, in the project's editor when it is up again.
    private func savedAfterClose(_ memo: ProjectMemo) {
        guard let desktop else { return }
        if memoEditor?.projectID == memo.projectID {
            memoEditor?.earlierSaveLanded(memo)
            guard let editor = memoEditor else { return }
            keepMemoText(editor)
            if editor.pendingSave != nil { saveMemoSoon(after: .zero) }
            return
        }
        guard let kept = memoDrafts.entry(desktop: desktop.id, project: memo.projectID) else { return }
        if ProjectMemoText.normalize(kept.text) == memo.text {
            memoDrafts.keep(nil, desktop: desktop.id, project: memo.projectID)
        } else {
            memoDrafts.keep(kept.text, base: memo.updatedAt, desktop: desktop.id, project: memo.projectID)
            let desktopID = desktop.id
            Task { await replayKept(projectID: memo.projectID, desktopID: desktopID) }
        }
    }

    private func keepMemoText(_ editor: ProjectMemoEditor) {
        guard let desktop else { return }
        memoDrafts.keep(editor.textToKeep, base: editor.base, desktop: desktop.id, project: editor.projectID)
    }

    /// Saves text kept for a project whose editor is not up: after a read, and only over the memo it was written on. Text the Mac
    /// has already goes; text over a memo that changed on the Mac since stays kept, and its editor asks when it is opened.
    private func replayKept(projectID: String, desktopID: String) async {
        guard memoEditor?.projectID != projectID, desktop?.id == desktopID,
              let kept = memoDrafts.entry(desktop: desktopID, project: projectID) else { return }
        let text = ProjectMemoText.normalize(kept.text)
        guard ProjectMemoText.tooLargeMessage(text) == nil, let current = try? await client.readProjectMemo(projectID: projectID) else { return }
        let stillKept = { [weak self] in
            self?.memoEditor?.projectID != projectID && self?.desktop?.id == desktopID
                && self?.memoDrafts.entry(desktop: desktopID, project: projectID)?.text == kept.text
        }
        if current.text == text {
            if stillKept() { memoDrafts.keep(nil, desktop: desktopID, project: projectID) }
            return
        }
        guard current.updatedAt == kept.base, stillKept(),
              let memo = try? await client.saveProjectMemo(projectID: projectID, text: text), memo.text == text, stillKept() else { return }
        memoDrafts.keep(nil, desktop: desktopID, project: projectID)
    }

    /// A connection began: the editor that is up reads again (and saves what waits), and text kept for other projects of this Mac is
    /// saved where it can be.
    func memoConnected(token: UUID) {
        guard memoSupported, let desktop else { return }
        let desktopID = desktop.id
        Task { [weak self] in
            guard let self, self.generation == token else { return }
            if self.memoEditor != nil { await self.loadMemo() }
            for project in self.memoDrafts.keptProjects(desktop: desktopID) {
                guard self.generation == token, self.state == .connected else { return }
                await self.replayKept(projectID: project, desktopID: desktopID)
            }
        }
    }

    // MARK: `/memo` in a chat

    /// When the chat's draft ends in `/memo`, takes the command out of it and puts the project's memo up instead of sending. Whether
    /// it did.
    func openMemoPreview(chat: ChatInfo) -> Bool {
        let conversation = conversation(chat.id)
        guard let rest = ProjectMemoText.commandRest(conversation.draft) else { return false }
        conversation.draft = rest
        guard let projectID = ProjectMemoPreview.projectID(of: chat) else {
            memoPreview = MemoPreviewState(chatID: chat.id, preview: .noProject)
            return true
        }
        guard memoSupported else {
            memoPreview = MemoPreviewState(chatID: chat.id, preview: .unsupported)
            return true
        }
        let shown = MemoPreviewState(chatID: chat.id, preview: .loading(projectID: projectID))
        memoPreview = shown
        Task { [weak self] in
            guard let self else { return }
            let preview: ProjectMemoPreview
            do { preview = .memo(try await self.client.readProjectMemo(projectID: projectID)) }
            catch {
                let error = ProjectMemoError.from(error)
                preview = error == .unsupported ? .unsupported : .failed(projectID: projectID, message: error.message)
            }
            guard self.memoPreview?.id == shown.id else { return }
            self.memoPreview?.preview = preview
        }
        return true
    }

    /// **Insert into composer**: the memo after the chat's draft. Nothing is sent.
    func insertMemoPreview() {
        guard let shown = memoPreview, case .memo(let memo) = shown.preview, !memo.isEmpty else { return }
        let conversation = conversation(shown.chatID)
        conversation.draft = ProjectMemoText.appended(to: conversation.draft, memo: memo.text)
        memoPreview = nil
    }

    /// The name of a project of this Mac, for the editor's title.
    func projectName(_ id: String) -> String { projects.first(where: { $0.id == id })?.name ?? "Project" }
}
