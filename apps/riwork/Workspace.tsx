import { AppPreviewTab } from "./AppPreviewTab";
import { CuaSetup, useCua, type CuaSetupControl } from "./CuaSetup";
import { Clipboard, Overlay, Scroll, Text, View, cleanup, colorScheme, computed, createAnimatedValue, effect, host, keymap, signal, type Point, type YNode } from "nsh";
import { Button, Dialog, Input, Popover, type DockAreaLayout, type DockAreaRef, type DockNode } from "nsh/base";
import { Menu, Window, type MenuItem } from "nsh/desktop";
import { Appearance, Orchestrators, Remote, Sessions, Tabs, type AppearanceDto, type LayoutSession, type SettingsDto } from "riwork/native";
import { DockArea } from "./DockArea";
import { CloseDialog } from "./CloseDialog";
import { DeferredPanel, MountProbe } from "./DeferredPanel";
import { LocalTerminal, type TerminalColors, type TerminalController, type TerminalEvents } from "./Terminal";
import { PanelContent, PanelEditors, createPanelController, type PanelController } from "./Panels";
import { WindowChrome, useWindowChrome, topPaneFrames, CONTROL_INSET } from "./WindowChrome";
import { workspaceCommands } from "./commands";
import { STATUS_COLORS, WORKSPACE_METRICS as M, headerHeight, mainMarkerThreshold, revealOffset, sessionCellWidth } from "./metrics";
import {
  activePane, closeChoice, createDockUpdater, createTitleUpdater, cycledTab, findPanel, numberedTab,
  panelTitle, stripTabs, windowTitle, type PanelModel, type PanelStatus, type WorkspaceSnapshot,
} from "./model";
import { DEFAULT_THEME, space, type InterfaceTheme } from "./appearance";
import { StatusBar } from "./StatusBar";
import { ChatTab } from "./chat/ChatTab";
import { createFilesController, FilesPanel, PreviewPanel } from "./Files";
import { AutomationsPanel } from "./schedules/Automations";
import { createRemoteController, RemoteProjects, RemoteProjectPanel, RemotePrompt, RemoteSettings, RemoteUnavailable, type RemoteApi } from "./remote/Remote";
import { RemoteTerminal } from "./remote/RemoteTerminal";
import { remoteHostLabel, remoteLocalOnly, remotePanelUnavailable, remoteProjectKey, remoteTabTitle, remoteTitle, splitRemoteTitle } from "./remote/model";
import { workspaceRemote, type WorkspaceRemoteApi } from "./remote/workspace";
import { AppSettings } from "./settings/AppSettings";
import { ProjectSettingsPanel } from "./settings/ProjectSettings";

type Extent = { width: number; height: number };
interface WorkspaceProps {
  session: LayoutSession;
  title: string;
  projectLabel: string;
  projectId?: string;
  settings?: () => SettingsDto | null;
  theme?: () => InterfaceTheme;
  appearanceNotice?: () => string;
  modalOpen?: () => boolean;
  /** Headless fixture diagnostics; omitted by production windows. */
  diagnostics?: boolean;
  defaultProjectDirectory?: string;
  startupNotice?: string | null;
  /** In-process observer used by the headless fixture, never a host API. */
  onChange?: (snapshot: WorkspaceSnapshot, dock: DockAreaLayout) => void;
  /** Observe native-node creation in the headless lifecycle regression. */
  onTerminalMount?: (shellId: string) => void;
  /** In-process geometry observer for headless gesture fixtures. */
  onViewRef?: (id: string, node: YNode) => void;
}

function panelIds(node: DockNode): string[] {
  return node.type === "tabs" ? node.panels : node.children.flatMap(panelIds);
}

const NAVIGATION_PANELS = ["projects", "worktrees", "tasks", "shells"];
const VIEWS = ["projects", "files", "preview", "worktrees", "tasks", "shells", "usage", "schedules", "dashboard", "project_settings", "settings"];
const VIEW_KEYS: Record<string, string> = { projects: "⌘B", files: "⌘⇧E", preview: "⌘⇧P", shells: "⌘⌥S", schedules: "⌘⇧S", dashboard: "⌘⌥G", project_settings: "⌘⌥A", settings: "⌘," };
const VIEW_SHORTCUTS: Record<string, string> = { files: "mod+shift+e", preview: "mod+shift+p", schedules: "mod+shift+s", dashboard: "mod+alt+g", settings: "mod+," };
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

function StatusDot(props: { status?: PanelStatus | null }) {
  const status = () => props.status;
  return <>{status() && <View style={{ width: M.statusDot, height: M.statusDot, justifyContent: "center", alignItems: "center" }}>
    <View style={{ width: status() === "idle" ? M.idleDot : M.statusDot, height: status() === "idle" ? M.idleDot : M.statusDot, borderRadius: 4,
      borderWidth: status() === "stopped" ? 1 : 0, borderColor: STATUS_COLORS[status()!], backgroundColor: status() === "stopped" ? "transparent" : STATUS_COLORS[status()!] }} />
  </View>}</>;
}

export function Workspace(props: WorkspaceProps) {
  const session = props.session;
  const snapshot = signal(session.snapshot() as WorkspaceSnapshot);
  const dock = signal<DockAreaLayout | null>(null);
  // Keep the DockArea subtree mounted as its controlled layout changes. Using
  // dock() as the conditional value would reconstruct it for every projection.
  const dockReady = signal(false);
  const notice = signal([props.startupNotice, snapshot().notice].filter(Boolean).join("\n"));
  const theme = props.theme ?? (() => DEFAULT_THEME);
  effect(() => { const next = props.appearanceNotice?.(); if (next) notice.set(next); });
  const textStyle = (base = 11) => ({ fontFamily: theme().family, fontSize: base * theme().scale, color: theme().palette.text });
  const mutedStyle = (base = 11) => ({ ...textStyle(base), color: theme().palette.muted });
  let root: YNode | null = null;
  function dismissNotice() { notice.set(""); root?.focus(); }
  const projectId = computed(() => snapshot().projectId ?? props.projectId ?? "");
  // Gate subtree construction on a stable primitive. Watching the whole layout
  // here would reconstruct local terminals on every unrelated delivery.
  const remoteProject = computed(() => !!snapshot().remoteProject);
  const switching = signal(false);
  const appearance = signal<AppearanceDto | null>(null);
  const pendingFocus = signal<string | null>(null);
  const previewFocus = signal<string | null>(null);
  const busy = signal(false);
  const statusMenu = signal(false);
  // Workspace transient UI: the close dialog, rename, the shared-strip popover,
  // native menus and drags hold session deliveries and freeze native terminals.
  const closing = signal<PanelModel | null>(null);
  const renaming = signal<PanelModel | null>(null);
  const renameTitle = signal("");
  const vimDialog = signal(false);
  const vimPath = signal("");
  let vimInput: YNode | undefined;
  const allTabs = signal(false);
  const allTabsAvailable = signal(false);
  const allIndex = signal(0);
  const menuOpen = signal(false);
  const dragging = signal(false);
  const stripWidth = signal(0);
  const width = signal(1280);
  const containerHeight = signal(0);
  const hovered = signal<string | null>(null);
  const tabHint = signal<{ id: string; text: string } | null>(null);
  const dragTab = signal<PanelModel | null>(null);
  const dragX = createAnimatedValue(0);
  const dragY = createAnimatedValue(0);
  let pressOrigin: Point = { x: 0, y: 0 };
  const dragOrigin = signal<Point>({ x: 0, y: 0 });
  let dockRef: DockAreaRef | undefined;
  let stripRef: YNode | undefined;
  let renameInput: YNode | undefined;
  let scrollOffset = 0;
  const bodyRefs = new Map<string, YNode>();
  const terminalControls = new Map<string, TerminalController>();
  const tabRefs = new Map<string, YNode>();
  let alive = true;
  cleanup(() => { alive = false; bodyRefs.clear(); terminalControls.clear(); tabRefs.clear(); });
  // Panels own the shared session inventory; appearance supplies terminal colors.
  if (props.projectId) {
    const subscription = Appearance.subscribe((event) => appearance.set(event.data));
    cleanup(() => subscription.close());
  }
  let size: Extent | undefined;
  const updateTitle = createTitleUpdater((title) => Window.setTitle(title));
  const panelState: PanelController = createPanelController({
    projectId,
    projectLabel: () => panelState.state().projects.find((project) => project.id === projectId())?.name ?? props.projectLabel,
    notice: (message) => notice.set(message),
    defaultProjectDirectory: props.defaultProjectDirectory,
    theme,
    initialWorktree: snapshot().selectedWorktreeId,
    initialTask: snapshot().selectedTaskId,
    openPanel: (id) => action(() => session.openPanel(id)),
    showShell: (id) => { void openShell(id); },
    selectWorktree: (id) => { try { session.setSelectedWorktree(id); snapshot.set(session.snapshot() as WorkspaceSnapshot); } catch (error) { notice.set(String(error)); } },
    selectTask: (id) => { try { session.setSelectedTask(id); snapshot.set(session.snapshot() as WorkspaceSnapshot); } catch (error) { notice.set(String(error)); } },
    switchProject: (project) => switchTo(project.id),
  });
  const remote = createRemoteController({
    api: Remote as unknown as RemoteApi, theme, copyText: (text) => Clipboard.writeText(text),
    notice: (message) => notice.set(message), modalBlocked: () => modal() || !!panelState.editor() || switching(),
    beforePrompt: () => { allTabs.set(false); commands.stripOpen.set(false); freeze(); },
    afterPrompt: () => thaw(),
    wants: () => ({ order: panelState.order(), descending: panelState.descending(), folders: isVisible("panel:projects"), settings: isVisible("panel:settings"),
      collapsed: [...panelState.collapsed()], projectKey: remoteProjectKey(projectId()) ? projectId() : null,
      worktrees: isVisible("panel:worktrees"), tasks: isVisible("panel:tasks"), shells: isVisible("panel:shells"),
      tabHosts: [...new Set(snapshot().panes.flatMap((pane) => pane.tabs.filter((tab) => tab.kind === "remote_shell" && isVisible(tab.id)).map((tab) => tab.id.split(":")[1])))],
      tabs: snapshot().panes.flatMap((pane) => pane.tabs.filter((tab) => tab.kind === "remote_shell").map((tab) => tab.id)),
    }),
  });
  // As GPUI: a window opened (or restored) on another Mac's project records it
  // for the next plain launch.
  if (remoteProjectKey(props.projectId)) void Remote.select(props.projectId!).catch(() => {});
  const remoteActions = workspaceRemote({ api: Remote as unknown as WorkspaceRemoteApi, session, projectId, snapshot,
    data: remote.data, selectedWorktree: panelState.selectedWorktree, alive: () => alive, generation: panelState.generation,
    install, focus: (key) => pendingFocus.set(key), notice: (message) => notice.set(message) });
  function localOnly() {
    const refusal = remoteLocalOnly(remote.data(), projectId());
    if (refusal) notice.set(refusal);
    return !!refusal;
  }
  const cua = useCua();
  const cuaFocus = signal<CuaSetupControl | null>(null);
  let cuaStartupChecked = false;
  effect(() => {
    const status = cua.state().status;
    if (!status || cuaStartupChecked) return;
    cuaStartupChecked = true;
    if (!status.installed) action(() => session.openCuaSetup());
  });
  function newAppPreview() {
    if (localOnly()) return;
    const cwd = filesRoot();
    if (!cwd) { notice.set("App previews require a local project folder."); return; }
    action(() => session.openAppPreview({ command: "", cwd, mode: "cooperative" }));
  }
  function filesRoot() {
    if (snapshot().remoteProject || !panelState.loaded()) return null;
    const treeId = panelState.selectedWorktree();
    if (treeId) {
      const tree = panelState.state().worktrees.find((tree) => tree.id === treeId && tree.projectId === projectId());
      return tree?.present ? tree.path : null;
    }
    return panelState.state().projects.find((project) => project.id === projectId())?.root ?? null;
  }
  const fileState = createFilesController({
    root: filesRoot,
    label: () => {
      const tree = panelState.state().worktrees.find((tree) => tree.id === panelState.selectedWorktree() && tree.projectId === projectId());
      return tree ? `${panelState.projectLabel()} · ${tree.branch}` : panelState.projectLabel();
    },
    theme,
    visible: () => isVisible("panel:files") || isVisible("panel:preview"),
    active: () => {
      if (closing() || renaming() || vimDialog() || remote.prompt() || switching() || panelState.editor()) return null;
      const id = activePane(snapshot())?.activeTab;
      return id === "panel:files" ? "files" : id === "panel:preview" ? "preview" : null;
    },
    previewRequested: (asked) => action(() => session.revealPanel("preview", !!asked, !!asked || (props.settings?.()?.openPreviewOnSelect ?? true))),
    edit: (entry, line) => openEditor(entry.path, line, undefined, entry.identity),
    open: async (entry) => { await host.call("system.openWith", { path: entry.path }); },
    reveal: async (path) => { await host.call("system.revealInFinder", { path }); },
    closePreview: () => action(() => session.closePanel("panel:preview", false)),
  });
  effect(() => updateTitle(remoteTitle(remote.data(), projectId()) ? `RiWork · ${remoteTitle(remote.data(), projectId())}` : panelState.state().projects.some((project) => project.id === projectId()) ? windowTitle(panelState.projectLabel()) : props.title));
  // The Dock label waits for stored project names and follows project switches.
  const updateDock = createDockUpdater((info) => host.post("window.setDockInfo", info));
  effect(() => updateDock(panelState.loaded() || !!snapshot().remoteProject, remoteTitle(remote.data(), projectId()) ?? panelState.state().projects.find((project) => project.id === projectId())?.name ?? props.projectLabel));

  const shared = () => Array.isArray(snapshot().sharedTabs) && !snapshot().remoteProject;
  const selected = () => activePane(snapshot())?.activeTab;
  let followedShell: string | undefined;
  effect(() => {
    const id = selected();
    const shell = id?.startsWith("shell:") ? panelState.shells().find((shell) => shell.id === id.slice(6) && shell.projectId === projectId()) : undefined;
    const key = shell ? `${projectId()}:${shell.id}:${shell.cwd}` : undefined;
    if (key === followedShell) return;
    followedShell = key;
    if (!shell) return;
    const tree = panelState.state().worktrees.filter((tree) => tree.projectId === projectId() &&
      (shell.cwd === tree.path || shell.cwd.startsWith(`${tree.path}/`))).sort((a, b) => b.path.length - a.path.length)[0];
    const next = tree?.id ?? shell.worktreeId;
    if (next && next !== panelState.selectedWorktree()) panelState.selectWorktree(next);
  });
  function modal(): boolean { return !!closing() || !!renaming() || vimDialog() || !!remote.prompt(); }
  const frozen = () => modal() || menuOpen() || dragging() || allTabs();
  const ordered = () => stripTabs(snapshot());
  const sessions = () => ordered().filter((tab) => tab.kind !== "panel");
  const header = () => headerHeight(11 * theme().scale);
  const totalWidth = () => ordered().reduce((sum, tab) => sum + (tab.kind === "panel" ? M.panelCell : sessionCellWidth(tab.title)), 0) + 48;
  const overflow = () => totalWidth() > stripWidth();
  effect(() => allTabsAvailable.set(overflow() || allTabs()));
  effect(() => {
    const id = hovered();
    const tab = id ? findPanel(snapshot(), id) : undefined;
    tabHint.set(null);
    if (!tab) return;
    const key = tab.kind === "panel" ? VIEW_KEYS[tab.id.slice(6)] : undefined;
    // Panel tooltips keep their natural titles; shells use their session names.
    const text = tab.kind === "panel" ? tab.title : displayTitle(tab);
    const timer = setTimeout(() => tabHint.set({ id: tab.id, text: `${text}${key ? ` · ${key}` : ""}` }), 500);
    cleanup(() => clearTimeout(timer));
  });

  function terminalColors(): TerminalColors {
    const value = appearance();
    // A null projection follows Ghostty in the native terminal component.
    return ((value ? colorScheme() === "dark" ? value.terminalDark : value.terminalLight : null) ?? { source: "ghostty" }) as TerminalColors;
  }
  function isVisible(id: string): boolean {
    return snapshot().panes.some((pane) => pane.activeTab === id && (!snapshot().focusMode || pane.id === snapshot().activePane));
  }
  /** Shell titles prefer shared membership, then the session inventory. */
  function displayTitle(tab: PanelModel): string {
    if (tab.kind === "panel") return tab.title;
    if (tab.kind === "remote_shell") return remoteTabTitle(remote.data(), tab.id);
    if (!tab.id.startsWith("shell:")) return tab.title;
    const shell = panelState.shells().find((item) => item.id === tab.id.slice(6));
    return panelState.tabForShell(tab.id.slice(6))?.title || (shell ? panelState.titleForShell(shell) : tab.title);
  }
  /** Another Mac's tabs wear its name in the remote accent. */
  function remoteMark(tab: PanelModel, title: string) {
    const parts = tab.kind === "remote_shell" ? splitRemoteTitle(title) : null;
    return parts ? [<Text style={{ color: theme().palette.magenta }}>{parts[0]}</Text>, parts[1]] : title;
  }
  function membershipProject(id: string): string | null {
    // Locked panes can carry a shell from the previous project. Its shared
    // membership remains in its owning project, while placement stays local.
    const project = panelState.shells().find((shell) => shell.id === id)?.projectId ?? projectId();
    // Legacy layout fixtures have opaque project keys and no shared membership.
    return UUID.test(project) ? project : null;
  }
  async function openShell(id: string) {
    if (switching()) return;
    const project = projectId(), token = panelState.generation(), membership = membershipProject(id);
    try {
      if (membership && !findPanel(snapshot(), `shell:${id}`)) {
        await Tabs.update(membership, { action: "unhide", key: `shell:${id}` });
      }
      if (alive && project === projectId() && token === panelState.generation()) { install(session.openShell(id)); pendingFocus.set(id); }
    } catch (error) { if (alive) notice.set(String(error)); }
  }
  /** ⌘⌥O and P·ORCH: the project's one orchestrator, found or made off-thread
   * as the project or Settings say (SHELL-217..219). A second request waits. */
  const orchestratorBusy = signal(false);
  async function openProjectOrchestrator() {
    const project = projectId();
    if (remoteProjectKey(project)) { if (!orchestratorBusy() && !switching() && !modal()) { orchestratorBusy.set(true); try { await remoteActions.orchestrator(); } catch (error) { if (alive) notice.set(String(error).replace(/^(?:Error: )?(?:invalid|io|refused|broken|unavailable): /u, "")); } finally { orchestratorBusy.set(false); } } return; }
    if (orchestratorBusy() || switching() || modal() || !UUID.test(project)) return;
    orchestratorBusy.set(true);
    try {
      const made = await Orchestrators.start(project);
      if (!alive || project !== projectId()) return;
      if (made.fallback) notice.set(made.fallback);
      if (made.kind === "chat" && made.chatId) install(session.openChat(made.chatId));
      else if (made.shellId) await openShell(made.shellId);
    } catch (error) { if (alive) notice.set(String(error).replace(/^Error: /, "")); }
    finally { if (alive) orchestratorBusy.set(false); }
  }
  function openGlobalOrchestrator() {
    void host.call("window.open", { kind: "global", view: "chat", start: true, activate: true }).catch((error: unknown) => notice.set(String(error)));
  }
  /** One local-shell creation path: the layout session creates and places it. */
  async function createShell(split?: "right" | "down", kind = "shell") {
    if (busy() || switching() || panelState.pending() || modal()) return;
    busy.set(true);
    const before = new Set(snapshot().panes.flatMap((pane) => pane.tabs.map((tab) => tab.id)));
    const tree = panelState.state().worktrees.find((tree) => tree.id === panelState.selectedWorktree() && tree.projectId === projectId());
    try {
      if (remoteProjectKey(projectId()) && !split) { await remoteActions.create(kind); return; }
      install(await session.newShell({ ...(split ? { split } : {}), ...(tree ? { worktreeId: tree.id, cwd: tree.path } : {}) }));
      const created = snapshot().panes.flatMap((pane) => pane.tabs).find((tab) => tab.kind === "shell" && !before.has(tab.id));
      if (created) pendingFocus.set(created.id.slice(6));
    } catch (error) { if (alive) notice.set(String(error)); }
    finally { if (alive) busy.set(false); thaw(); }
  }
  async function createSubshell(parentShellId = selected()?.startsWith("shell:") ? selected()!.slice(6) : undefined) {
    if (localOnly()) return;
    if (!parentShellId || busy() || switching() || modal()) return;
    busy.set(true);
    const project = projectId(), generation = panelState.generation();
    try {
      const shell = await Sessions.createSubshell({ projectId: project, parentShellId });
      if (alive && projectId() === project && generation === panelState.generation()) {
        await openShell(shell.id);
      }
    } catch (error) { if (alive) notice.set(String(error)); }
    finally { if (alive) busy.set(false); thaw(); }
  }
  async function openEditor(path: string, line?: number, worktreeId?: string, identity?: string) {
    if (localOnly()) return;
    if (busy() || switching() || modal()) return;
    busy.set(true);
    const project = projectId(), generation = panelState.generation();
    const selectedTree = panelState.selectedWorktree();
    const tree = worktreeId ?? selectedTree ?? undefined;
    try {
      const shell = await Sessions.openEditor({ projectId: project, worktreeId: tree, path, line, identity });
      if (alive && projectId() === project && generation === panelState.generation() && selectedTree === panelState.selectedWorktree()) {
        install(session.openEditorShell(shell.id)); pendingFocus.set(shell.id);
      } else if (alive && projectId() === project) {
        notice.set("The selected worktree changed. Select the file again.");
      }
    } catch (error) { if (alive) notice.set(String(error)); }
    finally { if (alive) busy.set(false); thaw(); }
  }
  function showVimDialog() { if (!localOnly()) { vimDialog.set(true); freeze(); } }
  function submitVim() {
    const path = vimPath();
    if (!path) return;
    vimDialog.set(false);
    void openEditor(path);
  }
  function find(id: string, args?: { query?: string }): boolean {
    const active = selected();
    const terminal = active ? terminalControls.get(active) : undefined;
    if (terminal) {
      if (id === "find") terminal.find(args?.query);
      else if (id === "findNext") terminal.findNext();
      else if (id === "findPrevious") terminal.findPrevious();
      else if (id === "findClose") terminal.findClose();
      return true;
    }
    if (id === "find" && active) {
      if (active === "panel:files" || active === "panel:preview") fileState.searches.get("panel:files")?.focus();
      else panelState.searches.get(active)?.focus();
      return true;
    }
    return false;
  }
  function openLink(event: TerminalEvents["linkOpen"]) {
    if (event.action === "edit" && event.kind === "file") { void openEditor(event.target, event.line); return; }
    const root = filesRoot();
    if (root && (event.target === root || event.target.startsWith(`${root}/`))) {
      if (event.kind === "folder") action(() => session.revealPanel("files", true, true));
      else action(() => session.revealPanel("preview", true, true));
      void fileState.reveal(event.target, event.line);
      return;
    }
    const call = event.kind === "folder" ? "system.revealInFinder" : "system.openWith";
    void host.call(call, { path: event.target }).catch((error: unknown) => notice.set(String(error)));
  }
  async function switchTo(id: string) {
    if (switching()) throw new Error("conflict: project switch is already in progress");
    closing.set(null); renaming.set(null); vimDialog.set(false); allTabs.set(false);
    switching.set(true);
    const from = projectId();
    try {
      const next = await session.switchProject(id);
      if (!alive) return;
      install(size ? session.setExtent(size) : next);
      panelState.resetProject({ worktree: snapshot().selectedWorktreeId ?? null, task: snapshot().selectedTaskId ?? null });
      // As GPUI: another Mac's project is recorded for the next plain launch; back
      // on this Mac's projects the record is cleared, by the window that left it.
      if (id !== from && (remoteProjectKey(id) || remoteProjectKey(from))) {
        void Remote.select(remoteProjectKey(id) ? id : null).catch((error) => { if (alive) notice.set(String(error)); });
      }
      pendingFocus.set(null);
    } finally { switching.set(false); }
  }

  const chrome = useWindowChrome();
  const measured = signal<Extent>({ width: 0, height: 0 });
  const tabWidths = signal<Record<string, number>>({});
  function measureTab(group: number, panel: string, width: number) {
    const key = `${group}:${panel}`;
    if (tabWidths()[key] === width) return;
    tabWidths.set({ ...tabWidths(), [key]: width });
  }
  function chromePanes() {
    if (!dock() || shared()) return [];
    return topPaneFrames(dock()!.center, measured()).map((pane) => {
      const tabs = snapshot().panes.find((candidate) => candidate.id === pane.id)?.tabs ?? [];
      const widths = tabs.filter((tab) => !tab.id.startsWith("empty:")).map((tab) => tabWidths()[`${pane.id}:${tab.id}`]);
      // Until layout measures every tab, expose only the reserved empty area.
      // Never cover a clickable tab while its width is still unknown.
      const tabsEnd = widths.every((width) => width !== undefined)
        ? widths.reduce((total, width) => total + width, 0) + (pane.x === 0 && !chrome.fullscreen() ? CONTROL_INSET : 0)
        : undefined;
      // The fixed pane-control suffix stays clickable, outside the drag region.
      return { ...pane, width: Math.max(0, pane.width - controlsWidth(pane.id ?? 0)), tabsEnd };
    });
  }

  function focusActive() {
    const id = selected();
    if (id) bodyRefs.get(id)?.focus();
  }
  function reveal() {
    if (!shared()) return;
    let x = 48;
    for (const tab of ordered()) {
      const cellWidth = tab.kind === "panel" ? M.panelCell : sessionCellWidth(tab.title);
      if (tab.id === selected()) {
        scrollOffset = revealOffset(x, cellWidth, Math.max(0, stripWidth() - (overflow() ? 28 : 0)), scrollOffset);
        stripRef?.scrollTo({ x: scrollOffset, y: 0, animated: false });
        break;
      }
      x += cellWidth;
    }
  }
  function install(next: unknown) {
    // Only session results may be installed. The reported DockArea tree is never
    // assigned to this signal; all gestures are replayed through riwork-core.
    if (!alive || !next) return;
    dock.set(next as DockAreaLayout);
    snapshot.set(session.snapshot() as WorkspaceSnapshot);
    // YNode refs own their subtree in NSH. Keep refs for every placed panel,
    // including hidden tabs and moves, but release removed panels promptly so
    // their native component can be collected while siblings remain mounted.
    const placed = new Set(panelIds(dock()!.center));
    for (const id of bodyRefs.keys()) if (!placed.has(id)) bodyRefs.delete(id);
    for (const id of terminalControls.keys()) if (!placed.has(id)) terminalControls.delete(id);
    for (const key of tabRefs.keys()) if (!placed.has(key.slice(key.indexOf(":") + 1))) tabRefs.delete(key);
    if (snapshot().notice) notice.set(snapshot().notice!);
    if (!dockReady()) dockReady.set(true);
    if (snapshot().focusMode || !shared()) allTabs.set(false);
    props.onChange?.(snapshot(), dock()!);
    reveal();
  }
  function thaw() {
    if (alive && !frozen() && !switching()) install(session.setInteraction(false));
    focusActive();
  }
  function freeze() { session.setInteraction(true); }
  function action(run: () => unknown) {
    if (switching()) return;
    try {
      const result = run();
      if (result && typeof (result as Promise<unknown>).then === "function") {
        void (result as Promise<unknown>).then(install).catch((error) => { if (alive) notice.set(String(error)); });
      } else install(result);
    } catch (error) { notice.set(String(error)); }
  }
  function select(id: string) { hovered.set(null); allTabs.set(false); action(() => session.selectPanel(id)); focusActive(); thaw(); }
  function edit(next: DockAreaLayout) {
    if (!size || switching()) return;
    dragging.set(false);
    try { install(session.applyDock(next, size)); }
    catch (error) { notice.set(String(error)); install(session.dock(size)); }
    thaw();
  }
  function measure(next: Extent) {
    if (next.width <= 0 || next.height <= 0) return;
    if (size?.width === next.width && size?.height === next.height) return;
    const first = !size;
    size = next;
    measured.set(next);
    action(() => first ? session.dock(next) : session.setExtent(next));
  }
  function close(id = selected()) {
    hovered.set(null);
    if (!id || switching()) return;
    const pane = snapshot().panes.find((pane) => pane.tabs.some((tab) => tab.id === id));
    if (pane?.locked) { notice.set("Unlock the pane to close its tabs"); return; }
    const tab = findPanel(snapshot(), id);
    if (!tab) return;
    allTabs.set(false);
    commands.stripOpen.set(false);
    const choice = closeChoice(snapshot(), tab);
    if (choice === "ask") { closing.set(tab); freeze(); }
    else action(() => session.closePanel(id, choice === "exit"));
  }
  function closePane(id = snapshot().activePane) {
    allTabs.set(false);
    if (snapshot().panes.find((pane) => pane.id === id)?.locked) { notice.set("Unlock the pane to close it"); return; }
    action(() => session.closePane(id));
  }
  function popup(items: MenuItem[]) {
    hovered.set(null);
    menuOpen.set(true);
    freeze();
    void Menu.popup(items).finally(() => { menuOpen.set(false); thaw(); });
  }
  function view(panel: string) {
    if (panel === "dashboard") {
      void host.call("window.open", { kind: "global", view: "dashboard", start: false }).catch((error) => notice.set(String(error)));
    } else action(() => session.openPanel(panel));
  }
  let settingsControl: { revealApiKey: () => void } | undefined;
  let projectSettingsControl: { revealMemo: () => void } | undefined;
  let revealSettingsKey = false, revealProjectMemo = false;
  function paneWidth(id: number) {
    const nodeWidth = (node: DockNode, available: number): number | undefined => {
      if (node.type === "tabs") return node.id === id ? available : undefined;
      const fixed = (node.sizes ?? []).reduce((sum: number, value) => sum + (value ?? 0), 0);
      const flexible = (node.sizes ?? []).filter((value) => value === null).length;
      for (let i = 0; i < node.children.length; i++) {
        const w = node.axis === "horizontal" ? (node.sizes?.[i] ?? Math.max(0, available - fixed - M.divider * Math.max(0, node.children.length - 1)) / Math.max(1, flexible)) : available;
        const found = nodeWidth(node.children[i], w);
        if (found !== undefined) return found;
      }
    };
    return dock() ? nodeWidth(dock()!.center, size?.width ?? width()) ?? width() : width();
  }
  function headerWidth(id: number) {
    const pane = paneWidth(id);
    const inset = id === snapshot().panes[0]?.id ? Math.min(M.trafficLightInset, Math.max(0, pane - M.menuButton)) : 0;
    return (pane - inset) / (header() / M.header);
  }
  function controlsWidth(id: number) {
    const pane = snapshot().panes.find((pane) => pane.id === id);
    const width = headerWidth(id);
    const button = M.nativeButton + M.buttonGap;
    return M.menuButton + (width >= M.lockAt ? button : 0) + (width >= M.focusAt ? button : 0)
      + (pane?.main && width >= mainMarkerThreshold(snapshot().theme, snapshot().panelTabIcons) ? button : 0);
  }
  /** A chat tab, or a pending one still being created. It follows its chat
   * only once first shown, so restored background tabs never subscribe at
   * launch (CHATT-015); hidden tabs keep their composer alive (CHATC-051). */
  function ChatPanel(chatProps: { panelId: string; title: string }) {
    const chatId = signal<string | null>(null);
    const pending = signal(false);
    const seen = signal(false);
    effect(() => {
      snapshot();
      const saved = session.panel(chatProps.panelId) as { chat_id?: string } | null;
      const id = saved?.chat_id ?? "";
      pending.set(id.startsWith("pending:"));
      const next = UUID.test(id) ? id : null;
      if (next !== chatId()) chatId.set(next);
    });
    effect(() => { if (isVisible(chatProps.panelId)) seen.set(true); });
    const ready = () => !!chatId() && seen();
    return <View testID={`panel:${chatProps.panelId}`} style={{ flex: 1, minHeight: 0 }}>
      {ready() && <ChatTab chatId={chatId()!} visible={isVisible(chatProps.panelId)} theme={theme}
        onClose={() => { const tab = findPanel(snapshot(), chatProps.panelId); if (tab) closing.set(tab); }} />}
      {!ready() && <View style={{ flex: 1, padding: 16, gap: 8 }}>
        <Text style={textStyle()}>{pending() ? "Starting chat…" : chatProps.title}</Text>
        {!pending() && !chatId() && <Text style={mutedStyle()}>This view is unavailable.</Text>}
      </View>}
    </View>;
  }
  function paneMenu(id: number) {
    const pane = snapshot().panes.find((pane) => pane.id === id);
    if (!pane) return;
    action(() => session.selectPane(id));
    const disabled = (label: string, shortcut?: string): MenuItem => ({ label, disabled: true, shortcut });
    const rows: MenuItem[] = [disabled("Existing sessions", "mod+alt+s"), disabled("Existing Claude chats"), disabled("Existing Muse chats"), { type: "separator" }, disabled("New tab"),
      { label: "Shell", shortcut: "mod+t", onSelect: () => { void createShell(); } },
      { label: "Subshell", disabled: !selected()?.startsWith("shell:"), onSelect: () => { void createSubshell(); } },
      { label: "Open file in Vim…", onSelect: showVimDialog },
      ...[["Codex", "mod+shift+c"], ["Claude", "mod+shift+l"], ["Grok", "mod+shift+g"], ["Muse", "mod+shift+u"]].map(([label, shortcut]) => ({ label, shortcut, disabled: !remoteProjectKey(projectId()), onSelect: () => { void createShell(undefined, label.toLowerCase()); } })),
      { label: "Global orchestrator", shortcut: "mod+shift+o", onSelect: openGlobalOrchestrator },
      { label: "Project orchestrator", shortcut: "mod+alt+o", disabled: !UUID.test(projectId()) && !remoteProjectKey(projectId()), onSelect: () => { void openProjectOrchestrator(); } },
      { label: "New chat…", disabled: !remoteProjectKey(projectId()), onSelect: () => { commands.dispatch("newChat"); } },
      { label: "App preview…", disabled: !filesRoot() && !snapshot().remoteProject, onSelect: newAppPreview },
      { type: "separator" }, disabled("Views"),
      ...VIEWS.map((panel): MenuItem => ({ label: panelTitle(panel), shortcut: VIEW_SHORTCUTS[panel], onSelect: () => view(panel) })),
      { type: "separator" }, disabled("Pane"),
      { label: "Split right", shortcut: "mod+d", onSelect: () => { void createShell("right"); } },
      { label: "Split down", shortcut: "mod+shift+d", onSelect: () => { void createShell("down"); } },
    ];
    if (!pane.locked) rows.push({ label: "Close pane", shortcut: "mod+shift+w", onSelect: () => closePane(id) });
    rows.push({ label: "Main pane", checked: pane.main, onSelect: () => action(() => session.toggleMain(id)) });
    if (snapshot().panes.some((p) => p.main)) rows.push({ label: "Gather tabs into main pane", shortcut: "mod+shift+m", onSelect: () => action(() => session.gather()) });
    if (headerWidth(id) < M.lockAt) rows.push({ label: pane.locked ? "Unlock pane" : "Lock pane", onSelect: () => action(() => session.toggleLock(id)) });
    if (headerWidth(id) < M.focusAt) rows.push({ label: "Focus tab", shortcut: "mod+shift+f", onSelect: () => action(() => session.toggleFocus()) });
    popup(rows);
  }
  function tabMenu(id: string) {
    const tab = findPanel(snapshot(), id);
    if (!tab || tab.kind === "panel" || tab.shared === false || !(snapshot().sharedTabs ?? []).some((entry) => entry.id === id)) {
      const pane = snapshot().panes.find((pane) => pane.tabs.some((entry) => entry.id === id));
      if (pane) paneMenu(pane.id);
      return;
    }
    const tabs = (snapshot().sharedTabs ?? []).filter((entry) => entry.kind !== "panel" && entry.shared !== false && entry.parent === tab.parent);
    const index = tabs.findIndex((tab) => tab.id === id);
    popup([
      { label: "Rename…", onSelect: () => { renameTitle.set(tab.title); renaming.set(tab); freeze(); } },
      { type: "separator" },
      { label: "Move left", disabled: index <= 0, onSelect: () => action(() => session.moveTab(id, -1)) },
      { label: "Move right", disabled: index < 0 || index === tabs.length - 1, onSelect: () => action(() => session.moveTab(id, 1)) },
      { type: "separator" }, { label: "Close", onSelect: () => close(id) },
      { type: "separator" }, { label: "Cancel", onSelect: focusActive },
    ]);
  }
  function saveRename() {
    const tab = renaming();
    if (!tab) return;
    const title = renameTitle();
    renaming.set(null);
    action(() => session.renameTab(tab.id, title));
    thaw();
  }

  const commands = workspaceCommands(session, snapshot, action, {
    blocked: () => modal() || !!panelState.editor() || switching(),
    previewOwnsEscape: () => !!previewFocus() && isVisible(previewFocus()!) && !frozen() &&
      !statusMenu() && !props.modalOpen?.() && !panelState.editor() && !switching(),
    newShell: (split) => { allTabs.set(false); void createShell(split); },
    newSubshell: () => { allTabs.set(false); void createSubshell(); },
    remoteAgent: (kind) => { if (!remoteProjectKey(projectId())) return false; void createShell(undefined, kind); return true; },
    remoteRefusal: (id) => {
      if (!remoteProjectKey(projectId())) return false;
      notice.set(id === "handOff" ? remoteLocalOnly(remote.data(), projectId(), "handoff")! : "Chats run on this Mac. Open a project here to start one.");
      return true;
    },
    openVim: showVimDialog,
    appPreview: newAppPreview,
    find,
    closeTab: () => close(),
    closePane: () => closePane(),
    toggleFocus: () => { allTabs.set(false); action(() => session.toggleFocus()); thaw(); },
    toggleCentered: () => { action(() => session.toggleCentered()); thaw(); },
    tabContextMenu: () => { if (selected()) tabMenu(selected()!); },
    view: (panel) => { allTabs.set(false); view(panel); },
    selectNumbered: (n) => {
      const pane = activePane(snapshot());
      const tab = shared() ? (n === 9 ? sessions()[sessions().length - 1] : sessions()[n - 1]) : pane && numberedTab(pane, n, false);
      if (tab) select(tab.id);
    },
    cycle: (direction) => {
      const pane = activePane(snapshot());
      const tab = pane && cycledTab(pane, pane.activeTab, direction, modal());
      if (tab) select(tab.id);
    },
    escape: () => {
      if (remote.prompt()) { remote.close(); thaw(); return; }
      if (modal()) { closing.set(null); renaming.set(null); vimDialog.set(false); thaw(); return; }
      if (selected() === "panel:preview") { fileState.clearPreview(); fileState.closePreview?.(); return; }
      const terminal = selected() ? terminalControls.get(selected()!) : undefined;
      if (terminal?.finding()) { terminal.findClose(); return; }
      if (allTabs()) allTabs.set(false);
      thaw();
    },
    projectId: () => projectId() || undefined,
    projectOrchestrator: () => { void openProjectOrchestrator(); },
    globalOrchestrator: openGlobalOrchestrator,
  });

  cleanup(host.on("route", (data) => {
    const step = (data as { step?: { kind?: string; type?: string; shellId?: string; panel?: string; parentShellId?: string; path?: string; line?: number; worktreeId?: string; chatId?: string; taskId?: string } }).step;
    // `{ panel }` alone is the contract's shorthand for a panel step (§2.3).
    const kind = step?.kind ?? step?.type ?? (step?.panel ? "panel" : undefined);
    if (kind === "showShell" && step?.shellId) void openShell(step.shellId);
    else if (kind === "panel" && step?.panel) view(step.panel);
    else if (kind === "chat" && step?.chatId) action(() => session.openChat(step.chatId!));
    else if (kind === "task" && step?.taskId) {
      try { session.setSelectedTask(step.taskId); } catch (error) { notice.set(String(error)); }
      action(() => session.openPanel("tasks"));
    }
    else if ((step?.kind ?? step?.type) === "newSubshell") void createSubshell(step?.parentShellId);
    else if ((step?.kind ?? step?.type) === "openEditor" && step?.path) void openEditor(step.path, step.line, step.worktreeId);
    else if (kind === "apiKeySettings") {
      revealSettingsKey = !settingsControl;
      action(() => session.openPanel("settings"));
      settingsControl?.revealApiKey();
    }
    else if (kind === "projectMemo") {
      revealProjectMemo = !projectSettingsControl;
      action(() => session.openPanel("project_settings"));
      projectSettingsControl?.revealMemo();
    }
  }));
  cleanup(host.on("project.open", (data) => {
    void switchTo((data as { projectId: string }).projectId).catch((error: unknown) => notice.set(String(error)));
  }));
  const subscription = session.subscribe((next) => { if (!frozen()) install(next); });
  cleanup(() => subscription.close());
  cleanup(keymap.bind("mod+f", () => {
    if (panelState.editor() || modal() || switching()) return;
    find("find");
  }));
  cleanup(keymap.bind("mod+g", () => { if (!modal() && !panelState.editor()) find("findNext"); }));
  cleanup(keymap.bind("mod+shift+g", () => { if (!modal() && !panelState.editor()) find("findPrevious"); }));
  cleanup(keymap.bind("shift+f10", () => { if (!modal() && selected()) tabMenu(selected()!); }));
  effect(() => {
    if (!allTabs()) return;
    for (const [key, direction] of [["down", 1], ["up", -1]] as const) cleanup(keymap.bind(key, () => {
      if (sessions().length) allIndex.set((allIndex() + direction + sessions().length) % sessions().length);
    }));
    cleanup(keymap.bind("enter", () => { const tab = sessions()[allIndex()]; if (tab) select(tab.id); }));
  });

  function paneControls(id: number) {
    const pane = () => snapshot().panes.find((pane) => pane.id === id);
    const icon = (on = false) => ({ fontFamily: theme().family, fontSize: 11 * theme().scale, color: on ? theme().palette.cyan : theme().palette.muted });
    return <View style={{ flexDirection: "row", alignItems: "center", gap: M.buttonGap }}>
      {pane()?.main && headerWidth(id) >= mainMarkerThreshold(snapshot().theme, snapshot().panelTabIcons) && <View testID={`pane-main-${id}`} accessibilityLabel="Unset main pane" onPress={() => action(() => session.toggleMain(id))} style={{ width: M.nativeButton }}><Text style={icon(true)}>★</Text></View>}
      {headerWidth(id) >= M.lockAt && <View testID={`pane-lock-${id}`} accessibilityLabel={pane()?.locked ? "Unlock pane" : "Lock pane"} onPress={() => action(() => session.toggleLock(id))} style={{ width: M.nativeButton, height: M.nativeButton, borderRadius: M.buttonRadius, justifyContent: "center" }}><Text style={icon(!!pane()?.locked)}>{pane()?.locked ? "◆" : "◇"}</Text></View>}
      {headerWidth(id) >= M.focusAt && <View testID={`pane-focus-${id}`} accessibilityLabel="Focus tab" onPress={() => { action(() => session.selectPane(id)); commands.dispatch("toggleFocusMode"); }} style={{ width: M.nativeButton, height: M.nativeButton, justifyContent: "center" }}><Text style={icon()}>↗</Text></View>}
      <View testID={`pane-menu-${id}`} accessibilityLabel="Pane menu" onPress={() => paneMenu(id)} style={{ width: M.menuButton, height: header(), justifyContent: "center", alignItems: "center" }}><Text style={icon()}>…</Text></View>
    </View>;
  }
  function tabCell(tab: PanelModel, active: boolean, strip: boolean) {
    const pane = snapshot().panes.find((pane) => pane.tabs.some((panel) => panel.id === tab.id));
    const shownTitle = displayTitle(tab);
    return <View ref={(node) => tabRefs.set(`${strip ? "strip" : "pane"}:${tab.id}`, node)} testID={`tab-${tab.id}`} role={strip ? "tab" : undefined} accessibilityLabel={`${tab.kind === "remote_shell" ? shownTitle : tab.title}${tab.kind !== "panel" && tab.status ? `, ${tab.status}` : ""}`}
      accessibilityState={{ selected: active }} focusable={true}
      onPress={() => select(tab.id)} onContextMenu={() => tabMenu(tab.id)} onHoverIn={() => hovered.set(tab.id)} onHoverOut={() => hovered.set(null)}
      onPressDown={(event) => { if (strip && event) pressOrigin = event.pagePosition; }}
      onTouchStart={(event) => { if (strip) pressOrigin = { x: event.pageX, y: event.pageY }; }}
      pan={strip ? { x: dragX, y: dragY } : undefined}
      onPanStart={strip ? () => { hovered.set(null); dragging.set(true); allTabs.set(false); freeze(); dragOrigin.set(pressOrigin); dragTab.set(tab); } : undefined}
      onPanEnd={strip ? (event) => { void stripDrop(tab.id, event.translationX, event.translationY, event.cancelled); } : undefined}
      style={{ height: header(), width: strip ? (tab.kind === "panel" ? M.panelCell : sessionCellWidth(tab.title)) : undefined, minWidth: 0, paddingLeft: space(8, theme().scale), paddingRight: 4, flexDirection: "row", alignItems: "center", gap: 6, backgroundColor: active ? theme().palette.panelActive : theme().palette.panel }}>
      {tab.kind !== "panel" && <StatusDot status={tab.status} />}
      <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: (strip ? M.tabFont : 11) * theme().scale, flexShrink: 1, color: theme().palette.text }}>{strip && tab.kind === "panel" ? shownTitle.slice(0, 1) : remoteMark(tab, shownTitle)}</Text>
      {!pane?.locked && (active || hovered() === tab.id) && <View testID={`tab-close-${tab.id}`} tooltip="Close tab · ⌘W" accessibilityLabel={`Close ${tab.title}`} onPress={() => close(tab.id)} style={{ width: M.tabClose, alignItems: "center" }}><Text style={mutedStyle()}>×</Text></View>}
      {pane?.locked && !strip && <Text style={{ ...textStyle(9), color: theme().palette.cyan }}>◆</Text>}
    </View>;
  }
  async function stripDrop(id: string, dx: number, dy: number, cancelled: boolean) {
    const point = { x: pressOrigin.x + dx, y: pressOrigin.y + dy };
    dragTab.set(null); dragX.set(0); dragY.set(0);
    if (!cancelled && point.x >= 0 && point.x <= width() && point.y >= 0) {
      if (point.y < header()) {
        let x = 48 + (chrome.fullscreen() ? 0 : CONTROL_INSET) - scrollOffset;
        const target = ordered().find((tab) => { x += tab.kind === "panel" ? M.panelCell : sessionCellWidth(tab.title); return point.x < x; });
        const group = snapshot().panes.find((pane) => pane.tabs.some((tab) => tab.id === (target?.id ?? id)));
        if (group && target?.id !== id) {
          const index = target ? group.tabs.findIndex((tab) => tab.id === target.id) : group.tabs.length;
          // movePanel removes the source before insertion. The target's original
          // index therefore means before it moving left and after it moving right.
          dockRef?.movePanel(id, { group: group.id, index });
        }
      } else {
        // Ask actual mounted pane surfaces for their frames; no cross-window or
        // cross-project payload is accepted. Edge zones match DockArea (35%).
        for (const pane of snapshot().panes) {
          const node = bodyRefs.get(pane.activeTab ?? `empty:${pane.id}`);
          const measured = await node?.measure();
          if (!measured) continue;
          const { pagePosition: origin, bounds: { size: extent } } = measured;
          const x = point.x - origin.x, y = point.y - origin.y;
          if (x < 0 || y < 0 || x > extent.width || y > extent.height) continue;
          const side = x < extent.width * .35 ? "left" : x > extent.width * .65 ? "right" : y < extent.height * .35 ? "top" : y > extent.height * .65 ? "bottom" : undefined;
          dockRef?.movePanel(id, { group: pane.id, side });
          break;
        }
      }
    }
    dragging.set(false);
    thaw();
  }
  // The persistent notice is an overlay that hears Escape before the window
  // keymap; a dialog, rename or tab menu opened over it takes Escape first.
  function escapeTopmost() {
    if (commands.stripOpen()) commands.stripOpen.set(false);
    else if (modal() || allTabs()) { closing.set(null); renaming.set(null); vimDialog.set(false); allTabs.set(false); thaw(); }
    else dismissNotice();
  }
  const terminalFrozen = () => frozen() || statusMenu() || commands.stripOpen() || panelState.editor() !== null || switching();
  const paneOf = (id: string) => snapshot().panes.find((pane) => pane.tabs.some((tab) => tab.id === id) || id === `empty:${pane.id}`);

  return (
    <View testID="workspace" ref={(node) => { root = node; }} focusable={true} onLayout={(bounds) => width.set(bounds.size.width)} style={{ flex: 1, backgroundColor: theme().palette.bg }}>
      {shared() && !snapshot().focusMode && <View testID="shared-strip" desktop={{ windowDrag: true }} style={{ height: header(), flexDirection: "row", paddingLeft: chrome.fullscreen() ? 0 : CONTROL_INSET, backgroundColor: theme().palette.panel }} onLayout={(bounds) => { stripWidth.set(bounds.size.width); reveal(); }}>
        <Scroll ref={(node) => { stripRef = node; }} horizontal={true} role="tablist" accessibilityLabel="Tabs" style={{ flex: 1 }} contentStyle={{ flexDirection: "row", alignItems: "center" }}>
          <Text style={{ ...mutedStyle(9), width: 48 }}>Panels</Text>
          {ordered().map((tab) => tabCell(tab, selected() === tab.id, true))}
        </Scroll>
        {allTabsAvailable() && <Popover open={allTabs()} onOpenChange={(open) => { allTabs.set(open); if (open) { hovered.set(null); allIndex.set(Math.max(0, sessions().findIndex((tab) => tab.id === selected()))); freeze(); } else thaw(); }} placement="bottom" align="end"
          popupStyle={{ width: M.allTabsWidth, maxHeight: M.allTabsMaxHeight, backgroundColor: theme().palette.panelActive, padding: 4 }}
          trigger={<View testID="strip-all-tabs" accessibilityLabel="All tabs" style={{ width: 28, justifyContent: "center", alignItems: "center" }}><Text style={mutedStyle()}>⌄</Text></View>}>
          <Scroll style={{ maxHeight: M.allTabsMaxHeight }}>
            {sessions().map((tab, index) => <View role="menuitem" accessibilityLabel={tab.title} onPress={() => select(tab.id)} style={{ height: 28, paddingHorizontal: 8, gap: 8, flexDirection: "row", alignItems: "center", backgroundColor: allIndex() === index ? theme().palette.divider : theme().palette.panelActive }}>
              <StatusDot status={tab.status} /><Text style={{ ...textStyle(), flex: 1 }}>{displayTitle(tab)}</Text><Text style={{ ...textStyle(), color: theme().palette.cyan }}>{selected() === tab.id ? "✓" : ""}</Text>
            </View>)}
          </Scroll>
        </Popover>}
      </View>}
      <View testID="dock-container" onLayout={(bounds) => containerHeight.set(bounds.size.height)} style={{ flex: 1, minHeight: 0, alignItems: "center", paddingBottom: snapshot().focusMode && snapshot().focusCentered ? containerHeight() * M.focusBottomMargin : 0 }}>
        <View style={{ flex: 1, width: "100%", maxWidth: snapshot().focusMode && snapshot().focusCentered ? M.focusMaxWidth : undefined }} onLayout={(bounds) => measure(bounds.size)}>
          {dockReady() && <DockArea ref={(node) => { dockRef = node; }} testID="workspace-dock" style={{ flex: 1 }} layout={dock()!} compact={false} dividerSize={M.divider}
            panels={panelIds(dock()!.center).map((id) => ({ id, closable: false, zoomable: false, visible: !snapshot().focusMode || !!snapshot().panes.find((pane) => pane.id === snapshot().activePane && (pane.tabs.some((tab) => tab.id === id) || id === `empty:${pane.id}`)) }))}
            onLayoutChange={edit}
            tabBarStyle={{ height: shared() && !snapshot().focusMode ? 0 : header(), flexDirection: "row", backgroundColor: theme().palette.panel, overflow: "hidden" }}
            tabStyle={{ padding: 0, minWidth: 0 }}
            contentStyle={{ flex: 1, backgroundColor: theme().palette.bg }}
            handleStyle={{ backgroundColor: theme().palette.divider }}
            handleAccessibilityLabel="Pane divider"
            renderTabBarSuffix={(id) => paneControls(id)}
            renderDragPreview={(tab) => <View style={{ paddingHorizontal: 12, paddingVertical: 7, borderWidth: 1, borderColor: theme().palette.cyan, backgroundColor: theme().palette.panel }}><Text style={textStyle()}>{findPanel(snapshot(), tab.panel) ? displayTitle(findPanel(snapshot(), tab.panel)!) : tab.panel}</Text></View>}
            renderTab={(tab) => <View testID={`tab:${tab.panel}`} ref={(node) => props.onViewRef?.(`tab:${tab.panel}`, node)}
              onLayout={(bounds) => measureTab(tab.group, tab.panel, bounds.size.width)}
              onPressDown={() => { hovered.set(null); dragging.set(true); freeze(); }} onPressUp={() => { dragging.set(false); thaw(); }}
              onPress={() => tab.panel.startsWith("empty:") ? action(() => session.selectPane(tab.group)) : select(tab.panel)} onContextMenu={() => tabMenu(tab.panel)}
              style={{ flexDirection: "row", alignItems: "center", marginLeft: tab.index === 0 && !chrome.fullscreen() && topPaneFrames(dock()!.center, measured()).some((pane) => pane.id === tab.group && pane.x === 0) ? CONTROL_INSET : 0 }}>
              {!tab.panel.startsWith("empty:") && tabCell(findPanel(snapshot(), tab.panel) ?? { id: tab.panel, title: tab.panel, kind: "empty" }, tab.active, false)}
            </View>}
            renderPanel={(panel) => <View ref={(node) => { bodyRefs.set(panel.id, node); props.onViewRef?.(`panel:${panel.id}`, node); }} focusable={true} testID={`pane-${paneOf(panel.id)?.id}`} style={{ flex: 1, minHeight: 0 }}
              onPress={() => { action(() => panel.id.startsWith("empty:") ? session.selectPane(Number(panel.id.slice(6))) : session.selectPanel(panel.id)); focusActive(); }}>
              {shared() && !snapshot().focusMode && <View style={{ height: header(), flexDirection: "row", justifyContent: "flex-end", backgroundColor: theme().palette.panel }}>{paneControls(paneOf(panel.id)?.id ?? snapshot().activePane)}</View>}
              {remoteProject() && panel.id.startsWith("panel:") && remotePanelUnavailable(panel.id.slice(6)) ?
                <RemoteUnavailable hostLabel={remoteHostLabel(remote.data(), remoteProjectKey(projectId())!.host)} panel={panel.id.slice(6)} theme={theme} /> :
                panel.id.startsWith("panel:") && NAVIGATION_PANELS.includes(panel.id.slice(6)) ?
                <View testID={`panel:${panel.id}`} style={{ flex: 1, minHeight: 0 }}>
                  {remoteProject() && panel.id !== "panel:projects" ? <Scroll style={{ flex: 1, minHeight: 0 }}>
                    <RemoteProjectPanel controller={remote} projectKey={projectId} panel={panel.id.slice(6) as "worktrees" | "tasks" | "shells"}
                      query={panelState.query} selectedWorktree={panelState.selectedWorktree}
                      onWorktree={(id) => { panelState.selectWorktree(id); void remoteActions.worktree(id).catch((error) => notice.set(String(error))); }}
                      onTask={(id) => { panelState.selectTask(id); const task = remote.data().projects.find((project) => project.key === projectId())?.tasks.find((task) => task.id === id); if (task?.worktreeId) panelState.selectWorktree(task.worktreeId); }}
                      onShell={(hostId, shell) => remoteActions.open(hostId, shell)} />
                  </Scroll> : <PanelContent controller={panelState} panel={panel.id.slice(6)} surface={panel.id} />}
                  {panel.id === "panel:projects" && <Scroll testID="remote-projects-scroll" style={{ maxHeight: Math.min(260 * theme().scale, (containerHeight() || 600) * 0.4), flexShrink: 1 }}>
                    <RemoteProjects controller={remote} query={panelState.query} selected={projectId} order={() => ({ order: panelState.order(), descending: panelState.descending() })} collapsed={panelState.collapsed} onToggleFolder={panelState.toggleFolder}
                      onSelectProject={(key) => { void switchTo(key).catch((error) => notice.set(String(error))); }} />
                  </Scroll>}
                </View> : panel.id === "panel:files" || panel.id === "panel:preview" || panel.id === "panel:project_settings" || panel.id === "panel:settings" ?
                <View testID={`panel:${panel.id}`} style={{ flex: 1, minHeight: 0 }}>
                  {props.diagnostics && <MountProbe id={panel.id} visible={isVisible(panel.id)} theme={theme} />}
                  {panel.id === "panel:files" ? <FilesPanel controller={fileState} surface={panel.id} /> :
                    panel.id === "panel:preview" ? <PreviewPanel controller={fileState} /> :
                    panel.id === "panel:settings" ? <AppSettings theme={theme} settings={props.settings} visible={() => isVisible(panel.id)} onError={(error) => notice.set(String(error))} onNotice={(message) => notice.set(message)}
                      remote={<RemoteSettings controller={remote} />}
                      computerUseFocus={cuaFocus} computerUse={<CuaSetup theme={theme} visible={isVisible(panel.id)}
                        onControl={(control) => { cuaFocus.set(control); return () => { if (cuaFocus() === control) cuaFocus.set(null); }; }} />}
                      onControl={(control) => { settingsControl = control; if (revealSettingsKey) { revealSettingsKey = false; control.revealApiKey(); } return () => { if (settingsControl === control) settingsControl = undefined; }; }} /> :
                    <ProjectSettingsPanel projectId={projectId} theme={theme} visible={() => isVisible(panel.id)}
                      onControl={(control) => { projectSettingsControl = control ?? undefined; if (control && revealProjectMemo) { revealProjectMemo = false; control.revealMemo(); } }} />}
                </View> : panel.id === "panel:schedules" ?
                <View testID={`panel:${panel.id}`} style={{ flex: 1, minHeight: 0 }}>
                  {props.diagnostics && <MountProbe id={panel.id} visible={isVisible(panel.id)} theme={theme} />}
                  <AutomationsPanel projectId={projectId} worktreeId={panelState.selectedWorktree} visible={() => isVisible(panel.id)} active={() => selected() === panel.id} theme={theme}
                    openChat={(id) => action(() => session.openChat(id))} />
                </View> : panel.id.startsWith("remote:") ?
                <RemoteTerminal desktopId={panel.id.split(":")[1]} shellId={panel.id.split(":")[2]} label={remoteHostLabel(remote.data(), panel.id.split(":")[1])}
                  link={remote.data().hosts.find((host) => host.id === panel.id.split(":")[1])?.link ?? null} visible={isVisible(panel.id)} focusOnAttach={pendingFocus() === panel.id}
                  colors={terminalColors()} frozen={terminalFrozen()} theme={theme} onNotice={(message) => notice.set(message)}
                  onControl={(control) => { terminalControls.set(panel.id, control); return () => { if (terminalControls.get(panel.id) === control) terminalControls.delete(panel.id); }; }}
                  onFocus={() => action(() => session.selectPanel(panel.id))} onAttached={() => { if (pendingFocus() === panel.id) pendingFocus.set(null); }} /> :
                panel.id.startsWith("shell:") ?
                <View testID={`panel:${panel.id}`} style={{ flex: 1, minHeight: 0 }}>
                  {props.diagnostics && <MountProbe id={panel.id} visible={isVisible(panel.id)} theme={theme} />}
                  <LocalTerminal shellId={panel.id.slice(6)} visible={isVisible(panel.id)}
                    pinned={membershipProject(panel.id.slice(6)) ? !panelState.shells().find((shell) => shell.id === panel.id.slice(6))?.alive : false}
                    focusOnAttach={pendingFocus() === panel.id.slice(6)} colors={terminalColors()} frozen={terminalFrozen()}
                    onMount={() => props.onTerminalMount?.(panel.id.slice(6))}
                    onControl={(control) => {
                      terminalControls.set(panel.id, control);
                      return () => {
                        if (terminalControls.get(panel.id) === control) terminalControls.delete(panel.id);
                      };
                    }}
                    onAttached={() => { if (pendingFocus() === panel.id.slice(6)) pendingFocus.set(null); }}
                    onFocus={() => action(() => session.selectPanel(panel.id))} onLink={openLink} onNotice={(message) => notice.set(message)} />
                </View> : panel.id.startsWith("chat:") || panel.id.startsWith("chat-pending:") ?
                <ChatPanel panelId={panel.id} title={findPanel(snapshot(), panel.id) ? displayTitle(findPanel(snapshot(), panel.id)!) : panel.id} /> :
                panel.id.startsWith("app_preview:") ?
                <AppPreviewTab spec={findPanel(snapshot(), panel.id)!.previewSpec!} visible={isVisible(panel.id)} theme={theme}
                  onFocusChange={(focused) => {
                    if (focused) { previewFocus.set(panel.id); action(() => session.selectPanel(panel.id)); }
                    else if (previewFocus() === panel.id) previewFocus.set(null);
                  }}
                  configure={(spec) => action(() => session.configureAppPreview(panel.id, spec))} /> :
                <DeferredPanel id={panel.id}
                  title={findPanel(snapshot(), panel.id) ? displayTitle(findPanel(snapshot(), panel.id)!) : panel.id}
                  theme={theme} diagnostics={props.diagnostics}
                  visible={isVisible(panel.id)}
                  onPress={() => { action(() => panel.id.startsWith("empty:") ? session.selectPane(Number(panel.id.slice(6))) : session.selectPanel(panel.id)); focusActive(); }} />}
            </View>} />}
        </View>
      </View>
      <WindowChrome state={chrome} panes={chromePanes()} />
      <View testID="panel-toolbar" style={{ flexDirection: "row", alignItems: "center", paddingHorizontal: space(8, theme().scale), paddingVertical: space(4, theme().scale), gap: 6, backgroundColor: theme().palette.panel }}>
        {(["projects", "files", "worktrees", "tasks", "shells"] as const).map((id) => <Button testID={`show-panel:${id}`} accessibilityLabel={`Show ${id === "shells" ? "Sessions" : id}`} disabled={switching()} onPress={() => action(() => session.openPanel(id))} style={{ paddingHorizontal: space(8, theme().scale), paddingVertical: space(4, theme().scale), borderRadius: 4 }} hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.divider }}>
          <Text style={textStyle()}>{id === "shells" ? "Sessions" : id.charAt(0).toUpperCase() + id.slice(1)}</Text>
        </Button>)}
        <View style={{ flex: 1 }} />
        {snapshot().focusMode && <View testID="focus-restore" role="button" accessibilityLabel="Restore layout" onPress={() => commands.dispatch("toggleFocusMode")}><Text style={{ ...textStyle(), color: theme().palette.cyan }}>↙ RESTORE  ⌘⇧F</Text></View>}
        <View testID="all-tabs" role="button" accessibilityLabel="All tabs" onPress={commands.toggleStrip}><Text style={textStyle()}>All tabs</Text></View>
      </View>
      <StatusBar session={session} snapshot={snapshot} projectId={projectId} projectLabel={() => remoteTitle(remote.data(), projectId()) ?? panelState.projectLabel()}
        settings={props.settings} theme={theme} action={action} onError={(error) => notice.set(String(error))} modalOpen={() => !!props.modalOpen?.() || modal() || !!panelState.editor() || switching()}
        newShell={props.projectId ? { busy, create: () => { void createShell(); } } : undefined} onMenu={(open) => statusMenu.set(open)}
        openProjectOrchestrator={UUID.test(projectId()) || remoteProjectKey(projectId()) ? () => commands.dispatch("openProjectOrchestrator") : undefined}
        remote={() => { const lists = remote.data().projects.find((row) => row.key === projectId()); const tree = panelState.selectedWorktree();
          return { branch: lists?.worktrees.find((row) => row.id === tree)?.branch ?? null, live: lists?.live ?? null }; }}
        focus={{ mode: () => !!snapshot().focusMode, centered: () => !!snapshot().focusCentered,
          toggle: () => commands.dispatch("toggleFocusMode"), toggleCentered: () => commands.dispatch("focusCentered") }} />
      {notice() !== "" && <Overlay testID="workspace-notice" onCancel={escapeTopmost} margin={8}
        style={{ top: 31, right: 8, width: 520, maxWidth: "100%", padding: 8, backgroundColor: theme().palette.panel, borderColor: theme().palette.gold, borderWidth: 1, flexDirection: "row", gap: 10 }}>
        <Text testID="workspace-notice-message" accessible={true} role="alert" accessibilityLabel={notice()} style={{ ...textStyle(), flex: 1, color: theme().palette.gold }}>{notice()}</Text>
        <View testID="dismiss-notice" accessibilityLabel="Dismiss" role="button" onPress={dismissNotice}><Text style={{ ...textStyle(), color: theme().palette.gold }}>×</Text></View>
      </Overlay>}
      {commands.stripOpen() && <Overlay onCancel={() => commands.stripOpen.set(false)} onPressOutside={() => commands.stripOpen.set(false)}
        style={{ right: 8, bottom: 24, padding: 8, backgroundColor: theme().palette.panelActive }}>
        <View testID="all-tabs-menu" role="menu" accessibilityLabel="All tabs">
          {commands.stripItems().map((tab, index) => <View testID={`all-tabs:${tab.id}`} role="menuitem" accessibilityLabel={tab.title} accessibilityState={{ selected: commands.stripIndex() === index }} onPress={() => commands.activateStrip(index)} style={{ padding: 8, backgroundColor: commands.stripIndex() === index ? theme().palette.divider : "transparent" }}><Text style={textStyle()}>{displayTitle(tab)}</Text></View>)}
        </View>
      </Overlay>}
      {closing() && <CloseDialog title={displayTitle(closing()!)} theme={theme} onChoice={(choice) => { const tab = closing()!; closing.set(null); if (choice) action(() => session.closePanel(tab.id, choice === "exit")); thaw(); }} />}
      {vimDialog() && <Dialog testID="vim-open-dialog" animation="none" closeOnEscape={true} closeOnBackdropPress={false}
        onOpen={() => vimInput?.focus()} onOpenChange={() => { vimDialog.set(false); thaw(); }}
        style={{ width: 480, maxWidth: "100%", padding: 18, gap: 10, borderRadius: 12, backgroundColor: theme().palette.panel }}>
        <Text style={textStyle(13)}>Open file in Vim</Text>
        <Input testID="vim-open-path" accessibilityLabel="File path" placeholder="File path in the selected project or worktree" value={vimPath()}
          ref={(node) => { vimInput = node; }} onChange={(value) => vimPath.set(value)} onSubmit={submitVim} />
        <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
          <Button testID="vim-open-cancel" onPress={() => { vimDialog.set(false); thaw(); }}><Text style={textStyle()}>Cancel</Text></Button>
          <Button testID="vim-open-submit" disabled={!vimPath()} onPress={submitVim}><Text style={textStyle()}>Open</Text></Button>
        </View>
      </Dialog>}
      {tabHint() && !frozen() && <Overlay pointerEvents="none" anchor={tabRefs.get(`${shared() && !snapshot().focusMode ? "strip" : "pane"}:${tabHint()!.id}`)} placement="bottom" align="start" offset={4} style={{ paddingHorizontal: 8, paddingVertical: 5, backgroundColor: theme().palette.panelActive, borderRadius: 4 }}><Text style={textStyle()}>{tabHint()!.text}</Text></Overlay>}
      {dragTab() && <Overlay pointerEvents="none" style={{ position: "absolute", left: dragOrigin().x, top: dragOrigin().y, transform: [{ translateX: dragX }, { translateY: dragY }], paddingHorizontal: 12, paddingVertical: 7, borderWidth: 1, borderColor: theme().palette.cyan, backgroundColor: theme().palette.panel }}><Text style={textStyle()}>{displayTitle(dragTab()!)}</Text></Overlay>}
      <Popover open={!!renaming()} onOpenChange={(open) => { if (!open) { renaming.set(null); thaw(); } }} onOpen={() => renameInput?.focus()} popupStyle={{ width: 260, padding: 12, gap: 8, backgroundColor: theme().palette.panelActive }} trigger={<View style={{ position: "absolute", top: 56, left: 120, width: 1, height: 1 }} />}>
        <Input ref={(node) => { renameInput = node; }} testID="tab-rename-input" placeholder="Tab title" value={renameTitle()} onChange={(value) => renameTitle.set(value)} onSubmit={saveRename} />
        <View role="button" accessibilityLabel="Save title" onPress={saveRename}><Text style={textStyle()}>Save title</Text></View>
      </Popover>
      <PanelEditors controller={panelState} />
      <RemotePrompt controller={remote} />
    </View>
  );
}
