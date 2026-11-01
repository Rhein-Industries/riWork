/** The project list's tree: folders, the projects in each, and the search. Shared
 * code (the Mac's Projects panel through `panel_model.ts`, the iPhone and iPad
 * Projects page): it imports nothing native, and takes any project and folder
 * shape with the fields it reads. */

export interface TreeProject { id: string; name: string; root: string; folderId?: string | null }
export interface TreeFolder { id: string; name: string; parentId?: string | null }
export type TreeRow<P extends TreeProject, F extends TreeFolder> =
  | { kind: "folder"; id: string; name: string; depth: number; count: number; folder?: F }
  | { kind: "project"; id: string; depth: number; project: P }
  | { kind: "empty"; id: string; depth: number };

export function matches(query: string, ...fields: (string | null | undefined)[]): boolean {
  const needle = query.trim().toLowerCase();
  return !needle || fields.some((field) => field?.toLowerCase().includes(needle));
}

/** Name without regard to case, then as written, then id: both sides list the same order. */
export function byName(a: { id: string; name: string }, b: { id: string; name: string }): number {
  const first = a.name.toLowerCase(), second = b.name.toLowerCase();
  return (first < second ? -1 : first > second ? 1 : 0) ||
    (a.name < b.name ? -1 : a.name > b.name ? 1 : 0) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
}

/** `projects` (already in the chosen order) as rows: with a search, the hits as a flat list, name hits
 * first; without one, Unfiled and then each folder by name, nested, with their counts, a collapsed
 * folder's projects left out. This is a presentation projection only; all tree mutations are validated
 * by the host's store. */
export function treeRows<P extends TreeProject, F extends TreeFolder>(
  projects: readonly P[], projectFolders: readonly F[], query: string, collapsed: ReadonlySet<string>,
): TreeRow<P, F>[] {
  if (query.trim()) {
    // Name hits precede id/path hits, without changing the chosen order within either group.
    const name = projects.filter((project) => matches(query, project.name));
    return [...name, ...projects.filter((project) => !matches(query, project.name) && matches(query, project.id, project.root))]
      .map((project) => ({ kind: "project", id: `project-${project.id}`, depth: 0, project }));
  }
  const folders = [...projectFolders].sort(byName);
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
  const rows: TreeRow<P, F>[] = [];
  function add(id: string, name: string, depth: number, folder?: F) {
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
