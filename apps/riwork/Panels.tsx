import { List, Scroll, Text, View, cleanup, context, effect, host, keymap, signal, type JSX, type YNode } from "nsh";
import { Button, Input, Switch } from "nsh/base";
import { Menu } from "nsh/desktop";
import { Chat, Sessions, Settings, Store, Tabs, type ChatSummary, type ProjectDto, type ProjectInspectionDto, type SettingsDto, type ShellSessionDto, type StoreState, type Subscription, type TabEntryDto, type TabsSnapshot, type TaskDto } from "riwork/native";
import { EMPTY_STORE, countLabel, matches, projectRows, shellLabel, shellMatches, taskAge, taskCounts, taskDate, taskMatches, taskRows, taskStatusLabel, taskWorktree, visibleWorktrees, worktreeLabel, type ListPanel, type ProjectOrder, type ProjectTreeRow, type TaskFilter, type TaskRow } from "./panel_model";
import { DEFAULT_THEME, space, type InterfaceTheme } from "./appearance";
import { Icon, type IconName } from "./Icon";
import { PANEL } from "./panel_layout";

const defaultTheme = () => DEFAULT_THEME;
const PanelTheme = context<() => InterfaceTheme>(defaultTheme);
const theme = () => PanelTheme()();
const ink = () => theme().palette.text, muted = () => theme().palette.muted, accent = () => theme().palette.cyan;
const text = (base: number) => base * theme().scale;
const button = () => ({ paddingHorizontal: space(10, theme().scale), paddingVertical: space(4, theme().scale), borderRadius: space(14, theme().scale), backgroundColor: theme().palette.panelActive });
// A form's input sits on InlineEditor's `bg` card: the panel's fill and an edge set it apart, and
// the edge takes the focus color while it is focused (`fieldFocus`, the Input's `focusStyle`).
const field = () => ({ minHeight: space(28, theme().scale), paddingHorizontal: space(8, theme().scale), paddingVertical: space(5, theme().scale), borderRadius: 4, borderWidth: 1, borderColor: theme().palette.divider, backgroundColor: theme().palette.panel, color: ink(), fontFamily: theme().family, fontSize: text(12) });
const fieldFocus = () => ({ borderColor: theme().palette.focus });
const row = () => ({ paddingHorizontal: space(6, theme().scale), paddingVertical: space(6, theme().scale), gap: space(2, theme().scale), borderRadius: space(7, theme().scale) });
const statusColor = (status: string) => status === "done" ? accent() : status === "in_progress" ? theme().palette.working : muted();
const statusIcon = (status: string): IconName => status === "done" ? "task-done" : status === "in_progress" ? "task-active" : "task-todo";
const TASK_STATUSES = ["todo", "in_progress", "done"] as const;
type Editor = { kind: "folder"; parent?: string; id?: string; name: string } | { kind: "project" } | { kind: "task" };

interface PanelOptions {
  projectId: () => string;
  projectLabel: () => string;
  switchProject: (project: ProjectDto) => Promise<void>;
  openPanel: (panel: string) => void;
  showShell: (id: string) => void;
  showChat?: (id: string) => void;
  newChat?: () => void;
  /** In-memory Exit/list seam for isolated confirmation tests. */
  sessionActions?: Pick<typeof Sessions, "list" | "close">;
  selectWorktree: (id: string | null) => void;
  selectTask: (id: string | null) => void;
  initialWorktree?: string | null;
  initialTask?: string | null;
  /** Fixture-only destination; production resolves it through the native Paths adapter. */
  defaultProjectDirectory?: string;
  notice: (message: string) => void;
  theme?: () => InterfaceTheme;
  settings?: () => SettingsDto | null;
}

/** One controller per window. Hidden panel tabs never own independent queries or subscriptions. */
export function createPanelController(options: PanelOptions) {
  const state = signal<StoreState>(EMPTY_STORE);
  // True after the first Store delivery; the Dock label waits for stored names.
  const loaded = signal(false);
  const shells = signal<ShellSessionDto[]>([]);
  const tabs = signal<TabsSnapshot | null>(null);
  const chats = signal<ChatSummary[]>([]);
  const catalogLoading = signal(false), catalogNotice = signal("");
  const sessionFilter = signal<"all" | "codex" | "claude" | "muse" | "shells">("all");
  const selectedChat = signal<string | null>(null);
  const closingShell = signal<ShellSessionDto | null>(null);
  const query = signal("");
  const collapsed = signal<ReadonlySet<string>>(new Set());
  const selectedWorktree = signal<string | null>(options.initialWorktree ?? null);
  const selectedTask = signal<string | null>(options.initialTask ?? null);
  const taskFilter = signal<TaskFilter>("open");
  // Unix seconds for relative task times. Store pushes and window activation
  // advance it; the panel data contract is push-only, so no timer does.
  const now = signal(Math.floor(Date.now() / 1000));
  const selectedShell = signal<string | null>(null);
  const editor = signal<Editor | null>(null);
  const pending = signal(false);
  const order = signal<ProjectOrder>("edited");
  const descending = signal(true);
  const sortPending = signal(false);
  const projectPanel = signal<{ view: "list" | "columns" | "grid"; pinned: string[] }>({ view: "list", pinned: [] });
  const panelSaving = signal(false);
  let savingPanel = false, panelRequest = 0;
  let deferredPanel: SettingsDto["projectPanel"];
  function applyPanel(value: SettingsDto["projectPanel"]) {
    if (!value) return;
    if (savingPanel) { deferredPanel = value; return; }
    projectPanel.set({ view: value.view === "grid" || value.view === "columns" ? value.view : "list", pinned: value.pinned });
  }
  effect(() => applyPanel(options.settings?.()?.projectPanel));
  if (!options.settings) {
    const subscription = Settings.subscribe((event) => applyPanel(event.data.projectPanel));
    cleanup(() => subscription.close());
  }
  async function setProjectPanel(next: { view: "list" | "columns" | "grid"; pinned: string[] }) {
    if (savingPanel) return;
    const previous = projectPanel(), request = ++panelRequest;
    savingPanel = true; deferredPanel = undefined; panelSaving.set(true); projectPanel.set(next);
    let saved: SettingsDto["projectPanel"];
    try { saved = (await Settings.set({ projectPanel: next })).projectPanel; }
    catch (error) { if (alive) { projectPanel.set(previous); options.notice(`Could not save project browser: ${error}`); } }
    finally {
      savingPanel = false;
      if (alive) {
        applyPanel(saved ?? deferredPanel); panelSaving.set(false);
        // Re-read once after the write gate: a pushed edit from another window during
        // the save must win without replacing this save's normalized response with an old snapshot.
        void Settings.get().then((latest) => { if (alive && request === panelRequest) applyPanel(latest.projectPanel); }).catch(() => {});
      }
    }
  }
  function pinProject(id: string) {
    if (!state().projects.some((project) => project.id === id)) return;
    const prior = projectPanel();
    void setProjectPanel({ ...prior, pinned: prior.pinned.includes(id) ? prior.pinned.filter((pin) => pin !== id) : [...prior.pinned, id] });
  }
  const searches = new Map<string, YNode>();
  let generation = 0;
  let alive = true;
  let tabSubscription: Subscription | undefined;
  let storeRev = 0, sessionsRev = 0;
  let catalogRequest = 0;
  let savingSort = false;
  const sortIds = { name: "name", edited: "last_edited", added: "date_added", live: "live_sessions" } as const;
  function applyOrder(value: SettingsDto["projectOrder"] | undefined) {
    if (!value) return;
    order.set((Object.keys(sortIds) as ProjectOrder[]).find((key) => sortIds[key] === value.sort) ?? "edited");
    descending.set(value.descending);
  }
  effect(() => { const value = options.settings?.()?.projectOrder; if (!savingSort) applyOrder(value); });
  async function setOrder(next: ProjectOrder, reversed = next !== "name") {
    if (savingSort || next === order() && reversed === descending()) return;
    const prior = { sort: order(), descending: descending() };
    savingSort = true; sortPending.set(true); order.set(next); descending.set(reversed);
    try {
      const saved = await Settings.set({ projectOrder: { sort: sortIds[next], descending: reversed } });
      if (alive) applyOrder(saved.projectOrder);
    } catch (error) {
      if (alive) { order.set(prior.sort); descending.set(prior.descending); options.notice(`Could not save project order: ${error}`); }
    } finally { savingSort = false; if (alive) sortPending.set(false); }
  }
  async function refreshCatalog() {
    const request = ++catalogRequest, token = generation, project = options.projectId();
    if (project.startsWith("remote:")) return;
    catalogLoading.set(true);
    try {
      const next = await Chat.list();
      if (alive && request === catalogRequest && token === generation && project === options.projectId()) {
        chats.set(next); catalogNotice.set("");
      }
    } catch (reason) { if (alive && request === catalogRequest && token === generation) catalogNotice.set(`Chat history unavailable: ${reason}`); }
    finally { if (alive && request === catalogRequest && token === generation) catalogLoading.set(false); }
  }
  function openChat(chat: ChatSummary, project: string, token: number) {
    if (token !== generation || project !== options.projectId() || chat.projectId !== project) return;
    selectedChat.set(chat.id); selectedShell.set(null);
    options.showChat?.(chat.id);
  }
  cleanup(() => { alive = false; generation++; searches.clear(); tabSubscription?.close(); });
  function watchTabs() {
    tabSubscription?.close(); tabs.set(null);
    const projectId = options.projectId(), token = generation;
    // Legacy layout fixtures use opaque non-UUID project keys; shared membership
    // is defined only for canonical persisted project identities.
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(projectId)) return;
    try { tabSubscription = Tabs.subscribe(projectId, (event) => { if (alive && token === generation && projectId === options.projectId()) { tabs.set(event.data); void refreshCatalog(); } }); }
    catch (error) { options.notice(String(error)); }
  }
  watchTabs();
  void refreshCatalog();
  let wasForeground: boolean | undefined;
  cleanup(host.on("window.state", (value) => {
    const window = value as { key?: boolean; appActive?: boolean };
    const foreground = window.key === true && window.appActive !== false;
    // Moving and resizing also publish window.state. Refresh history only
    // when returning to this window, not on every live-resize frame.
    if (foreground && wasForeground === false) { now.set(Math.floor(Date.now() / 1000)); void refreshCatalog(); }
    wasForeground = foreground;
  }));
  try {
    const store = Store.subscribe((event) => {
      if (!alive || event.rev < storeRev) return;
      storeRev = event.rev;
      state.set(event.data); now.set(Math.floor(Date.now() / 1000));
      if (!loaded()) loaded.set(true);
      if (options.projectId().startsWith("remote:")) return;
      if (selectedTask() && !event.data.tasks.some((task) => task.id === selectedTask() && task.projectId === options.projectId())) selectTask(null);
      const trees = visibleWorktrees(event.data, options.projectId());
      // Keep an explicit unavailable selection: Files must not silently browse
      // a different worktree after the selected one disappears.
      if (selectedWorktree() === null) selectWorktree(trees.find((tree) => tree.isPrimary)?.id ?? null);
    });
    const sessions = Sessions.subscribe((event) => {
      if (!alive || event.rev < sessionsRev) return;
      sessionsRev = event.rev;
      shells.set(event.data.sessions);
    });
    cleanup(() => { store.close(); sessions.close(); });
  } catch (error) { options.notice(String(error)); }

  function selectWorktree(id: string | null) { selectedWorktree.set(id); options.selectWorktree(id); }
  function selectTask(id: string | null) {
    selectedTask.set(id); options.selectTask(id);
    const task = state().tasks.find((task) => task.id === id && task.projectId === options.projectId());
    if (task?.worktreeId && state().worktrees.some((tree) => tree.id === task.worktreeId && tree.projectId === options.projectId())) selectWorktree(task.worktreeId);
  }
  function resetProject(navigation?: { worktree: string | null; task: string | null }) {
    generation++;
    query.set(""); selectTask(navigation?.task ?? null); selectedShell.set(null); selectedChat.set(null); closingShell.set(null); chats.set([]); catalogNotice.set(""); editor.set(null);
    selectWorktree(navigation ? navigation.worktree : visibleWorktrees(state(), options.projectId()).find((tree) => tree.isPrimary)?.id ?? null);
    watchTabs(); void refreshCatalog();
  }
  async function run(action: () => Promise<unknown>, success?: () => void) {
    if (pending()) return;
    pending.set(true);
    try { await action(); if (alive) success?.(); }
    catch (error) { if (alive) options.notice(String(error)); }
    finally { if (alive) pending.set(false); }
  }
  async function activate(project: ProjectDto) {
    if (project.id === options.projectId() || pending()) return;
    await run(async () => {
      // As recording it would: a project removed elsewhere is refused before switching.
      if (!state().projects.some((known) => known.id === project.id)) throw new Error(`No project matches '${project.id}'`);
      const previous = generation;
      // The switch goes first: nothing on it reads the active project, and recording
      // it is a durable write whose publish every window handles on its JS thread.
      // The project is recorded as active whether or not the switch succeeds.
      try {
        await options.switchProject(project);
        if (generation === previous) resetProject();
      } finally { await Store.useProject(project.id); }
    });
  }
  function attach(shell: ShellSessionDto, projectId: string, token: number) {
    if (projectId !== options.projectId() || token !== generation || shell.projectId !== projectId || !state().projects.some((project) => project.id === projectId)) return;
    if (!shell.alive) { options.notice(`Shell ${shell.id.slice(0, 8)} has exited`); return; }
    selectedShell.set(shell.id); selectedChat.set(null);
    if (tabForShell(shell.id)?.hidden) {
      void run(async () => {
        await Tabs.update(projectId, { action: "unhide", key: `shell:${shell.id}` });
        if (alive && token === generation && projectId === options.projectId()) options.showShell(shell.id);
      });
    } else options.showShell(shell.id);
  }
  function tabForShell(id: string): TabEntryDto | undefined {
    function find(entries: TabEntryDto[]): TabEntryDto | undefined {
      for (const entry of entries) { if (entry.key === `shell:${id}`) return entry; const child = find(entry.children); if (child) return child; }
      return undefined;
    }
    return find(tabs()?.entries ?? []);
  }
  function titleForShell(shell: ShellSessionDto) { return tabForShell(shell.id)?.title || shellLabel(shell, state()); }
  function toggleFolder(id: string) {
    const next = new Set(collapsed()); if (!next.delete(id)) next.add(id); collapsed.set(next);
  }
  function expandFolder(id?: string) {
    const next = new Set(collapsed());
    const seen = new Set<string>();
    while (id && !seen.has(id)) { seen.add(id); next.delete(id); id = state().projectFolders.find((folder) => folder.id === id)?.parentId ?? undefined; }
    next.delete("unfiled"); collapsed.set(next);
  }
  function folderPath(id?: string | null) {
    const path: string[] = [], seen = new Set<string>();
    while (id && !seen.has(id)) {
      seen.add(id); const folder = state().projectFolders.find((folder) => folder.id === id);
      if (!folder) break;
      path.unshift(folder.name); id = folder.parentId;
    }
    return path.join(" / ") || "Root";
  }
  function folderMenu(projectId?: string, folderId?: string) {
    const current = projectId ? state().projects.find((project) => project.id === projectId)?.folderId : state().projectFolders.find((folder) => folder.id === folderId)?.parentId;
    void Menu.popup([
      { label: "Unfiled", checked: !current, disabled: !current, onSelect: () => void run(() => projectId ? Store.moveProjectToFolder(projectId, undefined) : Store.moveFolder(folderId!, undefined), () => expandFolder()) },
      ...state().projectFolders.map((folder) => ({ label: folderPath(folder.id), checked: current === folder.id, disabled: current === folder.id || folderId === folder.id,
        onSelect: () => void run(() => projectId ? Store.moveProjectToFolder(projectId, folder.id) : Store.moveFolder(folderId!, folder.id), () => expandFolder(folder.id)) })),
    ]);
  }
  function setTaskStatus(task: TaskDto, status: string) { void run(() => Store.setTaskStatus(task.id, status)); }
  function assignItems(task: TaskDto) {
    return [
      { label: "Unassigned", checked: !task.worktreeId, onSelect: () => void run(() => Store.unassignTasks([task.id])) },
      ...state().worktrees.filter((tree) => tree.projectId === options.projectId()).map((tree) => ({ label: worktreeLabel(tree), checked: tree.id === task.worktreeId,
        onSelect: () => void run(() => Store.assignTasks(tree.id, [task.id]), () => selectWorktree(tree.id)) })),
    ];
  }
  function taskMenu(task: TaskDto) {
    void Menu.popup([
      ...TASK_STATUSES.map((status) => ({ label: taskStatusLabel(status), checked: task.status === status, disabled: pending(), onSelect: () => setTaskStatus(task, status) })),
      { type: "separator" },
      { label: "Assign to", disabled: pending(), submenu: assignItems(task) },
    ]);
  }
  return { ...options, projectPanel, panelSaving, setProjectPanel, pinProject, state, loaded, shells, chats, catalogLoading, catalogNotice, sessionFilter, selectedChat, closingShell, refreshCatalog, openChat, tabs, query, collapsed, selectedWorktree, selectedTask, taskFilter, now, selectedShell, editor, pending, order, descending, searches,
    selectWorktree, selectTask, setTaskStatus, assignItems, taskMenu, resetProject, run, activate, attach, tabForShell, titleForShell, toggleFolder, expandFolder, folderMenu, folderPath, setOrder, sortPending, generation: () => generation };
}
export type PanelController = ReturnType<typeof createPanelController>;

/** A chip or a button. A `selected` chip (the current filter or status) and a `primary` button (the
 * main action of a form) take the accent fill with a `bg` label, and keep it while hovered or
 * focused: hover and focus come after the selected state, so a plain focus fill would leave the
 * light label on light gray after a click. A filled one shows its focus as a ring set off from the
 * fill by a gap of the surface, so it shows even where the focus color is the accent (native light).
 * Disabled, a primary button is a plain one with a muted label, and a selected chip keeps its fill,
 * dimmed. */
function Command(props: { id?: string; label: string; disabled?: boolean; selected?: boolean; primary?: boolean; onPress: () => void; children?: JSX.Element }) {
  const filled = () => !!props.selected || !!props.primary && !props.disabled;
  const fill = (other: string) => filled() ? theme().palette.cyan : other;
  const ring = () => ({ backgroundColor: theme().palette.cyan, outlineStyle: "solid" as const, outlineWidth: 2, outlineOffset: 2, outlineColor: theme().palette.focus });
  return <Button testID={props.id} accessibilityLabel={props.label} disabled={props.disabled} selected={props.selected} onPress={props.onPress}
    style={{ ...button(), backgroundColor: fill(theme().palette.panelActive), opacity: props.disabled && props.selected ? 0.5 : 1 }}
    hoverStyle={filled() ? { backgroundColor: theme().palette.cyan, opacity: 0.85 } : { backgroundColor: theme().palette.divider }} focusStyle={filled() ? ring() : { backgroundColor: theme().palette.divider }}>
    <Text style={{ fontFamily: theme().family, fontSize: text(11), fontWeight: props.primary ? "600" : "400", color: filled() ? theme().palette.bg : props.disabled ? muted() : ink() }}>{props.children ?? props.label}</Text>
  </Button>;
}

function PanelSearch(props: { controller: PanelController; panel: ListPanel; surface: string }) {
  const c = props.controller;
  const noun = props.panel === "shells" ? "sessions" : props.panel;
  let input: YNode | undefined;
  cleanup(() => c.searches.delete(props.surface));
  return <View style={{ paddingHorizontal: space(12, theme().scale), paddingBottom: space(8, theme().scale) }}>
    <View style={{ flexDirection: "row", gap: 5, paddingHorizontal: space(9, theme().scale), height: space(28, theme().scale), borderRadius: space(14, theme().scale), backgroundColor: theme().palette.panelActive, alignItems: "center", minWidth: 0 }} onPress={() => input?.focus()}>
      <Icon name="search" size={text(13)} color={muted()} />
      <Input testID={`search:${props.surface}`} accessibilityLabel={`Search ${noun}`} placeholder="Search  ⌘F" value={c.query()}
        onChange={(value) => c.query.set(value)} ref={(node) => { input = node; c.searches.set(props.surface, node); }}
        style={{ flex: 1, minWidth: 0, padding: 0, backgroundColor: "transparent", color: ink(), fontFamily: theme().family, fontSize: text(11) }} />
      {c.query() !== "" && <ToolbarCommand id={`clear-search:${props.surface}`} label="Clear the search" icon="close" onPress={() => { c.query.set(""); input?.focus(); }}>×</ToolbarCommand>}
    </View>
  </View>;
}

function ToolbarCommand(props: { id?: string; label: string; icon?: IconName; disabled?: boolean; selected?: boolean; onPress: () => void; children?: JSX.Element }) {
  return <Button tooltip={<View style={{ padding: 7, borderRadius: 6, backgroundColor: theme().palette.panelActive }}><Text style={{ fontFamily: theme().family, fontSize: text(11), color: ink() }}>{props.label}</Text></View>} testID={props.id} accessibilityLabel={props.label} disabled={props.disabled} selected={props.selected} onPress={props.onPress}
    style={{ width: space(PANEL.control, theme().scale), height: space(PANEL.control, theme().scale), borderRadius: space(13, theme().scale), alignItems: "center", justifyContent: "center", backgroundColor: props.selected ? theme().palette.panelActive : "transparent" }}
    hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.divider }} disabledStyle={{ opacity: 0.4 }}>
    <Icon name={props.icon ?? "more"} size={text(PANEL.icon)} color={props.selected ? ink() : muted()} />
  </Button>;
}

function Header(props: { title: string; count: number; total: number; noun: string; project?: string; children?: JSX.Element }) {
  return <View style={{ minHeight: space(PANEL.header, theme().scale), paddingHorizontal: space(PANEL.inset, theme().scale), flexDirection: "row", alignItems: "center", gap: space(8, theme().scale) }}>
    <View style={{ flex: 1, minWidth: 0, gap: 1 }}>
      <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: text(13), fontWeight: "600", color: ink() }}>{props.title}</Text>
      <Text numberOfLines={1} testID={`count:${props.noun}`} style={{ fontFamily: theme().family, fontSize: text(10), color: muted() }}>{`${countLabel(props.count, props.total, props.noun)}${props.project ? ` · ${props.project}` : ""}`}</Text>
    </View>
    {props.children && <View style={{ flexDirection: "row", alignItems: "center", gap: 2 }}>{props.children}</View>}
  </View>;
}
interface EmptyAction { id: string; label: string; text?: string; disabled?: boolean; onPress: () => void }
/** A list's empty state: "Loading…" until the first Store delivery (`loading`), then `message`, or
 * "No matches" under a search and "No <noun>" without one. `icon` replaces the search or folder
 * icon; `action` is one way out (show everything, add the first), hidden while loading. */
function Empty(props: { query: string; noun: string; loading?: boolean; message?: string; icon?: IconName; action?: EmptyAction }) {
  const searching = () => props.query.trim() !== "";
  return <View style={{ minHeight: space(120, theme().scale), padding: 24, gap: 10, alignItems: "center", justifyContent: "center" }}>
    <Icon name={props.icon ?? (searching() ? "search" : "folder")} size={text(22)} color={muted()} />
    <Text style={{ color: muted(), fontFamily: theme().family, fontSize: text(11) }}>{props.loading ? "Loading…" : props.message ?? (searching() ? "No matches" : `No ${props.noun}`)}</Text>
    {!props.loading && props.action && <Command id={props.action?.id} label={props.action?.label ?? ""} disabled={props.action?.disabled} onPress={() => props.action?.onPress()}>{props.action?.text}</Command>}
  </View>;
}

function ProjectRow(props: { item: () => ProjectTreeRow; controller: PanelController }) {
  // List recycles a cell across unrelated keys and row kinds. Keep both the
  // branch and every identity reactive instead of capturing the first item.
  // Projects move with "Move to folder…"; nothing in the panel takes a drop.
  return <>{props.item().kind === "empty" ?
    <View style={{ paddingLeft: space(6 + props.item().depth * 16, theme().scale), paddingVertical: space(7, theme().scale) }}><Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: text(10), color: muted() }}>Empty. Move projects here from their menu.</Text></View> :
    props.item().kind === "folder" ? <FolderRow item={props.item} controller={props.controller} /> : <ProjectItemRow item={props.item} controller={props.controller} />}</>;
}

function FolderRow(props: { item: () => ProjectTreeRow; controller: PanelController }) {
  const c = props.controller;
  const current = () => props.item() as Extract<ProjectTreeRow, { kind: "folder" }>;
  const id = () => current().folder?.id ?? current().id.replace(/^folder-/, "");
  const hovering = signal(false);
  function menu() {
    if (!current().folder) return;
    void Menu.popup([
      { label: "New subfolder", onSelect: () => c.editor.set({ kind: "folder", parent: id(), name: "" }) },
      { label: "Rename folder…", onSelect: () => c.editor.set({ kind: "folder", id: id(), parent: current().folder!.parentId ?? undefined, name: current().name }) },
      { label: "Move folder to…", onSelect: () => c.folderMenu(undefined, id()) },
      { type: "separator" },
      { label: "Remove folder", onSelect: () => void c.run(() => Store.removeFolder(id())) },
    ]);
  }
  return <View onHoverIn={() => hovering.set(true)} onHoverOut={() => hovering.set(false)} onContextMenu={menu}
    style={{ paddingLeft: space(6 + current().depth * 16, theme().scale), paddingRight: 6, height: space(30, theme().scale), flexDirection: "row", alignItems: "center", gap: 3 }}>
    <View testID={current().id} role="button" focusable={true} accessibilityLabel={current().name} accessibilityState={{ expanded: !c.collapsed().has(id()) }} onPress={() => c.toggleFolder(id())}
      style={{ flex: 1, minWidth: 0, flexDirection: "row", alignItems: "center", gap: space(6, theme().scale), height: "100%" }} focusStyle={{ backgroundColor: theme().palette.divider }}>
      <Icon name={c.collapsed().has(id()) ? "right" : "down"} size={text(10)} color={muted()} />
      <Text numberOfLines={1} style={{ flex: 1, fontFamily: theme().family, fontSize: text(10), fontWeight: "600", color: muted() }}>{current().name}</Text>
      <Text style={{ fontFamily: theme().family, fontSize: text(9), color: muted() }}>{String(current().count).padStart(2, "0")}</Text>
    </View>
    {current().folder && hovering() && <>
      <ToolbarCommand id={`subfolder:${id()}`} icon="add" label="Create subfolder" onPress={() => c.editor.set({ kind: "folder", parent: id(), name: "" })} />
      <ToolbarCommand id={`folder-menu:${id()}`} icon="more" label="Folder actions" onPress={menu} />
    </>}
  </View>;
}

const plural = (count: number, noun: string) => `${count} ${count === 1 ? noun : noun.endsWith("y") ? `${noun.slice(0, -1)}ies` : `${noun}s`}`;

function ProjectItemRow(props: { item: () => ProjectTreeRow; controller: PanelController }) {
  const c = props.controller;
  const project = () => (props.item() as Extract<ProjectTreeRow, { kind: "project" }>).project;
  const tasks = () => c.state().tasks.filter((task) => task.projectId === project().id);
  const trees = () => visibleWorktrees(c.state(), project().id);
  const live = () => c.shells().filter((shell) => shell.projectId === project().id && shell.alive).length;
  const selected = () => c.projectId() === project().id;
  const hovering = signal(false);
  const pinned = () => c.projectPanel().pinned.includes(project().id);
  const activity = () => {
    const chats = c.chats().filter((chat) => chat.projectId === project().id);
    const working = chats.filter((chat) => chat.state === "running").length, waiting = chats.filter((chat) => chat.state === "waiting").length;
    return [working ? `${working} working` : "", waiting ? `${waiting} waiting` : ""].filter(Boolean).join(" · ");
  };
  // Only what is there: every project has its primary worktree, and zeros say nothing. An idle project is one line.
  const summary = () => {
    const open = tasks().filter((task) => task.status !== "done").length, extra = trees().length;
    return [activity() ? `Agents ${activity()}` : "", live() ? plural(live(), "live session") : "", open ? plural(open, "open task") : "", extra > 1 ? plural(extra, "worktree") : ""].filter(Boolean).join(" · ");
  };
  async function settings() { await c.activate(project()); if (c.projectId() === project().id) c.openPanel("project_settings"); }
  function openWindow() { void c.run(() => host.call("window.open", { kind: "project", projectId: project().id, path: project().root })); }
  // The row's ⋯ opens this too; the notification toggles live here, with checkmarks.
  function menu() {
    void Menu.popup([
      { label: "Open project", onSelect: () => void c.activate(project()) },
      { label: "Open in another window", onSelect: openWindow },
      { label: "Project settings…", onSelect: () => void settings() },
      { label: pinned() ? "Unpin project" : "Pin project", checked: pinned(), disabled: c.panelSaving(), onSelect: () => c.pinProject(project().id) },
      { label: "Move to folder…", onSelect: () => c.folderMenu(project().id) },
      { type: "separator" },
      { label: "Agent completion notifications", checked: project().notifyOnAgentDone, onSelect: () => void c.run(() => Store.setProjectNotifications(project().id, !project().notifyOnAgentDone)) },
      { label: "Wake orchestrators when a worker finishes", checked: project().wakeOrchestratorsOnAgentDone, onSelect: () => void c.run(() => Store.setProjectOrchestratorWakes(project().id, !project().wakeOrchestratorsOnAgentDone)) },
    ]);
  }
  return <View onHoverIn={() => hovering.set(true)} onHoverOut={() => hovering.set(false)} onContextMenu={menu}
    style={{ borderRadius: space(7, theme().scale), backgroundColor: selected() ? theme().palette.panelActive : "transparent" }} hoverStyle={{ backgroundColor: theme().palette.panelActive }}>
    <View style={{ flexDirection: "row", alignItems: "center", paddingRight: 2 }}>
      <Button testID={`project-${project().id}`} accessibilityLabel={project().name} selected={selected()} disabled={c.pending()} onPress={() => void c.activate(project())}
        style={{ flex: 1, minWidth: 0, minHeight: space(30, theme().scale), justifyContent: "center", paddingLeft: space(6 + props.item().depth * 16, theme().scale), paddingVertical: space(6, theme().scale), gap: 2 }} focusStyle={{ backgroundColor: theme().palette.divider }}>
        <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
          {pinned() && <Icon name="pin" size={text(12)} color={accent()} />}
          <Text numberOfLines={1} style={{ flex: 1, fontFamily: theme().family, fontSize: text(12), fontWeight: selected() ? "600" : "400", color: ink() }}>{project().name}</Text>
        </View>
        {summary() !== "" && <Text numberOfLines={1} testID={`project-summary:${project().id}`} style={{ fontFamily: theme().family, fontSize: text(10), color: muted() }}>{summary()}</Text>}
      </Button>
      {(selected() || hovering()) && <View style={{ flexDirection: "row", alignItems: "center", gap: 0 }}>
        <ToolbarCommand id={`pin-project:${project().id}`} icon="pin" label={pinned() ? "Unpin project" : "Pin project"} selected={pinned()} disabled={c.panelSaving()} onPress={() => c.pinProject(project().id)} />
        <ToolbarCommand id={`project-settings:${project().id}`} icon="settings" label="Project settings" onPress={() => void settings()} />
        <ToolbarCommand id={`open-project-window:${project().id}`} icon="open" label="Open project in another window" onPress={openWindow} />
        <ToolbarCommand id={`project-menu:${project().id}`} icon="more" label="Project actions" onPress={menu} />
      </View>}
    </View>
  </View>;
}

function Projects(props: { controller: PanelController; surface: string; footer?: JSX.Element }) {
  const c = props.controller;
  const width = signal(600);
  const rows = () => {
    const pins = new Set(c.projectPanel().pinned);
    const all = projectRows(c.state(), c.query(), new Set(), c.order(), c.descending(), c.shells());
    const pinned = all.filter((row): row is Extract<ProjectTreeRow, { kind: "project" }> => row.kind === "project" && pins.has(row.project.id));
    const rest = { ...c.state(), projects: c.state().projects.filter((project) => !pins.has(project.id)) };
    const flat = !rest.projectFolders.length;
    const normal = projectRows(rest, c.query(), flat ? new Set<string>() : c.collapsed(), c.order(), c.descending(), c.shells())
      .filter((row) => flat ? row.kind === "project" : !((row.id === "folder-unfiled" && row.kind === "folder" && row.count === 0) || row.id === "empty-unfiled"))
      .map((row) => flat && row.kind === "project" ? { ...row, depth: 0 } : row);
    return pinned.length ? [
      { kind: "folder", id: "folder-pinned", name: "Pinned", depth: 0, count: pinned.length } as ProjectTreeRow,
      ...(c.collapsed().has("pinned") && !c.query().trim() ? [] : pinned.map((row) => ({ ...row, depth: 0 }))),
      ...((flat || c.query().trim()) && normal.length ? [{ kind: "folder", id: "folder-projects", name: c.query().trim() ? "Matches" : "Projects", depth: 0, count: normal.length } as ProjectTreeRow] : []),
      ...(flat && c.collapsed().has("projects") && !c.query().trim() ? [] : normal),
    ] : normal;
  };
  const groups = () => {
    const result: { id: string; title: string; header?: ProjectTreeRow; rows: ProjectTreeRow[] }[] = [];
    for (const row of rows()) {
      if (row.kind === "folder") result.push({ id: row.id, title: row.name, header: row, rows: [] });
      else { if (!result.length) result.push({ id: "all", title: c.query().trim() ? "Matches" : "Projects", rows: [] }); result[result.length - 1].rows.push(row); }
    }
    return result;
  };
  // A toolbar button closes the form it opened, and shows while that form is open.
  const rootFolderForm = () => { const editor = c.editor(); return editor?.kind === "folder" && !editor.id && !editor.parent; };
  function sortMenu() {
    void Menu.popup([
      ...([["name", "Project name"], ["edited", "Last edited"], ["added", "Date added"], ["live", "Live sessions"]] as const).map(([value, label]) => ({ label, checked: c.order() === value, disabled: c.sortPending(),
        onSelect: () => { if (c.order() !== value) void c.setOrder(value); } })),
      { type: "separator" },
      { label: c.order() === "name" ? "A to Z" : c.order() === "live" ? "Fewest live sessions first" : "Oldest first", checked: !c.descending(), disabled: c.sortPending(), onSelect: () => void c.setOrder(c.order(), false) },
      { label: c.order() === "name" ? "Z to A" : c.order() === "live" ? "Most live sessions first" : "Newest first", checked: c.descending(), disabled: c.sortPending(), onSelect: () => void c.setOrder(c.order(), true) },
    ]);
  }
  return <View testID="projects-panel" onLayout={(event) => width.set(event.size.width)} style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.panel }}>
    <Header title="Projects" count={c.query().trim() ? rows().filter((row) => row.kind === "project").length : c.state().projects.length} total={c.state().projects.length} noun="project">
      <ToolbarCommand id="sort-projects" icon="sort" label="Sort projects" onPress={sortMenu} />
      <ToolbarCommand id="new-folder" icon="folder" label="New folder" selected={rootFolderForm()} onPress={() => c.editor.set(rootFolderForm() ? null : { kind: "folder", name: "" })} />
      <ToolbarCommand id="new-project" icon="add" label="New project" selected={c.editor()?.kind === "project"} onPress={() => c.editor.set(c.editor()?.kind === "project" ? null : { kind: "project" })} />
    </Header>
    <View testID="project-view-controls" role="radiogroup" accessibilityLabel="Project view" style={{ flexDirection: "row", gap: 4, paddingHorizontal: space(PANEL.inset, theme().scale), paddingBottom: 8 }}>
      {(["list", "columns", "grid"] as const).map((view) => <ToolbarCommand id={`project-view-${view}`} icon={view} label={`${view[0].toUpperCase() + view.slice(1)} view`} selected={c.projectPanel().view === view} disabled={c.panelSaving()} onPress={() => void c.setProjectPanel({ ...c.projectPanel(), view })} />)}
    </View>
    <InlineEditor controller={c} kinds={["folder", "project"]} />
    <PanelSearch controller={c} panel="projects" surface={props.surface} />
    {c.projectPanel().view === "list" ? <List each={rows()} keyExtractor={(item) => item.id} estimatedItemSize={space(46, theme().scale)} style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, theme().scale), paddingBottom: 8 }} footer={props.footer} empty={props.footer ? undefined : <Empty query={c.query()} noun="projects" loading={!c.loaded()} />}>
      {(item) => <ProjectRow item={item} controller={c} />}
    </List> : <Scroll testID={`projects-${c.projectPanel().view}`} style={{ flex: 1, minHeight: 0 }} contentStyle={{ padding: space(PANEL.inset, theme().scale), gap: 12 }}>
      {c.projectPanel().view === "columns" ? <View style={{ display: "grid", gridTemplateColumns: `repeat(${Math.max(1, Math.floor(width() / space(300, theme().scale)))}, minmax(0, 1fr))`, gap: 12 }}>
        {groups().map((group) => <View style={{ minWidth: 0, padding: 8, gap: 6, borderRadius: 10, backgroundColor: theme().palette.bg }}>
          <View>{group.header ? <FolderRow item={() => ({ ...group.header!, depth: 0 })} controller={c} /> : <Text style={{ fontFamily: theme().family, fontSize: text(12), fontWeight: "600", color: muted() }}>{group.title}</Text>}</View>
          {group.rows.map((item) => <ProjectRow item={() => ({ ...item, depth: 0 })} controller={c} />)}
        </View>)}
      </View> : groups().map((group) => <View style={{ gap: 8 }}>
        <View>{group.header ? <FolderRow item={() => ({ ...group.header!, depth: 0 })} controller={c} /> : <Text style={{ fontFamily: theme().family, fontSize: text(12), fontWeight: "600", color: muted() }}>{group.title}</Text>}</View>
        <View style={{ display: "grid", gridTemplateColumns: `repeat(${Math.max(1, Math.floor(width() / space(280, theme().scale)))}, minmax(0, 1fr))`, gap: 10 }}>
          {group.rows.map((item) => <View style={{ minWidth: 0, minHeight: 86, padding: 8, borderRadius: 10, backgroundColor: theme().palette.bg }}><ProjectRow item={() => ({ ...item, depth: 0 })} controller={c} /></View>)}
        </View>
      </View>)}
      {!rows().length && <Empty query={c.query()} noun="projects" loading={!c.loaded()} />}
      {props.footer}
    </Scroll>}

  </View>;
}

function Worktrees(props: { controller: PanelController; surface: string }) {
  type Tree = ReturnType<typeof visibleWorktrees>[number];
  const c = props.controller, s = () => theme().scale, family = () => theme().family;
  const all = () => visibleWorktrees(c.state(), c.projectId());
  const rows = () => all().filter((tree) => matches(c.query(), tree.id, tree.branch, tree.path, tree.repositoryRoot));
  const primary = () => all().find((tree) => tree.isPrimary);
  // A project folder without Git is listed with the placeholder branch "root".
  const name = (tree: Tree) => tree.isPrimary && !tree.repositoryRoot ? "Project folder" : tree.branch;
  // Name a repository only when it is not the project's own, so one-repository projects do not repeat it.
  const repository = (tree: Tree) => tree.repositoryRoot && tree.repositoryRoot !== primary()?.repositoryRoot ? tree.repositoryRoot.split("/").filter(Boolean).pop() ?? "" : "";
  // The Store keeps a selection after its worktree goes missing (Files must not switch by
  // itself), but a missing linked worktree has no row. Say so, and offer the way back.
  const stranded = () => {
    const id = c.selectedWorktree();
    if (!c.loaded() || id === null || all().some((tree) => tree.id === id)) return null;
    const tree = c.state().worktrees.find((tree) => tree.id === id);
    return tree ? name(tree) : `Worktree ${id.slice(0, 8)}`;
  };
  return <View testID="worktrees-panel" style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.panel }}>
    <Header title="Worktrees" count={rows().length} total={all().length} noun="worktree" project={c.projectLabel()}>
      <ToolbarCommand id="refresh-worktrees" icon="refresh" label="Refresh worktrees" disabled={c.pending()} onPress={() => void c.run(() => Store.refreshWorktrees(c.projectId()))} />
    </Header>
    <PanelSearch controller={c} panel="worktrees" surface={props.surface} />
    {stranded() && <View testID="worktree-selection-missing" style={{ marginHorizontal: space(12, s()), marginBottom: space(8, s()), padding: space(10, s()), gap: space(6, s()), borderRadius: space(8, s()), backgroundColor: theme().palette.bg }}>
      <Text style={{ fontFamily: family(), fontSize: text(11), fontWeight: "600", color: theme().palette.gold }}>Selected worktree is missing</Text>
      <Text style={{ fontFamily: family(), fontSize: text(10), color: muted() }}>{`${stranded()} is no longer on disk, so Files has nothing to show.`}</Text>
      <View style={{ flexDirection: "row" }}>
        <Command id="worktree-use-primary" label={primary() ? `Use ${name(primary()!)}` : "Use the project folder"} onPress={() => c.selectWorktree(primary()?.id ?? null)} />
      </View>
    </View>}
    <List each={rows()} keyExtractor={(tree) => tree.id} estimatedItemSize={space(58, s())} style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, s()), paddingBottom: space(6, s()) }} empty={<Empty query={c.query()} noun="worktrees" loading={!c.loaded()} />}>
      {(item) => <WorktreeRow item={item} />}
    </List>
  </View>;

  function WorktreeRow(props: { item: () => Tree }) {
    // List recycles a cell across keys: read the tree through item() every time.
    const tree = props.item;
    const selected = () => c.selectedWorktree() === tree().id, missing = () => tree().present === false;
    const tasks = () => {
      let open = 0, done = 0;
      for (const task of c.state().tasks) if (task.worktreeId === tree().id) { if (task.status === "done") done++; else open++; }
      return [open ? `${open} open` : "", done ? `${done} done` : ""].filter(Boolean).join(" · ");
    };
    const meta = () => [missing() ? "Missing on disk" : "", tasks()].filter(Boolean).join(" · ");
    const label = () => [name(tree()) === tree().branch ? worktreeLabel(tree()) : name(tree()), tree().isPrimary ? "primary" : "", missing() ? "missing" : ""].filter(Boolean).join(", ");
    return <Button testID={`worktree-${tree().id}`} accessibilityLabel={label()} selected={selected()} onPress={() => c.selectWorktree(tree().id)}
      tooltip={<View style={{ maxWidth: space(300, s()), padding: 7, gap: 2, borderRadius: 6, backgroundColor: theme().palette.panelActive }}>
        <Text style={{ fontFamily: theme().monoFamily, fontSize: text(10), color: ink() }}>{tree().path}</Text>
        <Text style={{ fontFamily: theme().monoFamily, fontSize: text(9), color: muted() }}>{tree().id}</Text>
      </View>}
      style={{ ...row(), flexDirection: "row", alignItems: "flex-start", gap: space(8, s()) }} hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.divider }} selectedStyle={{ backgroundColor: theme().palette.panelActive }}>
      <View style={{ paddingTop: 1 }}><Icon name={tree().repositoryRoot ? "worktrees" : "folder"} size={text(13)} color={missing() ? theme().palette.gold : selected() ? accent() : muted()} /></View>
      <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
        <View style={{ flexDirection: "row", alignItems: "center", gap: space(6, s()), minWidth: 0 }}>
          <Text numberOfLines={1} style={{ flexShrink: 1, fontFamily: family(), fontSize: text(11), fontWeight: selected() ? "600" : "400", color: ink() }}>{name(tree())}</Text>
          {repository(tree()) !== "" && <Text numberOfLines={1} style={{ flexShrink: 0, fontFamily: family(), fontSize: text(10), color: muted() }}>{repository(tree())}</Text>}
          {tree().isPrimary && name(tree()) === tree().branch && <Text style={{ flexShrink: 0, fontFamily: family(), fontSize: text(10), color: muted() }}>primary</Text>}
        </View>
        <Text numberOfLines={1} ellipsizeMode="head" style={{ fontFamily: theme().monoFamily, fontSize: text(10), color: muted() }}>{tree().path}</Text>
        {meta() !== "" && <Text numberOfLines={1} style={{ fontFamily: family(), fontSize: text(10), color: missing() ? theme().palette.gold : muted() }}>{meta()}</Text>}
      </View>
      {selected() && <View style={{ paddingTop: 1 }}><Icon name="check" size={text(12)} color={accent()} /></View>}
    </Button>;
  }
}

/** The selected task. `task` follows Store pushes in place: the detail is not mounted again, so
 * the focus and the details' scroll position stay. Choosing the status a task already has does
 * nothing (the Store would still move it up as "updated just now"). */
function TaskDetail(props: { controller: PanelController; task: () => TaskDto }) {
  const c = props.controller, task = props.task;
  const s = () => theme().scale;
  return <View testID="task-detail" style={{ borderTopWidth: 1, borderTopColor: theme().palette.divider, paddingHorizontal: space(12, s()), paddingTop: space(10, s()), paddingBottom: space(10, s()), gap: space(8, s()) }}>
    <View style={{ flexDirection: "row", alignItems: "flex-start", gap: space(6, s()) }}>
      <View style={{ paddingTop: 1 }}><Icon name={statusIcon(task().status)} size={text(14)} color={statusColor(task().status)} /></View>
      <Text selectable={true} numberOfLines={4} style={{ flex: 1, minWidth: 0, fontFamily: theme().family, fontSize: text(12), fontWeight: "600", color: ink() }}>{task().title}</Text>
      <ToolbarCommand id="close-task-detail" icon="close" label="Close task detail" onPress={() => c.selectTask(null)} />
    </View>
    <View style={{ flexDirection: "row", gap: 4, flexWrap: "wrap", alignItems: "center" }}>
      {TASK_STATUSES.map((status) => <Command id={`task-status:${status}`} label={taskStatusLabel(status)}
        selected={task().status === status} disabled={c.pending()} onPress={() => { if (task().status !== status) c.setTaskStatus(task(), status); }} />)}
    </View>
    <Button testID="assign-task" accessibilityLabel={`Assign task to worktree, currently ${taskWorktree(task(), c.state())}`} disabled={c.pending()}
      onPress={() => void Menu.popup(c.assignItems(task()))}
      style={{ ...button(), flexDirection: "row", alignItems: "center", gap: space(6, s()), alignSelf: "flex-start", maxWidth: "100%" }}
      hoverStyle={{ backgroundColor: theme().palette.divider }} focusStyle={{ backgroundColor: theme().palette.divider }}>
      <Icon name="worktrees" size={text(12)} color={muted()} />
      <Text numberOfLines={1} style={{ flexShrink: 1, fontFamily: theme().family, fontSize: text(11), color: task().worktreeId ? ink() : muted() }}>{taskWorktree(task(), c.state())}</Text>
      <Icon name="down" size={text(10)} color={muted()} />
    </Button>
    <If when={task().details.trim() !== ""}>
      <Scroll testID="task-detail-text" style={{ maxHeight: space(132, s()) }}>
        <Text selectable={true} style={{ fontFamily: theme().family, fontSize: text(11), color: ink() }}>{task().details}</Text>
      </Scroll>
    </If>
    <View style={{ gap: 2 }}>
      <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: text(10), color: muted() }}>{`Created ${taskDate(task().createdAt, c.now())} · updated ${taskAge(task().updatedAt, c.now())}`}</Text>
      <Text selectable={true} numberOfLines={1} style={{ fontFamily: theme().monoFamily, fontSize: text(9), color: muted() }}>{task().id}</Text>
    </View>
  </View>;
}

function TaskListRow(props: { item: () => TaskRow; controller: PanelController }) {
  // List recycles a cell across unrelated keys and row kinds; read through item().
  return <>{props.item().kind === "section" ? <TaskSection item={props.item} /> : <TaskItem item={props.item} controller={props.controller} />}</>;
}

function TaskSection(props: { item: () => TaskRow }) {
  const section = () => props.item() as Extract<TaskRow, { kind: "section" }>;
  return <View testID={section().id} style={{ height: space(28, theme().scale), paddingTop: space(6, theme().scale), paddingHorizontal: space(6, theme().scale), flexDirection: "row", alignItems: "center", gap: 6 }}>
    <Text numberOfLines={1} style={{ flex: 1, fontFamily: theme().family, fontSize: text(10), fontWeight: "600", color: muted() }}>{taskStatusLabel(section().status)}</Text>
    <Text style={{ fontFamily: theme().family, fontSize: text(9), color: muted() }}>{String(section().count)}</Text>
  </View>;
}

function TaskItem(props: { item: () => TaskRow; controller: PanelController }) {
  const c = props.controller;
  const task = () => (props.item() as Extract<TaskRow, { kind: "task" }>).task;
  const done = () => task().status === "done";
  const selected = () => c.selectedTask() === task().id;
  const meta = () => [task().worktreeId ? taskWorktree(task(), c.state()) : "", taskAge(task().updatedAt, c.now())].filter(Boolean).join(" · ");
  return <View onContextMenu={() => c.taskMenu(task())}
    style={{ flexDirection: "row", alignItems: "flex-start", borderRadius: space(7, theme().scale), backgroundColor: selected() ? theme().palette.panelActive : "transparent" }} hoverStyle={{ backgroundColor: theme().palette.panelActive }}>
    <Button testID={`task-toggle:${task().id}`} accessibilityLabel={done() ? `Reopen ${task().title}` : `Mark ${task().title} done`} disabled={c.pending()}
      onPress={() => c.setTaskStatus(task(), done() ? "todo" : "done")}
      style={{ width: space(26, theme().scale), height: space(28, theme().scale), borderRadius: space(7, theme().scale), alignItems: "center", justifyContent: "center" }}
      hoverStyle={{ backgroundColor: theme().palette.divider }} focusStyle={{ backgroundColor: theme().palette.divider }}>
      <Icon name={statusIcon(task().status)} size={text(14)} color={statusColor(task().status)} />
    </Button>
    <Button testID={`task-${task().id}`} accessibilityLabel={`${task().title} · ${taskStatusLabel(task().status)}`} selected={selected()} onPress={() => c.selectTask(task().id)}
      style={{ flex: 1, minWidth: 0, paddingRight: space(6, theme().scale), paddingVertical: space(6, theme().scale), gap: 2 }} focusStyle={{ backgroundColor: theme().palette.divider }}>
      <Text numberOfLines={2} style={{ fontFamily: theme().family, fontSize: text(11), color: done() ? muted() : ink() }}>{task().title}</Text>
      <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: text(10), color: muted() }}>{meta()}</Text>
    </Button>
  </View>;
}

function TaskEmpty(props: { controller: PanelController; counts: () => Record<TaskFilter, number> }) {
  const c = props.controller;
  const searching = () => c.query().trim() !== "";
  // The list is empty, so every count here is a match outside the current filter.
  const elsewhere = () => props.counts().all;
  const message = () => !elsewhere() ? searching() ? "No matches" : "No tasks yet" : `No ${c.taskFilter()} tasks${searching() ? " match" : ""}`;
  const action = (): EmptyAction | undefined => elsewhere() > 0 ? { id: "tasks-show-all", label: "Show all tasks", text: `Show all ${elsewhere()}`, onPress: () => c.taskFilter.set("all") }
    : !searching() ? { id: "tasks-empty-add", label: "Add task", disabled: c.pending(), onPress: () => { if (c.editor()?.kind !== "task") c.editor.set({ kind: "task" }); } } : undefined;
  return <Empty query={c.query()} noun="tasks" loading={!c.loaded()} message={message()} action={action()}
    icon={searching() ? "search" : elsewhere() && c.taskFilter() === "open" ? "task-done" : "tasks"} />;
}

function Tasks(props: { controller: PanelController; surface: string }) {
  const c = props.controller;
  const all = () => c.state().tasks.filter((task) => task.projectId === c.projectId());
  const found = () => all().filter((task) => taskMatches(task, c.state(), c.query()));
  const counts = () => taskCounts(found());
  const rows = () => taskRows(found(), c.taskFilter());
  const selected = () => all().find((task) => task.id === c.selectedTask());
  // A new editor value would mount a new, empty form: keep an open form and what it holds.
  const add = () => { if (c.editor()?.kind !== "task") c.editor.set({ kind: "task" }); };
  // The filter chips count each filter; the header, like the other panels', counts search matches.
  return <View testID="tasks-panel" style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.panel }}>
    <Header title="Tasks" count={counts().all} total={all().length} noun="task" project={c.projectLabel()}>
      <ToolbarCommand id="new-task" icon="add" label="Add task" disabled={c.pending()} onPress={add} />
    </Header>
    <InlineEditor controller={c} kinds={["task"]} />
    <PanelSearch controller={c} panel="tasks" surface={props.surface} />
    <View testID="tasks-controls" style={{ paddingHorizontal: space(12, theme().scale), paddingBottom: space(6, theme().scale), flexDirection: "row", gap: 4 }}>
      {([["open", "Open"], ["done", "Done"], ["all", "All"]] as const).map(([filter, label]) =>
        <Command id={`tasks-filter-${filter}`} label={`${label} tasks`} selected={c.taskFilter() === filter} onPress={() => c.taskFilter.set(filter)}>{`${label} ${counts()[filter]}`}</Command>)}
    </View>
    <List each={rows()} keyExtractor={(row) => row.id} estimatedItemSize={space(42, theme().scale)} style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, theme().scale), paddingBottom: space(6, theme().scale) }} empty={<TaskEmpty controller={c} counts={counts} />}>
      {(item) => <TaskListRow item={item} controller={c} />}
    </List>
    <If when={selected()}>{(task) => <TaskDetail controller={c} task={task} />}</If>
  </View>;
}

function Shells(props: { controller: PanelController; surface: string }) {
  const c = props.controller, sessionApi = c.sessionActions ?? Sessions;
  const allShells = () => c.shells().filter((shell) => shell.kind === "project" && shell.projectId === c.projectId());
  const allChats = () => c.chats().filter((chat) => chat.projectId === c.projectId());
  const project = () => c.state().projects.find((project) => project.id === c.projectId());
  const context = (chat: ChatSummary) => {
    const trees = c.state().worktrees.filter((tree) => tree.projectId === chat.projectId);
    const tree = trees.find((tree) => tree.id === chat.worktreeId) ?? trees.filter((tree) => chat.cwd === tree.path || chat.cwd.startsWith(`${tree.path}/`)).sort((a, b) => b.path.length - a.path.length)[0];
    return tree ? worktreeLabel(tree) : chat.worktreeId ? `Worktree ${chat.worktreeId.slice(0, 8)}` : "Project root";
  };
  const provider = (chat: ChatSummary) => chat.provider === "codex" ? "Codex" : chat.provider === "claude" ? "Claude" : "Muse";
  const status = (chat: ChatSummary) => chat.state === "running" ? "Working" : chat.state.charAt(0).toUpperCase() + chat.state.slice(1);
  // [filter, chip, what it lists, accessibility label]
  const FILTERS = [["all", "All", "sessions", "All sessions"], ["codex", "Codex", "Codex chats", "Codex chats"], ["claude", "Claude", "Claude chats", "Claude chats"],
    ["muse", "Muse", "Muse chats", "Muse chats"], ["shells", "Shells", "shells", "Shells"]] as const;
  type SessionFilter = typeof FILTERS[number][0];
  type SessionRow = { kind: "chat"; chat: ChatSummary; id: string; projectId: string; token: number; rank: number } | { kind: "shell"; shell: ShellSessionDto; id: string; projectId: string; token: number; rank: number };
  // What needs the person first, then live work, then history; as Tasks lists open work first.
  const chatRank = (chat: ChatSummary) => chat.state === "waiting" || chat.state === "failed" ? 0 : chat.state === "running" || chat.state === "starting" ? 1 : chat.state === "stopped" ? 3 : 2;
  const foundChats = () => { const query = c.query(); return allChats().filter((chat) => matches(query, chat.id, chat.title, provider(chat), status(chat), context(chat), chat.cwd, chat.worktreeId)); };
  const foundShells = () => { const query = c.query(); return allShells().filter((shell) => shellMatches(shell, c.state(), query) || (!!query.trim() && matches(query, c.tabForShell(shell.id)?.title))); };
  const counts = (): Record<SessionFilter, number> => {
    const chats = foundChats(), shells = foundShells().length;
    const of = (name: string) => chats.filter((chat) => chat.provider === name).length;
    return { all: chats.length + shells, codex: of("codex"), claude: of("claude"), muse: of("muse"), shells };
  };
  const rows = (): SessionRow[] => {
    const filter = c.sessionFilter(), projectId = c.projectId(), token = c.generation();
    const chats: SessionRow[] = filter === "shells" ? [] : foundChats().filter((chat) => filter === "all" || chat.provider === filter)
      .map((chat) => ({ kind: "chat", chat, id: `chat:${chat.id}`, projectId, token, rank: chatRank(chat) }));
    const shells: SessionRow[] = (filter === "all" || filter === "shells" ? foundShells() : [])
      .map((shell) => ({ kind: "shell", shell, id: `shell:${shell.id}`, projectId, token, rank: shell.alive ? 1 : 3 }));
    // Grouped by rank, each group in the host's order.
    const all = [...chats, ...shells];
    return [0, 1, 2, 3].flatMap((rank) => all.filter((row) => row.rank === rank));
  };
  function createShell() {
    void c.run(async () => {
      const projectId = c.projectId(), token = c.generation();
      const project = c.state().projects.find((project) => project.id === projectId);
      if (!project) return;
      const tree = c.state().worktrees.find((tree) => tree.id === c.selectedWorktree() && tree.projectId === projectId);
      const shell = await Sessions.create({ projectId, worktreeId: tree?.id, cwd: tree?.path ?? project.root });
      c.attach(shell, projectId, token);
    });
  }
  function refresh() {
    void c.refreshCatalog();
    // The subscription is ordered by revision and pushes only changes: a push that lands while
    // this read is in flight is newer than it, and must not be rolled back.
    const before = c.shells();
    void sessionApi.list().then((next) => { if (c.shells() === before) c.shells.set(next.sessions); }).catch((reason) => c.notice(String(reason)));
  }
  const ending = signal<{ project: string; generation: number; ids: string[] } | null>(null);
  const targets = () => allShells().filter((shell) => shell.alive && shell.kind === "project");
  effect(() => { const request = ending(); if (request && (request.project !== c.projectId() || request.generation !== c.generation())) ending.set(null); });
  effect(() => { if (ending()) return keymap.bind("escape", () => { if (!c.pending()) ending.set(null); }); });
  function requestEnd() {
    if (c.pending()) return;
    const ids = targets().map((shell) => shell.id);
    if (ids.length) ending.set({ project: c.projectId(), generation: c.generation(), ids });
  }
  function endTerminals() {
    const request = ending();
    if (!request || request.project !== c.projectId() || request.generation !== c.generation()) { ending.set(null); return; }
    ending.set(null);
    void c.run(async () => {
      const current = await sessionApi.list();
      const allowed = new Set(current.sessions.filter((shell) => shell.alive && shell.kind === "project" && shell.projectId === request.project).map((shell) => shell.id));
      let ended = 0;
      const failed: string[] = [];
      for (const id of request.ids) {
        if (request.project !== c.projectId() || request.generation !== c.generation()) break;
        if (!allowed.has(id)) continue;
        try { await sessionApi.close(id); ended++; }
        catch (error) { failed.push(`${id.slice(0, 8)} (${error})`); }
      }
      c.notice(`Ended ${ended} terminal${ended === 1 ? "" : "s"}.${failed.length ? ` Not ended: ${failed.join("; ")}` : ""}`);
      refresh();
    });
  }
  const s = () => theme().scale;
  const lead = () => space(6, s()) + text(13) + space(8, s());
  const rowStyle = () => ({ ...row(), flexDirection: "row" as const, alignItems: "flex-start" as const, gap: space(8, s()) });
  return <View testID="shells-panel" style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.panel }}>
    <Header title="Sessions" count={rows().length} total={allShells().length + allChats().length} noun="session" project={c.projectLabel()}>
      <ToolbarCommand id="sessions-refresh" icon="refresh" label="Refresh sessions" disabled={c.catalogLoading()} onPress={refresh} />
      {c.newChat && <ToolbarCommand id="sessions-new-chat" icon="chat" label="New chat…" onPress={() => c.newChat?.()} />}
      <ToolbarCommand id="new-shell" icon="terminal" label="New terminal" disabled={c.pending() || !project()} onPress={createShell} />
      <ToolbarCommand id="nuke-shells" icon="radiation" label="End project terminals…" disabled={c.pending() || !targets().length} onPress={requestEnd} />
    </Header>
    {ending() && <View testID="end-terminals-confirm" role="alert" accessibilityLabel="End project terminals?" style={{ marginHorizontal: space(PANEL.inset, theme().scale), marginBottom: 12, padding: 12, gap: 10, borderRadius: 10, backgroundColor: theme().palette.bg, borderWidth: 1, borderColor: theme().palette.danger }}>
      <Text style={{ fontFamily: theme().family, fontSize: text(13), fontWeight: "600", color: ink() }}>{`End ${ending()!.ids.length} project terminal${ending()!.ids.length === 1 ? "" : "s"}?`}</Text>
      <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>Running processes and agents stop. Unsaved terminal output and temporary shell uploads are deleted. Chats and orchestrator terminals are kept.</Text>
      <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
        <Button testID="end-terminals-cancel" ref={(node) => node.focus()} onPress={() => ending.set(null)} style={button()}><Text style={{ fontFamily: theme().family, fontSize: text(11), color: ink() }}>Cancel</Text></Button>
        <Button testID="end-terminals-confirm-action" onPress={endTerminals} style={button()}><Text style={{ fontFamily: theme().family, fontSize: text(11), color: theme().palette.danger }}>{`End ${ending()!.ids.length} terminals`}</Text></Button>
      </View>
    </View>}
    <PanelSearch controller={c} panel="shells" surface={props.surface} />
    <View testID="sessions-controls" style={{ paddingHorizontal: space(12, s()), paddingBottom: space(6, s()), flexDirection: "row", gap: 4, flexWrap: "wrap" }}>
      {FILTERS.filter(([filter]) => filter === "all" || filter === "shells" || c.sessionFilter() === filter || allChats().some((chat) => chat.provider === filter)).map(([filter, chip, , label]) =>
        <Command id={`sessions-filter-${filter}`} label={label} selected={c.sessionFilter() === filter} onPress={() => c.sessionFilter.set(filter)}>{`${chip} ${counts()[filter]}`}</Command>)}
    </View>
    {c.catalogNotice() !== "" && <Text testID="sessions-catalog-notice" style={{ fontFamily: theme().family, fontSize: text(10), color: theme().palette.danger, paddingHorizontal: 12, paddingBottom: 8 }}>{c.catalogNotice()}</Text>}
    {c.catalogLoading() && allChats().length === 0 && <Text style={{ fontFamily: theme().family, fontSize: text(10), color: muted(), paddingHorizontal: 12, paddingBottom: 8 }}>Loading chat history…</Text>}
    <List each={rows()} keyExtractor={(item) => item.id} estimatedItemSize={space(58, s())} style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, s()), paddingBottom: 6 }} empty={<SessionsEmpty />}>
      {(item) => <>{item().kind === "chat" ? <ChatSession item={() => item() as Extract<SessionRow, { kind: "chat" }>} /> : <ShellSession item={() => item() as Extract<SessionRow, { kind: "shell" }>} />}</>}
    </List>
  </View>;

  function SessionsEmpty() {
    const searching = () => c.query().trim() !== "";
    const entry = () => FILTERS.find(([filter]) => filter === c.sessionFilter()) ?? FILTERS[0];
    // The list is empty, so under a narrower filter every match counted here is outside it.
    const elsewhere = () => c.sessionFilter() === "all" ? 0 : counts().all;
    const shellsShown = () => c.sessionFilter() === "all" || c.sessionFilter() === "shells";
    // Without a match elsewhere, Empty says "No matches" or "No <what the filter lists>" itself.
    const action = (): EmptyAction | undefined => elsewhere() > 0 ? { id: "sessions-show-all", label: "Show all sessions", text: `Show all ${elsewhere()}`, onPress: () => c.sessionFilter.set("all") }
      : !searching() && shellsShown() ? { id: "sessions-empty-new-shell", label: "New shell", disabled: c.pending() || !project(), onPress: createShell } : undefined;
    return <Empty query={c.query()} noun={entry()[2]} loading={!c.loaded()} message={elsewhere() ? `No ${entry()[2]}${searching() ? " match" : ""}` : undefined} action={action()}
      icon={searching() ? "search" : shellsShown() ? "terminal" : "chat"} />;
  }
  function ChatSession(props: { item: () => Extract<SessionRow, { kind: "chat" }> }) {
    const item = props.item, chat = () => item().chat;
    // Waiting and failed chats need the person; the weight marks them where the theme gives
    // working and attention the same colour.
    const attention = () => chat().state === "waiting" || chat().state === "failed";
    const tone = () => chat().state === "running" || chat().state === "starting" ? theme().palette.working
      : chat().state === "failed" ? theme().palette.danger : attention() ? theme().palette.gold : muted();
    return <Button testID={`session-chat-${chat().id}`} accessibilityLabel={`${provider(chat())} chat · ${chat().title || "Untitled chat"} · ${status(chat())} · ${context(chat())}`}
      selected={c.selectedChat() === chat().id} disabled={!c.showChat} onPress={() => c.openChat(chat(), item().projectId, item().token)}
      style={rowStyle()} hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.divider }} selectedStyle={{ backgroundColor: theme().palette.panelActive }}>
      <View style={{ paddingTop: 1 }}><Icon name="chat" size={text(13)} color={tone()} /></View>
      <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
        <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: text(11), color: ink() }}>{chat().title.trim() || "Untitled chat"}</Text>
        <View style={{ flexDirection: "row", minWidth: 0 }}>
          <Text numberOfLines={1} style={{ flexShrink: 0, fontFamily: theme().family, fontSize: text(10), fontWeight: attention() ? "600" : "400", color: tone() }}>{status(chat())}</Text>
          <Text numberOfLines={1} style={{ flexShrink: 1, minWidth: 0, fontFamily: theme().family, fontSize: text(10), color: muted() }}>{` · ${provider(chat())} · ${context(chat())}`}</Text>
        </View>
        <Text numberOfLines={1} ellipsizeMode="middle" style={{ fontFamily: theme().monoFamily, fontSize: text(10), color: muted() }}>{chat().cwd}</Text>
      </View>
    </Button>;
  }
  function ShellSession(props: { item: () => Extract<SessionRow, { kind: "shell" }> }) {
    const item = props.item, shell = () => item().shell;
    const state = () => shell().alive ? "Live" : "Exited";
    const tone = () => shell().alive ? accent() : muted();
    function menu() {
      void Menu.popup([
        { label: "Open session", disabled: !shell().alive, onSelect: () => c.attach(shell(), item().projectId, item().token) },
        { type: "separator" },
        { label: "End shell…", disabled: !shell().alive, onSelect: () => c.closingShell.set(shell()) },
      ]);
    }
    return <View onContextMenu={menu}>
      <Button testID={`shell-${shell().id}`} accessibilityLabel={`Shell · ${c.titleForShell(shell())} · ${state()} · ${shell().command ?? shell().cwd}`} selected={c.selectedShell() === shell().id} onPress={() => c.attach(shell(), item().projectId, item().token)} style={rowStyle()}
        hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.divider }} selectedStyle={{ backgroundColor: theme().palette.panelActive }}>
        <View style={{ paddingTop: 1 }}><Icon name="terminal" size={text(13)} color={tone()} /></View>
        <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
          <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: text(11), color: shell().alive ? ink() : muted() }}>{c.titleForShell(shell())}</Text>
          <View style={{ flexDirection: "row", minWidth: 0 }}>
            <Text numberOfLines={1} style={{ flexShrink: 0, fontFamily: theme().family, fontSize: text(10), color: tone() }}>{state()}</Text>
            <Text numberOfLines={1} style={{ flexShrink: 1, minWidth: 0, fontFamily: theme().family, fontSize: text(10), color: muted() }}>{` · ${shell().harness ?? "shell"}${c.tabForShell(shell().id)?.hidden ? " · Hidden tab" : ""}`}</Text>
          </View>
          <Text numberOfLines={1} ellipsizeMode={shell().command ? "tail" : "middle"} style={{ fontFamily: theme().monoFamily, fontSize: text(10), color: muted() }}>{shell().command ?? shell().cwd}</Text>
        </View>
      </Button>
      {c.selectedShell() === shell().id && shell().alive && c.closingShell()?.id !== shell().id && <View style={{ paddingLeft: lead(), paddingBottom: 4, alignItems: "flex-start" }}><Command id={`close-shell:${shell().id}`} label="End shell…" disabled={c.pending()} onPress={() => c.closingShell.set(shell())} /></View>}
      {c.closingShell()?.id === shell().id && shell().alive && <View testID="end-shell-confirm" style={{ marginLeft: lead(), marginRight: 6, marginBottom: 6, padding: 10, gap: 8, borderRadius: 8, backgroundColor: theme().palette.bg }}>
        <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>{`This ends ${c.titleForShell(shell())} and the processes running in it. Close its tab instead to keep it running.`}</Text>
        <View style={{ flexDirection: "row", gap: 6 }}>
          <Command id="confirm-end-shell" label="End shell" disabled={c.pending()} onPress={() => void c.run(() => Sessions.close(shell().id), () => c.closingShell.set(null))} />
          <Command label="Cancel" disabled={c.pending()} onPress={() => c.closingShell.set(null)} />
        </View>
      </View>}
    </View>;
  }
}

export function PanelContent(props: { controller: PanelController; panel: string; surface: string; projectFooter?: JSX.Element }) {
  // NSH's context provider reads its children untracked before resolving them.
  // Keep the changing panel choice in a callable child so its memo tracks it.
  return <PanelTheme.Provider value={props.controller.theme ?? defaultTheme}>{() => props.panel === "projects" ? <Projects controller={props.controller} surface={props.surface} footer={props.projectFooter} /> :
    props.panel === "worktrees" ? <Worktrees controller={props.controller} surface={props.surface} /> :
    props.panel === "tasks" ? <Tasks controller={props.controller} surface={props.surface} /> :
    props.panel === "shells" ? <Shells controller={props.controller} surface={props.surface} /> : null}</PanelTheme.Provider>;
}

export function ShellIntent(props: { controller: PanelController; shellId: string }) {
  return <PanelTheme.Provider value={props.controller.theme ?? defaultTheme}><ShellIntentContent {...props} /></PanelTheme.Provider>;
}

function ShellIntentContent(props: { controller: PanelController; shellId: string }) {
  const shell = () => props.controller.shells().find((shell) => shell.id === props.shellId);
  // It draws its own background: it can sit on any host surface.
  return <View testID={`shell-intent:${props.shellId}`} style={{ padding: 16, gap: 8, backgroundColor: theme().palette.panel }}>
    <Text style={{ fontFamily: theme().family, fontSize: text(13), color: ink() }}>{shell() ? props.controller.titleForShell(shell()!) : "Shell"}</Text>
    <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>{shell()?.alive === false ? "This shell has exited." : "Opening terminal…"}</Text>
    <Text selectable={true} style={{ fontFamily: theme().monoFamily, fontSize: text(11), color: muted() }}>{props.shellId}</Text>
    {shell() && <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>{`${shell()!.alive ? "Live" : "Exited"} · ${shell()!.cwd}`}</Text>}
  </View>;
}

function FolderEditor(props: { controller: PanelController; editor: Extract<Editor, { kind: "folder" }> }) {
  const c = props.controller, name = signal(props.editor.name), error = signal("");
  const blank = () => !name().trim();
  function save() {
    if (c.pending() || blank()) return;
    error.set("");
    void c.run(async () => {
      try { const folder = props.editor.id ? await Store.renameFolder(props.editor.id, name()) : await Store.createFolder(name(), props.editor.parent); c.expandFolder(folder.id); c.editor.set(null); }
      catch (reason) { error.set(String(reason)); }
    });
  }
  return <View style={{ gap: 10 }}>
    <Text numberOfLines={2} style={{ fontFamily: theme().family, fontSize: text(15), color: ink() }}>{props.editor.id ? "Rename folder" : props.editor.parent ? `New folder in ${c.folderPath(props.editor.parent)}` : "New folder"}</Text>
    <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>Group projects without moving their files.</Text>
    <Input testID="folder-name" accessibilityLabel="Folder name" placeholder="Folder name" autoFocus={true} value={name()} onChange={(value) => name.set(value)} onSubmit={save} style={field()} focusStyle={fieldFocus()} />
    {error() !== "" && <Text style={{ color: theme().palette.danger, fontFamily: theme().family, fontSize: text(11) }}>{error()}</Text>}
    <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 6 }}>
      <Command label="Cancel" disabled={c.pending()} onPress={() => c.editor.set(null)} />
      <Command id="save-folder" label={props.editor.id ? "Rename folder" : "Create folder"} primary={true} disabled={c.pending() || blank()} onPress={save} />
    </View>
  </View>;
}

function TaskEditor(props: { controller: PanelController }) {
  const c = props.controller, title = signal(""), details = signal(""), error = signal("");
  const projectId = c.projectId();
  function save() {
    // Return in the title submits too; a blank title is not a task.
    if (c.pending() || !title().trim() || projectId !== c.projectId()) return;
    void c.run(async () => {
      try {
        const task = await Store.addTask(projectId, title(), details());
        // The window switched projects while this saved: the task is the previous project's.
        if (projectId !== c.projectId()) return;
        if (c.taskFilter() === "done") c.taskFilter.set("open");
        c.selectTask(task.id); c.query.set(""); c.editor.set(null);
      }
      catch (reason) { error.set(String(reason)); }
    });
  }
  return <View style={{ gap: 10 }}>
    <Text style={{ fontFamily: theme().family, fontSize: text(13), fontWeight: "600", color: ink() }}>New task</Text>
    <Input testID="task-title" accessibilityLabel="Task title" placeholder="Task title" autoFocus={true} value={title()} onChange={(value) => { title.set(value); error.set(""); }} onSubmit={save} style={field()} focusStyle={fieldFocus()} />
    <Input testID="task-details" accessibilityLabel="Task details" placeholder="Details (optional)" multiline={true} autoCorrect={false} value={details()} onChange={(value) => details.set(value)} style={{ ...field(), minHeight: 90 }} focusStyle={fieldFocus()} />
    {error() !== "" && <Text style={{ color: theme().palette.danger, fontFamily: theme().family, fontSize: text(11) }}>{error()}</Text>}
    <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 6 }}>
      <Command label="Cancel" disabled={c.pending()} onPress={() => c.editor.set(null)} />
      <Command id="save-task" label="Add task" primary={true} disabled={c.pending() || !title().trim()} onPress={save} />
    </View>
  </View>;
}

function ProjectEditor(props: { controller: PanelController }) {
  const c = props.controller, name = signal(""), path = signal(""), defaultDirectory = signal("");
  const inspection = signal<ProjectInspectionDto | null>(null), checking = signal(false), error = signal(""), initGit = signal(true);
  const followsName = signal(true);
  let alive = true, generation = 0, timer: number | undefined;
  let pathInput: YNode | undefined;
  cleanup(() => { alive = false; generation++; clearTimeout(timer); });
  void (c.defaultProjectDirectory ? Promise.resolve(c.defaultProjectDirectory) : Store.defaultProjectDirectory()).then((directory) => {
    if (!alive) return;
    defaultDirectory.set(directory);
    if (followsName()) setPath(`${directory}/${name().trim()}`);
  }).catch((reason: unknown) => { if (alive) error.set(String(reason)); });
  function setPath(value: string) {
    path.set(value); inspection.set(null); error.set("");
    const token = ++generation;
    clearTimeout(timer);
    // A default path without a name is the projects directory itself: nothing to inspect (a deep
    // repository walk) and nothing to create, so Create stays off until there is a name.
    if (!value.trim() || followsName() && !name().trim()) { checking.set(false); return; }
    checking.set(true);
    timer = setTimeout(() => {
      void Store.inspectProject(value).then((next) => { if (alive && token === generation) { inspection.set(next); checking.set(false); } })
        .catch((reason: unknown) => { if (alive && token === generation) { error.set(String(reason)); checking.set(false); } });
    }, 250);
  }
  // The project is named with the trimmed name, so its default folder is too.
  function setName(value: string) { name.set(value); if (followsName() && defaultDirectory()) setPath(`${defaultDirectory()}/${value.trim()}`); }
  async function browse() {
    if (c.pending()) return;
    try {
      const result = await host.call("system.openPanel", { files: false, directories: true, multiple: false, prompt: "Choose project folder", startIn: path() }, { timeout: 0 }) as { paths: string[] };
      if (alive && result.paths[0]) { followsName.set(false); setPath(result.paths[0]); pathInput?.focus(); }
    } catch (reason) { error.set(String(reason)); }
  }
  function save() {
    const checked = inspection();
    if (!checked || checking() || c.pending()) return;
    if (followsName() && (name().includes("/") || name().startsWith("."))) {
      error.set(name().startsWith(".") ? "A default project name cannot start with a dot; use PATH for another location" : "A default project name must be one folder name; use PATH for another location"); return;
    }
    void c.run(async () => {
      try {
        const request = { root: checked.root, name: name().trim() || undefined, initGit: checked.canInitGit && initGit() };
        const project = checked.exists ? await Store.createProject(request) : await Store.createNewProject(request);
        await Store.useProject(project.id);
        const previous = c.generation();
        await c.switchProject(project); if (c.generation() === previous) c.resetProject(); c.openPanel("projects");
      } catch (reason) { error.set(String(reason)); }
    });
  }
  // The path as typed is in its field; name the folder it resolves to only when that differs (~, symlinks).
  const resolved = () => { const root = inspection()?.root ?? ""; return root === path().trim().replace(/(.)\/+$/, "$1") ? "" : root; };
  cleanup(keymap.bind("mod+o", () => void browse()));
  cleanup(keymap.bind("mod+g", () => { if (inspection()?.canInitGit && !c.pending()) initGit.set(!initGit()); }));
  return <View style={{ gap: 10 }}>
    <Text style={{ fontFamily: theme().family, fontSize: text(15), color: ink() }}>New project</Text>
    <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>Project name</Text>
    <Input testID="project-name" accessibilityLabel="Project name" placeholder={followsName() ? "my-project" : "Use folder name"} value={name()} readonly={c.pending()} autoFocus={true} onChange={setName} onSubmit={save} style={field()} focusStyle={fieldFocus()} />
    <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>Folder</Text>
    <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
      <Input testID="project-path" accessibilityLabel="Project folder" placeholder="/path/to/project" value={path()} readonly={c.pending()} onChange={(value) => { followsName.set(false); setPath(value); }} onSubmit={save} ref={(node) => { pathInput = node; }} style={{ ...field(), fontFamily: theme().monoFamily, flex: 1, minWidth: 0 }} focusStyle={fieldFocus()} />
      <Command id="browse-project" label="Browse…" disabled={c.pending()} onPress={() => void browse()} />
    </View>
    <Text testID="project-inspection" style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>{checking() ? "Checking folder…" : inspection() ? `${inspection()!.exists ? "Existing folder" : "New folder"} · ${inspection()!.repositoryCount ? plural(inspection()!.repositoryCount, "Git repository") : "no repositories"}` : "Enter a project name, or choose an existing folder."}</Text>
    {resolved() !== "" && <Text numberOfLines={2} style={{ fontFamily: theme().family, fontSize: text(10), color: muted() }}>{`Resolves to ${resolved()}`}</Text>}
    {!!inspection()?.warning && <Text style={{ fontFamily: theme().family, fontSize: text(11), color: theme().palette.gold }}>{inspection()!.warning}</Text>}
    {inspection()?.canInitGit ? <Switch testID="project-init-git" accessibilityLabel="Initialize Git" checked={initGit()} onChange={(value) => initGit.set(value)} disabled={c.pending()} style={{ flexDirection: "row", alignItems: "center", gap: 8 }}
      trackStyle={{ width: 28, height: 16, borderRadius: 8, padding: 2, alignItems: initGit() ? "flex-end" : "flex-start", backgroundColor: initGit() ? accent() : theme().palette.divider }} thumbStyle={{ width: 12, height: 12, borderRadius: 6, backgroundColor: theme().palette.bg }}>
      <Text style={{ fontFamily: theme().family, fontSize: text(11), color: ink() }}>Initialize Git</Text>
    </Switch> : inspection() && <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>Use existing repositories</Text>}
    {error() !== "" && <Text testID="project-error" style={{ fontFamily: theme().family, fontSize: text(11), color: theme().palette.danger }}>{error()}</Text>}
    <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 6 }}>
      <Command label="Cancel" disabled={c.pending()} onPress={() => c.editor.set(null)} />
      <Command id="create-project" label={c.pending() ? "Creating…" : "Create project"} primary={true} disabled={c.pending() || checking() || !inspection()} onPress={save} />
    </View>
  </View>;
}

/** A panel's form (new or renamed folder, new project, new task) unfolds under its toolbar. */
function InlineEditor(props: { controller: PanelController; kinds: Editor["kind"][] }) {
  const c = props.controller;
  return <View>{!!c.editor() && props.kinds.includes(c.editor()!.kind) && <View testID="panel-editor"
    style={{ marginHorizontal: space(8, theme().scale), marginBottom: space(8, theme().scale), padding: space(12, theme().scale), borderRadius: 8, backgroundColor: theme().palette.bg }}>
    {c.editor()!.kind === "folder" ? <FolderEditor controller={c} editor={c.editor() as Extract<Editor, { kind: "folder" }>} /> :
      c.editor()!.kind === "task" ? <TaskEditor controller={c} /> : <ProjectEditor controller={c} />}
  </View>}</View>;
}

