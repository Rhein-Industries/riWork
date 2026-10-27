import { Scroll, Text, View, cleanup, keymap, signal } from "nsh";
import { DockArea, type DockAreaLayout, type DockNode } from "nsh/base";
import { Menu, Window } from "nsh/desktop";
import type { LayoutSession } from "riwork/native";
import {
  activePane, createTitleUpdater, cycledTab, findPanel, mainPaneSummary, numberedTab,
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
  const notice = signal([props.startupNotice, snapshot().notice].filter(Boolean).join("\n"));
  let size: Extent | undefined;
  createTitleUpdater((title) => Window.setTitle(title))(props.title);
  Window.setTitleBar("hidden", 28);

  function install(next: unknown) {
    // Only session results may be installed. The reported DockArea tree is never
    // assigned to this signal; all gestures are replayed through riwork-core.
    dock.set(next as DockAreaLayout);
    snapshot.set(session.snapshot() as WorkspaceSnapshot);
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
        {dock() && (
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
                <Text style={{ fontSize: 11, color: tab.active ? "#eef2f9" : "#aeb5c2" }}>{findPanel(snapshot(), tab.panel)?.title ?? tab.panel}</Text>
                {snapshot().panes.find((pane) => pane.id === tab.group)?.locked && <Text style={{ fontSize: 9, color: "#7acddd" }}>◆</Text>}
              </View>
            )}
            renderPanel={(panel) => (
              <View testID={`panel:${panel.id}`} onPress={() => action(() => panel.id.startsWith("empty:") ? session.selectPane(Number(panel.id.slice(6))) : session.selectPanel(panel.id))} style={{ flex: 1, minHeight: 0, padding: 16, gap: 8 }}>
                <Text style={{ fontSize: 13, color: "#eef2f9" }}>{panel.id.startsWith("empty:") ? "Empty pane" : findPanel(snapshot(), panel.id)?.title ?? panel.id}</Text>
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
