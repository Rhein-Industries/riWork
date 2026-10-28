import { Clipboard, Image, List, Scroll, Text, View, cleanup, effect, host, keymap, signal, untracked, type YNode } from "nsh";
import { Button, Editor, Input, TextView, type EditorRef } from "nsh/base";
import { Menu } from "nsh/desktop";
import { Files, Store, type Subscription } from "riwork/native";
import { DEFAULT_THEME, space, type InterfaceTheme } from "./appearance";
import { Icon, type IconName } from "./Icon";
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
  const filterFocused = signal(false), filterComposing = signal(false), focused = signal<"files" | "preview" | null>(null), zoom = signal(1);
  const searches = new Map<string, YNode>();
  const treeLists = new Map<string, YNode>();
  const treeFocus = new Map<string, YNode>();
  const theme = options.theme ?? (() => DEFAULT_THEME);
  const watches = new Map<string, Subscription>(), revisions = new Map<string, number>();
  let currentRoot: string | null = null, generation = 0, watchGeneration = 0, previewGeneration = 0, alive = true;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const rows = () => fileRows(currentRoot, listings(), expanded(), query(), hidden());
  const setNotice = (message: unknown) => { if (alive) notice.set(String(message)); };
  const visible = () => options.visible?.() ?? true;
  function closeWatches() { watchGeneration++; for (const watcher of watches.values()) watcher.close(); watches.clear(); revisions.clear(); }
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
    const token = generation, watchToken = watchGeneration;
    try {
      const subscription = Files.subscribe(currentRoot, path, hidden(), (event) => {
        if (token !== generation || watchToken !== watchGeneration || event.rev < (revisions.get(path) ?? 0)) return;
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
    // The native subscription supplies the initial listing. A workspace's
    // selected worktree alone must not read a hidden Files/Preview panel.
    if (currentRoot && visible()) watch(currentRoot);
  }
  effect(() => { const next = options.root(); if (next !== currentRoot) resetRoot(); });
  // Visibility changes only arm subscriptions. There is no window or per-frame polling.
  effect(() => {
    if (!visible()) { closeWatches(); clearTimeout(timer); previewGeneration++; previewLoading.set(false); return; }
    if (currentRoot) watch(currentRoot);
    for (const path of expanded()) watch(path);
  });
  cleanup(() => { alive = false; generation++; previewGeneration++; clearTimeout(timer); closeWatches(); searches.clear(); treeLists.clear(); treeFocus.clear(); });
  // A filtered-out selection must not survive as an invisible keyboard target.
  effect(() => {
    const current = rows(), entry = untracked(selected);
    if (entry && !current.some((row) => row.entry.path === entry.path)) selected.set(current[0]?.entry ?? null);
  });

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
    clearTimeout(timer); previewGeneration++; previewLoading.set(false);
    selected.set(entry); notice.set(""); line.set(undefined); zoom.set(1);
    if (entry.kind === "directory") { toggle(entry); return; }
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
    else { next.add(entry.path); watch(entry.path); }
    expanded.set(next);
  }
  function refresh() {
    if (!currentRoot) return;
    // A failed native watcher can recover on a user refresh.
    closeWatches(); watch(currentRoot); for (const path of expanded()) watch(path);
    if (previewEntry()) void loadPreview(previewEntry()!, preview()?.kind === "pdf", preview()?.page);
  }
  function clearPreview() { previewGeneration++; clearTimeout(timer); preview.set(null); previewEntry.set(null); line.set(undefined); zoom.set(1); previewLoading.set(false); }
  function toggleHidden() {
    hidden.set(!hidden()); generation++; closeWatches();
    if (currentRoot) watch(currentRoot);
    for (const path of expanded()) watch(path);
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
  async function copyPath(entry = selected()) {
    if (!entry) return;
    try { await Clipboard.writeText(entry.relativePath); if (alive) notice.set(`Copied ${entry.relativePath}`); }
    catch (reason) { setNotice(reason); }
  }
  async function openPreviewLink(url: string) {
    if (/^https?:\/\//i.test(url) || /^mailto:/i.test(url)) {
      try { await host.call("system.openWith", { path: url }); } catch (reason) { setNotice(reason); }
      return;
    }
    if (url.startsWith("#")) return;
    const current = previewEntry(), root = currentRoot;
    if (!current || !root || /^[a-z][a-z0-9+.-]*:/i.test(url)) return;
    try {
      const [href, fragment] = url.split("#", 2);
      const parts = (href.startsWith("/") ? decodeURIComponent(href) : `${current.path.slice(0, current.path.lastIndexOf("/"))}/${decodeURIComponent(href)}`).split("/");
      const resolved: string[] = [];
      for (const part of parts) { if (part === "..") resolved.pop(); else if (part && part !== ".") resolved.push(part); }
      const lineMatch = /^L?(\d+)/.exec(fragment ?? "");
      await reveal(`/${resolved.join("/")}`, lineMatch ? Number(lineMatch[1]) : undefined);
    } catch (reason) { setNotice(reason); }
  }
  async function copyContents() {
    const entry = previewEntry(), root = currentRoot;
    if (!entry?.identity || !root) return;
    const token = generation;
    try { const text = await Files.readText(root, entry.path, entry.identity); if (alive && token === generation) { await Clipboard.writeText(text); notice.set(`Copied ${text.split("\n").length} lines`); } }
    catch (reason) { setNotice(reason); }
  }
  function highlight(entry: FileEntry) {
    clearTimeout(timer); previewGeneration++; previewLoading.set(false);
    selected.set(entry); zoom.set(1); line.set(undefined);
    const index = rows().findIndex((row) => row.entry.path === entry.path);
    if (index >= 0) for (const list of treeLists.values()) list.scrollToIndex({ index, animated: false, viewPosition: 0.5 });
    if (entry.kind === "file") { options.previewRequested(); timer = setTimeout(() => void loadPreview(entry, false), 150); }
  }
  function move(direction: number) {
    const current = rows(), index = current.findIndex((row) => row.entry.path === selected()?.path);
    const row = current[Math.max(0, Math.min(current.length - 1, index + direction))];
    if (row) highlight(row.entry);
  }
  function focusTree(id?: string) {
    const node = id ? treeFocus.get(id) : treeFocus.values().next().value;
    node?.focus(); filterFocused.set(false); treeFocused.set(true); focused.set("files");
  }
  const active = () => visible() && (options.active ? options.active() : focused());
  function navigate(key: string) {
    if (active() !== "files" || filterFocused()) return;
    treeFocused.set(true);
    if (key === "up" || key === "down") { move(key === "up" ? -1 : 1); return; }
    const currentRows = rows(), entry = selected();
    if (key === "home" || key === "end") { const row = currentRows[key === "home" ? 0 : currentRows.length - 1]; if (row) highlight(row.entry); return; }
    if (!entry) return;
    if (key === "right") {
      if (entry.kind === "directory" && !expanded().has(entry.path)) toggle(entry);
      else { const index = currentRows.findIndex((row) => row.entry.path === entry.path); const current = currentRows[index], child = currentRows[index + 1]; if (current && child && child.depth > current.depth) highlight(child.entry); }
    } else if (key === "left") {
      if (entry.kind === "directory" && expanded().has(entry.path)) toggle(entry);
      else { const parent = entry.path.slice(0, entry.path.lastIndexOf("/")); const row = currentRows.find((row) => row.entry.path === parent); if (row) highlight(row.entry); }
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
        bindings.push(keymap.bind("mod+c", () => { void copyPath(); }));
      }
    }
    if (view === "preview") {
      bindings.push(keymap.bind("escape", () => { clearPreview(); options.closePreview?.(); }));
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
  effect(() => { if (active() !== "files" || !query() || filterComposing()) return; return keymap.bind("escape", () => { query.set(""); focusTree(); }); });
  return { ...options, theme, listings, expanded, query, hidden, selected, preview, previewEntry, loading, previewLoading, notice, renderedMarkdown, line, treeFocused,
    rows, resetRoot, refresh, toggleHidden, select, toggle, loadPreview, edit, open, reveal, copyContents, copyPath, openPreviewLink, move, filterFocused, filterComposing, focused, searches, treeLists, treeFocus, focusTree, zoom, clearPreview,
    revealInFinder: (path: string) => { void Promise.resolve(options.reveal?.(path)).catch(setNotice); } };
}
export type FilesController = ReturnType<typeof createFilesController>;

function styles(c: FilesController) {
  const t = c.theme();
  return { text: { fontFamily: t.family, fontSize: 11 * t.scale, color: t.palette.text }, muted: { fontFamily: t.family, fontSize: 10 * t.scale, color: t.palette.muted },
    button: { paddingHorizontal: space(10, t.scale), paddingVertical: space(4, t.scale), borderRadius: space(14, t.scale), backgroundColor: t.palette.panelActive } };
}
function FileAction(props: { controller: FilesController; id: string; label: string; icon: IconName; disabled?: boolean; selected?: boolean; onPress: () => void }) {
  const c = props.controller;
  return <Button tooltip={<View style={{ padding: 7, borderRadius: 6, backgroundColor: c.theme().palette.panelActive }}><Text style={styles(c).text}>{props.label}</Text></View>} testID={props.id} accessibilityLabel={props.label} disabled={props.disabled} selected={props.selected} onPress={props.onPress}
    style={{ width: space(26, c.theme().scale), height: space(26, c.theme().scale), borderRadius: space(13, c.theme().scale), alignItems: "center", justifyContent: "center", opacity: props.disabled ? 0.4 : 1 }}
    hoverStyle={{ backgroundColor: c.theme().palette.panelActive }} focusStyle={{ backgroundColor: c.theme().palette.divider }} selectedStyle={{ backgroundColor: c.theme().palette.panelActive }}>
    <Icon name={props.icon} size={14 * c.theme().scale} color={props.selected ? c.theme().palette.text : c.theme().palette.muted} />
  </Button>;
}
export function FilesPanel(props: { controller: FilesController; surface?: string }) {
  const c = props.controller, id = props.surface ?? "files";
  let search: YNode | undefined;
  cleanup(() => { c.searches.delete(id); c.treeLists.delete(id); c.treeFocus.delete(id); });
  const empty = () => !c.root() ? "Select a worktree to browse its files." : c.loading() ? "Loading worktree…" : c.listings().get(c.root()!)?.error || (c.query() ? "No loaded files match. Clear the filter and expand folders to load more." : !c.hidden() ? "No visible files. Turn on Hidden to show dotfiles." : "This folder is empty.");
  function contextMenu(entry: FileEntry) {
    c.selected.set(entry);
    void Menu.popup([
      { label: "Preview", disabled: entry.kind !== "file", onSelect: () => c.select(entry) },
      { label: "Edit in Vim", disabled: entry.kind !== "file" || !entry.identity, onSelect: () => void c.edit(entry) },
      { label: "Open externally", disabled: entry.kind === "symlink", onSelect: () => void c.open(entry) },
      { type: "separator" },
      { label: "Copy path", onSelect: () => void c.copyPath(entry) },
      { label: "Reveal in Finder", onSelect: () => c.revealInFinder(entry.path) },
    ]);
  }
  return <View testID="files-panel" ref={(node) => c.treeFocus.set(id, node)} focusable={true} accessibilityLabel="Files"
    onFocus={() => { c.focused.set("files"); c.treeFocused.set(true); }} onBlur={() => c.treeFocused.set(false)}
    onPress={() => c.focused.set("files")} style={{ flex: 1, minHeight: 0, backgroundColor: c.theme().palette.panel }}>
    <View style={{ height: space(46, c.theme().scale), paddingLeft: space(12, c.theme().scale), paddingRight: space(8, c.theme().scale), flexDirection: "row", alignItems: "center", gap: 6 }}>
      <View style={{ flex: 1, minWidth: 0, gap: 1 }}>
        <Text style={{ ...styles(c).text, fontSize: 13 * c.theme().scale, fontWeight: "600" }}>Files</Text>
        <Text numberOfLines={1} style={styles(c).muted}>{c.root() ? `${c.label()} · ${c.rows().length} ${c.rows().length === 1 ? "item" : "items"}` : "No worktree"}</Text>
      </View>
      <View style={{ flexDirection: "row", gap: 2 }}>
        <FileAction controller={c} id={`files-refresh:${id}`} label="Refresh Files" icon="refresh" onPress={c.refresh} />
        <FileAction controller={c} id={`files-hidden:${id}`} label="Show hidden files" icon="eye" selected={c.hidden()} onPress={c.toggleHidden} />
        <FileAction controller={c} id={`files-reveal-root:${id}`} label="Reveal worktree in Finder" icon="folder" disabled={!c.root()} onPress={() => { if (c.root()) c.revealInFinder(c.root()!); }} />
      </View>
    </View>
    <View style={{ paddingHorizontal: space(12, c.theme().scale), paddingBottom: space(6, c.theme().scale) }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: 5, height: space(28, c.theme().scale), paddingHorizontal: 9, borderRadius: space(14, c.theme().scale), backgroundColor: c.theme().palette.panelActive }}>
        <Icon name="search" size={13 * c.theme().scale} color={c.theme().palette.muted} />
        <Input testID={`files-filter:${id}`} ref={(node) => { search = node; c.searches.set(id, node); }} accessibilityLabel="Filter loaded files" placeholder="Filter loaded files…" value={c.query()} onChange={(value) => c.query.set(value)} onFocus={() => { c.focused.set("files"); c.filterFocused.set(true); }} onBlur={() => c.filterFocused.set(false)} onCompositionChange={(event) => c.filterComposing.set(event.composing)} onSubmit={() => { if (!c.filterComposing()) c.focusTree(id); }} style={{ ...styles(c).text, padding: 0, flex: 1, minWidth: 0, backgroundColor: "transparent" }} />
        {c.query() !== "" && <FileAction controller={c} id={`files-clear:${id}`} label="Clear filter" icon="close" onPress={() => { c.query.set(""); search?.focus(); }} />}
      </View>
    </View>
    {c.notice() !== "" && <Text testID="files-notice" role="alert" style={{ ...styles(c).text, color: c.theme().palette.gold, paddingHorizontal: 12, paddingBottom: 6 }}>{c.notice()}</Text>}
    <List testID={`files-tree:${id}`} ref={(node) => c.treeLists.set(id, node)} each={c.rows()} keyExtractor={(item) => item.entry.path} estimatedItemSize={space(26, c.theme().scale)} style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, c.theme().scale), paddingBottom: 6 }}
      empty={<View style={{ minHeight: space(120, c.theme().scale), padding: 24, gap: 8, alignItems: "center", justifyContent: "center" }}><Icon name="folder" size={22 * c.theme().scale} color={c.theme().palette.muted} /><Text testID="files-empty" style={{ ...styles(c).muted, textAlign: "center" }}>{empty()}</Text></View>}>
      {(item) => <FileRow item={item} />}
    </List>
    {!c.query() && [...c.listings().values()].filter((listing) => listing.omitted > 0 && (listing.path === c.root() || c.expanded().has(listing.path))).map((listing) => <Text style={{ ...styles(c).muted, padding: 8 }}>{omittedLabel(listing.omitted, listing.scanLimited)}</Text>)}
    <Text testID="files-selection" numberOfLines={1} style={{ ...styles(c).muted, fontSize: 9 * c.theme().scale, fontFamily: c.selected() ? c.theme().monoFamily : c.theme().family, paddingHorizontal: space(12, c.theme().scale), paddingVertical: 8 }}>{c.selected()?.relativePath ?? "Select a file or folder"}</Text>
  </View>;

  function FileRow(props: { item: () => { entry: FileEntry; depth: number } }) {
    const entry = () => props.item().entry;
    return <View testID={`file:${entry().relativePath}`} role="row" accessibilityLabel={entry().relativePath}
      accessibilityState={{ selected: c.selected()?.path === entry().path, expanded: entry().kind === "directory" ? c.expanded().has(entry().path) : undefined }}
      onContextMenu={() => contextMenu(entry())} onPress={(event) => { c.focusTree(id); c.select(entry()); if (event?.clickCount === 2 && entry().kind === "file") void c.edit(entry()); }}
      style={{ height: space(26, c.theme().scale), paddingLeft: space(6 + props.item().depth * 16, c.theme().scale), paddingRight: 6, flexDirection: "row", alignItems: "center", gap: 5, borderRadius: space(7, c.theme().scale), backgroundColor: c.selected()?.path === entry().path ? c.theme().palette.panelActive : "transparent" }}
      hoverStyle={{ backgroundColor: c.theme().palette.panelActive }} focusStyle={{ backgroundColor: c.theme().palette.divider }}>
      <Icon name={entry().kind === "directory" ? c.expanded().has(entry().path) ? "down" : "right" : entry().kind === "symlink" ? "open" : "file"} size={12 * c.theme().scale} color={c.theme().palette.muted} />
      <Text numberOfLines={1} style={{ ...styles(c).text, fontFamily: c.theme().monoFamily, flex: 1 }}>{entry().name}</Text>
      {entry().kind === "symlink" && <Text style={styles(c).muted}>Link</Text>}
      {entry().gitStatus && <Text testID={`git:${entry().relativePath}`} accessibilityLabel={`Git ${entry().gitStatus}`} style={{ ...styles(c).muted, color: c.theme().palette.gold }}>{entry().gitStatus}</Text>}
    </View>;
  }
}

export function PreviewPanel(props: { controller: FilesController }) {
  const c = props.controller, data = c.preview, entry = c.previewEntry;
  const editor = signal<EditorRef | null>(null);
  const compact = signal(false);
  effect(() => {
    const node = editor(), atLine = c.line(), content = data();
    if (!node || !atLine || content?.kind !== "text") return;
    const lines = content.lines ?? content.text?.split("\n") ?? [];
    if (atLine > lines.length) { c.notice.set(`Line ${atLine} is past the end of ${content.truncated ? "this preview, which stops early" : "this file"}.`); return; }
    node.revealPosition({ line: Math.max(0, atLine - 1), column: 0 }, true);
  });
  return <View testID="file-preview-panel" onLayout={(bounds) => compact.set(bounds.size.width < space(360, c.theme().scale))}
    onPress={() => c.focused.set("preview")} style={{ flex: 1, minHeight: 0, backgroundColor: c.theme().palette.bg }}>
    <View testID="preview-toolbar" style={{ height: space(38, c.theme().scale), paddingHorizontal: space(8, c.theme().scale), flexDirection: "row", alignItems: "center", gap: 6, backgroundColor: c.theme().palette.panel }}>
      <Icon name="file" size={14 * c.theme().scale} color={c.theme().palette.muted} />
      <Text testID="preview-title" numberOfLines={1} style={{ ...styles(c).text, flex: 1, minWidth: 0, fontWeight: "600" }}>{entry()?.relativePath ?? "Preview"}</Text>
      <FileAction controller={c} id="preview-edit" label="Edit in Vim" icon="terminal" disabled={entry()?.kind !== "file" || !entry()?.identity} onPress={() => void c.edit(entry())} />
      {!compact() && <>
        <FileAction controller={c} id="preview-copy-path" label="Copy path" icon="copy" disabled={!entry()} onPress={() => void c.copyPath(entry())} />
        <FileAction controller={c} id="preview-reveal" label="Reveal in Finder" icon="folder" disabled={!entry()} onPress={() => { if (entry()) c.revealInFinder(entry()!.path); }} />
        <FileAction controller={c} id="preview-open" label="Open externally" icon="open" disabled={!entry() || entry()?.kind === "symlink"} onPress={() => void c.open(entry())} />
      </>}
      <FileAction controller={c} id="preview-more" label="Preview actions" icon="more" disabled={!entry()} onPress={() => void Menu.popup([
        { label: "Edit in Vim", shortcut: "mod+e", disabled: entry()?.kind !== "file" || !entry()?.identity, onSelect: () => void c.edit(entry()) },
        { label: "Open externally", shortcut: "mod+o", disabled: !entry() || entry()?.kind === "symlink", onSelect: () => void c.open(entry()) },
        { label: "Reveal in Finder", disabled: !entry(), onSelect: () => { if (entry()) c.revealInFinder(entry()!.path); } },
        { label: "Copy path", disabled: !entry(), onSelect: () => void c.copyPath(entry()) },
        { type: "separator" },
        { label: "Copy contents", disabled: data()?.kind !== "text", onSelect: () => void c.copyContents() },
        { label: c.renderedMarkdown() ? "Show Markdown source" : "Render Markdown", disabled: !data()?.markdown, onSelect: () => c.renderedMarkdown.set(!c.renderedMarkdown()) },
        { type: "separator" },
        { label: "Zoom in", onSelect: () => c.zoom.set(Math.min(5, c.zoom() * 1.2)) },
        { label: "Zoom out", onSelect: () => c.zoom.set(Math.max(data()?.kind === "text" ? 0.5 : 0.05, c.zoom() / 1.2)) },
        { label: "Actual size", onSelect: () => c.zoom.set(1) },
      ])} />
      {c.closePreview && <FileAction controller={c} id="preview-close" label="Close Preview" icon="close" onPress={() => { c.clearPreview(); c.closePreview?.(); }} />}
    </View>
    <View style={{ flexDirection: "row", alignItems: "center", gap: 6, paddingHorizontal: space(12, c.theme().scale), paddingVertical: 4, backgroundColor: c.theme().palette.panel, borderBottomWidth: 1, borderBottomColor: c.theme().palette.divider }}>
      <Text testID="preview-metadata" numberOfLines={1} style={{ ...styles(c).muted, flex: 1 }}>{data()?.kind === "text" ? data()?.markdown ? c.renderedMarkdown() ? "Markdown · Read only" : "Markdown source · Read only" : "Text / Code · Read only" : data()?.kind === "image" || data()?.kind === "pdf" ? `${data()?.mime ?? (data()?.kind === "pdf" ? "PDF" : "Image")} · ${data()?.width ?? 0} × ${data()?.height ?? 0} pixels` : "Read only"}</Text>
      {!compact() && data()?.kind === "text" && <Button testID="preview-copy-contents" onPress={() => void c.copyContents()} style={styles(c).button}><Text style={styles(c).muted}>Copy contents</Text></Button>}
      {!compact() && data()?.markdown && <Button testID="preview-markdown-mode" onPress={() => c.renderedMarkdown.set(!c.renderedMarkdown())} style={styles(c).button}><Text style={styles(c).muted}>{c.renderedMarkdown() ? "Source" : "Rendered"}</Text></Button>}
      <Button testID="preview-zoom-out" accessibilityLabel="Zoom out" disabled={!data()} onPress={() => c.zoom.set(Math.max(data()?.kind === "text" ? 0.5 : 0.05, c.zoom() / 1.2))} style={{ paddingHorizontal: 5 }}><Text style={styles(c).text}>−</Text></Button>
      <Button testID="preview-zoom-fit" accessibilityLabel="Actual size" disabled={!data()} onPress={() => c.zoom.set(1)}><Text style={styles(c).muted}>{`${Math.round(c.zoom() * 100)}%`}</Text></Button>
      <Button testID="preview-zoom-in" accessibilityLabel="Zoom in" disabled={!data()} onPress={() => c.zoom.set(Math.min(5, c.zoom() * 1.2))} style={{ paddingHorizontal: 5 }}><Text style={styles(c).text}>+</Text></Button>
    </View>
    {c.notice() !== "" && <Text testID="preview-notice" role="alert" style={{ ...styles(c).text, color: c.theme().palette.gold }}>{c.notice()}</Text>}
    {c.previewLoading() && !data() && <Text style={styles(c).muted}>Loading…</Text>}
    {!data() && !c.previewLoading() && <Text testID="preview-empty" style={{ ...styles(c).muted, padding: 24 }}>Select a file to preview it.</Text>}
    {data()?.truncated && <Text testID="preview-truncated" style={{ ...styles(c).muted, color: c.theme().palette.gold }}>Preview truncated at 1 MiB or 10,000 lines</Text>}
    {data()?.kind === "message" && <View testID="preview-fallback" style={{ gap: 8, padding: 12 }}><Text selectable={true} style={styles(c).muted}>{data()?.message}</Text>
      {entry()?.name.toLowerCase().endsWith(".pdf") && <Button testID="preview-pdf-request" disabled={c.previewLoading()} onPress={() => { if (entry()) void c.loadPreview(entry()!, true); }} style={styles(c).button}><Text style={styles(c).text}>Preview PDF</Text></Button>}
    </View>}
    {data()?.kind === "text" && <View testID="preview-text" style={{ flex: 1, minHeight: 0 }}>
      {data()?.markdown && c.renderedMarkdown() ? <Scroll style={{ flex: 1, minHeight: 0 }} contentStyle={{ padding: space(16, c.theme().scale) }}><TextView testID="preview-markdown" markdown={data()?.text ?? data()?.lines?.join("\n") ?? ""} selectable={true} links="report" onLinkPress={(event) => void c.openPreviewLink(event.url)} renderImage={(image) => <Text style={styles(c).muted}>{`[Image: ${image.alt ?? "image"}]`}</Text>} styles={{ root: { ...styles(c).text, fontSize: 12 * c.theme().scale * c.zoom() }, code: { fontFamily: c.theme().monoFamily } }} /></Scroll> :
        <Editor testID="preview-code" ref={(node) => editor.set(node)} value={data()?.text ?? data()?.lines?.join("\n") ?? ""} language={entry()?.name} readOnly={true} lineNumbers={true} fontFamily={c.theme().monoFamily} fontSize={12 * c.theme().scale * c.zoom()}
          decorations={c.line() ? [{ range: { start: { line: Math.max(0, c.line()! - 1), column: 0 }, end: { line: Math.max(0, c.line()! - 1), column: 4000 } }, lineStyle: { backgroundColor: c.theme().palette.panelActive } }] : []} style={{ flex: 1, minHeight: 0 }} />}
    </View>}
    {(data()?.kind === "image" || data()?.kind === "pdf") && <View testID={`preview-${data()!.kind}`} style={{ flex: 1, minHeight: 0, gap: 8 }}>
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
