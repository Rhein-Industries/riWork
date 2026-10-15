import SwiftUI
import UIKit
import RiWorkCore

// The `/` and `@` list above a chat's composer field: the highlighted row's card (what it is, what it puts in, what sending does), then
// the rows, at most five in view before they scroll. The decisions are `ComposerSuggestionList` in RiWorkCore; the catalog is the
// desktop's (`RemoteModel+Suggestions.swift`).
//
// Touch: a tap on a row highlights it and its card shows; a tap on the highlighted row, or on the card's Insert, puts it in the draft; ×
// closes the list until the token goes. A hardware keyboard: ↑ and ↓ move, ⏎ and Tab put the row in, ⎋ closes (`ChatKeyRouter`).
// Nothing here sends: the person's own ⏎ or Send does, once the list is gone.

extension ChatComposer {
    /// The list is on screen: a token opened it and something matches (or the files are being listed), and no request answers the keys
    /// with an empty draft.
    var showsSuggestions: Bool {
        suggestions.isShown && !(approval != nil && conversation.draft.isEmpty && conversation.attachments.isEmpty)
    }
    /// The text view's text or selection changed.
    func suggestionCaretMoved(_ text: String, _ selection: NSRange) {
        suggestionCaret = selection.length == 0 ? selection.location : nil
        refreshSuggestions(text: text)
    }
    /// Filters again what the chat's catalog holds, and has the catalog read while a token is open (it is read once, then every 30 s
    /// at most; filtering never waits for it).
    func refreshSuggestions(text: String) {
        suggestions.update(text: text, caret: suggestionCaret, catalog: suggestionCatalog)
        if suggestions.trigger != nil { wantSuggestions?() }
    }
}

/// Keeps the list with the catalog as it is read, and closes it when the draft changes from elsewhere (sent, restored, another chat).
struct ComposerSuggestionUpdates: ViewModifier {
    let composer: ChatComposer
    func body(content: Content) -> some View {
        content
            .onChange(of: composer.suggestionCatalog) { _, _ in composer.refreshSuggestions(text: composer.suggestions.text) }
            .onChange(of: composer.conversation.draft) { _, draft in
                guard draft != composer.suggestions.text else { return }
                composer.suggestionCaret = nil
                composer.refreshSuggestions(text: draft)
            }
            .onChange(of: composer.conversation.id) { _, _ in
                composer.suggestions = ComposerSuggestionList()
                composer.suggestionCaret = nil
            }
    }
}

extension UITextView {
    /// Puts `text` in place of the whole text with the caret at UTF-16 offset `caret`, and tells the delegate as typing does
    /// (`TextInsertion`), so a SwiftUI binding follows.
    func putDraft(_ text: String, caret: Int) {
        self.text = text
        selectedRange = NSRange(location: min(max(caret, 0), (text as NSString).length), length: 0)
        delegate?.textViewDidChange?(self)
        scrollRangeToVisible(selectedRange)
    }
}

/// The list and the card, on one card above the field.
struct ComposerSuggestionPanel: View {
    @Environment(\.desktopStyle) private var style
    let list: ComposerSuggestionList
    /// A tap on a row (by position): it highlights the row, or puts the highlighted one in.
    let tap: (Int) -> Void
    /// The card's Insert: puts the highlighted row in.
    let insert: () -> Void
    let close: () -> Void
    /// The most rows in view before the list scrolls.
    static let visibleRows = 5
    @State private var rowHeight: CGFloat = 0

    private var files: Bool { list.trigger?.sigil == .mention }
    var body: some View {
        ChatCard {
            if let preview = list.preview {
                ComposerSuggestionCard(preview: preview, insert: insert, close: close)
                DesktopRule()
            }
            if !list.items.isEmpty {
                BoundedScroll(maxHeight: (rowHeight > 0 ? rowHeight : style.target + 4) * CGFloat(Self.visibleRows)) {
                    ScrollViewReader { proxy in
                        VStack(spacing: 0) {
                            ForEach(Array(list.items.enumerated()), id: \.element.id) { index, item in
                                row(item, at: index).id(item.id).boundedScrollBreak()
                            }
                        }
                        .onChange(of: list.highlighted) { _, index in
                            if list.items.indices.contains(index) { proxy.scrollTo(list.items[index].id) }
                        }
                    }
                }
            }
            if list.listingFiles {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Listing files…").font(style.system(.footnote)).foregroundStyle(style.muted)
                    Spacer(minLength: 0)
                    ComposerSuggestionClose(close: close)
                }
                .padding(.leading, 12)
                .accessibilityElement(children: .combine)
                .chatLayoutProbe("suggestion-loading")
            }
            if list.filesTruncated, !list.items.isEmpty {
                Text("The folder is large: not every file was listed.").font(style.system(.caption)).foregroundStyle(style.muted)
                    .padding(.horizontal, 12).padding(.vertical, 4)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(files ? "File suggestions" : "Command suggestions")
        .chatLayoutProbe("suggestions")
    }

    private func row(_ item: ComposerSuggestion, at index: Int) -> some View {
        let highlighted = index == list.highlighted
        return Button { tap(index) } label: {
            HStack(spacing: 10) {
                Image(systemName: Self.symbol(item)).font(.system(size: style.pt(14))).foregroundStyle(highlighted ? style.accent : style.muted)
                    .frame(width: style.pt(18)).accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 1) {
                    Text(verbatim: item.title).font(style.code).foregroundStyle(style.text).lineLimit(1).truncationMode(.middle)
                    Text(verbatim: item.detail).font(style.system(.caption)).foregroundStyle(style.muted).lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12).padding(.vertical, 4)
            .frame(maxWidth: .infinity, minHeight: style.target, alignment: .leading)
            .desktopRowFill(style, selected: highlighted)
        }
        .buttonStyle(.plain)
        .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { if index == 0 { rowHeight = $0 } }
        .accessibilityLabel("\(item.title), \(item.detail)")
        .accessibilityAddTraits(highlighted ? .isSelected : [])
        .accessibilityHint(highlighted ? "Puts it in the message. Nothing is sent." : "Shows what it puts in")
        .accessibilityAction(named: "Insert") {
            if !highlighted { tap(index) }
            tap(index)
        }
        .chatLayoutProbe("suggestion-row-\(index)") { tap(index) }
    }
    static func symbol(_ item: ComposerSuggestion) -> String {
        switch item {
        case .command: "chevron.right.square"
        case .file(let file): file.dir ? "folder" : "doc"
        }
    }
}

/// What the highlighted row is before it is put in: its text, what kind of thing it is, what a command does, and what sending does
/// with it (or, for a file, that only its path goes in).
struct ComposerSuggestionCard: View {
    @Environment(\.desktopStyle) private var style
    let preview: ComposerPreview
    let insert: () -> Void
    let close: () -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 4) {
                VStack(alignment: .leading, spacing: 1) {
                    Text(verbatim: preview.title).font(style.code.weight(.semibold)).foregroundStyle(style.text).lineLimit(1).truncationMode(.middle)
                    Text(verbatim: preview.kind).font(style.system(.caption)).foregroundStyle(style.muted).lineLimit(1)
                }
                Spacer(minLength: 4)
                Button(action: insert) { Text("Insert").font(style.system(.subheadline, weight: .semibold)) }
                    .buttonStyle(DesktopButtonStyle(prominent: true, compact: true))
                    .accessibilityLabel("Insert \(preview.inserts)").accessibilityHint("Puts it in the message. Nothing is sent.")
                    .chatLayoutProbe("suggestion-insert", action: insert)
                ComposerSuggestionClose(close: close)
            }
            ForEach(Array(preview.body.enumerated()), id: \.offset) { _, line in
                Text(verbatim: line).font(style.system(.footnote)).foregroundStyle(style.text).lineLimit(2).fixedSize(horizontal: false, vertical: true)
            }
            Text(verbatim: preview.effect).font(style.system(.caption)).foregroundStyle(style.muted).lineLimit(3).fixedSize(horizontal: false, vertical: true)
        }
        .padding(.leading, 12).padding(.bottom, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .chatLayoutProbe("suggestion-preview")
    }
}

/// × : closes the list until its token goes.
private struct ComposerSuggestionClose: View {
    @Environment(\.desktopStyle) private var style
    let close: () -> Void
    var body: some View {
        Button(action: close) { Image(systemName: "xmark").font(.system(size: style.pt(12), weight: .semibold)).foregroundStyle(style.muted) }
            .buttonStyle(TargetButtonStyle())
            .accessibilityLabel("Close suggestions")
            .chatLayoutProbe("suggestion-close", action: close)
    }
}
