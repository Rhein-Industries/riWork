import { Scroll, Text, View, cleanup, colorScheme, host, keymap, signal } from "nsh";
import { DockArea, type DockAreaLayout, type DockNode } from "nsh/base";
import { Menu, Window } from "nsh/desktop";
import { Appearance, Sessions, Tabs, type AppearanceSnapshot, type LayoutSession, type ShellSessionDto, type Subscription } from "riwork/native";
import { LocalTerminal, type TerminalColors, type TerminalEvents } from "./Terminal";
import {
  activePane, createTitleUpdater, cycledTab, findPanel, mainPaneSummary, numberedTab,
  type WorkspaceSnapshot,
} from "./model";

type Extent = { width: number; height: number };
interface WorkspaceProps {
  session: LayoutSession;
  title: string;
  projectLabel: string;
  projectId?: string;
  startupNotice?: string | null;
  /** In-process observer used by the headless fixture, never a host API. */
  onChange?: (snapshot: WorkspaceSnapshot, dock: DockAreaLayout) => void;
  /** Observe native-node creation in the headless lifecycle regression. */
  onTerminalMount?: (shellId: string) => void;
}

function panels(node: DockNode): string[] {
  return node.type === "tabs" ? node.panels : node.children.flatMap(panels);
}

export function Workspace(props: WorkspaceProps) {
  const session = props.session;
  const snapshot = signal(session.snapshot() as WorkspaceSnapshot);
  const dock = signal<DockAreaLayout | null>(null);
  // A layout object changes on every selection/resize. Mounting under dock()
  // would recreate DockArea and all native terminals on each such change.
  const dockReady = signal(false);
  const notice = signal([props.startupNotice, snapshot().notice].filter(Boolean).join("\n"));
  const shells = signal<ShellSessionDto[]>([]);
  const appearance = signal<AppearanceSnapshot | null>(null);
  const pendingFocus = signal<string | null>(null);
  const busy = signal(false);
  const overlay = signal(false);
  let alive = true;
  const subscriptions: Subscription[] = [];
  cleanup(() => { alive = false; subscriptions.forEach((subscription) => subscription.close()); });
  // The host (and App's startup barrier in headless tests) initializes services
  // before mounting. Subscribe while this rendering owner is active.
  if (props.projectId) {
    subscriptions.push(Sessions.subscribe((event) => shells.set(event.data.sessions)));
    subscriptions.push(Appearance.subscribe((event) => appearance.set(event.data)));
  }
  function terminalColors(): TerminalColors {
    const value = appearance();
    return (value ? colorScheme() === "dark" ? value.terminalDark : value.terminalLight : { source: "ghostty" }) as TerminalColors;
  }
  function isVisible(id: string): boolean { return snapshot().panes.some((pane) => pane.activeTab === id); }
  function title(id: string): string {
    const shell = id.startsWith("shell:") && shells().find((shell) => shell.id === id.slice(6));
    return shell ? shell.cwd.split("/").filter(Boolean).pop() || "Shell" : findPanel(snapshot(), id)?.title ?? id;
  }
  async function openShell(id: string) {
    try {
      if (props.projectId && !findPanel(snapshot(), `shell:${id}`)) {
        await Tabs.update(props.projectId, { action: "unhide", key: `shell:${id}` });
      }
      if (alive) { install(session.openShell(id)); pendingFocus.set(id); }
    } catch (error) { if (alive) notice.set(String(error)); }
  }
  async function createShell() {
    if (!props.projectId || busy()) return;
    if (!snapshot().panes.some((pane) => !pane.locked)) { notice.set("Unlock a pane before opening a shell."); return; }
    busy.set(true);
    try {
      const shell = await Sessions.create({ projectId: props.projectId });
      if (alive) { shells.set([...shells().filter((item) => item.id !== shell.id), shell]); await openShell(shell.id); }
    } catch (error) { if (alive) notice.set(String(error)); }
    finally { if (alive) busy.set(false); }
  }
  function removeShell(id: string) { action(() => session.detachShell(id)); }
  async function detachShell(id: string) {
    try {
      if (props.projectId) await Tabs.update(props.projectId, { action: "hide", key: `shell:${id}` });
      if (alive) removeShell(id);
    } catch (error) {
      if (!alive) return;
      // An unavailable shared store permits only local Detach. Preserve its
      // unreadable bytes and record the core's legacy detached identity.
      if (String(error).replace(/^Error:\s*/, "").startsWith("unavailable:")) removeShell(id);
      notice.set(String(error));
    }
  }
  async function exitShell(id: string) {
    try { await Sessions.close(id); if (alive) removeShell(id); }
    catch (error) { if (alive) notice.set(String(error)); }
  }
  function closeShell(id: string) {
    const behavior = appearance()?.tabCloseBehavior ?? "ask";
    if (behavior === "detach") { void detachShell(id); return; }
    if (behavior === "exit") { void exitShell(id); return; }
    overlay.set(true);
    void Menu.popup([
      { label: "Detach tab — keep session running", onSelect: () => { void detachShell(id); } },
      { label: "Exit session", onSelect: () => { void exitShell(id); } },
      { type: "separator" }, { label: "Cancel" },
    ]).finally(() => overlay.set(false));
  }
  function openLink(event: TerminalEvents["linkOpen"]) {
    if (event.action === "edit") { notice.set("Opening terminal links in Vim is coming in the next terminal slice."); return; }
    const call = event.kind === "folder" ? "system.revealInFinder" : "system.openWith";
    void host.call(call, { path: event.target }).catch((error: unknown) => notice.set(String(error)));
  }
  let size: Extent | undefined;
  createTitleUpdater((title) => Window.setTitle(title))(props.title);
  Window.setTitleBar("hidden", 28);

  function install(next: unknown) {
    // Only session results may be installed. The reported DockArea tree is never
    // assigned to this signal; all gestures are replayed through riwork-core.
    dock.set(next as DockAreaLayout);
    snapshot.set(session.snapshot() as WorkspaceSnapshot);
    if (!dockReady()) dockReady.set(true);
    props.onChange?.(snapshot(), dock()!);
  }

  function action(run: () => unknown) {
    try { install(run()); } catch (error) { notice.set(String(error)); }
  }

  function edit(next: DockAreaLayout) {
    if (!size) return;
    try { install(session.applyDock(next, size)); }
    catch (error) {
      notice.set(String(error));
      install(session.dock(size));
    }
  }

  function measure(next: Extent) {
    if (next.width <= 0 || next.height <= 0) return;
    if (size?.width === next.width && size?.height === next.height) return;
    const first = !size;
    size = next;
    action(() => first ? session.dock(next) : session.setExtent(next));
  }

  function paneMenu(id: number) {
    const pane = snapshot().panes.find((pane) => pane.id === id);
    if (!pane) return;
    overlay.set(true);
    void Menu.popup([
      { label: pane.locked ? "Unlock pane" : "Lock pane", onSelect: () => action(() => session.toggleLock(id)) },
      { label: "Main pane", checked: pane.main, onSelect: () => action(() => session.toggleMain(id)) },
    ]).finally(() => overlay.set(false));
  }

  function layoutMenu() {
    overlay.set(true);
    void Menu.popup([
      { label: "Gather tabs into main pane", disabled: !snapshot().panes.some((pane) => pane.main), onSelect: () => action(() => session.gather()) },
      { type: "separator" },
      { label: `Main pane: ${mainPaneSummary(snapshot())}`, disabled: true },
    ]).finally(() => overlay.set(false));
  }

  cleanup(host.on("route", (data) => {
    const step = (data as { step?: { kind?: string; type?: string; shellId?: string; panel?: string } }).step;
    if ((step?.kind ?? step?.type) === "showShell" && step?.shellId) void openShell(step.shellId);
    else if ((step?.kind ?? step?.type) === "panel" && step?.panel) action(() => session.selectPanel(`panel:${step.panel}`));
  }));
  cleanup(host.on("command", (data) => {
    const command = data as { id?: string };
    if (command.id === "closeTab") {
      const tab = activePane(snapshot())?.activeTab;
      if (tab?.startsWith("shell:")) closeShell(tab.slice(6));
    } else if (command.id === "newShell") { void createShell(); }
  }));

  for (let n = 1; n <= 9; n++) {
    cleanup(keymap.bind(`mod+${n}`, () => {
      const pane = activePane(snapshot());
      // P1a shared-membership service is pending: pane order is authoritative.
      const tab = pane && numberedTab(pane, n, false);
      if (tab) action(() => session.selectPanel(tab.id));
    }));
  }
  for (const [key, direction] of [["ctrl+tab", 1], ["ctrl+shift+tab", -1]] as const) {
    cleanup(keymap.bind(key, () => {
      const pane = activePane(snapshot());
      const tab = pane && cycledTab(pane, pane.activeTab, direction);
      if (tab) action(() => session.selectPanel(tab.id));
    }));
  }

  return (
    <View testID="workspace" style={{ flex: 1, backgroundColor: "#17191e" }}>
      <View testID="title-bar" desktop={{ windowDrag: true }} style={{ height: 28, flexDirection: "row", alignItems: "center", paddingLeft: 92, paddingRight: 12, backgroundColor: "#21242b" }}>
        <Text style={{ fontSize: 11, color: "#aeb5c2" }}>{props.title}</Text>
      </View>
      <View testID="dock-container" style={{ flex: 1, minHeight: 0 }} onLayout={(bounds) => measure(bounds.size)}>
        {dockReady() && (
          <DockArea
            testID="workspace-dock"
            style={{ flex: 1 }}
            layout={dock()!}
            compact={false}
            panels={panels(dock()!.center).map((id) => ({ id, closable: false, zoomable: false, visible: true }))}
            onLayoutChange={edit}
            tabBarStyle={{ height: 28, flexDirection: "row", backgroundColor: "#21242b" }}
            tabStyle={{ padding: 0, minWidth: 0 }}
            contentStyle={{ flex: 1, backgroundColor: "#17191e" }}
            handleStyle={{ backgroundColor: "#343943" }}
            handleAccessibilityLabel="Resize panes"
            renderTab={(tab) => tab.panel.startsWith("empty:") ? null : (
              <View testID={`tab:${tab.panel}`} onPress={() => action(() => session.selectPanel(tab.panel))} onContextMenu={() => paneMenu(tab.group)} style={{ height: 28, paddingHorizontal: 10, flexDirection: "row", alignItems: "center", gap: 6, backgroundColor: tab.active ? "#303643" : "#21242b" }}>
                <Text style={{ fontSize: 11, color: tab.active ? "#eef2f9" : "#aeb5c2" }}>{title(tab.panel)}</Text>
                {tab.panel.startsWith("shell:") && <View testID={`close:${tab.panel}`} accessibilityLabel="Close shell tab" onPress={() => closeShell(tab.panel.slice(6))}><Text style={{ fontSize: 12, color: "#aeb5c2" }}>×</Text></View>}
                {snapshot().panes.find((pane) => pane.id === tab.group)?.locked && <Text style={{ fontSize: 9, color: "#7acddd" }}>◆</Text>}
              </View>
            )}
            renderPanel={(panel) => panel.id.startsWith("shell:") ? (
              <LocalTerminal shellId={panel.id.slice(6)} visible={isVisible(panel.id)}
                pinned={props.projectId ? !shells().find((shell) => shell.id === panel.id.slice(6))?.alive : false}
                focusOnAttach={pendingFocus() === panel.id.slice(6)} colors={terminalColors()} frozen={overlay()}
                onMount={() => props.onTerminalMount?.(panel.id.slice(6))}
                onAttached={() => { if (pendingFocus() === panel.id.slice(6)) pendingFocus.set(null); }}
                onFocus={() => action(() => session.selectPanel(panel.id))} onLink={openLink} onNotice={(message) => notice.set(message)} />
            ) : panel.id === "panel:shells" && props.projectId ? (
              <View testID="session-list" style={{ flex: 1, padding: 12, gap: 8 }}>
                <View testID="new-shell" accessibilityLabel="New local shell" onPress={() => { void createShell(); }}><Text style={{ color: "#7acddd", fontSize: 12 }}>{busy() ? "Starting…" : "+ New shell"}</Text></View>
                <Scroll style={{ flex: 1 }} contentStyle={{ gap: 6 }}>
                  {shells().filter((shell) => shell.projectId === props.projectId && !shell.parentId).map((shell) =>
                    <View testID={`open-shell:${shell.id}`} onPress={() => { void openShell(shell.id); }} style={{ padding: 6, gap: 3, backgroundColor: "#21242b" }}>
                      <Text style={{ fontSize: 11, color: "#eef2f9" }}>{shell.cwd.split("/").filter(Boolean).pop() || "Shell"}</Text>
                      <Text style={{ fontSize: 9, color: "#8c95a5" }}>{`${shell.id.slice(0, 8)} · ${shell.alive ? "Running" : "Ended"}`}</Text>
                    </View>)}
                </Scroll>
              </View>
            ) : (
              <View testID={`panel:${panel.id}`} onPress={() => action(() => panel.id.startsWith("empty:") ? session.selectPane(Number(panel.id.slice(6))) : session.selectPanel(panel.id))} style={{ flex: 1, minHeight: 0, padding: 16, gap: 8 }}>
                <Text style={{ fontSize: 13, color: "#eef2f9" }}>{panel.id.startsWith("empty:") ? "Empty pane" : title(panel.id)}</Text>
                <Text style={{ fontSize: 11, color: "#8c95a5" }}>{panel.id.startsWith("empty:") ? "Drag a tab here" : "This view is unavailable."}</Text>
              </View>
            )}
          />
        )}
      </View>
      {notice() !== "" && <View testID="workspace-notice" style={{ padding: 8, backgroundColor: "#343028", flexDirection: "row", gap: 10 }}>
        <Text style={{ flex: 1, fontSize: 11, color: "#f1ce85" }}>{notice()}</Text>
        <View testID="dismiss-notice" accessibilityLabel="Dismiss" onPress={() => notice.set("")}><Text style={{ color: "#f1ce85" }}>×</Text></View>
      </View>}
      <View testID="status-bar" style={{ height: 22, flexDirection: "row", alignItems: "center", paddingHorizontal: 8, gap: 10, backgroundColor: "#21242b" }}>
        <Scroll horizontal={true} style={{ maxWidth: "40%" }}><Text numberOfLines={1} style={{ fontSize: 9, color: "#aeb5c2" }}>{props.projectLabel}</Text></Scroll>
        {props.projectId && <View testID="new-shell-status" accessibilityLabel="New local shell" onPress={() => { void createShell(); }}><Text style={{ fontSize: 10, color: "#7acddd" }}>{busy() ? "Starting…" : "+ Shell"}</Text></View>}
        <View style={{ flex: 1 }} />
        <Scroll horizontal={true} contentStyle={{ flexDirection: "row", alignItems: "center", gap: 10 }}>
        <Text testID="pane-count" style={{ fontSize: 9, color: "#aeb5c2" }}>{`${snapshot().panes.length} panes`}</Text>
        <View testID="status-layout" accessibilityLabel="Layout" onPress={layoutMenu}>
          <Text style={{ fontSize: 9, color: "#aeb5c2" }}>{`Main pane: ${mainPaneSummary(snapshot())}`}</Text>
        </View>
        </Scroll>
      </View>
    </View>
  );
}
