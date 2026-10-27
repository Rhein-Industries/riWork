import { View, cleanup, effect, host, signal } from "nsh";
import { Window } from "nsh/desktop";
import { Appearance, Store } from "riwork/native";
import type { DockNode } from "nsh/base";
import { createTitleUpdater, windowTitle, type HostParams } from "./model";

export const CONTROL_WIDTH = 78;
export const CONTROL_HEIGHT = 28;
export const CONTROL_INSET = CONTROL_WIDTH + 14;
export const DRAG_HANDLE_WIDTH = 12;
export const DRAG_ROOM = 18;

type Palette = { uiTextSize?: number; theme: string; controlsIsland: boolean; panelActive: string; divider: string; background: string; panel: string; text: string };
type WindowState = { mode?: string; key?: boolean; visible?: boolean; occluded?: boolean; appActive?: boolean };
export type TopPane = { id?: number; x: number; width: number; tabsEnd?: number };

export function dragRegion(pane: TopPane): { left: number; width: number } {
  const end = Math.max(0, Math.min(pane.tabsEnd ?? pane.width - DRAG_ROOM - DRAG_HANDLE_WIDTH, pane.width - DRAG_ROOM - DRAG_HANDLE_WIDTH));
  return { left: pane.x + end, width: pane.width - end };
}

/** Derive only top-edge panes from the Rust-controlled projection. */
export function topPaneFrames(node: DockNode, size: { width: number; height: number }, x = 0): TopPane[] {
  if (node.type === "tabs") return [{ id: node.id, x, width: size.width }];
  const extent = node.axis === "horizontal" ? size.width : size.height;
  const free = extent - Math.max(0, node.children.length - 1) * 5;
  const fixed = (node.sizes ?? []).reduce<number>((total, value) => total + (value ?? 0), 0);
  const flexible = node.children.filter((_, index) => node.sizes?.[index] == null).length;
  let offset = 0;
  return node.children.flatMap((child, index) => {
    const amount = node.sizes?.[index] ?? Math.max(0, free - fixed) / Math.max(1, flexible);
    const result = node.axis === "horizontal"
      ? topPaneFrames(child, { width: amount, height: size.height }, x + offset)
      : index === 0 ? topPaneFrames(child, { width: size.width, height: amount }, x) : [];
    offset += amount + 5;
    return result;
  });
}

export function useWindowChrome(fallbackTitle: () => string) {
  const params = host.params as unknown as HostParams;
  const state = signal<WindowState>({ mode: "windowed" });
  const palette = signal<Palette>({ theme: "ri_work", controlsIsland: true, panelActive: "#14212a", divider: "#253c45", background: "#090d14", panel: "#101720", text: "#d3e1e6" });
  const title = signal<string | null>(null);
  const projectName = signal<string | null>(null);
  // The initial path basename is only a fallback after Store has answered.
  // Publishing it before the snapshot arrives flickers the Dock label and
  // turns one initial announcement into two when the stored name differs.
  const projectResolved = signal(params.kind !== "project" || !params.projectId);
  const notice = signal<string | null>(null);
  const updateTitle = createTitleUpdater((value) => Window.setTitle(value));
  let alive = true;
  cleanup(() => { alive = false; });
  Window.setTitleBar("hidden", CONTROL_HEIGHT);
  effect(() => updateTitle(title() ?? fallbackTitle()));
  function refreshAppearance() {
    void Appearance.get().then((value) => { if (alive) palette.set(value as Palette); }).catch(() => {});
  }
  refreshAppearance();
  cleanup(host.on("appearance", refreshAppearance));
  cleanup(host.on("window.state", (next) => state.set(next as WindowState)));
  if (params.kind === "project" && params.projectId) {
    let subscription: ReturnType<typeof Store.subscribe> | undefined;
    const install = (projects: { id: string; name: string }[]) => {
      const project = projects.find((candidate) => candidate.id === params.projectId);
      projectName.set(project?.name ?? null);
      if (project) title.set(windowTitle(project.name));
      projectResolved.set(true);
    };
    // An embedded/headless LayoutSession can exist before the process Store
    // service. Its async snapshot initializes that service off the engine;
    // subscribing synchronously before it completes would abort the workspace.
    void Store.snapshot().then((snapshot) => {
      if (!alive) return;
      install(snapshot.projects);
      subscription = Store.subscribe((event) => install(event.data.projects));
    }).catch((error: unknown) => {
      if (alive) { notice.set(`Project title: ${String(error)}`); projectResolved.set(true); }
    });
    cleanup(() => subscription?.close());
  }
  return { palette, state, projectName, projectResolved, notice, fullscreen: () => state().mode === "fullscreen" };
}

export function WindowChrome(props: { state: {palette: () => Palette; state: () => WindowState; fullscreen: () => boolean}; panes: TopPane[] }) {
  const palette = props.state.palette;
  return <>
    {!props.state.fullscreen() && <View testID="window-controls-island" desktop={{ windowDrag: true }} style={{
      position: "absolute", left: 0, top: 0, width: CONTROL_WIDTH, height: CONTROL_HEIGHT,
      backgroundColor: palette().controlsIsland ? palette().panelActive : "transparent",
      borderBottomRightRadius: palette().controlsIsland ? 8 : 0,
      borderRightWidth: palette().controlsIsland ? 1 : 0, borderBottomWidth: palette().controlsIsland ? 1 : 0,
      borderColor: palette().divider,
    }} />}
    {props.panes.filter((pane) => pane.width >= 40).map((pane) => <View testID={`window-drag:${pane.id}`} desktop={{ windowDrag: true }} style={{
      position: "absolute", top: 0, left: dragRegion(pane).left,
      width: dragRegion(pane).width, height: CONTROL_HEIGHT,
    }}>
      <View testID={`window-drag-handle:${pane.id}`} desktop={{ windowDrag: true }} style={{ position: "absolute", right: 0, top: 0, width: DRAG_HANDLE_WIDTH, height: CONTROL_HEIGHT }} />
    </View>)}
  </>;
}
