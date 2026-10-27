import { Overlay, Text, View, cleanup, effect, keymap, signal, type YNode } from "nsh";
import { DockArea, type DockAreaLayout, type DockNode } from "nsh/base";
import { Menu, Window } from "nsh/desktop";
import { Settings, type LayoutSession, type SettingsDto } from "riwork/native";
import { DeferredPanel } from "./DeferredPanel";
import { DEFERRED_KEYMAP, TAB_CYCLE_KEYS } from "./keyboard";
import {
  activePane, createTitleUpdater, cycledTab, findPanel, numberedTab,
  type WorkspaceSnapshot,
} from "./model";
import { DEFAULT_THEME, space, type InterfaceTheme } from "./appearance";
import { StatusBar } from "./StatusBar";

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
  const theme = props.theme ?? (() => DEFAULT_THEME);
  effect(() => { const next = props.appearanceNotice?.(); if (next) notice.set(next); });
  const label = (text: string) => Settings.cased(text, theme().native);
  const textStyle = (base = 11) => ({ fontFamily: theme().family, fontSize: base * theme().scale, color: theme().palette.text });
  let root: YNode | null = null;
  function dismissNotice() { notice.set(""); root?.focus(); }
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
    void Menu.popup([
      { label: pane.locked ? "Unlock pane" : "Lock pane", onSelect: () => action(() => session.toggleLock(id)) },
      { label: "Main pane", checked: pane.main, onSelect: () => action(() => session.toggleMain(id)) },
      { label: "Settings", onSelect: () => action(() => session.openPanel("settings")) },
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
  for (const [key, direction] of TAB_CYCLE_KEYS) {
    cleanup(keymap.bind(key, () => {
      const pane = activePane(snapshot());
      const tab = pane && cycledTab(pane, pane.activeTab, direction);
      if (tab) action(() => session.selectPanel(tab.id));
    }));
  }
  // The placeholders have no chat inputs yet. Bind each deferred chord once;
  // the declaration keeps its future chat/window context without launching it.
  for (const key of new Set(DEFERRED_KEYMAP.map(([key]) => key))) {
    cleanup(keymap.bind(key, () => {}));
  }

  return (
    <View testID="workspace" ref={(node) => { root = node; }} focusable={true} style={{ flex: 1, backgroundColor: theme().palette.bg }}>
      <View testID="title-bar" desktop={{ windowDrag: true }} style={{ height: 28, flexDirection: "row", alignItems: "center", paddingLeft: 92, paddingRight: 12, backgroundColor: theme().palette.panel }}>
        <Text style={textStyle()}>{props.title}</Text>
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
            tabBarStyle={{ height: space(28, theme().scale), flexDirection: "row", backgroundColor: theme().palette.panel }}
            tabStyle={{ padding: 0, minWidth: 0 }}
            contentStyle={{ flex: 1, backgroundColor: theme().palette.bg }}
            handleStyle={{ backgroundColor: theme().palette.divider }}
            handleAccessibilityLabel="Pane divider"
            renderTab={(tab) => tab.panel.startsWith("empty:") ? null : (
              <View testID={`tab:${tab.panel}`} onPress={() => action(() => session.selectPanel(tab.panel))} onContextMenu={() => paneMenu(tab.group)} style={{ height: space(28, theme().scale), paddingHorizontal: space(10, theme().scale), flexDirection: "row", alignItems: "center", gap: 6, backgroundColor: tab.active ? theme().palette.panelActive : theme().palette.panel }}>
                <Text style={textStyle()}>{tab.panel.startsWith("panel:") ? label(findPanel(snapshot(), tab.panel)?.title ?? tab.panel) : findPanel(snapshot(), tab.panel)?.title ?? tab.panel}</Text>
                {snapshot().panes.find((pane) => pane.id === tab.group)?.locked && <Text style={{ ...textStyle(9), color: theme().palette.cyan }}>◆</Text>}
              </View>
            )}
            renderPanel={(panel) => <DeferredPanel id={panel.id}
              title={findPanel(snapshot(), panel.id)?.title ?? panel.id}
              theme={theme}
              settings={props.settings} onError={(error) => notice.set(String(error))}
              diagnostics={props.diagnostics}
              visible={snapshot().panes.some((pane) => pane.activeTab === panel.id)}
              onPress={() => action(() => panel.id.startsWith("empty:") ? session.selectPane(Number(panel.id.slice(6))) : session.selectPanel(panel.id))} />}
          />
        )}
      </View>
      <StatusBar session={session} snapshot={snapshot} projectId={props.projectId} projectLabel={props.projectLabel}
        settings={props.settings} theme={theme} action={action} onError={(error) => notice.set(String(error))} modalOpen={props.modalOpen} />
      {notice() !== "" && <Overlay testID="workspace-notice" onCancel={dismissNotice} margin={8}
        style={{ top: 31, right: 8, width: 520, maxWidth: "100%", padding: 8, backgroundColor: theme().palette.panel, borderColor: theme().palette.gold, borderWidth: 1, flexDirection: "row", gap: 10 }}>
        <Text style={{ ...textStyle(), flex: 1, color: theme().palette.gold }}>{notice()}</Text>
        <View testID="dismiss-notice" accessibilityLabel="Dismiss" role="button" onPress={dismissNotice}><Text style={{ ...textStyle(), color: theme().palette.gold }}>×</Text></View>
      </Overlay>}
    </View>
  );
}
