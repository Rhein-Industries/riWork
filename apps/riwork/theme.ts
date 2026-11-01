/** The look every RiWork screen shares, on the Mac, the iPad and the iPhone: the
 * palette's shape, the interface theme and the spacing rule. Shared code: it
 * imports nothing native. Each host resolves the theme its own way (the Mac
 * in `appearance.ts`, iOS in Swift, pushed as `platform.theme`). */

/** Native light, the context before the native owner's first delivery. Never a resolver. */
export const NATIVE_PALETTE = {
  bg: "#ffffff", panel: "#f5f5f7", panelActive: "#e8e8ed", divider: "#d2d2d7",
  cyan: "#000000", magenta: "#3a3a3c", gold: "#b34000", text: "#1d1d1f",
  muted: "#636366", focus: "#000000", working: "#0058d0", danger: "#c80018",
};

export interface InterfaceTheme {
  palette: typeof NATIVE_PALETTE;
  scale: number;
  family: string;
  monoFamily: string;
}

export const DEFAULT_THEME: InterfaceTheme = {
  palette: NATIVE_PALETTE, scale: 1, family: "system-ui", monoFamily: "Menlo",
};

export function space(base: number, scale: number): number {
  return Math.round(base * Math.max(scale, 1));
}
