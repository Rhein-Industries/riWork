import Foundation

// The desktop's global dashboard, as `dashboard.get` sends it (docs/remote-protocol.md, "Dashboard extension"): the work on the Mac
// across every project, arranged by the desktop's own `global_overview::build`. A desktop offers it with `ready.features.dashboard`; an
// older one does not, and the phone then says why there is no dashboard and still lists and opens the projects.
//
// Reading is lenient, as for every additive field: a figure that is missing, of another type or out of range is unknown (nil), drawn as
// unknown and never as 0; a row the phone cannot read is left out and the rest of the list stands. Only a reply that is not an object, or
// says another version than 1, is unreadable as a whole. Everything here is pure and `Sendable`.

/// Agents by what they are doing. Each figure is read on its own: nil is unknown.
public struct DashboardAgentCounts: Sendable, Equatable {
    public var working: Int?, waiting: Int?, done: Int?, unknown: Int?, exited: Int?, subagents: Int?
    public init(working: Int? = nil, waiting: Int? = nil, done: Int? = nil, unknown: Int? = nil, exited: Int? = nil, subagents: Int? = nil) {
        self.working = working; self.waiting = waiting; self.done = done; self.unknown = unknown; self.exited = exited; self.subagents = subagents
    }
    /// An object with the counts; nil for anything else (then every figure is unknown).
    init?(wire value: JSONValue) {
        guard case .object = value else { return nil }
        self.init(working: LenientNumber.count(value["working"]), waiting: LenientNumber.count(value["waiting"]), done: LenientNumber.count(value["done"]),
                  unknown: LenientNumber.count(value["unknown"]), exited: LenientNumber.count(value["exited"]), subagents: LenientNumber.count(value["subagents"]))
    }
}

/// Tasks by status. Nil is unknown.
public struct DashboardTaskCounts: Sendable, Equatable {
    public var todo: Int?, inProgress: Int?, done: Int?
    public init(todo: Int? = nil, inProgress: Int? = nil, done: Int? = nil) { self.todo = todo; self.inProgress = inProgress; self.done = done }
    init?(wire value: JSONValue) {
        guard case .object = value else { return nil }
        self.init(todo: LenientNumber.count(value["todo"]), inProgress: LenientNumber.count(value["in_progress"]), done: LenientNumber.count(value["done"]))
    }
    /// To do and in progress, when both are known.
    public var open: Int? { todo.flatMap { todo in inProgress.map { todo + $0 } } }
    /// Every task, when each status is known.
    public var total: Int? { open.flatMap { open in done.map { open + $0 } } }
}

/// Which orchestrator a session is, if it is one.
public enum DashboardOrchestratorScope: String, Sendable, Equatable { case global, project }

/// Where a dashboard row leads, as the desktop says.
public enum DashboardTarget: Sendable, Equatable {
    case shell(id: String)
    case chat(id: String, projectID: String?, orchestrator: DashboardOrchestratorScope?)
    case project(id: String)
    case task(id: String)
    case automations(projectID: String?)

    /// Nil for a target the phone cannot read (a kind it does not know, an id that is no UUID).
    init?(wire value: JSONValue) {
        let id = DashboardWire.uuid(value["id"])
        switch value["type"].string {
        case "shell": guard let id else { return nil }; self = .shell(id: id)
        case "chat": guard let id else { return nil }
            self = .chat(id: id, projectID: DashboardWire.uuid(value["project_id"]), orchestrator: value["orchestrator"].string.flatMap(DashboardOrchestratorScope.init(rawValue:)))
        case "project": guard let id else { return nil }; self = .project(id: id)
        case "task": guard let id = id ?? DashboardWire.text(value["id"]) else { return nil }; self = .task(id: id)
        case "automations": self = .automations(projectID: DashboardWire.uuid(value["project_id"]))
        default: return nil
        }
    }
}

/// A session on a row: who runs in it and where.
public struct DashboardSession: Sendable, Equatable {
    public let target: DashboardTarget
    /// "Codex", "Claude", "Grok", "Muse", "Orchestrator", "Shell" (or a word a newer desktop uses).
    public let agent: String
    /// A chat's own title.
    public let title: String?
    public let projectID: String?
    public let projectName: String?
    public let branch: String?
    public let orchestrator: DashboardOrchestratorScope?
    public let isChat: Bool

    init?(wire value: JSONValue) {
        guard let target = DashboardTarget(wire: value["target"]) else { return nil }
        self.target = target
        agent = DashboardWire.text(value["agent"]) ?? "Agent"
        title = DashboardWire.text(value["title"])
        projectID = DashboardWire.uuid(value["project_id"])
        projectName = DashboardWire.text(value["project_name"])
        branch = DashboardWire.text(value["branch"])
        orchestrator = value["orchestrator"].string.flatMap(DashboardOrchestratorScope.init(rawValue:))
        isChat = value["is_chat"] == .bool(true)
    }
    /// What the row calls it: the global or a project's orchestrator by that name, a chat by its title, else the agent.
    public var name: String {
        switch orchestrator {
        case .global: return "Global orchestrator"
        case .project: return "Project orchestrator"
        case nil: return title ?? agent
        }
    }
    /// Where it is: the project (or "Global" for an orchestrator of no project) and the worktree's branch.
    public var place: String {
        let scope = orchestrator == .global ? "Global" : (projectName ?? (projectID == nil ? nil : "Removed project"))
        return [scope, branch].compactMap { $0 }.joined(separator: " · ")
    }
}

/// An agent at work, or one that finished recently.
public struct DashboardWork: Sendable, Equatable {
    public let session: DashboardSession
    public let activity: AgentActivity
    public let sinceUnix: UInt64?
    /// Subagents at work under it; nil when not said.
    public let subagents: Int?
    init?(wire value: JSONValue) {
        guard let session = DashboardSession(wire: value["session"]) else { return nil }
        self.session = session
        activity = AgentActivity(wire: value["activity"])
        sinceUnix = LenientNumber.seconds(value["since_unix"])
        subagents = LenientNumber.count(value["subagents"])
    }
}

/// Why a row needs a person.
public enum DashboardAttentionKind: Sendable, Equatable {
    /// An agent waits for an answer or an approval.
    case waiting
    /// A chat's agent failed; the next message tries to resume it.
    case chatFailed(message: String)
    /// An automation's last run did not go through (`outcome`: missed, failed, uncertain).
    case automation(title: String, outcome: String, message: String)
    /// A kind a newer desktop added.
    case other(String)
}

/// Exact provider controls, kept separate from descriptive attention or AI summaries.
public struct DashboardDecision: Sendable, Equatable, Identifiable {
    public enum Content: Sendable, Equatable {
        case approval(ChatApproval), question(ChatQuestion)
    }
    public let id: String
    public let chatID: String
    public let requestID: String
    public let session: DashboardSession
    public let observedAt: UInt64?
    public let source: DashboardEventSource
    public let content: Content
    public var title: String {
        switch content { case .approval(let approval): approval.title; case .question(let question): question.questions.first?.question ?? "Question" }
    }
    init?(wire value: JSONValue) {
        // An identifier is answered exactly as the provider sent it: never trimmed or cut like display text.
        guard let chatID = DashboardWire.uuid(value["chat_id"]), let requestID = value["request_id"].string, !requestID.isEmpty,
              requestID.utf8.count <= 512, let session = DashboardSession(wire: value["session"]),
              case .chat(let targetID, _, _) = session.target, targetID == chatID,
              let source = DashboardEventSource(wire: value["source"]), source.chatID == chatID else { return nil }
        let content: Content
        switch value["kind"].string {
        case "approval":
            guard let approval = try? value["approval"].decode(ChatApproval.self), approval.requestID == requestID else { return nil }
            content = .approval(approval)
        case "question":
            // Answers go by position: a prompt or option this phone cannot read would shift or hide them, so the card is left out whole.
            let prompts = value["question"]["questions"].array
            guard let question = try? value["question"].decode(ChatQuestion.self), question.requestID == requestID,
                  !question.questions.isEmpty, question.questions.count <= 16, prompts.count == question.questions.count,
                  zip(prompts, question.questions).allSatisfy({ $0["options"].array.count == $1.options.count }) else { return nil }
            content = .question(question)
        default: return nil
        }
        self.chatID = chatID; self.requestID = requestID; self.session = session; self.source = source; self.content = content
        self.id = "\(chatID):\(value["kind"].string ?? ""):\(requestID)"
        observedAt = LenientNumber.seconds(value["observed_at"])
    }
    public enum Check: Sendable, Equatable {
        /// The same request, still pending with the same body and choices.
        case current
        /// Resolved, changed, another chat, or not a response this card offers.
        case changed
        /// The host's controls for this request could not be read in full: nothing can vouch for it from here.
        case unreadable
    }
    /// Recheck the complete current control set immediately before an explicit response. Cached cards cannot authorize a changed
    /// request, another chat, or a choice the provider never offered. Descriptive/AI-generated rows never reach this path. Only this
    /// request's own controls count: an unrelated row or request the phone cannot read leaves it answerable.
    public func check(_ command: ChatCommand, snapshot: ChatSnapshotReply) -> Check {
        guard snapshot.v == 1, snapshot.chatID == chatID else { return .changed }
        if snapshot.unattributedRequestControl || snapshot.partialRequestIDs.contains(requestID) { return .unreadable }
        var transcript = ChatTranscript()
        snapshot.controls.forEach { transcript.apply($0) }
        if transcript.elidedRequests.contains(where: { $0.requestID == requestID }) { return .unreadable }
        guard transcript.state == .waiting else { return .changed }
        let current: Bool
        switch (content, command) {
        case (.approval(let approval), .approve(let id, let choice)):
            current = id == requestID && !approval.choices.isEmpty && approval.choices.contains(choice)
                && transcript.approvals.first(where: { $0.requestID == requestID }) == approval
        case (.question(let question), .answer(let id, let answers)):
            current = id == requestID && transcript.questions.first(where: { $0.requestID == requestID }) == question && answers.count == question.questions.count
                && answers.allSatisfy { !$0.isEmpty && $0.allSatisfy { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty } }
        default: current = false
        }
        return current ? .current : .changed
    }
    public func validates(_ command: ChatCommand, snapshot: ChatSnapshotReply) -> Bool { check(command, snapshot: snapshot) == .current }
}

public struct DashboardEventSource: Sendable, Equatable {
    public let chatID: String
    public let eventSequence: UInt64
    public let eventID: String
    init?(wire value: JSONValue) {
        guard let chatID = DashboardWire.uuid(value["chat_id"]), let sequence = LenientNumber.seconds(value["event_seq"]),
              value["event_id"].string == "chat:\(chatID):seq:\(sequence)" else { return nil }
        self.chatID = chatID; eventSequence = sequence; eventID = "chat:\(chatID):seq:\(sequence)"
    }
}

public struct DashboardDecisionSources: Sendable, Equatable {
    public let state: String
    public let sampledAt: UInt64?
    public let chatsRead: Int?
    public let chatsComplete: Int?
    public let chatsTotal: Int?
    public var complete: Bool { state == "read" }
    init(wire value: JSONValue) {
        state = value["state"].string ?? "unavailable"
        sampledAt = LenientNumber.seconds(value["sampled_at"])
        chatsRead = LenientNumber.count(value["chats_read"]); chatsComplete = LenientNumber.count(value["chats_complete"])
        chatsTotal = LenientNumber.count(value["chats_total"])
    }
}

public struct DashboardRecentActivity: Sendable, Equatable, Identifiable {
    public let session: DashboardSession
    public let source: DashboardEventSource
    public let observedAt: UInt64?
    public let kind: String
    public let text: String
    public let status: String?
    public var id: String { source.eventID }
    init?(wire value: JSONValue) {
        guard let session = DashboardSession(wire: value["session"]), let source = DashboardEventSource(wire: value["source"]),
              let text = DashboardWire.text(value["text"]) else { return nil }
        self.session = session; self.source = source; self.text = text
        observedAt = LenientNumber.seconds(value["observed_at"]); kind = value["kind"].string ?? "activity"
        status = DashboardWire.text(value["status"])
    }
}

public struct DashboardIntelligence: Sendable, Equatable {
    public struct Insight: Sendable, Equatable, Identifiable {
        /// Its place in its list: the same text from the same events can come twice, and each is a row of its own.
        public let id: Int
        public let text: String
        public let sourceEventIDs: [String]
    }
    public let state: String
    public let model: String?
    public let generatedAt: UInt64?
    public let summary: String?
    public let highlights: [Insight]
    public let followups: [Insight]
    public let error: String?
    public let stale: Bool
    init(wire value: JSONValue) {
        state = value["state"].string ?? "disabled"
        model = DashboardWire.text(value["model"])
        generatedAt = LenientNumber.seconds(value["generated_at"])
        summary = DashboardWire.text(value["summary"], limit: 600)
        error = DashboardWire.text(value["error"]).map { String($0.prefix(300)) }
        stale = value["stale"] == .bool(true)
        func insights(_ value: JSONValue) -> [Insight] {
            let rows = value.array.prefix(8).compactMap { row -> (text: String, sources: [String])? in
                guard let text = DashboardWire.text(row["text"]) else { return nil }
                let sources = row["source_event_ids"].array.prefix(8).compactMap(\.string)
                guard !sources.isEmpty, sources.allSatisfy({ $0.hasPrefix("chat:") && $0.utf8.count <= 512 }) else { return nil }
                return (String(text.prefix(300)), Array(sources))
            }
            return rows.enumerated().map { Insight(id: $0.offset, text: $0.element.text, sourceEventIDs: $0.element.sources) }
        }
        highlights = insights(value["highlights"]); followups = insights(value["followups"])
    }
}

public struct DashboardAttention: Sendable, Equatable {
    public let kind: DashboardAttentionKind
    public let target: DashboardTarget
    public let session: DashboardSession?
    public let projectID: String?
    public let projectName: String?
    /// When it happened; nil when not known.
    public let at: UInt64?
    init?(wire value: JSONValue) {
        guard let target = DashboardTarget(wire: value["target"]), let word = value["kind"].string else { return nil }
        let message = DashboardWire.text(value["message"]) ?? ""
        switch word {
        case "waiting": kind = .waiting
        case "chat_failed": kind = .chatFailed(message: message)
        case "automation": kind = .automation(title: DashboardWire.text(value["title"]) ?? "Automation", outcome: value["outcome"].string ?? "failed", message: message)
        default: kind = .other(word)
        }
        self.target = target
        session = DashboardSession(wire: value["session"])
        projectID = DashboardWire.uuid(value["project_id"])
        projectName = DashboardWire.text(value["project_name"])
        at = LenientNumber.seconds(value["at"])
    }
    public var isWaiting: Bool { kind == .waiting }
}

/// How a project's own orchestrator runs.
public enum DashboardProjectOrchestrator: Sendable, Equatable {
    case terminal(activity: AgentActivity?)
    case chat(provider: ChatProvider?, state: ChatState)
    init?(wire value: JSONValue) {
        switch value["mode"].string {
        case "terminal": self = .terminal(activity: value["activity"] == .null ? nil : AgentActivity(wire: value["activity"]))
        case "chat": self = .chat(provider: DashboardWire.provider(value["provider"]), state: DashboardWire.chatState(value["chat_state"], message: value["message"]))
        default: return nil
        }
    }
}

public struct DashboardProject: Sendable, Equatable, Identifiable {
    public let id: String
    public let name: String
    public let folder: String?
    public let agents: DashboardAgentCounts?
    public let liveShells: Int?
    /// Nil while the desktop has not read its chats.
    public let liveChats: Int?
    public let tasks: DashboardTaskCounts?
    public let worktrees: Int?
    /// Rows of this project in the attention list.
    public let attention: Int?
    public let orchestrator: DashboardProjectOrchestrator?
    /// The latest thing recorded happening in it; nil when nothing recorded a time.
    public let lastActivity: UInt64?
    init?(wire value: JSONValue, chatsKnown: Bool?) {
        guard let id = DashboardWire.uuid(value["id"]), let name = DashboardWire.text(value["name"]) else { return nil }
        self.id = id; self.name = name
        folder = DashboardWire.text(value["folder"])
        agents = DashboardAgentCounts(wire: value["agents"])
        liveShells = LenientNumber.count(value["live_shells"])
        liveChats = chatsKnown == true ? LenientNumber.count(value["live_chats"]) : nil
        tasks = DashboardTaskCounts(wire: value["tasks"])
        worktrees = LenientNumber.count(value["worktrees"])
        attention = LenientNumber.count(value["attention"])
        orchestrator = DashboardProjectOrchestrator(wire: value["orchestrator"])
        lastActivity = LenientNumber.seconds(value["last_activity"])
    }
    /// Live terminals and chats, when both are known.
    public var liveSessions: Int? { liveShells.flatMap { shells in liveChats.map { shells + $0 } } }
}

public struct DashboardTask: Sendable, Equatable, Identifiable {
    public let id: String
    public let title: String
    public let projectID: String
    public let projectName: String
    public let branch: String?
    public let updatedAt: UInt64?
    init?(wire value: JSONValue) {
        // A task's id is only its row's identity (the phone sends no task request), so any id will do; its project is opened by id.
        guard let id = DashboardWire.text(value["id"]), let projectID = DashboardWire.uuid(value["project_id"]) else { return nil }
        self.id = id; self.projectID = projectID
        title = DashboardWire.text(value["title"]) ?? "Untitled task"
        projectName = DashboardWire.text(value["project_name"]) ?? "Project"
        branch = DashboardWire.text(value["branch"])
        updatedAt = LenientNumber.seconds(value["updated_at"])
    }
}

/// The global orchestrator, as the desktop finds it: a live terminal first, else a chat, else none.
public enum DashboardGlobal: Sendable, Equatable {
    /// The desktop has not read its chats yet and no terminal one runs: whether there is one is not known.
    case unknown
    case none(exitedTerminal: Bool)
    case terminal(shellID: String, activity: AgentActivity?)
    case chat(chatID: String, provider: ChatProvider?, state: ChatState)
    init(wire value: JSONValue) {
        switch value["kind"].string {
        case "none": self = .none(exitedTerminal: value["exited_terminal"] == .bool(true))
        case "terminal":
            if let id = DashboardWire.uuid(value["shell_id"]) { self = .terminal(shellID: id, activity: value["activity"] == .null ? nil : AgentActivity(wire: value["activity"])) }
            else { self = .unknown }
        case "chat":
            if let id = DashboardWire.uuid(value["chat_id"]) { self = .chat(chatID: id, provider: DashboardWire.provider(value["provider"]), state: DashboardWire.chatState(value["chat_state"], message: value["message"])) }
            else { self = .unknown }
        default: self = .unknown
        }
    }
}

public struct DashboardAutomation: Sendable, Equatable, Identifiable {
    public let id: String
    public let title: String
    /// Nil for an app automation.
    public let projectID: String?
    public let projectName: String?
    public let paused: Bool
    public let nextRun: UInt64?
    /// How its last run went (`outcome` as the desktop says it) and when that was seen.
    public let lastOutcome: String?
    public let lastRunAt: UInt64?
    init?(wire value: JSONValue) {
        guard let id = DashboardWire.text(value["id"]) else { return nil }
        self.id = id
        title = DashboardWire.text(value["title"]) ?? "Automation"
        projectID = DashboardWire.uuid(value["project_id"])
        projectName = DashboardWire.text(value["project_name"])
        paused = value["paused"] == .bool(true)
        nextRun = paused ? nil : LenientNumber.seconds(value["next_run"])
        lastOutcome = value["last_run"]["outcome"].string
        lastRunAt = LenientNumber.seconds(value["last_run"]["at"])
    }
    /// Its last run did not go through.
    public var lastRunFailed: Bool { ["failed", "missed", "uncertain"].contains(lastOutcome ?? "") }
}

public enum DashboardAutomations: Sendable, Equatable {
    /// The desktop has not read them yet.
    case notRead
    /// The desktop could not read them; it says why.
    case unreadable(String)
    case read(total: Int?, paused: Int?, next: (title: String, at: UInt64)?, items: DashboardList<DashboardAutomation>)
    init(wire value: JSONValue) {
        switch value["state"].string {
        case "unreadable": self = .unreadable(DashboardWire.text(value["message"]) ?? "The automations could not be read.")
        case "read":
            let next: (title: String, at: UInt64)? = LenientNumber.seconds(value["next"]["at"]).map { (DashboardWire.text(value["next"]["title"]) ?? "Automation", $0) }
            self = .read(total: LenientNumber.count(value["total"]), paused: LenientNumber.count(value["paused"]), next: next,
                         items: DashboardList(value["items"], total: value["items_total"], arrange: DashboardArrangement.automations, read: DashboardAutomation.init(wire:)))
        default: self = .notRead
        }
    }
    public static func == (a: Self, b: Self) -> Bool {
        switch (a, b) {
        case (.notRead, .notRead): true
        case (.unreadable(let x), .unreadable(let y)): x == y
        case (.read(let t1, let p1, let n1, let i1), .read(let t2, let p2, let n2, let i2)): t1 == t2 && p1 == p2 && n1?.title == n2?.title && n1?.at == n2?.at && i1 == i2
        default: false
        }
    }
}

/// A list as the desktop cut it: the rows it sent, and how many there are in all.
public struct DashboardList<Row: Sendable & Equatable>: Sendable, Equatable {
    public let rows: [Row]
    /// Every row the desktop had; nil when it did not say.
    public let total: Int?
    /// Rows the desktop has that were not sent (`total` beyond what came); 0 when it did not say.
    public var more: Int { max(0, (total ?? rows.count) - rows.count) }
    public init(rows: [Row], total: Int?) { self.rows = rows; self.total = total }
    init(_ value: JSONValue, total: JSONValue, arrange: ([Row]) -> [Row], read: (JSONValue) -> Row?) {
        let arranged = arrange(value.array.compactMap(read))
        rows = arranged
        // A total under what arrived would hide rows that are on screen: it is never less than them.
        self.total = LenientNumber.count(total).map { max($0, arranged.count) }
    }
}

public struct DashboardTotals: Sendable, Equatable {
    public var projects: Int?, activeProjects: Int?, liveShells: Int?
    /// Nil while the desktop has not read its chats.
    public var liveChats: Int?
    public var agents: DashboardAgentCounts?
    public var tasks: DashboardTaskCounts?
    public var attention: Int?
    init(wire value: JSONValue, chatsKnown: Bool?) {
        projects = LenientNumber.count(value["projects"]); activeProjects = LenientNumber.count(value["active_projects"])
        liveShells = LenientNumber.count(value["live_shells"])
        liveChats = chatsKnown == true ? LenientNumber.count(value["live_chats"]) : nil
        agents = DashboardAgentCounts(wire: value["agents"]); tasks = DashboardTaskCounts(wire: value["tasks"])
        attention = LenientNumber.count(value["attention"])
    }
    /// Live terminals and chats, when both are known.
    public var liveSessions: Int? { liveShells.flatMap { shells in liveChats.map { shells + $0 } } }
}

/// One answer to `dashboard.get`.
public struct DashboardSnapshot: Sendable, Equatable {
    public let intelligence: DashboardIntelligence
    public let decisions: DashboardList<DashboardDecision>
    public let decisionSources: DashboardDecisionSources
    public let recentActivity: DashboardList<DashboardRecentActivity>
    /// When the desktop sampled it (Unix seconds); nil when it did not say.
    public let generatedAt: UInt64?
    /// Whether the desktop had read its chats: until it has, chat figures are unknown (nil), not zero. Nil when it did not say.
    public let chatsKnown: Bool?
    public let totals: DashboardTotals
    public let global: DashboardGlobal
    /// Waiting agents first, then failures, the newest first within each.
    public let attention: DashboardList<DashboardAttention>
    /// Agents working now, the longest-running first.
    public let working: DashboardList<DashboardWork>
    /// Agents that finished within the last hour, the most recent first.
    public let finished: DashboardList<DashboardWork>
    /// Every project, the busiest first.
    public let projects: DashboardList<DashboardProject>
    /// Tasks in progress, the most recently updated first.
    public let tasksInProgress: DashboardList<DashboardTask>
    public let automations: DashboardAutomations

    public enum ReadError: Error, Equatable, LocalizedError {
        case unreadable, unsupportedVersion
        public var errorDescription: String? {
            switch self {
            case .unreadable: "The Mac's dashboard could not be read."
            case .unsupportedVersion: "The Mac sends a newer dashboard than this iPhone reads. Update RiWork on the iPhone."
            }
        }
    }

    public init(wire value: JSONValue) throws {
        guard case .object = value else { throw ReadError.unreadable }
        guard value["v"] == .number(1) else { throw value["v"] == .null ? ReadError.unreadable : ReadError.unsupportedVersion }
        generatedAt = LenientNumber.seconds(value["generated_at"])
        intelligence = DashboardIntelligence(wire: value["intelligence"])
        decisions = DashboardList(value["decisions"], total: value["decisions_total"], arrange: { $0 }, read: DashboardDecision.init(wire:))
        decisionSources = DashboardDecisionSources(wire: value["decision_sources"])
        recentActivity = DashboardList(value["recent_activity"], total: value["recent_activity_total"], arrange: { $0 }, read: DashboardRecentActivity.init(wire:))
        chatsKnown = { if case .bool(let known) = value["chats_known"] { return known }; return nil }()
        totals = DashboardTotals(wire: value["totals"], chatsKnown: chatsKnown)
        global = DashboardGlobal(wire: value["global"])
        attention = DashboardList(value["attention"], total: value["attention_total"], arrange: DashboardArrangement.attention, read: DashboardAttention.init(wire:))
        working = DashboardList(value["working"], total: value["working_total"], arrange: DashboardArrangement.working, read: DashboardWork.init(wire:))
        finished = DashboardList(value["finished"], total: value["finished_total"], arrange: DashboardArrangement.finished, read: DashboardWork.init(wire:))
        let known = chatsKnown
        projects = DashboardList(value["projects"], total: value["projects_total"], arrange: DashboardArrangement.projects, read: { DashboardProject(wire: $0, chatsKnown: known) })
        tasksInProgress = DashboardList(value["tasks_in_progress"], total: value["tasks_in_progress_total"], arrange: DashboardArrangement.tasks, read: DashboardTask.init(wire:))
        automations = DashboardAutomations(wire: value["automations"])
    }
}

// MARK: - Arrangement

/// The desktop's order (`global_overview::build`), applied again to what arrived. The desktop's lists are already in it, and every sort
/// here is stable, so a list in the desktop's order stays exactly as it came (ties keep the desktop's order); one from a desktop that
/// sent it otherwise is put the same way.
public enum DashboardArrangement {
    /// Waiting first, then failures, newest first within each; a row whose time is not known counts as the oldest.
    public static func attention(_ rows: [DashboardAttention]) -> [DashboardAttention] {
        stable(rows) { a, b in
            if a.isWaiting != b.isWaiting { return a.isWaiting }
            return (a.at ?? 0) > (b.at ?? 0)
        }
    }
    /// The longest-running first; those whose start is not known last.
    public static func working(_ rows: [DashboardWork]) -> [DashboardWork] {
        stable(rows) { a, b in
            switch (a.sinceUnix, b.sinceUnix) {
            case (let x?, let y?): x < y
            case (.some, nil): true
            default: false
            }
        }
    }
    /// The most recent first; those whose time is not known last.
    public static func finished(_ rows: [DashboardWork]) -> [DashboardWork] {
        stable(rows) { a, b in
            switch (a.sinceUnix, b.sinceUnix) {
            case (let x?, let y?): x > y
            case (.some, nil): true
            default: false
            }
        }
    }
    /// The busiest first: rows needing a person, agents working, live sessions, tasks in progress, open tasks; then by name (ignoring
    /// case) and id. A figure that is not known counts as none, as the desktop counts a chat it has not read.
    public static func projects(_ rows: [DashboardProject]) -> [DashboardProject] {
        func key(_ p: DashboardProject) -> [Int] {
            [p.attention ?? 0, p.agents?.working ?? 0, (p.liveShells ?? 0) + (p.liveChats ?? 0), p.tasks?.inProgress ?? 0, (p.tasks?.todo ?? 0) + (p.tasks?.inProgress ?? 0)]
        }
        return stable(rows) { a, b in
            let ka = key(a), kb = key(b)
            if ka != kb { return kb.lexicographicallyPrecedes(ka) }
            let na = scalars(a.name.lowercased()), nb = scalars(b.name.lowercased())
            if na != nb { return na.lexicographicallyPrecedes(nb) }
            return scalars(a.id).lexicographicallyPrecedes(scalars(b.id))
        }
    }
    /// The most recently updated first, then by id.
    public static func tasks(_ rows: [DashboardTask]) -> [DashboardTask] {
        stable(rows) { a, b in
            if (a.updatedAt ?? 0) != (b.updatedAt ?? 0) { return (a.updatedAt ?? 0) > (b.updatedAt ?? 0) }
            return scalars(a.id).lexicographicallyPrecedes(scalars(b.id))
        }
    }
    /// The next to run first; those with no next run after them, the paused ones last; then by title (ignoring case) and id.
    public static func automations(_ rows: [DashboardAutomation]) -> [DashboardAutomation] {
        stable(rows) { a, b in
            if a.paused != b.paused { return !a.paused }
            if (a.nextRun == nil) != (b.nextRun == nil) { return a.nextRun != nil }
            if let x = a.nextRun, let y = b.nextRun, x != y { return x < y }
            let ta = scalars(a.title.lowercased()), tb = scalars(b.title.lowercased())
            if ta != tb { return ta.lexicographicallyPrecedes(tb) }
            return scalars(a.id).lexicographicallyPrecedes(scalars(b.id))
        }
    }
    /// Rust compares strings by their bytes, which for UTF-8 is the order of their scalars.
    private static func scalars(_ text: String) -> [UInt32] { text.unicodeScalars.map(\.value) }
    private static func stable<T>(_ rows: [T], by before: (T, T) -> Bool) -> [T] {
        rows.enumerated().sorted { before($0.element, $1.element) || (!before($1.element, $0.element) && $0.offset < $1.offset) }.map(\.element)
    }
}

// MARK: - Freshness

/// What the dashboard says about how current it is, as the desktop's does: live, the age of the last read, or that it is being read.
public struct DashboardFreshness: Sendable, Equatable {
    /// A read older than this is shown as stale (the desktop's `STALE_AFTER_SECS`).
    public static let staleAfter: TimeInterval = 30
    public let text: String
    /// The figures on screen may be out of date.
    public let stale: Bool
    /// - `connected`: the link is up. `readAt`: when the last read that worked arrived. `failure`: why the last read did not.
    public static func decide(connected: Bool, connectionLabel: String, readAt: Date?, failure: String?, now: Date) -> DashboardFreshness {
        guard let readAt else {
            if !connected { return DashboardFreshness(text: connectionLabel, stale: true) }
            if let failure { return DashboardFreshness(text: "The dashboard could not be read: \(failure)", stale: true) }
            return DashboardFreshness(text: "Reading the dashboard…", stale: true)
        }
        let age = max(0, now.timeIntervalSince(readAt))
        let ago = "\(DashboardText.duration(age)) ago"
        if !connected { return DashboardFreshness(text: "\(connectionLabel) · showing the read from \(ago)", stale: true) }
        if let failure { return DashboardFreshness(text: "Showing the read from \(ago) · \(failure)", stale: true) }
        if age > staleAfter { return DashboardFreshness(text: "Last read \(ago)", stale: true) }
        return DashboardFreshness(text: "Live", stale: false)
    }
}

// MARK: - Where a row leads

/// What a dashboard row opens on the phone.
public enum DashboardDestination: Sendable, Equatable {
    /// The Global screen (the global orchestrator, outside every project).
    case global
    /// A project's tabs, with the tab to put on screen (`chat:<id>` or `shell:<id>`) when the row is one.
    case project(id: String, tab: String?)
    /// Nothing to open on the phone (an app automation).
    case none
}

public enum DashboardNavigation {
    /// - `globalIDs`: every id that names the global orchestrator (`OrchestratorScope.globalIDs`), so a row of it opens the Global screen
    ///   even when the desktop did not mark it.
    public static func destination(_ target: DashboardTarget, session: DashboardSession? = nil, projectID: String? = nil, globalIDs: Set<String> = []) -> DashboardDestination {
        switch target {
        case .shell(let id):
            if globalIDs.contains(id) || session?.orchestrator == .global { return .global }
            guard let project = session?.projectID ?? projectID else { return .none }
            return .project(id: project, tab: "shell:\(id)")
        case .chat(let id, let project, let orchestrator):
            if orchestrator == .global || globalIDs.contains(id) { return .global }
            guard let project = project ?? session?.projectID ?? projectID else { return .none }
            return .project(id: project, tab: "chat:\(id)")
        case .project(let id): return .project(id: id, tab: nil)
        case .task: return (projectID ?? session?.projectID).map { .project(id: $0, tab: nil) } ?? .none
        case .automations(let project): return (project ?? projectID).map { .project(id: $0, tab: nil) } ?? .none
        }
    }
}

// MARK: - Words

public enum DashboardText {
    /// A duration as the desktop writes it: "now", "45s", "12m", "3h", "2d".
    public static func duration(_ seconds: TimeInterval) -> String {
        let s = Int(max(0, seconds))
        switch s {
        case 0..<5: return "now"
        case 5..<60: return "\(s)s"
        case 60..<3600: return "\(s / 60)m"
        case 3600..<86400: return "\(s / 3600)h"
        default: return "\(s / 86400)d"
        }
    }
    /// A count, or "—" when it is not known.
    public static func count(_ value: Int?) -> String { value.map(String.init) ?? "—" }
    /// What a chat is doing, in a word.
    public static func chatState(_ state: ChatState) -> String {
        switch state {
        case .starting: "Starting"
        case .idle: "Ready"
        case .running: "Working"
        case .waiting: "Waiting for input"
        case .stopped: "Stopped"
        case .failed: "Failed"
        case .unknown(let word): word.capitalized
        }
    }
    /// What a terminal's agent is doing, in a word; nil when nothing is known.
    public static func activity(_ activity: AgentActivity?) -> String? {
        switch activity {
        case .working: "Working"
        case .waiting: "Waiting for input"
        case .done: "Done"
        case .exited: "Exited"
        case .unknown, nil: nil
        }
    }
    /// "2 to do · 1 in progress · 3 done", or "No tasks"; "—" when not known.
    public static func tasks(_ counts: DashboardTaskCounts?) -> String {
        guard let counts, let todo = counts.todo, let progress = counts.inProgress, let done = counts.done else { return "Tasks —" }
        if todo + progress + done == 0 { return "No tasks" }
        return [progress > 0 ? "\(progress) in progress" : nil, todo > 0 ? "\(todo) to do" : nil, done > 0 ? "\(done) done" : nil].compactMap { $0 }.joined(separator: " · ")
    }
}

// MARK: - Reading the wire

enum DashboardWire {
    /// A canonical UUID, lowercased; anything else is no id.
    static func uuid(_ value: JSONValue) -> String? {
        guard let text = value.string, text.utf8.count == 36, let uuid = UUID(uuidString: text) else { return nil }
        return uuid.uuidString.lowercased()
    }
    /// Text that says something: trimmed, not empty, with control characters taken out, and no longer than a row can use.
    static func text(_ value: JSONValue, limit: Int = 300) -> String? {
        guard let raw = value.string else { return nil }
        let cleaned = String(String.UnicodeScalarView(raw.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) })).trimmingCharacters(in: .whitespacesAndNewlines)
        guard !cleaned.isEmpty else { return nil }
        return cleaned.count > limit ? String(cleaned.prefix(limit)) : cleaned
    }
    static func provider(_ value: JSONValue) -> ChatProvider? {
        value.string.flatMap { ChatProvider(rawValue: $0.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()) }
    }
    static func chatState(_ value: JSONValue, message: JSONValue) -> ChatState {
        switch value.string {
        case "starting": .starting
        case "idle": .idle
        case "running": .running
        case "waiting": .waiting
        case "stopped": .stopped
        case "failed": .failed(text(message) ?? "")
        case let word?: .unknown(word)
        case nil: .unknown("unknown")
        }
    }
}
