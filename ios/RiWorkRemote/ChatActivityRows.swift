import SwiftUI
import UIKit
import RiWorkCore

/// The renderer deliberately consumes the Core projection. Group membership, lifted artifacts and
/// disclosure defaults are shared with the engine; this view only supplies the phone's row anatomy.
struct ChatActivityRows: View, Equatable {
    @Environment(\.desktopStyle) private var style
    let fileIndex: ChatFileIndex
    let row: ChatProjectionRow
    let items: ChatTranscript
    var clockMS: UInt64 = 0
    let provider: ChatProvider
    let toggle: @MainActor (String) -> Void
    var showArtifacts = true
    var showCommentary = true
    var depth = 0
    var rendersDescendants = true
    var onFrame: @MainActor (String, UUID, CGRect?) -> Void = { _, _, _ in }
    @State private var frameToken = UUID()

    nonisolated static func == (lhs: Self, rhs: Self) -> Bool {
        guard lhs.row == rhs.row, lhs.provider == rhs.provider, lhs.clockMS == rhs.clockMS,
              lhs.showArtifacts == rhs.showArtifacts, lhs.showCommentary == rhs.showCommentary, lhs.depth == rhs.depth,
              lhs.rendersDescendants == rhs.rendersDescendants else { return false }
        // Comparing an entire 2,000-item value from every row is quadratic. A closed
        // group needs no child bodies; an open group checks only its visible subtree.
        func unchanged(_ row: ChatProjectionRow) -> Bool {
            guard lhs.items.item(row.id) == rhs.items.item(row.id),
                  lhs.fileIndex.byItem[row.id] == rhs.fileIndex.byItem[row.id] else { return false }
            if lhs.rendersDescendants, row.open, !row.children.allSatisfy(unchanged) { return false }
            return !lhs.rendersDescendants || row.comments.allSatisfy(unchanged)
        }
        if [.user, .final, .commentary].contains(lhs.row.kind),
           let item = lhs.items.item(lhs.row.id),
           lhs.fileIndex.byTurn[item.turnID ?? "legacy:\(item.id)"] != rhs.fileIndex.byTurn[item.turnID ?? "legacy:\(item.id)"] { return false }
        return unchanged(lhs.row) && lhs.fileIndex.resolve(artifacts: lhs.row.artifacts) == rhs.fileIndex.resolve(artifacts: rhs.row.artifacts)
    }

    private var item: ChatItem? { items.item(row.id) }
    private var artifacts: [ChatFileEntry] { fileIndex.resolve(artifacts: row.artifacts) }
    private var ownedFiles: [ChatFileEntry] { fileIndex.byItem[row.id] ?? [] }
    private var stripArtifacts: [ChatFileEntry] { Self.stripArtifacts(row: row, artifacts: artifacts, owned: ownedFiles) }

    /// Flattened footers and nested rows must suppress exactly the images already drawn inline.
    nonisolated static func stripArtifacts(row: ChatProjectionRow, artifacts: [ChatFileEntry], owned: [ChatFileEntry]) -> [ChatFileEntry] {
        guard row.kind != .activity else { return artifacts }
        let inlineIDs = row.open ? Set(owned.filter(\.isImage).map(\.id)) : Set(artifacts.first(where: \.isImage).map { [$0.id] } ?? [])
        return artifacts.filter { !inlineIDs.contains($0.id) }
    }
    private var isContainer: Bool { row.kind == .activity || row.kind == .group || row.kind == .subagent }

    var body: some View {
        let _ = Perf.count("body.ChatActivityRows")
        VStack(alignment: .leading, spacing: 0) {
            switch row.kind {
            case .user, .final:
                message
            case .commentary:
                if showCommentary { commentary }
            case .request:
                request
            case .compaction:
                HStack(spacing: 8) {
                    DesktopRule()
                    Text(row.title).font(.system(size: 11)).foregroundStyle(style.muted).fixedSize()
                    DesktopRule()
                }
                .padding(.vertical, 8)
                .accessibilityLabel(row.title)
            case .notice:
                Text(messageText).font(.system(size: 12)).foregroundStyle(style.muted)
                    .frame(maxWidth: .infinity, alignment: .leading).padding(.leading, 16)
            default:
                disclosure
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .id(row.id)
        .accessibilityIdentifier("chat.projection.\(row.id)")
        .onGeometryChange(for: CGRect.self) { $0.frame(in: .named("chat-transcript-space")) } action: { frame in
            if !isContainer { onFrame(row.id, frameToken, frame) }
        }
        .onDisappear { onFrame(row.id, frameToken, nil) }
    }

    private var messageText: String {
        guard let item else { return row.target ?? row.title }
        switch item.body {
        case .userMessage(let text), .agentMessage(let text), .reasoning(let text): return text
        case .notice(_, let text, _, _, _, _): return text
        default: return row.target ?? row.title
        }
    }

    @ViewBuilder private var message: some View {
        VStack(alignment: .leading, spacing: 0) {
            if row.kind == .user {
                HStack {
                    Spacer(minLength: 36)
                    let typedText = ChatFileCatalog.messageText(messageText, files: ownedFiles, attachments: item?.presentation.attachments ?? [])
                    VStack(alignment: .leading, spacing: 0) {
                        if !ownedFiles.isEmpty { ChatSentAttachments(files: ownedFiles) }
                        if !typedText.isEmpty {
                            Text(typedText).chatProse().foregroundStyle(style.text)
                                .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    .padding(.horizontal, 14).padding(.vertical, 10)
                    .background(style.native ? style.active : style.accent.opacity(0.12), in: style.block(18))
                    .accessibilityLabel("You said").accessibilityValue(typedText)
                }
            } else {
                if !messageText.isEmpty {
                    ChatMarkdownView(source: messageText).equatable()
                        .environment(\.chatFileEntries, fileIndex.byTurn[item?.turnID ?? "legacy:\(row.id)"] ?? [])
                        .accessibilityLabel("\(provider.title) said").accessibilityValue(messageText)
                }
                if showArtifacts, !ownedFiles.isEmpty { ChatFileStrip(files: ownedFiles) }
            }
        }
        .padding(.horizontal, 16).padding(.vertical, 10)
    }

    private var commentary: some View {
        Text(messageText).font(.system(size: 12)).foregroundStyle(style.muted)
            .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 16).padding(.vertical, 2)
    }

    private var request: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: "checkmark.bubble").foregroundStyle(style.link).accessibilityHidden(true)
            Text(row.title).font(.system(size: 12)).foregroundStyle(style.muted)
                .lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, 8).padding(.vertical, 7)
        .background(style.link.opacity(0.045))
        .overlay(alignment: .leading) { Rectangle().fill(style.link.opacity(0.7)).frame(width: 2) }
        .accessibilityElement(children: .combine).accessibilityAddTraits(.updatesFrequently)
    }

    private var disclosure: some View {
        VStack(alignment: .leading, spacing: 0) {
            ChatActivityHeader(row: row, item: item, clockMS: clockMS, imageCount: ownedFiles.filter(\.isImage).count,
                               thumbnail: showArtifacts && !row.open && row.kind != .activity ? artifacts.first(where: \.isImage) : nil) {
                UIImpactFeedbackGenerator(style: .soft).impactOccurred()
                toggle(row.id)
            }
            .onGeometryChange(for: CGRect.self) { $0.frame(in: .named("chat-transcript-space")) } action: { frame in
                if isContainer { onFrame(row.id, frameToken, frame) }
            }
            .contextMenu {
                Button("Copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = copyText }
                ShareLink(item: copyText) { Label("Share", systemImage: "square.and.arrow.up") }
            }
            if row.open {
                if !row.children.isEmpty {
                    if rendersDescendants { if depth > 2, row.kind == .subagent {
                        NavigationLink {
                            ScrollView { children.padding(.horizontal, 16) }
                                .navigationTitle(row.target ?? "Agent").navigationBarTitleDisplayMode(.inline)
                        } label: {
                            Label("Open agent activity", systemImage: "arrow.up.right.square")
                                .font(.system(size: 13)).frame(minHeight: 44)
                        }
                        .padding(.leading, 16)
                    } else {
                        children.padding(.leading, 16)
                    } }
                } else if row.elided {
                    Text("Shown on your Mac").font(.system(size: 12)).foregroundStyle(style.muted)
                        .padding(.leading, 16)
                } else if let item {
                    ChatActivityDetail(item: item, kind: row.kind, files: ownedFiles, provider: provider)
                        .padding(.leading, 16)
                }
                if rendersDescendants, row.kind == .subagent, !row.children.isEmpty, let item {
                    ChatActivityDetail(item: item, kind: row.kind, files: ownedFiles, provider: provider, childrenCaptured: true)
                        .padding(.leading, 16)
                }
            }
            // Normal artifacts live on the activity even while closed. They are suppressed on
            // its disclosed children, so opening a turn cannot duplicate its files or images.
            if rendersDescendants, showArtifacts, !stripArtifacts.isEmpty {
                ChatFileStrip(files: stripArtifacts).padding(.leading, 16)
            }
            if rendersDescendants, showCommentary {
                ForEach(row.comments) { comment in
                    ChatActivityRows(fileIndex: fileIndex, row: comment, items: items, clockMS: clockMS, provider: provider, toggle: toggle,
                                     showArtifacts: false, showCommentary: true, depth: depth, onFrame: onFrame).equatable()
                }
            }
        }
    }

    private var children: some View {
        LazyVStack(alignment: .leading, spacing: 0) {
        ForEach(row.children) { child in
            AnyView(ChatActivityRows(fileIndex: fileIndex, row: child, items: items, clockMS: clockMS, provider: provider, toggle: toggle,
                                     showArtifacts: row.kind == .activity ? false : showArtifacts,
                                     showCommentary: row.kind == .activity ? false : showCommentary,
                                     depth: depth + 1, onFrame: onFrame).equatable())
        }
        }
    }

    private var copyText: String {
        guard let item else { return row.title }
        switch item.body {
        case .command(let command, _, let output, _): return "$ \(command)\n\(output)"
        case .toolCall(_, _, let input, let output): return ChatToolSummary.pretty(input, limit: .max) + (output.map { "\n\($0)" } ?? "")
        case .fileChange(let changes): return changes.map { $0.path + "\n" + ($0.diff ?? "") }.joined(separator: "\n")
        default: return messageText
        }
    }
}

/// Separately virtualized content after a container's children, preserving source/artifact order.
struct ChatActivityFooter: View {
    let row: ChatProjectionRow
    let item: ChatItem?
    let files: [ChatFileEntry]
    let ownedFiles: [ChatFileEntry]
    let provider: ChatProvider
    var showsDetail = true
    var body: some View {
        let strip = ChatActivityRows.stripArtifacts(row: row, artifacts: files, owned: ownedFiles)
        VStack(alignment: .leading, spacing: 0) {
            if showsDetail, row.open, row.kind == .subagent, !row.children.isEmpty, let item {
                ChatActivityDetail(item: item, kind: row.kind, files: ownedFiles, provider: provider, childrenCaptured: true)
            }
            if !strip.isEmpty { ChatFileStrip(files: strip) }
        }.padding(.leading, 16)
    }
}

private enum ChatActivityHeaderRole: Sendable { case fixed, title, meta, spacer }
private struct ChatActivityHeaderRoleKey: LayoutValueKey {
    static let defaultValue = ChatActivityHeaderRole.fixed
}

/// Reserve the meta's intrinsic width (up to the mockup's 24%) before laying out the title.
/// A plain HStack gives a long title the meta's entire allocation, even at a lower priority.
private struct ChatActivityHeaderLayout: Layout {
    private let spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        CGSize(width: proposal.width ?? subviews.reduce(0) { $0 + $1.sizeThatFits(.unspecified).width }
               + CGFloat(max(0, subviews.count - 1)) * spacing, height: 36)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var widths = subviews.map { $0.sizeThatFits(.unspecified).width }
        var title: Int?, spacer: Int?
        for index in subviews.indices {
            switch subviews[index][ChatActivityHeaderRoleKey.self] {
            case .title: title = index
            case .meta: widths[index] = min(widths[index], bounds.width * 0.24)
            case .spacer: spacer = index; widths[index] = 0
            case .fixed: break
            }
        }
        let gaps = CGFloat(max(0, subviews.count - 1)) * spacing
        let remaining = bounds.width - gaps - widths.reduce(0, +)
        if let title { widths[title] = max(0, widths[title] + min(0, remaining)) }
        if let spacer { widths[spacer] = max(0, bounds.width - gaps - widths.reduce(0, +)) }
        var x = bounds.minX
        for index in subviews.indices {
            subviews[index].place(at: CGPoint(x: x, y: bounds.midY), anchor: .leading,
                                 proposal: ProposedViewSize(width: widths[index], height: bounds.height))
            x += widths[index] + spacing
        }
    }
}

private struct ChatActivityHeader: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.chatOpenFile) private var openFile
    let row: ChatProjectionRow
    let item: ChatItem?
    var clockMS: UInt64 = 0
    var imageCount = 0
    var thumbnail: ChatFileEntry?
    let action: () -> Void

    var body: some View {
        let meta = meta
        Button(action: action) {
            ChatActivityHeaderLayout {
                Image(systemName: "chevron.right").font(.system(size: 9, weight: .semibold))
                    .rotationEffect(.degrees(row.open ? 90 : 0)).frame(width: 9).foregroundStyle(style.muted)
                if row.kind == .activity {
                    if row.running { ProgressView().controlSize(.mini).frame(width: 16) }
                    ChatActivityElapsed(row: row, clockMS: clockMS, activity: true)
                        .font(.system(size: 12)).foregroundStyle(style.muted)
                        .lineLimit(1).truncationMode(.tail)
                        .layoutValue(key: ChatActivityHeaderRoleKey.self, value: .title)
                    if row.provisional {
                        Text("Earlier activity not loaded").font(.system(size: 10)).foregroundStyle(style.muted)
                            .lineLimit(1).truncationMode(.tail)
                            .layoutValue(key: ChatActivityHeaderRoleKey.self, value: .meta)
                    }
                    Spacer(minLength: 0).layoutValue(key: ChatActivityHeaderRoleKey.self, value: .spacer)
                    if row.failedCount > 0 {
                        Circle().fill(style.error).frame(width: 5, height: 5)
                            .accessibilityLabel("\(row.failedCount) failed")
                    }
                } else {
                    Image(systemName: icon).font(.system(size: 12)).foregroundStyle(iconColor).frame(width: 16)
                    headerText.lineLimit(1).truncationMode(row.kind == .read || row.kind == .edit ? .middle : .tail)
                        .layoutValue(key: ChatActivityHeaderRoleKey.self, value: .title)
                    if !meta.isEmpty {
                        Text("· " + meta).font(.system(size: 11)).foregroundStyle(style.muted)
                            .lineLimit(1).truncationMode(.tail)
                            .layoutValue(key: ChatActivityHeaderRoleKey.self, value: .meta)
                            .chatLayoutProbe("activity-meta-\(row.id)")
                    }
                    Spacer(minLength: 0).layoutValue(key: ChatActivityHeaderRoleKey.self, value: .spacer)
                    badge
                }
                if thumbnail != nil { Color.clear.frame(width: 32, height: 28) }
            }
            .frame(height: 36).padding(.horizontal, 8)
            .contentShape(Rectangle().inset(by: -4))
            .background(row.kind == .group ? style.muted.opacity(0.055) : .clear)
            .chatLayoutProbe("activity-header-\(row.id)")
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibleLabel(meta: meta))
        .accessibilityValue(row.open ? "Expanded" : "Collapsed")
        .accessibilityHint(row.open ? "Hides details" : "Shows details")
        .accessibilityIdentifier("chat.activity.toggle.\(row.id)")
        .overlay(alignment: .trailing) {
            if let thumbnail {
                Button { openFile(thumbnail) } label: {
                    ChatFileThumbnail(file: thumbnail).frame(width: 32, height: 28)
                        .clipShape(RoundedRectangle(cornerRadius: 4)).padding(.horizontal, 8)
                        .contentShape(Rectangle().inset(by: -8))
                        .chatLayoutProbe("activity-thumbnail-\(row.id)")
                }
                .buttonStyle(.plain).accessibilityLabel("Preview \(thumbnail.displayName)")
            }
        }
        .chatLayoutProbe("activity-row-\(row.id)", revision: String(row.open), action: action)
    }

    private var headerText: Text {
        let title: String
        if row.kind == .mcp, let item, case .toolCall(let server, let tool, _, _) = item.body {
            title = [server, tool].compactMap { $0 }.joined(separator: " · ")
        } else { title = row.title }
        return Text(title).font(.system(size: 13, weight: row.kind == .group ? .medium : .regular)).foregroundColor(style.text)
            + Text(displayTarget.map { " " + $0 } ?? "").font(.system(size: 12.5, design: .monospaced)).foregroundColor(style.muted)
    }

    private var displayTarget: String? {
        guard let target = row.target, !target.isEmpty else { return nil }
        if row.kind == .read || row.kind == .edit {
            // The full target remains in the VoiceOver label and the row's copy/share payload.
            let components = target.split(separator: "/", omittingEmptySubsequences: true)
            return target.hasPrefix("/") ? components.suffix(2).joined(separator: "/") : target
        }
        if row.kind == .mcp, let item, case .toolCall(let server, let tool, let input, _) = item.body {
            let qualified = server.map { $0 + "." + tool } ?? tool
            if target == tool || target == qualified || target == [server, tool].compactMap({ $0 }).joined(separator: " · ") {
                let summary = ChatToolSummary.line(for: input)
                return summary.isEmpty || summary == "{}" ? nil : summary
            }
        }
        return target
    }

    @ViewBuilder private var badge: some View {
        if row.running {
            HStack(spacing: 3) {
                ProgressView().controlSize(.mini)
                if row.durationMS != nil { ChatActivityElapsed(row: row, clockMS: clockMS) }
            }
            .font(.system(size: 10)).foregroundStyle(style.muted).fixedSize()
        } else if row.failedCount > 0 {
            Text(failureTitle).font(.system(size: 10, weight: .medium)).foregroundStyle(style.error)
                .padding(.horizontal, 5).padding(.vertical, 2).background(style.error.opacity(0.10), in: Capsule()).fixedSize()
        } else if item?.status == .declined || item?.status == .interrupted {
            Text(item?.status == .declined ? "Declined" : "Interrupted").font(.system(size: 10))
                .foregroundStyle(item?.status == .declined ? style.gold : style.muted).fixedSize()
        }
    }

    private var failureTitle: String {
        if row.kind == .group || row.kind == .activity { return "\(row.failedCount) failed" }
        if let item, case .command(_, _, _, let exit) = item.body, let exit, exit != 0 { return "Exit \(exit)" }
        return "Failed"
    }

    private var meta: String {
        var parts: [String] = []
        if row.provisional { parts.append("earlier activity not loaded") }
        if row.elided { parts.append("shown on your Mac") }
        if row.running, row.kind == .group, let label = row.currentLabel { parts.append(label) }
        if let semantics = item?.presentation.semantics {
            if let range = semantics.range { parts.append("L\(range.start)" + (range.end.map { "–\($0)" } ?? "")) }
            if let count = semantics.matchCount { parts.append("\(count) \(count == 1 ? "match" : "matches")") }
            if row.kind == .subagent, let count = semantics.worker?.childCount { parts.append("\(count) tools") }
        }
        if imageCount > 0 { parts.append("\(imageCount) \(imageCount == 1 ? "image" : "images")") }
        if let item {
            if case .command(_, let cwd, _, _) = item.body, let cwd, !cwd.isEmpty { parts.append((cwd as NSString).lastPathComponent) }
            if case .fileChange(let changes) = item.body {
                let counts = changes.compactMap(\.diff).map { ChatDiff.parse($0, maxLines: 0) }
                let added = counts.reduce(0) { $0 + $1.added }, removed = counts.reduce(0) { $0 + $1.removed }
                if added + removed > 0 { parts.append("+\(added) −\(removed)") }
                else if changes.count > 1 { parts.append("\(changes.count) files") }
            }
        }
        if !row.running, let duration = row.durationMS { parts.append(Self.duration(duration)) }
        return parts.joined(separator: " · ")
    }

    private func accessibleLabel(meta: String) -> String {
        [row.title, row.target, meta.isEmpty ? nil : meta,
         row.failedCount > 0 ? failureTitle : nil, row.running ? "Running" : nil]
            .compactMap { $0 }.joined(separator: ", ")
    }
    private var icon: String {
        switch row.kind {
        case .shell: "terminal"
        case .read: "doc.text"
        case .search: "magnifyingglass"
        case .list: "folder"
        case .edit: "pencil"
        case .webSearch, .webFetch: "globe"
        case .mcp: "point.3.connected.trianglepath.dotted"
        case .subagent: "person.2"
        case .reasoning: "brain"
        case .plan, .todo: "checklist"
        case .group: "square.stack.3d.up"
        default: "wrench.and.screwdriver"
        }
    }
    private var iconColor: Color {
        switch row.kind {
        case .read, .webFetch, .webSearch: style.link
        case .edit: style.gold
        case .subagent, .mcp: .purple
        default: style.muted
        }
    }
    static func duration(_ milliseconds: UInt64) -> String {
        let seconds = milliseconds / 1000
        return seconds >= 60 ? "\(seconds / 60)m \(seconds % 60)s" : "\(seconds)s"
    }
}

private struct ChatActivityDetail: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.chatOpenFile) private var openFile
    let item: ChatItem
    let kind: ChatProjectionRowKind
    let files: [ChatFileEntry]
    let provider: ChatProvider
    var childrenCaptured = false
    @State private var rawInput = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            switch item.body {
            case .command(let command, let cwd, let output, let exit):
                ChatActivityOutput(text: output, command: command, exit: exit, cwd: cwd, startedAt: item.startedAt, endedAt: item.endedAt,
                                   running: item.status == .inProgress, itemID: item.id)
                    .padding(.leading, 29)
            case .reasoning(let text):
                Text(text.isEmpty ? "Nothing to show yet." : text).font(.system(size: 12)).foregroundStyle(style.muted)
                    .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            case .fileChange(let changes):
                ForEach(changes.indices, id: \.self) { index in
                    let change = changes[index]
                    if changes.count > 1 { fileButton(change.path) }
                    if let diff = change.diff, !diff.isEmpty { ChatActivityDiff(text: diff, snippet: provider == .claude) }
                    else { Text("Diff not available").font(.system(size: 12)).foregroundStyle(style.muted) }
                    fileButton(change.path, title: "Open \((change.path as NSString).lastPathComponent)")
                }
            case .toolCall(_, _, let input, let output):
                if kind == .mcp || kind == .other {
                    HStack {
                        Text("Input").font(.system(size: 11)).foregroundStyle(style.muted)
                        Spacer()
                        Button(rawInput ? "Fields" : "Raw") { rawInput.toggle() }
                            .font(.system(size: 11)).frame(minHeight: 44)
                    }
                    if rawInput || inputFields(input) == nil {
                        Text(ChatToolSummary.pretty(input)).font(.system(size: 11, design: .monospaced))
                            .foregroundStyle(style.text).textSelection(.enabled)
                    } else if let fields = inputFields(input) {
                        ForEach(fields.keys.sorted(), id: \.self) { key in
                            HStack(alignment: .top, spacing: 8) {
                                Text(key).foregroundStyle(style.muted).frame(maxWidth: 80, alignment: .leading)
                                Text(fields[key]?.string ?? ChatToolSummary.pretty(fields[key] ?? .null))
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                            .font(.system(size: 11, design: .monospaced)).textSelection(.enabled)
                        }
                    }
                }
                if let output, !output.isEmpty {
                    if let output = imageOutput(output) { toolOutput(output) }
                } else if let excerpt = item.presentation.semantics?.excerpt {
                    if let excerpt = imageOutput(excerpt) { toolOutput(excerpt) }
                } else if kind == .read, !files.contains(where: \.isImage) { toolOutput("") }
                else if (kind == .mcp || kind == .other), item.status == .inProgress { toolOutput("") }
                else if kind == .subagent, !childrenCaptured { Text("Child activity was not captured.").font(.system(size: 12)).foregroundStyle(style.muted) }
            case .plan(let explanation, let steps):
                if let explanation { Text(explanation).font(.system(size: 12)).foregroundStyle(style.muted) }
                checklist(steps)
            case .todo(let steps): checklist(steps)
            case .webSearch:
                Text(item.presentation.semantics?.excerpt ?? "No result preview was provided.")
                    .font(.system(size: 12)).foregroundStyle(style.muted).textSelection(.enabled)
            case .elided:
                Text(ChatElision.explanation).font(.system(size: 12)).foregroundStyle(style.muted)
            default:
                if let excerpt = item.presentation.semantics?.excerpt { ChatActivityOutput(text: excerpt, itemID: item.id) }
                else { Text("Details are available on your Mac.").font(.system(size: 12)).foregroundStyle(style.muted) }
            }
            if files.contains(where: \.isImage) {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(files.filter(\.isImage)) { file in ChatInlineImage(file: file) }
                }.padding(.leading, 29).padding(.bottom, 4)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func inputFields(_ input: JSONValue) -> [String: JSONValue]? {
        guard case .object(let fields) = input else { return nil }
        return fields
    }

    private func imageOutput(_ output: String) -> String? {
        guard files.contains(where: \.isImage) else { return output }
        let text = ChatContainerText.withoutImagePlaceholders(output)
        return text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : text
    }

    @ViewBuilder private func toolOutput(_ output: String) -> some View {
        if isFileExcerpt {
            ChatActivityFileExcerpt(text: output, path: item.presentation.semantics?.target ?? "File", range: item.presentation.semantics?.range,
                                    file: files.first { $0.path == item.presentation.semantics?.target && !$0.isImage },
                                    running: item.status == .inProgress, failed: item.status == .failed, itemID: item.id)
                .padding(.leading, 29)
        } else if kind == .search || kind == .list {
            let matches = output.split(separator: "\n").prefix(80).compactMap { ChatActivityMatch(String($0)) }
            if !matches.isEmpty {
                ForEach(Array(matches.enumerated()), id: \.offset) { _, match in
                    let file = files.first { $0.path == match.path }
                    Button {
                        if var file { file.line = match.line; openFile(file) }
                    } label: {
                        HStack(spacing: 5) {
                            Image(systemName: "doc.text")
                            Text("\((match.path as NSString).lastPathComponent):\(match.line)").lineLimit(1)
                            if !match.text.isEmpty { Text(match.text).foregroundStyle(style.muted).lineLimit(1) }
                        }
                        .font(.system(size: 11, design: .monospaced)).frame(minHeight: 44).frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .buttonStyle(.plain).foregroundStyle(style.link).disabled(file == nil)
                }
            } else { ChatActivityOutput(text: output, running: item.status == .inProgress, itemID: item.id).padding(.leading, 29) }
        } else {
            ChatActivityOutput(text: output, running: item.status == .inProgress, itemID: item.id).padding(.leading, 29)
        }
    }

    private var isFileExcerpt: Bool {
        guard !files.contains(where: { $0.path == item.presentation.semantics?.target && $0.isImage }) else { return false }
        if kind == .read { return true }
        guard let target = item.presentation.semantics?.target, !target.hasPrefix("http") else { return false }
        return files.contains { $0.path == target && !$0.isImage } || (target.contains("/") && !(target as NSString).pathExtension.isEmpty)
    }

    private func fileButton(_ path: String, title: String? = nil) -> some View {
        let file = files.first { $0.path == path }
        return Button { if let file { openFile(file) } } label: {
            Label(title ?? path, systemImage: "doc.text").font(.system(size: 11, design: .monospaced))
                .lineLimit(1).truncationMode(.middle).frame(minHeight: 44)
        }
        .buttonStyle(.plain).foregroundStyle(style.link).disabled(file == nil)
    }

    private func checklist(_ steps: [ChatStep]) -> some View {
        ForEach(steps.indices, id: \.self) { index in
            let step = steps[index]
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Image(systemName: step.status == .completed ? "checkmark.circle" : (step.status == .inProgress ? "arrow.right.circle" : "circle"))
                    .foregroundStyle(step.status == .inProgress ? style.link : style.muted)
                Text(step.text).foregroundStyle(step.status == .completed ? style.muted : style.text)
            }
            .font(.system(size: 12)).frame(minHeight: 28).textSelection(.enabled)
        }
    }
}

private struct ChatActivityMatch {
    let path: String
    let line: Int
    let text: String
    init?(_ source: String) {
        let parts = source.split(separator: ":", maxSplits: 2, omittingEmptySubsequences: false)
        guard parts.count >= 2, let line = Int(parts[1]), line > 0 else { return nil }
        path = String(parts[0]); self.line = line; text = parts.count > 2 ? String(parts[2]) : ""
    }
}

/// Formatting is shared by the shell and excerpt views so copying never includes a drawn gutter.
enum ChatContainerText {
    struct Line: Equatable { var number: Int?; var text: String }
    static func clean(_ text: String) -> String {
        let terminal = terminalText(text)
        return terminal.utf8.contains(9) ? terminal.replacingOccurrences(of: "\t", with: "    ") : terminal
    }
    private static func terminalText(_ text: String) -> String {
        var value = text
        if text.utf8.contains(13) {
            value = text.components(separatedBy: "\n").map { line in
                line.components(separatedBy: "\r").last(where: { !$0.isEmpty }) ?? ""
            }.joined(separator: "\n")
        }
        if value.utf8.contains(27) {
            for pattern in ["\u{001B}\\][^\u{0007}\u{001B}]*(?:\u{0007}|\u{001B}\\\\)", "\u{001B}\\[[0-?]*[ -/]*[@-~]"] {
                value = value.replacingOccurrences(of: pattern, with: "", options: .regularExpression)
            }
        }
        return value
    }
    static func withoutImagePlaceholders(_ text: String) -> String {
        text.components(separatedBy: "\n").filter {
            !["[image]", "image"].contains($0.trimmingCharacters(in: .whitespacesAndNewlines).lowercased())
        }.joined(separator: "\n")
    }
    static func omission(_ count: Int, file: Bool = false) -> String {
        let noun = count == 1 ? "line" : "lines"
        return file ? "\(count) more \(noun)" : "\(count) \(noun) omitted"
    }
    static func duration(_ milliseconds: UInt64) -> String {
        if milliseconds < 60_000 { return String(format: "%.1fs", Double(milliseconds) / 1000) }
        let seconds = milliseconds / 1000
        return "\(seconds / 60)m \(seconds % 60)s"
    }
    static func lines(_ text: String) -> [String] {
        guard !text.isEmpty else { return [] }
        var lines = text.components(separatedBy: "\n")
        if lines.last == "" { lines.removeLast() }
        return lines
    }
    private static let numberExpression = try! NSRegularExpression(pattern: #"^\s*(\d+)(?:→|\t)"#)
    private static func numberedTerminal(_ text: String, start: Int?) -> [Line] {
        lines(text).enumerated().map { index, line in
            if let match = numberExpression.firstMatch(in: line, range: NSRange(line.startIndex..., in: line)),
               let numberRange = Range(match.range(at: 1), in: line), let prefix = Range(match.range, in: line) {
                return Line(number: Int(line[numberRange]), text: String(line[prefix.upperBound...]).replacingOccurrences(of: "\t", with: "    "))
            }
            let inferred = start.flatMap { value -> Int? in
                let sum = value.addingReportingOverflow(index)
                return sum.overflow ? nil : sum.partialValue
            }
            return Line(number: inferred, text: line.replacingOccurrences(of: "\t", with: "    "))
        }
    }
    static func excerptCopy(_ text: String) -> String {
        let terminal = terminalText(text)
        return numberedTerminal(terminal, start: nil).map(\.text).joined(separator: "\n") + (terminal.hasSuffix("\n") ? "\n" : "")
    }
    struct OutputSnapshot {
        var output: [(Int, String, Bool)]
        var lineCount: Int
        var omitted: Int
        var byteTruncated: Bool
        var bounded: Bool
    }
    struct ExcerptSnapshot {
        var lines: [Line]
        var lineCount: Int
        var byteTruncated: Bool
        var bounded: Bool
    }
    static func excerptSnapshot(_ source: String, start: Int?, expanded: Bool = false) -> ExcerptSnapshot {
        let terminal = terminalText(source)
        let count = ChatOutput.lineCount(terminal)
        let window = headWindow(terminal, maxLines: expanded ? 200 : 15, maxBytes: 32_768)
        let numbered = numberedTerminal(window.text, start: start)
        let visible = boundedHead(numbered, maxLines: expanded ? 200 : 15)
        let truncated = window.byteTruncated || visible != numbered
        return .init(lines: visible, lineCount: count, byteTruncated: truncated,
                     bounded: count > 200 || terminal.utf8.count > 32_768 || truncated)
    }
    private struct Window { var text: String; var byteTruncated: Bool }

    /// Cleaning and the full newline count happen once. Only the bounded windows become line strings.
    static func outputSnapshot(_ source: String, running: Bool = false, expanded: Bool = false) -> OutputSnapshot {
        let clean = clean(source)
        let count = ChatOutput.lineCount(clean)
        let bounded = count > 200 || clean.utf8.count > 32_768
        func rows(_ window: Window, start: Int) -> [(Int, String, Bool)] {
            lines(window.text).enumerated().map { (start + $0.offset, $0.element, false) }
        }
        if running || expanded {
            let tail = tailWindow(clean, maxLines: running ? 3 : 200, maxBytes: 32_768)
            let kept = lines(tail.text)
            let output = kept.enumerated().map { (count - kept.count + $0.offset, $0.element, false) }
            return .init(output: output, lineCount: count, omitted: max(0, count - kept.count), byteTruncated: tail.byteTruncated, bounded: bounded)
        }
        if count > 15 {
            let head = headWindow(clean, maxLines: 3, maxBytes: 16_384)
            let tail = tailWindow(clean, maxLines: 12, maxBytes: 16_384)
            let first = rows(head, start: 0), last = lines(tail.text)
            let omitted = max(0, count - first.count - last.count)
            let output = first + [(3, "… " + omission(omitted), true)]
                + last.enumerated().map { (count - last.count + $0.offset, $0.element, false) }
            return .init(output: output, lineCount: count, omitted: omitted, byteTruncated: head.byteTruncated || tail.byteTruncated, bounded: bounded)
        }
        let head = headWindow(clean, maxLines: 15, maxBytes: 32_768)
        var output = rows(head, start: 0)
        if head.byteTruncated { output.append((count, "… Output truncated", true)) }
        return .init(output: output, lineCount: count, omitted: max(0, count - output.filter { !$0.2 }.count), byteTruncated: head.byteTruncated, bounded: bounded)
    }

    private static func headWindow(_ text: String, maxLines: Int, maxBytes: Int) -> Window {
        let bytes = text.utf8
        var end = bytes.startIndex, used = 0, newlines = 0
        while end < bytes.endIndex && used < maxBytes {
            let byte = bytes[end]
            end = bytes.index(after: end); used += 1
            if byte == 10 { newlines += 1; if newlines == maxLines { break } }
        }
        let truncated = used == maxBytes && end < bytes.endIndex && newlines < maxLines
        while end > bytes.startIndex && end < bytes.endIndex && bytes[end] & 0xC0 == 0x80 { end = bytes.index(before: end) }
        return Window(text: String(decoding: bytes[..<end], as: UTF8.self), byteTruncated: truncated)
    }
    private static func tailWindow(_ text: String, maxLines: Int, maxBytes: Int) -> Window {
        let bytes = text.utf8
        guard !bytes.isEmpty, maxLines > 0, maxBytes > 0 else { return Window(text: "", byteTruncated: false) }
        let trailingNewline = bytes.last == 10
        let end = trailingNewline ? bytes.index(before: bytes.endIndex) : bytes.endIndex
        let budget = maxBytes - (trailingNewline ? 1 : 0)
        var start = end, used = 0, newlines = 0, lineLimited = false
        while start > bytes.startIndex && used < budget {
            let previous = bytes.index(before: start)
            if bytes[previous] == 10 {
                newlines += 1
                if newlines == maxLines { lineLimited = true; break }
            }
            start = previous; used += 1
        }
        let truncated = start > bytes.startIndex && !lineLimited && used == budget
        while start < end && bytes[start] & 0xC0 == 0x80 { start = bytes.index(after: start) }
        // Prefer whole trailing lines when a byte cap cuts through their predecessor.
        if truncated, start > bytes.startIndex, bytes[bytes.index(before: start)] != 10,
           let newline = bytes[start..<end].firstIndex(of: 10) { start = bytes.index(after: newline) }
        return Window(text: String(decoding: bytes[start..<bytes.endIndex], as: UTF8.self), byteTruncated: truncated)
    }
    static func boundedHead(_ lines: [Line], maxLines: Int = 200, maxBytes: Int = 32_768) -> [Line] {
        var result: [Line] = [], bytes = 0
        for line in lines.prefix(maxLines) {
            let remaining = max(0, maxBytes - bytes)
            if remaining == 0 { break }
            if line.text.utf8.count > remaining {
                var text = "", used = 0
                for scalar in line.text.unicodeScalars {
                    let size = String(scalar).utf8.count
                    if used + size > remaining { break }
                    text.unicodeScalars.append(scalar); used += size
                }
                if !text.isEmpty { result.append(Line(number: line.number, text: text)) }
                break
            }
            result.append(line); bytes += line.text.utf8.count + 1
        }
        return result
    }
    static func isError(_ line: String) -> Bool {
        let lower = line.trimmingCharacters(in: .whitespaces).lowercased()
        return ["error", "stderr", "fatal", "panic", "panicked"].contains { lower == $0 || lower.hasPrefix($0 + ":") || lower.hasPrefix($0 + " ") || lower.hasPrefix($0 + "[") }
    }
}

private struct ChatContainerFooter: View {
    @Environment(\.desktopStyle) private var style
    let omitted: Int
    let showAll: Bool
    var file = false
    var bounded = false
    var byteTruncated = false
    let action: () -> Void
    var body: some View {
        HStack(spacing: 0) {
            if omitted > 0 { Text("… " + ChatContainerText.omission(omitted, file: file) + " · ").foregroundStyle(style.muted) }
            else if byteTruncated { Text("… Output truncated · ").foregroundStyle(style.muted) }
            Button(showAll ? "Show less" : "Show all", action: action)
                .buttonStyle(.plain).foregroundStyle(style.link).contentShape(Rectangle().inset(by: -12))
            if bounded, showAll || byteTruncated { Text(" · Full output on your Mac").foregroundStyle(style.muted) }
        }
        .font(style.system(.caption2)).lineLimit(1).frame(height: 20, alignment: .leading)
    }
}

/// A closed call instantiates no output. Live output is a three-line tail; completed output
/// starts with three plus twelve lines and expands only to the bounded local tail.
private struct ChatActivityOutput: View {
    @Environment(\.desktopStyle) private var style
    let text: String
    var command: String? = nil
    var exit: Int? = nil
    var cwd: String? = nil
    var startedAt: UInt64? = nil
    var endedAt: UInt64? = nil
    var running = false
    var itemID = ""
    @State private var showAll = false
    @State private var copied = false
    @State private var copyResetTask: Task<Void, Never>?


    var body: some View {
        let _ = Perf.count("body.ChatActivityOutput")
        let prepared = ChatContainerText.outputSnapshot(text, running: running, expanded: showAll)
        let output = prepared.output
        VStack(alignment: .leading, spacing: 0) {
            if let command {
                Group {
                    if running { TimelineView(.periodic(from: .now, by: 1)) { context in header(command, now: context.date) } }
                    else { header(command, now: .now) }
                }.padding(.bottom, output.isEmpty && !running ? 0 : 4)
            }
            VStack(alignment: .leading, spacing: 0) {
                if running, prepared.lineCount == 0 { Text("Waiting for output…").foregroundStyle(style.muted) }
                ForEach(output, id: \.0) { index, line, marker in
                    Text(line.isEmpty ? " " : line).italicIf(marker)
                        .foregroundStyle(marker ? style.muted : (ChatContainerText.isError(line) ? style.removed : style.text))
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(!marker && ChatContainerText.isError(line) ? style.error.opacity(0.12) : .clear)
                        .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                        .chatLayoutProbe("output-line-\(itemID)-\(index)")
                }
            }
            .font(style.codeSmall).frame(maxWidth: .infinity, alignment: .leading)
            .animation(running ? .easeInOut(duration: 0.12) : nil, value: output.count)
            if !running, prepared.lineCount > 15 || prepared.bounded || prepared.byteTruncated {
                ChatContainerFooter(omitted: prepared.omitted, showAll: showAll,
                                    bounded: prepared.bounded, byteTruncated: prepared.byteTruncated) { showAll.toggle() }
                    .chatLayoutProbe("output-show-all-\(itemID)", revision: String(showAll), action: { showAll.toggle() })
            }
        }
        .chatContainerSurface()
        .overlay(alignment: .topTrailing) {
            ChatContainerIconAction(symbol: copied ? "checkmark" : "doc.on.doc", label: "Copy output", action: copy)
                .padding(4).chatLayoutProbe("copy-output-button-\(itemID)")
        }
        .chatLayoutProbe("output-block-\(itemID)")
        .contextMenu { Button("Copy output", systemImage: "doc.on.doc", action: copy) }
        .chatLayoutProbe("copy-output-\(itemID)", revision: text, action: copy)
        .onDisappear { copyResetTask?.cancel() }
        .padding(.bottom, 4)
    }
    private func header(_ command: String, now: Date) -> some View {
        ViewThatFits(in: .horizontal) {
            headerLine(command, now: now, cwd: true, duration: true).fixedSize(horizontal: true, vertical: false)
            headerLine(command, now: now, cwd: false, duration: true).fixedSize(horizontal: true, vertical: false)
            headerLine(command, now: now, cwd: false, duration: false).fixedSize(horizontal: true, vertical: false)
            headerLine(command, now: now, cwd: false, duration: false)
        }
        .font(style.mono(10.5)).foregroundStyle(style.muted).lineLimit(1).frame(height: 18, alignment: .leading).padding(.trailing, 48)
        .chatLayoutProbe("output-header-\(itemID)")
    }
    private func headerLine(_ command: String, now: Date, cwd showCwd: Bool, duration showDuration: Bool) -> some View {
        HStack(spacing: 0) {
            Text("$ " + (command.components(separatedBy: .newlines).first ?? "") + (command.contains("\n") ? "…" : ""))
                .textSelection(.enabled).lineLimit(1).truncationMode(.middle)
                .contextMenu { Button("Copy command", systemImage: "doc.on.doc") { UIPasteboard.general.string = command } }
                .chatLayoutProbe("copy-command-\(itemID)", revision: command, action: { UIPasteboard.general.string = command })
            if let exit, exit != 0 { Text(" · exit \(exit)").foregroundStyle(style.error).fixedSize() }
            if running {
                Text(" · ")
                ProgressView().controlSize(.mini).scaleEffect(0.7).frame(width: 10, height: 10)
                Text(" " + elapsed(now)).monospacedDigit().fixedSize()
            } else if showDuration, let startedAt, let endedAt, endedAt >= startedAt {
                Text(" · " + ChatContainerText.duration(endedAt - startedAt)).fixedSize()
            }
            if showCwd, let cwd, !cwd.isEmpty { Text(" · " + cwd.split(separator: "/").suffix(2).joined(separator: "/")).lineLimit(1).truncationMode(.middle) }
        }
    }
    private func elapsed(_ now: Date) -> String {
        let seconds = max(0, Int(now.timeIntervalSince1970 - Double(startedAt ?? UInt64(now.timeIntervalSince1970 * 1000)) / 1000))
        return "\(seconds / 60):\(String(format: "%02d", seconds % 60))"
    }
    private func copy() {
        UIPasteboard.general.string = ChatContainerText.clean(text); copied = true
        copyResetTask?.cancel()
        copyResetTask = Task {
            do { try await Task.sleep(for: .milliseconds(1500)); copied = false } catch { }
        }
    }
}

private extension View {
    @ViewBuilder func italicIf(_ condition: Bool) -> some View { if condition { italic() } else { self } }
}

private struct ChatActivityFileExcerpt: View {
    @Environment(\.desktopStyle) private var style
    @Environment(\.chatOpenFile) private var openFile
    let text: String
    let path: String
    let range: ChatLineRange?
    let file: ChatFileEntry?
    var running = false
    var failed = false
    var itemID = ""
    @State private var showAll = false
    @State private var copied = false
    @State private var copyResetTask: Task<Void, Never>?
    @ScaledMetric(relativeTo: .body) private var gutterCharacterWidth: CGFloat = 6.6
    private var firstLine: Int? { ChatContainerText.excerptSnapshot(text, start: range.map { Int(clamping: $0.start) }).lines.first?.number ?? range.map { Int(clamping: $0.start) } }
    var body: some View {
        let prepared = ChatContainerText.excerptSnapshot(text, start: range.map { Int(clamping: $0.start) }, expanded: showAll)
        let visible = prepared.lines
        let gutterWidth = CGFloat(max(3, String(visible.compactMap(\.number).max() ?? 0).count + 1)) * gutterCharacterWidth * CGFloat(style.scale)
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 0) {
                Text(path.split(separator: "/").suffix(2).joined(separator: "/")).textSelection(.enabled).truncationMode(.middle)
                if let range { Text(" · L\(range.start)" + (range.end.map { "–\($0)" } ?? "")).fixedSize() }
                if prepared.lineCount > 0 { Text(" · \(prepared.lineCount) \(prepared.lineCount == 1 ? "line" : "lines")").fixedSize() }
            }
            .font(style.mono(10.5)).foregroundStyle(style.muted).lineLimit(1).frame(height: 18, alignment: .leading)
            .padding(.trailing, 48).padding(.bottom, 4)
            if running { Text("Reading…").foregroundStyle(style.muted) }
            else {
                ForEach(visible.indices, id: \.self) { index in
                    HStack(alignment: .top, spacing: 8) {
                        if !failed, visible.contains(where: { $0.number != nil }) {
                            Text(visible[index].number.map(String.init) ?? "").monospacedDigit().foregroundStyle(style.muted)
                                .frame(width: gutterWidth, alignment: .trailing).accessibilityHidden(true)
                                .chatLayoutProbe("file-gutter-\(itemID)-\(index)")
                        }
                        Text(visible[index].text.isEmpty ? " " : visible[index].text).foregroundStyle(failed ? style.error : style.text)
                            .frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                    }.chatLayoutProbe("file-line-\(itemID)-\(index)")
                }
            }
            if !running, prepared.lineCount > 15 || prepared.byteTruncated {
                ChatContainerFooter(omitted: max(0, prepared.lineCount - visible.count), showAll: showAll, file: true,
                                    bounded: prepared.bounded, byteTruncated: prepared.byteTruncated) { showAll.toggle() }
                    .chatLayoutProbe("output-show-all-\(itemID)", revision: String(showAll), action: { showAll.toggle() })
            }
        }
        .font(style.codeSmall).chatContainerSurface()
        .overlay(alignment: .topTrailing) {
            HStack(spacing: 2) {
                ChatContainerIconAction(symbol: copied ? "checkmark" : "doc.on.doc", label: "Copy file excerpt", action: copy)
                ChatContainerIconAction(symbol: "arrow.up.right.square", label: "Open in viewer", action: open).disabled(file == nil)
                    .chatLayoutProbe("open-excerpt-\(itemID)", action: open)
            }.padding(4).chatLayoutProbe("file-excerpt-actions-\(itemID)")
        }
        .chatLayoutProbe("output-block-\(itemID)")
        .chatLayoutProbe("file-excerpt-\(itemID)")
        .contextMenu {
            Button("Copy file excerpt", systemImage: "doc.on.doc", action: copy)
            Button("Open in viewer", systemImage: "arrow.up.right.square", action: open).disabled(file == nil)
        }
        .chatLayoutProbe("copy-output-\(itemID)", revision: text, action: copy)
        .onDisappear { copyResetTask?.cancel() }
        .padding(.bottom, 4)
    }
    private func copy() {
        UIPasteboard.general.string = ChatContainerText.excerptCopy(text); copied = true
        copyResetTask?.cancel()
        copyResetTask = Task {
            do { try await Task.sleep(for: .milliseconds(1500)); copied = false } catch { }
        }
    }
    private func open() {
        guard var file else { return }
        let line = firstLine
        file.line = line
        file.textStartLine = line ?? file.textStartLine
        file.availableText = ChatContainerText.excerptCopy(text)
        openFile(file)
    }
}

/// A wrapped, source-numbered diff with eight-line context folds and a fixed display cap.

/// Diffs are prepared once away from the UI executor and bounded in both rows and cache cost.
private actor ChatActivityDiffCache {
    static let shared = ChatActivityDiffCache()
    private final class Value: NSObject {
        let diff: ChatDiff
        init(_ diff: ChatDiff) { self.diff = diff }
    }
    private let cache: NSCache<NSString, Value> = {
        let cache = NSCache<NSString, Value>()
        cache.totalCostLimit = 32 * 1024 * 1024
        return cache
    }()
    func prepare(_ source: String) -> ChatDiff {
        let key = source as NSString
        if let known = cache.object(forKey: key) { return known.diff }
        let diff = Perf.interval("ChatDiffPrepare") { ChatDiff.parse(source, maxLines: 400) }
        cache.setObject(Value(diff), forKey: key, cost: source.utf8.count + diff.lines.reduce(0) { $0 + $1.text.utf8.count })
        return diff
    }
}

private struct ChatActivityDiff: View {
    @Environment(\.desktopStyle) private var style
    let text: String
    var snippet = false
    @State private var unfolded: Set<Int> = []
    @State private var prepared: ChatDiff?
    var body: some View {
        let parsed = prepared ?? ChatDiff()
        VStack(alignment: .leading, spacing: 0) {
            if snippet { Text("snippet").font(.system(size: 10)).foregroundStyle(style.muted) }
            if prepared == nil { ProgressView().controlSize(.small) }
            ForEach(displayLines(parsed.lines), id: \.index) { entry in
                if entry.hidden > 0 {
                    Button("… \(entry.hidden) unchanged lines") { unfolded.insert(entry.index - 3) }
                        .font(.system(size: 10, design: .monospaced)).foregroundStyle(style.muted).frame(minHeight: 36)
                } else {
                    lineView(entry.line)
                }
            }
            if parsed.hiddenLines > 0 { Text("… \(parsed.hiddenLines) more lines · Full diff on your Mac").font(.system(size: 10)).foregroundStyle(style.muted) }
        }
        .contextMenu { Button("Copy diff", systemImage: "doc.on.doc") { UIPasteboard.general.string = text } }
        .task(id: text) {
            let diff = await ChatActivityDiffCache.shared.prepare(text)
            guard !Task.isCancelled else { return }
            prepared = diff
        }
    }
    private struct Entry { let index: Int; let line: ChatDiffLine; var hidden = 0 }
    private func displayLines(_ lines: [ChatDiffLine]) -> [Entry] {
        var result: [Entry] = [], index = 0
        while index < lines.count {
            if lines[index].kind == .context {
                var end = index
                while end < lines.count && lines[end].kind == .context { end += 1 }
                if end - index > 8, !unfolded.contains(index) {
                    for leading in index..<(index + 3) { result.append(Entry(index: leading, line: lines[leading])) }
                    result.append(Entry(index: index + 3, line: lines[index], hidden: end - index - 6))
                    for trailing in (end - 3)..<end { result.append(Entry(index: trailing, line: lines[trailing])) }
                } else { for context in index..<end { result.append(Entry(index: context, line: lines[context])) } }
                index = end
            } else { result.append(Entry(index: index, line: lines[index])); index += 1 }
        }
        return result
    }
    private func lineView(_ line: ChatDiffLine) -> some View {
        HStack(alignment: .top, spacing: 0) {
            Text(line.oldLine.map(String.init) ?? "").frame(width: 27, alignment: .trailing).foregroundStyle(style.muted)
            Text(line.newLine.map(String.init) ?? "").frame(width: 27, alignment: .trailing).foregroundStyle(style.muted)
            Text(line.kind == .added ? "+" : (line.kind == .removed ? "−" : " "))
                .frame(width: 14).foregroundStyle(line.kind == .added ? style.added : style.removed)
            Text(line.text.isEmpty ? " " : line.text).foregroundStyle(line.kind == .hunk || line.kind == .meta ? style.muted : style.text)
                .fixedSize(horizontal: false, vertical: true).frame(maxWidth: .infinity, alignment: .leading)
        }
        .font(.system(size: 11, design: .monospaced)).textSelection(.enabled)
        .background(line.kind == .added ? style.added.opacity(0.12) : (line.kind == .removed ? style.removed.opacity(0.12) : .clear))
    }
}


/// The one-second clock belongs to the visible label, never the transcript/list owner.
private struct ChatActivityElapsed: View {
    let row: ChatProjectionRow
    let clockMS: UInt64
    var activity = false
    var body: some View {
        if !row.running { Text(activity ? row.title : row.durationMS.map(ChatProjection.formatDuration) ?? "") }
        else { TimelineView(.periodic(from: .now, by: 1)) { context in
            let elapsed = row.durationMS.map { duration in
                duration + (row.running ? UInt64(max(0, context.date.timeIntervalSince1970 * 1000 - Double(clockMS))) : 0)
            }
            if activity, row.running {
                Text(ChatProjection.activityTitle(running: true, duration: elapsed, current: row.currentLabel))
            } else if activity { Text(row.title) }
            else if let elapsed { Text(ChatProjection.formatDuration(elapsed)) }
        } }
    }
}
