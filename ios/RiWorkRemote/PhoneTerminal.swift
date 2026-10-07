import SwiftUI
import UIKit
import RiWorkCore

/// What the swipe gesture of a full-screen program keeps between events. Plain storage, not observable.
final class PhoneScrollState {
    var pager = SwipePager()
    /// Where the current swipe began; a different start is a new swipe, even when the last one was cancelled without ending.
    var swipeStart: CGPoint?
}

/// The terminal on an iPhone.
///
/// - The shell's scrollback and screen are drawn by `TerminalSurfaceView` (UIKit): one scroll surface over every line loaded, with a
///   fixed place for every line, so lines that arrive above or below the view never move what is being read.
/// - Long lines are clipped at the right edge; there is no sideways scrolling (the desktop pane has the phone's width).
/// - The view follows new output only while it is at the bottom, or within a line of it. Scrolled up, it stays where it is. A pill
///   takes the reader back.
/// - Older lines are fetched in the background (see `RemoteModel+Scroll.swift`); scrolling through them is local.
/// - While a full-screen program (vim, less, htop) has the screen there is nothing to scroll; vertical swipes page the program.
struct PhoneTerminal: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.colorScheme) private var colorScheme
    @Bindable var model: RemoteModel
    /// The size being drawn now (follows a pinch), and the size the grid was last committed at.
    let fontSize: Double
    let committedSize: Double
    let showCursor: Bool
    let padding: Double
    @State private var scroll = PhoneScrollState()

    private var cell: (width: Double, height: Double) { TerminalFont.cell(size: fontSize) }
    private var settings: TerminalRenderer.Settings {
        TerminalRenderer.Settings(style: style, dark: colorScheme == .dark, showCursor: showCursor, committedSize: committedSize, boldIsBright: model.boldIsBright)
    }

    var body: some View {
        let _ = Perf.count("body.PhoneTerminal")
        if !model.hasOutput {
            ScrollView { TerminalPlaceholder(model: model).frame(maxWidth: .infinity, alignment: .leading) }
        } else if model.alternateScreen {
            alternateScreen
        } else {
            ScrollbackSurface(model: model, settings: settings, fontSize: fontSize, padding: padding)
        }
    }

    // MARK: Alternate screen

    /// A full-screen program: its screen, as the desktop draws it, and nothing to scroll. Swipes page the program itself.
    /// The screen takes exactly the room the pane is given, never more: rows that do not fit (the desktop has not followed a smaller
    /// pane yet) are left out, keeping the cursor in view (`AlternateRows`). A screen taller than the pane would make the whole
    /// screen taller than the display: the header and tabs went up under the status bar, the last rows down under the key bar, and
    /// the pane, measured that tall, kept asking the desktop for that many rows.
    private var alternateScreen: some View {
        let lines = Array(model.alternateLines)
        let current = settings
        let size = cell
        let cursorLine = model.styledOutput.cursorLine.map { $0 - model.styledOutput.historyLines }
        return GeometryReader { geometry in
            let fitting = AlternateRows.fitting(height: geometry.size.height - 2 * padding, lineHeight: size.height)
            VStack(alignment: .leading, spacing: 0) {
                ForEach(AlternateRows.shown(count: lines.count, fitting: fitting, cursor: cursorLine), id: \.self) { index in
                    TerminalRow(line: lines[index], cursorColumn: showCursor && cursorLine == index ? model.styledOutput.cursorColumn : nil,
                                settings: current, fontSize: fontSize, height: size.height).equatable()
                        .frame(maxWidth: .infinity, alignment: .topLeading).clipped()
                }
            }
            .padding(padding)
            .frame(width: geometry.size.width, height: geometry.size.height, alignment: .topLeading)
            .contentShape(Rectangle())
            .simultaneousGesture(swipe(viewHeight: geometry.size.height))
        }
        .clipped()
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Terminal output, full-screen program")
        .accessibilityHint("Drag up or down to page the program")
        .overlay(alignment: .top) {
            if model.keysSupport != .unsupported { AlternateHint(loud: model.alternateHintLoud) }
        }
    }

    /// A page is a share of the pane as it is on screen.
    private func swipe(viewHeight swipeHeight: Double) -> some Gesture {
        DragGesture(minimumDistance: 12)
            .onChanged { value in
                if scroll.swipeStart != value.startLocation { scroll.swipeStart = value.startLocation; scroll.pager.begin() }
                // Only a mostly vertical drag pages.
                let vertical = abs(value.translation.height) >= abs(value.translation.width) ? value.translation.height : 0
                if let key = scroll.pager.update(translation: vertical, viewHeight: swipeHeight, now: ProcessInfo.processInfo.systemUptime) {
                    model.sendPageKey(key)
                }
            }
            .onEnded { value in
                let vertical = abs(value.predictedEndTranslation.height) >= abs(value.predictedEndTranslation.width) ? value.predictedEndTranslation.height : 0
                if let key = scroll.pager.end(predictedTranslation: vertical, viewHeight: swipeHeight, now: ProcessInfo.processInfo.systemUptime) {
                    model.sendPageKey(key)
                }
                scroll.swipeStart = nil
            }
    }
}

/// The scrollback surface of the shell on screen. This is the part of the screen that follows the buffer, so it is a view of its own:
/// it reads the buffer (the header row at the top of the loaded lines, which changes with every live answer) and the keys typed (the
/// room kept for the chip above the keyboard, the request to go to the bottom), and nothing else is rebuilt for those.
private struct ScrollbackSurface: View {
    @Environment(\.desktopStyle) private var style
    let model: RemoteModel
    let settings: TerminalRenderer.Settings
    let fontSize: Double
    let padding: Double

    var body: some View {
        let _ = Perf.count("body.ScrollbackSurface")
        let floatingInset = model.floatingInset(style: style)
        let look = TerminalSurfaceView.Look(settings: settings, fontSize: fontSize, padding: padding, floatingInset: floatingInset, headerSize: 10 * style.scale)
        TerminalSurface(model: model, look: look, header: model.historyHeader, jumpToken: model.typedCount &+ model.jumpRequests)
            .overlay(alignment: .bottomTrailing) { LivePill(model: model, floatingInset: floatingInset) }
    }
}

/// "↓ Live · N new": takes the reader back to the latest output. Its own view, since the count moves with the output.
private struct LivePill: View {
    @Environment(\.desktopStyle) private var style
    let model: RemoteModel
    let floatingInset: Double

    var body: some View {
        let pill = model.scrollFollow.pill
        Group {
            if let pill { button(pill) }
        }
        .animation(.easeInOut(duration: 0.15), value: pill)
    }

    private func button(_ pill: StickyBottom.Pill) -> some View {
        Button { model.jumpToLatest() } label: {
            Text(pill.label).font(style.face(11, bold: true, relativeTo: .caption)).foregroundStyle(style.accent)
                .padding(.horizontal, 12).frame(minHeight: style.pt(30))
                .background(.ultraThinMaterial, in: Capsule())
                .overlay(Capsule().stroke(style.divider, lineWidth: 1))
                // A full target (44 points, more at a larger interface) around the 30-point pill, without moving it.
                .padding(.vertical, (style.target - style.pt(30)) / 2).contentShape(Rectangle()).padding(.vertical, -(style.target - style.pt(30)) / 2)
        }
        .buttonStyle(.plain)
        .padding(.trailing, 10).padding(.bottom, 8 + floatingInset)
        .transition(.opacity)
        .accessibilityLabel(pill.accessibilityLabel)
    }
}

/// What the terminal shows before there is anything to show.
struct TerminalPlaceholder: View {
    @Environment(\.desktopStyle) private var style
    let model: RemoteModel
    var body: some View {
        if model.state == .connected && !model.viewportReady && model.viewportError == nil && model.output.isEmpty {
            ProgressView("Fitting desktop terminal…").padding(20)
        } else {
            Text(model.lastOutputAt == nil ? "Output will appear when this session is connected." : "The session has no output yet.")
                .foregroundStyle(style.terminalForeground.opacity(0.6)).padding(20)
        }
    }
}

/// "Scrolling the app": swipes go to the full-screen program as Page Up / Page Down. Clear for the first few programs, then faint.
struct AlternateHint: View {
    @Environment(\.desktopStyle) private var style
    let loud: Bool
    @State private var settled = false
    var body: some View {
        Text("Scrolling the app").font(style.face(10, relativeTo: .caption2))
            .foregroundStyle(style.terminalForeground)
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(style.panel.opacity(0.85), in: Capsule())
            .opacity(loud && !settled ? 0.95 : 0.4)
            .padding(.top, 6)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
            .animation(.easeInOut(duration: 0.4), value: settled)
            .task { try? await Task.sleep(for: .seconds(4)); settled = true }
    }
}
