/** Plays the iOS Swift host (ios/RiWorkRemote/NSHHostAPI.swift) for the iPad preview (iPad.fixture.tsx):
 * a paired, connected Mac with folders, a busy project and its tabs, and a second Mac that is offline. The
 * calls answer from sample data; the theme follows the window's appearance, as Swift's `platform.theme`
 * does. `host.params` `windowControls` (points) plays an iPadOS 26 window's own controls. */
import { colorScheme, effect, host, untracked } from "nsh";
import { NATIVE_PALETTE } from "../../theme";
import { NOW, WIRE } from "./sample";

const DARK = {
  bg: "#000000", panel: "#1c1c1e", panelActive: "#2c2c2e", divider: "#3a3a3c", cyan: "#ffffff", magenta: "#c7c7cc",
  gold: "#ff9f0a", text: "#f5f5f7", muted: "#98989d", focus: "#ffffff", working: "#409cff", danger: "#ff6961",
};
export const HOST = "mac-1";
const given = ((host as unknown as { params?: { windowControls?: number } }).params ?? {});
const FEATURES = ["dashboard", "tabs", "chat", "project_memo", "native", "native:Store.snapshot", "shell_create_as_settings"];
const hosts = { hosts: [
  { id: HOST, name: "Dominik's MacBook Pro", address: "dominiks-macbook-pro", selected: true, link: { state: "connected", label: "Connected" }, features: FEATURES },
  { id: "mac-2", name: "Studio Mac", address: "studio-mac", selected: false, link: { state: "offline", label: "Not connected" }, features: [] },
] };
const snapshot = { schema: 2, revision: 1, nativePresented: false, libraryUnavailable: false, loading: false,
  details: { [HOST]: "Updated just now", "mac-2": "Updated 2 h ago" }, keyboard: { hardware: true, clicks: false }, windowControls: given.windowControls ?? 0 };

const folders = [{ id: "f-work", name: "Work", parentId: null, createdAt: NOW - 90 * 86400 }, { id: "f-tools", name: "Tools", parentId: null, createdAt: NOW - 80 * 86400 }];
const listed: [string, string, string, string | null, number, number, number, number][] = [
  ["riwork", "riWork", "~/orca/projects/riWork", "f-work", 2, 1, 1, 20],
  ["nsh", "nsh", "~/orca/projects/nsh", "f-work", 1, 0, 0, 95],
  ["ios", "Somtus companion for iPhone and iPad", "~/orca/projects/riWork/ios", "f-work", 0, 1, 0, 840],
  ["site", "orca-site", "~/orca/projects/orca-site", null, 0, 0, 2, 7200],
  ["cool365", "cool365-assist", "~/Documents/riwork/cool365-assist", null, 0, 0, 0, 9000],
  ["transcribe", "riTranscribe", "~/orca/projects/riTranscribe", null, 0, 0, 0, 20000],
  ["paper", "riPaper", "~/orca/projects/riPaper", null, 1, 0, 0, 30000],
  ["mail", "Jmail", "~/code/jmail", null, 0, 0, 0, 60000],
  ["museacc", "openmuseacc", "~/code/openmuseacc", null, 0, 0, 3, 90000],
  ["cad", "CAD Explorer repair", "~/code/cad-explorer", null, 0, 0, 0, 120000],
  ["pixel", "pixel-editor", "~/code/pixel-editor", "f-tools", 0, 0, 0, 259200],
  ["notes", "notes", "~/notes", "f-tools", 0, 0, 0, 900000],
];
const projects = { projects: listed.map(([id, name, root, , working, waiting, done, ago]) => ({ id, name, root, created_at: NOW - 100 * 86400,
  last_activity_unix: NOW - ago, agents: { working, waiting, done } })) };
const store = { value: { projects: listed.map(([id, name, root, folderId]) => ({ id, name, root, folderId, createdAt: NOW - 100 * 86400 })),
  projectFolders: folders, projectRecency: {} } };
const entry = (key: string, kind: string, title: string, status: string, order: number, extra: Record<string, unknown> = {}) =>
  ({ key, kind, title, status, hidden: false, worker: false, order, parent: null, children: [], child_count: 0, ...extra });
const tabs = { epoch: "e1", revision: 3, entries: [
  entry("shell:orch", "shell", "Project orchestrator", "working", 0),
  entry("chat:c1", "chat", "Claude · main", "waiting", 1),
  entry("shell:codex", "shell", "Codex · r59-systemone", "done", 2),
  entry("shell:sh", "shell", "Shell · r59-systemone", "idle", 3),
  entry("shell:hidden", "shell", "Worker · tests", "working", 4, { hidden: true, worker: true }),
] };
const shell = (id: string, harness: string | null, activity: string) => ({ id, project_id: "riwork", kind: "project", cwd: "/Users/dominik/orca/projects/riWork",
  harness, alive: true, created_at_unix: NOW - 3600, activity });
const shells = { shells: [shell("orch", "claude", "working"), shell("codex", "codex", "done"), shell("sh", null, "unknown"), shell("hidden", "codex", "working")] };
const chats = { chats: [{ id: "c1", provider: "claude", title: "Claude · main", project_id: "riwork", state: { state: "waiting" }, created_at_unix: NOW - 3000 }] };
const worktrees = { worktrees: [{ id: "w0", project_id: "riwork", branch: "main", path: "/Users/dominik/orca/projects/riWork", is_primary: true },
  { id: "w1", project_id: "riwork", branch: "r59-systemone", path: "/Users/dominik/orca/projects/riWork/.wt/r59", is_primary: false }] };

function relay(data: unknown): unknown {
  const call = (data ?? {}) as { method?: string };
  switch (call.method) {
    case "dashboard.get": return WIRE;
    case "projects.list": return projects;
    case "native.invoke": return store;
    case "tabs.list": return tabs;
    case "shells.list": return shells;
    case "chats.list": return chats;
    case "worktrees.list": return worktrees;
    default: throw new Error(`unsupported: ${call.method}`);
  }
}
const handlers: Record<string, unknown> = {
  "mobile.snapshot": snapshot, "mobile.action": snapshot, "relay.hosts": hosts, "relay.select": {}, "relay.disconnect": {},
  "relay.call": relay, "platform.prefs.get": { value: null }, "platform.prefs.set": {}, "nav.present": {},
};
export const presented: unknown[] = [];
const theme = (scheme: "light" | "dark") => ({ theme: { palette: scheme === "dark" ? DARK : NATIVE_PALETTE, scale: 1, family: "system-ui", monoFamily: "Menlo" },
  native: true, mic: false, dark: scheme === "dark" });
const target = host as unknown as Record<string, unknown>;
const define = (name: string, value: unknown) => Object.defineProperty(target, name, { value, configurable: true, writable: true });
Object.defineProperty(target, "params", { get: () => ({ ...given, theme: theme(untracked(colorScheme)) }), configurable: true });
define("call", async (name: string, data?: unknown) => {
  if (name === "nav.present") presented.push(data);
  const answer = handlers[name];
  if (answer === undefined) throw new Error(`unsupported: ${name}`);
  return typeof answer === "function" ? (answer as (value: unknown) => unknown)(data) : answer;
});
define("on", (name: string, listener: (value: unknown) => void) => {
  // Made while the app renders, so it ends with the app.
  if (name === "platform.theme") effect(() => { const next = theme(colorScheme()); untracked(() => listener(next)); });
  return () => {};
});
define("post", () => {});
