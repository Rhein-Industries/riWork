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
    /// Text for prose (messages, questions): the system face, which reads better in paragraphs than Menlo; Menlo stays for code and chrome.
    var prose: Font { system(.callout) }
    /// Code, commands and their output.
    var code: Font { mono(12, relativeTo: .footnote) }
    var codeSmall: Font { mono(11, relativeTo: .caption) }
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
        Text(style.cased(text)).font(style.face(9, bold: true, relativeTo: .caption2)).foregroundStyle(style.muted).accessibilityHidden(true)
    }
}

/// Copies text and says so for a moment. The label stays one size so the layout does not move.
struct CopyButton: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let title: String
    let text: @MainActor () -> String
    @State private var copied = false
    var body: some View {
        Button {
            UIPasteboard.general.string = text()
            copied = true
            Task { try? await Task.sleep(for: .seconds(1.5)); copied = false }
        } label: {
            Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc").labelStyle(.titleAndIcon)
                .font(style.face(10, relativeTo: .caption2)).foregroundStyle(copied ? style.accent : style.muted)
                .padding(.trailing, 8)
                .frame(minWidth: style.pt(44), minHeight: style.pt(28), alignment: .trailing)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
    }
}

/// Scrolling content that takes the room it needs up to `maxHeight`, and scrolls only past that. (A bare `ScrollView` is flexible, and
/// next to the transcript, which is too, it is given next to no room.)
struct BoundedScroll<Content: View>: View {
    let maxHeight: CGFloat
    @ViewBuilder var content: () -> Content
    @State private var height: CGFloat = 0
    var body: some View {
        ScrollView {
            content().onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height = $0 }
        }
        .scrollBounceBehavior(.basedOnSize)
        .frame(height: height > 0 ? min(height, maxHeight) : nil)
    }
}
