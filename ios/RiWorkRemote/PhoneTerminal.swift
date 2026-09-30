import SwiftUI
import UIKit
import RiWorkCore

/// What the scroll view and its gestures keep between events. Plain storage, not observable: it changes with every scroll step and
/// nothing is to be redrawn for it.
final class PhoneScrollState {
    /// The line at the top of the view, as the scroll view reports it. SwiftUI keeps this line where it is when lines are added
    /// above it, which is what keeps a page of history from moving what is being read.
    var anchorID: Int?
    var metrics: ScrollMetrics?
    var phase = ScrollPhase.idle
    var pager = SwipePager()
    /// Where the current swipe began; a different start is a new swipe, even when the last one was cancelled without ending.
    var swipeStart: CGPoint?
}

/// The terminal on an iPhone: one vertical scroll view over every line loaded for the shell, scrollback and screen.
///
/// - Lines are keyed by their absolute index in `TerminalBuffer`, and rows are laid out lazily, one grid row each, so 20,000 lines
///   cost no more than the ones on screen.
/// - Long lines are clipped at the right edge; there is no sideways scrolling (the desktop pane has the phone's width).
/// - The view follows new output only while it is at the bottom, or within a line of it. Scrolled up, it stays where it is: new
///   lines and older pages are added under and over the line being read without moving it. A pill takes the reader back.
/// - Near the top of what is loaded, the next older page is fetched.
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

    static let bottomMarker = "terminal-bottom"

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
        let buffer = model.terminal
        let current = settings
        let size = cell
        return ScrollViewReader { proxy in
            ScrollView(.vertical) {
                VStack(alignment: .leading, spacing: 0) {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        if model.historyPaging && buffer.heldHistory > 0 { historyRow(buffer: buffer, height: size.height) }
                        ForEach(buffer.indices, id: \.self) { index in
                            TerminalRow(line: buffer[index] ?? PhoneTerminal.blank, cursorColumn: showCursor && buffer.cursorIndex == index ? buffer.cursorColumn : nil,
                                        settings: current, fontSize: fontSize, height: size.height).equatable()
                                .frame(maxWidth: .infinity, alignment: .topLeading).clipped()
                        }
                    }
                    .scrollTargetLayout()
                    .textSelection(.enabled)
                    .accessibilityLabel("Terminal output")
                    Color.clear.frame(height: 1).id(Self.bottomMarker)
                }.padding(padding)
            }
            .scrollPosition(id: Binding(get: { scroll.anchorID }, set: { scroll.anchorID = $0 }))
            // A fresh terminal starts at the bottom; with less than a screenful, the lines sit at the top like a real terminal's.
            .defaultScrollAnchor(.bottom, for: .initialOffset)
            .defaultScrollAnchor(.top, for: .alignment)
            .contentMargins(.bottom, floatingInset, for: .scrollContent)
            .onScrollGeometryChange(for: ScrollMetrics.self, of: PhoneTerminal.metrics) { old, new in scrolled(from: old, to: new, proxy: proxy) }
            .onScrollPhaseChange { _, phase in phaseChanged(phase) }
            .onChange(of: model.outputVersion) { _, _ in if model.scrollFollow.following { toBottom(proxy) } }
            .onChange(of: model.terminal.start) { _, _ in if model.scrollFollow.following { toBottom(proxy) } }
            // Lines renumbered: the line remembered as the top of the view is another line now.
            .onChange(of: model.terminal.epoch) { _, _ in scroll.anchorID = nil }
            // The scroll view can go away mid-gesture (a program takes the screen, another shell is chosen); no rest will be reported.
            .onDisappear { model.setScrollBusy(false) }
            // A moment after the lines above change (a page, a rebuild): still within a screen of the top, so the next page, for pages
            // that are small or follow a failed try. Not right away: the view is put back where it was only after the layout.
            .task(id: TopCheck(start: model.terminal.start, epoch: model.terminal.epoch)) {
                try? await Task.sleep(for: .milliseconds(350))
                guard !Task.isCancelled, scroll.phase == .idle, let metrics = scroll.metrics, metrics.distanceFromTop <= metrics.viewportHeight else { return }
                model.loadOlderHistory()
            }
            // Typing, sending and the menu command all bring the reader back to the prompt.
            .onChange(of: model.typedCount) { _, _ in jump(proxy) }
            .onChange(of: model.jumpRequests) { _, _ in jump(proxy) }
            .overlay(alignment: .bottomTrailing) {
                if let pill = model.scrollFollow.pill { pillButton(pill, proxy: proxy) }
            }
            .animation(.easeInOut(duration: 0.15), value: model.scrollFollow.pill)
        }
    }

    private static let blank = StyledLine(text: "", runs: [], columns: 0)

    /// "Loading…", "Beginning of history" and so on, always one row tall so it appearing changes nothing.
    private func historyRow(buffer: TerminalBuffer, height: Double) -> some View {
        let text: String = if model.historyLoading { "Loading…" }
            else if model.historyFailed { "Couldn't load older lines · tap to retry" }
            else if buffer.atTop { "Beginning of history" }
            else if buffer.limitReached { "Showing the last \(HistoryLimits.heldLines.formatted()) lines" }
            else { " " }
        return Text(text).font(style.mono(10, relativeTo: .caption2)).lineLimit(1)
            .foregroundStyle(style.terminalForeground.opacity(0.55))
            .frame(maxWidth: .infinity, minHeight: height, maxHeight: height, alignment: .leading)
            .contentShape(Rectangle())
            .onTapGesture { if model.historyFailed { model.retryHistory() } }
            .accessibilityAddTraits(model.historyFailed ? .isButton : [])
    }

    private func pillButton(_ pill: StickyBottom.Pill, proxy: ScrollViewProxy) -> some View {
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

    // MARK: Scroll events

    private struct TopCheck: Equatable { let start: Int, epoch: Int }

    nonisolated static func metrics(_ geometry: ScrollGeometry) -> ScrollMetrics {
        ScrollMetrics(offset: geometry.contentOffset.y, contentHeight: geometry.contentSize.height, viewportHeight: geometry.containerSize.height,
                      topInset: geometry.contentInsets.top, bottomInset: geometry.contentInsets.bottom)
    }

    private func scrolled(from old: ScrollMetrics, to new: ScrollMetrics, proxy: ScrollViewProxy) {
        scroll.metrics = new
        let phase = scroll.phase
        let userDriven = phase == .tracking || phase == .interacting || phase == .decelerating
        let response = model.scrollMetricsChanged(from: old, to: new, lineHeight: cell.height, userDriven: userDriven)
        if response == .scrollToBottom { toBottom(proxy) }
        // Within a screen of the top of what is loaded: the next older page. Not while the contents are merely growing: a page that has
        // just been put in leaves the reader where they were a moment later, and is no reason to ask for another.
        if new.distanceFromTop <= new.viewportHeight, userDriven || !new.resized(since: old) { model.loadOlderHistory() }
    }

    private func phaseChanged(_ phase: ScrollPhase) {
        scroll.phase = phase
        // Anything but rest: a finger, its momentum, or an animated scroll all own the offset, and only a view at rest keeps the line
        // being read where it is when lines are added above it.
        model.setScrollBusy(phase != .idle)
    }

    private func toBottom(_ proxy: ScrollViewProxy) {
        // At the bottom there is no line to hold on to; the next user scroll names one again.
        scroll.anchorID = nil
        proxy.scrollTo(Self.bottomMarker, anchor: .bottom)
    }

    /// The model has already decided to follow again; this is the scroll.
    private func jump(_ proxy: ScrollViewProxy) { toBottom(proxy) }

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
