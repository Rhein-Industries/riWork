import Foundation

/// A host's most recent overview read. The pairing and its credentials never become part of dashboard data.
public struct HostDashboardEntry: Sendable, Equatable, Identifiable {
    public enum State: Sendable, Equatable { case idle, refreshing, ready, offline, unsupported, paused }

    /// The saved pairing's route id, rather than a project/chat id that can also occur on another host.
    public let id: String
    public var name: String
    public var snapshot: DashboardSnapshot?
    public var readAt: Date?
    public var state: State
    public var failure: String?

    public init(id: String, name: String, snapshot: DashboardSnapshot? = nil, readAt: Date? = nil, state: State = .idle, failure: String? = nil) {
        self.id = id; self.name = name; self.snapshot = snapshot; self.readAt = readAt; self.state = state; self.failure = failure
    }

    public func freshness(now: Date = .now) -> DashboardFreshness {
        let label: String
        switch state {
        case .idle: label = "Not read yet"
        case .refreshing: label = "Reading…"
        case .ready: label = "Available"
        // Why, when the read said: a pending invite needs its first explicit connection, not a Mac that is "unavailable".
        case .offline: label = failure ?? "Host unavailable"
        case .unsupported: label = failure ?? "Update RiWork on this host"
        case .paused: label = "Paused"
        }
        // Only a read that worked is current: a host being read again after a failure is not live until it answers.
        return DashboardFreshness.decide(connected: state == .ready, connectionLabel: label, readAt: readAt, failure: failure, now: now)
    }
}

/// A sum of the known host figures, with its coverage. A host that did not answer contributes unknown, never an invented zero.
public struct HostDashboardCount: Sendable, Equatable {
    public let knownValue: Int
    public let knownHosts: Int
    public let totalHosts: Int
    public var unknownHosts: Int { max(0, totalHosts - knownHosts) }
    public var isComplete: Bool { totalHosts > 0 && knownHosts == totalHosts }
    public var value: Int? { knownHosts > 0 ? knownValue : nil }

    init(hosts: [HostDashboardEntry], value: (DashboardSnapshot) -> Int?) {
        let values = hosts.compactMap { $0.snapshot.flatMap(value) }
        knownValue = values.reduce(0) { sum, next in
            let result = sum.addingReportingOverflow(next)
            return result.overflow ? Int.max : result.partialValue
        }
        knownHosts = values.count; totalHosts = hosts.count
    }
}

public struct HostDashboardRowID: Hashable, Sendable {
    public let hostID: String
    public let localID: String
    public init(hostID: String, localID: String) { self.hostID = hostID; self.localID = localID }
}

/// Every row keeps its host route all the way to navigation. Identical ids on different Macs remain separate rows.
public struct HostDashboardRow<Value: Sendable & Equatable>: Sendable, Equatable, Identifiable {
    public let id: HostDashboardRowID
    public let hostName: String
    public let value: Value
    public let stale: Bool
    public let readAt: Date?
    public var hostID: String { id.hostID }
    public init(host: HostDashboardEntry, localID: String, value: Value, now: Date = .now) {
        id = HostDashboardRowID(hostID: host.id, localID: localID); hostName = host.name; self.value = value
        stale = host.freshness(now: now).stale; readAt = host.readAt
    }
}

/// A phone overview across its saved hosts. Last successful reads remain useful while another host is offline, with explicit age and coverage.
public struct HostDashboardAggregate: Sendable, Equatable {
    public let hosts: [HostDashboardEntry]
    public let sampledHostCount: Int
    public let freshHostCount: Int
    public let staleHostCount: Int
    public var totalHostCount: Int { hosts.count }
    public var unavailableHostCount: Int { totalHostCount - sampledHostCount }
    public let projects: HostDashboardCount
    public let activeProjects: HostDashboardCount
    public let liveShells: HostDashboardCount
    public let liveChats: HostDashboardCount
    public let working: HostDashboardCount
    public let waiting: HostDashboardCount
    public let attention: HostDashboardCount
    public let openTasks: HostDashboardCount
    public let subagents: HostDashboardCount
    public let projectRows: [HostDashboardRow<DashboardProject>]
    public let attentionRows: [HostDashboardRow<DashboardAttention>]
    public let workingRows: [HostDashboardRow<DashboardWork>]
    public let finishedRows: [HostDashboardRow<DashboardWork>]
    public let taskRows: [HostDashboardRow<DashboardTask>]
    /// Every host's decision cards, newest first, so one host's many cannot push another's out of a short list.
    public let decisionRows: [HostDashboardRow<DashboardDecision>]

    public init(hosts: [HostDashboardEntry], now: Date = .now) {
        self.hosts = hosts
        sampledHostCount = hosts.filter { $0.snapshot != nil }.count
        freshHostCount = hosts.filter { $0.snapshot != nil && !$0.freshness(now: now).stale }.count
        staleHostCount = sampledHostCount - freshHostCount
        projects = HostDashboardCount(hosts: hosts) { $0.totals.projects }
        activeProjects = HostDashboardCount(hosts: hosts) { $0.totals.activeProjects }
        liveShells = HostDashboardCount(hosts: hosts) { $0.totals.liveShells }
        liveChats = HostDashboardCount(hosts: hosts) { $0.totals.liveChats }
        working = HostDashboardCount(hosts: hosts) { $0.totals.agents?.working }
        waiting = HostDashboardCount(hosts: hosts) { $0.totals.agents?.waiting }
        attention = HostDashboardCount(hosts: hosts) { $0.totals.attention }
        openTasks = HostDashboardCount(hosts: hosts) { $0.totals.tasks?.open }
        subagents = HostDashboardCount(hosts: hosts) { $0.totals.agents?.subagents }
        projectRows = hosts.flatMap { host in
            (host.snapshot?.projects.rows ?? []).map { HostDashboardRow(host: host, localID: $0.id, value: $0, now: now) }
        }
        attentionRows = Self.stable(hosts.flatMap { host in
            (host.snapshot?.attention.rows ?? []).enumerated().map { index, row in
                HostDashboardRow(host: host, localID: "\(Self.targetID(row.target)):\(index)", value: row, now: now)
            }
        }) { a, b in
            if a.value.isWaiting != b.value.isWaiting { return a.value.isWaiting }
            return (a.value.at ?? 0) > (b.value.at ?? 0)
        }
        workingRows = Self.stable(hosts.flatMap { host in
            (host.snapshot?.working.rows ?? []).map { HostDashboardRow(host: host, localID: Self.targetID($0.session.target), value: $0, now: now) }
        }) { ($0.value.sinceUnix ?? UInt64.max) < ($1.value.sinceUnix ?? UInt64.max) }
        finishedRows = Self.stable(hosts.flatMap { host in
            (host.snapshot?.finished.rows ?? []).map { HostDashboardRow(host: host, localID: Self.targetID($0.session.target), value: $0, now: now) }
        }) { ($0.value.sinceUnix ?? 0) > ($1.value.sinceUnix ?? 0) }
        taskRows = hosts.flatMap { host in
            (host.snapshot?.tasksInProgress.rows ?? []).map { HostDashboardRow(host: host, localID: $0.id, value: $0, now: now) }
        }
        decisionRows = Self.stable(hosts.flatMap { host in
            (host.snapshot?.decisions.rows ?? []).map { HostDashboardRow(host: host, localID: $0.id, value: $0, now: now) }
        }) { ($0.value.observedAt ?? 0) > ($1.value.observedAt ?? 0) }
    }

    private static func targetID(_ target: DashboardTarget) -> String {
        switch target {
        case .shell(let id): "shell:\(id)"
        case .chat(let id, _, _): "chat:\(id)"
        case .project(let id): "project:\(id)"
        case .task(let id): "task:\(id)"
        case .automations(let id): "automations:\(id ?? "global")"
        }
    }
    private static func stable<T>(_ values: [T], before: (T, T) -> Bool) -> [T] {
        values.enumerated().sorted { a, b in
            before(a.element, b.element) || (!before(b.element, a.element) && a.offset < b.offset)
        }.map(\.element)
    }
}
