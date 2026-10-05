import SwiftUI
import RiWorkCore

/// An agent's message, drawn from `ChatMarkdown` blocks: paragraphs, headings, lists, quotes, tables and code, with the inline syntax
/// (bold, italic, `code`, links) done by `AttributedString`. Text can be selected; a code block has its own Copy.
struct ChatMarkdownView: View, Equatable {
    @Environment(\.desktopStyle) private var style
    let source: String
    nonisolated static func == (lhs: ChatMarkdownView, rhs: ChatMarkdownView) -> Bool { lhs.source == rhs.source }

    var body: some View {
        MarkdownBlocks(blocks: ChatMarkdown.parse(source), depth: 0)
            .frame(maxWidth: .infinity, alignment: .leading).tint(style.link)
    }
}

private struct MarkdownBlocks: View {
    let blocks: [ChatMarkdownBlock]
    let depth: Int
    var body: some View {
        VStack(alignment: .leading, spacing: depth == 0 ? 14 : 6) {
            ForEach(blocks.indices, id: \.self) { MarkdownBlockView(block: blocks[$0], depth: depth) }
        }
    }
}

private extension EnvironmentValues {
    /// Inside a quote the text is the muted one.
    @Entry var markdownMuted = false
}

private struct MarkdownBlockView: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.markdownMuted) private var muted
    let block: ChatMarkdownBlock
    let depth: Int

    var body: some View {
        switch block {
        case .heading(let level, let text):
            Text(ChatMarkdown.inline(text)).font(headingFont(level)).foregroundStyle(style.text)
                .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityAddTraits(.isHeader).accessibilityHeading(level <= 1 ? .h1 : level == 2 ? .h2 : .h3)
        case .paragraph(let text):
            Text(ChatMarkdown.inline(text)).font(style.prose).lineSpacing(3).foregroundStyle(muted ? style.muted : style.text).tint(style.link)
                .textSelection(.enabled).fixedSize(horizontal: false, vertical: true).frame(maxWidth: .infinity, alignment: .leading)
        case .code(let language, let text, _):
            CodeBlock(language: language, text: text)
        case .list(let ordered, let start, let items):
            VStack(alignment: .leading, spacing: 6) {
                ForEach(items.indices, id: \.self) { index in
                    ListRow(marker: marker(ordered: ordered, number: start + index, item: items[index]), item: items[index], depth: depth)
                }
            }
        case .quote(let blocks):
            HStack(alignment: .top, spacing: 8) {
                style.block(1.5).fill(style.divider).frame(width: 3)
                MarkdownBlocks(blocks: blocks, depth: depth + 1).environment(\.markdownMuted, true)
            }.fixedSize(horizontal: false, vertical: true)
        case .rule:
            DesktopRule()
        case .table(let header, let rows):
            TableBlock(header: header, rows: rows)
        }
    }

    private func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: style.system(.title3, weight: .bold)
        case 2: style.system(.headline, weight: .semibold)
        default: style.system(.callout, weight: .semibold)
        }
    }
    private func marker(ordered: Bool, number: Int, item: ChatMarkdownListItem) -> ListMarker {
        if let checked = item.checked { return .check(checked) }
        return ordered ? .number(number) : .bullet
    }
}

private enum ListMarker { case bullet, number(Int), check(Bool) }

private struct ListRow: View {
    @Environment(\.desktopStyle) private var style
    let marker: ListMarker
    let item: ChatMarkdownListItem
    let depth: Int
    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Group {
                switch marker {
                case .bullet: Text("•")
                case .number(let n): Text("\(n).").monospacedDigit()
                case .check(let done): Image(systemName: done ? "checkmark.square" : "square").accessibilityLabel(done ? "Done" : "Not done")
                }
            }
            .font(style.prose).foregroundStyle(style.muted).frame(minWidth: style.pt(16), alignment: .trailing)
            MarkdownBlocks(blocks: item.blocks, depth: depth + 1)
        }
    }
}

private struct CodeBlock: View {
    @Environment(\.desktopStyle) private var style
    let language: String?
    let text: String
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 4) {
                Text(language ?? "code").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1)
                Spacer(minLength: 4)
                CopyButton(title: "Copy code", text: { text })
            }
            .padding(.leading, 12).background(style.cardHeader)
            ScrollView(.horizontal, showsIndicators: false) {
                Text(verbatim: text).font(style.code).foregroundStyle(style.text).textSelection(.enabled)
                    .fixedSize(horizontal: true, vertical: true).padding(12)
            }
        }
        .background(style.panel).clipShape(style.block(14))
        .overlay { if !style.native { Rectangle().stroke(style.divider, lineWidth: 1) } }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(language ?? "Code") block")
    }
}

private struct TableBlock: View {
    @Environment(\.desktopStyle) private var style
    let header: [String]
    let rows: [[String]]
    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 4) {
                GridRow { ForEach(header.indices, id: \.self) { Text(ChatMarkdown.inline(header[$0])).font(style.system(.footnote, weight: .bold)) } }
                Divider().gridCellUnsizedAxes(.horizontal)
                ForEach(rows.indices, id: \.self) { row in
                    GridRow { ForEach(rows[row].indices, id: \.self) { Text(ChatMarkdown.inline(rows[row][$0])).font(style.system(.footnote)) } }
                }
            }
            .foregroundStyle(style.text).textSelection(.enabled).padding(8)
        }
        .background(style.panel).clipShape(style.block(14))
        .overlay { if !style.native { Rectangle().stroke(style.divider, lineWidth: 1) } }
    }
}
