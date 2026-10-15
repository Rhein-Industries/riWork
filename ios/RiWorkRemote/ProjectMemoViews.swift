import SwiftUI
import UIKit
import RiWorkCore

/// A project's memo: plain Markdown the person writes for that project, saved by itself a moment after typing pauses. The line under it
/// says how saving stands, and never says saved before the Mac answered: text the Mac does not have is kept on this iPhone and the line
/// says so.
struct ProjectMemoSheet: View {
    @Environment(\.desktopStyle) private var style
    @Bindable var model: RemoteModel

    var body: some View {
        let editor = model.memoEditor
        VStack(spacing: 0) {
            WorkspaceBar(title: style.cased("Memo · \(editor?.projectName ?? "Project")")) {
                Button("Done") { model.closeMemoEditor() }.accessibilityHint("Saves what is not saved yet and closes the memo")
            }
            if let editor {
                ZStack(alignment: .topLeading) {
                    MemoTextView(text: Binding(get: { editor.text }, set: { model.editMemo($0) }), editable: editor.isEditable)
                    if editor.text.isEmpty, editor.isEditable {
                        Text("Write notes for this project in Markdown: plans, links, decisions. Only you see them. Type /memo in one of its chats to insert them.")
                            .font(style.mono(13, relativeTo: .body)).foregroundStyle(style.muted)
                            .padding(.horizontal, 9).padding(.vertical, 12)
                            .allowsHitTesting(false).accessibilityHidden(true)
                    }
                }
                .padding(.horizontal, 8)
                status(editor)
            }
        }
        .desktopSheetSurface(style)
        .foregroundStyle(style.text).font(style.face(13, relativeTo: .body)).tint(style.accent)
        .presentationDetents([.large]).presentationDragIndicator(.visible)
    }

    private func status(_ editor: ProjectMemoEditor) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(editor.status)
                .font(style.face(12, relativeTo: .footnote))
                .foregroundStyle(editor.statusIsWarning ? style.warning : style.muted)
                .accessibilityIdentifier("memo.status")
            Spacer(minLength: 0)
            if editor.canRetry { retry }
            if editor.phase == .conflict {
                Button("Use the Mac’s") { model.resolveMemoConflict(keepMine: false) }.accessibilityIdentifier("memo.useMac")
                Button("Keep mine") { model.resolveMemoConflict(keepMine: true) }.accessibilityIdentifier("memo.keepMine")
            }
        }
        .padding(.horizontal, 16).padding(.vertical, 10)
    }

    private var retry: some View {
        Button("Retry") { Task { await model.retryMemo() } }.disabled(model.state != .connected)
    }
}

/// The memo's text field: a UIKit text view with smart quotes and dashes off, so Markdown is kept exactly as typed.
struct MemoTextView: UIViewRepresentable {
    @Binding var text: String
    var editable: Bool
    func makeCoordinator() -> Coordinator { Coordinator(self) }
    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.delegate = context.coordinator
        view.backgroundColor = .clear
        let style = context.environment.desktopStyle
        view.textColor = style.textUI
        view.tintColor = style.accentUI
        view.font = UIFontMetrics(forTextStyle: .body).scaledFont(for: UIFont(name: "Menlo", size: 13) ?? .monospacedSystemFont(ofSize: 13, weight: .regular))
        view.adjustsFontForContentSizeCategory = true
        view.smartQuotesType = .no
        view.smartDashesType = .no
        view.smartInsertDeleteType = .no
        view.autocapitalizationType = .sentences
        view.dataDetectorTypes = []
        view.alwaysBounceVertical = true
        view.keyboardDismissMode = .interactive
        view.accessibilityLabel = "Project memo"
        view.accessibilityIdentifier = "memo.editor"
        return view
    }
    func updateUIView(_ view: UITextView, context: Context) {
        context.coordinator.parent = self
        view.isEditable = editable
        if view.text != text { view.text = text }
    }
    @MainActor final class Coordinator: NSObject, UITextViewDelegate {
        var parent: MemoTextView
        init(_ parent: MemoTextView) { self.parent = parent }
        func textViewDidChange(_ textView: UITextView) { parent.text = textView.text }
    }
}

/// `/memo` over a chat: the project's memo with **Insert into composer** (appended to the draft, never sent) and **Edit**; an empty memo
/// offers Edit only, and a chat of no project says there is no memo here.
struct MemoPreviewSheet: View {
    @Environment(\.desktopStyle) private var style
    let preview: ProjectMemoPreview
    let insert: () -> Void
    let edit: () -> Void
    let close: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(title: style.cased(preview == .noProject ? "No project memo here" : "Project memo")) {
                Button("Close", action: close)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    if let message = preview.message {
                        Text(message).foregroundStyle(style.muted).accessibilityIdentifier("memo.preview.message")
                    }
                    if case .memo(let memo) = preview, !memo.isEmpty {
                        Text(memo.text).font(style.mono(13, relativeTo: .body)).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .accessibilityIdentifier("memo.preview.text")
                    }
                    if case .loading = preview { ProgressView() }
                }
                .padding(16)
            }
            HStack(spacing: 12) {
                Spacer(minLength: 0)
                if preview.canEdit { Button("Edit", action: edit).accessibilityIdentifier("memo.preview.edit") }
                if preview.canInsert {
                    Button("Insert into composer", action: insert).buttonStyle(.borderedProminent)
                        .accessibilityIdentifier("memo.preview.insert")
                        .accessibilityHint("Adds the memo to the end of your message. Nothing is sent.")
                }
            }
            .padding(.horizontal, 16).padding(.vertical, 12)
        }
        .desktopSheetSurface(style)
        .foregroundStyle(style.text).font(style.face(13, relativeTo: .body)).tint(style.accent)
        .presentationDetents([.medium, .large]).presentationDragIndicator(.visible)
    }
}
