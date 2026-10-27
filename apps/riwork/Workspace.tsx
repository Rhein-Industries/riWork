import { Overlay, Scroll, Text, View, cleanup, createAnimatedValue, effect, host, keymap, signal, type YNode, type Point } from "nsh";
import { Input, Popover, type DockAreaLayout, type DockAreaRef, type DockNode } from "nsh/base";
import { Menu, Window, type MenuItem } from "nsh/desktop";
import { type LayoutSession } from "riwork/native";
import { DockArea } from "./DockArea";
import { CloseDialog } from "./CloseDialog";
import { DeferredPanel } from "./DeferredPanel";
import { DEFERRED_KEYMAP, TAB_CYCLE_KEYS } from "./keyboard";
import { STATUS_COLORS, WORKSPACE_METRICS as M, headerHeight, mainMarkerThreshold, sessionCellWidth, revealOffset } from "./metrics";
import { activePane, closeChoice, createTitleUpdater, cycledTab, findPanel, mainPaneSummary, numberedTab, panelTitle, stripTabs, type PanelModel, type PanelStatus, type WorkspaceSnapshot } from "./model";

type Extent = { width: number; height: number };
interface WorkspaceProps {
  session: LayoutSession;
  title: string;
  projectLabel: string;
  startupNotice?: string | null;
  onChange?: (snapshot: WorkspaceSnapshot, dock: DockAreaLayout) => void;
}
function panels(node: DockNode): string[] {
  return node.type === "tabs" ? node.panels : node.children.flatMap(panels);
}

const VIEWS = ["projects", "files", "preview", "worktrees", "tasks", "shells", "usage", "schedules", "dashboard", "project_settings", "settings"];
const VIEW_KEYS: Record<string, string> = { projects: "⌘B", files: "⌘⇧E", preview: "⌘⇧P", shells: "⌘⌥S", schedules: "⌘⇧S", dashboard: "⌘⌥G", project_settings: "⌘⌥A", settings: "⌘," };
const VIEW_SHORTCUTS: Record<string, string> = { files: "mod+shift+e", preview: "mod+shift+p", schedules: "mod+shift+s", dashboard: "mod+alt+g", settings: "mod+," };

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
  // This boolean, rather than the projection object, owns the one DockArea mount.
  const dockReady = signal(false);
  const notice = signal([props.startupNotice, snapshot().notice].filter(Boolean).join("\n"));
  const closing = signal<PanelModel | null>(null);
  const renaming = signal<PanelModel | null>(null);
  const renameTitle = signal("");
  const allTabs = signal(false);
  const allTabsAvailable = signal(false);
  const allIndex = signal(0);
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
  let size: Extent | undefined;
  let dockRef: DockAreaRef | undefined;
  let stripRef: YNode | undefined;
  let renameInput: YNode | undefined;
  let scrollOffset = 0;
  let menuOpen = false;
  let dragging = false;
  let alive = true;
  const bodyRefs = new Map<string, YNode>();
  const tabRefs = new Map<string, YNode>();
  createTitleUpdater((title) => Window.setTitle(title))(props.title);
  Window.setTitleBar("hidden", 28);
  const shared = () => Array.isArray(snapshot().sharedTabs) && !snapshot().remoteProject;
  const selected = () => activePane(snapshot())?.activeTab;
  const modal = () => !!closing() || !!renaming();
  const frozen = () => modal() || menuOpen || dragging || allTabs();
  const ordered = () => stripTabs(snapshot());
  const sessions = () => ordered().filter((tab) => tab.kind !== "panel");
  const header = () => headerHeight(snapshot().uiTextSize);
  const totalWidth = () => ordered().reduce((sum, tab) => sum + (tab.kind === "panel" ? M.panelCell : sessionCellWidth(tab.title)), 0) + 48;
  const overflow = () => totalWidth() > stripWidth();
  effect(() => allTabsAvailable.set(overflow() || allTabs()));
  effect(() => {
    const id = hovered();
    const tab = id ? findPanel(snapshot(), id) : undefined;
    tabHint.set(null);
    if (!tab) return;
    const key = tab.kind === "panel" ? VIEW_KEYS[tab.id.slice(6)] : undefined;
    const timer = setTimeout(() => tabHint.set({ id: tab.id, text: `${tab.title}${key ? ` · ${key}` : ""}` }), 500);
    cleanup(() => clearTimeout(timer));
  });

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
    if (!alive || !next) return;
    dock.set(next as DockAreaLayout);
    snapshot.set(session.snapshot() as WorkspaceSnapshot);
    if (snapshot().notice) notice.set(snapshot().notice!);
    if (!dockReady()) dockReady.set(true);
    if (snapshot().focusMode || !shared()) allTabs.set(false);
    props.onChange?.(snapshot(), dock()!);
    reveal();
  }
  function thaw() {
    if (!frozen()) {
      install(session.setInteraction(false));
    }
    focusActive();
  }
  function freeze() { session.setInteraction(true); }
  function action(run: () => unknown) {
    try {
      const result = run();
      if (result && typeof (result as Promise<unknown>).then === "function") {
        void (result as Promise<unknown>).then(install).catch((error) => notice.set(String(error)));
      } else install(result);
    } catch (error) { notice.set(String(error)); }
  }
  function select(id: string) { hovered.set(null); allTabs.set(false); action(() => session.selectPanel(id)); focusActive(); thaw(); }
  function edit(next: DockAreaLayout) {
    if (!size) return;
    dragging = false;
    try { install(session.applyDock(next, size)); }
    catch (error) { notice.set(String(error)); install(session.dock(size)); }
    thaw();
  }
  function measure(next: Extent) {
    if (next.width <= 0 || next.height <= 0) return;
    if (size?.width === next.width && size?.height === next.height) return;
    const first = !size;
    size = next;
    action(() => first ? session.dock(next) : session.setExtent(next));
  }
  function close(id = selected()) {
    hovered.set(null);
    if (!id) return;
    const pane = snapshot().panes.find((pane) => pane.tabs.some((tab) => tab.id === id));
    if (pane?.locked) { notice.set("Unlock the pane to close its tabs"); return; }
    const tab = findPanel(snapshot(), id);
    if (!tab) return;
    allTabs.set(false);
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
    menuOpen = true;
    freeze();
    void Menu.popup(items).finally(() => { menuOpen = false; thaw(); });
  }
  function view(panel: string) {
    if (panel === "dashboard") {
      void host.call("window.open", { kind: "global", view: "dashboard", start: false }).catch((error) => notice.set(String(error)));
    } else action(() => session.openPanel(panel));
  }
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
  function paneMenu(id: number) {
    const pane = snapshot().panes.find((pane) => pane.id === id);
    if (!pane) return;
    action(() => session.selectPane(id));
    const disabled = (label: string, shortcut?: string): MenuItem => ({ label, disabled: true, shortcut });
    const rows: MenuItem[] = [disabled("Existing sessions", "mod+alt+s"), disabled("Existing Claude chats"), disabled("Existing Muse chats"), { type: "separator" }, disabled("New tab"),
      { label: "Shell", shortcut: "mod+t", onSelect: () => action(() => session.newShell({})) },
      ...[["Codex", "mod+shift+c"], ["Claude", "mod+shift+l"], ["Grok", "mod+shift+g"], ["Muse", "mod+shift+u"], ["Global orchestrator", "mod+shift+o"], ["Project orchestrator", "mod+alt+o"], ["New chat…"], ["App preview…"]].map(([label, shortcut]) => disabled(label, shortcut)),
      { type: "separator" }, disabled("Views"),
      ...VIEWS.map((panel): MenuItem => ({ label: panelTitle(panel), shortcut: VIEW_SHORTCUTS[panel], onSelect: () => view(panel) })),
      { type: "separator" }, disabled("Pane"),
      { label: "Split right", shortcut: "mod+d", onSelect: () => action(() => session.newShell({ split: "right" })) },
      { label: "Split down", shortcut: "mod+shift+d", onSelect: () => action(() => session.newShell({ split: "down" })) },
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
  function layoutMenu() {
    popup([
      { label: "Gather tabs into main pane", shortcut: "mod+shift+m", onSelect: () => action(() => session.gather()) },
      { label: "Default layout", shortcut: "mod+alt+l", onSelect: () => action(() => session.defaultLayout()) },
      { label: "Focus tab", shortcut: "mod+shift+f", checked: !!snapshot().focusMode, onSelect: () => action(() => session.toggleFocus()) },
      { label: "Center focused tab", checked: !!snapshot().focusCentered, onSelect: () => action(() => session.toggleCentered()) },
      { type: "separator" }, { label: `Main pane: ${mainPaneSummary(snapshot())}`, disabled: true },
    ]);
  }
  function command(id: string) {
    if (modal()) return;
    allTabs.set(false);
    switch (id) {
      case "newShell": action(() => session.newShell({})); break;
      case "splitRight": action(() => session.newShell({ split: "right" })); break;
      case "splitDown": action(() => session.newShell({ split: "down" })); break;
      case "closeTab": close(); break;
      case "tabContextMenu": if (selected()) tabMenu(selected()!); break;
      case "closePane": closePane(); break;
      case "gatherTabs": case "gather": action(() => session.gather()); break;
      case "defaultLayout": action(() => session.defaultLayout()); break;
      case "focusMode": case "toggleFocus": action(() => session.toggleFocus()); break;
      case "focusCentered": action(() => session.toggleCentered()); break;
      default: if (id.startsWith("panel:")) view(id.slice(6));
    }
    thaw();
  }
  cleanup(host.on("command", (data) => command((data as { id: string }).id)));
  cleanup(host.on("project.open", (data) => {
    closing.set(null); renaming.set(null); allTabs.set(false);
    action(() => session.switchProject((data as { projectId: string }).projectId));
  }));
  const subscription = session.subscribe((next) => { if (!frozen()) install(next); });
  cleanup(() => { alive = false; subscription.close(); });
  const bind = (key: string, run: () => void) => cleanup(keymap.bind(key, run));
  for (const [key, id] of [["mod+t", "newShell"], ["mod+d", "splitRight"], ["mod+shift+d", "splitDown"], ["mod+w", "closeTab"], ["mod+shift+w", "closePane"], ["mod+shift+m", "gatherTabs"], ["mod+alt+l", "defaultLayout"], ["mod+shift+f", "focusMode"]]) bind(key, () => command(id));
  for (const [key, panel] of [["mod+b", "projects"], ["mod+shift+e", "files"], ["mod+shift+p", "preview"], ["mod+shift+s", "schedules"], ["mod+,", "settings"]]) bind(key, () => { if (!modal()) view(panel); });
  for (let n = 1; n <= 9; n++) bind(`mod+${n}`, () => {
    if (modal()) return;
    const pane = activePane(snapshot());
    const tab = shared() ? (n === 9 ? sessions()[sessions().length - 1] : sessions()[n - 1]) : pane && numberedTab(pane, n, false);
    if (tab) select(tab.id);
  });
  for (const [key, direction] of TAB_CYCLE_KEYS) bind(key, () => {
    const pane = activePane(snapshot());
    const tab = pane && cycledTab(pane, pane.activeTab, direction, modal());
    if (tab) select(tab.id);
  });
  bind("shift+f10", () => { if (!modal() && selected()) tabMenu(selected()!); });
  effect(() => {
    if (!allTabs()) return;
    for (const [key, direction] of [["down", 1], ["up", -1]] as const) cleanup(keymap.bind(key, () => {
      if (sessions().length) allIndex.set((allIndex() + direction + sessions().length) % sessions().length);
    }));
    cleanup(keymap.bind("enter", () => { const tab = sessions()[allIndex()]; if (tab) select(tab.id); }));
  });
  bind("escape", () => {
    if (closing()) closing.set(null);
    if (renaming()) renaming.set(null);
    if (allTabs()) allTabs.set(false);
    thaw();
  });
  for (const key of new Set(DEFERRED_KEYMAP.map(([key]) => key))) if (key !== "escape") bind(key, () => {});

  function headerWidth(id: number) {
    const pane = paneWidth(id);
    const inset = id === snapshot().panes[0]?.id ? Math.min(M.trafficLightInset, Math.max(0, pane - M.menuButton)) : 0;
    return (pane - inset) / (header() / M.header);
  }
  function paneControls(id: number) {
    const pane = () => snapshot().panes.find((pane) => pane.id === id);
    return <View style={{ flexDirection: "row", alignItems: "center", gap: M.buttonGap }}>
      {pane()?.main && headerWidth(id) >= mainMarkerThreshold(snapshot().theme, snapshot().panelTabIcons) && <View testID={`pane-main-${id}`} accessibilityLabel="Unset main pane" onPress={() => action(() => session.toggleMain(id))} style={{ width: 22 }}><Text style={{ color: "#7acddd" }}>★</Text></View>}
      {headerWidth(id) >= M.lockAt && <View testID={`pane-lock-${id}`} accessibilityLabel={pane()?.locked ? "Unlock pane" : "Lock pane"} onPress={() => action(() => session.toggleLock(id))} style={{ width: 22, height: 22, borderRadius: 3, justifyContent: "center" }}><Text style={{ color: pane()?.locked ? "#7acddd" : "#aeb5c2" }}>{pane()?.locked ? "◆" : "◇"}</Text></View>}
      {headerWidth(id) >= M.focusAt && <View testID={`pane-focus-${id}`} accessibilityLabel="Focus tab" onPress={() => { action(() => session.selectPane(id)); command("focusMode"); }} style={{ width: 22, height: 22, justifyContent: "center" }}><Text style={{ color: "#aeb5c2" }}>↗</Text></View>}
      <View testID={`pane-menu-${id}`} accessibilityLabel="Pane menu" onPress={() => paneMenu(id)} style={{ width: M.menuButton, height: header(), justifyContent: "center", alignItems: "center" }}><Text style={{ color: "#aeb5c2" }}>…</Text></View>
    </View>;
  }
  function tabCell(tab: PanelModel, active: boolean, strip: boolean) {
    const pane = snapshot().panes.find((pane) => pane.tabs.some((panel) => panel.id === tab.id));
    const shownTitle = tab.kind === "panel" && snapshot().theme && snapshot().theme !== "native" ? tab.title.toUpperCase() : tab.title;
    return <View ref={(node) => tabRefs.set(`${strip ? "strip" : "pane"}:${tab.id}`, node)} testID={`tab-${tab.id}`} role={strip ? "tab" : undefined} accessibilityLabel={`${tab.title}${tab.kind !== "panel" && tab.status ? `, ${tab.status}` : ""}`}
      accessibilityState={{ selected: active }} focusable={true}
      onPress={() => select(tab.id)} onContextMenu={() => tabMenu(tab.id)} onHoverIn={() => hovered.set(tab.id)} onHoverOut={() => hovered.set(null)}
      onPressDown={(event) => { if (strip && event) pressOrigin = event.pagePosition; }}
      onTouchStart={(event) => { if (strip) pressOrigin = { x: event.pageX, y: event.pageY }; }}
      pan={strip ? { x: dragX, y: dragY } : undefined}
      onPanStart={strip ? () => { hovered.set(null); dragging = true; allTabs.set(false); freeze(); dragOrigin.set(pressOrigin); dragTab.set(tab); } : undefined}
      onPanEnd={strip ? (event) => { void stripDrop(tab.id, event.translationX, event.translationY, event.cancelled); } : undefined}
      style={{ height: header(), width: strip ? (tab.kind === "panel" ? M.panelCell : sessionCellWidth(tab.title)) : undefined, minWidth: 0, paddingLeft: 8, paddingRight: 4, flexDirection: "row", alignItems: "center", gap: 6, backgroundColor: active ? "#303643" : "#21242b" }}>
      {tab.kind !== "panel" && <StatusDot status={tab.status} />}
      <Text numberOfLines={1} style={{ fontSize: strip ? M.tabFont : 11, flexShrink: 1, color: active ? "#eef2f9" : "#aeb5c2" }}>{strip && tab.kind === "panel" ? shownTitle.slice(0, 1) : shownTitle}</Text>
      {!pane?.locked && (active || hovered() === tab.id) && <View testID={`tab-close-${tab.id}`} tooltip="Close tab · ⌘W" accessibilityLabel={`Close ${tab.title}`} onPress={() => close(tab.id)} style={{ width: M.tabClose, alignItems: "center" }}><Text style={{ color: "#aeb5c2" }}>×</Text></View>}
    </View>;
  }
  async function stripDrop(id: string, dx: number, dy: number, cancelled: boolean) {
    const point = { x: pressOrigin.x + dx, y: pressOrigin.y + dy };
    dragTab.set(null); dragX.set(0); dragY.set(0);
    if (!cancelled && point.x >= 0 && point.x <= width() && point.y >= 0) {
      if (point.y < 28 + header()) {
        let x = 48 - scrollOffset;
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
    dragging = false;
    thaw();
  }
  return <View testID="workspace" style={{ flex: 1, backgroundColor: "#17191e" }} onLayout={(bounds) => width.set(bounds.size.width)}>
    <View testID="title-bar" desktop={{ windowDrag: true }} style={{ height: 28, flexDirection: "row", alignItems: "center", paddingLeft: 92, paddingRight: 12, backgroundColor: "#21242b" }}>
      <Text style={{ fontSize: 11, color: "#aeb5c2" }}>{props.title}</Text>
      <View style={{ flex: 1 }} />
      {snapshot().focusMode && <View testID="focus-restore" onPress={() => command("focusMode")}><Text style={{ fontSize: 11, color: "#7acddd" }}>↙ RESTORE  ⌘⇧F</Text></View>}
    </View>
    {shared() && !snapshot().focusMode && <View testID="shared-strip" style={{ height: header(), flexDirection: "row", backgroundColor: "#21242b" }} onLayout={(bounds) => { stripWidth.set(bounds.size.width); reveal(); }}>
      <Scroll ref={(node) => { stripRef = node; }} horizontal={true} role="tablist" accessibilityLabel="Tabs" style={{ flex: 1 }} contentStyle={{ flexDirection: "row", alignItems: "center" }}>
        <Text style={{ fontSize: 9, width: 48, color: "#8c95a5" }}>Panels</Text>
        {ordered().map((tab) => tabCell(tab, selected() === tab.id, true))}
      </Scroll>
      {allTabsAvailable() && <Popover open={allTabs()} onOpenChange={(open) => { allTabs.set(open); if (open) { hovered.set(null); allIndex.set(Math.max(0, sessions().findIndex((tab) => tab.id === selected()))); freeze(); } else thaw(); }} placement="bottom" align="end"
        popupStyle={{ width: M.allTabsWidth, maxHeight: M.allTabsMaxHeight, backgroundColor: "#282d36", padding: 4 }}
        trigger={<View testID="strip-all-tabs" accessibilityLabel="All tabs" style={{ width: 28, justifyContent: "center", alignItems: "center" }}><Text style={{ color: "#aeb5c2" }}>⌄</Text></View>}>
        <Scroll style={{ maxHeight: M.allTabsMaxHeight }}>
          {sessions().map((tab, index) => <View role="menuitem" accessibilityLabel={tab.title} onPress={() => select(tab.id)} style={{ height: 28, paddingHorizontal: 8, gap: 8, flexDirection: "row", alignItems: "center", backgroundColor: allIndex() === index ? "#3a4351" : "#282d36" }}>
            <StatusDot status={tab.status} /><Text style={{ color: "#eef2f9", flex: 1, fontSize: 11 }}>{tab.title}</Text><Text style={{ color: "#7acddd" }}>{selected() === tab.id ? "✓" : ""}</Text>
          </View>)}
        </Scroll>
      </Popover>}
    </View>}
    <View testID="dock-container" onLayout={(bounds) => containerHeight.set(bounds.size.height)} style={{ flex: 1, minHeight: 0, alignItems: "center", paddingBottom: snapshot().focusMode && snapshot().focusCentered ? containerHeight() * M.focusBottomMargin : 0 }}>
      <View style={{ flex: 1, width: "100%", maxWidth: snapshot().focusMode && snapshot().focusCentered ? M.focusMaxWidth : undefined }} onLayout={(bounds) => measure(bounds.size)}>
      {dockReady() && <DockArea ref={(node) => { dockRef = node; }} testID="workspace-dock" style={{ flex: 1 }} layout={dock()!} compact={false} dividerSize={M.divider}
        panels={panels(dock()!.center).map((id) => ({ id, closable: false, zoomable: false, visible: !snapshot().focusMode || !!snapshot().panes.find((pane) => pane.id === snapshot().activePane && (pane.tabs.some((tab) => tab.id === id) || id === `empty:${pane.id}`)) }))}
        onLayoutChange={edit}
        tabBarStyle={{ height: shared() && !snapshot().focusMode ? 0 : header(), flexDirection: "row", backgroundColor: "#21242b", overflow: "hidden" }}
        tabStyle={{ padding: 0, minWidth: 0 }} contentStyle={{ flex: 1, backgroundColor: "#17191e" }} handleStyle={{ backgroundColor: "#343943" }} handleAccessibilityLabel="Pane divider"
        renderTabBarSuffix={(id) => paneControls(id)}
        renderDragPreview={(tab) => <View style={{ paddingHorizontal: 12, paddingVertical: 7, borderWidth: 1, borderColor: "#7acddd", backgroundColor: "#21242b" }}><Text style={{ fontSize: 11, color: "#eef2f9" }}>{findPanel(snapshot(), tab.panel)?.title ?? tab.panel}</Text></View>}
        renderTab={(tab) => <View testID={`tab:${tab.panel}`} onPressDown={() => { hovered.set(null); dragging = true; freeze(); }} onPressUp={() => { dragging = false; thaw(); }} onPress={() => tab.panel.startsWith("empty:") ? action(() => session.selectPane(tab.group)) : select(tab.panel)} onContextMenu={() => tabMenu(tab.panel)} style={{ flexDirection: "row", alignItems: "center", paddingLeft: tab.index === 0 && tab.group === snapshot().panes[0]?.id ? M.trafficLightInset : 0 }}>
          {!tab.panel.startsWith("empty:") && tabCell(findPanel(snapshot(), tab.panel) ?? { id: tab.panel, title: tab.panel, kind: "empty" }, tab.active, false)}
        </View>}
        renderPanel={(panel) => <View ref={(node) => { bodyRefs.set(panel.id, node); }} focusable={true} testID={`pane-${snapshot().panes.find((pane) => pane.tabs.some((tab) => tab.id === panel.id) || panel.id === `empty:${pane.id}`)?.id}`} style={{ flex: 1 }} onPress={() => { action(() => panel.id.startsWith("empty:") ? session.selectPane(Number(panel.id.slice(6))) : session.selectPanel(panel.id)); focusActive(); }}>
          {shared() && !snapshot().focusMode && <View style={{ height: header(), flexDirection: "row", justifyContent: "flex-end", backgroundColor: "#21242b" }}>{paneControls(snapshot().panes.find((pane) => pane.tabs.some((tab) => tab.id === panel.id) || panel.id === `empty:${pane.id}`)?.id ?? snapshot().activePane)}</View>}
          <DeferredPanel id={panel.id} title={findPanel(snapshot(), panel.id)?.title ?? panel.id} visible={snapshot().panes.some((pane) => pane.activeTab === panel.id && (!snapshot().focusMode || pane.id === snapshot().activePane))} onPress={() => { action(() => panel.id.startsWith("empty:") ? session.selectPane(Number(panel.id.slice(6))) : session.selectPanel(panel.id)); focusActive(); }} />
        </View>} />}
      </View>
    </View>
    {notice() !== "" && <View testID="workspace-notice" style={{ padding: 8, backgroundColor: "#343028", flexDirection: "row", gap: 10 }}>
      <Text style={{ flex: 1, fontSize: 11, color: "#f1ce85" }}>{notice()}</Text><View testID="dismiss-notice" accessibilityLabel="Dismiss" onPress={() => notice.set("")}><Text style={{ color: "#f1ce85" }}>×</Text></View>
    </View>}
    <View testID="status-bar" style={{ height: 22, flexDirection: "row", alignItems: "center", paddingHorizontal: 8, gap: 10, backgroundColor: "#21242b" }}>
      <Scroll horizontal={true} style={{ maxWidth: "40%" }}><Text numberOfLines={1} style={{ fontSize: 9, color: "#aeb5c2" }}>{props.projectLabel}</Text></Scroll><View style={{ flex: 1 }} />
      <Text testID="pane-count" style={{ fontSize: 9, color: "#aeb5c2" }}>{`${snapshot().panes.length} panes`}</Text>
      <View testID="status-layout" accessibilityLabel="Layout" onPress={layoutMenu}><Text style={{ fontSize: 9, color: "#aeb5c2" }}>{`Main pane: ${mainPaneSummary(snapshot())}`}</Text></View>
    </View>
    {closing() && <CloseDialog title={closing()!.title} onChoice={(choice) => { const tab = closing()!; closing.set(null); if (choice) action(() => session.closePanel(tab.id, choice === "exit")); thaw(); }} />}
    {tabHint() && !frozen() && <Overlay pointerEvents="none" anchor={tabRefs.get(`${shared() && !snapshot().focusMode ? "strip" : "pane"}:${tabHint()!.id}`)} placement="bottom" align="start" offset={4} style={{ paddingHorizontal: 8, paddingVertical: 5, backgroundColor: "#303643", borderRadius: 4 }}><Text style={{ fontSize: 11, color: "#eef2f9" }}>{tabHint()!.text}</Text></Overlay>}
    {dragTab() && <Overlay pointerEvents="none" style={{ position: "absolute", left: dragOrigin().x, top: dragOrigin().y, transform: [{ translateX: dragX }, { translateY: dragY }], paddingHorizontal: 12, paddingVertical: 7, borderWidth: 1, borderColor: "#7acddd", backgroundColor: "#21242b" }}><Text style={{ fontSize: 11, color: "#eef2f9" }}>{dragTab()!.title}</Text></Overlay>}
    <Popover open={!!renaming()} onOpenChange={(open) => { if (!open) { renaming.set(null); thaw(); } }} onOpen={() => renameInput?.focus()} popupStyle={{ width: 260, padding: 12, gap: 8, backgroundColor: "#282d36" }} trigger={<View style={{ position: "absolute", top: 56, left: 120, width: 1, height: 1 }} />}>
      <Input ref={(node) => { renameInput = node; }} testID="tab-rename-input" placeholder="Tab title" value={renameTitle()} onChange={(value) => renameTitle.set(value)} onSubmit={saveRename} />
      <View role="button" accessibilityLabel="Save title" onPress={saveRename}><Text style={{ color: "#eef2f9" }}>Save title</Text></View>
    </Popover>
  </View>;
}
