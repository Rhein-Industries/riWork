import { List, Scroll, Text, View, cleanup, context, effect, host, keymap, signal, type JSX, type YNode } from "nsh";
import { Button, Dialog, Input, Switch } from "nsh/base";
import { Menu } from "nsh/desktop";
import { Chat, Sessions, Settings, Store, Tabs, type ChatSummary, type ProjectDto, type ProjectInspectionDto, type SettingsDto, type ShellSessionDto, type StoreState, type Subscription, type TabEntryDto, type TabsSnapshot, type TaskDto } from "riwork/native";
import { EMPTY_STORE, countLabel, matches, projectRows, shellLabel, shellMatches, taskMark, taskMatches, visibleWorktrees, worktreeLabel, type ListPanel, type ProjectOrder, type ProjectTreeRow } from "./panel_model";
import { DEFAULT_THEME, space, type InterfaceTheme } from "./appearance";
import { Icon, type IconName } from "./Icon";

const defaultTheme = () => DEFAULT_THEME;
const PanelTheme = context<() => InterfaceTheme>(defaultTheme);
const theme = () => PanelTheme()();
const ink = () => theme().palette.text, muted = () => theme().palette.muted, accent = () => theme().palette.cyan;
const text = (base: number) => base * theme().scale;
const button = () => ({ paddingHorizontal: space(10, theme().scale), paddingVertical: space(4, theme().scale), borderRadius: space(14, theme().scale), backgroundColor: theme().palette.panelActive });
const field = () => ({ minHeight: space(28, theme().scale), paddingHorizontal: space(8, theme().scale), paddingVertical: space(5, theme().scale), borderRadius: 4, backgroundColor: theme().palette.bg, color: ink(), fontFamily: theme().family, fontSize: text(12) });
const row = () => ({ paddingHorizontal: space(6, theme().scale), paddingVertical: space(6, theme().scale), gap: space(2, theme().scale), borderRadius: space(7, theme().scale) });
const statusColor = (status: string) => status === "done" ? accent() : status === "in_progress" ? theme().palette.gold : muted();
type Editor = { kind: "folder"; parent?: string; id?: string; name: string } | { kind: "project" } | { kind: "task" };

interface PanelOptions {
  projectId: () => string;
  projectLabel: () => string;
  switchProject: (project: ProjectDto) => Promise<void>;
  openPanel: (panel: string) => void;
  showShell: (id: string) => void;
  showChat?: (id: string) => void;
  newChat?: () => void;
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
  const selectedShell = signal<string | null>(null);
  const editor = signal<Editor | null>(null);
  const pending = signal(false);
  const order = signal<ProjectOrder>("edited");
  const descending = signal(true);
  const sortPending = signal(false);
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
    if (foreground && wasForeground === false) void refreshCatalog();
    wasForeground = foreground;
  }));
  try {
    const store = Store.subscribe((event) => {
      if (!alive || event.rev < storeRev) return;
      storeRev = event.rev;
      state.set(event.data);
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
      await Store.useProject(project.id);
      const previous = generation;
      await options.switchProject(project);
      if (generation === previous) resetProject();
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
  return { ...options, state, loaded, shells, chats, catalogLoading, catalogNotice, sessionFilter, selectedChat, closingShell, refreshCatalog, openChat, tabs, query, collapsed, selectedWorktree, selectedTask, selectedShell, editor, pending, order, descending, searches,
    selectWorktree, selectTask, resetProject, run, activate, attach, tabForShell, titleForShell, toggleFolder, expandFolder, folderMenu, folderPath, setOrder, sortPending, generation: () => generation };
}
export type PanelController = ReturnType<typeof createPanelController>;

function Command(props: { id?: string; label: string; disabled?: boolean; selected?: boolean; onPress: () => void; children?: JSX.Element }) {
  return <Button testID={props.id} accessibilityLabel={props.label} disabled={props.disabled} selected={props.selected} onPress={props.onPress}
    style={button()} hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.divider }} selectedStyle={{ backgroundColor: theme().palette.cyan }}>
    <Text style={{ fontFamily: theme().family, fontSize: text(11), color: props.disabled ? muted() : props.selected ? theme().palette.bg : ink() }}>{props.children ?? props.label}</Text>
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
    style={{ width: space(24, theme().scale), height: space(24, theme().scale), borderRadius: space(13, theme().scale), alignItems: "center", justifyContent: "center", backgroundColor: "transparent" }}
    hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.divider }}>
    <Icon name={props.icon ?? "more"} size={text(13)} color={props.selected ? ink() : muted()} />
  </Button>;
}

function Header(props: { title: string; count: number; total: number; noun: string; project?: string; children?: JSX.Element }) {
  return <View style={{ height: space(46, theme().scale), paddingLeft: space(12, theme().scale), paddingRight: space(8, theme().scale), flexDirection: "row", alignItems: "center", gap: space(8, theme().scale) }}>
    <View style={{ flex: 1, minWidth: 0, gap: 1 }}>
      <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: text(13), fontWeight: "600", color: ink() }}>{props.title}</Text>
      <Text numberOfLines={1} testID={`count:${props.noun}`} style={{ fontFamily: theme().family, fontSize: text(10), color: muted() }}>{`${countLabel(props.count, props.total, props.noun)}${props.project ? ` · ${props.project}` : ""}`}</Text>
    </View>
    {props.children && <View style={{ flexDirection: "row", alignItems: "center", gap: 2 }}>{props.children}</View>}
  </View>;
}
function Empty(props: { query: string; noun: string }) {
  return <View style={{ minHeight: space(120, theme().scale), padding: 24, gap: 8, alignItems: "center", justifyContent: "center" }}>
    <Icon name={props.query.trim() ? "search" : "folder"} size={text(22)} color={muted()} />
    <Text style={{ color: muted(), fontFamily: theme().family, fontSize: text(11) }}>{props.query.trim() ? "No matches" : `No ${props.noun}`}</Text>
  </View>;
}

function ProjectRow(props: { item: () => ProjectTreeRow; controller: PanelController }) {
  // List recycles a cell across unrelated keys and row kinds. Keep both the
  // branch and every identity reactive instead of capturing the first item.
  return <>{props.item().kind === "empty" ?
    <View style={{ paddingLeft: 12 + props.item().depth * 12, paddingVertical: 7 }}><Text style={{ fontFamily: theme().family, fontSize: text(10), color: muted() }}>Drop projects here</Text></View> :
    props.item().kind === "folder" ? <FolderRow item={props.item} controller={props.controller} /> : <ProjectItemRow item={props.item} controller={props.controller} />}</>;
}

function FolderRow(props: { item: () => ProjectTreeRow; controller: PanelController }) {
  const c = props.controller;
  const current = () => props.item() as Extract<ProjectTreeRow, { kind: "folder" }>;
  const id = () => current().folder?.id ?? "unfiled";
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

function ProjectItemRow(props: { item: () => ProjectTreeRow; controller: PanelController }) {
  const c = props.controller;
  const project = () => (props.item() as Extract<ProjectTreeRow, { kind: "project" }>).project;
  const tasks = () => c.state().tasks.filter((task) => task.projectId === project().id);
  const trees = () => visibleWorktrees(c.state(), project().id);
  const live = () => c.shells().filter((shell) => shell.projectId === project().id && shell.alive).length;
  const selected = () => c.projectId() === project().id;
  const hovering = signal(false);
  const activity = () => {
    const chats = c.chats().filter((chat) => chat.projectId === project().id);
    const working = chats.filter((chat) => chat.state === "running").length, waiting = chats.filter((chat) => chat.state === "waiting").length;
    return [working ? `${working} working` : "", waiting ? `${waiting} waiting` : ""].filter(Boolean).join(" · ");
  };
  async function settings() { await c.activate(project()); if (c.projectId() === project().id) c.openPanel("project_settings"); }
  function openWindow() { void c.run(() => host.call("window.open", { kind: "project", projectId: project().id, path: project().root })); }
  function menu() {
    void Menu.popup([
      { label: "Open project", onSelect: () => void c.activate(project()) },
      { label: "Open in another window", onSelect: openWindow },
      { label: "Project settings…", onSelect: () => void settings() },
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
        style={{ flex: 1, minWidth: 0, paddingLeft: space(6 + props.item().depth * 16, theme().scale), paddingVertical: space(6, theme().scale), gap: 2 }} focusStyle={{ backgroundColor: theme().palette.divider }}>
        <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: text(11), color: ink() }}>{project().name}</Text>
        <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: text(10), color: muted() }}>{`${activity() ? `Agents ${activity()} · ` : ""}${trees().length} trees · ${tasks().filter((task) => task.status === "done").length}/${tasks().length} tasks · ${live()} live`}</Text>
      </Button>
      {(selected() || hovering()) && <View style={{ flexDirection: "row", alignItems: "center", gap: 0 }}>
        <ToolbarCommand id={`project-notifications:${project().id}`} icon={project().notifyOnAgentDone ? "bell" : "bell-off"} label="Agent completion notifications" selected={project().notifyOnAgentDone} disabled={c.pending()} onPress={() => void c.run(() => Store.setProjectNotifications(project().id, !project().notifyOnAgentDone))} />
        <ToolbarCommand id={`project-wakes:${project().id}`} icon="refresh" label="Wake orchestrators when a worker finishes" selected={project().wakeOrchestratorsOnAgentDone} disabled={c.pending()} onPress={() => void c.run(() => Store.setProjectOrchestratorWakes(project().id, !project().wakeOrchestratorsOnAgentDone))} />
        <ToolbarCommand id={`project-settings:${project().id}`} icon="settings" label="Project settings" onPress={() => void settings()} />
        <ToolbarCommand id={`open-project-window:${project().id}`} icon="open" label="Open project in another window" onPress={openWindow} />
      </View>}
    </View>
  </View>;
}

function Projects(props: { controller: PanelController; surface: string; footer?: JSX.Element }) {
  const c = props.controller;
  const rows = () => projectRows(c.state(), c.query(), c.collapsed(), c.order(), c.descending(), c.shells());
  function sortMenu() {
    void Menu.popup([
      ...([["name", "Project name"], ["edited", "Last edited"], ["added", "Date added"], ["live", "Live sessions"]] as const).map(([value, label]) => ({ label, checked: c.order() === value, disabled: c.sortPending(),
        onSelect: () => { if (c.order() !== value) void c.setOrder(value); } })),
      { type: "separator" },
      { label: c.order() === "name" ? "A to Z" : c.order() === "live" ? "Fewest live sessions first" : "Oldest first", checked: !c.descending(), disabled: c.sortPending(), onSelect: () => void c.setOrder(c.order(), false) },
      { label: c.order() === "name" ? "Z to A" : c.order() === "live" ? "Most live sessions first" : "Newest first", checked: c.descending(), disabled: c.sortPending(), onSelect: () => void c.setOrder(c.order(), true) },
    ]);
  }
  return <View testID="projects-panel" style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.panel }}>
    <Header title="Projects" count={c.query().trim() ? rows().filter((row) => row.kind === "project").length : c.state().projects.length} total={c.state().projects.length} noun="project">
      <ToolbarCommand id="sort-projects" icon="sort" label="Sort projects" onPress={sortMenu} />
      <ToolbarCommand id="new-folder" icon="folder" label="New folder" onPress={() => c.editor.set({ kind: "folder", name: "" })} />
      <ToolbarCommand id="new-project" icon="add" label="New project" onPress={() => c.editor.set({ kind: "project" })} />
    </Header>
    <PanelSearch controller={c} panel="projects" surface={props.surface} />
    <List each={rows()} keyExtractor={(item) => item.id} estimatedItemSize={space(46, theme().scale)} style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, theme().scale), paddingBottom: 8 }} footer={props.footer} empty={props.footer ? undefined : <Empty query={c.query()} noun="projects" />}>
      {(item) => <ProjectRow item={item} controller={c} />}
    </List>
  </View>;
}

function Worktrees(props: { controller: PanelController; surface: string }) {
  const c = props.controller;
  const all = () => visibleWorktrees(c.state(), c.projectId());
  const rows = () => all().filter((tree) => matches(c.query(), tree.id, tree.branch, tree.path));
  return <View testID="worktrees-panel" style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.panel }}>
    <Header title="Worktrees" count={rows().length} total={all().length} noun="worktree" project={c.projectLabel()}>
      <ToolbarCommand id="refresh-worktrees" icon="refresh" label="Refresh worktrees" disabled={c.pending()} onPress={() => void c.run(() => Store.refreshWorktrees(c.projectId()))} />
    </Header>
    <PanelSearch controller={c} panel="worktrees" surface={props.surface} />
    <List each={rows()} keyExtractor={(tree) => tree.id} estimatedItemSize={space(72, theme().scale)} style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, theme().scale) }} empty={<Empty query={c.query()} noun="worktrees" />}>
      {(item) => <Button testID={`worktree-${item().id}`} accessibilityLabel={worktreeLabel(item())} selected={c.selectedWorktree() === item().id} onPress={() => c.selectWorktree(item().id)} style={row()} hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.divider }} selectedStyle={{ backgroundColor: theme().palette.panelActive }}>
        <Text style={{ fontFamily: theme().family, fontSize: text(11), color: ink() }}>{`${item().isPrimary ? "▣" : "⑂"} ${worktreeLabel(item())}`}</Text>
        <Text numberOfLines={1} style={{ fontFamily: theme().monoFamily, fontSize: text(10), color: muted() }}>{item().path}</Text>
        {item().present === false && <Text style={{ fontFamily: theme().family, fontSize: text(10), color: theme().palette.gold }}>Missing</Text>}
        <Text style={{ fontFamily: theme().family, fontSize: text(10), color: muted() }}>{`${c.state().tasks.filter((task) => task.worktreeId === item().id && task.status === "done").length}/${c.state().tasks.filter((task) => task.worktreeId === item().id).length} tasks · ${item().id.slice(0, 8)}`}</Text>
      </Button>}
    </List>
  </View>;
}

function TaskDetail(props: { controller: PanelController; task: TaskDto }) {
  const c = props.controller;
  function assign() {
    void Menu.popup([
      { label: "UNASSIGNED", checked: !props.task.worktreeId, onSelect: () => void c.run(() => Store.unassignTasks([props.task.id])) },
      ...c.state().worktrees.filter((tree) => tree.projectId === c.projectId()).map((tree) => ({ label: worktreeLabel(tree), checked: tree.id === props.task.worktreeId,
        onSelect: () => void c.run(() => Store.assignTasks(tree.id, [props.task.id]), () => c.selectWorktree(tree.id)) })),
    ]);
  }
  return <Scroll testID="task-detail" style={{ maxHeight: 200, borderTopWidth: 1, borderTopColor: theme().palette.divider }} contentStyle={{ padding: 10, gap: 7 }}>
    <Text style={{ fontFamily: theme().family, fontSize: text(11), fontWeight: "600", color: ink() }}>Task detail</Text>
    <Text style={{ fontFamily: theme().family, fontSize: text(12), color: ink() }}>{props.task.title}</Text>
    <Text style={{ fontFamily: theme().family, fontSize: text(10), color: statusColor(props.task.status) }}>{`${props.task.status} · @ ${worktreeLabel(c.state().worktrees.find((tree) => tree.id === props.task.worktreeId))}`}</Text>
    <Text selectable={true} style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>{props.task.details || "No details"}</Text>
    <Text selectable={true} style={{ fontFamily: theme().monoFamily, fontSize: text(9), color: muted() }}>{props.task.id}</Text>
    <View style={{ flexDirection: "row", gap: 4, flexWrap: "wrap" }}>
      {(["todo", "in_progress", "done"] as const).map((status) => <Command id={`task-status:${status}`} label={status === "in_progress" ? "In progress" : status === "done" ? "Done" : "Todo"}
        selected={props.task.status === status} disabled={c.pending()} onPress={() => void c.run(() => Store.setTaskStatus(props.task.id, status))} />)}
      <Command id="assign-task" label="Assign task to worktree" disabled={c.pending()} onPress={assign}>Assign…</Command>
    </View>
  </Scroll>;
}

function Tasks(props: { controller: PanelController; surface: string }) {
  const c = props.controller;
  const all = () => c.state().tasks.filter((task) => task.projectId === c.projectId());
  const rows = () => all().filter((task) => taskMatches(task, c.state(), c.query()));
  const selected = () => all().find((task) => task.id === c.selectedTask());
  return <View testID="tasks-panel" style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.panel }}>
    <Header title="Tasks" count={rows().length} total={all().length} noun="task" project={c.projectLabel()}>
      <ToolbarCommand id="new-task" icon="add" label="Add task" disabled={c.pending()} onPress={() => c.editor.set({ kind: "task" })} />
    </Header>
    <PanelSearch controller={c} panel="tasks" surface={props.surface} />
    <List each={rows()} keyExtractor={(task) => task.id} estimatedItemSize={space(46, theme().scale)} style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, theme().scale) }} empty={<Empty query={c.query()} noun="tasks" />}>
      {(item) => <Button testID={`task-${item().id}`} accessibilityLabel={item().title} selected={c.selectedTask() === item().id} onPress={() => c.selectTask(item().id)} style={row()} hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.divider }} selectedStyle={{ backgroundColor: theme().palette.panelActive }}>
        <Text style={{ fontFamily: theme().family, fontSize: text(11), color: ink() }}>{`${taskMark(item().status)} ${item().title}`}</Text>
        <Text style={{ fontFamily: theme().family, fontSize: text(10), color: statusColor(item().status) }}>{`${item().status} · @ ${worktreeLabel(c.state().worktrees.find((tree) => tree.id === item().worktreeId))}`}</Text>
      </Button>}
    </List>
    {selected() && <TaskDetail controller={c} task={selected()!} />}
  </View>;
}

function Shells(props: { controller: PanelController; surface: string }) {
  const c = props.controller;
  const allShells = () => c.shells().filter((shell) => shell.kind === "project" && shell.projectId === c.projectId());
  const allChats = () => c.chats().filter((chat) => chat.projectId === c.projectId());
  const context = (chat: ChatSummary) => {
    const trees = c.state().worktrees.filter((tree) => tree.projectId === chat.projectId);
    const tree = trees.find((tree) => tree.id === chat.worktreeId) ?? trees.filter((tree) => chat.cwd === tree.path || chat.cwd.startsWith(`${tree.path}/`)).sort((a, b) => b.path.length - a.path.length)[0];
    return tree ? worktreeLabel(tree) : chat.worktreeId ? `Worktree ${chat.worktreeId.slice(0, 8)}` : "Project root";
  };
  const provider = (chat: ChatSummary) => chat.provider === "codex" ? "Codex" : chat.provider === "claude" ? "Claude" : "Muse";
  const status = (chat: ChatSummary) => chat.state === "running" ? "Working" : chat.state.charAt(0).toUpperCase() + chat.state.slice(1);
  type SessionRow = { kind: "chat"; chat: ChatSummary; id: string; projectId: string; token: number } | { kind: "shell"; shell: ShellSessionDto; id: string; projectId: string; token: number };
  const rows = (): SessionRow[] => {
    const filter = c.sessionFilter(), query = c.query(), projectId = c.projectId(), token = c.generation();
    const chats: SessionRow[] = allChats().filter((chat) => filter !== "shells" && (filter === "all" || chat.provider === filter) && matches(query, chat.id, chat.title, provider(chat), status(chat), context(chat), chat.cwd, chat.worktreeId))
      .map((chat) => ({ kind: "chat", chat, id: `chat:${chat.id}`, projectId, token }));
    const shells: SessionRow[] = (filter === "all" || filter === "shells" ? allShells() : [])
      .filter((shell) => shellMatches(shell, c.state(), query) || (!!query.trim() && matches(query, c.tabForShell(shell.id)?.title)))
      .map((shell) => ({ kind: "shell", shell, id: `shell:${shell.id}`, projectId, token }));
    return [...chats, ...shells];
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
  function refresh() { void c.refreshCatalog(); void Sessions.list().then((next) => c.shells.set(next.sessions)).catch((reason) => c.notice(String(reason))); }
  return <View testID="shells-panel" style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.panel }}>
    <Header title="Sessions" count={rows().length} total={allShells().length + allChats().length} noun="session" project={c.projectLabel()}>
      <ToolbarCommand id="sessions-refresh" icon="refresh" label="Refresh sessions" disabled={c.catalogLoading()} onPress={refresh}>↻</ToolbarCommand>
      <ToolbarCommand id="new-shell" icon="add" label="New shell" disabled={c.pending()} onPress={createShell}>＋</ToolbarCommand>
    </Header>
    <PanelSearch controller={c} panel="shells" surface={props.surface} />
    <View testID="sessions-controls" style={{ paddingHorizontal: 8, paddingBottom: 8, flexDirection: "row", gap: 4, flexWrap: "wrap" }}>
      {c.newChat && <Command id="sessions-new-chat" label="New chat…" onPress={() => c.newChat?.()} />}
      {([["all", "All sessions"], ["codex", "Codex chats"], ["claude", "Claude chats"], ["muse", "Muse chats"], ["shells", "Shells"]] as const).map(([filter, label]) =>
        <Command id={`sessions-filter-${filter}`} label={label} selected={c.sessionFilter() === filter} onPress={() => c.sessionFilter.set(filter)} />)}
    </View>
    {c.catalogNotice() !== "" && <Text testID="sessions-catalog-notice" style={{ fontFamily: theme().family, fontSize: text(10), color: theme().palette.gold, paddingHorizontal: 12, paddingBottom: 8 }}>{c.catalogNotice()}</Text>}
    {c.catalogLoading() && allChats().length === 0 && <Text style={{ fontFamily: theme().family, fontSize: text(10), color: muted(), paddingHorizontal: 12, paddingBottom: 8 }}>Loading chat history…</Text>}
    <List each={rows()} keyExtractor={(item) => item.id} estimatedItemSize={space(76, theme().scale)} style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, theme().scale), paddingBottom: 6 }} empty={<Empty query={c.query()} noun="sessions" />}>
      {(item) => <>{item().kind === "chat" ? <ChatSession item={() => item() as Extract<SessionRow, { kind: "chat" }>} /> : <ShellSession item={() => item() as Extract<SessionRow, { kind: "shell" }>} />}</>}
    </List>
  </View>;

  function ChatSession(props: { item: () => Extract<SessionRow, { kind: "chat" }> }) {
    const item = props.item, chat = () => item().chat;
    const tone = () => chat().state === "running" ? theme().palette.working : chat().state === "waiting" || chat().state === "failed" ? theme().palette.gold : muted();
    return <Button testID={`session-chat-${chat().id}`} accessibilityLabel={`${provider(chat())} chat · ${chat().title || "Untitled chat"} · ${status(chat())} · ${context(chat())}`}
      selected={c.selectedChat() === chat().id} disabled={!c.showChat} onPress={() => c.openChat(chat(), item().projectId, item().token)}
      style={row()} hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.divider }} selectedStyle={{ backgroundColor: theme().palette.panelActive }}>
      <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: text(11), color: ink() }}>{`${provider(chat())} · ${chat().title.trim() || "Untitled chat"}`}</Text>
      <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: text(10), color: tone() }}>{`${status(chat())} · ${context(chat())}`}</Text>
      <Text numberOfLines={1} style={{ fontFamily: theme().monoFamily, fontSize: text(10), color: muted() }}>{chat().cwd}</Text>
      <Text numberOfLines={1} style={{ fontFamily: theme().monoFamily, fontSize: text(9), color: muted() }}>{chat().id}</Text>
    </Button>;
  }
  function ShellSession(props: { item: () => Extract<SessionRow, { kind: "shell" }> }) {
    const item = props.item, shell = () => item().shell;
    function menu() {
      void Menu.popup([
        { label: "Open session", disabled: !shell().alive, onSelect: () => c.attach(shell(), item().projectId, item().token) },
        { type: "separator" },
        { label: "End shell…", disabled: !shell().alive, onSelect: () => c.closingShell.set(shell()) },
      ]);
    }
    return <View onContextMenu={menu}>
      <Button testID={`shell-${shell().id}`} accessibilityLabel={c.titleForShell(shell())} selected={c.selectedShell() === shell().id} onPress={() => c.attach(shell(), item().projectId, item().token)} style={row()}
        hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.divider }} selectedStyle={{ backgroundColor: theme().palette.panelActive }}>
        <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: text(11), color: ink() }}>{c.titleForShell(shell())}</Text>
        <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: text(10), color: shell().alive ? accent() : muted() }}>{`${shell().alive ? "Live" : "Exited"} · ${shell().harness ?? "shell"}${c.tabForShell(shell().id)?.hidden ? " · Hidden tab" : ""}`}</Text>
        <Text numberOfLines={1} style={{ fontFamily: theme().monoFamily, fontSize: text(10), color: muted() }}>{shell().command ?? shell().cwd}</Text>
        <Text numberOfLines={1} style={{ fontFamily: theme().monoFamily, fontSize: text(9), color: muted() }}>{shell().id}</Text>
      </Button>
      {c.selectedShell() === shell().id && shell().alive && <View style={{ paddingHorizontal: 6, paddingBottom: 4 }}><Command id={`close-shell:${shell().id}`} label="End shell…" disabled={c.pending()} onPress={() => c.closingShell.set(shell())} /></View>}
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
  return <View testID={`shell-intent:${props.shellId}`} style={{ padding: 16, gap: 8 }}>
    <Text style={{ fontFamily: theme().family, fontSize: text(13), color: ink() }}>{shell() ? props.controller.titleForShell(shell()!) : "Shell"}</Text>
    <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>Opening terminal…</Text>
    <Text selectable={true} style={{ fontFamily: theme().monoFamily, fontSize: text(11), color: muted() }}>{props.shellId}</Text>
    {shell() && <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>{`${shell()!.alive ? "Live" : "Exited"} · ${shell()!.cwd}`}</Text>}
  </View>;
}

function FolderEditor(props: { controller: PanelController; editor: Extract<Editor, { kind: "folder" }> }) {
  const c = props.controller, name = signal(props.editor.name), error = signal("");
  function save() {
    if (c.pending()) return;
    error.set("");
    void c.run(async () => {
      try { const folder = props.editor.id ? await Store.renameFolder(props.editor.id, name()) : await Store.createFolder(name(), props.editor.parent); c.expandFolder(folder.id); c.editor.set(null); }
      catch (reason) { error.set(String(reason)); }
    });
  }
  return <View style={{ gap: 10 }}>
    <Text style={{ fontFamily: theme().family, fontSize: text(15), color: ink() }}>{props.editor.id ? "Rename virtual folder" : props.editor.parent ? "New subfolder" : "New virtual folder"}</Text>
    <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>Group projects without moving their files.</Text>
    <Text style={{ fontFamily: theme().family, fontSize: text(10), color: muted() }}>{`PARENT / ${c.folderPath(props.editor.parent)}`}</Text>
    <Input testID="folder-name" accessibilityLabel="Folder name" autoFocus={true} value={name()} onChange={(value) => name.set(value)} onSubmit={save} style={field()} />
    {error() !== "" && <Text style={{ color: theme().palette.gold, fontFamily: theme().family, fontSize: text(11) }}>{error()}</Text>}
    <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 6 }}>
      <Command label="Cancel" disabled={c.pending()} onPress={() => c.editor.set(null)} />
      <Command id="save-folder" label="Save folder" disabled={c.pending()} onPress={save} />
    </View>
  </View>;
}

function TaskEditor(props: { controller: PanelController }) {
  const c = props.controller, title = signal(""), details = signal(""), error = signal("");
  const projectId = c.projectId();
  function save() {
    if (c.pending() || projectId !== c.projectId()) return;
    void c.run(async () => {
      try { const task = await Store.addTask(projectId, title(), details()); c.selectTask(task.id); c.query.set(""); c.editor.set(null); }
      catch (reason) { error.set(String(reason)); }
    });
  }
  return <View style={{ gap: 10 }}>
    <Text style={{ fontFamily: theme().family, fontSize: text(15), color: ink() }}>Add task</Text>
    <Input testID="task-title" accessibilityLabel="Task title" placeholder="Task title" autoFocus={true} value={title()} onChange={(value) => title.set(value)} onSubmit={save} style={field()} />
    <Input testID="task-details" accessibilityLabel="Task details" placeholder="Details (optional)" multiline={true} value={details()} onChange={(value) => details.set(value)} style={{ ...field(), minHeight: 90 }} />
    {error() !== "" && <Text style={{ color: theme().palette.gold, fontFamily: theme().family, fontSize: text(11) }}>{error()}</Text>}
    <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 6 }}>
      <Command label="Cancel" disabled={c.pending()} onPress={() => c.editor.set(null)} />
      <Command id="save-task" label="Add task" disabled={c.pending() || !title().trim()} onPress={save} />
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
    if (followsName()) setPath(`${directory}/${name()}`);
  }).catch((reason: unknown) => { if (alive) error.set(String(reason)); });
  function setPath(value: string) {
    path.set(value); inspection.set(null); error.set(""); checking.set(true);
    const token = ++generation;
    clearTimeout(timer);
    timer = setTimeout(() => {
      void Store.inspectProject(value).then((next) => { if (alive && token === generation) { inspection.set(next); checking.set(false); } })
        .catch((reason: unknown) => { if (alive && token === generation) { error.set(String(reason)); checking.set(false); } });
    }, 250);
  }
  function setName(value: string) { name.set(value); if (followsName() && defaultDirectory()) setPath(`${defaultDirectory()}/${value}`); }
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
  cleanup(keymap.bind("mod+o", () => void browse()));
  cleanup(keymap.bind("mod+g", () => { if (inspection()?.canInitGit && !c.pending()) initGit.set(!initGit()); }));
  return <View style={{ gap: 10 }}>
    <Text style={{ fontFamily: theme().family, fontSize: text(15), color: ink() }}>New project</Text>
    <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>Project name</Text>
    <Input testID="project-name" accessibilityLabel="Project name" placeholder={followsName() ? "my-project" : "Use folder name"} value={name()} readonly={c.pending()} autoFocus={true} onChange={setName} onSubmit={save} style={field()} />
    <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>Folder</Text>
    <View style={{ flexDirection: "row", gap: 6 }}>
      <Input testID="project-path" accessibilityLabel="Project folder" placeholder="/path/to/project" value={path()} readonly={c.pending()} onChange={(value) => { followsName.set(false); setPath(value); }} onSubmit={save} ref={(node) => { pathInput = node; }} style={{ ...field(), fontFamily: theme().monoFamily, flex: 1 }} />
      <Command id="browse-project" label="Browse…" disabled={c.pending()} onPress={() => void browse()} />
    </View>
    <Text testID="project-inspection" style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>{checking() ? "Checking folder…" : inspection() ? `${inspection()!.exists ? "Folder" : "New folder"} · ${inspection()!.repositoryCount ? `${inspection()!.repositoryCount} Git repositories` : "no repositories"}` : "Enter a project name, or choose an existing folder."}</Text>
    {inspection() && <Text numberOfLines={2} style={{ fontFamily: theme().family, fontSize: text(10), color: muted() }}>{`Folder: ${inspection()!.root}`}</Text>}
    {!!inspection()?.warning && <Text style={{ fontFamily: theme().family, fontSize: text(11), color: theme().palette.gold }}>{inspection()!.warning}</Text>}
    {inspection()?.canInitGit ? <Switch testID="project-init-git" accessibilityLabel="Initialize Git" checked={initGit()} onChange={(value) => initGit.set(value)} disabled={c.pending()} style={{ flexDirection: "row", alignItems: "center", gap: 8 }}
      trackStyle={{ width: 28, height: 16, borderRadius: 8, padding: 2, alignItems: initGit() ? "flex-end" : "flex-start", backgroundColor: initGit() ? accent() : theme().palette.divider }} thumbStyle={{ width: 12, height: 12, borderRadius: 6, backgroundColor: theme().palette.bg }}>
      <Text style={{ fontFamily: theme().family, fontSize: text(11), color: ink() }}>Initialize Git</Text>
    </Switch> : inspection() && <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>Use existing repositories</Text>}
    {error() !== "" && <Text testID="project-error" style={{ fontFamily: theme().family, fontSize: text(11), color: theme().palette.gold }}>{error()}</Text>}
    <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 6 }}>
      <Command label="Cancel" disabled={c.pending()} onPress={() => c.editor.set(null)} />
      <Command id="create-project" label={c.pending() ? "Creating…" : "Create project"} disabled={c.pending() || checking() || !inspection()} onPress={save} />
    </View>
  </View>;
}

export function PanelEditors(props: { controller: PanelController }) {
  return <PanelTheme.Provider value={props.controller.theme ?? defaultTheme}><EditorDialog controller={props.controller} /></PanelTheme.Provider>;
}

function EditorDialog(props: { controller: PanelController }) {
  const c = props.controller;
  return <>
    {c.closingShell() && <Dialog testID="end-shell-dialog" animation="none" closeOnEscape={!c.pending()} closeOnBackdropPress={false} onOpenChange={(open) => { if (!open && !c.pending()) c.closingShell.set(null); }}
      style={{ width: 380, maxWidth: "90%", padding: 18, gap: 12, backgroundColor: theme().palette.panel, borderRadius: 12 }} backdropStyle={{ backgroundColor: "#00000080" }}>
      <Text style={{ fontFamily: theme().family, fontSize: text(14), fontWeight: "600", color: ink() }}>End this shell?</Text>
      <Text style={{ fontFamily: theme().family, fontSize: text(11), color: muted() }}>{`This ends ${c.titleForShell(c.closingShell()!)} and the processes running in it. Close its tab instead to keep it running.`}</Text>
      <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 6 }}>
        <Command label="Cancel" disabled={c.pending()} onPress={() => c.closingShell.set(null)} />
        <Command id="confirm-end-shell" label="End shell" disabled={c.pending()} onPress={() => { const shell = c.closingShell(); if (shell) void c.run(() => Sessions.close(shell.id), () => c.closingShell.set(null)); }} />
      </View>
    </Dialog>}
    {c.editor() && <Dialog testID="panel-editor" animation="none" closeOnEscape={!c.pending()} closeOnBackdropPress={false} onOpenChange={(open) => { if (!open && !c.pending()) c.editor.set(null); }}
    style={{ width: 440, maxWidth: "90%", padding: 18, backgroundColor: theme().palette.panel, borderRadius: 8 }} backdropStyle={{ backgroundColor: "#00000080" }}>
    {c.editor()!.kind === "folder" ? <FolderEditor controller={c} editor={c.editor() as Extract<Editor, { kind: "folder" }>} /> :
      c.editor()!.kind === "task" ? <TaskEditor controller={c} /> : <ProjectEditor controller={c} />}
  </Dialog>}</>;
}
