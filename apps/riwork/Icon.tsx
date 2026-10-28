import { Canvas, Path2D, View, nativeComponent, platform, signal } from "nsh";

// Older macOS versions and hosts without AppKit retain a vector fallback.
const paths = {
  projects: "M5 3h14M4 6h16M3 9h18v12H3Z",
  folder: "M3 6h7l2 2h9v12H3Z",
  file: "M6 3h8l4 4v14H6ZM14 3v5h4",
  worktrees: "M6 4v12a4 4 0 0 0 4 4M6 10h8a4 4 0 0 0 4-4V4M4 2h4v4H4ZM16 2h4v4h-4ZM8 18h4v4H8Z",
  tasks: "M3 5l2 2 3-4M11 5h10M3 12l2 2 3-4M11 12h10M3 19l2 2 3-4M11 19h10",
  terminal: "M3 5h18v14H3ZM6 9l3 3-3 3M12 15h5",
  chat: "M3 4h18v13H9l-5 4v-4H3Z",
  person: "M21 12a9 9 0 1 0-18 0 9 9 0 0 0 18 0M15 9a3 3 0 1 0-6 0 3 3 0 0 0 6 0M5 18c1-5 13-5 14 0",
  "project-orchestrator": "M13 20H3V6h7l2 2h9v3M21 15a3 3 0 1 0-6 0 3 3 0 0 0 6 0M13 23c0-5 10-5 10 0",
  key: "M14 8a5 5 0 1 0-10 0 5 5 0 0 0 10 0M13 11l8 8v2h-4v-3h-3v-3l-3-3M8 7h.1",
  microphone: "M9 5a3 3 0 0 1 6 0v7a3 3 0 0 1-6 0ZM5 10v2a7 7 0 0 0 14 0v-2M12 19v3M8 22h8",
  usage: "M3 13h4v8H3ZM10 8h4v13h-4ZM17 3h4v18h-4Z",
  settings: "M10 3h4l1 3 3 1 3 3v4l-3 1-1 3-3 3h-4l-1-3-3-1-3-3v-4l3-1 1-3ZM15.5 12a3.5 3.5 0 1 0-7 0 3.5 3.5 0 0 0 7 0",
  sliders: "M3 6h6M13 6h8M3 12h12M19 12h2M3 18h2M9 18h12M9 3v6M15 9v6M5 15v6",
  calendar: "M3 5h18v16H3ZM7 2v6M17 2v6M3 10h18M7 14h3M14 14h3M7 18h3",
  globe: "M21 12a9 9 0 1 0-18 0 9 9 0 0 0 18 0M3 12h18M12 3c-6 6-6 12 0 18M12 3c6 6 6 12 0 18",
  eye: "M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12ZM15 12a3 3 0 1 0-6 0 3 3 0 0 0 6 0",
  add: "M12 4v16M4 12h16",
  close: "M6 6l12 12M18 6 6 18",
  more: "M5 12h.1M12 12h.1M19 12h.1",
  lock: "M5 10h14v11H5ZM8 10V7a4 4 0 0 1 8 0v3",
  unlock: "M5 10h14v11H5ZM8 10V7a4 4 0 0 1 8 0",
  focus: "M3 9V3h6M3 3l7 7M21 15v6h-6M21 21l-7-7",
  restore: "M9 3v6H3M9 9 3 3M15 21v-6h6M15 15l6 6",
  star: "m12 3 3 6 7 1-5 5 1 7-6-3-6 3 1-7-5-5 7-1Z",
  sort: "M7 3v18M3 7l4-4 4 4M17 3v18M13 17l4 4 4-4",
  open: "M14 3h7v7M21 3l-11 11M10 5H3v16h16v-7",
  bell: "M5 16h14l-2-3V9a5 5 0 0 0-10 0v4ZM10 20h4M12 2v2",
  "bell-off": "M5 16h14l-2-3V9a5 5 0 0 0-10 0v4ZM10 20h4M3 3l18 18",
  refresh: "M20 8a8 8 0 1 0 0 8M20 3v6h-6",
  search: "M17 10a7 7 0 1 0-14 0 7 7 0 0 0 14 0M15 15l6 6",
  down: "m5 9 7 7 7-7",
  up: "M12 20V4M5 11l7-7 7 7",
  stop: "M6 6h12v12H6Z",
  right: "m9 5 7 7-7 7",
  check: "m4 12 5 5 11-11",
  copy: "M8 8h13v13H8ZM16 8V3H3v13h5",
  layout: "M3 4h18v16H3ZM9 4v16",
} as const;

export type IconName = keyof typeof paths;

// The original desktop's icons.rs catalogue. AppKit draws these same symbols,
// at the original weights and optical sizes, without changing the layout boxes.
const symbols: Record<IconName, string> = {
  projects: "rectangle.stack", folder: "folder", file: "doc", worktrees: "arrow.triangle.branch",
  tasks: "checklist", terminal: "terminal", chat: "bubble.left.and.bubble.right",
  person: "person.crop.circle", "project-orchestrator": "folder.badge.person.crop", key: "key.fill", microphone: "mic", usage: "chart.bar",
  settings: "gearshape", sliders: "slider.horizontal.3", calendar: "calendar", globe: "globe",
  eye: "eye", add: "plus", close: "xmark", more: "ellipsis", lock: "lock", unlock: "lock.open",
  focus: "arrow.up.left.and.arrow.down.right", restore: "arrow.down.right.and.arrow.up.left",
  star: "star", sort: "arrow.up.arrow.down", open: "arrow.up.forward.app", bell: "bell",
  "bell-off": "bell.slash", refresh: "arrow.clockwise", search: "magnifyingglass",
  down: "chevron.down", up: "arrow.up", stop: "stop.fill", right: "chevron.right", check: "checkmark", copy: "doc.on.doc", layout: "sidebar.left",
};

const SystemSymbol = nativeComponent<{
  symbol: { name: string; points: number; weight: "regular" | "medium"; color: string };
}, { availability: { name: string; available: boolean } }>("riwork.symbol", {
  props: ["symbol"], events: ["availability"],
});

// Availability is a property of this AppKit host, not a particular view. A
// remounted toolbar icon can use its symbol immediately instead of briefly
// replacing it with the vector fallback while the native event makes a round trip.
const availableSymbols = new Set<string>();

export const PANEL_ICONS: Record<string, IconName> = {
  projects: "projects", files: "folder", worktrees: "worktrees", tasks: "tasks", shells: "terminal",
  usage: "usage", settings: "settings", project_settings: "sliders", schedules: "calendar", dashboard: "globe", preview: "eye",
};

export function Icon(props: { name: IconName; size?: number; color?: string }) {
  const side = () => props.size ?? 14;
  const symbol = () => symbols[props.name];
  const ready = signal<string | null>(availableSymbols.has(symbol()) ? symbol() : null);
  const native = () => platform.os === "macos" && ready() === symbol();
  const points = () => side() * (props.name === "focus" || props.name === "restore" || props.name === "layout" ? 0.72 : 0.8);
  return <View pointerEvents="none" accessible={false} style={{ width: side(), height: side(), flexShrink: 0 }}>
    {platform.os === "macos" && <SystemSymbol pointerEvents="none" accessible={false}
      symbol={{ name: symbol(), points: points(), weight: props.name === "check" || props.name === "close" ? "medium" : "regular", color: props.color ?? "#98989d" }}
      onAvailability={(event) => {
        if (event.available) availableSymbols.add(event.name);
        else availableSymbols.delete(event.name);
        if (event.name === symbol()) ready.set(event.available ? event.name : null);
      }}
      style={{ position: "absolute", left: 0, top: 0, width: side(), height: side(), opacity: native() ? 1 : 0 }} />}
    {!native() && <Canvas pointerEvents="none" style={{ width: side(), height: side(), flexShrink: 0 }} draw={(cx, size) => {
    cx.scale(size.width / 24, size.height / 24);
    cx.strokeStyle = props.color ?? "#98989d";
    cx.lineWidth = props.name === "more" ? 3 : 1.5;
    cx.lineCap = "round";
    cx.lineJoin = "round";
    cx.stroke(new Path2D(paths[props.name]));
    }} />}
  </View>;
}
