import { View, cleanup, host, signal } from "nsh";
import { Window } from "nsh/desktop";
import type { DockNode } from "nsh/base";
import { WORKSPACE_METRICS } from "./metrics";

export const CONTROL_WIDTH = 78;
export const CONTROL_HEIGHT = 28;
export const CONTROL_INSET = CONTROL_WIDTH + 14;
export const DRAG_HANDLE_WIDTH = 12;
export const DRAG_ROOM = 18;

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
  const free = extent - Math.max(0, node.children.length - 1) * WORKSPACE_METRICS.divider;
  const fixed = (node.sizes ?? []).reduce<number>((total, value) => total + (value ?? 0), 0);
  const flexible = node.children.filter((_, index) => node.sizes?.[index] == null).length;
  let offset = 0;
  return node.children.flatMap((child, index) => {
    const amount = node.sizes?.[index] ?? Math.max(0, free - fixed) / Math.max(1, flexible);
    const result = node.axis === "horizontal"
      ? topPaneFrames(child, { width: amount, height: size.height }, x + offset)
      : index === 0 ? topPaneFrames(child, { width: size.width, height: amount }, x) : [];
    offset += amount + WORKSPACE_METRICS.divider;
    return result;
  });
}

/** Window state for the controls island, which leaves the window controls on
 * the window's own surface; title and Dock metadata follow the workspace's project. */
export function useWindowChrome() {
  const state = signal<WindowState>({ mode: "windowed" });
  // Supplying a height makes NSH install an empty native toolbar. Its automatic
  // traffic-light layout then competes with our host's compact control inset on
  // every live-resize frame. The host owns that geometry; we reserve the same
  // 28-point strip here without asking AppKit for a second titlebar layout.
  Window.setTitleBar("hidden");
  cleanup(host.on("window.state", (next) => state.set(next as WindowState)));
  return { state, fullscreen: () => state().mode === "fullscreen" };
}

export function WindowChrome(props: { state: {state: () => WindowState; fullscreen: () => boolean}; panes: TopPane[] }) {
  return <>
    {!props.state.fullscreen() && <View testID="window-controls-island" desktop={{ windowDrag: true }} style={{
      position: "absolute", left: 0, top: 0, width: CONTROL_WIDTH, height: CONTROL_HEIGHT,
    }} />}
    {props.panes.filter((pane) => pane.width >= 40).map((pane) => <View testID={`window-drag:${pane.id}`} desktop={{ windowDrag: true }} style={{
      position: "absolute", top: 0, left: dragRegion(pane).left,
      width: dragRegion(pane).width, height: CONTROL_HEIGHT,
    }}>
      <View testID={`window-drag-handle:${pane.id}`} desktop={{ windowDrag: true }} style={{ position: "absolute", right: 0, top: 0, width: DRAG_HANDLE_WIDTH, height: CONTROL_HEIGHT }} />
    </View>)}
  </>;
}
