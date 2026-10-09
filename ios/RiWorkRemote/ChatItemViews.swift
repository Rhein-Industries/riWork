import SwiftUI
import UIKit
import RiWorkCore

// One row of the transcript for each kind of item. A row is `Equatable` on its item and on which of its parts are open, so a message
// that streams rebuilds only itself, and cards that scroll out and back in come back as they were.

/// The state of an item that ran: running, done, failed, declined or interrupted. One shape for each, so color is never the only difference.
struct ChatStatusGlyph: View {
    @Environment(\.desktopStyle) private var style
    let status: ChatItemStatus
    /// A finished command that exited with an error counts as failed whatever the provider called the item.
    var errored = false
    var body: some View {
        Group {
            if errored && status != .inProgress {
                Image(systemName: "xmark.circle.fill").foregroundStyle(style.error)
            } else {
                switch status {
                case .inProgress: ActivityIndicator(activity: .working)
                case .completed: Image(systemName: "checkmark").foregroundStyle(style.muted)
                case .failed: Image(systemName: "xmark.circle.fill").foregroundStyle(style.error)
                case .declined: Image(systemName: "hand.raised.fill").foregroundStyle(style.gold)
                case .interrupted: Image(systemName: "stop.circle").foregroundStyle(style.muted)
                }
            }
        }
        .font(style.system(.caption, weight: .semibold)).frame(width: style.pt(20), height: style.pt(20))
        .accessibilityHidden(true)
    }
}

struct ChatItemRow: View, Equatable {
    @Environment(\.desktopStyle) private var style
    let item: ChatItem
    let provider: ChatProvider
    /// The ids of the parts of this item that are open: the item's own id, or "id#path" for a file of an edit.
    let open: Set<String>
    let toggle: @MainActor (String) -> Void

    nonisolated static func == (lhs: ChatItemRow, rhs: ChatItemRow) -> Bool { lhs.item == rhs.item && lhs.provider == rhs.provider && lhs.open == rhs.open }

    /// The transcript's text margin: where its messages begin (and the composer's cards).
    static let horizontalInset: CGFloat = 16
    var body: some View {
        content
            .padding(.horizontal, Self.horizontalInset).padding(.vertical, rowSpacing)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var rowSpacing: CGFloat {
        switch item.body {
        case .userMessage, .agentMessage: 12
        default: 3
        }
    }

    @ViewBuilder private var content: some View {
        switch item.body {
        case .userMessage(let text): UserBlock(text: text)
        case .agentMessage(let text):
            ChatMarkdownView(source: text).equatable()
                .accessibilityLabel("\(provider.title) said")
                .accessibilityValue(text)
        case .reasoning(let text): ReasoningBlock(item: item, text: text, isOpen: open.contains(item.id), toggle: { toggle(item.id) })
        case .plan(let explanation, let steps): ChecklistCard(title: "Plan", icon: "list.bullet.clipboard", explanation: explanation, steps: steps)
        case .todo(let items): ChecklistCard(title: "To-do", icon: "checklist", explanation: nil, steps: items)
        case .command(let command, let cwd, let output, let exitCode):
            CommandCard(item: item, command: command, cwd: cwd, output: output, exitCode: exitCode, isOpen: open.contains(item.id), toggle: { toggle(item.id) })
        case .fileChange(let changes): FileChangeCard(item: item, changes: changes, open: open, toggle: toggle)
        case .toolCall(let server, let tool, let input, let output):
            ToolCard(item: item, server: server, tool: tool, input: input, output: output, isOpen: open.contains(item.id), toggle: { toggle(item.id) })
        case .webSearch(let query):
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(style.muted)
                Text(query).font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted).lineLimit(2)
                Spacer(minLength: 0)
                ChatStatusGlyph(status: item.status)
            }
            .accessibilityElement(children: .combine).accessibilityLabel("Web search: \(query)")
        case .compaction:
            HStack(spacing: 8) {
                DesktopRule()
                Text("Context compacted").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted).fixedSize()
                DesktopRule()
            }
            .accessibilityElement(children: .combine).accessibilityLabel("Context compacted")
        case .notice(let level, let text, _, _, _, _): NoticeBlock(level: level, text: text)
        case .elided: if let elision = ChatElision(item.body) { ElidedBlock(elision: elision, isOpen: open.contains(item.id), toggle: { toggle(item.id) }) }
        }
    }
}

// MARK: - Messages

/// What the person sent: an accent block. In Native, a bubble on the trailing side, as a chat on iOS has it.
private struct UserBlock: View {
    @Environment(\.desktopStyle) private var style
    let text: String
    @Environment(\.dynamicTypeSize) private var typeSize
    var body: some View {
        HStack {
            Spacer(minLength: typeSize.isAccessibilitySize ? 16 : 40)
            Text(text).chatProse().foregroundStyle(style.text).textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.vertical, 12).padding(.horizontal, 16)
                .background(style.native ? style.active : style.accent.opacity(0.12), in: style.block(18))
                .accessibilityLabel("You said").accessibilityValue(text)
        }
    }

}

/// The agent's reasoning: one collapsed line, opened on a tap.
private struct ReasoningBlock: View {
    @Environment(\.desktopStyle) private var style
    let item: ChatItem
    let text: String
    let isOpen: Bool
    let toggle: () -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button(action: toggle) {
                HStack(spacing: 6) {
                    if item.status == .inProgress { ActivityIndicator(activity: .working) } else { Image(systemName: "brain").font(style.system(.caption)) }
                    Text(item.status == .inProgress ? "Thinking…" : "Thought").font(style.face(11, relativeTo: .caption))
                    Image(systemName: "chevron.right").font(style.system(.caption2, weight: .semibold)).rotationEffect(.degrees(isOpen ? 90 : 0))
                    Spacer(minLength: 0)
                }
                .foregroundStyle(style.muted).frame(minHeight: 44).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(item.status == .inProgress ? "Thinking" : "Thought")
            .accessibilityValue(isOpen ? "Shown" : "Hidden")
            .accessibilityHint(isOpen ? "Hides the reasoning" : "Shows the reasoning")
            .accessibilityAddTraits(.isButton)
            if isOpen {
                Text(text.isEmpty ? "Nothing to show yet." : text).font(style.system(.footnote)).italic().foregroundStyle(style.muted)
                    .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                    .padding(.leading, 10).overlay(alignment: .leading) { style.block(1).fill(style.divider).frame(width: 2) }
            }
        }
    }
}

private struct NoticeBlock: View {
    @Environment(\.desktopStyle) private var style
    let level: ChatNoticeLevel
    let text: String
    private var color: Color { level == .error ? style.error : (level == .warning ? style.gold : style.muted) }
    private var icon: String { level == .error ? "xmark.octagon.fill" : (level == .warning ? "exclamationmark.triangle.fill" : "info.circle") }
    private var word: String { level == .error ? "Error" : (level == .warning ? "Warning" : "Note") }
    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: icon).foregroundStyle(color).accessibilityHidden(true)
            Text(text).font(style.system(.footnote)).foregroundStyle(level == .info ? style.muted : style.text).textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(12).background(color.opacity(level == .info ? 0 : 0.12), in: style.block(10))
        .accessibilityElement(children: .combine).accessibilityLabel("\(word): \(text)")
    }
}

/// Content the relay left out as too large: one compact row ("Shown on your Mac · command · 1.9 MB", or a note for another event) that
/// a tap opens to say why.
private struct ElidedBlock: View {
    @Environment(\.desktopStyle) private var style
    let elision: ChatElision
    let isOpen: Bool
    let toggle: () -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button(action: toggle) {
                HStack(spacing: 6) {
                    Image(systemName: elision.isNote ? "exclamationmark.triangle" : "macbook").font(style.system(.caption))
                        .foregroundStyle(elision.isNote ? style.gold : style.muted)
                    Text(elision.line).font(style.face(11, relativeTo: .caption)).lineLimit(1)
                    Image(systemName: "info.circle").font(style.system(.caption2))
                    Spacer(minLength: 0)
                }
                .foregroundStyle(style.muted).frame(minHeight: 44).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(elision.line)
            .accessibilityHint(isOpen ? "Hides why" : "Explains why it is not shown here")
            .accessibilityIdentifier("chat.elided")
            if isOpen {
                Text(ChatElision.explanation).font(style.system(.footnote)).foregroundStyle(style.muted)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.leading, 10).overlay(alignment: .leading) { style.block(1).fill(style.divider).frame(width: 2) }
            }
        }
    }
}

// MARK: - Plan and to-do

private struct ChecklistCard: View {
    @Environment(\.desktopStyle) private var style
    let title: String
    let icon: String
    let explanation: String?
    let steps: [ChatStep]
    private var done: Int { steps.filter { $0.status == .completed }.count }
    var body: some View {
        ChatCard {
            HStack(spacing: 6) {
                Image(systemName: icon).foregroundStyle(style.accent).accessibilityHidden(true)
                ChatCaption(text: title)
                Spacer(minLength: 0)
                if !steps.isEmpty { Text("\(done)/\(steps.count)").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted).monospacedDigit() }
            }
            .padding(.horizontal, 10).padding(.vertical, 8).background(style.cardHeader)
            VStack(alignment: .leading, spacing: 6) {
                if let explanation, !explanation.isEmpty {
                    Text(explanation).font(style.system(.footnote)).foregroundStyle(style.muted).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                }
                ForEach(steps.indices, id: \.self) { StepRow(step: steps[$0]) }
                if steps.isEmpty { Text("No steps yet.").font(style.system(.footnote)).foregroundStyle(style.muted) }
            }
            .padding(.horizontal, 10).padding(.top, 2).padding(.bottom, 10)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(title.capitalized), \(done) of \(steps.count) done")
    }
}

private struct StepRow: View {
    @Environment(\.desktopStyle) private var style
    let step: ChatStep
    private var icon: String {
        switch step.status {
        case .pending: "circle"
        case .inProgress: "arrow.right.circle.fill"
        case .completed: "checkmark.circle.fill"
        }
    }
    private var word: String { step.status == .completed ? "done" : (step.status == .inProgress ? "in progress" : "to do") }
    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: icon).foregroundStyle(step.status == .pending ? style.muted : style.accent).accessibilityHidden(true)
            Text(step.text).font(style.system(.footnote, weight: step.status == .inProgress ? .semibold : .regular))
                .foregroundStyle(step.status == .completed ? style.muted : style.text).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine).accessibilityLabel("\(step.text), \(word)")
    }
}

// MARK: - Cards

/// The one header row of a collapsible card: the status, what it is (bold), what it was about (muted, after it on the same line, cut to
/// one line while the card is closed), anything else worth a glance (`trailing`: counts, exit code, Copy while open) and one chevron.
/// A closed card is this row alone.
private struct CardHeader<Trailing: View>: View {
    @Environment(\.desktopStyle) private var style
    let isOpen: Bool
    let label: String
    let status: ChatItemStatus
    var errored = false
    let title: String
    var titleFont: Font?
    var subtitle: String = ""
    var subtitleFont: Font?
    /// How the subtitle is cut: a path keeps its end, a command its start.
    var truncation: Text.TruncationMode = .tail
    let toggle: () -> Void
    @ViewBuilder var trailing: () -> Trailing
    var body: some View {
        HStack(alignment: .center, spacing: 6) {
            Button(action: toggle) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    ChatStatusGlyph(status: status, errored: errored).alignmentGuide(.firstTextBaseline) { $0[VerticalAlignment.center] + 4 }
                    // The title keeps its room before the subtitle does, and is cut itself only when it alone is too long (a long
                    // server and tool name): nothing in the row ever pushes it wider than the card.
                    Text(title).font(titleFont ?? style.system(.subheadline, weight: .semibold)).foregroundStyle(style.text)
                        .lineLimit(isOpen ? 6 : 1).truncationMode(.tail).layoutPriority(1)
                    if !subtitle.isEmpty {
                        Text(subtitle).font(subtitleFont ?? style.codeSmall).foregroundStyle(style.muted)
                            .lineLimit(isOpen ? 4 : 1).truncationMode(truncation).multilineTextAlignment(.leading)
                            .frame(minWidth: style.pt(48), alignment: .leading)
                    }
                    Spacer(minLength: 0)
                }
                .frame(minHeight: style.target).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(label)
            .accessibilityValue(isOpen ? "Expanded" : "Collapsed")
            .accessibilityHint(isOpen ? "Collapses the details" : "Shows the details")
            .accessibilityAddTraits(.isButton)
            trailing()
            Button(action: toggle) {
                Image(systemName: "chevron.right").font(style.system(.caption2, weight: .semibold)).foregroundStyle(style.muted)
                    .rotationEffect(.degrees(isOpen ? 90 : 0)).frame(minWidth: style.target, minHeight: style.target).contentShape(Rectangle())
            }
            .buttonStyle(.plain).accessibilityHidden(true)
        }
        .padding(.leading, 10)
        .background(style.cardHeader)
        .chatLayoutProbe("card-header-\(label.prefix(24))")
    }
}
extension CardHeader where Trailing == EmptyView {
    init(isOpen: Bool, label: String, status: ChatItemStatus, errored: Bool = false, title: String, titleFont: Font? = nil, subtitle: String = "",
         subtitleFont: Font? = nil, truncation: Text.TruncationMode = .tail, toggle: @escaping () -> Void) {
        self.init(isOpen: isOpen, label: label, status: status, errored: errored, title: title, titleFont: titleFont, subtitle: subtitle,
                  subtitleFont: subtitleFont, truncation: truncation, toggle: toggle) { EmptyView() }
    }
}

/// The body of an open card: tight insets, set off from the header by a hairline.
private struct CardBody<Content: View>: View {
    @Environment(\.desktopStyle) private var style
    var spacing: CGFloat = 6
    @ViewBuilder var content: () -> Content
    var body: some View {
        VStack(alignment: .leading, spacing: spacing, content: content)
            .padding(.horizontal, 10).padding(.top, 6).padding(.bottom, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .overlay(alignment: .top) { DesktopRule().opacity(0.6) }
    }
}

/// Monospaced output, bounded: the end of it, and what was left out.
private struct OutputText: View {
    @Environment(\.desktopStyle) private var style
    let text: String
    @State private var more = false
    var body: some View {
        let preview = ChatOutput.tail(text, lines: 8)
        let tail = more ? ChatOutput.tail(text, lines: 80) : preview
        VStack(alignment: .leading, spacing: 4) {
            if tail.hidden > 0 { Text("… \(tail.hidden) earlier \(tail.hidden == 1 ? "line" : "lines") not shown").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted) }
            if tail.text.isEmpty { Text("No output.").font(style.face(11, relativeTo: .caption)).foregroundStyle(style.muted) }
            else {
                ScrollView(.horizontal, showsIndicators: false) {
                    Text(verbatim: tail.text).font(style.codeSmall).foregroundStyle(style.text).textSelection(.enabled).fixedSize(horizontal: true, vertical: true)
                }
            }
            if more || preview.hidden > 0 {
                Button(more ? "Show less output" : "Show more output") { more.toggle() }
                    .font(style.system(.footnote)).frame(minHeight: 44).contentShape(Rectangle())
                    .buttonStyle(.plain).foregroundStyle(style.link)
                    .accessibilityIdentifier("chat-tool-output-toggle")
                    .chatLayoutProbe("output-toggle", action: { more.toggle() })
            }
        }
    }
}

private struct CommandCard: View {
    @Environment(\.desktopStyle) private var style
    let item: ChatItem
    let command: String
    let cwd: String?
    let output: String
    let exitCode: Int?
    let isOpen: Bool
    let toggle: () -> Void
    private var errored: Bool { (exitCode ?? 0) != 0 }
    var body: some View {
        ChatCard {
            CardHeader(isOpen: isOpen, label: "\(ChatItemBody.command(command: command, cwd: cwd, output: "", exitCode: exitCode).summary), \(item.status.spoken)",
                       status: item.status, errored: errored, title: "$ \(command)", titleFont: style.code, toggle: toggle) {
                if let exitCode, exitCode != 0 { Text("exit \(exitCode)").font(style.face(10, bold: true, relativeTo: .caption2)).foregroundStyle(style.error).fixedSize() }
                if isOpen, !output.isEmpty { CopyButton(title: "Copy output", text: { output }, iconOnly: true) }
            }
            if isOpen {
                CardBody(spacing: 4) {
                    if let cwd, !cwd.isEmpty { Text(cwd).font(style.mono(10, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1).truncationMode(.head) }
                    OutputText(text: output)
                }
            } else if item.status == .inProgress, !output.isEmpty {
                // Output as it arrives, three lines of it, without opening the card.
                Text(verbatim: ChatOutput.tail(output, lines: 3).text).font(style.codeSmall).foregroundStyle(style.muted).lineLimit(3)
                    .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 10).padding(.bottom, 6).accessibilityHidden(true)
            }
        }
    }
}

private struct ToolCard: View {
    @Environment(\.desktopStyle) private var style
    let item: ChatItem
    let server: String?
    let tool: String
    let input: JSONValue
    let output: String?
    let isOpen: Bool
    let toggle: () -> Void
    var body: some View {
        ChatCard {
            CardHeader(isOpen: isOpen, label: "\(item.body.summary), \(item.status.spoken)", status: item.status,
                       title: [server, tool].compactMap { $0 }.joined(separator: " · "), subtitle: ChatToolSummary.line(for: input), truncation: .head, toggle: toggle) {
                if isOpen, let output, !output.isEmpty { CopyButton(title: "Copy result", text: { output }, iconOnly: true) }
            }
            if isOpen {
                CardBody {
                    if input != .null {
                        ChatCaption(text: "Input")
                        Text(verbatim: ChatToolSummary.pretty(input)).font(style.codeSmall).foregroundStyle(style.text).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                    }
                    if let output, !output.isEmpty {
                        ChatCaption(text: "Result").padding(.top, 2)
                        OutputText(text: output)
                    }
                }
            }
        }
    }
}

// MARK: - Edits

/// Parsed diffs, kept: a card that scrolls out of the list and back is not parsed again, and neither is the same diff in the approval bar.
@MainActor enum DiffCache {
    private static let cache: NSCache<NSString, Box> = { let cache = NSCache<NSString, Box>(); cache.countLimit = 64; return cache }()
    private final class Box { let diff: ChatDiff; init(_ diff: ChatDiff) { self.diff = diff } }
    static func parse(_ text: String) -> ChatDiff {
        let key = "\(text.utf8.count):\(text.hashValue)" as NSString
        if let hit = cache.object(forKey: key) { return hit.diff }
        let diff = ChatDiff.parse(text)
        cache.setObject(Box(diff), forKey: key)
        return diff
    }
}

private struct FileChangeCard: View {
    @Environment(\.desktopStyle) private var style
    let item: ChatItem
    let changes: [ChatFileChange]
    let open: Set<String>
    let toggle: @MainActor (String) -> Void
    private var counts: (added: Int, removed: Int) {
        changes.reduce(into: (0, 0)) { total, change in
            if let diff = change.diff { let parsed = DiffCache.parse(diff); total.0 += parsed.added; total.1 += parsed.removed }
        }
    }
    private var subtitle: String {
        guard let first = changes.first else { return "" }
        return first.path + (changes.count > 1 ? " and \(changes.count - 1) more" : "")
    }
    var body: some View {
        let total = counts
        let isOpen = open.contains(item.id)
        // One file: the card is the file, and opening it shows its diff. Several: each file is a row of its own that opens its diff.
        let single = changes.count == 1 ? changes.first : nil
        let verb = single.map { FileRow.verb($0.kind) } ?? "Edit"
        ChatCard {
            CardHeader(isOpen: isOpen, label: "\(single.map { "\(FileRow.word($0.kind)) \($0.path)" } ?? item.body.summary), \(item.status.spoken), \(total.added) added, \(total.removed) removed",
                       status: item.status, title: single == nil ? "Edit · \(changes.count) files" : verb, subtitle: isOpen && single == nil ? "" : subtitle, truncation: .head,
                       toggle: { toggle(item.id) }) {
                if total.added + total.removed > 0 {
                    HStack(spacing: 4) {
                        Text("+\(total.added)").foregroundStyle(style.added)
                        Text("−\(total.removed)").foregroundStyle(style.removed)
                    }
                    .font(style.face(11, bold: true, relativeTo: .caption)).monospacedDigit().fixedSize()
                }
                if isOpen, let diff = single?.diff { CopyButton(title: "Copy diff", text: { diff }, iconOnly: true) }
            }
            if isOpen {
                if let single {
                    if let diff = single.diff { DiffView(text: diff, flush: true) }
                    else { CardBody { Text("\(FileRow.word(single.kind)) \(single.path). No diff was sent.").font(style.system(.footnote)).foregroundStyle(style.muted) } }
                } else {
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(changes.indices, id: \.self) { index in
                            FileRow(change: changes[index], isOpen: open.contains("\(item.id)#\(changes[index].path)"),
                                    toggle: { toggle("\(item.id)#\(changes[index].path)") })
                        }
                    }
                    .overlay(alignment: .top) { DesktopRule().opacity(0.6) }
                }
            }
        }
    }
}

/// One file of an edit of several: its change, its path and, opened, its diff flush with the card.
private struct FileRow: View {
    @Environment(\.desktopStyle) private var style
    let change: ChatFileChange
    let isOpen: Bool
    let toggle: () -> Void
    private var icon: String {
        switch change.kind {
        case .add: "plus.circle"
        case .modify: "pencil.circle"
        case .delete: "minus.circle"
        case .rename: "arrow.right.circle"
        }
    }
    private var color: Color { change.kind == .add ? style.added : (change.kind == .delete ? style.removed : style.muted) }
    private var word: String { Self.word(change.kind) }
    static func word(_ kind: ChatChangeKind) -> String { kind == .add ? "Added" : (kind == .delete ? "Deleted" : (kind == .rename ? "Renamed" : "Edited")) }
    /// The title of a one-file edit's card: what was done to the file.
    static func verb(_ kind: ChatChangeKind) -> String {
        switch kind {
        case .add: "New file"
        case .modify: "Edit"
        case .delete: "Delete"
        case .rename: "Rename"
        }
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                Button(action: { if change.diff != nil { toggle() } }) {
                    HStack(spacing: 6) {
                        Image(systemName: icon).font(style.system(.caption)).foregroundStyle(color).accessibilityHidden(true)
                        Text(change.path).font(style.codeSmall).foregroundStyle(style.text).lineLimit(1).truncationMode(.head)
                        Spacer(minLength: 4)
                        if change.diff != nil {
                            Image(systemName: "chevron.right").font(style.system(.caption2, weight: .semibold)).foregroundStyle(style.muted).rotationEffect(.degrees(isOpen ? 90 : 0))
                        }
                    }
                    .padding(.leading, 12).padding(.trailing, 10)
                    .frame(minHeight: style.target).contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("\(word) \(change.path)")
                .accessibilityValue(change.diff == nil ? "" : (isOpen ? "Diff shown" : "Diff hidden"))
                .accessibilityHint(change.diff == nil ? "" : (isOpen ? "Hides the diff" : "Shows the diff"))
                if isOpen, let diff = change.diff { CopyButton(title: "Copy diff", text: { diff }, iconOnly: true).padding(.trailing, 4) }
            }
            if isOpen, let text = change.diff { DiffView(text: text, flush: true) }
        }
    }
}

/// A unified diff in monospace: added lines green, removed red, hunk headers muted. Lines do not wrap (the whole diff scrolls sideways),
/// so a line is a line. In a card it is `flush`: edge to edge under the header, on the content's background, with no frame of its own
/// (the card's rounding is the only one); elsewhere (the approval bar) it is a framed block. Copy is the card's (or a long press).
struct DiffView: View {
    @Environment(\.desktopStyle) private var style
    let text: String
    var flush = false
    /// The width of the frame the diff sits in, so that the bands of added and removed lines reach across it, not just across their text.
    @State private var width: CGFloat = 0
    var body: some View {
        let diff = DiffCache.parse(text)
        VStack(alignment: .leading, spacing: 0) {
            let lines = ScrollView(.horizontal, showsIndicators: false) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(diff.lines.indices, id: \.self) { index in DiffLineView(line: diff.lines[index]) }
                }
                .padding(.vertical, 4)
                .frame(minWidth: width, alignment: .leading)
            }
            .background(style.background)
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
            .contextMenu { Button("Copy diff", systemImage: "doc.on.doc") { UIPasteboard.general.string = text } }
            if flush { lines } else { lines.clipShape(style.block(10)).overlay(style.block(10).stroke(style.divider, lineWidth: 1)) }
            if diff.hiddenLines > 0 {
                Text("… \(diff.hiddenLines) more lines not shown").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted)
                    .padding(.horizontal, flush ? 10 : 0).padding(.vertical, 4)
            }
        }
    }
}

private struct DiffLineView: View {
    @Environment(\.desktopStyle) private var style
    let line: ChatDiffLine
    private var marker: String {
        switch line.kind {
        case .added: "+"
        case .removed: "−"
        default: " "
        }
    }
    private var fill: Color {
        switch line.kind {
        case .added: style.added.opacity(0.16)
        case .removed: style.removed.opacity(0.16)
        case .hunk: style.accent.opacity(0.08)
        default: .clear
        }
    }
    private var markerColor: Color { line.kind == .added ? style.added : (line.kind == .removed ? style.removed : style.muted) }
    var body: some View {
        HStack(spacing: 0) {
            Text(marker).foregroundStyle(markerColor).frame(width: style.pt(14))
            Text(verbatim: line.text.isEmpty ? " " : line.text).foregroundStyle(line.kind == .hunk || line.kind == .meta ? style.muted : style.text)
                .fixedSize(horizontal: true, vertical: false)
            Spacer(minLength: 0)
        }
        .font(style.codeSmall).textSelection(.enabled)
        .padding(.horizontal, 4).background(fill)
        .accessibilityLabel(line.kind == .added ? "Added: \(line.text)" : (line.kind == .removed ? "Removed: \(line.text)" : line.text))
    }
}
