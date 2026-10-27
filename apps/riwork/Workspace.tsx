import { Scroll, Text, View, cleanup, effect, host, signal } from "nsh";
import { DockArea, type DockAreaLayout, type DockNode } from "nsh/base";
import { Menu } from "nsh/desktop";
import { WindowChrome, useWindowChrome, topPaneFrames, CONTROL_INSET } from "./WindowChrome";
import type { LayoutSession } from "riwork/native";
import { DeferredPanel } from "./DeferredPanel";
import { workspaceCommands } from "./commands";
import {
  createDockUpdater, findPanel, mainPaneSummary,
  type WorkspaceSnapshot,
} from "./model";

type Extent = { width: number; height: number };
interface WorkspaceProps {
  session: LayoutSession;
  title: string;
  projectLabel: string;
  startupNotice?: string | null;
  /** In-process observer used by the headless fixture, never a host API. */
  onChange?: (snapshot: WorkspaceSnapshot, dock: DockAreaLayout) => void;
}

function panels(node: DockNode): string[] {
  return node.type === "tabs" ? node.panels : node.children.flatMap(panels);
}

export function Workspace(props: WorkspaceProps) {
  const session = props.session;
  const snapshot = signal(session.snapshot() as WorkspaceSnapshot);
  const dock = signal<DockAreaLayout | null>(null);
  // Keep the DockArea subtree mounted as its controlled layout changes. Using
  // dock() as the conditional value would reconstruct it for every projection.
  const dockReady = signal(false);
  const notice = signal([props.startupNotice, snapshot().notice].filter(Boolean).join("\n"));
  let size: Extent | undefined;
  const chrome = useWindowChrome(() => props.title);
  effect(() => { const message = chrome.notice(); if (message) notice.set(message); });
  const measured = signal<Extent>({ width: 0, height: 0 });
  const tabWidths = signal<Record<string, number>>({});
  function measureTab(group: number, panel: string, width: number) {
    const key = `${group}:${panel}`;
    if (tabWidths()[key] === width) return;
    tabWidths.set({ ...tabWidths(), [key]: width });
  }
  function chromePanes() {
    if (!dock()) return [];
    return topPaneFrames(shownDock().center, measured()).map((pane) => {
      const tabs = snapshot().panes.find((candidate) => candidate.id === pane.id)?.tabs ?? [];
      const widths = tabs.filter((tab) => !tab.id.startsWith("empty:")).map((tab) => tabWidths()[`${pane.id}:${tab.id}`]);
      // Until layout measures every tab, expose only the reserved empty area.
      // Never cover a clickable tab while its width is still unknown.
      const tabsEnd = widths.every((width) => width !== undefined)
        ? widths.reduce((total, width) => total + width, 0) + (pane.x === 0 && !chrome.fullscreen() ? CONTROL_INSET : 0)
        : undefined;
      return { ...pane, tabsEnd };
    });
  }

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
    measured.set(next);
    action(() => first ? session.dock(next) : session.setExtent(next));
  }

  function paneMenu(id: number) {
    const pane = snapshot().panes.find((pane) => pane.id === id);
    if (!pane) return;
    void Menu.popup([
      { label: pane.locked ? "Unlock pane" : "Lock pane", onSelect: () => action(() => session.toggleLock(id)) },
      { label: "Main pane", checked: pane.main, onSelect: () => action(() => session.toggleMain(id)) },
    ]);
  }

  function layoutMenu() {
    void Menu.popup([
      { label: "Gather tabs into main pane", disabled: !snapshot().panes.some((pane) => pane.main), onSelect: () => action(() => session.gather()) },
      { type: "separator" },
      { label: `Main pane: ${mainPaneSummary(snapshot())}`, disabled: true },
    ]);
  }

  const commands = workspaceCommands(session, snapshot, action);
  function shownDock(): DockAreaLayout {
    const full = dock()!;
    if (!commands.focusMode()) return full;
    function pane(node: DockNode): DockNode | undefined {
      return node.type === "tabs" ? node.id === snapshot().activePane ? node : undefined : node.children.map(pane).find(Boolean);
    }
    return { center: pane(full.center) ?? full.center };
  }
  const textSize = signal(11);
  effect(() => {
    const value = chrome.palette().uiTextSize;
    if (typeof value === "number" && Number.isFinite(value)) textSize.set(Math.max(9, Math.min(24, value)));
  });
  cleanup(host.on("settings.changed", (data) => {
    const value = (data as { uiTextSize?: unknown }).uiTextSize;
    if (typeof value === "number" && Number.isFinite(value)) textSize.set(Math.max(9, Math.min(24, value)));
  }));
  const updateDock = createDockUpdater((info) => host.post("window.setDockInfo", info));
  effect(() => {
    updateDock(chrome.projectResolved(), chrome.projectName() ?? props.projectLabel);
  });

  return (
    <View testID="workspace" style={{ flex: 1, backgroundColor: "#17191e" }}>
      <View testID="dock-container" style={{ flex: 1, minHeight: 0 }} onLayout={(bounds) => measure(bounds.size)}>
        {dockReady() && (
          <DockArea
            testID="workspace-dock"
            style={{ flex: 1 }}
            layout={shownDock()}
            compact={false}
            panels={panels(dock()!.center).map((id) => ({ id, closable: false, zoomable: false, visible: true }))}
            onLayoutChange={(next) => { if (!commands.focusMode()) edit(next); }}
            tabBarStyle={{ height: 28, flexDirection: "row", paddingRight: 30, backgroundColor: chrome.palette().panel }}
            tabStyle={{ padding: 0, minWidth: 0 }}
            contentStyle={{ flex: 1, backgroundColor: "#17191e" }}
            handleStyle={{ backgroundColor: "#343943" }}
            handleAccessibilityLabel="Pane divider"
            renderTab={(tab) => tab.panel.startsWith("empty:") ? null : (
              <View testID={`tab:${tab.panel}`} onLayout={(bounds) => measureTab(tab.group, tab.panel, bounds.size.width)} onPress={() => action(() => session.selectPanel(tab.panel))} onContextMenu={() => paneMenu(tab.group)} style={{ height: 28, paddingHorizontal: 10, marginLeft: tab.index === 0 && !chrome.fullscreen() && topPaneFrames(shownDock().center, measured()).some((pane) => pane.id === tab.group && pane.x === 0) ? CONTROL_INSET : 0, flexDirection: "row", alignItems: "center", gap: 6, backgroundColor: tab.active ? "#303643" : "#21242b" }}>
                <Text style={{ fontSize: textSize(), color: tab.active ? "#eef2f9" : "#aeb5c2" }}>{findPanel(snapshot(), tab.panel)?.title ?? tab.panel}</Text>
                {snapshot().panes.find((pane) => pane.id === tab.group)?.locked && <Text style={{ fontSize: textSize() - 2, color: "#7acddd" }}>◆</Text>}
              </View>
            )}
            renderPanel={(panel) => <DeferredPanel id={panel.id} textSize={textSize()}
              title={findPanel(snapshot(), panel.id)?.title ?? panel.id}
              visible={snapshot().panes.some((pane) => pane.activeTab === panel.id)}
              onPress={() => action(() => panel.id.startsWith("empty:") ? session.selectPane(Number(panel.id.slice(6))) : session.selectPanel(panel.id))} />}
          />
        )}
        <WindowChrome state={chrome} panes={chromePanes()} />
      </View>
      {notice() !== "" && <View testID="workspace-notice" style={{ padding: 8, backgroundColor: "#343028", flexDirection: "row", gap: 10 }}>
        <Text testID="workspace-notice-message" accessible={true} role="alert" accessibilityLabel={notice()} style={{ flex: 1, fontSize: textSize(), color: "#f1ce85" }}>{notice()}</Text>
        <View testID="dismiss-notice" accessibilityLabel="Dismiss" onPress={() => notice.set("")}><Text style={{ color: "#f1ce85" }}>×</Text></View>
      </View>}
      {commands.stripOpen() && <View testID="all-tabs-menu" role="menu" accessibilityLabel="All tabs" style={{ position: "absolute", right: 8, bottom: 24, padding: 8, backgroundColor: "#303643" }}>
        {commands.stripItems().map((tab, index) => <View testID={`all-tabs:${tab.id}`} role="menuitem" accessibilityLabel={tab.title} accessibilityState={{ selected: commands.stripIndex() === index }} onPress={() => commands.activateStrip(index)} style={{ padding: 8, backgroundColor: commands.stripIndex() === index ? "#45536a" : "transparent" }}><Text>{tab.title}</Text></View>)}
      </View>}
      {commands.closeDialog() && <View testID="close-dialog-backdrop" style={{ position: "absolute", zIndex: 100, top: 0, right: 0, bottom: 0, left: 0, padding: 16, backgroundColor: "#00000099", alignItems: "center", justifyContent: "center" }}>
        <View testID="close-dialog" style={{ padding: 16, gap: 12, backgroundColor: "#303643" }}>
          <Text>Close this tab?</Text>
          <View testID="cancel-close" onPress={() => commands.closeDialog.set(false)}><Text>Cancel</Text></View>
          <View testID="confirm-close" onPress={commands.confirmClose}><Text>Close tab</Text></View>
        </View>
      </View>}
      <View testID="status-bar" style={{ height: 22, flexDirection: "row", alignItems: "center", paddingHorizontal: 8, gap: 10, backgroundColor: "#21242b" }}>
        <Scroll horizontal={true} style={{ maxWidth: "40%" }}><Text numberOfLines={1} style={{ fontSize: textSize() - 2, color: "#aeb5c2" }}>{props.projectLabel}</Text></Scroll>
        <View style={{ flex: 1 }} />
        <View testID="all-tabs" accessibilityLabel="All tabs" onPress={commands.toggleStrip}><Text style={{ fontSize: textSize() - 2, color: "#aeb5c2" }}>All tabs</Text></View>
        <Scroll horizontal={true} contentStyle={{ flexDirection: "row", alignItems: "center", gap: 10 }}>
        <Text testID="pane-count" style={{ fontSize: textSize() - 2, color: "#aeb5c2" }}>{`${snapshot().panes.length} panes`}</Text>
        <View testID="status-layout" accessibilityLabel="Layout" onPress={layoutMenu}>
          <Text style={{ fontSize: textSize() - 2, color: "#aeb5c2" }}>{`Main pane: ${mainPaneSummary(snapshot())}`}</Text>
        </View>
        </Scroll>
      </View>
    </View>
  );
}
