import Foundation
import Observation
import RiWorkCore

/// The lists that show what agents are doing, and so are read again while they are on screen.
enum ActivityScope: Hashable, Sendable {
    /// The project list: `agents` per project.
    case projects
    /// The terminal tabs of the chosen project: `activity` per shell and orchestrator.
    case sessions
}

// Agent activity: whether a Claude or a Codex in a terminal is working, waiting for a person or done, with its subagents, and the
// project order (Recent, Name, Date added). The desktop works the states out and sends them with the lists it already answers
// (`projects.list`, `shells.list`, `orchestrators.list`), so the phone only has to read those lists again while somebody looks at
// them. Nothing is asked for, or kept, while no such view is on screen.
extension RemoteModel {
    // MARK: Project order

    /// The project list in the order the person chose.
    var sortedProjects: [RemoteProject] { ProjectSorting.sorted(projects, by: projectSort, touched: touchedProjects) }

    /// What the list shows for the text in the search field: the matches, in the same order.
    func visibleProjects(matching search: String) -> [RemoteProject] {
        ProjectSorting.visible(projects, matching: search, by: projectSort, touched: touchedProjects)
    }

    func setProjectSort(_ sort: ProjectSort) {
        guard sort != projectSort else { return }
        projectSort = sort
        sort.remember(in: defaults)
    }
    /// The keyboard shortcut: the next order, wrapping.
    func cycleProjectSort() { setProjectSort(projectSort.next) }

    // MARK: Reading the lists again

    func noteListsRead() { let now = ContinuousClock.now; lastListRead[.projects] = now; lastListRead[.sessions] = now }

    /// Keeps `scope`'s list fresh for as long as the caller lives: a view runs this in `.task`, so it stops when the view goes away,
    /// and nothing is read for a screen nobody is looking at. A list read a moment ago (the connect, an explicit refresh) is not read
    /// again at once. Nothing is read while the link is down or the app is not in the foreground, and a failed read is skipped
    /// silently: the next one tries again, and the connection has its own way of saying it is down.
    func keepFresh(_ scope: ActivityScope) async {
        while !Task.isCancelled {
            var pause = activityRefreshInterval
            if state == .connected, appActive, !loading {
                let age = lastListRead[scope].map { ContinuousClock.now - $0 }
                if let age, age < activityRefreshInterval { pause = activityRefreshInterval - age }
                else {
                    switch scope {
                    case .projects: await refreshProjectsQuietly()
                    case .sessions: await refreshSessionsQuietly()
                    }
                }
            }
            try? await Task.sleep(for: pause)
        }
    }

    /// The tabs of the chosen project, read again without the side effects of a full refresh: shells and orchestrators together, so
    /// each tab's state is current. The selection is left alone; a terminal that went away is found by the live read, as before.
    func refreshSessionsQuietly() async {
        guard state == .connected, let project = projectID, loadedProjectID == project else { return }
        let token = generation, stamp = inventoryStamp
        async let workers = try? rpc("shells.list", ["project_id": .string(project)])["shells"].decode([RemoteSession].self)
        async let managers = try? rpc("orchestrators.list")["orchestrators"].decode([RemoteSession].self)
        // The chats are tabs in the same strip, and have states of their own.
        async let tabList = sharedTabsOfProject(project)
        async let talks = chatsOfProject(project)
        let (listedShells, listedManagers, listedChats, listedTabs) = await (workers, managers, talks, tabList)
        guard generation == token, projectID == project, loadedProjectID == project, state == .connected else { return }
        // Something newer was installed while these were read (a chat or terminal made here, a tab change's reply): this read is older
        // than the screen, its lists with it; the next refresh brings them. Its tab list is only believed if not older either.
        guard inventoryStamp == stamp else { return }
        if let listedTabs, !acceptSharedTabs(listedTabs) { return }
        if let listedChats { installChats(listedChats, project: project) }
        if let listedShells, listedShells != shells { shells = listedShells }
        if let listedManagers, listedManagers != orchestrators { orchestrators = listedManagers }
        // The project's first shared list arrives late (its first read failed): the remembered tab comes back now.
        if listedTabs != nil { restorePendingTab(project: project) }
        // An orchestrator that runs as a chat comes and goes with this list, and so does the chat it opened.
        reconcileChatSelection()
        if listedTabs != nil { try? reconcileSelectedSession() }
        if listedShells != nil || listedManagers != nil { lastListRead[.sessions] = .now }
    }
}
