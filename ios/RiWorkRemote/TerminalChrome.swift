import SwiftUI
import UIKit
import RiWorkCore

/// The small single-line chip above the key bar: pending input as text and key glyphs, plus why it is waiting.
struct KeyPreviewChip: View {
    @Environment(\.desktopStyle) private var style
    let preview: KeyPreview
    var discard: () -> Void
    private var tint: Color {
        switch preview.tone {
        case .sending: style.muted
        case .offline, .full: style.warning
        case .blocked: style.error
        }
    }
    var body: some View {
        HStack(spacing: 6) {
            Text(preview.text).font(style.mono(11, relativeTo: .caption))
                .lineLimit(1).truncationMode(.head).frame(maxWidth: .infinity, alignment: .leading)
            Text(preview.label).font(style.face(10, relativeTo: .caption2)).foregroundStyle(tint)
                .lineLimit(1).minimumScaleFactor(0.7).layoutPriority(1)
            // A 44-point target over a 28-point chip: the extra height reaches over the chip's edges without making it taller.
            Button { discard() } label: {
                Image(systemName: "xmark.circle.fill").font(.system(size: style.pt(13))).frame(width: style.pt(44), height: style.pt(44)).contentShape(Rectangle())
            }
            .padding(.vertical, -style.pt(8))
                .buttonStyle(.plain).foregroundStyle(style.muted).accessibilityLabel("Discard pending input")
        }
        .padding(.leading, 8).frame(minHeight: style.pt(28)).background(style.panel)
        .overlay(alignment: .top) { DesktopRule() }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Pending input: \(preview.text). \(preview.label)")
    }
}

extension RemoteModel {
    /// The short note shown above the keyboard with direct typing (a delivery problem, a fallback); none with the line composer.
    var floatingNotice: String? { directTyping ? deliveryNotice : nil }
    /// Room kept under the last line for what floats over the pane: the pending-input chip and the short note. They float instead of
    /// taking room from the pane, so that they coming and going never changes the terminal's size (and never resizes the desktop).
    func floatingInset(style: DesktopStyle) -> Double {
        guard directTyping else { return 0 }
        return (keyPreview != nil ? Double(style.pt(FloatingStatus.chipHeight)) : 0) + (floatingNotice != nil ? Double(style.pt(FloatingStatus.noticeHeight)) : 0)
    }
}

/// Pending-input chip and short notices (also in focus mode), floating at the bottom of the terminal.
///
/// A view of its own: it reads the key buffers, which change with every key typed, and nothing else on the screen should be rebuilt
/// for that.
struct FloatingStatus: View {
    static let chipHeight: CGFloat = 28, noticeHeight: CGFloat = 24
    @Environment(\.desktopStyle) private var style
    let model: RemoteModel
    var body: some View {
        if model.directTyping {
            VStack(spacing: 0) {
                if let notice = model.floatingNotice {
                    Text(notice).font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1)
                        .padding(.horizontal, 8).frame(maxWidth: .infinity, minHeight: Double(style.pt(Self.noticeHeight)), alignment: .leading)
                        .background(style.panel).overlay(alignment: .top) { DesktopRule() }
                }
                if let preview = model.keyPreview {
                    KeyPreviewChip(preview: preview, discard: model.discardPendingKeys).frame(height: Double(style.pt(Self.chipHeight)))
                }
            }
        }
    }
}

/// Translucent corner controls in focus mode. Fully visible after a tap, then fades to a faint ghost.
struct FocusControls: View {
    @Environment(\.desktopStyle) private var style
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
        .overlay(Capsule().stroke(style.divider, lineWidth: 1))
        .opacity(visible ? 1 : 0.18)
        // A faded control is not tappable: the first tap only brings it back.
        .allowsHitTesting(visible)
        .animation(.easeInOut(duration: 0.3), value: visible)
        .padding(6)
    }
}

/// The terminal screen: its text in the synced colors, with attributes, and the desktop cursor drawn as an inverted block.
///
/// The screen is drawn line by line in a lazy stack, so only the lines that are on screen are built and laid out, and a redraw
/// touches only the lines that changed (typing changes one). Every line is exactly one grid row tall, whatever fonts its glyphs
/// come from, and the block is exactly as wide as the widest line, so the scroll extent never jumps as lines come into view.
/// Pinching scales the text through the font alone.
struct TerminalScreenView: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.colorScheme) private var colorScheme
    let screen: StyledScreen
    let showCursor: Bool
    /// The size being drawn now (follows a pinch), and the size the layout was last committed at.
    let fontSize: Double
    let committedSize: Double
    let boldIsBright: Bool

    private var settings: TerminalRenderer.Settings {
        TerminalRenderer.Settings(style: style, dark: colorScheme == .dark, showCursor: showCursor, committedSize: committedSize, boldIsBright: boldIsBright)
    }
    var body: some View {
        let current = settings
        let cell = TerminalFont.cell(size: fontSize)
        LazyVStack(alignment: .leading, spacing: 0) {
            ForEach(screen.lines.indices, id: \.self) { index in
                TerminalRow(line: screen.lines[index], cursorColumn: showCursor && screen.cursorLine == index ? screen.cursorColumn : nil,
                            settings: current, fontSize: fontSize, height: cell.height).equatable()
            }
        }
        .frame(width: max(1, Double(screen.columns) * cell.width), height: max(1, Double(screen.lines.count) * cell.height), alignment: .topLeading)
        .foregroundStyle(TerminalRenderer.color(current.colors.foreground))
    }
}

extension TerminalRenderer.Settings {
    /// The synced terminal colors and cursor for the current appearance.
    init(style: DesktopStyle, dark: Bool, showCursor: Bool, committedSize: Double, boldIsBright: Bool) {
        let colors = style.theme.terminalColors(dark: dark, boldIsBright: boldIsBright)
        let cursor = style.theme.terminalCursor
        self.init(colors: colors, cursorBackground: dark ? cursor.dark : cursor.light, cursorForeground: colors.background,
                  showCursor: showCursor, link: dark ? style.theme.accent.dark : style.theme.accent.light, fontSize: committedSize)
    }
}

/// One grid row. Equatable, so an unchanged line is not built again when the screen around it changes.
struct TerminalRow: View, Equatable {
    let line: StyledLine
    let cursorColumn: Int?
    let settings: TerminalRenderer.Settings
    let fontSize: Double
    let height: Double
    var body: some View {
        Group {
            if line.text.isEmpty {
                Color.clear
            } else {
                Text(TerminalRenderer.attributed(text: line.text, runs: line.runs, cursorColumn: cursorColumn, settings: settings))
                    .font(.custom("Menlo", fixedSize: fontSize)).fixedSize()
            }
        }.frame(height: height, alignment: .topLeading)
    }
}

/// The debug latency overlay: a small translucent block in the corner of the terminal. Touches go through it.
struct LatencyOverlay: View {
    @Environment(\.desktopStyle) private var style
    /// The numbers change with every answer; reading them here keeps the rest of the screen out of it.
    let model: RemoteModel
    var body: some View {
        TimelineView(.periodic(from: .now, by: 0.5)) { _ in
            let latency = model.latency, mode = model.syncMode
            let age = latency.age(at: ProcessInfo.processInfo.systemUptime)
            VStack(alignment: .leading, spacing: 1) {
                Text("\(mode.label) · \(LatencyBook.format(bytes: latency.payloadBytes))")
                Text("keys \(LatencyBook.format(latency.keys))")
                Text("out  \(LatencyBook.format(latency.output))")
                Text("echo \(LatencyBook.format(latency.echo))")
                Text("age  \(LatencyBook.format(age: age))")
                ForEach(Array(model.linkLines.enumerated()), id: \.offset) { _, line in Text(line) }
            }
            .font(style.mono(9, relativeTo: .caption2)).monospacedDigit()
            .foregroundStyle(style.text)
            .padding(.horizontal, 6).padding(.vertical, 4)
            .background(style.panel.opacity(0.78), in: RoundedRectangle(cornerRadius: 5))
            .overlay(RoundedRectangle(cornerRadius: 5).stroke(style.divider, lineWidth: 1))
        }
        .padding(6)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// Small badge when the pane is in tmux copy mode.
struct CopyModeBadge: View {
    @Environment(\.desktopStyle) private var style
    var body: some View {
        Text(style.cased("Copy mode")).font(style.face(9, bold: true, relativeTo: .caption2)).foregroundStyle(style.warning)
            .padding(.horizontal, 4).padding(.vertical, 1)
            .overlay(RoundedRectangle(cornerRadius: 2).stroke(style.warning.opacity(0.6), lineWidth: 1))
            .accessibilityLabel("Terminal is in copy mode")
    }
}
