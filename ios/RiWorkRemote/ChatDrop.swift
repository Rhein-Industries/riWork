import SwiftUI
import UIKit
import UniformTypeIdentifiers
import Observation
import RiWorkCore

/// Provider requests must begin before UIKit releases the drop session. The task only awaits an already-started request.
final class StartedDropLoad<Value: Sendable>: @unchecked Sendable {
    let task: Task<Value, Error>
    private let progress: DropProgress

    init(start: (DropProgress, @escaping @Sendable (Result<Value, Error>) -> Void) -> Void) {
        let progress = DropProgress()
        self.progress = progress
        let (stream, continuation) = AsyncThrowingStream<Value, Error>.makeStream()
        start(progress) { result in
            switch result {
            case .success(let value): continuation.yield(value); continuation.finish()
            case .failure(let error): continuation.finish(throwing: error)
            }
        }
        task = Task {
            try await withTaskCancellationHandler {
                for try await value in stream { try Task.checkCancellation(); return value }
                throw CancellationError()
            } onCancel: { progress.cancel(); continuation.finish(throwing: CancellationError()) }
        }
    }

    func value() async throws -> Value {
        try await withTaskCancellationHandler { try await task.value } onCancel: { self.cancel() }
    }
    func cancel() { progress.cancel(); task.cancel() }
    deinit { progress.cancel(); task.cancel() }
}

/// Cancellation can race a callback (including a synchronous callback that starts a fallback request).
final class DropProgress: @unchecked Sendable {
    private let lock = NSLock()
    private var progresses: [Progress] = []
    private var cancelled = false
    var isCancelled: Bool { lock.withLock { cancelled } }
    func add(_ progress: Progress) {
        let cancel = lock.withLock { progresses.append(progress); return cancelled }
        if cancel { progress.cancel() }
    }
    func cancel() {
        let values = lock.withLock { cancelled = true; return progresses }
        values.forEach { $0.cancel() }
    }
}

private struct DropProvider: @unchecked Sendable { let value: NSItemProvider }

/// A drop owns its copy independently of the provider's temporary URL and the serial upload job.
final class DroppedAttachment: @unchecked Sendable {
    let file: StartedDropLoad<URL>
    let type: UTType
    let name: String
    let reencode: Bool
    let maximumBytes: Int
    private let directory: URL

    init(provider: NSItemProvider, type: UTType, maximumBytes: Int) {
        self.type = type
        self.maximumBytes = maximumBytes
        reencode = type.conforms(to: .image) && provider.registeredContentTypesForOpenInPlace.isEmpty
        var name = provider.suggestedName ?? "Dropped item"
        if (name as NSString).pathExtension.isEmpty, let ext = type.preferredFilenameExtension { name += "." + ext }
        self.name = name
        let fileName = name
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("chat-drop-" + UUID().uuidString, isDirectory: true)
        self.directory = directory
        let boxed = DropProvider(value: provider)
        file = StartedDropLoad { progress, finish in
            progress.add(provider.loadFileRepresentation(for: type, openInPlace: false) { url, _, error in
                do {
                    if let error { throw error }
                    guard !progress.isCancelled else { throw CancellationError() }
                    if let url {
                        // Inspect before copying, and consume the provider URL before this callback returns.
                        let values = try url.resourceValues(forKeys: [.fileSizeKey, .isDirectoryKey, .isPackageKey])
                        guard values.isDirectory != true, values.isPackage != true else { throw UploadError.refused("Drop files rather than folders or packages.") }
                        guard let size = values.fileSize else { throw UploadError.refused("The dropped file's size could not be read.") }
                        guard size <= maximumBytes else { throw UploadError.tooLarge(name: url.lastPathComponent, limit: maximumBytes) }
                        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                        let copy = directory.appendingPathComponent(url.lastPathComponent)
                        try FileManager.default.copyItem(at: url, to: copy)
                        guard !progress.isCancelled else { throw CancellationError() }
                        finish(.success(copy))
                    } else if type.conforms(to: .image) {
                        // Only in-memory images without a file representation need a data fallback. Errors above stay errors.
                        progress.add(boxed.value.loadDataRepresentation(forTypeIdentifier: type.identifier) { data, error in
                            do {
                                if let error { throw error }
                                guard !progress.isCancelled else { throw CancellationError() }
                                guard let data else { throw UploadError.refused("The dropped image could not be read.") }
                                guard data.count <= maximumBytes else { throw UploadError.tooLarge(name: fileName, limit: maximumBytes) }
                                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                                let copy = directory.appendingPathComponent((fileName as NSString).lastPathComponent)
                                try data.write(to: copy)
                                guard !progress.isCancelled else { throw CancellationError() }
                                finish(.success(copy))
                            } catch { try? FileManager.default.removeItem(at: directory); finish(.failure(error)) }
                        })
                    } else { throw UploadError.refused("The dropped file could not be read.") }
                } catch { try? FileManager.default.removeItem(at: directory); finish(.failure(error)) }
            })
        }
    }

    /// Long text has already been loaded during the drop; it needs no new item-provider request.
    init(text: String, maximumBytes: Int) throws {
        type = .plainText; name = "dropped-text.txt"; reencode = false; self.maximumBytes = maximumBytes
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("chat-drop-" + UUID().uuidString, isDirectory: true)
        self.directory = directory
        let data = Data(text.utf8)
        guard data.count <= maximumBytes else { throw UploadError.tooLarge(name: name, limit: maximumBytes) }
        let copy = directory.appendingPathComponent(name)
        file = StartedDropLoad { _, finish in
            do {
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                try data.write(to: copy)
                finish(.success(copy))
            } catch { try? FileManager.default.removeItem(at: directory); finish(.failure(error)) }
        }
    }
    func removeCopy() { try? FileManager.default.removeItem(at: directory) }
    deinit { file.cancel(); try? FileManager.default.removeItem(at: directory) }
}

/// File-backed text remains a file. Only text dragged as text (or a link) belongs in the draft.
enum DroppedItems {
    static func dropType(_ provider: NSItemProvider) -> UTType? {
        let open = provider.registeredContentTypesForOpenInPlace
        let types = provider.registeredTypeIdentifiers.compactMap(UTType.init)
        func container(_ type: UTType) -> Bool { type.conforms(to: .directory) || type.conforms(to: .package) }
        guard !open.contains(where: container), !types.contains(where: container) else { return nil }
        return (open + types).first(where: { $0.conforms(to: .image) })
            ?? open.first(where: { !$0.conforms(to: .url) })
            ?? PasteboardAttachments.fileType(of: provider)
    }
    static func classify(_ providers: [NSItemProvider]) -> (files: [NSItemProvider], texts: [NSItemProvider]) {
        var files: [NSItemProvider] = [], texts: [NSItemProvider] = []
        for provider in providers {
            if dropType(provider) != nil { files.append(provider) }
            else if !provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier),
                    !provider.hasItemConformingToTypeIdentifier(UTType.directory.identifier),
                    !provider.hasItemConformingToTypeIdentifier(UTType.package.identifier),
                    provider.canLoadObject(ofClass: NSString.self) || provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
                texts.append(provider)
            }
        }
        return (files, texts)
    }

    static func text(_ provider: NSItemProvider) -> StartedDropLoad<String> {
        // Start both representations during perform, so fallback never requests data after the session has ended.
        let string: StartedDropLoad<String>? = provider.canLoadObject(ofClass: NSString.self) ? StartedDropLoad { progress, finish in
            progress.add(provider.loadObject(ofClass: NSString.self) { object, error in
                if let text = object as? String { finish(.success(text)) }
                else { finish(.failure(error ?? UploadError.refused("The dropped text could not be read."))) }
            })
        } : nil
        let link: StartedDropLoad<String>? = provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) ? StartedDropLoad { progress, finish in
            progress.add(provider.loadObject(ofClass: NSURL.self) { object, error in
                if let url = object as? URL {
                    finish(.success(url.absoluteString))
                } else { finish(.failure(error ?? UploadError.refused("The dropped link could not be read."))) }
            })
        } : nil
        return StartedDropLoad { progress, finish in
            let task = Task {
                do {
                    if let link, let value = try? await link.value() {
                        guard URL(string: value)?.isFileURL != true else { throw UploadError.refused("Drop the file itself rather than its file URL.") }
                        finish(.success(value)); return
                    }
                    guard let string else { throw UploadError.refused("The dropped text could not be read.") }
                    finish(.success(try await string.value()))
                } catch { finish(.failure(error)) }
            }
            let cancellation = Progress(totalUnitCount: 1)
            cancellation.cancellationHandler = { task.cancel(); string?.cancel(); link?.cancel() }
            progress.add(cancellation)
        }
    }
}

@MainActor @Observable final class ChatDropState {
    var screenTargeted = false
    var composerTargeted = false
    var providers: [NSItemProvider] = []
    enum Text: Equatable { case loading, loaded(String), failed }
    var text: [ObjectIdentifier: Text] = [:]
    @ObservationIgnored var requests: [ObjectIdentifier: StartedDropLoad<String>] = [:]
    @ObservationIgnored var generation = UUID()
    @ObservationIgnored let transcript = TranscriptDropPassThrough()
    func clear() {
        if !providers.isEmpty { providers = [] }
        if !text.isEmpty { text = [:] }
        requests.values.forEach { $0.cancel() }; requests = [:]; generation = UUID()
    }
}

/// Shared by the screen's SwiftUI delegate and the innermost UIKit interaction in the composer.
@MainActor struct ChatDropHandler {
    let model: RemoteModel
    let chat: String
    let state: ChatDropState
    var composerTargeted: Bool { state.composerTargeted }
    var transcript: TranscriptDropPassThrough { state.transcript }
    private func isTab(_ text: String) -> Bool {
        (text.hasPrefix("shell:") || text.hasPrefix("chat:")) && model.sharedTabs?.allEntries.contains(where: { $0.key == text }) == true
    }
    func target(_ providers: [NSItemProvider], composer: Bool, local: Bool = false) {
        if providers.map(ObjectIdentifier.init) != state.providers.map(ObjectIdentifier.init) {
            state.clear(); state.providers = providers
        }
        if composer { if !state.composerTargeted { state.composerTargeted = true } }
        else if !state.screenTargeted { state.screenTargeted = true }
        // Hovering an external drag must not request its contents. Local text is only for rejecting tab reorders early.
        guard local else { return }
        for provider in DroppedItems.classify(providers).texts {
            let id = ObjectIdentifier(provider)
            guard state.text[id] == nil else { continue }
            state.text[id] = .loading
            let generation = state.generation
            let request = DroppedItems.text(provider)
            state.requests[id] = request
            Task {
                let result = try? await request.value()
                guard state.generation == generation else { return }
                let next: ChatDropState.Text = result.map { .loaded($0) } ?? .failed
                if state.text[id] != next { state.text[id] = next }
            }
        }
    }
    func exit(composer: Bool) {
        if composer { if state.composerTargeted { state.composerTargeted = false } }
        else if state.screenTargeted { state.screenTargeted = false }
        state.clear()
    }
    private func fileRefusal(_ count: Int) -> String? {
        guard count > 0 else { return nil }
        if model.state != .connected { return "Connect to the Mac first" }
        guard let feature = model.desktopFeatures.upload else { return UploadError.desktopTooOld.localizedDescription }
        if model.uploadInProgress { return "Wait for the files under way" }
        if count > feature.maximumFiles { return UploadError.tooMany(limit: feature.maximumFiles).localizedDescription }
        return nil
    }
    func refusal(_ providers: [NSItemProvider], local: Bool = false) -> String? {
        let items = DroppedItems.classify(providers)
        if local && !items.files.isEmpty { return "Drop text from this app" }
        if let reason = fileRefusal(fileCount(providers)) { return reason }
        if items.files.isEmpty && items.texts.allSatisfy({
            if case .loaded(let text) = state.text[ObjectIdentifier($0)] { return isTab(text) }
            return false
        }) { return items.texts.isEmpty ? "Drop files, images or text" : "Reorder tabs in the tab strip" }
        return nil
    }
    var label: String {
        if let reason = refusal(state.providers) { return reason }
        let count = fileCount(state.providers)
        return count == 0 ? "Drop to add text" : "Drop to attach \(count) \(count == 1 ? "item" : "items")"
    }
    private func fileCount(_ providers: [NSItemProvider]) -> Int {
        let items = DroppedItems.classify(providers)
        return items.files.count + items.texts.filter {
            guard case .loaded(let text) = state.text[ObjectIdentifier($0)] else { return false }
            return !isTab(text) && text.utf8.count > 32 * 1024
        }.count
    }
    @discardableResult func perform(_ providers: [NSItemProvider], local: Bool = false, insert: (@MainActor (String) -> Void)? = nil) -> Bool {
        let reason = refusal(providers, local: local)
        if state.screenTargeted { state.screenTargeted = false }
        if state.composerTargeted { state.composerTargeted = false }
        state.clear()
        guard reason == nil else { return false }
        let items = DroppedItems.classify(providers)
        let maximumBytes = model.desktopFeatures.upload?.maximumBytes ?? 0
        // Start every provider here, not inside the asynchronous upload loop below.
        let files = items.files.compactMap { provider -> DroppedAttachment? in
            guard let type = DroppedItems.dropType(provider) else { return nil }
            return DroppedAttachment(provider: provider, type: type, maximumBytes: maximumBytes)
        }
        let requests = items.texts.map(DroppedItems.text)
        let desktop = model.selectedDesktopID
        Task {
            do {
                var files = files
                var texts: [String] = []
                for request in requests {
                    let text = try await request.value()
                    guard !isTab(text) else { continue }
                    if text.utf8.count > 32 * 1024 {
                        if let reason = fileRefusal(files.count + 1) { throw UploadError.refused(reason) }
                        files.append(try DroppedAttachment(text: text, maximumBytes: maximumBytes))
                    }
                    else if !text.isEmpty { texts.append(text) }
                }
                guard model.selectedDesktopID == desktop else { return }
                if let reason = fileRefusal(files.count) { throw UploadError.refused(reason) }
                let text = texts.joined(separator: "\n")
                if !text.isEmpty {
                    let conversation = model.conversation(chat)
                    let separator = insert == nil && !conversation.draft.isEmpty ? "\n" : ""
                    guard conversation.draft.utf8.count + separator.utf8.count + text.utf8.count <= ChatDraftStore.maximumBytes else {
                        throw UploadError.refused("The draft can hold at most 256 KB of text.")
                    }
                    if let insert { insert(text) } else { conversation.draft += separator + text }
                }
                if !files.isEmpty { model.attach(files.map(AttachmentSource.dropped), to: .chat(chat)) }
            } catch {
                guard model.selectedDesktopID == desktop, !model.uploadInProgress else { return }
                model.attachments.activity = UploadActivity(target: .chat(chat), phase: .failed(error.localizedDescription))
            }
        }
        return true
    }
}

private struct ChatDropHandlerKey: EnvironmentKey {
    static let defaultValue: ChatDropHandler? = nil
}

extension EnvironmentValues {
    var chatDropHandler: ChatDropHandler? {
        get { self[ChatDropHandlerKey.self] }
        set { self[ChatDropHandlerKey.self] = newValue }
    }
}

private struct ChatDropDelegate: DropDelegate {
    let handler: ChatDropHandler
    private func providers(_ info: DropInfo) -> [NSItemProvider] { info.itemProviders(for: [.item]) }
    func validateDrop(info: DropInfo) -> Bool { !providers(info).isEmpty }
    func dropEntered(info: DropInfo) { handler.target(providers(info), composer: false) }
    func dropExited(info: DropInfo) { handler.exit(composer: false) }
    func dropUpdated(info: DropInfo) -> DropProposal? {
        let providers = providers(info)
        handler.target(providers, composer: false)
        return DropProposal(operation: handler.refusal(providers) == nil ? .copy : .forbidden)
    }
    func performDrop(info: DropInfo) -> Bool { handler.perform(providers(info)) }
}

private struct ChatDropTarget: ViewModifier {
    @Environment(\.desktopStyle) private var style
    let model: RemoteModel
    let chat: ChatInfo
    @State private var state = ChatDropState()

    func body(content: Content) -> some View {
        let handler = ChatDropHandler(model: model, chat: chat.id, state: state)
        content
            .environment(\.chatDropHandler, handler)
            .onDisappear { state.clear(); state.transcript.restore() }
            .contentShape(Rectangle())
            .onDrop(of: [.item], delegate: ChatDropDelegate(handler: handler))
            .overlay {
                if state.screenTargeted || state.composerTargeted {
                    let shape = RoundedRectangle(cornerRadius: style.pt(16))
                    ZStack {
                        shape.fill(style.background.opacity(0.6))
                        shape.strokeBorder(style.accent, style: StrokeStyle(lineWidth: 2, dash: [8, 5]))
                        Text(handler.label).font(style.face(15, bold: true, relativeTo: .body))
                            .foregroundStyle(style.accent).padding(.horizontal, 16).padding(.vertical, 10)
                            .background(style.background, in: Capsule())
                    }
                    .padding(8).frame(maxWidth: 760)
                    .allowsHitTesting(false)
                    .accessibilityElement(children: .ignore).accessibilityLabel(handler.label)
                    .accessibilityIdentifier("chat-drop-target")
                }
            }
    }
}

/// The materialized transcript row supplies its exact collection view. Never search the surrounding screen or tab views.
@MainActor final class TranscriptDropPassThrough {
    private weak var list: UICollectionView?
    private var removed: [UIDropInteraction] = []
    private var monitor: Task<Void, Never>?

    func capture(_ scroll: UIScrollView) {
        guard let list = scroll as? UICollectionView else { return }
        if self.list !== list { restore(); self.list = list }
        removeInteractions()
        guard monitor == nil else { return }
        monitor = Task { [weak self] in
            while !Task.isCancelled {
                // SwiftUI may reinstall the row-reordering interaction during a List update.
                self?.removeInteractions()
                try? await Task.sleep(for: .milliseconds(100))
                guard self != nil else { return }
            }
        }
    }
    private func removeInteractions() {
        guard let list, list.window != nil else { return }
        for interaction in list.interactions.compactMap({ $0 as? UIDropInteraction }) {
            if !removed.contains(where: { $0 === interaction }) { removed.append(interaction) }
            list.removeInteraction(interaction)
        }
    }
    func restore() {
        monitor?.cancel(); monitor = nil
        if let list {
            for interaction in removed where interaction.view == nil { list.addInteraction(interaction) }
        }
        removed = []; list = nil
    }
    deinit { monitor?.cancel() }
}

extension View {
    func chatDropTarget(model: RemoteModel, chat: ChatInfo) -> some View { modifier(ChatDropTarget(model: model, chat: chat)) }
}

extension ChatComposerTextView: UIDropInteractionDelegate {
    func dropInteraction(_ interaction: UIDropInteraction, canHandle session: any UIDropSession) -> Bool {
        guard dropHandler != nil else { return false }
        let items = DroppedItems.classify(session.items.map(\.itemProvider))
        return session.localDragSession == nil ? !items.files.isEmpty || !items.texts.isEmpty : items.files.isEmpty && !items.texts.isEmpty
    }

    func dropInteraction(_ interaction: UIDropInteraction, sessionDidEnter session: any UIDropSession) {
        dropHandler?.target(session.items.map(\.itemProvider), composer: true, local: session.localDragSession != nil)
    }

    func dropInteraction(_ interaction: UIDropInteraction, sessionDidUpdate session: any UIDropSession) -> UIDropProposal {
        guard let dropHandler else { return UIDropProposal(operation: .forbidden) }
        let providers = session.items.map(\.itemProvider)
        dropHandler.target(providers, composer: true, local: session.localDragSession != nil)
        return UIDropProposal(operation: dropHandler.refusal(providers, local: session.localDragSession != nil) == nil ? .copy : .forbidden)
    }

    func dropInteraction(_ interaction: UIDropInteraction, sessionDidExit session: any UIDropSession) { dropHandler?.exit(composer: true) }
    func dropInteraction(_ interaction: UIDropInteraction, sessionDidEnd session: any UIDropSession) { dropHandler?.exit(composer: true) }

    func dropInteraction(_ interaction: UIDropInteraction, performDrop session: any UIDropSession) {
        guard let handler = dropHandler else { return }
        let position = closestPosition(to: session.location(in: self)) ?? endOfDocument
        let offset = self.offset(from: beginningOfDocument, to: position)
        _ = onDropItems?(session.items.map(\.itemProvider), session.localDragSession != nil, { [weak self] text in
            guard let self, self.dropHandler?.chat == handler.chat,
                  let caret = self.position(from: self.beginningOfDocument, offset: min(offset, (self.text as NSString).length)) else {
                // A slow provider may finish after the composer has switched chats or left the screen.
                let conversation = handler.model.conversation(handler.chat)
                conversation.draft += (conversation.draft.isEmpty ? "" : "\n") + text
                return
            }
            self.selectedTextRange = self.textRange(from: caret, to: caret)
            self.insertText(text)
        })
    }
}
