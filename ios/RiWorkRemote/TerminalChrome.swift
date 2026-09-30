import SwiftUI
import UIKit
import RiWorkCore

/// The small single-line chip above the key bar: pending input as text and key glyphs, plus why it is waiting.
struct KeyPreviewChip: View {
    let preview: KeyPreview
    var discard: () -> Void
    private var tint: Color {
        switch preview.tone {
        case .sending: DesktopStyle.muted
        case .offline, .full: DesktopStyle.warning
        case .blocked: DesktopStyle.error
        }
    }
    var body: some View {
        HStack(spacing: 6) {
            Text(preview.text).font(.custom("Menlo", size: 11, relativeTo: .caption))
                .lineLimit(1).truncationMode(.head).frame(maxWidth: .infinity, alignment: .leading)
            Text(preview.label).font(.custom("Menlo", size: 10, relativeTo: .caption2)).foregroundStyle(tint)
                .lineLimit(1).minimumScaleFactor(0.7).layoutPriority(1)
            Button { discard() } label: { Image(systemName: "xmark.circle.fill").font(.system(size: 13)).frame(width: 30, height: 28).contentShape(Rectangle()) }
                .buttonStyle(.plain).foregroundStyle(DesktopStyle.muted).accessibilityLabel("Discard pending input")
        }
        .padding(.leading, 8).frame(minHeight: 28).background(DesktopStyle.panel)
        .overlay(alignment: .top) { DesktopRule() }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Pending input: \(preview.text). \(preview.label)")
    }
}

/// Translucent corner controls in focus mode. Fully visible after a tap, then fades to a faint ghost.
struct FocusControls: View {
    var visible: Bool
    var fontSize: Double
    var smaller: () -> Void
    var larger: () -> Void
    var exit: () -> Void
    var body: some View {
        HStack(spacing: 0) {
            Button("Smaller text", systemImage: "textformat.size.smaller", action: smaller).disabled(fontSize <= TerminalFontSize.range.lowerBound)
            Button("Larger text", systemImage: "textformat.size.larger", action: larger).disabled(fontSize >= TerminalFontSize.range.upperBound)
            Button("Leave focus mode", systemImage: "arrow.down.right.and.arrow.up.left", action: exit)
        }
        .labelStyle(.iconOnly).buttonStyle(DesktopButtonStyle(compact: true))
        .background(.ultraThinMaterial, in: Capsule())
        .overlay(Capsule().stroke(DesktopStyle.divider, lineWidth: 1))
        .opacity(visible ? 1 : 0.18)
        // A faded control is not tappable: the first tap only brings it back.
        .allowsHitTesting(visible)
        .animation(.easeInOut(duration: 0.3), value: visible)
        .padding(6)
    }
}

/// The screen text with the desktop cursor drawn as an inverted block, when the desktop reported one.
enum TerminalScreenText {
    static func text(output: String, cursorOffset: Int?) -> Text {
        guard let cursorOffset, cursorOffset >= 0, cursorOffset < output.count else { return Text(output) }
        let index = output.index(output.startIndex, offsetBy: cursorOffset)
        var attributed = AttributedString(output[..<index])
        var cell = AttributedString(String(output[index]))
        cell.backgroundColor = DesktopStyle.accent
        cell.foregroundColor = DesktopStyle.background
        attributed += cell
        attributed += AttributedString(output[output.index(after: index)...])
        return Text(attributed)
    }
}

/// Small badge when the pane is in tmux copy mode.
struct CopyModeBadge: View {
    var body: some View {
        Text("COPY MODE").font(.custom("Menlo-Bold", size: 9, relativeTo: .caption2)).foregroundStyle(DesktopStyle.warning)
            .padding(.horizontal, 4).padding(.vertical, 1)
            .overlay(RoundedRectangle(cornerRadius: 2).stroke(DesktopStyle.warning.opacity(0.6), lineWidth: 1))
            .accessibilityLabel("Terminal is in copy mode")
    }
}
