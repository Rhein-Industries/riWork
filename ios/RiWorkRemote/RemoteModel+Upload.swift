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

/// A file picked for a chat, shown as a card at once while it is read and sent (`ChatAttachmentStrip`); it becomes a `StagedAttachment`
/// the moment the Mac has it. Not saved: an upload does not outlive the app.
struct PendingAttachment: Identifiable, Equatable {
    let id: String
    let chat: String
    var kind: StagedAttachment.Kind
    var name: String
    /// Known once the file is read.
    var size: Int?
    /// How much of it the Mac has, 0 to 1.
    var fraction: Double = 0
}

@MainActor @Observable final class Attachments {
    var activity: UploadActivity?
    /// The cards of files on their way to a chat, in the order picked.
    var pending: [PendingAttachment] = []
    static func pendingID(_ job: UUID, _ offset: Int) -> String { "\(job.uuidString)-\(offset)" }
    /// Changes the card of file `offset` of `job`, if that job is still the one under way.
    func updatePending(_ job: UUID, _ offset: Int, _ change: (inout PendingAttachment) -> Void) {
        guard self.job == job, let index = pending.firstIndex(where: { $0.id == Self.pendingID(job, offset) }) else { return }
        change(&pending[index])
    }
    @ObservationIgnored var task: Task<Void, Never>?
    @ObservationIgnored var job: UUID?
    @ObservationIgnored var loadSource: @MainActor (AttachmentSource) async throws -> UploadFile = { try await $0.load() }
}

// Files and photos from the phone: they go to the Mac (`FileTransfer`), into the inbox of the shell or chat on screen, and then
// into the shell as a drop of them on its terminal would type them, or into the chat's message as their paths. See "File upload
// extension" in docs/remote-protocol.md.
extension RemoteModel {
    /// The cards of files on their way to `chat`.
    func pendingAttachments(for chat: String) -> [PendingAttachment] { attachments.pending.filter { $0.chat == chat } }

    /// The activity of `target`, if it is the one under way (or the one that failed).
    func uploadActivity(for target: UploadTarget) -> UploadActivity? {
        attachments.activity.flatMap { $0.target == target ? $0 : nil }
    }

    /// Sends what was picked or pasted to `target`. For a shell the files are then pasted into it; for a chat they become cards above its
    /// composer (`stageInChat`), and their paths go into the message when it is sent, where the agent reads them as files.
    func attach(_ sources: [AttachmentSource], to target: UploadTarget) {
        guard !sources.isEmpty else { return }
        if attachments.task != nil { attachments.activity?.phase = .failed("Wait for the files under way, or cancel them."); return }
        guard state == .connected else { fail(target, "Connect to the Mac first."); return }
        guard let feature = desktopFeatures.upload else { fail(target, UploadError.desktopTooOld.localizedDescription); return }
        guard sources.count <= feature.maximumFiles else { fail(target, UploadError.tooMany(limit: feature.maximumFiles).localizedDescription); return }
        guard let desktop else { fail(target, "Connect to the Mac first."); return }
        attachments.activity = UploadActivity(target: target, phase: .preparing, count: sources.count)
        let client = BoundUploadTransport(upstream: client, identity: UploadConnectionIdentity(pairing: desktop.pairing))
        let desktopID = desktop.id
        let job = UUID()
        attachments.job = job
        // A chat shows a card for each file at once, before a byte is read; it fills in as the file is read and sent.
        if case .chat(let chat) = target {
            attachments.pending = sources.enumerated().map { offset, source in
                PendingAttachment(id: Attachments.pendingID(job, offset), chat: chat, kind: source.placeholder.kind, name: source.placeholder.name)
            }
        }
        let attachments = attachments
        attachments.task = Task { [weak self] in
            defer { if attachments.job == job { attachments.task = nil; attachments.job = nil; attachments.pending = [] } }
            do {
                var uploaded: [UploadedFile] = []
                for (offset, source) in sources.enumerated() {
                    let file = try await attachments.loadSource(source)
                    try Task.checkCancellation()
                    guard let self, self.selectedDesktopID == desktopID, attachments.job == job else { throw CancellationError() }
                    self.update { $0.phase = .sending; $0.name = file.name; $0.index = offset + 1; $0.sent = 0; $0.total = file.data.count }
                    attachments.updatePending(job, offset) {
                        $0.name = file.name; $0.kind = StagedAttachment.kind(mediaType: file.mediaType, name: file.name); $0.size = file.data.count
                    }
                    let total = Double(max(1, file.data.count))
                    let sent = try await FileTransfer.send(file, to: target, feature: feature, over: client) { received in
                        Task { @MainActor in
                            if attachments.job == job, attachments.activity?.index == offset + 1 { attachments.activity?.sent = received }
                            attachments.updatePending(job, offset) { $0.fraction = min(1, Double(received) / total) }
                        }
                    }
                    uploaded.append(sent)
                    if case .chat(let chat) = target {
                        let kind = StagedAttachment.kind(mediaType: file.mediaType, name: file.name)
                        // The card's pictures, from the bytes the Mac has, made off the main thread; then the file's card takes the place
                        // of its placeholder, so a file the Mac has is a card even if a later one fails or is cancelled.
                        let images = self.chatAttachmentImages, data = file.data, id = sent.upload
                        let pictured = kind == .image ? await Task.detached(priority: .userInitiated) { images.save(id, data: data) }.value : false
                        guard self.selectedDesktopID == desktopID, attachments.job == job else { throw CancellationError() }
                        self.stageInChat(chat, [StagedAttachment(id: id, kind: pictured ? .image : .file, name: file.name, size: file.data.count, path: sent.path)])
                        attachments.pending.removeAll { $0.id == Attachments.pendingID(job, offset) }
                    }
                }
                // Another Mac chosen meanwhile: what was sent stays in that Mac's inbox until it is swept.
                try Task.checkCancellation()
                guard let self, self.selectedDesktopID == desktopID, attachments.job == job else { throw CancellationError() }
                if case .shell(let shell) = target {
                    self.update { $0.phase = .pasting }
                    try await FileTransfer.paste(uploaded.map(\.upload), into: shell, over: client)
                    try Task.checkCancellation()
                    guard attachments.job == job, self.selectedDesktopID == desktopID else { throw CancellationError() }
                    self.jumpToLatest()
                }
                if attachments.job == job { attachments.activity = nil }
            } catch is CancellationError {
                if attachments.job == job { attachments.activity = nil }
            } catch {
                if attachments.job == job { self?.update { $0.phase = .failed(error.localizedDescription) } }
            }
        }
    }

    func cancelUpload() {
        attachments.job = nil
        attachments.pending = []
        attachments.task?.cancel()
        attachments.task = nil
        attachments.activity = nil
    }
    func dismissUploadFailure() { if attachments.activity?.failed == true { attachments.activity = nil } }

    /// The files sent become cards after those staged already; their paths go into the message when it is sent. A chat whose conversation
    /// was let go meanwhile gets them in its saved draft.
    func stageInChat(_ chat: String, _ cards: [StagedAttachment]) {
        guard !cards.isEmpty else { return }
        if let conversation = chatConversations[chat] {
            conversation.attachments = ChatDraft.merged(conversation.attachments, cards)
        } else {
            chatDrafts.setAttachments(ChatDraft.merged(chatDrafts.draft(chat)?.attachments ?? [], cards), for: chat)
        }
    }
    /// The card leaves the composer. The desktop has no way to delete one finished upload (`upload.cancel` leaves a complete one), so
    /// the file stays in the chat's inbox until the desktop sweeps it; only the phone forgets it.
    func removeStagedAttachment(_ id: String, from chat: String) {
        let conversation = conversation(chat)
        conversation.attachments.removeAll { $0.id == id }
        if !chatDrafts.attachmentIDs.contains(id) { chatAttachmentImages.remove(id) }
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

/// Every upload step, including retry and detached cancellation, is bound at the transport to its authenticated destination.
private struct BoundUploadTransport: RemoteTransport {
    let upstream: any RemoteTransport
    let identity: UploadConnectionIdentity
    func connect(pairing: Pairing, allowLocalDevelopment: Bool) async throws -> Pairing { throw RemoteError.disconnected }
    func disconnect() async {}
    func isConnected() async -> Bool { await upstream.isConnected() }
    func request(method: String, params: [String: JSONValue], id: String) async throws -> JSONValue {
        try await upstream.request(method: method, params: params, id: id, boundTo: identity)
    }
}
