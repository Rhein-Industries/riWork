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
    /// Room kept under the last line for the pending-input chip and notices that float over the pane.
    let floatingInset: Double
    @State private var scroll = PhoneScrollState()

    private var cell: (width: Double, height: Double) { TerminalFont.cell(size: fontSize) }
    private var settings: TerminalRenderer.Settings {
        TerminalRenderer.Settings(style: style, dark: colorScheme == .dark, showCursor: showCursor, committedSize: committedSize, boldIsBright: model.boldIsBright)
    }

    var body: some View {
        if model.output.isEmpty {
            ScrollView { TerminalPlaceholder(model: model).frame(maxWidth: .infinity, alignment: .leading) }
        } else if model.alternateScreen {
            alternateScreen
        } else {
            scrollback
        }
    }

    // MARK: Scrollback

    private var scrollback: some View {
        TerminalSurface(model: model, look: look, header: model.historyHeader, jumpToken: model.typedCount &+ model.jumpRequests)
            .overlay(alignment: .bottomTrailing) {
                if let pill = model.scrollFollow.pill { pillButton(pill) }
            }
            .animation(.easeInOut(duration: 0.15), value: model.scrollFollow.pill)
    }

    private var look: TerminalSurfaceView.Look {
        TerminalSurfaceView.Look(settings: settings, fontSize: fontSize, padding: padding, floatingInset: floatingInset, headerSize: 10 * style.scale)
    }

    private func pillButton(_ pill: StickyBottom.Pill) -> some View {
        Button { model.jumpToLatest() } label: {
            Text(pill.label).font(style.mono(11, bold: true, relativeTo: .caption)).foregroundStyle(style.accent)
                .padding(.horizontal, 12).frame(minHeight: style.pt(30))
                .background(.ultraThinMaterial, in: Capsule())
                .overlay(Capsule().stroke(style.divider, lineWidth: 1))
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .padding(.trailing, 10).padding(.bottom, 8 + floatingInset)
        .transition(.opacity)
        .accessibilityLabel(pill.accessibilityLabel)
    }

    // MARK: Alternate screen

    /// A full-screen program: its screen, as the desktop draws it, and nothing to scroll. Swipes page the program itself.
    private var alternateScreen: some View {
        let lines = Array(model.alternateLines)
        let current = settings
        let size = cell
        let cursorLine = model.styledOutput.cursorLine.map { $0 - model.styledOutput.historyLines }
        return VStack(alignment: .leading, spacing: 0) {
            ForEach(lines.indices, id: \.self) { index in
                TerminalRow(line: lines[index], cursorColumn: showCursor && cursorLine == index ? model.styledOutput.cursorColumn : nil,
                            settings: current, fontSize: fontSize, height: size.height).equatable()
                    .frame(maxWidth: .infinity, alignment: .topLeading).clipped()
            }
        }
        .padding(padding)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .contentShape(Rectangle())
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Terminal output, full-screen program")
        .accessibilityHint("Drag up or down to page the program")
        .simultaneousGesture(swipe)
        .overlay(alignment: .top) {
            if model.keysSupport != .unsupported { AlternateHint(loud: model.alternateHintLoud) }
        }
    }

    private var swipe: some Gesture {
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
    private var swipeHeight: Double { Double(model.terminalArea?.height ?? 600) }
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
        Text("Scrolling the app").font(style.mono(10, relativeTo: .caption2))
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
