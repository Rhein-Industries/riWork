import Foundation
import Observation
import RiWorkCore

/// What is being sent to the Mac now, or what went wrong with the last of it: shown over the terminal or above a chat's composer until
/// it is done or dismissed. One sending at a time per phone.
struct UploadActivity: Equatable {
    enum Phase: Equatable { case preparing, sending, pasting, failed(String) }
    let target: UploadTarget
    var phase: Phase
    var name = ""
    /// The file being sent (1-based) and how many there are.
    var index = 0
    var count = 0
    var sent = 0
    var total = 0
    var fraction: Double { total > 0 ? min(1, Double(sent) / Double(total)) : 0 }
    var failed: Bool { if case .failed = phase { true } else { false } }
}

@MainActor @Observable final class Attachments {
    var activity: UploadActivity?
    @ObservationIgnored var task: Task<Void, Never>?
}

// Files and photos from the phone: they go to the Mac (`FileTransfer`), into the inbox of the shell or chat on screen, and then
// into the shell as a drop of them on its terminal would type them, or into the chat's message as their paths. See "File upload
// extension" in docs/remote-protocol.md.
extension RemoteModel {
    /// The activity of `target`, if it is the one under way (or the one that failed).
    func uploadActivity(for target: UploadTarget) -> UploadActivity? {
        attachments.activity.flatMap { $0.target == target ? $0 : nil }
    }

    /// Sends what was picked or pasted to `target`. For a shell the files are then pasted into it; for a chat their paths go into its
    /// draft (`appendToChat`), where the agent reads them as files when the message is sent.
    func attach(_ sources: [AttachmentSource], to target: UploadTarget) {
        guard !sources.isEmpty else { return }
        if attachments.task != nil { attachments.activity?.phase = .failed("Wait for the files under way, or cancel them."); return }
        guard state == .connected else { fail(target, "Connect to the Mac first."); return }
        guard let feature = desktopFeatures.upload else { fail(target, UploadError.desktopTooOld.localizedDescription); return }
        guard sources.count <= feature.maximumFiles else { fail(target, UploadError.tooMany(limit: feature.maximumFiles).localizedDescription); return }
        attachments.activity = UploadActivity(target: target, phase: .preparing, count: sources.count)
        let client = client
        let desktopID = selectedDesktopID
        let attachments = attachments
        attachments.task = Task { [weak self] in
            do {
                var uploaded: [UploadedFile] = []
                for (offset, source) in sources.enumerated() {
                    let file = try await source.load()
                    try Task.checkCancellation()
                    self?.update { $0.phase = .sending; $0.name = file.name; $0.index = offset + 1; $0.sent = 0; $0.total = file.data.count }
                    let sent = try await FileTransfer.send(file, to: target, feature: feature, over: client) { received in
                        Task { @MainActor in if attachments.activity?.index == offset + 1, attachments.task != nil { attachments.activity?.sent = received } }
                    }
                    uploaded.append(sent)
                }
                // Another Mac chosen meanwhile: what was sent stays in that Mac's inbox until it is swept.
                guard let self, self.selectedDesktopID == desktopID else { return }
                switch target {
                case .shell(let shell):
                    self.update { $0.phase = .pasting }
                    try await FileTransfer.paste(uploaded.map(\.upload), into: shell, over: client)
                    self.jumpToLatest()
                case .chat(let chat):
                    self.appendToChat(chat, paths: uploaded.map(\.path))
                }
                self.attachments.activity = nil
            } catch is CancellationError {
                self?.attachments.activity = nil
            } catch {
                self?.update { $0.phase = .failed(error.localizedDescription) }
            }
            self?.attachments.task = nil
        }
    }

    func cancelUpload() {
        attachments.task?.cancel()
        attachments.task = nil
        attachments.activity = nil
    }
    func dismissUploadFailure() { if attachments.activity?.failed == true { attachments.activity = nil } }

    /// The paths go at the end of the draft, each on its own line, so the message names the files the agent should read.
    func appendToChat(_ chat: String, paths: [String]) {
        guard let conversation = chatConversations[chat], !paths.isEmpty else { return }
        var draft = conversation.draft
        if !draft.isEmpty, !draft.hasSuffix("\n"), !draft.hasSuffix(" ") { draft += "\n" }
        conversation.draft = draft + paths.joined(separator: "\n") + "\n"
    }

    private func update(_ change: (inout UploadActivity) -> Void) {
        guard var activity = attachments.activity else { return }
        change(&activity)
        attachments.activity = activity
    }
    private func fail(_ target: UploadTarget, _ message: String) {
        attachments.activity = UploadActivity(target: target, phase: .failed(message))
    }
}
