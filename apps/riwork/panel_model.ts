import type { FolderDto, ProjectDto, ShellSessionDto, StoreState, TaskDto, WorktreeDto } from "riwork/native";
import { byName, matches, treeRows, type TreeRow } from "./project_tree";

export type ListPanel = "projects" | "worktrees" | "tasks" | "shells";
export type ProjectOrder = "name" | "edited" | "added" | "live";
export type ProjectTreeRow = TreeRow<ProjectDto, FolderDto>;

export const EMPTY_STORE: StoreState = {
  schemaVersion: 1, activeProjectId: null, projects: [], projectFolders: [], worktrees: [], tasks: [],
  orcaImport: null, projectMemos: {}, projectRecency: {},
};

export { matches };

export function countLabel(count: number, total: number, noun: string): string {
  return `${count === total ? count : `${count} of ${total}`} ${total === 1 ? noun : `${noun}s`}`;
}

/** This is a presentation projection only; all tree mutations are validated by Store. The tree itself is
 * `project_tree.ts`, shared with the iPhone and iPad Projects page. */
export function projectRows(
  state: StoreState, query: string, collapsed: ReadonlySet<string>,
  order: ProjectOrder = "edited", descending = true, sessions: ShellSessionDto[] = [],
): ProjectTreeRow[] {
  const live = new Map<string, number>();
  for (const shell of sessions) if (shell.alive && shell.projectId) live.set(shell.projectId, (live.get(shell.projectId) ?? 0) + 1);
  const recency = state.projectRecency ?? {};
  const projects = [...state.projects].sort((a, b) => {
    if (order === "name") return byName(a, b) * (descending ? -1 : 1);
    const av = order === "live" ? live.get(a.id) ?? 0 : order === "edited" ? recency[a.id] ?? 0 : a.createdAt;
    const bv = order === "live" ? live.get(b.id) ?? 0 : order === "edited" ? recency[b.id] ?? 0 : b.createdAt;
    // Unknown dates remain last in either direction, as engine::project_sort.
    if (order !== "live" && (!av || !bv)) return av ? -1 : bv ? 1 : byName(a, b);
    return (av - bv) * (descending ? -1 : 1) || byName(a, b);
  });
  return treeRows(projects, state.projectFolders, query, collapsed);
}

export function visibleWorktrees(state: StoreState, projectId: string): WorktreeDto[] {
  return state.worktrees.filter((tree) => tree.projectId === projectId && (tree.isPrimary || tree.present !== false));
}

export function worktreeLabel(tree: WorktreeDto | undefined): string {
  return tree ? `${tree.branch}${tree.repositoryRoot ? ` · ${tree.repositoryRoot.split("/").filter(Boolean).pop()}` : ""}` : "Unassigned";
}

/** A task's worktree as shown; an assignment whose worktree is gone keeps its short id. */
export function taskWorktree(task: TaskDto, state: StoreState): string {
  const tree = state.worktrees.find((tree) => tree.id === task.worktreeId);
  return tree || !task.worktreeId ? worktreeLabel(tree) : `Worktree ${task.worktreeId.slice(0, 8)}`;
}

export function taskMatches(task: TaskDto, state: StoreState, query: string): boolean {
  return matches(query, task.id, task.title, task.details, task.status, taskStatusLabel(task.status), taskWorktree(task, state));
}

export type TaskFilter = "open" | "done" | "all";
export type TaskRow =
  | { kind: "section"; id: string; status: string; count: number }
  | { kind: "task"; id: string; task: TaskDto };

const TASK_ORDER = ["in_progress", "todo", "done"];
const taskRank = (status: string) => { const rank = TASK_ORDER.indexOf(status); return rank < 0 ? 1.5 : rank; };

export function taskStatusLabel(status: string): string {
  return status === "in_progress" ? "In progress" : status === "done" ? "Done" : status === "todo" ? "Todo" : status;
}

export function taskInFilter(task: TaskDto, filter: TaskFilter): boolean {
  return filter === "all" || (filter === "done") === (task.status === "done");
}

export function taskCounts(tasks: TaskDto[]): Record<TaskFilter, number> {
  const done = tasks.filter((task) => task.status === "done").length;
  return { open: tasks.length - done, done, all: tasks.length };
}

/** Open work first (in progress, then todo, then done), each newest first. Sections head each status when more than one is shown. */
export function taskRows(tasks: TaskDto[], filter: TaskFilter): TaskRow[] {
  const shown = tasks.filter((task) => taskInFilter(task, filter)).sort((a, b) =>
    taskRank(a.status) - taskRank(b.status) || (a.status < b.status ? -1 : a.status > b.status ? 1 : 0) ||
    b.updatedAt - a.updatedAt || b.createdAt - a.createdAt || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  const sections = new Set(shown.map((task) => task.status)).size > 1;
  const rows: TaskRow[] = [];
  let previous: string | undefined;
  for (const task of shown) {
    if (sections && task.status !== previous)
      rows.push({ kind: "section", id: `section-${task.status}`, status: task.status, count: shown.filter((other) => other.status === task.status).length });
    previous = task.status;
    rows.push({ kind: "task", id: task.id, task });
  }
  return rows;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** A calendar day for a Unix time in seconds; the year only when it is not the current one. */
export function taskDate(seconds: number, now: number): string {
  const date = new Date(seconds * 1000), year = date.getFullYear();
  return `${MONTHS[date.getMonth()]} ${date.getDate()}${year === new Date(now * 1000).getFullYear() ? "" : `, ${year}`}`;
}

/** How long ago a Unix time in seconds was; older than a week reads as its date. */
export function taskAge(seconds: number, now: number): string {
  const elapsed = now - seconds;
  if (elapsed < 60) return "just now";
  if (elapsed < 3600) return `${Math.floor(elapsed / 60)}m ago`;
  if (elapsed < 86400) return `${Math.floor(elapsed / 3600)}h ago`;
  if (elapsed < 7 * 86400) return `${Math.floor(elapsed / 86400)}d ago`;
  return taskDate(seconds, now);
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
