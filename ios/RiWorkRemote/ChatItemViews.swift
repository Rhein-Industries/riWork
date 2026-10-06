import SwiftUI
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

    var body: some View {
        content
            .padding(.horizontal, 16).padding(.vertical, rowSpacing)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var rowSpacing: CGFloat {
        switch item.body {
        case .userMessage, .agentMessage: 12
        default: 5
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
        case .notice(let level, let text): NoticeBlock(level: level, text: text)
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
            .padding(.horizontal, 8).padding(.vertical, 6).background(style.cardHeader)
            VStack(alignment: .leading, spacing: 6) {
                if let explanation, !explanation.isEmpty {
                    Text(explanation).font(style.system(.footnote)).foregroundStyle(style.muted).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                }
                ForEach(steps.indices, id: \.self) { StepRow(step: steps[$0]) }
                if steps.isEmpty { Text("No steps yet.").font(style.system(.footnote)).foregroundStyle(style.muted) }
            }
            .padding(12)
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

/// The header line of a collapsible card.
private struct CardHeader<Title: View>: View {
    @Environment(\.desktopStyle) private var style
    let isOpen: Bool
    let label: String
    let toggle: () -> Void
    @ViewBuilder var title: () -> Title
    var body: some View {
        Button(action: toggle) {
            HStack(alignment: .top, spacing: 6) {
                title()
                Spacer(minLength: 4)
                Image(systemName: "chevron.right").font(style.system(.caption2, weight: .semibold)).foregroundStyle(style.muted)
                    .rotationEffect(.degrees(isOpen ? 90 : 0)).padding(.top, 3).accessibilityHidden(true)
            }
            .padding(.horizontal, 12).padding(.vertical, 10).frame(minHeight: 44, alignment: .leading).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(style.cardHeader)
        .accessibilityLabel(label)
        .accessibilityValue(isOpen ? "Expanded" : "Collapsed")
        .accessibilityHint(isOpen ? "Collapses the details" : "Shows the details")
        .accessibilityAddTraits(.isButton)
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
                    .font(style.system(.footnote)).frame(minHeight: 44)
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
            CardHeader(isOpen: isOpen, label: "\(ChatItemBody.command(command: command, cwd: cwd, output: "", exitCode: exitCode).summary), \(item.status.spoken)", toggle: toggle) {
                ChatStatusGlyph(status: item.status, errored: errored)
                Text("$ \(command)").font(style.code).foregroundStyle(style.text).lineLimit(isOpen ? 8 : 2).multilineTextAlignment(.leading)
                if let exitCode, exitCode != 0 { Text("exit \(exitCode)").font(style.face(10, bold: true, relativeTo: .caption2)).foregroundStyle(style.error).fixedSize() }
            }
            if isOpen {
                VStack(alignment: .leading, spacing: 6) {
                    if let cwd, !cwd.isEmpty { Text(cwd).font(style.mono(10, relativeTo: .caption2)).foregroundStyle(style.muted).lineLimit(1).truncationMode(.head) }
                    OutputText(text: output)
                    if !output.isEmpty { HStack { Spacer(); CopyButton(title: "Copy output", text: { output }) } }
                }
                .padding(12)
            } else if item.status == .inProgress, !output.isEmpty {
                // Output as it arrives, three lines of it, without opening the card.
                Text(verbatim: ChatOutput.tail(output, lines: 3).text).font(style.codeSmall).foregroundStyle(style.muted).lineLimit(3)
                    .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 8).padding(.vertical, 6).accessibilityHidden(true)
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
        let line = ChatToolSummary.line(for: input)
        ChatCard {
            CardHeader(isOpen: isOpen, label: "\(item.body.summary), \(item.status.spoken)", toggle: toggle) {
                ChatStatusGlyph(status: item.status)
                VStack(alignment: .leading, spacing: 2) {
                    Text([server, tool].compactMap { $0 }.joined(separator: " · ")).font(style.system(.subheadline, weight: .semibold)).foregroundStyle(style.text).lineLimit(1)
                    if !line.isEmpty { Text(line).font(style.codeSmall).foregroundStyle(style.muted).lineLimit(isOpen ? 4 : 1).multilineTextAlignment(.leading) }
                }
            }
            if isOpen {
                VStack(alignment: .leading, spacing: 8) {
                    if input != .null {
                        ChatCaption(text: "Input")
                        Text(verbatim: ChatToolSummary.pretty(input)).font(style.codeSmall).foregroundStyle(style.text).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                    }
                    if let output, !output.isEmpty {
                        ChatCaption(text: "Result")
                        OutputText(text: output)
                        HStack { Spacer(); CopyButton(title: "Copy result", text: { output }) }
                    }
                }
                .padding(12)
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
    var body: some View {
        let total = counts
        let isOpen = open.contains(item.id)
        ChatCard {
            CardHeader(isOpen: isOpen, label: "\(item.body.summary), \(item.status.spoken), \(total.added) added, \(total.removed) removed", toggle: { toggle(item.id) }) {
                ChatStatusGlyph(status: item.status)
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(changes.count == 1 ? "Edit" : "Edit · \(changes.count) files").font(style.system(.subheadline, weight: .semibold)).foregroundStyle(style.text)
                        if total.added + total.removed > 0 {
                            Text("+\(total.added)").foregroundStyle(style.added).monospacedDigit()
                            Text("−\(total.removed)").foregroundStyle(style.removed).monospacedDigit()
                        }
                    }
                    .font(style.face(11, bold: true, relativeTo: .caption))
                    if !isOpen, let first = changes.first {
                        Text(first.path + (changes.count > 1 ? " and \(changes.count - 1) more" : "")).font(style.codeSmall).foregroundStyle(style.muted).lineLimit(1).truncationMode(.head)
                    }
                }
            }
            if isOpen {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(changes.indices, id: \.self) { index in
                        FileRow(change: changes[index], isOpen: open.contains("\(item.id)#\(changes[index].path)"),
                                toggle: { toggle("\(item.id)#\(changes[index].path)") })
                    }
                }
                .padding(12)
            }
        }
    }
}

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
    private var word: String { change.kind == .add ? "Added" : (change.kind == .delete ? "Deleted" : (change.kind == .rename ? "Renamed" : "Edited")) }
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button(action: { if change.diff != nil { toggle() } }) {
                HStack(spacing: 6) {
                    Image(systemName: icon).foregroundStyle(color).accessibilityHidden(true)
                    Text(change.path).font(style.code).foregroundStyle(style.text).lineLimit(1).truncationMode(.head)
                    Spacer(minLength: 4)
                    if change.diff != nil { Image(systemName: "chevron.right").font(style.system(.caption2, weight: .semibold)).foregroundStyle(style.muted).rotationEffect(.degrees(isOpen ? 90 : 0)) }
                }
                .frame(minHeight: 44).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(word) \(change.path)")
            .accessibilityValue(change.diff == nil ? "" : (isOpen ? "Diff shown" : "Diff hidden"))
            .accessibilityHint(change.diff == nil ? "" : (isOpen ? "Hides the diff" : "Shows the diff"))
            if isOpen, let text = change.diff { DiffView(text: text) }
        }
    }
}

/// A unified diff in monospace: added lines green, removed red, hunk headers muted. Lines do not wrap (the whole diff scrolls sideways),
/// so a line is a line.
struct DiffView: View {
    @Environment(\.desktopStyle) private var style
    let text: String
    /// The width of the frame the diff sits in, so that the bands of added and removed lines reach across it, not just across their text.
    @State private var width: CGFloat = 0
    var body: some View {
        let diff = DiffCache.parse(text)
        VStack(alignment: .leading, spacing: 4) {
            ScrollView(.horizontal, showsIndicators: false) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(diff.lines.indices, id: \.self) { index in DiffLineView(line: diff.lines[index]) }
                }
                .frame(minWidth: width, alignment: .leading)
            }
            .background(style.background).clipShape(style.block(10)).overlay(style.block(10).stroke(style.divider, lineWidth: 1))
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
            if diff.hiddenLines > 0 {
                Text("… \(diff.hiddenLines) more lines not shown").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted)
            }
            HStack(spacing: 8) {
                Text("+\(diff.added) −\(diff.removed)").font(style.face(10, relativeTo: .caption2)).foregroundStyle(style.muted).monospacedDigit()
                Spacer()
                CopyButton(title: "Copy diff", text: { text })
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
