import Foundation

/// How the project list is ordered. The desktop has more (and a direction); the phone keeps the three a thumb needs, each in the
/// direction the desktop starts with (`src/project_sort.rs`: Last edited and Date added newest first, Name A to Z).
public enum ProjectSort: String, CaseIterable, Sendable, Identifiable {
    /// The desktop's "Last edited".
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
    /// - `recent`: projects with a `last_edited_unix`, newest first; then those without (an older desktop, or not worked out yet)
    ///   by `created_at`, newest first. A figure of zero is no figure.
    /// - `dateAdded`: `created_at`, newest first; a project without one last.
    /// - `name`: A to Z, ignoring case.
    /// - Ties: name without regard to case, then as written, then id (the desktop's rule, so both sides list the same order).
    ///
    /// `touched` is for a project this phone just made: its id with the desktop's `created_at`. Until the desktop has a figure of its
    /// own for it, that counts as an edit, so a new project is at the top of `recent` and not behind every project that has a date.
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
                if let edited = known(max(project.last_edited_unix ?? 0, touched[project.id] ?? 0)) { return (0, edited) }
                if let added = known(project.created_at) { return (1, added) }
                return (2, 0)
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
