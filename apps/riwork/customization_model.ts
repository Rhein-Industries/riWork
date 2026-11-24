import type { PanelModel, WorkspaceSnapshot } from "./model";

/** Versioned user interface document. Session layouts remain owned by LayoutSession. */
export interface ModReference { addin: string; view: string; args?: unknown }
export interface CommandItem { command: string; label: string }
interface NodeBase { id: string; size?: number }
export type InterfaceNode = NodeBase & (
  | { type: "row" | "column"; children: InterfaceNode[] }
  | { type: "workspace" | "spacer" }
  | ({ type: "mod" } & ModReference)
  | { type: "commands"; items: CommandItem[] }
);
export interface InterfaceLayout {
  version: number; name: string; root: InterfaceNode;
  statusBar: boolean; tabBar: "default" | "hidden"; panels: Record<string, ModReference>;
}
export interface WorkspaceModInfo { id: string; name: string; version: string; description: string }
export interface CustomizationSnapshot {
  layout: InterfaceLayout; selected: string; layouts: { id: string; name: string }[];
  keys: Record<string, string | null>; error: string | null; directory: string;
  workspaceMod: string | null; mods: WorkspaceModInfo[];
}
export const DEFAULT_CUSTOMIZATION: CustomizationSnapshot = {
  layout: { version: 1, name: "Default", root: { type: "workspace", id: "workspace" }, statusBar: true, tabBar: "default", panels: {} },
  selected: "default", layouts: [{ id: "default", name: "Default" }], keys: {}, error: null, directory: "",
  workspaceMod: null, mods: [],
};

export function workspaceTabKey(tab: PanelModel): string {
  if (tab.kind === "panel") return tab.id.slice(6);
  if (tab.kind === "shell" || tab.kind === "remote_shell") return "cli";
  if (tab.kind === "remote_chat") return "chat";
  return tab.kind;
}

/** Read-only projection. DockArea hides tabs in place and displays the first
 * visible tab if the saved selection is hidden. Match that for focus and keys. */
export function visibleWorkspace(snapshot: WorkspaceSnapshot, enabled?: ReadonlySet<string>): WorkspaceSnapshot {
  if (!enabled) return snapshot;
  const panes = snapshot.panes.map((pane) => {
    const tabs = pane.tabs.filter((tab) => tab.kind === "empty" || enabled.has(workspaceTabKey(tab)));
    const activeTab = tabs.find((tab) => tab.id === pane.activeTab)?.id ?? tabs[0]?.id;
    return { ...pane, tabs, activeTab };
  });
  const hasTabs = (pane: typeof panes[number]) => pane.tabs.some((tab) => tab.kind !== "empty");
  const current = panes.find((pane) => pane.id === snapshot.activePane && hasTabs(pane));
  const activePane = (current ?? panes.find(hasTabs))?.id ?? snapshot.activePane;
  return { ...snapshot, activePane, focusMode: snapshot.focusMode && !!current,
    panes: panes.map((pane) => ({ ...pane, active: pane.id === activePane })) };
}
export interface Rect { x: number; y: number; width: number; height: number }
export interface InterfaceGeometry { workspace: Rect; leaves: { node: InterfaceNode; rect: Rect }[] }

/** Flatten the composition into rectangles. The workspace keeps one native parent
 * even when a user rearranges the tree, so terminals and chats are never remounted. */
export function interfaceGeometry(root: InterfaceNode, width: number, height: number): InterfaceGeometry {
  const result: InterfaceGeometry = { workspace: { x: 0, y: 0, width, height }, leaves: [] };
  function visit(node: InterfaceNode, rect: Rect) {
    if (node.type === "workspace") { result.workspace = rect; return; }
    if (node.type !== "row" && node.type !== "column") { result.leaves.push({ node, rect }); return; }
    const horizontal = node.type === "row";
    const extent = horizontal ? rect.width : rect.height;
    const fixed = node.children.reduce((total, child) => total + (child.size ?? 0), 0);
    const flexible = node.children.filter((child) => child.size === undefined).length;
    // Shrink fixed blocks on small windows instead of pushing the workspace out
    // of reach. A flexible branch retains room even when all requested sizes fit poorly.
    const budget = flexible ? Math.max(0, extent - Math.min(120, extent / 2)) : extent;
    const scale = fixed > budget ? budget / fixed : 1;
    const share = flexible ? Math.max(0, extent - fixed * scale) / flexible : 0;
    let offset = 0;
    for (const child of node.children) {
      const amount = child.size === undefined ? share : child.size * scale;
      visit(child, horizontal ? { ...rect, x: rect.x + offset, width: amount } : { ...rect, y: rect.y + offset, height: amount });
      offset += amount;
    }
  }
  visit(root, { x: 0, y: 0, width: Math.max(0, width), height: Math.max(0, height) });
  return result;
}
