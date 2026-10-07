import SwiftUI
import UIKit
import RiWorkCore

// How the chat screen is drawn, from the synced desktop theme: the colors it already has, plus the two a diff needs, taken from the
// desktop's terminal palette (green and red) so that +/- look like they do in the desktop's own terminal.

extension DesktopStyle {
    /// One of the terminal's 16 ANSI colors as a dynamic color, like the others.
    func ansiColor(_ index: Int) -> Color {
        guard theme.ansi.indices.contains(index) else { return text }
        let color = theme.ansi[index]
        let light = Self.uiColor(color.light)
        if color.isFixed { return Color(uiColor: light) }
        let dark = Self.uiColor(color.dark)
        return Color(uiColor: UIColor { $0.userInterfaceStyle == .dark ? dark : light })
    }
    /// Added lines and files.
    var added: Color { ansiColor(2) }
    /// Removed lines and files.
    var removed: Color { ansiColor(1) }
    /// Code and prose follow the same Dynamic Type curve, preserving their relative sizes.
    /// Code, commands and their output.
    var code: Font { mono(12, relativeTo: .body) }
    var codeSmall: Font { mono(11, relativeTo: .body) }
    /// The outline of a block of the transcript (a card, a code block, a diff, a bar): square in the terminal look, rounded as iOS rounds
    /// its grouped content in Native.
    func block(_ radius: CGFloat = 10) -> RoundedRectangle { RoundedRectangle(cornerRadius: native ? radius : 0, style: .continuous) }
    /// Links in a message: the accent color; in Native, whose accent is black or white like the text, the system's link blue.
    var link: Color { native ? Color(uiColor: .link) : accent }
    /// The band behind a card's header: the active color in the terminal look; none in Native, where the card's own fill is enough.
    var cardHeader: Color { native ? .clear : active }
}

extension ChatProvider {
    /// The glyph of a provider in tabs and in the New terminal sheet: the same ones the terminal kinds use, so Codex and Claude look alike
    /// wherever they are.
    var glyph: String { self == .codex ? "chevron.left.forwardslash.chevron.right" : "sparkles" }
}

extension View {
    /// What a bar that waits for the person (a request, a question) sits on, in its signal color: a band with a rule on top in the terminal
    /// look; in Native, a rounded tinted panel set in from the edges.
    @ViewBuilder func chatRequestSurface(_ style: DesktopStyle, tint: Color, opacity: Double) -> some View {
        if style.native {
            background(tint.opacity(opacity), in: style.block(16)).padding(.horizontal, 8).padding(.top, 6)
        } else {
            background(tint.opacity(opacity)).overlay(alignment: .top) { Rectangle().fill(tint).frame(height: 2) }
        }
    }
}

/// A card: a flat panel with a thin border, like the desktop's blocks. In Native, a rounded panel without the border, as iOS groups content.
struct ChatCard<Content: View>: View {
    @Environment(\.desktopStyle) private var style
    var tint: Color?
    @ViewBuilder var content: () -> Content
    var body: some View {
        let card = VStack(alignment: .leading, spacing: 0, content: content)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background((tint ?? style.panel).opacity(tint == nil ? 1 : 0.12))
        if style.native {
            card.clipShape(style.block()).overlay { if let tint { style.block().stroke(tint.opacity(0.5), lineWidth: 1) } }
        } else {
            card.overlay(Rectangle().stroke(tint ?? style.divider, lineWidth: 1))
        }
    }
}

/// A small label for the kind of thing a card is: "Input", "Result", "Plan". Written in sentence case; the terminal look shows capitals.
struct ChatCaption: View {
    @Environment(\.desktopStyle) private var style
    let text: String
    var body: some View {
        Text(style.cased(text)).font(style.system(.caption2, weight: .semibold)).foregroundStyle(style.muted).accessibilityHidden(true)
    }
}

/// Copies text and says so for a moment. The label stays one size so the layout does not move.
struct CopyButton: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let title: String
    let text: @MainActor () -> String
    /// Only the glyph, as in a card's header; the target stays 44 points.
    var iconOnly = false
    @State private var copied = false
    var body: some View {
        Button(action: copy) {
            Group {
                if iconOnly {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc").frame(minWidth: 44, minHeight: 44)
                } else {
                    Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc").labelStyle(.titleAndIcon)
                        .padding(.trailing, 8).frame(minWidth: 44, minHeight: 44, alignment: .trailing)
                }
            }
            .font(style.system(.caption)).foregroundStyle(copied ? style.accent : style.muted)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
        .chatLayoutProbe("copy-\(title)", action: copy)
    }
    private func copy() {
        UIPasteboard.general.string = text()
        copied = true
        Task { try? await Task.sleep(for: .seconds(1.5)); copied = false }
    }
}

/// Scrolling content that takes the room it needs up to `maxHeight`, and scrolls only past that. (A bare `ScrollView` is flexible, and
/// next to the transcript, which is too, it is given next to no room.)
///
/// In Native, content that does not fit is cut between two of its rows (those marked `boundedScrollBreak()`), never through one, and the
/// scroll indicator flashes to say there is more.
struct BoundedScroll<Content: View>: View {
    @Environment(\.desktopStyle) private var style
    let maxHeight: CGFloat
    @ViewBuilder var content: () -> Content
    @State private var height: CGFloat = 0
    @State private var breaks: [CGFloat] = []
    var body: some View {
        ScrollView {
            content()
                .coordinateSpace(.named(BoundedScrollBreaks.space))
                .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height = $0 }
                .onPreferenceChange(BoundedScrollBreaks.self) { breaks = $0 }
        }
        .scrollBounceBehavior(.basedOnSize)
        .scrollIndicatorsFlash(trigger: style.native ? shown : 0)
        // Nothing until the content is measured: a bare scroll view would first take half the room from the transcript beside it, and
        // throw the transcript off its bottom while it settles.
        .frame(height: shown)
    }
    private var shown: CGFloat {
        BoundedScrollBreaks.visibleHeight(content: height, maxHeight: maxHeight, breaks: style.native ? breaks : [])
    }
}

/// Where a `BoundedScroll`'s content may be cut: the bottoms of its rows, in the content's own coordinates.
struct BoundedScrollBreaks: PreferenceKey {
    static let space = "bounded-scroll"
    static let defaultValue: [CGFloat] = []
    static func reduce(value: inout [CGFloat], nextValue: () -> [CGFloat]) { value += nextValue() }
    /// The height shown: all of the content when it fits; else the room, cut at the lowest row bottom inside it unless that would give
    /// up more than half of it (then a row is cut, as before).
    static func visibleHeight(content: CGFloat, maxHeight: CGFloat, breaks: [CGFloat]) -> CGFloat {
        guard content > maxHeight else { return content }
        guard let cut = breaks.filter({ $0 <= maxHeight }).max(), cut >= maxHeight / 2 else { return maxHeight }
        return cut
    }
}

extension View {
    /// Marks this row's bottom as a place where a `BoundedScroll` that cannot show everything may end.
    func boundedScrollBreak() -> some View {
        background {
            GeometryReader { proxy in
                // The outline drawn on a row's edge reaches a point past it.
                Color.clear.preference(key: BoundedScrollBreaks.self, value: [proxy.frame(in: .named(BoundedScrollBreaks.space)).maxY + 1])
            }
        }
    }
}

// Hosted tests can inspect measured SwiftUI controls without depending on private accessibility APIs.
// The observer is absent by default and the modifier is inert in release builds.
#if DEBUG
@MainActor final class ChatLayoutInspection {
    var frames: [String: CGRect] = [:]
    var visible: [String: Bool] = [:]
    var actions: [String: () -> Void] = [:]
}
private struct ChatLayoutInspectionKey: EnvironmentKey {
    static let defaultValue: ChatLayoutInspection? = nil
}
extension EnvironmentValues {
    var chatLayoutInspection: ChatLayoutInspection? {
        get { self[ChatLayoutInspectionKey.self] }
        set { self[ChatLayoutInspectionKey.self] = newValue }
    }
}
private struct ChatLayoutProbe: ViewModifier {
    @Environment(\.chatLayoutInspection) private var inspection
    let name: String
    let visible: Bool
    let action: (() -> Void)?
    func body(content: Content) -> some View {
        if let inspection {
            content.onGeometryChange(for: CGRect.self) { $0.frame(in: .global) } action: { frame in
                inspection.frames[name] = frame
                inspection.actions[name] = action
            }
            .onChange(of: visible, initial: true) { _, value in inspection.visible[name] = value }
            .onDisappear { inspection.frames[name] = nil; inspection.visible[name] = nil; inspection.actions[name] = nil }
        } else { content }
    }
}
#endif
extension View {
    @ViewBuilder func chatLayoutProbe(_ name: String, visible: Bool = true, action: (() -> Void)? = nil) -> some View {
        #if DEBUG
        modifier(ChatLayoutProbe(name: name, visible: visible, action: action))
        #else
        self
        #endif
    }
}

private struct ChatProseFont: ViewModifier {
    @Environment(\.desktopStyle) private var style
    @ScaledMetric(relativeTo: .body) private var size: CGFloat = 17
    func body(content: Content) -> some View { content.font(.system(size: size * CGFloat(style.scale))) }
}
extension View {
    /// Prose keeps the system face and follows the same uncapped body curve as code.
    func chatProse() -> some View { modifier(ChatProseFont()) }
}
