import { AppPreviewTab } from "./AppPreviewTab";
import { CuaSetup, useCua, type CuaSetupControl } from "./CuaSetup";
import { Clipboard, ErrorBoundary, Overlay, Scroll, Text, View, cleanup, colorScheme, computed, effect, host, keymap, signal, type YNode } from "nsh";
import { Button, Input, Popover, type DockAreaLayout, type DockNode } from "nsh/base";
import { Menu, Window, type MenuItem } from "nsh/desktop";
import { Appearance, Chat, Files, Orchestrators, Remote, Sessions, Tabs, type AppearanceDto, type LayoutSession, type SettingsDto } from "riwork/native";
import { DockArea } from "./DockArea";
import { DeferredPanel, MountProbe } from "./DeferredPanel";
import { LocalTerminal, type TerminalColors, type TerminalController, type TerminalEvents } from "./Terminal";
import { PanelContent, createPanelController, type PanelController } from "./Panels";
import { WindowChrome, useWindowChrome, topPaneFrames, CONTROL_INSET } from "./WindowChrome";
import { workspaceCommands } from "./commands";
import { WORKSPACE_METRICS as M, headerHeight, mainMarkerThreshold, sessionCellWidth } from "./metrics";
import { Icon, PANEL_ICONS } from "./Icon";
import { NewChat, type NewChatLocation } from "./NewChat";
import { Handoff as HandoffSetup } from "./Handoff";
import { UsagePanel } from "./UsagePanel";
import {
  activePane, closeChoice, createDockUpdater, createTitleUpdater, cycledTab, findPanel, numberedTab,
  panelTitle, windowTitle, type PanelModel, type PanelStatus, type WorkspaceSnapshot,
} from "./model";
import { DEFAULT_THEME, space, type InterfaceTheme } from "./appearance";
import { StatusBar } from "./StatusBar";
import { ChatTab, type NativeChatUsage } from "./chat/ChatTab";
import { ChatFailure } from "./chat/ChatFailure";
import { createFilesController, FilesPanel, PreviewPanel } from "./Files";
import { AutomationsPanel } from "./schedules/Automations";
import { FlowsPanel } from "./flows/FlowsPanel";
import { createRemoteController, RemoteProjects, RemoteProjectPanel, RemoteSettings, RemoteUnavailable, type RemoteApi } from "./remote/Remote";
import { RemoteTerminal } from "./remote/RemoteTerminal";
import { filteredFolders, remoteHostLabel, remoteLocalOnly, remotePanelUnavailable, remoteProjectKey, remoteTabTitle, remoteTitle, splitRemoteTitle } from "./remote/model";
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
const VIEWS = ["projects", "files", "preview", "worktrees", "tasks", "shells", "usage", "schedules", "flows", "dashboard", "project_settings", "settings"];
const VIEW_KEYS: Record<string, string> = { projects: "⌘B", files: "⌘⇧E", preview: "⌘⇧P", shells: "⌘⌥S", schedules: "⌘⇧S", dashboard: "⌘⌥G", project_settings: "⌘⌥A", settings: "⌘," };
const VIEW_SHORTCUTS: Record<string, string> = { files: "mod+shift+e", preview: "mod+shift+p", schedules: "mod+shift+s", dashboard: "mod+alt+g", settings: "mod+," };
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

function StatusDot(props: { status?: PanelStatus | null; theme: InterfaceTheme }) {
  const status = () => props.status;
  const color = () => status() === "working" ? props.theme.palette.working : status() === "waiting" || status() === "error" ? props.theme.palette.gold : props.theme.palette.muted;
  return <>{status() && <View style={{ width: M.statusDot, height: M.statusDot, justifyContent: "center", alignItems: "center" }}>
    <View style={{ width: status() === "idle" ? M.idleDot : M.statusDot, height: status() === "idle" ? M.idleDot : M.statusDot, borderRadius: 4,
      borderWidth: status() === "stopped" ? 1 : 0, borderColor: color(), backgroundColor: status() === "stopped" ? "transparent" : color() }} />
  </View>}</>;
}

/** What a New chat or Hand off tab holds until it starts. */
type SetupTab =
  | { kind: "chat"; worktreeId?: string; cwd: string; locations: NewChatLocation[]; provider: "codex" | "claude" | "muse" }
  | { kind: "handoff"; sourceId: string };

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
  // Workspace transient UI: the shared-strip popover, native menus and drags hold
  // session deliveries and freeze native terminals. Nothing else covers the window:
  // closing, renaming, opening a file and new sessions happen in place.
  const renaming = signal<PanelModel | null>(null);
  const renameTitle = signal("");
  const vimBar = signal(false);
  const vimPath = signal("");
  const vimSuggestions = signal<string[]>([]);
  /** The tab just closed, while its notice offers Undo (and End, where Ask used to ask). */
  const closedTab = signal<{ title: string; kind: "shell" | "chat"; id: string; end: boolean } | null>(null);
  /** What a setup tab (New chat, Hand off) shows, by its pending id. */
  const setups = new Map<string, SetupTab>();
  const allTabs = signal(false);
  const allTabsPane = signal<number | null>(null);
  const allIndex = signal(0);
  const menuOpen = signal(false);
  const dragging = signal(false);
  const width = signal(1280);
  const containerHeight = signal(0);
  const hovered = signal<string | null>(null);
  const tabHint = signal<{ id: string; text: string } | null>(null);
  const bodyRefs = new Map<string, YNode>();
  const terminalControls = new Map<string, TerminalController>();
  const tabRefs = new Map<string, YNode>();
  const chatUsage = signal<Record<string, NativeChatUsage>>({});
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
    settings: props.settings,
    initialWorktree: snapshot().selectedWorktreeId,
    initialTask: snapshot().selectedTaskId,
    openPanel: (id) => action(() => session.openPanel(id)),
    showShell: (id) => { void openShell(id); },
    showChat: (id) => action(() => session.openChat(id)),
    newChat: () => showNewChat(),
    selectWorktree: (id) => { try { session.setSelectedWorktree(id); snapshot.set(session.snapshot() as WorkspaceSnapshot); } catch (error) { notice.set(String(error)); } },
    selectTask: (id) => { try { session.setSelectedTask(id); snapshot.set(session.snapshot() as WorkspaceSnapshot); } catch (error) { notice.set(String(error)); } },
    switchProject: (project) => switchTo(project.id),
  });
  const remote = createRemoteController({
    api: Remote as unknown as RemoteApi, theme, copyText: (text) => Clipboard.writeText(text),
    notice: (message) => notice.set(message), modalBlocked: () => switching(),
    beforePrompt: () => { allTabs.set(false); commands.stripOpen.set(false); },
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
      if (switching() || panelState.editor()) return null;
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
  const nativeChatUsage = (): NativeChatUsage | null => {
    const id = selected();
    if (!id || (!id.startsWith("chat:") && !id.startsWith("chat-pending:"))) return null;
    const delivered = chatUsage()[id];
    if (delivered) return delivered;
    const saved = session.panel(id) as { chat_id?: string } | null;
    return { provider: panelState.chats().find((chat) => chat.id === saved?.chat_id)?.provider ?? "unknown" };
  };
  function recordChatUsage(id: string, usage: NativeChatUsage) {
    const previous = chatUsage()[id];
    if (previous?.provider === usage.provider && previous?.usageLabel === usage.usageLabel && previous?.usageDetail === usage.usageDetail) return;
    chatUsage.set({ ...chatUsage(), [id]: usage });
  }
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
  // Made again only when it appears or goes, so a form open in the remote list keeps its focus.
  const remoteFolders = computed(() => filteredFolders(remote.data(), panelState.query()).length > 0);
  const frozen = () => menuOpen() || dragging() || allTabs();
  const sessions = () => (snapshot().panes.find((pane) => pane.id === (allTabsPane() ?? snapshot().activePane))?.tabs ?? []).filter((tab) => tab.kind !== "panel" && tab.kind !== "empty");
  const header = () => headerHeight(11 * theme().scale);
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
  function showCreatedSession(kind: string, id: string, owner?: string | null) {
    if (!alive) return;
    if (owner && owner !== projectId()) {
      void host.call("window.route", { projectId: owner, step: kind === "chat" ? { kind: "chat", chatId: id } : { kind: "showShell", shellId: id } })
        .catch((error) => notice.set(String(error)));
    } else if (kind === "chat") action(() => session.openChat(id));
    else void openShell(id);
  }
  async function openWorker(id: string, kind: "shell" | "chat") {
    try {
      const owner = kind === "shell" ? (await Sessions.list()).sessions.find((row) => row.id === id)?.projectId : (await Chat.list()).find((row) => row.id === id)?.projectId;
      showCreatedSession(kind, id, owner);
    } catch (error) { if (alive) notice.set(String(error)); }
  }
  /** ⌘⌥O and P·ORCH: the project's one orchestrator, found or made off-thread
   * as the project or Settings say (SHELL-217..219). A second request waits. */
  const orchestratorBusy = signal(false);
  async function openProjectOrchestrator() {
    const project = projectId();
    if (remoteProjectKey(project)) { if (!orchestratorBusy() && !switching()) { orchestratorBusy.set(true); try { await remoteActions.orchestrator(); } catch (error) { if (alive) notice.set(String(error).replace(/^(?:Error: )?(?:invalid|io|refused|broken|unavailable): /u, "")); } finally { orchestratorBusy.set(false); } } return; }
    if (orchestratorBusy() || switching() || !UUID.test(project)) return;
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
    if (busy() || switching() || panelState.pending()) return;
    busy.set(true);
    const before = new Set(snapshot().panes.flatMap((pane) => pane.tabs.map((tab) => tab.id)));
    const tree = panelState.state().worktrees.find((tree) => tree.id === panelState.selectedWorktree() && tree.projectId === projectId());
    try {
      if (remoteProjectKey(projectId()) && !split) { await remoteActions.create(kind); return; }
      if (kind !== "shell") {
        const project = projectId(), generation = panelState.generation();
        const shell = await Sessions.createHarness({ projectId: project, ...(tree ? { worktreeId: tree.id, cwd: tree.path } : {}) }, kind);
        if (alive && project === projectId() && generation === panelState.generation()) await openShell(shell.id);
        return;
      }
      install(await session.newShell({ ...(split ? { split } : {}), ...(tree ? { worktreeId: tree.id, cwd: tree.path } : {}) }));
      const created = snapshot().panes.flatMap((pane) => pane.tabs).find((tab) => tab.kind === "shell" && !before.has(tab.id));
      if (created) pendingFocus.set(created.id.slice(6));
    } catch (error) { if (alive) notice.set(String(error)); }
    finally { if (alive) busy.set(false); thaw(); }
  }
  async function createSubshell(parentShellId = selected()?.startsWith("shell:") ? selected()!.slice(6) : undefined) {
    if (localOnly()) return;
    if (!parentShellId || busy() || switching()) return;
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
    if (busy() || switching()) return;
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
  function showVimBar() { if (!localOnly()) { vimPath.set(""); vimSuggestions.set([]); vimBar.set(true); } }
  function showNewChat() {
    if (localOnly() || busy() || switching()) return;
    const locations = chatLocations();
    if (!locations.length) { notice.set("Choose an available project or worktree folder before starting a chat."); return; }
    if (snapshot().panes.every((pane) => pane.locked)) { notice.set("Unlock a pane to open a new chat."); return; }
    openSetup({ kind: "chat", worktreeId: panelState.selectedWorktree() ?? undefined, cwd: filesRoot() ?? locations[0].cwd, locations, provider: newChatProvider() });
  }
  /** New chat and Hand off open a tab of their own that holds their choices; starting turns
   * it into the chat (or opens the new shell), and closing it cancels. */
  function openSetup(setup: SetupTab) {
    const id = `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 10)}`;
    setups.set(id, setup);
    action(() => session.openPendingChat(id));
  }
  function finishSetup(panelId: string, kind: string, id: string, owner?: string | null) {
    setups.delete(panelId.slice("chat-pending:".length));
    const open = !!findPanel(snapshot(), panelId);
    if (open && kind === "chat" && (!owner || owner === projectId())) { action(() => session.resolvePendingChat(panelId, id)); return; }
    if (open) action(() => session.closePanel(panelId, false));
    showCreatedSession(kind, id, owner);
  }
  function closeSetup(panelId: string) {
    setups.delete(panelId.slice("chat-pending:".length));
    if (findPanel(snapshot(), panelId)) action(() => session.closePanel(panelId, false));
  }
  function chatLocations(): NewChatLocation[] {
    const project = panelState.state().projects.find((item) => item.id === projectId());
    if (!project) return [];
    return [{ label: "Project root", cwd: project.root }, ...panelState.state().worktrees
      .filter((tree) => tree.projectId === project.id && tree.present)
      .map((tree) => ({ label: tree.branch, cwd: tree.path, worktreeId: tree.id }))];
  }
  function newChatProvider(): "codex" | "claude" | "muse" {
    const provider = nativeChatUsage()?.provider;
    return provider === "claude" || provider === "muse" ? provider : "codex";
  }
  function showHandoff() {
    if (localOnly() || busy() || switching()) return;
    const tab = selected();
    if (!tab || (!tab.startsWith("shell:") && !tab.startsWith("chat:"))) return;
    openSetup({ kind: "handoff", sourceId: tab.slice(tab.indexOf(":") + 1) });
  }
  function submitVim(path = vimPath()) {
    if (!path) return;
    vimBar.set(false);
    void openEditor(path);
  }
  /** As a path is typed, the entries of its folder that start with its last part. */
  let vimRead = 0;
  async function suggestVim(typed: string) {
    vimPath.set(typed);
    const root = filesRoot(), token = ++vimRead;
    const slash = typed.lastIndexOf("/"), folder = typed.slice(0, slash + 1), start = typed.slice(slash + 1).toLowerCase();
    if (!root || typed.startsWith("/") || typed.includes("..")) { vimSuggestions.set([]); return; }
    try {
      const listing = await Files.listDirectory(root, folder ? `${root}/${folder.replace(/\/$/, "")}` : root, false);
      if (token !== vimRead || !alive) return;
      vimSuggestions.set(listing.entries.filter((entry) => entry.name.toLowerCase().startsWith(start))
        .slice(0, 8).map((entry) => `${folder}${entry.name}${entry.kind === "directory" ? "/" : ""}`));
    } catch { if (token === vimRead && alive) vimSuggestions.set([]); }
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
    if (id === "find" && active?.startsWith("panel:")) {
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
    renaming.set(null); vimBar.set(false); closedTab.set(null); allTabs.set(false); allTabsPane.set(null);
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

  function focusActive() {
    const id = selected();
    if (id) bodyRefs.get(id)?.focus();
  }
  function install(next: unknown, refreshMetadata = true) {
    // Only session results may be installed. The reported DockArea tree is never
    // assigned to this signal; all gestures are replayed through riwork-core.
    if (!alive || !next) return;
    dock.set(next as DockAreaLayout);
    if (refreshMetadata) snapshot.set(session.snapshot() as WorkspaceSnapshot);
    // YNode refs own their subtree in NSH. Keep refs for every placed panel,
    // including hidden tabs and moves, but release removed panels promptly so
    // their native component can be collected while siblings remain mounted.
    const placed = new Set(panelIds(dock()!.center));
    for (const id of bodyRefs.keys()) if (!placed.has(id)) bodyRefs.delete(id);
    for (const id of terminalControls.keys()) if (!placed.has(id)) terminalControls.delete(id);
    if (Object.keys(chatUsage()).some((id) => !placed.has(id))) chatUsage.set(Object.fromEntries(Object.entries(chatUsage()).filter(([id]) => placed.has(id))));
    for (const key of tabRefs.keys()) if (!placed.has(key.slice(key.indexOf(":") + 1))) tabRefs.delete(key);
    if (snapshot().notice) notice.set(snapshot().notice!);
    if (!dockReady()) dockReady.set(true);
    if (snapshot().focusMode || !shared()) allTabs.set(false);
    props.onChange?.(snapshot(), dock()!);
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
  function terminalTargets() {
    const targets = new Map<string, { id: string; label: string }>();
    for (const pane of snapshot().panes) for (const tab of pane.tabs) {
      if (tab.kind === "shell" && tab.id.startsWith("shell:")) targets.set(tab.id, { id: tab.id, label: displayTitle(tab) });
    }
    return [...targets.values()].sort((a, b) => a.label.localeCompare(b.label) || a.id.localeCompare(b.id));
  }
  async function copyToTerminal(id: string, text: string) {
    const current = () => alive && terminalTargets().some((target) => target.id === id);
    if (!current()) throw new Error("That terminal is no longer open. Choose a terminal again.");
    await Clipboard.writeText(text);
    if (!current()) throw new Error("That terminal is no longer open. Choose a terminal again.");
    select(id);
    notice.set("Snippet copied. Paste it in this terminal when ready.");
  }
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
    if (first) action(() => session.dock(next));
    else if (!switching()) {
      try {
        // setExtent changes only pane geometry. Replacing otherwise identical
        // tab metadata recreates the renderer's child icons on every frame.
        install(session.setExtent(next), false);
      } catch (error) { notice.set(String(error)); }
    }
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
    if (id.startsWith("chat-pending:")) { closeSetup(id); return; }
    const choice = closeChoice(snapshot(), tab);
    const saved = session.panel(id) as { kind?: string; shell_id?: string; chat_id?: string } | null;
    action(() => session.closePanel(id, choice === "exit"));
    // Undo reopens this Mac's shells and chats; a remote shell's tab is reopened from its host.
    const kind = saved?.kind === "shell" && saved.shell_id ? "shell" : saved?.kind === "chat" && saved.chat_id && UUID.test(saved.chat_id) ? "chat" : null;
    if (choice !== "exit" && kind) showClosed({ title: displayTitle(tab), kind, id: (saved!.shell_id ?? saved!.chat_id)!, end: choice === "ask" });
  }
  let closedTimer: ReturnType<typeof setTimeout> | undefined;
  function showClosed(closed: NonNullable<ReturnType<typeof closedTab>>) {
    closedTab.set(closed);
    if (closedTimer) clearTimeout(closedTimer);
    closedTimer = setTimeout(() => { if (alive && closedTab() === closed) closedTab.set(null); }, 15000);
  }
  function undoClose() {
    const closed = closedTab();
    closedTab.set(null);
    if (!closed) return;
    if (closed.kind === "shell") void openShell(closed.id);
    else action(() => session.openChat(closed.id));
  }
  function endClosed() {
    const closed = closedTab();
    closedTab.set(null);
    if (closed) void session.endSession(`${closed.kind}:${closed.id}`).catch((error) => { if (alive) notice.set(String(error)); });
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
      const flexible = node.children.filter((_, index) => node.sizes?.[index] == null).length;
      for (let i = 0; i < node.children.length; i++) {
        const w = node.axis === "horizontal" ? (node.sizes?.[i] ?? Math.max(0, available - fixed - M.divider * Math.max(0, node.children.length - 1)) / Math.max(1, flexible)) : available;
        const found = nodeWidth(node.children[i], w);
        if (found !== undefined) return found;
      }
    };
    return dock() ? nodeWidth(dock()!.center, size?.width ?? width()) ?? width() : width();
  }
  function headerWidth(id: number) {
    return (paneWidth(id) - controlInset(id)) / (header() / M.header);
  }
  function controlInset(id: number) {
    if (!dock() || chrome.fullscreen()) return 0;
    const top = topPaneFrames(dock()!.center, measured()).find((pane) => pane.id === id);
    return top && top.x < CONTROL_INSET ? Math.min(CONTROL_INSET - top.x, Math.max(0, top.width - space(M.menuButton, theme().scale))) : 0;
  }
  function controlsWidth(id: number) {
    const buttons = paneButtons(id);
    const button = M.nativeButton + M.buttonGap;
    return space(M.menuButton + 12 + (buttons.lock ? button : 0) + (buttons.focus ? button : 0) + (buttons.main ? button : 0), theme().scale);
  }
  function paneButtons(id: number) {
    const pane = snapshot().panes.find((pane) => pane.id === id);
    const width = headerWidth(id);
    const main = !!pane?.main && width >= mainMarkerThreshold(snapshot().theme, snapshot().panelTabIcons);
    let lock = width >= M.lockAt, focus = width >= M.focusAt;
    const panels = shared() || snapshot().panelTabIcons ? pane?.tabs.filter((tab) => tab.kind === "panel").length ?? 0 : 0;
    const needed = () => panels * M.panelCell + M.menuButton + 12 + ((main ? 1 : 0) + (lock ? 1 : 0) + (focus ? 1 : 0)) * (M.nativeButton + M.buttonGap);
    if (panels && needed() > width) focus = false;
    if (panels && needed() > width) lock = false;
    return { main, lock, focus };
  }
  function panelCellWidth(id: number) {
    const count = snapshot().panes.find((pane) => pane.id === id)?.tabs.filter((tab) => tab.kind === "panel").length ?? 1;
    return Math.min(space(M.panelCell, theme().scale), Math.max(space(18, theme().scale), (paneWidth(id) - controlInset(id) - controlsWidth(id)) / Math.max(1, count)));
  }
  function paneOverflow(id: number) {
    const tabs = snapshot().panes.find((pane) => pane.id === id)?.tabs ?? [];
    return tabs.reduce((total, tab) => total + (tabWidths()[`${id}:${tab.id}`] ?? space(tab.kind === "panel" ? M.panelCell : sessionCellWidth(tab.title), theme().scale)), 0)
      > paneWidth(id) - controlInset(id) - controlsWidth(id);
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
    const setup = setups.get(chatProps.panelId.slice("chat-pending:".length));
    const shown = () => isVisible(chatProps.panelId);
    return <View testID={`panel:${chatProps.panelId}`} style={{ flex: 1, minHeight: 0 }}>
      {!chatId() && pending() && setup?.kind === "chat" && <NewChat theme={theme} projectId={projectId()} worktreeId={setup.worktreeId} cwd={setup.cwd} defaults={props.settings?.()?.newChatDefaults}
        locations={setup.locations} provider={setup.provider} visible={shown} onError={(message) => notice.set(message)}
        onCreated={(id, owner) => finishSetup(chatProps.panelId, "chat", id, owner)} onCancel={() => closeSetup(chatProps.panelId)} />}
      {!chatId() && pending() && setup?.kind === "handoff" && <HandoffSetup sourceId={setup.sourceId} theme={theme} visible={shown}
        onDone={(result) => finishSetup(chatProps.panelId, result.kind, result.id, result.projectId)} onCancel={() => closeSetup(chatProps.panelId)}
        onError={(message) => notice.set(message)} onNotice={(message) => notice.set(message)} />}
      {ready() && <ErrorBoundary fallback={(error, retry) => <ChatFailure error={error} retry={retry} theme={theme} />}><ChatTab chatId={chatId()!} visible={isVisible(chatProps.panelId)} active={selected() === chatProps.panelId && !terminalFrozen()} theme={theme}
        onUsage={(usage) => recordChatUsage(chatProps.panelId, usage)}
        terminalTargets={terminalTargets} onCopyToTerminal={copyToTerminal}
        onLink={(target, line) => openLink({ target, line, kind: "file", action: "open" })}
        onOpenWorker={(id, kind) => { void openWorker(id, kind); }}
        onApiKeySettings={() => { revealSettingsKey = !settingsControl; action(() => session.openPanel("settings")); settingsControl?.revealApiKey(); }}
        onEditMemo={(owner) => {
          if (owner !== projectId()) { notice.set("Open that chat's project to edit its memo."); return; }
          revealProjectMemo = !projectSettingsControl;
          action(() => session.openPanel("project_settings"));
          projectSettingsControl?.revealMemo();
        }}
        onClose={() => action(() => session.closePanel(chatProps.panelId, false))} /></ErrorBoundary>}
      {!ready() && !(pending() && setup && !chatId()) && <View style={{ flex: 1, padding: 16, gap: 8 }}>
        <Text style={textStyle()}>{pending() ? "Starting chat…" : chatProps.title}</Text>
        {!pending() && !chatId() && <Text style={mutedStyle()}>This view is unavailable.</Text>}
      </View>}
    </View>;
  }
  function paneMenu(id: number) {
    const pane = snapshot().panes.find((pane) => pane.id === id);
    if (!pane) return;
    action(() => session.selectPane(id));
    // What is used most stays on top; the rest is grouped in submenus.
    const paneRows: MenuItem[] = [];
    if (!pane.locked) {
      paneRows.push({ label: "Close tab", shortcut: "mod+w", disabled: !pane.activeTab, onSelect: () => close(pane.activeTab) });
      paneRows.push({ label: "Close pane", shortcut: "mod+shift+w", onSelect: () => closePane(id) });
    }
    paneRows.push({ label: "Main pane", checked: pane.main, onSelect: () => action(() => session.toggleMain(id)) });
    if (snapshot().panes.some((p) => p.main)) paneRows.push({ label: "Gather tabs into main pane", shortcut: "mod+shift+m", onSelect: () => action(() => session.gather()) });
    if (!paneButtons(id).lock) paneRows.push({ label: pane.locked ? "Unlock pane" : "Lock pane", onSelect: () => action(() => session.toggleLock(id)) });
    if (!paneButtons(id).focus) paneRows.push({ label: snapshot().focusMode ? "Restore layout" : "Focus tab", shortcut: "mod+shift+f", onSelect: () => action(() => session.toggleFocus()) });
    popup([
      { label: "Shell", shortcut: "mod+t", onSelect: () => { void createShell(); } },
      { label: "New chat…", shortcut: "mod+alt+shift+c", onSelect: () => { commands.dispatch("newChat"); } },
      { label: "Split right", shortcut: "mod+d", onSelect: () => { void createShell("right"); } },
      { label: "Split down", shortcut: "mod+shift+d", onSelect: () => { void createShell("down"); } },
      { type: "separator" },
      { label: "New", submenu: [
        ...[["Codex", "mod+shift+c"], ["Claude", "mod+shift+l"], ["Grok", "mod+shift+g"], ["Muse", "mod+shift+u"]].map(([label, shortcut]): MenuItem => ({ label, shortcut, onSelect: () => { void createShell(undefined, label.toLowerCase()); } })),
        { type: "separator" },
        { label: "Subshell", disabled: !selected()?.startsWith("shell:"), onSelect: () => { void createSubshell(); } },
        { label: "Open file in Vim…", onSelect: showVimBar },
        { label: "App preview…", disabled: !filesRoot() && !snapshot().remoteProject, onSelect: newAppPreview },
      ] },
      { label: "Sessions", submenu: [
        { label: "Existing sessions", shortcut: "mod+alt+s", onSelect: () => { panelState.sessionFilter.set("all"); view("shells"); } },
        { label: "Existing Claude chats", onSelect: () => { panelState.sessionFilter.set("claude"); view("shells"); } },
        { label: "Existing Muse chats", onSelect: () => { panelState.sessionFilter.set("muse"); view("shells"); } },
        { type: "separator" },
        { label: "Hand off…", disabled: !selected()?.startsWith("shell:") && !selected()?.startsWith("chat:"), onSelect: showHandoff },
      ] },
      { label: "Orchestrators", submenu: [
        { label: "Global", shortcut: "mod+shift+o", onSelect: openGlobalOrchestrator },
        { label: "Project", shortcut: "mod+alt+o", disabled: !UUID.test(projectId()) && !remoteProjectKey(projectId()), onSelect: () => { void openProjectOrchestrator(); } },
      ] },
      { label: "Views", submenu: VIEWS.map((panel): MenuItem => ({ label: panelTitle(panel), shortcut: VIEW_SHORTCUTS[panel], onSelect: () => view(panel) })) },
      { label: "Pane", submenu: paneRows },
    ]);
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
      { label: "Rename…", onSelect: () => startRename(tab) },
      { type: "separator" },
      { label: "Move left", disabled: index <= 0, onSelect: () => action(() => session.moveTab(id, -1)) },
      { label: "Move right", disabled: index < 0 || index === tabs.length - 1, onSelect: () => action(() => session.moveTab(id, 1)) },
      { type: "separator" }, { label: "Close", onSelect: () => close(id) },
      { type: "separator" }, { label: "Cancel", onSelect: focusActive },
    ]);
  }
  /** Only a shared session tab has a title of its own (its menu offers Rename…). */
  function renameable(id: string) {
    const tab = findPanel(snapshot(), id);
    return !!tab && tab.kind !== "panel" && tab.shared !== false && (snapshot().sharedTabs ?? []).some((entry) => entry.id === id);
  }
  function startRename(tab: PanelModel) { renameTitle.set(tab.title); renaming.set(tab); }
  function saveRename() {
    const tab = renaming();
    if (!tab) return;
    const title = renameTitle();
    renaming.set(null);
    if (title !== tab.title) action(() => session.renameTab(tab.id, title));
  }

  const commands = workspaceCommands(session, snapshot, action, {
    blocked: () => switching(),
    previewOwnsEscape: () => !!previewFocus() && isVisible(previewFocus()!) && !frozen() &&
      !statusMenu() && !props.modalOpen?.() && !switching(),
    newShell: (split) => { allTabs.set(false); void createShell(split); },
    newAgent: (kind) => { allTabs.set(false); void createShell(undefined, kind); },
    newChat: showNewChat,
    newProject: () => { panelState.editor.set({ kind: "project" }); },
    handOff: showHandoff,
    existingSessions: (provider) => { panelState.sessionFilter.set(provider === "claude" ? "claude" : provider === "muse" ? "muse" : "all"); view("shells"); },
    newSubshell: () => { allTabs.set(false); void createSubshell(); },
    remoteAgent: (kind) => { if (!remoteProjectKey(projectId())) return false; void createShell(undefined, kind); return true; },
    remoteRefusal: (id) => {
      if (!remoteProjectKey(projectId())) return false;
      notice.set(id === "handOff" ? remoteLocalOnly(remote.data(), projectId(), "handoff")! : "Chats run on this Mac. Open a project here to start one.");
      return true;
    },
    openVim: showVimBar,
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
      const tab = pane && numberedTab(pane, n, shared());
      if (tab) select(tab.id);
    },
    cycle: (direction) => {
      const pane = activePane(snapshot());
      const tab = pane && cycledTab(pane, pane.activeTab, direction);
      if (tab) select(tab.id);
    },
    escape: () => {
      if (renaming()) { renaming.set(null); return; }
      if (panelState.editor() && !panelState.pending()) { panelState.editor.set(null); return; }
      if (panelState.closingShell() && !panelState.pending()) { panelState.closingShell.set(null); return; }
      if (vimBar()) { vimBar.set(false); focusActive(); return; }
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
    if (panelState.editor() || switching()) return;
    find("find");
  }));
  cleanup(keymap.bind("mod+g", () => { if (!panelState.editor()) find("findNext"); }));
  cleanup(keymap.bind("mod+shift+g", () => {
    if (panelState.editor()) return;
    if (selected() && terminalControls.get(selected()!)?.finding()) find("findPrevious");
    else commands.dispatch("openGrok");
  }));
  cleanup(keymap.bind("shift+f10", () => { if (selected()) tabMenu(selected()!); }));
  effect(() => {
    if (!allTabs()) return;
    for (const [key, direction] of [["down", 1], ["up", -1]] as const) cleanup(keymap.bind(key, () => {
      if (sessions().length) allIndex.set((allIndex() + direction + sessions().length) % sessions().length);
    }));
    cleanup(keymap.bind("enter", () => { const tab = sessions()[allIndex()]; if (tab) select(tab.id); }));
  });

  function paneControls(id: number) {
    const pane = () => snapshot().panes.find((pane) => pane.id === id);
    const control = () => ({ width: space(M.nativeButton, theme().scale), height: space(M.nativeButton, theme().scale), borderRadius: M.buttonRadius, justifyContent: "center" as const, alignItems: "center" as const });
    return <View desktop={{ windowDrag: !!dock() && topPaneFrames(dock()!.center, measured()).some((frame) => frame.id === id) }} style={{ flexDirection: "row", alignItems: "center", gap: M.buttonGap, paddingRight: 3, flexShrink: 0 }}>
      {(paneOverflow(id) || (allTabs() && allTabsPane() === id)) && <Popover open={allTabs() && allTabsPane() === id}
        onOpenChange={(open) => {
          if (open) { action(() => session.selectPane(id)); allTabsPane.set(id); allTabs.set(true); hovered.set(null); allIndex.set(Math.max(0, sessions().findIndex((tab) => tab.id === pane()?.activeTab))); freeze(); }
          else { allTabs.set(false); allTabsPane.set(null); thaw(); }
        }} placement="bottom" align="end" popupStyle={{ width: M.allTabsWidth, maxHeight: M.allTabsMaxHeight, backgroundColor: theme().palette.panel, padding: 4 }}
        trigger={<View testID={`strip-all-tabs:${id}`} accessibilityLabel="All tabs" tooltip="All tabs" style={control()}><Icon name="down" color={theme().palette.muted} /></View>}>
        <Scroll style={{ maxHeight: M.allTabsMaxHeight }}>
          {sessions().map((tab, index) => <View role="menuitem" accessibilityLabel={tab.title} onPress={() => { allTabsPane.set(null); select(tab.id); }}
            style={{ height: space(28, theme().scale), paddingHorizontal: 8, gap: 8, flexDirection: "row", alignItems: "center", backgroundColor: allIndex() === index ? theme().palette.panelActive : theme().palette.panel }}>
            <StatusDot status={tab.status} theme={theme()} /><Text style={{ ...textStyle(), flex: 1 }}>{displayTitle(tab)}</Text>
            {pane()?.activeTab === tab.id && <Icon name="check" color={theme().palette.text} size={12} />}
          </View>)}
        </Scroll>
      </Popover>}
      {dock() && topPaneFrames(dock()!.center, measured()).some((frame) => frame.id === id) && <View desktop={{ windowDrag: true }} style={{ width: 12, height: header() }} />}
      {paneButtons(id).main && <View testID={`pane-main-${id}`} role="button" tooltip="Main pane" accessibilityLabel="Unset main pane" onPress={() => action(() => session.toggleMain(id))} style={control()} hoverStyle={{ backgroundColor: theme().palette.panelActive }}><Icon name="star" color={theme().palette.text} /></View>}
      {paneButtons(id).lock && <View testID={`pane-lock-${id}`} role="button" tooltip={pane()?.locked ? "Unlock pane" : "Lock pane"} accessibilityLabel={pane()?.locked ? "Unlock pane" : "Lock pane"} onPress={() => action(() => session.toggleLock(id))} style={control()} hoverStyle={{ backgroundColor: theme().palette.panelActive }}><Icon name={pane()?.locked ? "lock" : "unlock"} color={pane()?.locked ? theme().palette.text : theme().palette.muted} /></View>}
      {paneButtons(id).focus && <View testID={`pane-focus-${id}`} role="button" tooltip={snapshot().focusMode ? "Restore layout · ⌘⇧F" : "Focus tab · ⌘⇧F"} accessibilityLabel={snapshot().focusMode ? "Restore layout" : "Focus tab"} onPress={() => { action(() => session.selectPane(id)); commands.dispatch("toggleFocusMode"); }} style={control()} hoverStyle={{ backgroundColor: theme().palette.panelActive }}><Icon name={snapshot().focusMode ? "restore" : "focus"} color={theme().palette.muted} /></View>}
      <View testID={`pane-menu-${id}`} role="button" tooltip="Pane menu" accessibilityLabel="Pane menu" onPress={() => paneMenu(id)} style={{ ...control(), width: space(M.menuButton, theme().scale) }} hoverStyle={{ backgroundColor: theme().palette.panelActive }}><Icon name="more" color={theme().palette.muted} /></View>
    </View>;
  }
  function setupTitle(id: string) {
    const setup = id.startsWith("chat-pending:") ? setups.get(id.slice("chat-pending:".length)) : undefined;
    return setup ? setup.kind === "handoff" ? "Hand off" : "New chat" : undefined;
  }
  function tabCell(tab: PanelModel, active: boolean) {
    const pane = snapshot().panes.find((pane) => pane.tabs.some((panel) => panel.id === tab.id));
    const shownTitle = setupTitle(tab.id) ?? displayTitle(tab);
    const panel = tab.kind === "panel";
    const symbol = panel && (shared() || snapshot().panelTabIcons);
    const ink = active ? theme().palette.text : theme().palette.muted;
    return <View ref={(node) => tabRefs.set(`pane:${tab.id}`, node)} testID={`tab-${tab.id}`} accessibilityLabel={`${shownTitle}${!panel && tab.status ? `, ${tab.status}` : ""}`}
      onContextMenu={() => tabMenu(tab.id)} onHoverIn={() => hovered.set(tab.id)} onHoverOut={() => hovered.set(null)}
      style={{ height: header() - 1, width: symbol ? panelCellWidth(pane?.id ?? snapshot().activePane) : undefined, maxWidth: symbol ? undefined : space(267, theme().scale), minWidth: 0, flexShrink: 0,
        paddingLeft: symbol ? 0 : space(8, theme().scale), paddingRight: symbol ? 0 : 4, flexDirection: "row", alignItems: "center", justifyContent: symbol ? "center" : undefined, gap: 6,
        borderRightWidth: 1, borderColor: theme().palette.divider, backgroundColor: active ? pane?.active ? theme().palette.bg : theme().palette.panelActive : theme().palette.panel }}
      hoverStyle={{ backgroundColor: active ? undefined : theme().palette.panelActive }}>
      {!panel && <StatusDot status={tab.status} theme={theme()} />}
      {symbol ? <Icon name={PANEL_ICONS[tab.id.slice(6)] ?? "file"} color={ink} size={space(13, theme().scale)} /> :
        renaming()?.id === tab.id ? <Input testID="tab-rename-input" accessibilityLabel="Tab title" placeholder="Tab title" value={renameTitle()} autoCorrect={false}
          ref={(node) => node.focus()} onChange={(value) => renameTitle.set(value)} onSubmit={saveRename} onBlur={saveRename}
          style={{ fontFamily: theme().family, fontSize: M.tabFont * theme().scale, color: theme().palette.text, width: space(150, theme().scale), paddingHorizontal: 4, paddingVertical: 1, borderRadius: 4, backgroundColor: theme().palette.bg }} /> :
        <Text numberOfLines={1} style={{ fontFamily: theme().family, fontSize: M.tabFont * theme().scale, fontWeight: active ? "500" : "400", flexShrink: 1, color: ink }}>{remoteMark(tab, shownTitle)}</Text>}
      {!pane?.locked && !symbol && <View testID={`tab-close-${tab.id}`} role="button" tooltip="Close tab · ⌘W" accessibilityLabel={`Close ${tab.title}`} onPress={() => close(tab.id)}
        style={{ width: space(M.tabClose, theme().scale), height: space(M.tabClose, theme().scale), borderRadius: 8, alignItems: "center", justifyContent: "center", opacity: active || hovered() === tab.id ? 1 : 0 }}
        hoverStyle={{ backgroundColor: theme().palette.divider }}><Icon name="close" size={10} color={theme().palette.muted} /></View>}
    </View>;
  }
  // The persistent notice is an overlay that hears Escape before the window
  // keymap; a dialog, rename or tab menu opened over it takes Escape first.
  function escapeTopmost() {
    if (commands.stripOpen()) commands.stripOpen.set(false);
    else if (renaming() || vimBar() || allTabs()) { renaming.set(null); vimBar.set(false); allTabs.set(false); allTabsPane.set(null); thaw(); }
    else dismissNotice();
  }
  const terminalFrozen = () => frozen() || statusMenu() || commands.stripOpen() || switching();
  const paneOf = (id: string) => snapshot().panes.find((pane) => pane.tabs.some((tab) => tab.id === id) || id === `empty:${pane.id}`);

  return (
    <View testID="workspace" ref={(node) => { root = node; }} focusable={true} onLayout={(bounds) => width.set(bounds.size.width)} style={{ flex: 1, backgroundColor: theme().palette.bg }}>
      <View testID="dock-container" onLayout={(bounds) => containerHeight.set(bounds.size.height)} style={{ flex: 1, minHeight: 0, alignItems: "center", paddingBottom: snapshot().focusMode && snapshot().focusCentered ? containerHeight() * M.focusBottomMargin : 0 }}>
        <View style={{ flex: 1, width: "100%", maxWidth: snapshot().focusMode && snapshot().focusCentered ? M.focusMaxWidth : undefined }} onLayout={(bounds) => measure(bounds.size)}>
          {dockReady() && <DockArea testID="workspace-dock" style={{ flex: 1, backgroundColor: theme().palette.panel }} layout={dock()!} compact={false} dividerSize={M.divider}
            panels={panelIds(dock()!.center).map((id) => ({ id, closable: false, zoomable: false, visible: !snapshot().focusMode || !!snapshot().panes.find((pane) => pane.id === snapshot().activePane && (pane.tabs.some((tab) => tab.id === id) || id === `empty:${pane.id}`)) }))}
            onLayoutChange={edit}
            onDragChange={(active) => { dragging.set(active); if (active) { hovered.set(null); freeze(); } else thaw(); }}
            tabBarStyle={{ height: header(), flexDirection: "row", backgroundColor: theme().palette.panel, borderBottomWidth: 1, borderColor: theme().palette.divider, overflow: "hidden" }}
            tabStyle={{ padding: 0, minWidth: 0 }}
            contentStyle={{ flex: 1, backgroundColor: theme().palette.bg }}
            handleStyle={{ backgroundColor: theme().palette.divider, opacity: 0.55 }}
            handleAccessibilityLabel="Pane divider"
            fixedPanels={snapshot().panes.flatMap((pane) => pane.tabs.filter((tab) => tab.kind === "panel").map((tab) => tab.id))}
            draggableTabBars={topPaneFrames(dock()!.center, measured()).flatMap((pane) => pane.id === undefined ? [] : [pane.id])}
            renderTabBarPrefix={(id) => <View desktop={{ windowDrag: true }} style={{ width: controlInset(id), height: header(), flexShrink: 0 }} />}
            renderTabBarSuffix={(id) => paneControls(id)}
            renderDropIndicator={(drop) => <View pointerEvents="none" accessible={false}
              style={{ flex: 1, margin: drop.side || drop.index !== undefined ? 0 : space(8, theme().scale),
                borderRadius: drop.index !== undefined ? 1 : 3,
                borderWidth: drop.index !== undefined ? 0 : 1,
                borderColor: `${theme().palette.cyan}66`,
                backgroundColor: drop.index !== undefined ? theme().palette.cyan : `${theme().palette.cyan}20` }} />}
            renderDragPreview={(tab) => <View style={{ paddingHorizontal: 12, paddingVertical: 7, borderWidth: 1, borderColor: theme().palette.cyan, backgroundColor: theme().palette.panel }}><Text style={textStyle()}>{findPanel(snapshot(), tab.panel) ? displayTitle(findPanel(snapshot(), tab.panel)!) : tab.panel}</Text></View>}
            renderTab={(tab) => <View testID={`tab:${tab.panel}`} ref={(node) => props.onViewRef?.(`tab:${tab.panel}`, node)}
              onLayout={(bounds) => measureTab(tab.group, tab.panel, bounds.size.width)}
              onPress={(event) => tab.panel.startsWith("empty:") ? action(() => session.selectPane(tab.group))
                : event?.clickCount === 2 && renameable(tab.panel) ? startRename(findPanel(snapshot(), tab.panel)!) : select(tab.panel)} onContextMenu={() => tabMenu(tab.panel)}
              style={{ flexDirection: "row", alignItems: "center" }}>
              {!tab.panel.startsWith("empty:") && tabCell(findPanel(snapshot(), tab.panel) ?? { id: tab.panel, title: tab.panel, kind: "empty" }, tab.active)}
            </View>}
            renderPanel={(panel) => <View ref={(node) => { bodyRefs.set(panel.id, node); props.onViewRef?.(`panel:${panel.id}`, node); }} focusable={true} testID={`pane-${paneOf(panel.id)?.id}`} style={{ flex: 1, minHeight: 0 }}
              onPress={() => { action(() => panel.id.startsWith("empty:") ? session.selectPane(Number(panel.id.slice(6))) : session.selectPanel(panel.id)); focusActive(); }}>
              {remoteProject() && panel.id.startsWith("panel:") && remotePanelUnavailable(panel.id.slice(6)) ?
                <RemoteUnavailable hostLabel={remoteHostLabel(remote.data(), remoteProjectKey(projectId())!.host)} panel={panel.id.slice(6)} theme={theme} /> :
                panel.id.startsWith("panel:") && NAVIGATION_PANELS.includes(panel.id.slice(6)) ?
                <View testID={`panel:${panel.id}`} style={{ flex: 1, minHeight: 0 }}>
                  {remoteProject() && panel.id !== "panel:projects" ?
                    <RemoteProjectPanel controller={remote} projectKey={projectId} panel={panel.id.slice(6) as "worktrees" | "tasks" | "shells"}
                      query={panelState.query} selectedWorktree={panelState.selectedWorktree}
                      onQuery={(query) => panelState.query.set(query)}
                      onSearch={(node) => { panelState.searches.set(panel.id, node); return () => { if (panelState.searches.get(panel.id) === node) panelState.searches.delete(panel.id); }; }}
                      onNewShell={() => { void createShell(); }}
                      onWorktree={(id) => { panelState.selectWorktree(id); void remoteActions.worktree(id).catch((error) => notice.set(String(error))); }}
                      onTask={(id) => { panelState.selectTask(id); const task = remote.data().projects.find((project) => project.key === projectId())?.tasks.find((task) => task.id === id); if (task?.worktreeId) panelState.selectWorktree(task.worktreeId); }}
                      onShell={(hostId, shell) => remoteActions.open(hostId, shell)} />
                   : <PanelContent controller={panelState} panel={panel.id.slice(6)} surface={panel.id}
                     projectFooter={panel.id === "panel:projects" && remoteFolders() ?
                       <RemoteProjects controller={remote} query={panelState.query} selected={projectId} order={() => ({ order: panelState.order(), descending: panelState.descending() })} collapsed={panelState.collapsed} onToggleFolder={panelState.toggleFolder}
                         onSelectProject={(key) => { void switchTo(key).catch((error) => notice.set(String(error))); }} /> : undefined} />}
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
                </View> : panel.id === "panel:usage" ?
                <UsagePanel theme={theme} projectId={projectId} visible={() => isVisible(panel.id)} /> : panel.id === "panel:schedules" ?
                <View testID={`panel:${panel.id}`} style={{ flex: 1, minHeight: 0 }}>
                  {props.diagnostics && <MountProbe id={panel.id} visible={isVisible(panel.id)} theme={theme} />}
                  <AutomationsPanel projectId={projectId} worktreeId={panelState.selectedWorktree} visible={() => isVisible(panel.id)} active={() => selected() === panel.id && !terminalFrozen()} theme={theme}
                    openChat={(id) => action(() => session.openChat(id))} />
                </View> : panel.id === "panel:flows" ?
                <View testID={`panel:${panel.id}`} style={{ flex: 1, minHeight: 0 }}>
                  {props.diagnostics && <MountProbe id={panel.id} visible={isVisible(panel.id)} theme={theme} />}
                  <FlowsPanel projectId={projectId} visible={() => isVisible(panel.id)} theme={theme}
                    chats={() => panelState.chats().filter((chat) => chat.projectId === projectId()).map((chat) => ({ id: chat.id, title: chat.title }))}
                    openChat={(id) => action(() => session.openChat(id))} editFile={(path) => void openEditor(path)} />
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
                    focusOnAttach={pendingFocus() === panel.id.slice(6)} colors={terminalColors()} frozen={terminalFrozen()} theme={theme}
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
      <WindowChrome state={chrome} panes={[]} />
      {vimBar() && <View testID="vim-open-bar" style={{ gap: 4, paddingHorizontal: space(10, theme().scale), paddingVertical: space(6, theme().scale), borderTopWidth: 1, borderColor: theme().palette.divider, backgroundColor: theme().palette.panel }}>
        <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
          <Text style={mutedStyle()}>Open in Vim</Text>
          <Input testID="vim-open-path" accessibilityLabel="File path" placeholder="File path in the selected project or worktree" value={vimPath()} autoCorrect={false}
            ref={(node) => node.focus()} onChange={(value) => void suggestVim(value)} onSubmit={() => submitVim()}
            style={{ ...textStyle(), flex: 1, minWidth: 0, paddingHorizontal: 8, paddingVertical: 4, borderRadius: 6, backgroundColor: theme().palette.bg }} />
          <Button testID="vim-open-submit" disabled={!vimPath()} onPress={() => submitVim()}><Text style={textStyle()}>Open</Text></Button>
          <Button testID="vim-open-cancel" onPress={() => { vimBar.set(false); focusActive(); }}><Text style={textStyle()}>Cancel</Text></Button>
        </View>
        {vimSuggestions().length > 0 && <View testID="vim-open-suggestions" style={{ flexDirection: "row", flexWrap: "wrap", gap: 4 }}>
          {vimSuggestions().map((path) => <View testID={`vim-open-suggestion:${path}`} role="button" accessibilityLabel={path}
            onPress={() => path.endsWith("/") ? void suggestVim(path) : submitVim(path)}
            style={{ paddingHorizontal: 7, paddingVertical: 2, borderRadius: 4, backgroundColor: theme().palette.panelActive }} hoverStyle={{ backgroundColor: theme().palette.divider }}>
            <Text style={{ ...textStyle(), fontFamily: theme().monoFamily }}>{path}</Text>
          </View>)}
        </View>}
      </View>}
      <StatusBar session={session} snapshot={snapshot} projectId={projectId} projectLabel={() => remoteTitle(remote.data(), projectId()) ?? panelState.projectLabel()}
        nativeChat={nativeChatUsage}
        settings={props.settings} theme={theme} action={action} onError={(error) => notice.set(String(error))} modalOpen={() => !!props.modalOpen?.() || switching()}
        newShell={props.projectId ? { busy, create: () => { void createShell(); } } : undefined} onMenu={(open) => statusMenu.set(open)}
        openProjectOrchestrator={UUID.test(projectId()) || remoteProjectKey(projectId()) ? () => commands.dispatch("openProjectOrchestrator") : undefined}
        remote={() => { const lists = remote.data().projects.find((row) => row.key === projectId()); const tree = panelState.selectedWorktree();
          return { branch: lists?.worktrees.find((row) => row.id === tree)?.branch ?? null, live: lists?.live ?? null }; }}
        focus={{ mode: () => !!snapshot().focusMode, centered: () => !!snapshot().focusCentered,
          toggle: () => commands.dispatch("toggleFocusMode"), toggleCentered: () => commands.dispatch("focusCentered") }} />
      {closedTab() && <Overlay testID="tab-closed-notice" margin={8}
        style={{ top: notice() !== "" ? 75 : 31, right: 8, width: 420, maxWidth: "100%", padding: 8, backgroundColor: theme().palette.panel, borderColor: theme().palette.divider, borderWidth: 1, flexDirection: "row", alignItems: "center", gap: 10 }}>
        <Text testID="tab-closed-message" role="text" numberOfLines={2} style={{ ...textStyle(), flex: 1 }}>{`Closed ${closedTab()!.title}. ${closedTab()!.kind === "shell" ? "The shell keeps running." : "The chat stays in Sessions."}`}</Text>
        <View testID="tab-closed-undo" role="button" accessibilityLabel="Undo" onPress={undoClose}><Text style={{ ...textStyle(), color: theme().palette.cyan }}>Undo</Text></View>
        {closedTab()!.end && <View testID="tab-closed-end" role="button" accessibilityLabel={closedTab()!.kind === "shell" ? "End the shell" : "Stop the chat"} onPress={endClosed}>
          <Text style={{ ...textStyle(), color: theme().palette.gold }}>{closedTab()!.kind === "shell" ? "End shell" : "Stop chat"}</Text></View>}
        <View testID="tab-closed-dismiss" role="button" accessibilityLabel="Dismiss" onPress={() => closedTab.set(null)}><Text style={mutedStyle()}>×</Text></View>
      </Overlay>}
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
      {tabHint() && !frozen() && <Overlay pointerEvents="none" anchor={tabRefs.get(`pane:${tabHint()!.id}`)} placement="bottom" align="start" offset={4} style={{ paddingHorizontal: 8, paddingVertical: 5, backgroundColor: theme().palette.panelActive, borderRadius: 4 }}><Text style={textStyle()}>{tabHint()!.text}</Text></Overlay>}
    </View>
  );
}
