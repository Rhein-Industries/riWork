import Foundation

/// Render-independent transcript projection. Keep this in step with the engine's
/// `chat/projection.rs`; both implementations execute spec/transcript-grouping/v2.
public enum ChatDisplayMode: String, Codable, Sendable, CaseIterable {
    case normal, verbose
}

public enum ChatProjectionRowKind: String, Codable, Sendable {
    case user, final, commentary, reasoning, shell, read, search, list
    case webSearch = "web_search", webFetch = "web_fetch"
    case mcp, subagent, other, edit, plan, todo, compaction, notice, request, group, activity
}

public struct ChatArtifactRef: Codable, Sendable, Equatable {
    public var itemID: String
    public var path: ChatPathRef?
    public var imageIndex: Int?
    public init(itemID: String, path: ChatPathRef? = nil, imageIndex: Int? = nil) {
        self.itemID = itemID; self.path = path; self.imageIndex = imageIndex
    }
    private enum CodingKeys: String, CodingKey { case itemID = "item_id", path, imageIndex = "image_index" }
}

/// Headers contain no provider output or error preview. Bodies are looked up by
/// item ID only after disclosure; children remain available while closed.
public struct ChatProjectionRow: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var kind: ChatProjectionRowKind
    public var title: String
    public var target: String?
    public var open: Bool
    public var children: [ChatProjectionRow]
    public var comments: [ChatProjectionRow]
    public var artifacts: [ChatArtifactRef]
    public var failedCount: Int
    public var running: Bool
    public var durationMS: UInt64?
    public var currentLabel: String?
    public var requestID: String?
    public var provisional: Bool
    public var elided: Bool
    public init(id: String = "", kind: ChatProjectionRowKind = .other, title: String = "", target: String? = nil,
                open: Bool = false, children: [ChatProjectionRow] = [], comments: [ChatProjectionRow] = [],
                artifacts: [ChatArtifactRef] = [], failedCount: Int = 0, running: Bool = false,
                durationMS: UInt64? = nil, currentLabel: String? = nil, requestID: String? = nil, provisional: Bool = false,
                elided: Bool = false) {
        self.id = id; self.kind = kind; self.title = title; self.target = target; self.open = open
        self.children = children; self.comments = comments; self.artifacts = artifacts; self.failedCount = failedCount
        self.running = running; self.durationMS = durationMS; self.currentLabel = currentLabel
        self.requestID = requestID; self.provisional = provisional; self.elided = elided
    }
    private enum CodingKeys: String, CodingKey {
        case id, kind, title, target, open, children, comments, artifacts, running, provisional, elided
        case failedCount = "failed_count", durationMS = "duration_ms", currentLabel = "current_label", requestID = "request_id"
    }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? ""
        kind = try c.decodeIfPresent(ChatProjectionRowKind.self, forKey: .kind) ?? .other
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        target = try c.decodeIfPresent(String.self, forKey: .target)
        open = try c.decodeIfPresent(Bool.self, forKey: .open) ?? false
        children = try c.decodeIfPresent([Self].self, forKey: .children) ?? []
        comments = try c.decodeIfPresent([Self].self, forKey: .comments) ?? []
        artifacts = try c.decodeIfPresent([ChatArtifactRef].self, forKey: .artifacts) ?? []
        failedCount = try c.decodeIfPresent(Int.self, forKey: .failedCount) ?? 0
        running = try c.decodeIfPresent(Bool.self, forKey: .running) ?? false
        durationMS = try c.decodeIfPresent(UInt64.self, forKey: .durationMS)
        currentLabel = try c.decodeIfPresent(String.self, forKey: .currentLabel)
        requestID = try c.decodeIfPresent(String.self, forKey: .requestID)
        provisional = try c.decodeIfPresent(Bool.self, forKey: .provisional) ?? false
        elided = try c.decodeIfPresent(Bool.self, forKey: .elided) ?? false
    }
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id); try c.encode(kind, forKey: .kind); try c.encode(title, forKey: .title)
        try c.encodeIfPresent(target, forKey: .target); try c.encode(open, forKey: .open)
        if !children.isEmpty { try c.encode(children, forKey: .children) }
        if !comments.isEmpty { try c.encode(comments, forKey: .comments) }
        if !artifacts.isEmpty { try c.encode(artifacts, forKey: .artifacts) }
        try c.encode(failedCount, forKey: .failedCount); try c.encode(running, forKey: .running)
        try c.encodeIfPresent(durationMS, forKey: .durationMS); try c.encodeIfPresent(currentLabel, forKey: .currentLabel)
        try c.encodeIfPresent(requestID, forKey: .requestID); try c.encode(provisional, forKey: .provisional)
        try c.encode(elided, forKey: .elided)
    }
}

/// Manual overrides survive closing their parent. Mode/bulk transitions reset them.
public struct ChatDisclosureState: Codable, Sendable, Equatable {
    public var overrides: [String: Bool]
    public var bulk: Bool?
    public init(overrides: [String: Bool] = [:], bulk: Bool? = nil) { self.overrides = overrides; self.bulk = bulk }
    public mutating func toggle(_ id: String, currentlyOpen: Bool) { overrides[id] = !currentlyOpen }
    public mutating func resetMode() { overrides = [:]; bulk = nil }
    public mutating func resetBulk(open: Bool) { overrides = [:]; bulk = open }
    fileprivate func isOpen(_ id: String, mode: ChatDisplayMode, activity: Bool = false) -> Bool {
        overrides[id] ?? ((activity || mode == .verbose) ? bulk ?? false : false)
    }
    private enum CodingKeys: String, CodingKey { case overrides, bulk }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        overrides = try c.decodeIfPresent([String: Bool].self, forKey: .overrides) ?? [:]
        bulk = try c.decodeIfPresent(Bool.self, forKey: .bulk)
    }
}

public struct ChatProjectionOptions: Codable, Sendable, Equatable {
    public var mode: ChatDisplayMode
    public var nowMS: UInt64?
    public var runningTurnID: String?
    public var turnTiming: [String: ChatTurnTiming]
    public var disclosure: ChatDisclosureState
    public var provisional: Bool
    public var elidedItems: Set<String>
    public init(mode: ChatDisplayMode = .normal, nowMS: UInt64? = nil, runningTurnID: String? = nil,
                turnTiming: [String: ChatTurnTiming] = [:], disclosure: ChatDisclosureState = .init(), provisional: Bool = false,
                elidedItems: Set<String> = []) {
        self.mode = mode; self.nowMS = nowMS; self.runningTurnID = runningTurnID
        self.turnTiming = turnTiming; self.disclosure = disclosure; self.provisional = provisional; self.elidedItems = elidedItems
    }
    private enum CodingKeys: String, CodingKey {
        case mode, nowMS = "now_ms", runningTurnID = "running_turn_id", turnTiming = "turn_timing", disclosure, provisional, elidedItems = "elided_items"
    }
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        mode = try c.decodeIfPresent(ChatDisplayMode.self, forKey: .mode) ?? .normal
        nowMS = try c.decodeIfPresent(UInt64.self, forKey: .nowMS)
        runningTurnID = try c.decodeIfPresent(String.self, forKey: .runningTurnID)
        turnTiming = try c.decodeIfPresent([String: ChatTurnTiming].self, forKey: .turnTiming) ?? [:]
        disclosure = try c.decodeIfPresent(ChatDisclosureState.self, forKey: .disclosure) ?? .init()
        provisional = try c.decodeIfPresent(Bool.self, forKey: .provisional) ?? false
        elidedItems = try c.decodeIfPresent(Set<String>.self, forKey: .elidedItems) ?? []
    }
}

public struct ChatPageWindow: Codable, Sendable, Equatable {
    public var start: Int
    public var end: Int
    public var provisional: Bool
}

public enum ChatProjection {
    public static let specVersion = 2

    public static func turnAlignedPage(items: [ChatItem], start: Int, end: Int, maxItems: Int? = nil) -> ChatPageWindow {
        var start = min(max(0, start), items.count)
        var end = max(min(max(0, end), items.count), start)
        guard start != end else { return .init(start: start, end: end, provisional: false) }
        while start > 0 && sameTurn(items[start - 1], items[start]) { start -= 1 }
        while end < items.count && sameTurn(items[end - 1], items[end]) { end += 1 }
        if let limit = maxItems { start = max(start, end - max(0, limit)) }
        let partial = start < end && ((start > 0 && sameTurn(items[start - 1], items[start]))
                                     || (end < items.count && sameTurn(items[end - 1], items[end])))
        return .init(start: start, end: end, provisional: partial)
    }

    public static func project(transcript: ChatTranscript, options: ChatProjectionOptions = .init()) -> [ChatProjectionRow] {
        var options = options
        for (turn, timing) in transcript.turnTiming where options.turnTiming[turn] == nil { options.turnTiming[turn] = timing }
        if options.runningTurnID == nil { options.runningTurnID = transcript.turnID }
        return project(items: transcript.items, itemSequences: transcript.itemSequences, requests: transcript.requests, options: options)
    }

    public static func project(items: [ChatItem], itemSequences: [String: UInt64] = [:], requests: [ChatRequestRecord] = [],
                               options: ChatProjectionOptions = .init()) -> [ChatProjectionRow] {
        var options = options
        var unique: [ChatItem] = [], index: [String: Int] = [:]
        for var item in items {
            if options.elidedItems.contains(item.id), case .agentMessage = item.body {
                item.body = .toolCall(server: nil, tool: "elided", input: .null, output: nil)
            }
            if item.presentation.semantics == nil { item.presentation.semantics = legacySemantics(item.body) }
            if let at = index[item.id] { unique[at] = item }
            else { index[item.id] = unique.count; unique.append(item) }
        }
        // Swift retains response elision as a native body case. Read the latest
        // deduplicated item, so a later hydration can restore its full content.
        for item in unique { if case .elided = item.body { options.elidedItems.insert(item.id) } }
        var rows: [ChatProjectionRow] = [], seenRequests = Set<String>(), start = 0
        let unplaced = requests.filter { record in
            !unique.contains { (record.turnID != nil && record.turnID == $0.turnID) || record.itemID == $0.id }
        }.sorted { $0.requestedSeq < $1.requestedSeq }
        while start < unique.count {
            let turn = unique[start].turnID ?? "legacy:\(unique[start].id)"
            var end = start + 1
            while end < unique.count && sameTurn(unique[start], unique[end]) { end += 1 }
            let turnItems = Array(unique[start..<end])
            let firstSeq = itemSequences[unique[start].id] ?? UInt64(start) * 2 + 1
            let nextSeq = end < unique.count ? itemSequences[unique[end].id] ?? UInt64.max : UInt64.max
            for record in unplaced where record.requestedSeq < firstSeq {
                if seenRequests.insert(record.requestID).inserted { rows.append(requestRow(record)) }
            }
            let turnRequests = requests.filter { record in
                record.turnID == turn
                || (record.turnID == nil && record.itemID.map { id in turnItems.contains { $0.id == id } } == true)
                || (record.turnID == nil && record.itemID == nil && record.requestedSeq >= firstSeq
                    && record.requestedSeq < nextSeq && !seenRequests.contains(record.requestID))
            }
            seenRequests.formUnion(turnRequests.map(\.requestID))
            let verbose = verboseRows(items: turnItems, orders: itemSequences, requests: turnRequests, turn: turn, options: options)
            if options.mode == .verbose { rows += verbose }
            else {
                rows += verbose.filter { $0.kind == .user }
                let work = turnItems.filter(isWork)
                if unique[start].turnID != nil || !work.isEmpty || options.runningTurnID == turn {
                    let id = "activity:\(turn)"
                    let running = options.runningTurnID == turn || work.contains { $0.status == .inProgress }
                    let current = work.last { $0.status == .inProgress }.map(shortLabel)
                    let duration = activityDuration(turn: turn, work: work, running: running, options: options)
                    let artifacts = turnItems.filter { ![.user, .final].contains(rowKind($0)) }.flatMap(artifacts)
                    let children = verbose.filter { ![.user, .final, .request, .compaction, .commentary, .notice].contains($0.kind) }.map(stripNormalDetails)
                    rows.append(.init(id: id, kind: .activity, title: activityTitle(work: work, running: running, duration: duration, current: current),
                                      open: options.disclosure.isOpen(id, mode: options.mode, activity: true), children: children, artifacts: artifacts,
                                      failedCount: work.filter(failed).count, running: running, durationMS: duration,
                                      currentLabel: current, provisional: options.provisional))
                }
                func outside(_ source: [ChatProjectionRow]) {
                    for row in source {
                        if [.request, .compaction, .final].contains(row.kind) { rows.append(row) }
                        else { outside(row.children) }
                    }
                }
                outside(verbose)
            }
            start = end
        }
        rows += requests.filter { !seenRequests.contains($0.requestID) }.sorted { $0.requestedSeq < $1.requestedSeq }.map(requestRow)
        return rows
    }

    private static func sameTurn(_ a: ChatItem, _ b: ChatItem) -> Bool { a.turnID != nil && a.turnID == b.turnID }

    private static func stripNormalDetails(_ source: ChatProjectionRow) -> ChatProjectionRow {
        var row = source
        row.artifacts = []; row.comments = []
        row.children = row.children.filter { ![.commentary, .request, .compaction, .final].contains($0.kind) }.map(stripNormalDetails)
        return row
    }

    private static func verboseRows(items: [ChatItem], orders: [String: UInt64], requests: [ChatRequestRecord], turn: String,
                                    options: ChatProjectionOptions) -> [ChatProjectionRow] {
        let byID = Dictionary(uniqueKeysWithValues: items.map { ($0.id, $0) })
        func parent(_ item: ChatItem) -> String? {
            guard let id = item.presentation.parentID, let owner = byID[id], rowKind(owner) == .subagent, id != item.id else { return nil }
            var visited: Set<String> = [item.id], next: String? = id
            while let id = next {
                guard visited.insert(id).inserted else { return nil }
                next = byID[id]?.presentation.parentID
            }
            return id
        }
        let roots = items.filter { parent($0) == nil }
        var nested: [String: [ChatItem]] = [:]
        for item in items { if let id = parent(item) { nested[id, default: []].append(item) } }
        func walk(_ items: [ChatItem], level: Int) -> [ChatProjectionRow] {
            if level > 64 {
                var pending = items, rows: [ChatProjectionRow] = []
                while let item = pending.popLast() {
                    rows.append(itemRow(item, options: options))
                    pending += (nested[item.id] ?? []).reversed()
                }
                return rows
            }
            var entries: [(seq: UInt64, at: Int, row: ChatProjectionRow, eligible: Bool)] = []
            for (at, item) in items.enumerated() {
                var row = itemRow(item, options: options)
                if let children = nested[item.id] { row.children = walk(children, level: level + 1) }
                let gated = requests.contains { $0.kind == .approval && $0.itemID == item.id }
                entries.append((orders[item.id] ?? UInt64(at) * 2 + 1, at, row, eligible(item, approvalGated: gated)))
            }
            for record in requests {
                let belongsHere = record.itemID.flatMap { id in nested.first { $0.value.contains { $0.id == id } }?.key }
                let thisParent = items.first?.presentation.parentID
                if belongsHere == thisParent || (belongsHere == nil && level == 0) {
                    entries.append((record.requestedSeq, Int.max, requestRow(record), false))
                }
            }
            entries.sort { $0.seq == $1.seq ? $0.at < $1.at : $0.seq < $1.seq }
            var rows: [ChatProjectionRow] = [], run: [ChatProjectionRow] = []
            for entry in entries {
                if entry.eligible || (entry.row.kind == .commentary && !run.isEmpty) { run.append(entry.row) }
                else { flush(rows: &rows, run: &run, turn: turn, options: options); rows.append(entry.row) }
            }
            flush(rows: &rows, run: &run, turn: turn, options: options)
            return rows
        }
        return walk(roots, level: 0)
    }

    private static func flush(rows: inout [ChatProjectionRow], run: inout [ChatProjectionRow], turn: String, options: ChatProjectionOptions) {
        guard run.filter({ $0.kind != .commentary }).count >= 2 else { rows += run; run = []; return }
        let comments = run.filter { $0.kind == .commentary }, members = run.filter { $0.kind != .commentary }
        run = []
        let buckets: [(ChatProjectionRowKind, String, String, String)] = [
            (.read, "Read", "file", "files"), (.search, "Searched", "pattern", "patterns"), (.list, "Listed", "directory", "directories"),
            (.webSearch, "Searched the web", "time", "times"), (.webFetch, "Fetched", "page", "pages"), (.mcp, "Used MCP", "time", "times"),
            (.shell, "Ran", "command", "commands"), (.reasoning, "Thought", "time", "times"), (.other, "Used", "tool", "tools")]
        let headers = buckets.compactMap { kind, verb, one, many -> String? in
            let count = members.filter { $0.kind == kind }.count
            return count > 0 ? "\(verb) \(count) \(count == 1 ? one : many)" : nil
        }
        var title = headers.prefix(3).joined(separator: " · ")
        if headers.count > 3 { title += " · +\(headers.count - 3) more" }
        let id = "g:\(turn):\(members[0].id)", current = members.last { $0.running }
        rows.append(.init(id: id, kind: .group, title: title, open: options.disclosure.isOpen(id, mode: options.mode), children: members,
                          comments: comments, failedCount: members.reduce(0) { $0 + $1.failedCount }, running: current != nil,
                          durationMS: sumDurations(members.map(\.durationMS)), currentLabel: current?.currentLabel, provisional: options.provisional))
    }

    private static func eligible(_ item: ChatItem, approvalGated: Bool) -> Bool {
        // Provider hints never bypass the actual command-body safety check.
        if case .command(let command, _, _, _) = item.body { return !approvalGated && isReadOnlyShell(command) }
        return switch rowKind(item) {
        case .read, .search, .list, .webSearch, .webFetch, .mcp, .other, .reasoning: true
        case .shell:
            !approvalGated && {
                if case .elided = item.body { return false }
                return item.presentation.semantics?.target.map(isReadOnlyShell) ?? false
            }()
        default: false
        }
    }

    public static func rowKind(_ item: ChatItem) -> ChatProjectionRowKind {
        switch item.body {
        case .userMessage: .user
        case .agentMessage: item.presentation.phase == .commentary ? .commentary : .final
        case .reasoning: .reasoning
        case .command: .shell
        case .fileChange: .edit
        case .plan: .plan
        case .todo: .todo
        case .compaction: .compaction
        case .notice: .notice
        default: ChatProjectionRowKind(rawValue: (item.presentation.semantics ?? legacySemantics(item.body))?.kind.rawValue ?? "other") ?? .other
        }
    }

    private static func itemRow(_ item: ChatItem, options: ChatProjectionOptions) -> ChatProjectionRow {
        let kind = rowKind(item)
        let title: String = switch kind {
        case .user: "User"
        case .final: "Answer"
        case .commentary: "Commentary"
        case .read: "Read"
        case .search: "Searched"
        case .list: "Listed"
        case .webSearch: "Searched the web"
        case .webFetch: "Fetched"
        case .mcp: "Used MCP"
        case .shell: "Ran"
        case .subagent: "Agent"
        case .reasoning: "Thought"
        case .edit: "Edited"
        case .plan: "Plan"
        case .todo: "Todo"
        case .compaction: "Context compacted"
        case .notice: "Notice"
        default: "Used tool"
        }
        let target: String?
        if case .command(let command, _, _, _) = item.body { target = command }
        else if let hint = item.presentation.semantics?.target { target = hint }
        else {
            switch item.body {
            case .webSearch(let query): target = query
            case .fileChange(let changes) where changes.count == 1: target = changes[0].path
            default: target = nil
            }
        }
        let running = isWork(item) && item.status == .inProgress
        let open = ![.user, .final, .commentary, .compaction, .notice].contains(kind) && options.disclosure.isOpen(item.id, mode: options.mode)
        return .init(id: item.id, kind: kind, title: title, target: target, open: open, artifacts: artifacts(item),
                     failedCount: isWork(item) && failed(item) ? 1 : 0, running: running,
                     durationMS: itemDuration(item, nowMS: options.nowMS), currentLabel: running ? shortLabel(item) : nil,
                     elided: options.elidedItems.contains(item.id))
    }

    private static func artifacts(_ item: ChatItem) -> [ChatArtifactRef] {
        (item.presentation.semantics?.produced ?? []).map { .init(itemID: item.id, path: $0) }
        + item.presentation.images.indices.map { .init(itemID: item.id, imageIndex: $0) }
    }
    private static func failed(_ item: ChatItem) -> Bool {
        if item.status == .failed { return true }
        if case .command(_, _, _, let exit?) = item.body { return exit != 0 }
        return false
    }
    private static func isWork(_ item: ChatItem) -> Bool {
        switch item.body { case .userMessage, .agentMessage, .notice, .compaction: false; default: true }
    }
    private static func difference(_ end: UInt64?, _ start: UInt64?) -> UInt64? {
        guard let end, let start, end >= start else { return nil }; return end - start
    }
    private static func itemDuration(_ item: ChatItem, nowMS: UInt64?) -> UInt64? {
        difference(item.endedAt ?? (item.status == .inProgress ? nowMS : nil), item.startedAt)
    }
    private static func sumDurations(_ durations: [UInt64?]) -> UInt64? {
        durations.compactMap { $0 }.reduce(nil) { total, duration in
            let sum = (total ?? 0).addingReportingOverflow(duration)
            return sum.overflow ? UInt64.max : sum.partialValue
        }
    }
    private static func activityDuration(turn: String, work: [ChatItem], running: Bool, options: ChatProjectionOptions) -> UInt64? {
        if let timing = options.turnTiming[turn], let start = timing.startedAt, let end = timing.endedAt ?? (running ? options.nowMS : nil) {
            return difference(end, start)
        }
        // Without a turn clock only known item intervals can contribute.
        return sumDurations(work.map { itemDuration($0, nowMS: options.nowMS) })
    }
    /// Shared activity wording for projection and the row-local elapsed clock.
    public static func activityTitle(work: [ChatItem] = [], running: Bool, duration: UInt64?, current: String?) -> String {
        let time = duration.map { " \(formatDuration($0))" } ?? ""
        if running { return "Working" + time + (current.map { " · \($0)" } ?? "") }
        let edits = Set(work.flatMap { item -> [String] in if case .fileChange(let changes) = item.body { return changes.map(\.path) }; return [] })
        let commands = work.filter { rowKind($0) == .shell }.count
        let reads = work.filter { [.read, .search, .list, .webSearch, .webFetch].contains(rowKind($0)) }.count
        let calls = work.filter { item in
            [.mcp, .other].contains(rowKind(item)) || (rowKind(item) == .subagent && !work.contains { $0.presentation.parentID == item.id })
        }
        let mcpOnly = calls.allSatisfy { rowKind($0) == .mcp }
        let buckets = [(edits.count, "file edited", "files edited"), (commands, "command", "commands"), (reads, "read", "reads"),
                       (calls.count, mcpOnly ? "MCP call" : "tool call", mcpOnly ? "MCP calls" : "tool calls")]
        return "Worked" + time + buckets.filter { $0.0 > 0 }.map { " · \($0.0) \($0.0 == 1 ? $0.1 : $0.2)" }.joined()
    }
    private static func shortLabel(_ item: ChatItem) -> String {
        let bodyTarget: String? = switch item.body {
        case .fileChange(let changes): changes.first?.path
        case .toolCall(_, let tool, _, _): tool
        default: nil
        }
        let target = item.presentation.semantics?.target ?? bodyTarget ?? ""
        let verb: String = switch rowKind(item) {
        case .read: "reading"
        case .search, .webSearch: "searching"
        case .list: "listing"
        case .webFetch: "fetching"
        case .reasoning: "thinking"
        case .subagent: "agent"
        case .edit: "editing"
        case .shell: "running"
        case .plan: "planning"
        case .todo: "updating todo"
        default: "using tool"
        }
        return target.isEmpty ? verb : "\(verb) \(target)"
    }
    public static func formatDuration(_ ms: UInt64) -> String {
        let seconds = ms / 1000
        return seconds >= 60 ? "\(seconds / 60)m \(seconds % 60)s" : "\(seconds)s"
    }
    private static func requestRow(_ record: ChatRequestRecord) -> ChatProjectionRow {
        let prefix: String = switch record.resolution {
        case "accept", "accept_for_session": "Allowed"
        case "decline": "Declined"
        case "cancel": "Cancelled"
        case "answered": "Answered"
        case .some: "Resolved"
        case .none: record.resolvedSeq != nil ? "Resolved" : record.kind == .question ? "Question pending" : "Approval pending"
        }
        var title = "\(prefix): \(record.title)"
        if let summary = record.answerSummary, !summary.isEmpty { title += " · \(summary)" }
        return .init(id: "request:\(record.requestID)", kind: .request, title: title, requestID: record.requestID)
    }

    public static func visibleAnchor(rows: [ChatProjectionRow], itemID: String) -> String? {
        func contains(_ row: ChatProjectionRow) -> Bool {
            row.id == itemID || row.children.contains(where: contains) || row.comments.contains(where: contains)
        }
        for row in rows {
            if row.id == itemID { return row.id }
            if let anchor = visibleAnchor(rows: row.comments, itemID: itemID) { return anchor }
            if contains(row) {
                return row.open ? visibleAnchor(rows: row.children, itemID: itemID) ?? visibleAnchor(rows: row.comments, itemID: itemID) : row.id
            }
        }
        return nil
    }

    public static func remapDisclosures(state: ChatDisclosureState, before: [ChatProjectionRow], after: [ChatProjectionRow]) -> ChatDisclosureState {
        func find(_ rows: [ChatProjectionRow], _ id: String) -> ChatProjectionRow? {
            for row in rows { if row.id == id { return row }; if let child = find(row.children, id) { return child } }; return nil
        }
        func firstLeaf(_ row: ChatProjectionRow) -> String { row.children.first.map(firstLeaf) ?? row.id }
        func groupFor(_ rows: [ChatProjectionRow], _ anchor: String) -> String? {
            for row in rows {
                if row.kind == .group && find(row.children, anchor) != nil { return row.id }
                if let id = groupFor(row.children, anchor) { return id }
            }
            return nil
        }
        var result = state
        for (id, open) in state.overrides where id.hasPrefix("g:") && find(after, id) == nil {
            if let old = find(before, id), let newID = groupFor(after, firstLeaf(old)) {
                result.overrides.removeValue(forKey: id)
                if state.overrides[newID] == nil { result.overrides[newID] = (result.overrides[newID] ?? false) || open }
            }
        }
        return result
    }

    public static func selectionLeaves(rows: [ChatProjectionRow]) -> [(id: String, text: String)] {
        rows.flatMap { row in
            [(row.id, row.title + (row.target.map { " \($0)" } ?? ""))]
            + (row.open ? selectionLeaves(rows: row.children) : []) + selectionLeaves(rows: row.comments)
        }
    }

    /// Deliberately weaker legacy classification, identical to engine `enrich`.
    private static func legacySemantics(_ body: ChatItemBody) -> ChatSemantics? {
        switch body {
        case .command(let command, _, _, _): return .init(kind: .shell, target: command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : command)
        case .webSearch(let query): return .init(kind: .webSearch, target: query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : query)
        case .toolCall(let server, let tool, let input, _):
            let name = tool.lowercased().replacingOccurrences(of: "_", with: "").replacingOccurrences(of: "-", with: "")
            let kind: ChatToolKind
            if server != nil || tool.hasPrefix("mcp__") { kind = .mcp }
            else {
                kind = switch name {
                case "read", "readfile", "fileread": .read
                case "grep", "search", "searchfiles", "filesearch", "ripgrep": .search
                case "glob", "ls", "list", "listfiles", "listdirectory": .list
                case "websearch": .webSearch
                case "webfetch", "fetchurl": .webFetch
                case "bash", "shell", "exec", "execcommand", "terminal", "command": .shell
                case "task", "agent", "subagent", "collabagenttoolcall": .subagent
                default: .other
                }
            }
            let keys: [String] = switch kind {
            case .read: ["file_path", "path", "filename", "file"]
            case .search: ["pattern", "query", "search"]
            case .list: ["pattern", "path", "directory"]
            case .webSearch: ["query", "search_term"]
            case .webFetch: ["url", "uri"]
            case .shell: ["command", "cmd"]
            case .subagent: ["description", "objective", "prompt"]
            case .mcp, .other: ["file_path", "path", "query", "url", "command"]
            }
            // Unknown inferred tools remain absent so provider-only lifecycle
            // metadata can still be inherited by the transcript adapter.
            guard kind != .other else { return nil }
            return .init(kind: kind, target: keys.compactMap { input[$0].string }.first { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty })
        default: return nil
        }
    }

    /// Quote-aware display classifier, never a command authorization mechanism.
    static func isReadOnlyShell(_ command: String) -> Bool {
        guard let commands = shellCommands(command), !commands.isEmpty else { return false }
        return commands.allSatisfy { words in
            guard let program = words.first else { return false }
            let args = Array(words.dropFirst())
            switch program {
            case "ls", "cat", "head", "tail", "grep": return true
            case "rg": return !args.contains { $0 == "--pre" || $0.hasPrefix("--pre=") || $0 == "--hostname-bin" || $0.hasPrefix("--hostname-bin=") }
            case "find": return !args.contains { ["-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls"].contains($0) }
            case "git": return args.first.map { ["status", "log", "diff", "show"].contains($0) } == true
                && !args.contains { $0 == "--output" || $0.hasPrefix("--output=") || $0 == "--ext-diff" || $0 == "--textconv" }
            case "sed":
                guard args.count >= 2, args[0] == "-n" else { return false }
                return args[1].split(separator: ";", omittingEmptySubsequences: false).allSatisfy { part in
                    let script = part.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard script.hasSuffix("p") else { return false }
                    return script.dropLast().allSatisfy { ("0"..."9").contains(String($0)) || [",", "$", " ", "\t"].contains($0) }
                } && args.dropFirst(2).allSatisfy { !$0.hasPrefix("-") }
            default: return false
            }
        }
    }
    private static func shellCommands(_ command: String) -> [[String]]? {
        let chars = Array(command)
        var at = 0, commands: [[String]] = [], words: [String] = [], word = "", started = false, quote: Character?
        while at < chars.count {
            let ch = chars[at]; at += 1
            if quote == "'" { if ch == "'" { quote = nil } else { word.append(ch) }; continue }
            if ch == "$" || ch == "`" { return nil }
            if ch == "\\" { guard at < chars.count else { return nil }; word.append(chars[at]); at += 1; started = true; continue }
            if quote == "\"" { if ch == "\"" { quote = nil } else { word.append(ch) }; continue }
            switch ch {
            case "'", "\"": quote = ch; started = true
            case "<", ">", "(", ")": return nil
            case "|", "&", ";", "\n":
                if started { words.append(word); word = ""; started = false }
                guard !words.isEmpty else { return nil }
                if ch == "&" { guard at < chars.count, chars[at] == "&" else { return nil }; at += 1 }
                if ch == "|" && at < chars.count && chars[at] == "|" { at += 1 }
                commands.append(words); words = []
            default:
                if ch.isWhitespace { if started { words.append(word); word = ""; started = false } }
                else { word.append(ch); started = true }
            }
        }
        guard quote == nil else { return nil }
        if started { words.append(word) }
        guard !words.isEmpty else { return nil }
        commands.append(words)
        return commands
    }
}


/// A conversation owns one cache. Token/output deltas do not change grouping; disclosure
/// only walks the small projection, without inspecting provider bodies or classifying calls.
public final class ChatProjectionCache {
    private struct Entry {
        var revision: UUID
        var provisional: Bool
        var rows: [ChatProjectionRow]
        var clockMS: UInt64
    }
    private var entries: [ChatDisplayMode: Entry] = [:]
    private var disclosed: (revision: UUID, mode: ChatDisplayMode, state: ChatDisclosureState, provisional: Bool, rows: [ChatProjectionRow])?
    public private(set) var builds = 0
    public private(set) var clockMS: UInt64 = 0
    public init() {}

    public func project(transcript: ChatTranscript, mode: ChatDisplayMode = .normal,
                        disclosure: ChatDisclosureState = .init(), provisional: Bool = false,
                        nowMS: UInt64 = UInt64(Date().timeIntervalSince1970 * 1000)) -> [ChatProjectionRow] {
        if let known = disclosed, known.revision == transcript.projectionRevision, known.mode == mode,
           known.state == disclosure, known.provisional == provisional { return known.rows }
        let entry: Entry
        if let known = entries[mode], known.revision == transcript.projectionRevision, known.provisional == provisional {
            entry = known
        } else {
            entry = Entry(revision: transcript.projectionRevision, provisional: provisional,
                          rows: ChatProjection.project(transcript: transcript, options: .init(mode: mode, nowMS: nowMS, provisional: provisional)), clockMS: nowMS)
            entries[mode] = entry; builds += 1
        }
        clockMS = entry.clockMS
        func disclose(_ source: ChatProjectionRow) -> ChatProjectionRow {
            var row = source
            if ![.user, .final, .commentary, .compaction, .notice, .request].contains(row.kind) {
                row.open = disclosure.isOpen(row.id, mode: mode, activity: row.kind == .activity)
            }
            row.children = row.children.map(disclose)
            row.comments = row.comments.map(disclose)
            return row
        }
        let rows = entry.rows.map(disclose)
        disclosed = (transcript.projectionRevision, mode, disclosure, provisional, rows)
        return rows
    }
}
