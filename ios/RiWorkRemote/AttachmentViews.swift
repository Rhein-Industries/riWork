import SwiftUI
import UIKit
import PhotosUI
import UniformTypeIdentifiers
import RiWorkCore

// Giving the Mac a photo or a file from the phone: where it comes from (the Photos library, Files, the camera, the pasteboard), the
// menu that offers those, and the line that shows it on its way. `RemoteModel.attach` sends it; see RemoteModel+Upload.swift.

/// Something picked or pasted, loaded only when it is sent.
enum AttachmentSource: @unchecked Sendable {
    case photo(PhotosPickerItem)
    /// A file from Files: security-scoped, read when it is sent.
    case file(URL)
    /// A photo just taken.
    case camera(Data)
    /// What a paste found on the pasteboard.
    case pasted(NSItemProvider)

    /// What its card shows before the file is read: a photo is a picture; a file from Files or the pasteboard has its name.
    var placeholder: (kind: StagedAttachment.Kind, name: String) {
        switch self {
        case .photo, .camera: (.image, "Photo")
        case .file(let url): (StagedAttachment.kind(mediaType: nil, name: url.lastPathComponent), url.lastPathComponent)
        case .pasted(let provider):
            (PasteboardAttachments.fileType(of: provider)?.conforms(to: .image) == true ? .image : .file, provider.suggestedName ?? "Pasted item")
        }
    }

    /// The file as it should travel (`PhotoPreparation`): a photo upright, as JPEG and without its location unless it is a PNG or a GIF;
    /// a file from Files as it is, unless it is a picture no agent reads (HEIC).
    func load() async throws -> UploadFile {
        switch self {
        case .photo(let item):
            guard let data = try await item.loadTransferable(type: Data.self) else { throw UploadError.refused("The photo could not be read.") }
            let type = item.supportedContentTypes.first(where: { $0.conforms(to: .image) }) ?? .jpeg
            return PhotoPreparation.prepare(data, name: "photo." + (type.preferredFilenameExtension ?? "jpg"), typeIdentifier: type.identifier, reencode: true)
        case .file(let url):
            let data: Data = try await Task.detached {
                let scoped = url.startAccessingSecurityScopedResource()
                defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                return try Data(contentsOf: url, options: .mappedIfSafe)
            }.value
            return PhotoPreparation.prepare(data, name: url.lastPathComponent, typeIdentifier: nil, reencode: false)
        case .camera(let data):
            return PhotoPreparation.prepare(data, name: "photo.jpg", typeIdentifier: UTType.jpeg.identifier, reencode: true)
        case .pasted(let provider):
            guard let type = PasteboardAttachments.fileType(of: provider) else { throw UploadError.refused("The pasteboard holds nothing to send.") }
            let data: Data = try await withCheckedThrowingContinuation { continuation in
                _ = provider.loadDataRepresentation(forTypeIdentifier: type.identifier) { data, error in
                    if let data { continuation.resume(returning: data) } else { continuation.resume(throwing: error ?? UploadError.refused("The pasted item could not be read.")) }
                }
            }
            let fallback = (type.conforms(to: .image) ? "pasted-image" : "pasted") + (type.preferredFilenameExtension.map { "." + $0 } ?? "")
            var name = provider.suggestedName ?? fallback
            if (name as NSString).pathExtension.isEmpty, let ext = type.preferredFilenameExtension { name += "." + ext }
            return PhotoPreparation.prepare(data, name: name, typeIdentifier: type.identifier, reencode: type.conforms(to: .image))
        }
    }
}

/// What a paste in the terminal or a chat finds on the pasteboard, by the Mac's rule (`src/terminal_drop.rs`): files first, then text,
/// then a picture alone. Text stays an ordinary paste.
enum PasteboardAttachments {
    /// Whether a paste should send files rather than type text. Looks at the types only, which never shows the paste prompt.
    static var available: Bool {
        let board = UIPasteboard.general
        let types = (board.types(forItemSet: nil) ?? []).map { $0.compactMap { UTType($0) } }
        if types.contains(where: { $0.contains(where: isFile) }) { return true }
        if board.hasStrings || board.hasURLs { return false }
        return board.hasImages
    }
    /// The items to send, in order.
    static func sources() -> [AttachmentSource] {
        UIPasteboard.general.itemProviders.filter { fileType(of: $0) != nil }.map(AttachmentSource.pasted)
    }
    /// The type an item is sent as: its first that is data and not text or a link.
    static func fileType(of provider: NSItemProvider) -> UTType? {
        provider.registeredTypeIdentifiers.compactMap(UTType.init).first { isFile($0) || $0.conforms(to: .image) }
    }
    /// A file's type: data, and not text, a link or a picture (a picture with text beside it is the text's).
    private static func isFile(_ type: UTType) -> Bool {
        type.conforms(to: .data) && !type.conforms(to: .text) && !type.conforms(to: .url) && !type.conforms(to: .image)
            && !type.conforms(to: .rtf) && !type.conforms(to: .html) && type.identifier != "com.apple.flat-rtfd"
    }
}

/// Where a photo or a file comes from. The paperclip offers these as a menu on itself (`AttachMenu`, and the paperclip key of
/// `KeyBarView`); the choice opens its picker (`attachmentPicker`).
enum AttachmentChoice: Hashable, Identifiable {
    case photos, camera, files
    var id: Self { self }
    /// In sentence case; `DesktopStyle.cased` gives the terminal look its capitals.
    var title: String {
        switch self {
        case .photos: "Photo library"
        case .camera: "Take photo"
        case .files: "Files"
        }
    }
    var symbol: String {
        switch self {
        case .photos: "photo.on.rectangle"
        case .camera: "camera"
        case .files: "folder"
        }
    }
    /// What this phone offers: the camera only where there is one.
    static var available: [AttachmentChoice] {
        UIImagePickerController.isSourceTypeAvailable(.camera) ? [.photos, .camera, .files] : [.photos, .files]
    }
}

/// The picker of `choice` (the Photos library, Files or the camera), shown while `choice` is set; closing it clears `choice`. `onDone`
/// runs once it has gone, picked or not (the terminal and the chat give the keyboard back then).
///
/// The pickers exist only while one of them is wanted, on a background of their own: installed for good, their presenters take the
/// keys and the keyboard from a chat's composer.
struct AttachmentPicker: ViewModifier {
    @Binding var choice: AttachmentChoice?
    var onPick: ([AttachmentSource]) -> Void
    var onDone: () -> Void = {}

    func body(content: Content) -> some View {
        content.background {
            if choice != nil { presenters }
        }
    }
    private func shows(_ wanted: AttachmentChoice) -> Binding<Bool> {
        Binding(get: { choice == wanted }, set: { if !$0, choice == wanted { choice = nil } })
    }
    private var presenters: some View {
        Color.clear
            .photosPicker(isPresented: shows(.photos), selection: picked, maxSelectionCount: UploadLimits.maximumFiles, matching: .images, photoLibrary: .shared())
            .fileImporter(isPresented: shows(.files), allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
                if case .success(let urls) = result, !urls.isEmpty { onPick(urls.map(AttachmentSource.file)) }
            }
            .fullScreenCover(isPresented: shows(.camera)) {
                CameraPicker { data in onPick([.camera(data)]) }.ignoresSafeArea()
            }
            .onDisappear(perform: finishSoon)
    }
    /// The photos chosen go out as the picker hands them over: the picker closes in the same moment, and with it these presenters.
    private var picked: Binding<[PhotosPickerItem]> {
        Binding(get: { [] }, set: { items in if !items.isEmpty { onPick(items.map(AttachmentSource.photo)) } })
    }
    /// Done once the picker has finished going away, unless another was asked for meanwhile.
    private func finishSoon() {
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(400))
            if choice == nil { onDone() }
        }
    }
}

extension View {
    func attachmentPicker(_ choice: Binding<AttachmentChoice?>, onDone: @escaping () -> Void = {}, onPick: @escaping ([AttachmentSource]) -> Void) -> some View {
        modifier(AttachmentPicker(choice: choice, onPick: onPick, onDone: onDone))
    }
}

/// The system camera, for one photo.
struct CameraPicker: UIViewControllerRepresentable {
    var onPhoto: (Data) -> Void
    @Environment(\.dismiss) private var dismiss
    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.delegate = context.coordinator
        return picker
    }
    func updateUIViewController(_ controller: UIImagePickerController, context: Context) {}
    func makeCoordinator() -> Coordinator { Coordinator(self) }
    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let parent: CameraPicker
        init(_ parent: CameraPicker) { self.parent = parent }
        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            if let image = info[.originalImage] as? UIImage, let data = image.jpegData(compressionQuality: 0.9) { parent.onPhoto(data) }
            parent.dismiss()
        }
        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) { parent.dismiss() }
    }
}

/// A file on its way to the Mac, or why it did not get there: one line with the name, how far it is and Cancel, or the reason and
/// Dismiss. Floats over the terminal (it never changes the terminal's size) and sits above a chat's composer.
struct UploadStatusBar: View {
    @Environment(\.desktopStyle) private var style
    let activity: UploadActivity
    let cancel: () -> Void
    let dismiss: () -> Void
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: activity.failed ? "exclamationmark.triangle" : "arrow.up.doc")
                .foregroundStyle(activity.failed ? style.warning : style.accent).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(style.face(11, relativeTo: .caption)).foregroundStyle(activity.failed ? style.warning : style.text)
                    .lineLimit(activity.failed ? 3 : 1).truncationMode(.middle).fixedSize(horizontal: false, vertical: true)
                if case .sending = activity.phase { ProgressView(value: activity.fraction).tint(style.accent) }
                else if !activity.failed { ProgressView().progressViewStyle(.linear).tint(style.accent) }
            }
            Spacer(minLength: 4)
            if activity.failed {
                Button("Dismiss", systemImage: "xmark", action: dismiss).labelStyle(.iconOnly).buttonStyle(DesktopButtonStyle(compact: true))
            } else {
                Button("Cancel", action: cancel).buttonStyle(DesktopButtonStyle(compact: true)).disabled(activity.phase == .pasting)
            }
        }
        .padding(.horizontal, 10).padding(.vertical, 6)
        .modifier(StatusSurface(style: style))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("upload-status")
    }
    private var title: String {
        switch activity.phase {
        case .preparing: activity.count > 1 ? "Preparing \(activity.count) files…" : "Preparing…"
        case .sending:
            (activity.count > 1 ? "\(activity.index) of \(activity.count) · " : "") + "Sending \(activity.name) · \(Int((activity.fraction * 100).rounded()))%"
        case .pasting: activity.count > 1 ? "Pasting \(activity.count) files…" : "Pasting…"
        case .failed(let message): message
        }
    }
    private struct StatusSurface: ViewModifier {
        let style: DesktopStyle
        func body(content: Content) -> some View {
            if style.native {
                content.background(style.glass ? Color.clear : style.panel, in: RoundedRectangle(cornerRadius: 12))
                    .nativeGlass(style, in: RoundedRectangle(cornerRadius: 12)).padding(.horizontal, 8).padding(.top, 6)
            } else {
                content.background(style.panel).overlay(alignment: .bottom) { DesktopRule() }
            }
        }
    }
}

/// The paperclip: a menu on itself of where the photo or file comes from, opening from the paperclip as the system's pull-down menus
/// do, so it stays beside what was tapped and clear of the screen's header.
struct AttachMenu<Label: View>: View {
    @Environment(\.desktopStyle) private var style
    let choose: (AttachmentChoice) -> Void
    @ViewBuilder var label: Label
    var body: some View {
        Menu {
            ForEach(AttachmentChoice.available) { choice in
                Button(style.cased(choice.title), systemImage: choice.symbol) { choose(choice) }
            }
        } label: { label }
            // Top to bottom as listed, whichever way the menu opens.
            .menuOrder(.fixed)
            .accessibilityLabel("Send a photo or file")
    }
}

/// The paperclip of the terminal's bars, in their button style. Equatable on its look alone, for `.equatable()`: a menu whose view is
/// redrawn while it is open (the bars are redrawn as the terminal changes) stops taking taps on its items.
struct AttachButton: View, Equatable {
    var compact = true
    let choose: (AttachmentChoice) -> Void
    nonisolated static func == (lhs: Self, rhs: Self) -> Bool { lhs.compact == rhs.compact }
    var body: some View {
        AttachMenu(choose: choose) { Image(systemName: "paperclip") }
            .menuStyle(.button).buttonStyle(DesktopButtonStyle(compact: compact))
            .accessibilityHint("Sends a photo or a file to the Mac and pastes its path")
    }
}

// MARK: - Cards above a chat's composer

/// The files staged for a chat's message, as cards in one row above the composer's field (as Claude and ChatGPT show them): an image as
/// a rounded thumbnail, any other file as a small card with its kind, name and size. A file shows its card the moment it is picked,
/// with its progress, until the Mac has it (`PendingAttachment`). × removes one (or cancels the files on their way); tapping an image
/// shows it full screen. The row scrolls sideways when there are more than fit; the composer shows none of it while nothing is staged.
struct ChatAttachmentStrip: View {
    @Environment(\.desktopStyle) private var style
    let attachments: [StagedAttachment]
    var pending: [PendingAttachment] = []
    let images: ChatAttachmentImages
    let remove: (String) -> Void
    var cancelPending: () -> Void = {}
    /// Where the first card starts: where the composer's text does.
    var leadingInset: CGFloat = 0
    @State private var previewing: StagedAttachment?
    private var count: Int { attachments.count + pending.count }

    var body: some View {
        ScrollView(.horizontal) {
            HStack(spacing: style.pt(10)) {
                ForEach(Array(attachments.enumerated()), id: \.element.id) { index, card in
                    ChatAttachmentCard(card: card, images: images, open: { previewing = card }, remove: { remove(card.id) })
                        .chatLayoutProbe("card-\(index)")
                        // An earlier card's × reaches over the next card's edge: it stays on top there.
                        .zIndex(Double(count - index))
                        .transition(.scale(scale: 0.8).combined(with: .opacity))
                }
                ForEach(Array(pending.enumerated()), id: \.element.id) { index, card in
                    ChatPendingAttachmentCard(card: card, cancel: cancelPending)
                        .zIndex(Double(pending.count - index))
                        .transition(.opacity)
                }
            }
            // Room for the × badges, which stand out over the cards' corners.
            .padding(.top, style.pt(8)).padding(.trailing, style.pt(8)).padding(.bottom, 2)
        }
        .scrollIndicators(.hidden)
        .contentMargins(.leading, leadingInset, for: .scrollContent)
        .scrollClipDisabled()
        .accessibilityElement(children: .contain)
        .accessibilityLabel(count == 1 ? "1 attachment" : "\(count) attachments")
        .accessibilityIdentifier("attachment-strip")
        .background {
            // Installed only while wanted, as the pickers are: a presenter installed for good takes the keyboard from the composer.
            if previewing != nil {
                Color.clear.fullScreenCover(item: $previewing) { card in
                    ChatAttachmentPreview(card: card, url: images.previewURL(card.id)) { previewing = nil }.desktopThemed(style)
                }
            }
        }
    }
}

/// One staged file: an image's thumbnail, or a file's kind, name and size; with its × in the top-trailing corner (a 44-point target).
struct ChatAttachmentCard: View {
    @Environment(\.desktopStyle) private var style
    let card: StagedAttachment
    let images: ChatAttachmentImages
    let open: () -> Void
    let remove: () -> Void

    private var side: CGFloat { style.pt(64) }
    private var shape: RoundedRectangle { RoundedRectangle(cornerRadius: style.native ? style.pt(12) : 4, style: .continuous) }

    var body: some View {
        let expired = card.isExpired()
        content(expired: expired)
            .opacity(expired ? 0.55 : 1)
            .overlay(shape.strokeBorder(expired ? style.warning : style.divider, lineWidth: 1))
            .overlay(alignment: .bottom) { if expired, card.kind == .image { expiredBadge } }
            .overlay(alignment: .topTrailing) { ChatCardCloseButton(label: "Remove \(card.name)", action: remove) }
            .accessibilityIdentifier(expired ? "attachment-card-expired" : "attachment-card")
    }
    /// The Mac no longer has the file (`StagedAttachment.isExpired`): said on the card, and its path is not sent.
    private var expiredBadge: some View {
        Text("Expired").font(style.face(10, bold: true, relativeTo: .caption2)).foregroundStyle(style.background)
            .lineLimit(1).fixedSize()
            .padding(.horizontal, 5).padding(.vertical, 2)
            .background(style.warning, in: Capsule())
            .padding(.bottom, 5)
            .accessibilityHidden(true)
    }
    @ViewBuilder private func content(expired: Bool) -> some View {
        let spoken = expired ? ", expired on the Mac, attach it again" : ""
        if card.kind == .image, let thumbnail = ChatAttachmentImageCache.image(images.thumbnailURL(card.id)) {
            Button(action: open) {
                Image(uiImage: thumbnail).resizable().scaledToFill()
                    .frame(width: side, height: side).clipShape(shape).contentShape(shape)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Image, \(card.name), \(card.sizeText)" + spoken).accessibilityHint("Shows it full screen")
        } else {
            HStack(spacing: style.pt(8)) {
                Image(systemName: card.kind == .image ? "photo" : card.symbol)
                    .font(.system(size: style.pt(17), weight: .medium)).foregroundStyle(style.accent)
                    .frame(width: style.pt(38), height: style.pt(38))
                    .background(style.accent.opacity(0.12), in: RoundedRectangle(cornerRadius: style.native ? style.pt(9) : 3, style: .continuous))
                VStack(alignment: .leading, spacing: 2) {
                    Text(card.name).font(style.face(13, relativeTo: .footnote)).foregroundStyle(style.text).lineLimit(1).truncationMode(.middle)
                    Text(expired ? "Expired" : card.sizeText).font(style.face(11, relativeTo: .caption)).foregroundStyle(expired ? style.warning : style.muted).lineLimit(1)
                }
                .frame(maxWidth: style.pt(118), alignment: .leading)
            }
            .padding(.leading, style.pt(10)).padding(.trailing, style.pt(14))
            .frame(height: side)
            .background(style.panel, in: shape)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("File, \(card.name), \(card.sizeText)" + spoken)
        }
    }
}

/// A file on its way to the Mac: its card at once, with a ring that fills as it is sent, until its real card takes its place. × cancels
/// the files on their way (one upload at a time).
struct ChatPendingAttachmentCard: View {
    @Environment(\.desktopStyle) private var style
    let card: PendingAttachment
    let cancel: () -> Void
    private var side: CGFloat { style.pt(64) }
    private var shape: RoundedRectangle { RoundedRectangle(cornerRadius: style.native ? style.pt(12) : 4, style: .continuous) }
    private var status: String { card.size == nil ? "Preparing…" : "Sending · \(Int((card.fraction * 100).rounded()))%" }

    var body: some View {
        Group {
            if card.kind == .image {
                ring.frame(width: side, height: side)
            } else {
                HStack(spacing: style.pt(8)) {
                    ring.frame(width: style.pt(38), height: style.pt(38))
                    VStack(alignment: .leading, spacing: 2) {
                        Text(card.name).font(style.face(13, relativeTo: .footnote)).foregroundStyle(style.text).lineLimit(1).truncationMode(.middle)
                        Text(status).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted).monospacedDigit().lineLimit(1)
                    }
                    .frame(maxWidth: style.pt(118), alignment: .leading)
                }
                .padding(.leading, style.pt(10)).padding(.trailing, style.pt(14))
                .frame(height: side)
            }
        }
        .background(style.panel, in: shape)
        .overlay(shape.strokeBorder(style.divider, lineWidth: 1))
        .overlay(alignment: .topTrailing) { ChatCardCloseButton(label: "Cancel sending \(card.name)", action: cancel) }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(card.kind == .image ? "Image" : "File"), \(card.name), \(status)")
        .accessibilityIdentifier("attachment-card-pending")
    }
    /// Indeterminate while the file is read, then how much the Mac has.
    private var ring: some View {
        ZStack {
            if card.size == nil {
                ProgressView().controlSize(.small).tint(style.muted)
            } else {
                Circle().stroke(style.divider, lineWidth: 2.5)
                Circle().trim(from: 0, to: max(0.03, card.fraction)).stroke(style.accent, style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
                    .rotationEffect(.degrees(-90)).animation(.easeOut(duration: 0.2), value: card.fraction)
            }
        }
        .frame(width: style.pt(22), height: style.pt(22))
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// A card's ×: a small filled circle on its corner, in a 44-point target centred on it.
struct ChatCardCloseButton: View {
    @Environment(\.desktopStyle) private var style
    let label: String
    let action: () -> Void
    var body: some View {
        Button(action: action) {
            Image(systemName: "xmark").font(.system(size: style.pt(9), weight: .bold)).foregroundStyle(style.background)
                .frame(width: style.pt(20), height: style.pt(20))
                .background(style.text.opacity(0.85), in: Circle())
                .overlay(Circle().strokeBorder(style.background, lineWidth: 1.5))
                .frame(width: style.target, height: style.target).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .offset(x: style.target / 2 - style.pt(6), y: -(style.target / 2 - style.pt(6)))
        .accessibilityLabel(label)
        .accessibilityIdentifier("attachment-remove")
    }
}

/// The cards' thumbnails, read once from disk.
@MainActor enum ChatAttachmentImageCache {
    private static let cache = NSCache<NSURL, UIImage>()
    static func image(_ url: URL) -> UIImage? {
        if let known = cache.object(forKey: url as NSURL) { return known }
        guard let image = UIImage(contentsOfFile: url.path) else { return nil }
        cache.setObject(image, forKey: url as NSURL)
        return image
    }
}

/// A staged image full screen, on black as Photos shows one: pinch or double-tap to zoom, Done or a swipe down to close.
struct ChatAttachmentPreview: View {
    @Environment(\.desktopStyle) private var style
    let card: StagedAttachment
    let url: URL
    let close: () -> Void
    @State private var image: UIImage?
    @State private var zoom: CGFloat = 1
    @State private var settledZoom: CGFloat = 1
    @State private var pan: CGSize = .zero

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            if let image {
                Image(uiImage: image).resizable().scaledToFit()
                    .scaleEffect(zoom)
                    .offset(pan)
                    .gesture(MagnifyGesture().onChanged { zoom = max(1, min(5, settledZoom * $0.magnification)) }.onEnded { _ in settledZoom = zoom })
                    .simultaneousGesture(DragGesture().onChanged { value in
                        pan = zoom > 1 ? value.translation : CGSize(width: 0, height: max(0, value.translation.height))
                    }.onEnded { value in
                        if zoom <= 1, value.translation.height > 120 { close() } else if zoom <= 1 { withAnimation(.spring) { pan = .zero } }
                    })
                    .onTapGesture(count: 2) { withAnimation(.spring) { zoom = zoom > 1 ? 1 : 2.5; settledZoom = zoom; pan = .zero } }
                    .accessibilityLabel("Image, \(card.name)")
            } else {
                ProgressView().tint(.white)
            }
        }
        .overlay(alignment: .top) {
            HStack(spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(card.name).font(style.face(15, bold: true, relativeTo: .headline)).lineLimit(1).truncationMode(.middle)
                    Text(card.sizeText).font(style.face(12, relativeTo: .caption)).opacity(0.7)
                }
                Spacer(minLength: 8)
                Button("Done", action: close).font(style.face(16, bold: true, relativeTo: .body))
                    .frame(minWidth: style.target, minHeight: style.target).contentShape(Rectangle())
                    .accessibilityIdentifier("attachment-preview-done")
            }
            .foregroundStyle(.white)
            .padding(.horizontal, 16).padding(.top, 4)
            .background(LinearGradient(colors: [.black.opacity(0.6), .clear], startPoint: .top, endPoint: .bottom).ignoresSafeArea())
        }
        .statusBarHidden(false)
        .preferredColorScheme(.dark)
        .task(id: url) {
            let url = url
            image = await Task.detached(priority: .userInitiated) { UIImage(contentsOfFile: url.path) }.value
        }
    }
}
