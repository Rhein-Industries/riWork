import { Clipboard, Image, Scroll, Text, View, cleanup, effect, keymap, signal, type YNode } from "nsh";
import { Button, Editor, Input, TextView, type EditorRef } from "nsh/base";
import { Files, Store, type Subscription } from "riwork/native";
import { DEFAULT_THEME, type InterfaceTheme } from "./appearance";
import { directoryData, fileRows, memoBytes, omittedLabel, previewData, reconcileListings, type DirectoryListing, type FileEntry, type FilePreview } from "./files_model";

export type { FileEntry } from "./files_model";
interface FilesOptions {
  root: () => string | null; label: () => string;
  theme?: () => InterfaceTheme; visible?: () => boolean;
  active?: () => "files" | "preview" | null;
  previewRequested: (asked?: boolean) => void;
  edit: (entry: FileEntry, line?: number) => void | Promise<void>;
  open?: (entry: FileEntry) => void | Promise<void>;
  reveal?: (path: string) => void | Promise<void>;
  closePreview?: () => void;
}

/** One owner per workspace, shared by every Files and Preview panel in that window. */
export function createFilesController(options: FilesOptions) {
  const listings = signal<ReadonlyMap<string, DirectoryListing>>(new Map());
  const expanded = signal<ReadonlySet<string>>(new Set());
  const query = signal(""), hidden = signal(false), selected = signal<FileEntry | null>(null);
  const preview = signal<FilePreview | null>(null), previewEntry = signal<FileEntry | null>(null);
  const loading = signal(false), previewLoading = signal(false), notice = signal("");
  const renderedMarkdown = signal(true), line = signal<number | undefined>(undefined);
  const treeFocused = signal(false);
  const filterFocused = signal(false), focused = signal<"files" | "preview" | null>(null), zoom = signal(1);
  const searches = new Map<string, YNode>();
  const theme = options.theme ?? (() => DEFAULT_THEME);
  const watches = new Map<string, Subscription>(), revisions = new Map<string, number>();
  let currentRoot: string | null = null, generation = 0, previewGeneration = 0, alive = true;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const rows = () => fileRows(currentRoot, listings(), expanded(), query(), hidden());
  const setNotice = (message: unknown) => { if (alive) notice.set(String(message)); };
  const visible = () => options.visible?.() ?? true;
  function closeWatches() { for (const watcher of watches.values()) watcher.close(); watches.clear(); revisions.clear(); }
  function receive(listing: DirectoryListing, token: number) {
    if (!alive || token !== generation || listing.root !== currentRoot) return;
    // A late child load must never recreate a folder evicted by its parent.
    if (listing.path !== currentRoot && ![...listings().values()].some((parent) => parent.entries.some((entry) => entry.path === listing.path && entry.kind === "directory"))) return;
    const prior = selected();
    const previous = listings(), next = reconcileListings(previous, listing);
    listings.set(next); loading.set(false);
    // Evicted descendants can no longer deliver into this generation of the tree.
    const directories = new Set([...next.values()].flatMap((parent) => parent.entries.filter((entry) => entry.kind === "directory").map((entry) => entry.path)));
    for (const [path, subscription] of watches) if (path !== currentRoot && !directories.has(path)) { subscription.close(); watches.delete(path); }
    const stillExpanded = new Set([...expanded()].filter((path) => directories.has(path)));
    if (stillExpanded.size !== expanded().size) expanded.set(stillExpanded);
    const visibleRows = rows();
    if (prior) {
      const replacement = [...next.values()].flatMap((value) => value.entries).find((entry) => entry.path === prior.path);
      if (replacement && (replacement.identity !== prior.identity || replacement.kind !== prior.kind)) {
        selected.set(replacement);
        if (previewEntry()?.path === replacement.path) void loadPreview(replacement, false);
      } else if (!visibleRows.some((row) => row.entry.path === prior.path)) selected.set(visibleRows[0]?.entry ?? null);
    } else selected.set(visibleRows[0]?.entry ?? null);
  }
  function watch(path: string) {
    if (!currentRoot || !visible() || watches.has(path)) return;
    const token = generation;
    try {
      const subscription = Files.subscribe(currentRoot, path, hidden(), (event) => {
        if (token !== generation || event.rev < (revisions.get(path) ?? 0)) return;
        revisions.set(path, event.rev); receive(directoryData(event.data), token);
      });
      watches.set(path, subscription);
    } catch (reason) { setNotice(reason); loading.set(false); }
  }
  async function load(path: string) {
    if (!currentRoot) return;
    const root = currentRoot, token = generation;
    try { receive(directoryData(await Files.listDirectory(root, path, hidden())), token); }
    catch (reason) { if (token === generation) { setNotice(reason); loading.set(false); } }
  }
  function resetRoot() {
    generation++; previewGeneration++; closeWatches(); clearTimeout(timer);
    currentRoot = options.root();
    listings.set(new Map()); expanded.set(new Set()); query.set(""); selected.set(null);
    preview.set(null); previewEntry.set(null); line.set(undefined); notice.set(""); renderedMarkdown.set(true); zoom.set(1);
    loading.set(!!currentRoot); previewLoading.set(false);
    if (currentRoot) { watch(currentRoot); void load(currentRoot); }
  }
  effect(() => { const next = options.root(); if (next !== currentRoot) resetRoot(); });
  // Visibility changes only arm subscriptions. There is no window or per-frame polling.
  effect(() => {
    if (!visible()) { closeWatches(); return; }
    if (currentRoot) watch(currentRoot);
    for (const path of expanded()) watch(path);
  });
  cleanup(() => { alive = false; generation++; previewGeneration++; clearTimeout(timer); closeWatches(); searches.clear(); });

  async function loadPreview(entry: FileEntry, explicit: boolean, page?: number) {
    clearTimeout(timer);
    const root = currentRoot;
    if (!root) return;
    const token = ++previewGeneration, rootToken = generation;
    previewLoading.set(true);
    try {
      const result = await Files.preview(root, entry.path, entry.identity, explicit, page);
      if (!alive || token !== previewGeneration || rootToken !== generation) return;
      preview.set(previewData(result)); previewEntry.set(entry);
    } catch (reason) { if (alive && token === previewGeneration && rootToken === generation) { preview.set({ kind: "message", message: String(reason), truncated: false, markdown: false }); previewEntry.set(entry); } }
    finally { if (alive && token === previewGeneration && rootToken === generation) previewLoading.set(false); }
  }
  function select(entry: FileEntry, explicit = true) {
    selected.set(entry); notice.set(""); line.set(undefined);
    if (entry.kind === "directory") toggle(entry);
    options.previewRequested();
    if (explicit) void loadPreview(entry, true);
    else { clearTimeout(timer); previewGeneration++; timer = setTimeout(() => void loadPreview(entry, false), 150); }
  }
  function toggle(entry: FileEntry) {
    if (entry.kind !== "directory") return;
    const next = new Set(expanded());
    if (next.delete(entry.path)) {
      for (const path of next) if (path.startsWith(`${entry.path}/`)) next.delete(path);
      for (const [path, subscription] of watches) if (path === entry.path || path.startsWith(`${entry.path}/`)) { subscription.close(); watches.delete(path); }
    }
    else { next.add(entry.path); watch(entry.path); void load(entry.path); }
    expanded.set(next);
  }
  function refresh() {
    if (!currentRoot) return;
    // A failed native watcher can recover on a user refresh.
    closeWatches(); watch(currentRoot); void load(currentRoot); for (const path of expanded()) { watch(path); void load(path); }
    if (previewEntry()) void loadPreview(previewEntry()!, preview()?.kind === "pdf", preview()?.page);
  }
  function clearPreview() { previewGeneration++; clearTimeout(timer); preview.set(null); previewEntry.set(null); line.set(undefined); zoom.set(1); previewLoading.set(false); }
  function toggleHidden() {
    hidden.set(!hidden()); generation++; closeWatches();
    if (currentRoot) { watch(currentRoot); void load(currentRoot); }
    for (const path of expanded()) { watch(path); void load(path); }
  }
  async function edit(entry = selected(), atLine = line()) {
    if (!entry || !currentRoot || entry.kind !== "file" || !entry.identity) return;
    // Sessions.openEditor reuses a live editor first, then performs the atomic
    // containment and identity check before a new Vim launch.
    try { await options.edit(entry, atLine); } catch (reason) { setNotice(reason); }
  }
  async function open(entry = selected()) {
    if (!entry || !currentRoot) return;
    const root = currentRoot, token = generation;
    try { const path = await Files.validateOpen(root, entry.path, entry.identity); if (alive && token === generation) await options.open?.({ ...entry, path }); }
    catch (reason) { setNotice(reason); }
  }
  async function reveal(path: string, atLine?: number) {
    const root = currentRoot;
    if (root && path === root) { query.set(""); await load(root); return; }
    if (!root || !path.startsWith(`${root}/`)) { setNotice("That is not inside the folder Files shows."); return; }
    // This lexical display plan is never authorization: every load and read is contained natively.
    if (!hidden() && path.slice(root.length + 1).split("/").some((part) => part.startsWith("."))) toggleHidden();
    const token = generation;
    query.set("");
    const parts = path.slice(root.length + 1).split("/");
    let parent = root;
    for (let i = 0; i < parts.length; i++) {
      await load(parent); if (!alive || token !== generation) return;
      const entry = listings().get(parent)?.entries.find((item) => item.name === parts[i]);
      if (!entry) { setNotice("Cannot preview this file: it is missing or not listed in Files."); return; }
      if (i === parts.length - 1) {
        selected.set(entry); line.set(atLine); if (atLine) renderedMarkdown.set(false);
        if (entry.kind === "directory") { const next = new Set(expanded()); next.add(entry.path); expanded.set(next); watch(entry.path); await load(entry.path); return; }
        options.previewRequested(true); void loadPreview(entry, true); return;
      }
      if (entry.kind !== "directory") { setNotice("Symbolic links are not expanded. Use Reveal instead."); return; }
      const next = new Set(expanded()); next.add(entry.path); expanded.set(next); watch(entry.path); parent = entry.path;
    }
  }
  async function copyContents() {
    const entry = previewEntry(), root = currentRoot;
    if (!entry?.identity || !root) return;
    const token = generation;
    try { const text = await Files.readText(root, entry.path, entry.identity); if (alive && token === generation) { void Clipboard.writeText(text); notice.set(`Copied ${text.split("\n").length} lines`); } }
    catch (reason) { setNotice(reason); }
  }
  function move(direction: number) {
    const current = rows(), index = current.findIndex((row) => row.entry.path === selected()?.path);
    const row = current[Math.max(0, Math.min(current.length - 1, index + direction))];
    if (row) { selected.set(row.entry); if (row.entry.kind === "file") { options.previewRequested(); clearTimeout(timer); previewGeneration++; timer = setTimeout(() => void loadPreview(row.entry, false), 150); } }
  }
  const active = () => visible() && (options.active ? options.active() : focused());
  function navigate(key: string) {
    if (active() !== "files" || filterFocused()) return;
    treeFocused.set(true);
    if (key === "up" || key === "down") { move(key === "up" ? -1 : 1); return; }
    const currentRows = rows(), entry = selected();
    if (key === "home" || key === "end") { const row = currentRows[key === "home" ? 0 : currentRows.length - 1]; if (row) { selected.set(row.entry); if (row.entry.kind === "file") { options.previewRequested(); void loadPreview(row.entry, false); } } return; }
    if (!entry) return;
    if (key === "right") {
      if (entry.kind === "directory" && !expanded().has(entry.path)) toggle(entry);
      else { const index = currentRows.findIndex((row) => row.entry.path === entry.path); const child = currentRows[index + 1]; if (child && child.depth > currentRows[index].depth) selected.set(child.entry); }
    } else if (key === "left") {
      if (entry.kind === "directory" && expanded().has(entry.path)) toggle(entry);
      else { const parent = entry.path.slice(0, entry.path.lastIndexOf("/")); const row = currentRows.find((row) => row.entry.path === parent); if (row) selected.set(row.entry); }
    } else if (key === "enter") {
      if (entry.kind === "directory") toggle(entry);
      else if (entry.name.toLowerCase().endsWith(".pdf") && preview()?.kind !== "pdf") { options.previewRequested(); void loadPreview(entry, true); }
      else void edit();
    }
  }
  effect(() => {
    const view = active();
    if (!view) return;
    const bindings: (() => void)[] = [];
    bindings.push(keymap.bind("mod+r", refresh));
    if (!filterFocused()) {
      bindings.push(keymap.bind("mod+e", () => void edit(view === "preview" ? previewEntry() : selected())));
      bindings.push(keymap.bind("mod+o", () => void open(view === "preview" ? previewEntry() : selected())));
      if (view === "files") {
        for (const key of ["up", "down", "home", "end", "left", "right", "enter"]) bindings.push(keymap.bind(key, () => navigate(key)));
        bindings.push(keymap.bind("mod+.", toggleHidden));
        bindings.push(keymap.bind("mod+c", () => { if (selected()) { void Clipboard.writeText(selected()!.relativePath); notice.set(`Copied ${selected()!.relativePath}`); } }));
      }
    }
    if (view === "preview") {
      if (preview()?.kind === "pdf") for (const key of ["left", "right"]) bindings.push(keymap.bind(key, () => {
        if (preview()?.kind !== "pdf" || previewLoading()) return;
        const page = (preview()?.page ?? 1) + (key === "left" ? -1 : 1);
        if (page >= 1 && page <= (preview()?.pages ?? 1) && previewEntry()) void loadPreview(previewEntry()!, true, page);
      }));
      for (const key of ["mod+=", "mod+shift+=", "mod+-", "mod+0"]) bindings.push(keymap.bind(key, () => {
        zoom.set(key === "mod+0" ? 1 : Math.max(preview()?.kind === "text" ? 0.5 : 0.05, Math.min(5, zoom() * (key === "mod+-" ? 1 / 1.2 : 1.2))));
      }));
    }
    return () => { for (const unbind of bindings) unbind(); };
  });
  effect(() => { if (active() !== "files" || !query()) return; return keymap.bind("escape", () => { query.set(""); filterFocused.set(false); treeFocused.set(true); }); });
  return { ...options, theme, listings, expanded, query, hidden, selected, preview, previewEntry, loading, previewLoading, notice, renderedMarkdown, line, treeFocused,
    rows, resetRoot, refresh, toggleHidden, select, toggle, loadPreview, edit, open, reveal, copyContents, move, filterFocused, focused, searches, zoom, clearPreview,
    revealInFinder: (path: string) => { void Promise.resolve(options.reveal?.(path)).catch(setNotice); } };
}
export type FilesController = ReturnType<typeof createFilesController>;

function styles(c: FilesController) {
  const t = c.theme();
  return { text: { fontFamily: t.family, fontSize: 11 * t.scale, color: t.palette.text }, muted: { fontFamily: t.family, fontSize: 10 * t.scale, color: t.palette.muted },
    button: { paddingHorizontal: 7, paddingVertical: 5, borderRadius: 4, backgroundColor: t.palette.panelActive } };
}
export function FilesPanel(props: { controller: FilesController; surface?: string }) {
  const c = props.controller, id = props.surface ?? "files";
  let search: YNode | undefined;
  cleanup(() => c.searches.delete(id));
  const empty = () => !c.root() ? "Select a worktree to browse its files." : c.loading() ? "Loading worktree…" : c.listings().get(c.root()!)?.error || (c.query() ? "No loaded files match. Clear the filter and expand folders to load more." : !c.hidden() ? "No visible files. Turn on Hidden to show dotfiles." : "This folder is empty.");
  return <View testID="files-panel" onPress={() => c.focused.set("files")} style={{ flex: 1, minHeight: 0, gap: 6 }}>
    <View style={{ padding: 8, gap: 6 }}>
      <Text numberOfLines={1} style={styles(c).text}>{c.label()}</Text>
      <View style={{ flexDirection: "row", gap: 5, alignItems: "center" }}>
        <Text style={styles(c).muted}>{`${c.rows().length} items`}</Text><View style={{ flex: 1 }} />
        <Button testID={`files-refresh:${id}`} accessibilityLabel="Refresh Files" onPress={c.refresh} style={styles(c).button}><Text style={styles(c).text}>Refresh</Text></Button>
        <Button testID={`files-hidden:${id}`} accessibilityLabel="Show hidden files" selected={c.hidden()} onPress={c.toggleHidden} style={styles(c).button}><Text style={styles(c).text}>{c.hidden() ? "● Hidden" : "○ Hidden"}</Text></Button>
        <Button testID={`files-reveal-root:${id}`} disabled={!c.root()} accessibilityLabel="Reveal worktree in Finder" onPress={() => { if (c.root()) c.revealInFinder(c.root()!); }} style={styles(c).button}><Text style={styles(c).text}>Reveal</Text></Button>
      </View>
      <Input testID={`files-filter:${id}`} ref={(node) => { search = node; c.searches.set(id, node); }} accessibilityLabel="Filter loaded files" placeholder="Filter loaded files…" value={c.query()} onChange={(value) => c.query.set(value)} onFocus={() => { c.focused.set("files"); c.filterFocused.set(true); }} onBlur={() => c.filterFocused.set(false)} onSubmit={() => { c.filterFocused.set(false); c.treeFocused.set(true); }} />
      {c.query() !== "" && <Button testID={`files-clear:${id}`} onPress={() => { c.query.set(""); search?.focus(); }}><Text style={styles(c).muted}>Clear filter</Text></Button>}
    </View>
    {c.notice() !== "" && <Text testID="files-notice" role="alert" style={{ ...styles(c).text, color: c.theme().palette.gold }}>{c.notice()}</Text>}
    <Scroll testID={`files-tree:${id}`} style={{ flex: 1, minHeight: 0 }}>
      <View role="list" accessibilityLabel="Files">
        {c.rows().map(({ entry, depth }) => <View testID={`file:${entry.relativePath}`} role="row" accessibilityLabel={entry.relativePath}
          accessibilityState={{ selected: c.selected()?.path === entry.path, expanded: entry.kind === "directory" ? c.expanded().has(entry.path) : undefined }}
          onPress={(event) => { c.filterFocused.set(false); c.treeFocused.set(true); c.select(entry); if (event?.clickCount === 2 && entry.kind === "file") void c.edit(entry); }} style={{ height: 27, paddingLeft: 8 + depth * 14, paddingRight: 8, flexDirection: "row", alignItems: "center", gap: 5,
            backgroundColor: c.selected()?.path === entry.path ? c.theme().palette.panelActive : "transparent" }}>
          <Text style={{ ...styles(c).text, color: entry.kind === "directory" ? c.theme().palette.cyan : entry.kind === "symlink" ? c.theme().palette.magenta : c.theme().palette.muted }}>{entry.kind === "directory" ? c.expanded().has(entry.path) ? "▾" : "▸" : entry.kind === "symlink" ? "↗" : "·"}</Text>
          <Text numberOfLines={1} style={{ ...styles(c).text, flex: 1 }}>{entry.name}</Text>
          {entry.kind === "symlink" && <Text style={styles(c).muted}>LINK</Text>}
          {entry.gitStatus && <Text testID={`git:${entry.relativePath}`} accessibilityLabel={`Git ${entry.gitStatus}`} style={{ ...styles(c).muted, color: c.theme().palette.gold }}>{entry.gitStatus}</Text>}
        </View>)}
        {c.rows().length === 0 && <Text testID="files-empty" style={{ ...styles(c).muted, padding: 12 }}>{empty()}</Text>}
        {!c.query() && [...c.listings().values()].filter((listing) => listing.omitted > 0 && (listing.path === c.root() || c.expanded().has(listing.path))).map((listing) => <Text style={{ ...styles(c).muted, padding: 8 }}>{omittedLabel(listing.omitted, listing.scanLimited)}</Text>)}
      </View>
    </Scroll>
    <View style={{ padding: 8, gap: 5 }}>
      <Text testID="files-selection" numberOfLines={1} style={styles(c).muted}>{c.selected()?.relativePath ?? "Select a file or folder"}</Text>
      <View style={{ flexDirection: "row", gap: 6 }}>
        <Button testID={`files-open:${id}`} disabled={!c.selected()} onPress={() => void c.open()} style={styles(c).button}><Text style={styles(c).text}>Open</Text></Button>
        <Button testID={`files-edit:${id}`} disabled={c.selected()?.kind !== "file" || !c.selected()?.identity} onPress={() => void c.edit()} style={styles(c).button}><Text style={styles(c).text}>Edit in Vim</Text></Button>
      </View>
    </View>
  </View>;
}

export function PreviewPanel(props: { controller: FilesController }) {
  const c = props.controller, data = c.preview, entry = c.previewEntry;
  const editor = signal<EditorRef | null>(null);
  effect(() => {
    const node = editor(), atLine = c.line(), content = data();
    if (!node || !atLine || content?.kind !== "text") return;
    const lines = content.lines ?? content.text?.split("\n") ?? [];
    if (atLine > lines.length) { c.notice.set(`Line ${atLine} is past the end of ${content.truncated ? "this preview, which stops early" : "this file"}.`); return; }
    node.revealPosition({ line: Math.max(0, atLine - 1), column: 0 }, true);
  });
  return <View testID="file-preview-panel" onPress={() => c.focused.set("preview")} style={{ flex: 1, minHeight: 0, gap: 8, padding: 10 }}>
    <Text testID="preview-title" numberOfLines={1} style={styles(c).text}>{entry()?.relativePath ?? "Preview"}</Text>
    <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
      <Button testID="preview-open" disabled={!entry()} onPress={() => void c.open(entry())} style={styles(c).button}><Text style={styles(c).text}>Open</Text></Button>
      <Button testID="preview-reveal" disabled={!entry()} onPress={() => { if (entry()) c.revealInFinder(entry()!.path); }} style={styles(c).button}><Text style={styles(c).text}>Reveal</Text></Button>
      <Button testID="preview-edit" disabled={entry()?.kind !== "file" || !entry()?.identity} onPress={() => void c.edit(entry())} style={styles(c).button}><Text style={styles(c).text}>Edit in Vim</Text></Button>
      <Button testID="preview-copy-contents" disabled={data()?.kind !== "text"} onPress={() => void c.copyContents()} style={styles(c).button}><Text style={styles(c).text}>Copy contents</Text></Button>
      <Button testID="preview-zoom-out" onPress={() => c.zoom.set(Math.max(data()?.kind === "text" ? 0.5 : 0.05, c.zoom() / 1.2))} style={styles(c).button}><Text style={styles(c).text}>−</Text></Button>
      <Button testID="preview-zoom-fit" onPress={() => c.zoom.set(1)} style={styles(c).button}><Text style={styles(c).text}>{`Fit · ${Math.round(c.zoom() * 100)}%`}</Text></Button>
      <Button testID="preview-zoom-in" onPress={() => c.zoom.set(Math.min(5, c.zoom() * 1.2))} style={styles(c).button}><Text style={styles(c).text}>+</Text></Button>
      {data()?.markdown && <Button testID="preview-markdown-mode" onPress={() => c.renderedMarkdown.set(!c.renderedMarkdown())} style={styles(c).button}><Text style={styles(c).text}>{c.renderedMarkdown() ? "Source" : "Rendered"}</Text></Button>}
      {c.closePreview && <Button testID="preview-close" accessibilityLabel="Close Preview" onPress={() => { c.clearPreview(); c.closePreview?.(); }} style={styles(c).button}><Text style={styles(c).text}>Close</Text></Button>}
    </View>
    {c.notice() !== "" && <Text testID="preview-notice" role="alert" style={{ ...styles(c).text, color: c.theme().palette.gold }}>{c.notice()}</Text>}
    {c.previewLoading() && !data() && <Text style={styles(c).muted}>Loading…</Text>}
    {!data() && !c.previewLoading() && <Text testID="preview-empty" style={styles(c).muted}>Select a file to preview it.</Text>}
    {data()?.truncated && <Text testID="preview-truncated" style={{ ...styles(c).muted, color: c.theme().palette.gold }}>PREVIEW TRUNCATED AT 1 MiB OR 10,000 LINES</Text>}
    {data()?.kind === "message" && <View testID="preview-fallback" style={{ gap: 8 }}><Text selectable={true} style={styles(c).muted}>{data()?.message}</Text>
      {entry()?.name.toLowerCase().endsWith(".pdf") && <Button testID="preview-pdf-request" disabled={c.previewLoading()} onPress={() => { if (entry()) void c.loadPreview(entry()!, true); }} style={styles(c).button}><Text style={styles(c).text}>Preview PDF</Text></Button>}
    </View>}
    {data()?.kind === "text" && <View testID="preview-text" style={{ flex: 1, minHeight: 0 }}>
      <Text style={styles(c).muted}>{data()?.markdown ? c.renderedMarkdown() ? "MARKDOWN · READ ONLY" : "MARKDOWN SOURCE · READ ONLY" : "TEXT / CODE · READ ONLY"}</Text>
      {data()?.markdown && c.renderedMarkdown() ? <Scroll style={{ flex: 1, minHeight: 0 }}><TextView testID="preview-markdown" markdown={data()?.text ?? data()?.lines?.join("\n") ?? ""} selectable={true} links="report" renderImage={(image) => <Text style={styles(c).muted}>{`[Image: ${image.alt ?? "image"}]`}</Text>} styles={{ root: styles(c).text, code: { fontFamily: c.theme().monoFamily } }} /></Scroll> :
        <Editor testID="preview-code" ref={(node) => editor.set(node)} value={data()?.text ?? data()?.lines?.join("\n") ?? ""} language={entry()?.name} readOnly={true} lineNumbers={true} fontFamily={c.theme().monoFamily} fontSize={12 * c.theme().scale * c.zoom()}
          decorations={c.line() ? [{ range: { start: { line: Math.max(0, c.line()! - 1), column: 0 }, end: { line: Math.max(0, c.line()! - 1), column: 4000 } }, lineStyle: { backgroundColor: c.theme().palette.panelActive } }] : []} style={{ flex: 1, minHeight: 0 }} />}
    </View>}
    {(data()?.kind === "image" || data()?.kind === "pdf") && <View testID={`preview-${data()!.kind}`} style={{ flex: 1, minHeight: 0, gap: 8 }}>
      <Text style={styles(c).muted}>{`${data()?.mime ?? (data()?.kind === "pdf" ? "PDF" : "Image")} · ${data()?.width ?? 0} × ${data()?.height ?? 0} pixels`}</Text>
      {data()?.dataUrl && <Scroll minimumZoomScale={0.05} maximumZoomScale={5} zoomScale={c.zoom()} style={{ flex: 1, minHeight: 0 }} contentStyle={{ flexGrow: 1 }}><Image testID="preview-bitmap" src={data()!.dataUrl!} resizeMode="contain" style={{ flex: 1, minHeight: 100 }} /></Scroll>}
      {data()?.kind === "pdf" && <View style={{ flexDirection: "row", gap: 10, alignItems: "center" }}>
        <Button testID="preview-pdf-prev" disabled={c.previewLoading() || (data()?.page ?? 1) <= 1} onPress={() => { if (entry()) void c.loadPreview(entry()!, true, (data()?.page ?? 1) - 1); }}><Text style={styles(c).text}>‹ Prev</Text></Button>
        <Text testID="preview-pdf-page" style={styles(c).muted}>{`Page ${data()?.page ?? 1} of ${data()?.pages ?? 1}`}</Text>
        <Button testID="preview-pdf-next" disabled={c.previewLoading() || (data()?.page ?? 1) >= (data()?.pages ?? 1)} onPress={() => { if (entry()) void c.loadPreview(entry()!, true, (data()?.page ?? 1) + 1); }}><Text style={styles(c).text}>Next ›</Text></Button>
      </View>}
    </View>}
  </View>;
}

/** A serialized autosave queue survives component cleanup without dropping its final edit. */
export function MemoEditor(props: {
  projectId: () => string; theme?: () => InterfaceTheme; visible?: () => boolean;
  /** Receipt delivery seam; the default uses the durable native writer. */
  saveMemo?: typeof Store.setProjectMemo;
  /** A containing form can own Cmd+S and save its metadata and memo together. */
  bindSaveKey?: boolean;
  onControl?: (control: { save: () => void; focus: () => void } | null) => void;
}) {
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const value = signal(""), status = signal("Saved automatically"), saving = signal(false);
  type Draft = { text: string; saved: string; dirty: boolean; running: boolean; error: string; revision?: number };
  const drafts = new Map<string, Draft>();
  let current = "", alive = true, timer: ReturnType<typeof setTimeout> | undefined;
  let input: YNode | undefined;
  const draft = (id: string) => { if (!drafts.has(id)) drafts.set(id, { text: "", saved: "", dirty: false, running: false, error: "" }); return drafts.get(id)!; };
  function renderStatus() {
    if (!alive) return;
    const d = draft(current), bytes = memoBytes(d.text);
    saving.set(d.running);
    status.set(bytes > 32768 ? `The memo is ${bytes} bytes; a memo holds at most 32768 bytes (32 KB). Shorten it to save.` : d.error ? `Not saved: ${d.error}. Your text is still here.` : d.running || d.dirty ? "Saving…" : "Saved automatically");
  }
  async function save(id = current) {
    clearTimeout(timer);
    if (!id) return;
    const d = draft(id);
    if (d.running || !d.dirty || memoBytes(d.text) > 32768) { renderStatus(); return; }
    d.running = true; d.error = ""; renderStatus();
    try {
      while (d.dirty && memoBytes(d.text) <= 32768) {
        const text = d.text, revision = d.revision;
        let result;
        try { result = await (props.saveMemo ?? Store.setProjectMemo)(id, text, revision); }
        catch (reason) { if (d.revision !== revision) continue; throw reason; }
        d.saved = result.text;
        // Normalization is authoritative only if the user did not type during this save.
        if (d.text === text) { d.text = result.text; if (alive && current === id) value.set(result.text); }
        // An intervening edit may equal the old saved text but differ from this receipt.
        d.dirty = d.text !== d.saved;
      }
    } catch (reason) { d.error = String(reason); }
    finally { d.running = false; renderStatus(); }
  }
  effect(() => {
    const id = props.projectId();
    if (id === current) return;
    if (current) void save(current);
    current = id; value.set(draft(id).text); renderStatus();
  });
  const subscription = Store.subscribe((event) => {
    for (const [id, memo] of Object.entries(event.data.projectMemos)) {
      const d = draft(id);
      if (!d.dirty && !d.running) { d.text = memo.text; d.saved = memo.text; if (id === current && alive) value.set(memo.text); }
    }
    renderStatus();
  });
  const control = { save: () => void save(), focus: () => input?.focus() };
  cleanup(() => { alive = false; clearTimeout(timer); subscription.close(); props.onControl?.(null); for (const id of drafts.keys()) void save(id); });
  effect(() => { if (props.bindSaveKey === false || props.visible && !props.visible()) return; return keymap.bind("mod+s", () => void save()); });
  const textStyle = () => ({ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.text });
  return <View testID="project-memo-editor" style={{ flex: 1, minHeight: 0, padding: 12, gap: 8 }}>
    <Text style={textStyle()}>Project memo</Text>
    <Input testID="project-memo-input" ref={(node) => { input = node; props.onControl?.(control); }} accessibilityLabel="Project memo" multiline={true} value={value()} placeholder="Write notes for this project in Markdown: plans, links, decisions. Only you see them. No agent is given the memo; type /memo in a chat to insert it yourself."
      onChange={(text) => {
        const d = draft(current); d.text = text; d.dirty = d.running || d.text !== d.saved; d.error = ""; value.set(text); clearTimeout(timer);
        if (memoBytes(text) <= 32768) {
          try { d.revision = Store.stageProjectMemo(current, text); timer = setTimeout(() => void save(), 600); }
          catch (reason) { d.error = String(reason); }
        }
        renderStatus();
      }} onBlur={() => void save()}
      style={{ flex: 1, minHeight: 160, maxHeight: 560, fontFamily: theme().monoFamily, fontSize: 12 * theme().scale, color: theme().palette.text, backgroundColor: theme().palette.bg, padding: 8 }} />
    <Text testID="project-memo-status" style={{ ...textStyle(), color: status().startsWith("Not saved") || memoBytes(value()) > 32768 ? theme().palette.gold : theme().palette.muted }}>{status()}</Text>
    {memoBytes(value()) >= 26214 && <Text testID="project-memo-bytes" style={textStyle()}>{`${memoBytes(value())} / 32768 bytes`}</Text>}
    <Button testID="project-memo-save" disabled={saving() || memoBytes(value()) > 32768} onPress={() => void save()}><Text style={textStyle()}>{status().startsWith("Not saved") ? "Retry" : "Save now"}</Text></Button>
  </View>;
}
