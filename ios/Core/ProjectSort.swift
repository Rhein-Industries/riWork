import Foundation

/// How the project list is ordered. The desktop has more (and a direction); the phone keeps the three a thumb needs, each in the
/// direction the desktop starts with (`src/project_sort.rs`: Date added newest first, Name A to Z). Its Recent is not the desktop's
/// "Last edited": it follows when a project's terminals were last active, and falls back to the file edits the desktop sorts by.
public enum ProjectSort: String, CaseIterable, Sendable, Identifiable {
    /// Latest terminal activity, then latest file edit.
    case recent
    case name
    case dateAdded

    public var id: String { rawValue }
    /// The first time, and whenever the remembered value is not one of ours.
    public static let standard = ProjectSort.recent

    public var title: String {
        switch self {
        case .recent: "Recent"
        case .name: "Name"
        case .dateAdded: "Date added"
        }
    }
    /// The direction, as the desktop words it.
    public var detail: String {
        switch self {
        case .recent, .dateAdded: "Newest first"
        case .name: "A to Z"
        }
    }
    /// The one after this, wrapping: what the keyboard shortcut steps to.
    public var next: ProjectSort {
        let all = Self.allCases
        return all[((all.firstIndex(of: self) ?? 0) + 1) % all.count]
    }

    // MARK: Remembered choice

    public static let defaultsKey = "riwork.projectSort"
    /// What the person chose last (UserDefaults); `standard` when nothing, or something unknown, is stored.
    public static func stored(in defaults: UserDefaults) -> ProjectSort {
        defaults.string(forKey: defaultsKey).flatMap(ProjectSort.init(rawValue:)) ?? .standard
    }
    public func remember(in defaults: UserDefaults) { defaults.set(rawValue, forKey: Self.defaultsKey) }
}

public enum ProjectSorting {
    /// `projects` in the order `sort` asks for. Rules, all of them ending in the same tie-break so the order never flickers:
    /// - `recent`, in four steps, each step after every project of the one before it: projects with a `last_activity_unix` (one of
    ///   their terminals printed lately), newest first; then those with only a `last_edited_unix`, newest first; then those with
    ///   only a `created_at`, newest first; then the rest. A project with no live terminal has no activity, so it follows every
    ///   project that has some, however recently its files changed. A figure of zero is no figure, and an older desktop, which has
    ///   no `last_activity_unix`, gets the last three steps: the order Recent had before.
    /// - `dateAdded`: `created_at`, newest first; a project without one last.
    /// - `name`: A to Z, ignoring case.
    /// - Ties: name without regard to case, then as written, then id (the desktop's rule, so both sides list the same order).
    ///
    /// `touched` is for a project this phone just made: its id with the desktop's `created_at`. That counts as activity, so a new
    /// project is at the top of `recent` and not behind every project that has some; a later `last_activity_unix` of its own wins.
    public static func sorted(_ projects: [RemoteProject], by sort: ProjectSort, touched: [String: UInt64] = [:]) -> [RemoteProject] {
        order(projects, by: sort, touched: touched)
    }
    /// What the list shows: the projects whose name contains `search` (any case; all of them when it is empty), in the order of `sort`.
    /// The search takes projects out and never reorders the rest.
    public static func visible(_ projects: [RemoteProject], matching search: String, by sort: ProjectSort, touched: [String: UInt64] = [:]) -> [RemoteProject] {
        order(search.isEmpty ? projects : projects.filter { $0.name.localizedCaseInsensitiveContains(search) }, by: sort, touched: touched)
    }
    private static func order(_ projects: [RemoteProject], by sort: ProjectSort, touched: [String: UInt64]) -> [RemoteProject] {
        func known(_ time: UInt64?) -> UInt64? { time.flatMap { $0 > 0 ? $0 : nil } }
        // Rank 0 sorts before rank 1; within a rank the larger time comes first.
        func key(_ project: RemoteProject) -> (rank: Int, time: UInt64) {
            switch sort {
            case .name:
                return (0, 0)
            case .dateAdded:
                if let added = known(project.created_at) { return (0, added) }
                return (1, 0)
            case .recent:
                if let active = known(max(project.last_activity_unix ?? 0, touched[project.id] ?? 0)) { return (0, active) }
                if let edited = known(project.last_edited_unix) { return (1, edited) }
                if let added = known(project.created_at) { return (2, added) }
                return (3, 0)
            }
        }
        func byName(_ a: RemoteProject, _ b: RemoteProject) -> Bool? {
            let (lower, other) = (a.name.lowercased(), b.name.lowercased())
            if lower != other { return lower < other }
            if a.name != b.name { return a.name < b.name }
            return nil
        }
        return projects.sorted { a, b in
            if sort != .name {
                let (left, right) = (key(a), key(b))
                if left.rank != right.rank { return left.rank < right.rank }
                if left.time != right.time { return left.time > right.time }
            }
            if let named = byName(a, b) { return named }
            return a.id < b.id
        }
    }
}
