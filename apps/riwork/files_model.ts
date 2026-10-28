/** The native bridge owns filesystem access. These values are display data only. */
export interface FileEntry {
  name: string; path: string; relativePath: string;
  kind: "directory" | "file" | "symlink" | "other";
  identity?: string; gitStatus?: string;
}
export interface DirectoryListing {
  root: string; path: string; entries: FileEntry[];
  omitted: number; scanLimited: boolean; error?: string;
}
export interface FilePreview {
  kind: "text" | "image" | "pdf" | "message";
  text?: string; lines?: string[]; truncated: boolean; markdown: boolean;
  dataUrl?: string; mime?: string; width?: number; height?: number;
  message?: string; page?: number; pages?: number;
}
export interface FileRow { entry: FileEntry; depth: number }

/** Filtering searches only loaded folders, while retaining every matching ancestor. */
export function fileRows(root: string | null, listings: ReadonlyMap<string, DirectoryListing>, expanded: ReadonlySet<string>, query: string, hidden: boolean): FileRow[] {
  if (!root) return [];
  const needle = query.trim().toLocaleLowerCase();
  const allowed = (entry: FileEntry) => hidden || !entry.relativePath.split("/").some((part) => part.startsWith("."));
  const matches = new Set<string>();
  if (needle) for (const listing of listings.values()) for (const entry of listing.entries) {
    if (allowed(entry) && entry.relativePath.toLocaleLowerCase().includes(needle)) {
      matches.add(entry.path);
      let parent = entry.path.slice(0, entry.path.lastIndexOf("/"));
      while (parent.length > root.length) { matches.add(parent); parent = parent.slice(0, parent.lastIndexOf("/")); }
    }
  }
  const rows: FileRow[] = [], seen = new Set<string>();
  function append(path: string, depth: number) {
    if (seen.has(path)) return;
    seen.add(path);
    for (const entry of listings.get(path)?.entries ?? []) {
      if (!allowed(entry) || (needle && !matches.has(entry.path))) continue;
      rows.push({ entry, depth });
      if (entry.kind === "directory" && (needle || expanded.has(entry.path))) append(entry.path, depth + 1);
    }
  }
  append(root, 0);
  return rows;
}

/** Forget descendants when their parent is removed or replaced by a non-directory. */
export function reconcileListings(previous: ReadonlyMap<string, DirectoryListing>, listing: DirectoryListing): Map<string, DirectoryListing> {
  const next = new Map(previous);
  next.set(listing.path, listing);
  const directories = new Set(listing.entries.filter((entry) => entry.kind === "directory").map((entry) => entry.path));
  for (const path of next.keys()) {
    if (path === listing.path || !path.startsWith(`${listing.path}/`)) continue;
    const relative = path.slice(listing.path.length + 1);
    const child = `${listing.path}/${relative.split("/")[0]}`;
    if (!directories.has(child)) next.delete(path);
  }
  return next;
}

export function omittedLabel(count: number, limited: boolean): string {
  return `${count}${limited ? "+" : ""} more ${count === 1 && !limited ? "entry" : "entries"} not shown`;
}

export function utf8Bytes(text: string): number {
  let bytes = 0;
  for (const char of text) { const point = char.codePointAt(0)!; bytes += point < 0x80 ? 1 : point < 0x800 ? 2 : point < 0x10000 ? 3 : 4; }
  return bytes;
}

export const memoBytes = (text: string) => utf8Bytes(text.replace(/\r\n?/g, "\n").replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/g, ""));

export function directoryData(value: { root: string; path: string; entries: { name: string; path: string; relativePath: string; kind: string; identity?: string | null; gitStatus?: string | null }[]; omitted: number; scanLimited: boolean; error?: string | null }): DirectoryListing {
  return { ...value, error: value.error ?? undefined, entries: value.entries.map((entry) => ({ ...entry,
    kind: entry.kind === "directory" || entry.kind === "file" || entry.kind === "symlink" ? entry.kind : "other",
    identity: entry.identity ?? undefined, gitStatus: entry.gitStatus ?? undefined })) };
}

export function previewData(value: { kind: string; text?: string | null; lines?: string[] | null; truncated: boolean; markdown: boolean; dataUrl?: string | null; mime?: string | null; width?: number | null; height?: number | null; page?: number | null; pages?: number | null; message?: string | null }): FilePreview {
  return { kind: value.kind === "text" || value.kind === "image" || value.kind === "pdf" ? value.kind : "message",
    text: value.text ?? undefined, lines: value.lines ?? undefined, truncated: value.truncated, markdown: value.markdown,
    dataUrl: value.dataUrl ?? undefined, mime: value.mime ?? undefined, width: value.width ?? undefined, height: value.height ?? undefined,
    page: value.page ?? undefined, pages: value.pages ?? undefined, message: value.message ?? (value.kind === "message" ? undefined : "This item cannot be previewed.") };
}
