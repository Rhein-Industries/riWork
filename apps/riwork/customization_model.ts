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
export interface CustomizationSnapshot {
  layout: InterfaceLayout; selected: string; layouts: { id: string; name: string }[];
  keys: Record<string, string | null>; error: string | null; directory: string;
}
export const DEFAULT_CUSTOMIZATION: CustomizationSnapshot = {
  layout: { version: 1, name: "Default", root: { type: "workspace", id: "workspace" }, statusBar: true, tabBar: "default", panels: {} },
  selected: "default", layouts: [{ id: "default", name: "Default" }], keys: {}, error: null, directory: "",
};
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
