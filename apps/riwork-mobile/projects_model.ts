/** The Projects page's list: the host's projects in the order the person chose (`riwork.projectSort`,
 * through `platform.prefs` `projectSort`), with the Mac's folders when the host gives them
 * (`Store.snapshot`) and what each project's agents are doing (`projects.list`). */
import type { ProjectSummary, StoreFolder, StoreTree } from "../riwork/client/types";
import { byName, treeRows, type TreeRow } from "../riwork/project_tree";

/** The phone's three orders, each in the direction the Mac starts with. */
export type ProjectSort = "recent" | "name" | "dateAdded";
export const PROJECT_SORTS: readonly { value: ProjectSort; label: string; detail: string }[] = [
  { value: "recent", label: "Recent", detail: "Latest terminal activity, then edits" },
  { value: "name", label: "Name", detail: "A to Z" },
  { value: "dateAdded", label: "Date added", detail: "Newest first" },
];
export function projectSort(value: unknown): ProjectSort {
  return value === "name" || value === "dateAdded" ? value : "recent";
}

/** A project as the page lists it. */
export interface ListedProject extends ProjectSummary { folderId: string | null }

/** `projects.list`, with each project's folder from the tree (when there is one) and the tree's edits for a
 * project the list has none for. Without the list (it failed) the tree's projects stand alone. */
export function listedProjects(list: ProjectSummary[] | null, tree: StoreTree | null): ListedProject[] {
  const folders = new Map(tree?.projects.map((project) => [project.id, project.folderId ?? null]) ?? []);
  if (list) {
    return list.map((project) => {
      const listed: ListedProject = { ...project, folderId: folders.get(project.id) ?? null };
      const edited = tree?.projectRecency[project.id];
      if (listed.lastEdited == null && edited) listed.lastEdited = edited;
      return listed;
    });
  }
  return (tree?.projects ?? []).map((project) => {
    const listed: ListedProject = { id: project.id, name: project.name, root: project.root, createdAt: project.createdAt, folderId: project.folderId ?? null };
    const edited = tree?.projectRecency[project.id];
    if (edited) listed.lastEdited = edited;
    return listed;
  });
}

/** The phone's order (`ios/Core/ProjectSort.swift`, `ProjectSorting`): Recent in four steps (terminal activity,
 * then file edits, then date added, then the rest, each newest first; a just-made project counts as active);
 * Date added newest first; Name A to Z. Ties: name without regard to case, as written, then id. */
export function sortProjects<P extends ListedProject>(projects: readonly P[], sort: ProjectSort, touched: Record<string, number> = {}): P[] {
  const known = (time?: number) => time && time > 0 ? time : undefined;
  const key = (project: P): [number, number] => {
    if (sort === "name") return [0, 0];
    if (sort === "dateAdded") { const added = known(project.createdAt); return added ? [0, added] : [1, 0]; }
    const active = known(Math.max(project.lastActivity ?? 0, touched[project.id] ?? 0));
    if (active) return [0, active];
    const edited = known(project.lastEdited);
    if (edited) return [1, edited];
    const added = known(project.createdAt);
    return added ? [2, added] : [3, 0];
  };
  return [...projects].sort((a, b) => {
    const [ra, ta] = key(a), [rb, tb] = key(b);
    return ra - rb || tb - ta || byName(a, b);
  });
}

/** The rows: a search lists its hits flat (name hits first); otherwise the folders, when the host has any. */
export function projectListRows(projects: readonly ListedProject[], folders: readonly StoreFolder[], query: string, collapsed: ReadonlySet<string>): TreeRow<ListedProject, StoreFolder>[] {
  if (!folders.length && !query.trim()) return projects.map((project) => ({ kind: "project", id: `project-${project.id}`, depth: 0, project }));
  return treeRows(projects, folders, query, collapsed);
}

/** What a project's agents are doing, in words, and the tone to draw it in. */
export function projectActivity(project: ProjectSummary): { text: string; tone: "gold" | "working" | "muted" } | null {
  const agents = project.agents;
  if (!agents) return null;
  const parts: string[] = [];
  if (agents.working) parts.push(`${agents.working} working`);
  if (agents.waiting) parts.push(`${agents.waiting} waiting`);
  if (agents.done) parts.push(`${agents.done} done`);
  if (!parts.length) return null;
  return { text: parts.join(" · "), tone: agents.waiting ? "gold" : agents.working ? "working" : "muted" };
}
