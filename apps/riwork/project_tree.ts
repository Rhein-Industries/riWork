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
  const rank = new Map(folders.map((folder, index) => [folder.id, index]));
  const completed = new Set<string>();
  for (const folder of folders) {
    const chain: string[] = [], positions = new Map<string, number>();
    let id: string | null | undefined = folder.id;
    while (id && !completed.has(id)) {
      const cycle = positions.get(id);
      if (cycle !== undefined) {
        let root = chain[cycle];
        for (let at = cycle + 1; at < chain.length; at++) {
          if (rank.get(chain[at])! < rank.get(root)!) root = chain[at];
        }
        parent.set(root, null);
        break;
      }
      positions.set(id, chain.length);
      chain.push(id); id = parent.get(id);
    }
    chain.forEach((id) => completed.add(id));
  }
  const children = new Map<string | null, F[]>(), direct = new Map<string | null, P[]>();
  for (const folder of folders) {
    const owner = parent.get(folder.id) ?? null;
    const siblings = children.get(owner);
    if (siblings) siblings.push(folder); else children.set(owner, [folder]);
  }
  for (const project of projects) {
    const owner = project.folderId && ids.has(project.folderId) ? project.folderId : null;
    const siblings = direct.get(owner);
    if (siblings) siblings.push(project); else direct.set(owner, [project]);
  }
  // Count every subtree once, including collapsed descendants. Iterative walks
  // also keep deeply nested imported folders off the JavaScript call stack.
  const roots = children.get(null) ?? [], walk = [...roots], traversal: F[] = [];
  while (walk.length) {
    const folder = walk.pop()!;
    traversal.push(folder);
    for (const child of children.get(folder.id) ?? []) walk.push(child);
  }
  const counts = new Map<string, number>();
  for (let at = traversal.length - 1; at >= 0; at--) {
    const folder = traversal[at];
    const count = (direct.get(folder.id)?.length ?? 0) + (counts.get(folder.id) ?? 0);
    counts.set(folder.id, count);
    const owner = parent.get(folder.id);
    if (owner) counts.set(owner, (counts.get(owner) ?? 0) + count);
  }
  const rows: TreeRow<P, F>[] = [];
  function add(id: string, name: string, depth: number, folder?: F): boolean {
    const unfiled = id === "unfiled";
    const owned = direct.get(unfiled ? null : id) ?? [], nested = unfiled ? [] : children.get(id) ?? [];
    rows.push({ kind: "folder", id: `folder-${id}`, name, depth, count: unfiled ? owned.length : counts.get(id) ?? 0, folder });
    if (collapsed.has(id)) return false;
    owned.forEach((project) => rows.push({ kind: "project", id: `project-${project.id}`, depth: depth + 1, project }));
    if (!owned.length && !nested.length) rows.push({ kind: "empty", id: `empty-${id}`, depth: depth + 1 });
    return !unfiled;
  }
  add("unfiled", "Unfiled", 0);
  const pending = roots.map((folder) => ({ folder, depth: 0 })).reverse();
  while (pending.length) {
    const { folder, depth } = pending.pop()!;
    if (!add(folder.id, folder.name, depth, folder)) continue;
    const nested = children.get(folder.id) ?? [];
    for (let at = nested.length - 1; at >= 0; at--) pending.push({ folder: nested[at], depth: depth + 1 });
  }
  return rows;
}
