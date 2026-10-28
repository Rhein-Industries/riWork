import type { FolderDto, ProjectDto, ShellSessionDto, StoreState, TaskDto, WorktreeDto } from "riwork/native";

export type ListPanel = "projects" | "worktrees" | "tasks" | "shells";
export type ProjectOrder = "name" | "added" | "live";
export type ProjectTreeRow =
  | { kind: "folder"; id: string; name: string; depth: number; count: number; folder?: FolderDto }
  | { kind: "project"; id: string; depth: number; project: ProjectDto }
  | { kind: "empty"; id: string; depth: number };

export const EMPTY_STORE: StoreState = {
  schemaVersion: 1, activeProjectId: null, projects: [], projectFolders: [], worktrees: [], tasks: [],
  orcaImport: null, projectMemos: {},
};

export function matches(query: string, ...fields: (string | null | undefined)[]): boolean {
  const needle = query.trim().toLowerCase();
  return !needle || fields.some((field) => field?.toLowerCase().includes(needle));
}

export function countLabel(count: number, total: number, noun: string): string {
  return `${count === total ? count : `${count} of ${total}`} ${total === 1 ? noun : `${noun}s`}`;
}

function byName(a: { id: string; name: string }, b: { id: string; name: string }): number {
  const first = a.name.toLowerCase(), second = b.name.toLowerCase();
  return (first < second ? -1 : first > second ? 1 : 0) ||
    (a.name < b.name ? -1 : a.name > b.name ? 1 : 0) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
}

/** This is a presentation projection only; all tree mutations are validated by Store. */
export function projectRows(
  state: StoreState, query: string, collapsed: ReadonlySet<string>,
  order: ProjectOrder = "added", descending = true, sessions: ShellSessionDto[] = [],
): ProjectTreeRow[] {
  const live = (id: string) => sessions.filter((shell) => shell.projectId === id && shell.alive).length;
  const projects = [...state.projects].sort((a, b) => {
    if (order === "name") return byName(a, b) * (descending ? -1 : 1);
    const av = order === "live" ? live(a.id) : a.createdAt;
    const bv = order === "live" ? live(b.id) : b.createdAt;
    if (order === "added" && (!av || !bv)) return av ? -1 : bv ? 1 : byName(a, b);
    return (av - bv) * (descending ? -1 : 1) || byName(a, b);
  });
  if (query.trim()) {
    // Name hits precede id/path hits, without changing the chosen order within either group.
    const name = projects.filter((project) => matches(query, project.name));
    return [...name, ...projects.filter((project) => !matches(query, project.name) && matches(query, project.id, project.root))]
      .map((project) => ({ kind: "project", id: `project-${project.id}`, depth: 0, project }));
  }
  const folders = [...state.projectFolders].sort(byName);
  const ids = new Set(folders.map((folder) => folder.id));
  const parent = new Map(folders.map((folder) => [folder.id, folder.parentId && ids.has(folder.parentId) ? folder.parentId : null]));
  // Break every cycle at its first name-ranked folder, including malformed imported state.
  const completed = new Set<string>();
  for (const folder of folders) {
    const chain: string[] = [];
    let id: string | null | undefined = folder.id;
    while (id && !completed.has(id)) {
      const cycle = chain.indexOf(id);
      if (cycle >= 0) {
        const root = folders.find((item) => chain.slice(cycle).includes(item.id))!;
        parent.set(root.id, null);
        break;
      }
      chain.push(id); id = parent.get(id);
    }
    chain.forEach((id) => completed.add(id));
  }
  const children = (id: string | null) => folders.filter((folder) => parent.get(folder.id) === id);
  const direct = (id: string | null) => projects.filter((project) => (project.folderId && ids.has(project.folderId) ? project.folderId : null) === id);
  function count(id: string): number { return direct(id).length + children(id).reduce((sum, child) => sum + count(child.id), 0); }
  const rows: ProjectTreeRow[] = [];
  function add(id: string, name: string, depth: number, folder?: FolderDto) {
    const unfiled = id === "unfiled";
    const owned = direct(unfiled ? null : id), nested = unfiled ? [] : children(id);
    rows.push({ kind: "folder", id: `folder-${id}`, name, depth, count: unfiled ? owned.length : count(id), folder });
    if (collapsed.has(id)) return;
    owned.forEach((project) => rows.push({ kind: "project", id: `project-${project.id}`, depth: depth + 1, project }));
    if (!owned.length && !nested.length) rows.push({ kind: "empty", id: `empty-${id}`, depth: depth + 1 });
    nested.forEach((folder) => add(folder.id, folder.name, depth + 1, folder));
  }
  add("unfiled", "Unfiled", 0);
  children(null).forEach((folder) => add(folder.id, folder.name, 0, folder));
  return rows;
}

export function visibleWorktrees(state: StoreState, projectId: string): WorktreeDto[] {
  return state.worktrees.filter((tree) => tree.projectId === projectId && (tree.isPrimary || tree.present !== false));
}

export function worktreeLabel(tree: WorktreeDto | undefined): string {
  return tree ? `${tree.branch}${tree.repositoryRoot ? ` · ${tree.repositoryRoot.split("/").filter(Boolean).pop()}` : ""}` : "UNASSIGNED";
}

export function taskMatches(task: TaskDto, state: StoreState, query: string): boolean {
  return matches(query, task.id, task.title, task.details, task.status, worktreeLabel(state.worktrees.find((tree) => tree.id === task.worktreeId)));
}

export function shellLabel(shell: ShellSessionDto, state: StoreState): string {
  if (shell.editorPath) return `VIM · ${shell.editorPath.split("/").pop() ?? shell.editorPath}`;
  const trees = state.worktrees.filter((tree) => tree.projectId === shell.projectId && (shell.cwd === tree.path || shell.cwd.startsWith(`${tree.path}/`)));
  trees.sort((a, b) => b.path.length - a.path.length);
  return trees[0]?.branch ?? state.worktrees.find((tree) => tree.id === shell.worktreeId)?.branch ?? shell.harness ?? "Shell";
}

export function shellMatches(shell: ShellSessionDto, state: StoreState, query: string): boolean {
  return matches(query, "shell", shell.harness, shell.id, shellLabel(shell, state), shell.cwd, shell.command, shell.alive ? "live" : "exited");
}

export function taskColor(status: string): string { return status === "done" ? "#7acddd" : status === "in_progress" ? "#f1ce85" : "#aeb5c2"; }
export function taskMark(status: string): string { return status === "done" ? "■" : status === "in_progress" ? "◧" : "□"; }
