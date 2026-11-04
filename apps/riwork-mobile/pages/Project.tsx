/** A project's page: the strip of its tabs (`tabs.list`, with `shells.list`, or on a Mac without shared tabs
 * its live terminals and `chats.list`), the selected terminal (`riwork.terminal`, the iOS capture terminal),
 * and its actions: New terminal, Open a worker or shell, Move, Close (only after the person confirms), and
 * the project's native screens. On the iPhone a chat tab opens the native chat (`nav.present` `chat`); on the
 * iPad the page is the window's main area, drawn as the Mac's: no title bar, the strip of tabs at the top of
 * the window (as the Mac's tab cells, with ＋ and ⋯ at its end), and a chat tab drawn in place under it
 * (`riwork.chat`, `chat_view.ts`). The app chords work from the page's keymap, from the terminal's (or the
 * chat's) `command` events while it holds the keyboard, and from Swift's `keyboard.command` (its root key
 * host's Clicks prefix, a chord typed in the native chat). */
import { Scroll, Text, View, cleanup, computed, effect, keymap, signal, untracked, type YNode } from "nsh";
import { AlertDialog, Dialog, type AlertDialogAction } from "nsh/base";
import { APP_CHORDS, appChord, type AppChord } from "../../riwork/app_chords";
import { Icon } from "../../riwork/Icon";
import { revealOffset } from "../../riwork/metrics";
import { PortError } from "../../riwork/client/ports";
import type { HostChat, HostShell, TabEntry, TabUpdate, TabsReply } from "../../riwork/client/types";
import { Terminal, terminalStateMessage, type TerminalEvents } from "../../riwork/terminal_view";
import type { MobileApp, ProjectRoute } from "../app_state";
import { ChatView, type ChatEvents } from "../chat_view";
import { Leading, TabDot, TopRow, type PadChrome, type TabCounts } from "../shell/chrome";
import { Symbol } from "../Symbol";
import type { Kit, Layout } from "../ui";
import { closePlan, cycled, initialTerminal, moveLeft, moveRight, openableEntries, supersedes, workspaceTabs, type WorkspaceTab } from "../workspace_model";
import { NewTerminalForm } from "./NewTerminal";

/** How often the tabs are read again while the page is on screen: each tab's state changes while it is looked at. */
const POLL_MS = 4_000;

/** The tab a page opens on (the iPad, where a chat is drawn in place): the one asked for or remembered, of
 * any kind, else the first terminal, else the first tab. */
function initialTab(tabs: WorkspaceTab[], ...wanted: (string | null | undefined)[]): string | null {
  for (const key of wanted) if (key && tabs.some((tab) => tab.key === key)) return key;
  return tabs.find((tab) => tab.kind === "shell")?.key ?? tabs[0]?.key ?? null;
}

export function ProjectPage(props: {
  app: MobileApp; ui: Kit; layout: Layout; route: ProjectRoute; onTop: () => boolean;
  /** The iPad's window around the page: its sidebar and status bar. */
  pad?: PadChrome;
}) {
  const { app, ui } = props;
  const { Action, Tool, IconButton, Section, Divider, Note, Failure, StatusDot, palette, pt, target, text } = ui;
  const { host: hostId, projectId } = props.route;
  const pad = props.layout === "pad" ? props.pad ?? null : null;
  const workspace = app.services(hostId).workspace!;
  const reply = signal<TabsReply | null>(null), shells = signal<HostShell[]>([]), chats = signal<HostChat[]>([]);
  const listed = signal(false), reading = signal(false);
  /** Why the last read failed (cleared by the next good one), and what the last change said. */
  const readError = signal(""), notice = signal("");
  /** The tab on screen: a terminal (`shell:<id>`), or on the iPad a chat (`chat:<id>`). On the iPhone a chat
   * tab opens over the page and leaves it as it was. */
  const selected = signal<string | null>(null);
  /** The names the terminals and chats give their tabs (`title` events), by shell or chat id. */
  const titles = signal<Record<string, string>>({}), chatTitles = signal<Record<string, string>>({});
  /** The chat component's last \`state\` (iPad), with the chat it is about: a new chat says nothing until its own first one. */
  const chatState = signal<{ chatId: string; state: ChatEvents["state"] } | null>(null);
  const chatStateOf = (id: string) => { const value = chatState(); return value?.chatId === id ? value.state : null; };
  const state = signal<TerminalEvents["state"] | null>(null);
  const creating = signal(false), picking = signal(false), menuOpen = signal(false);
  let requestedTab = props.route.tab, requestNew = props.route.newTerminal === true;
  /** `requestedTab` is only the remembered one: on the iPad, one gone or hidden since is not opened again. */
  let remembered = false;
  /** A tab the person (or Swift) chose: the remembered one no longer applies when it arrives late. */
  let chosen = false;
  /** A terminal just made here stays on screen until the Mac's list has it (or for a while, if it never does). */
  let fresh: { key: string; until: number } | null = null;
  let terminal: YNode | undefined, chat: YNode | undefined, alive = true;
  cleanup(() => { alive = false; });

  const tabs = computed(() => workspaceTabs(reply(), shells(), chats()));
  const features = () => app.hosts().find((info) => info.id === hostId)?.features ?? [];
  /** What the Mac offers, as one value: a change (another connection) reads the tabs again. */
  const offered = computed(() => features().join(" "));
  const connected = () => app.link(hostId).state === "connected";
  const onScreen = computed(() => props.onTop() && !app.blocked());
  /** Nothing of the page's own is over it (a form, the picker, a menu; on the iPad a menu of the window). */
  const free = computed(() => onScreen() && !creating() && !picking() && !menuOpen() && !(pad?.menuOpen() ?? false));
  /** It takes keys: on the iPad also not while the sidebar is open over it (⌘B still closes it). */
  const keysLive = computed(() => free() && !(pad?.overlay() ?? false));
  const current = () => tabs().find((tab) => tab.key === selected()) ?? null;
  const title = (tab: WorkspaceTab) => tab.kind === "chat" ? chatTitles()[tab.id] ?? tab.title
    : !tab.entry ? titles()[tab.id] ?? tab.title : tab.title;

  // Swift asks the page on screen for a tab (a terminal its sheet made) or the form (⌘N on its native screens).
  let seen = app.projectRequest()?.seq ?? 0;
  effect(() => {
    const request = app.projectRequest();
    if (!request || request.seq === seen || request.projectId !== projectId) return;
    seen = request.seq;
    untracked(() => {
      if (request.tab) { requestedTab = request.tab; remembered = false; void read(); }
      if (request.newTerminal) { requestNew = true; settle(); }
      if (request.picker && reply()) picking.set(true);
    });
  });

  // The project's last tab on this device (`SavedDesktop.projectTabKeys`), unless one was asked for or chosen
  // meanwhile: a terminal, or on the iPad also a chat.
  void app.device.prefs.get(`projectTab.${projectId}`).then((value) => {
    if (!alive || chosen || requestedTab || typeof value !== "string" || !(value.startsWith("shell:") || (pad && value.startsWith("chat:")))) return;
    requestedTab = value; remembered = true;
    settle();
  }).catch(() => {});

  function install(next: TabsReply | null) {
    if (next == null) { reply.set(null); return; }
    if (supersedes(next, reply())) reply.set(next);
  }
  /** The tab on screen once the lists are in: the tab asked for, the one remembered, else the first terminal.
   * On the iPhone a chat asked for opens over the page; on the iPad it is chosen as a tab, and only one the
   * lists do not name opens over the page. */
  function settle() {
    if (!listed()) return;
    const quiet = remembered && pad != null;
    remembered = false;
    if (requestedTab?.startsWith("chat:") && onScreen() && !(pad && tabs().some((tab) => tab.key === requestedTab))) {
      const chatId = requestedTab.slice("chat:".length);
      const hidden = openableEntries(reply()).some((entry) => entry.key === requestedTab);
      if (!pad || (!hidden && !quiet)) { requestedTab = undefined; void app.present({ name: "chat", host: hostId, projectId, chatId }); }
    }
    // A terminal the shared list hides (a worker a dashboard row names) is opened, as the picker opens it.
    const hidden = requestedTab && !tabs().some((tab) => tab.key === requestedTab) ? openableEntries(reply()).find((entry) => entry.key === requestedTab) : undefined;
    if (hidden && !quiet) { requestedTab = undefined; chosen = true; void openHidden(hidden); return; }
    if (fresh && (tabs().some((tab) => tab.key === fresh!.key) || Date.now() > fresh.until)) fresh = null;
    const still = tabs().some((tab) => tab.key === selected()) || (fresh != null && fresh.key === selected());
    if (!still || requestedTab) {
      const next = pad ? initialTab(tabs(), requestedTab, selected()) : initialTerminal(tabs(), requestedTab, selected());
      if (requestedTab && next === requestedTab) chosen = true;
      requestedTab = undefined;
      if (next !== selected()) choose(next);
    }
    if (requestNew && connected()) { requestNew = false; creating.set(true); }
  }
  async function read() {
    if (!connected() || reading()) return;
    reading.set(true);
    try {
      const shared = features().includes("tabs");
      const [tabList, shellList, chatList] = await Promise.all([
        workspace.tabs(projectId), workspace.shells(projectId),
        // The shared list names every chat tab with its title and state; without it the chats are listed.
        shared ? Promise.resolve(chats()) : workspace.chats(projectId).catch(() => chats()),
      ]);
      if (!alive) return;
      install(tabList); shells.set(shellList); chats.set(chatList); listed.set(true); readError.set("");
      settle();
    } catch (error) {
      if (alive) readError.set(PortError.from(error).message);
    } finally {
      if (alive) reading.set(false);
    }
  }
  // The reads run untracked: what they set must not run this again.
  effect(() => {
    if (!onScreen() || !connected()) return;
    offered();
    untracked(() => void read());
    const timer = setInterval(() => void read(), POLL_MS);
    return () => clearInterval(timer);
  });
  effect(() => {
    if (!onScreen()) return;
    const refresh = () => void read();
    app.refresher.current = refresh;
    return () => { if (app.refresher.current === refresh) app.refresher.current = null; };
  });

  function choose(key: string | null) {
    if (key === selected()) return;
    selected.set(key); state.set(null);
    if (key) void app.device.prefs.set(`projectTab.${projectId}`, key).catch(() => {});
  }
  /** A terminal tab goes on screen; a chat tab too on the iPad, and on the iPhone it opens the native chat over the page. */
  function activate(tab: WorkspaceTab | null) {
    if (!tab) return;
    chosen = true;
    if (tab.kind === "chat" && !pad) { void app.present({ name: "chat", host: hostId, projectId, chatId: tab.id }); return; }
    choose(tab.key);
  }
  function back() {
    if (props.layout === "pad") app.openDashboard(hostId); else app.back();
  }
  /** An app chord, from the keymap, the terminal, the chat or Swift: only while the page takes keys (no form,
   * picker or menu up); the iPad's ⌘B also while the sidebar is open over the page, which it closes. */
  function run(chord: AppChord | null) {
    if (!chord || !(chord.type === "sidebar" ? free() : keysLive())) return;
    switch (chord.type) {
      case "select": activate(tabs()[chord.index] ?? null); break;
      case "next": activate(cycled(tabs(), selected(), 1)); break;
      case "previous": activate(cycled(tabs(), selected(), -1)); break;
      case "newTerminal": if (connected()) creating.set(true); break;
      case "back": back(); break;
      case "projects": pad?.reveal(); app.openProjects(hostId); break;
      case "display": void app.present({ name: "settings" }); break;
      case "picker": if (reply()) picking.set(true); break;
      case "sidebar": pad?.toggle(); break;
    }
  }
  effect(() => {
    if (!free()) return;
    const bindings: Record<string, () => void> = {};
    // The page's chords while it takes keys; ⌘B on the iPad also while the sidebar is open over it.
    for (const [, keys, chord] of APP_CHORDS) if (keys && (chord.type === "sidebar" ? pad != null : keysLive())) bindings[keys] = () => run(chord);
    const fromSwift = (id: string) => run(appChord(id));
    app.chords.current = fromSwift;
    const unbind = keymap.bind(bindings, { when: "always" });
    return () => { unbind(); if (app.chords.current === fromSwift) app.chords.current = null; };
  });

  // MARK: Changes to the shared list

  async function change(request: () => Promise<TabsReply>): Promise<boolean> {
    // The reply is the whole list: a tab hidden here leaves the screen at once.
    try { install(await request()); settle(); return true; }
    catch (error) {
      const failure = PortError.from(error);
      notice.set(failure.code === "uncertain" ? "The Mac may not have taken that change. The tabs were read again." : failure.message);
      void read();
      return false;
    }
  }
  const update = (value: TabUpdate) => change(() => workspace.updateTabs(projectId, value));
  /** Closing asks first, every time: Detach keeps the session running; Exit (a project terminal) ends it. */
  async function requestClose(tab: WorkspaceTab) {
    const plan = closePlan(tab, shells());
    if (!plan.detach && !plan.exit) { notice.set(tab.orchestrator ? "Stop the orchestrator on the Mac." : "This tab is closed on the Mac."); return; }
    const actions: AlertDialogAction[] = [];
    if (plan.detach) actions.push({ id: "detach", label: "Detach" });
    if (plan.exit) actions.push({ id: "exit", label: tab.entry ? "Exit" : "Close terminal", role: "destructive" });
    actions.push({ id: "cancel", label: "Cancel", role: "cancel" });
    const description = tab.kind === "chat" ? "Detach keeps the chat running on your Mac. Stop it from the chat's menu; its history stays."
      : !tab.entry ? `Close ${title(tab)}? This ends its running process on the Mac.`
      : plan.exit ? "Detach keeps the shell running on your Mac. Exit closes it and ends its process."
      : "Detach keeps it running on your Mac. Stop it on the Mac.";
    menuOpen.set(true);
    const choice = await AlertDialog.show({ title: `Close ${title(tab)}?`, description, actions }).catch(() => null);
    menuOpen.set(false);
    if (!alive || choice === "cancel" || choice == null) return;
    if (choice === "detach" && tab.entry) await update({ action: "hide", key: tab.key });
    if (choice === "exit") await exit(tab);
  }
  async function exit(tab: WorkspaceTab) {
    // Hidden first, as the Mac's Exit: a hide that fails stops here and nothing is ended.
    if (tab.entry && !(await update({ action: "hide", key: tab.key }))) return;
    try { await workspace.closeShell(tab.id); }
    catch (error) {
      const failure = PortError.from(error);
      if (failure.code !== "not_found") notice.set(failure.code === "uncertain" ? "The Mac may have closed this terminal; its answer was lost. The tabs were read again." : failure.message);
    }
    void read();
  }
  /** A tab's own menu (a long press, or ⋯ for the tab on screen). */
  async function tabMenu(tab: WorkspaceTab) {
    const shared = reply();
    const left = tab.entry && shared ? moveLeft(tab.entry, shared) : null, right = tab.entry && shared ? moveRight(tab.entry, shared) : null;
    const plan = closePlan(tab, shells());
    const actions: AlertDialogAction[] = [];
    if (tab.kind === "chat") actions.push({ id: "open", label: "Open chat" });
    if (left) actions.push({ id: "left", label: "Move left" });
    if (right) actions.push({ id: "right", label: "Move right" });
    if (plan.detach || plan.exit) actions.push({ id: "close", label: tab.worker ? "Close (detach)" : "Close tab…", role: "destructive" });
    actions.push({ id: "cancel", label: "Cancel", role: "cancel" });
    menuOpen.set(true);
    const choice = await AlertDialog.show({ title: title(tab), presentation: "actionSheet", actions }).catch(() => null);
    menuOpen.set(false);
    if (!alive) return;
    if (choice === "open") activate(tab);
    else if (choice === "left" && left) await update(left);
    else if (choice === "right" && right) await update(right);
    else if (choice === "close") await requestClose(tab);
  }
  /** The page's menu: the tab on screen and its terminal (or chat), new tabs, then the project's native screens. */
  async function pageMenu() {
    const tab = current(), openable = openableEntries(reply()).length, live = shellId() != null;
    const actions: AlertDialogAction[] = [{ id: "new", label: "New terminal" }];
    if (reply()) actions.push({ id: "open", label: openable ? `Open a worker or shell… (${openable})` : "Open a worker or shell…" });
    if (tab) actions.push({ id: "tab", label: `${title(tab)}…` });
    // The chat's own actions (its `menu` command: model, mode, notices, stop), as its ⋯ had them on the native screen.
    if (chatId() != null && chatStateOf(chatId()!)?.state === "ready") actions.push({ id: "chat", label: "Chat actions…" });
    // The terminal's own (schema 1 commands): the capture terminal has no selection, so this is how its text leaves the device.
    if (live) actions.push({ id: "copy", label: "Copy screen text" }, { id: "reattach", label: "Refresh output" });
    if (features().includes("project_memo")) actions.push({ id: "memo", label: "Memo…" });
    actions.push({ id: "global", label: "Global orchestrator" }, { id: "display", label: "Display settings" }, { id: "refresh", label: "Refresh tabs" });
    actions.push({ id: "cancel", label: "Cancel", role: "cancel" });
    menuOpen.set(true);
    const choice = await AlertDialog.show({ title: props.route.name, presentation: "actionSheet", actions }).catch(() => null);
    menuOpen.set(false);
    if (!alive) return;
    switch (choice) {
      case "new": creating.set(true); break;
      case "open": picking.set(true); break;
      case "tab": if (tab) await tabMenu(tab); break;
      case "chat": chat?.command("menu"); break;
      case "copy": terminal?.command("copy"); break;
      case "reattach": terminal?.command("reattach"); break;
      case "memo": void app.present({ name: "memo", host: hostId, projectId }); break;
      case "global": void app.present({ name: "global", host: hostId }); break;
      case "display": void app.present({ name: "settings" }); break;
      case "refresh": void read(); break;
    }
  }
  async function openHidden(entry: TabEntry) {
    picking.set(false);
    if (!(await change(() => workspace.openTab(projectId, entry.key)))) return;
    const tab = tabs().find((item) => item.key === entry.key);
    if (tab) activate(tab);
  }

  // MARK: Drawing

  function TabCell(cell: { tab: WorkspaceTab }) {
    const isSelected = () => cell.tab.key === selected();
    const waiting = () => cell.tab.status === "waiting";
    const symbol = cell.tab.kind === "chat" ? "bubble.left.and.text.bubble.right" : cell.tab.orchestrator ? "point.3.connected.trianglepath.dotted" : "terminal";
    return <View testID={`tab:${cell.tab.key}`} role="tab" accessibilityLabel={`${title(cell.tab)}${cell.tab.status !== "unknown" ? `, ${cell.tab.status}` : ""}`}
      accessibilityState={{ selected: isSelected() }} accessibilityActions={[{ name: "close", label: "Close tab" }, { name: "menu", label: "Tab actions" }]}
      onAccessibilityAction={(event) => { if (event.actionName === "close") void requestClose(cell.tab); else void tabMenu(cell.tab); }}
      disabled={app.blocked()} onPress={() => activate(cell.tab)} onLongPress={() => void tabMenu(cell.tab)}
      activeStyle={{ backgroundColor: palette().panelActive }}
      style={{ minHeight: target(), maxWidth: pt(220), paddingHorizontal: pt(12), flexDirection: "row", alignItems: "center", gap: pt(6),
        borderBottomWidth: 2, borderColor: isSelected() ? palette().cyan : waiting() ? palette().gold : "transparent",
        backgroundColor: isSelected() ? palette().panel : "transparent" }}>
      <Symbol name={symbol} color={cell.tab.orchestrator ? palette().magenta : cell.tab.kind === "chat" ? palette().cyan : palette().text} size={pt(14)} />
      <Text numberOfLines={1} style={{ ...text(13, isSelected() ? "text" : "muted"), fontWeight: isSelected() ? "600" : "400", flexShrink: 1 }}>{title(cell.tab)}</Text>
      <StatusDot status={cell.tab.status} />
    </View>;
  }
  function Strip() {
    return <View style={{ flexDirection: "row", alignItems: "center", borderBottomWidth: 0.5, borderTopWidth: 0.5, borderColor: palette().divider, backgroundColor: palette().bg }}>
      <Scroll horizontal={true} scrollbar={false} style={{ flex: 1 }} contentStyle={{ flexDirection: "row" }}>
        <View role="tablist" accessibilityLabel={`${props.route.name} tabs`} style={{ flexDirection: "row" }}>
          {tabs().map((tab) => <TabCell tab={tab} />)}
        </View>
      </Scroll>
      <IconButton label="New terminal" symbol="plus" disabled={app.blocked() || !connected()} action={() => creating.set(true)} />
    </View>;
  }
  function TerminalView(view: { shellId: string }) {
    return <Terminal testID={`terminal:${view.shellId}`} ref={(node) => { terminal = node; }} style={{ flex: 1, minHeight: 0 }}
      target={{ kind: "remote", desktopId: hostId, shellId: view.shellId }}
      visible={onScreen()} pinned={false} focusOnAttach={app.overview().keyboard.hardware} colors={{ source: "ghostty" }} frozen={pad?.overlay() ?? false}
      onState={(next) => { state.set(next); if (next.state === "ended" || next.state === "gone") void read(); }}
      onCommand={(event) => run(appChord(event.id))}
      onTitle={(event) => titles.set({ ...titles(), [view.shellId]: event.title })}
      onInputRefused={(event) => notice.set(event.reason)} />;
  }
  /** A chat tab on the iPad, drawn in place under the strip (`riwork.chat`): visible while the page takes
   * keys; one the Mac cannot show here says why and still opens as the native screen. */
  function ChatPane(view: { chatId: string }) {
    const unavailable = () => chatStateOf(view.chatId)?.state === "unavailable";
    // The component stays laid out and live while it cannot show the chat, so it says when it can again; the note covers it meanwhile.
    return <View style={{ flex: 1, minHeight: 0 }}>
      <ChatView testID={`chat:${view.chatId}`} ref={(node) => { chat = node; }} style={{ flex: 1, minHeight: 0 }}
        target={{ desktopId: hostId, projectId, chatId: view.chatId }} visible={keysLive()} focusOnAttach={app.overview().keyboard.hardware}
        onState={(next) => chatState.set({ chatId: view.chatId, state: next })} onCommand={(event) => run(appChord(event.id))}
        onTitle={(event) => chatTitles.set({ ...chatTitles(), [view.chatId]: event.title })} />
      {unavailable() && <View style={{ position: "absolute", left: 0, top: 0, right: 0, bottom: 0, zIndex: 1, padding: pt(16), backgroundColor: palette().bg }}>
        <Note title="This chat does not open here" message={chatStateOf(view.chatId)?.message ?? "The desktop did not give this chat to this device."}>
          <Action label="Open as a screen" disabled={app.blocked()} action={() => void app.present({ name: "chat", host: hostId, projectId, chatId: view.chatId })} />
        </Note>
      </View>}
    </View>;
  }
  /** A tab on the iPad as the Mac's strip draws it: its state dot and title, the selected one on the
   * content's color with its close ×, the others on the strip's, each set off by the divider. */
  function PadTab(cell: { tab: WorkspaceTab }) {
    const isSelected = () => cell.tab.key === selected();
    return <View ref={(node) => tabRefs.set(cell.tab.key, node)} testID={`tab:${cell.tab.key}`} role="tab" accessibilityLabel={`${title(cell.tab)}${cell.tab.status !== "unknown" ? `, ${cell.tab.status}` : ""}`}
      accessibilityState={{ selected: isSelected() }} accessibilityActions={[{ name: "close", label: "Close tab" }, { name: "menu", label: "Tab actions" }]}
      onAccessibilityAction={(event) => { if (event.actionName === "close") void requestClose(cell.tab); else void tabMenu(cell.tab); }}
      disabled={app.blocked()} onPress={() => activate(cell.tab)} onLongPress={() => void tabMenu(cell.tab)}
      activeStyle={{ backgroundColor: palette().panelActive }}
      style={{ height: ui.bar() - 1, maxWidth: pt(280), flexShrink: 0, flexDirection: "row", alignItems: "center", gap: pt(6), paddingLeft: pt(10),
        paddingRight: isSelected() ? 0 : pt(12), borderRightWidth: 1, borderColor: palette().divider, backgroundColor: isSelected() ? palette().bg : palette().panel }}>
      <TabDot ui={ui} status={cell.tab.status} />
      <Text numberOfLines={1} style={{ ...text(13, isSelected() ? "text" : "muted"), fontWeight: isSelected() ? "500" : "400", flexShrink: 1 }}>{title(cell.tab)}</Text>
      {isSelected() && <View testID={`tab-close:${cell.tab.key}`} role="button" accessibilityLabel={`Close ${title(cell.tab)}`} disabled={app.blocked()}
        onPress={() => void requestClose(cell.tab)} activeStyle={{ backgroundColor: palette().panelActive }}
        style={{ width: pt(28), height: "100%", alignItems: "center", justifyContent: "center" }}>
        <Icon name="close" size={pt(11)} color={palette().muted} />
      </View>}
    </View>;
  }
  const strip = () => state()?.state && state()!.state !== "attached" && state()!.state !== "detached" ? terminalStateMessage(state()!) : "";
  const bottomStrip = () => app.overview().keyboard.hardware;
  /** The shell on screen: it changes only when another terminal is chosen, so a read of the tabs never makes the terminal again. */
  const shellId = computed(() => { const key = selected(); return key?.startsWith("shell:") ? key.slice("shell:".length) : null; });
  /** The chat on screen (the iPad), likewise. */
  const chatId = computed(() => { const key = selected(); return pad && key?.startsWith("chat:") ? key.slice("chat:".length) : null; });
  // The iPad's strip scrolls the tab chosen into view, as the Mac's does (`revealOffset`).
  const tabRefs = new Map<string, YNode>();
  let stripScroll: YNode | undefined, stripX = 0;
  async function reveal(key: string) {
    const node = tabRefs.get(key);
    if (!node || !stripScroll) return;
    const [cell, viewport] = await Promise.all([node.measure(), stripScroll.measure()]);
    if (!alive || selected() !== key || !cell || !viewport) return;
    const start = cell.pagePosition.x - viewport.pagePosition.x + stripX;
    const next = revealOffset(start, cell.bounds.size.width, viewport.bounds.size.width, stripX);
    if (next !== stripX) stripScroll.scrollTo({ x: next, animated: true });
  }
  if (pad) effect(() => { const key = selected(); if (key) untracked(() => void reveal(key)); });
  const empty = () => listed() && !tabs().length;
  const notes = () => !!(notice() || app.failure() || readError() || !connected());

  // The iPad's status bar: this project's tabs by state and its live terminals.
  if (pad) {
    effect(() => {
      const counts: TabCounts = { working: 0, waiting: 0, done: 0, error: 0 };
      for (const tab of tabs()) if (tab.status in counts) counts[tab.status as keyof TabCounts] += 1;
      pad.status.set({ projectId, counts, live: listed() ? shells().filter((shell) => shell.alive).length : null });
    });
    cleanup(() => { if (untracked(pad.status)?.projectId === projectId) pad.status.set(null); });
  }

  function Notes() {
    return <View style={{ paddingHorizontal: notes() ? pt(12) : 0, gap: pt(8), paddingVertical: notes() ? pt(8) : 0 }}>
      <Failure />
      {notice() && <Note message={notice()} error={true}><Action label="Dismiss" compact={true} action={() => notice.set("")} /></Note>}
      {readError() && connected() && <Note message={listed() ? `Saved tabs · ${readError()}` : readError()} error={!listed()} />}
      {!connected() && <Note message={app.link(hostId).error ?? app.link(hostId).label}>
        {app.link(hostId).state !== "connecting" && <Action label="Connect" compact={true} disabled={app.blocked()} action={() => app.connect(hostId)} />}
      </Note>}
    </View>;
  }
  /** The terminal's state while it is not attached, with Reconnect. */
  function TerminalState() {
    return <>{strip() && <View style={{ flexDirection: "row", alignItems: "center", gap: pt(8), paddingHorizontal: pt(12), minHeight: target(), backgroundColor: palette().panel }}>
      <Text style={{ ...text(13, state()?.state === "failed" ? "error" : "muted"), flex: 1 }}>{strip()}</Text>
      {(state()?.state === "failed" || state()?.state === "reconnecting") && <Action label="Reconnect" compact={true} action={() => terminal?.command("reattach")} />}
    </View>}</>;
  }
  /** Where no terminal (or chat) is on screen: why, and the way to one. */
  function Placeholder() {
    return <Scroll style={{ flex: 1 }} contentStyle={{ padding: pt(16), gap: pt(16) }} refreshing={reading() && !listed()} onRefresh={() => void read()}>
      {empty() ? <Note title="No open terminals" message="Open one here, or on your desktop and then refresh.">
        <Action label="New terminal" symbol={pad ? undefined : "plus"} icon={pad ? "add" : undefined} prominent={true} disabled={app.blocked() || !connected()} action={() => creating.set(true)} />
        {openableEntries(reply()).length > 0 && <Action label="Open a worker or shell…" disabled={app.blocked()} action={() => picking.set(true)} />}
      </Note> : listed() ? <Note message="Choose a terminal tab, or open a chat." /> : connected() && <Note message="Reading the project's tabs…" />}
    </Scroll>;
  }

  if (pad) return <View testID={`page:project:${projectId}`} style={{ flex: 1, minWidth: 0, minHeight: 0, backgroundColor: palette().bg }}>
    <TopRow ui={ui}>
      <Leading app={app} ui={ui} chrome={pad} host={hostId} />
      <Scroll ref={(node) => { stripScroll = node; }} onScroll={(event) => { stripX = event.offset.x; }} horizontal={true} scrollbar={false}
        style={{ flex: 1, minWidth: 0, height: "100%" }} contentStyle={{ flexDirection: "row" }}>
        <View role="tablist" accessibilityLabel={`${props.route.name} tabs`} style={{ flexDirection: "row" }}>
          {tabs().map((tab) => <PadTab tab={tab} />)}
        </View>
      </Scroll>
      <View style={{ flexDirection: "row", alignItems: "center", flexShrink: 0, paddingHorizontal: pt(4) }}>
        <Tool testID="new-terminal-button" icon="add" label="New terminal" disabled={app.blocked() || !connected()} action={() => creating.set(true)} />
        <Tool testID="project-menu" icon="more" label="More options" disabled={app.blocked()} action={() => void pageMenu()} />
      </View>
    </TopRow>
    <Notes />
    <TerminalState />
    {shellId() ? [shellId()!].map((id) => <TerminalView shellId={id} />)
      : chatId() ? [chatId()!].map((id) => <ChatPane chatId={id} />)
      : <Placeholder />}
    <Forms />
  </View>;

  return <ui.Page layout={props.layout} title={props.route.name} testID={`page:project:${projectId}`} backTitle={props.route.name}
    right={<IconButton testID="project-menu" label="More options" symbol="ellipsis.circle" disabled={app.blocked()} action={() => void pageMenu()} />}>
    <View style={{ flex: 1, minHeight: 0, backgroundColor: palette().bg }}>
      {!bottomStrip() && tabs().length > 0 && <Strip />}
      <Notes />
      <TerminalState />
      {shellId() ? [shellId()!].map((id) => <TerminalView shellId={id} />) : <Placeholder />}
      {bottomStrip() && tabs().length > 0 && <Strip />}
    </View>
    <Forms />
  </ui.Page>;

  /** The New terminal form and the picker of hidden tabs. */
  function Forms() {
    return <>
      <NewTerminalForm app={app} ui={ui} host={hostId} projectId={projectId} projectName={props.route.name} open={creating()}
        onClose={() => creating.set(false)} onUncertain={() => void read()}
        onCreated={(shell) => {
          shells.set([...shells().filter((item) => item.id !== shell.id), shell]);
          chosen = true; requestedTab = undefined;
          fresh = { key: `shell:${shell.id}`, until: Date.now() + 15_000 };
          choose(fresh.key);
          void read();
        }} />
      <Dialog open={picking()} presentation="pageSheet" onOpenChange={(open) => { if (!open) picking.set(false); }} style={{ backgroundColor: palette().bg }}>
        <View style={{ flexDirection: "row", alignItems: "center", padding: pt(12) }}>
          <Text role="header" style={{ ...text(17), fontWeight: "600", flex: 1, paddingHorizontal: pt(8) }}>Open a worker or shell</Text>
          <Action label="Done" compact={true} action={() => picking.set(false)} />
        </View>
        <Scroll style={{ flex: 1 }} contentStyle={{ padding: pt(16), paddingTop: 0, gap: pt(12) }}>
          {openableEntries(reply()).length ? <Section>
            {openableEntries(reply()).map((entry, index) => <>
              {index > 0 && <Divider />}
              <View role="button" accessibilityLabel={`${entry.title}, ${entry.status}`} onPress={() => void openHidden(entry)}
                activeStyle={{ backgroundColor: palette().panelActive }} style={{ minHeight: target(), padding: pt(12), flexDirection: "row", alignItems: "center", gap: pt(10) }}>
                {pad ? <Icon name={entry.kind === "chat" ? "chat" : "terminal"} color={palette().cyan} size={pt(16)} />
                  : <Symbol name={entry.kind === "chat" ? "bubble.left.and.text.bubble.right" : "terminal"} color={palette().cyan} size={pt(16)} />}
                <View style={{ flex: 1, gap: pt(2) }}>
                  <Text numberOfLines={1} style={text(15)}>{entry.title}</Text>
                  <Text style={text(12, "muted")}>{[entry.worker || entry.parent ? "Worker" : entry.kind === "chat" ? "Chat" : "Shell", entry.status].join(" · ")}</Text>
                </View>
                {pad ? <TabDot ui={ui} status={entry.status} /> : <StatusDot status={entry.status} />}
              </View>
            </>)}
          </Section> : <Note message="Every chat and shell of this project is already a tab." />}
        </Scroll>
      </Dialog>
    </>;
  }
}
