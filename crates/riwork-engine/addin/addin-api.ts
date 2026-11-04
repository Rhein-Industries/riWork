/**
 * The Somtus add-in API, version 1. Somtus writes this file into every add-in it builds, so it
 * always matches the Somtus that runs the add-in: import from "./addin-api", edit nothing here.
 *
 * Each call needs the permission its comment names in the add-in's `addin.json`; a call without
 * it rejects. Storage, settings and opening the add-in's own views need nothing.
 */
import { host } from "nsh";

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type TaskStatus = "todo" | "in_progress" | "done";
export interface HostSession {
  id: string; kind: "chat" | "terminal"; projectId: string | null; parentId: string | null;
  title: string; provider: string | null;
  state: "starting" | "working" | "waiting" | "blocked" | "error" | "idle" | "done" | "stopped" | "exited" | "unknown";
  source: string; since: number | null; subagents: number | null; requests: string[];
  completion: string | null; unseen: boolean;
}
export interface HostInfo {
  id: string; connectionId: string | null; label: string; route: string;
  connection: "connecting" | "online" | "backoff" | "blocked";
  freshness: "unknown" | "current" | "stale"; receivedAt: number | null; error: string | null;
  snapshot: { version: 1; hostId: string; observedAt: number; capabilities: string[];
    sessions: HostSession[]; total: number; partial: boolean } | null;
}
export interface HostsSnapshot { version: 1; hosts: HostInfo[]; error: string | null }
/** hosts.read: sanitized metadata across local and paired hosts. A session is
 * identified by (host.id, kind, id), never id alone. Stale state is last known
 * state, not proof that work stopped. No transcripts or approval payloads. */
export const hosts = {
  get: () => call<HostsSnapshot>("hosts.get"),
  async subscribe(listener: (snapshot: HostsSnapshot) => void): Promise<() => void> {
    let received = false;
    let revision = 0;
    const stop = host.on("addin.hosts", (data) => {
      const event = data as unknown as { rev: number; data: HostsSnapshot };
      if (event.rev <= revision) return;
      received = true; revision = event.rev; listener(event.data);
    });
    try {
      const initial = await call<HostsSnapshot>("hosts.subscribe");
      if (!received) listener(initial);
      let closed = false;
      return () => { if (closed) return; closed = true; stop(); void call("hosts.unsubscribe").catch(() => {}); };
    } catch (error) { stop(); throw error; }
  },
};
export interface Worktree { id: string; branch: string; path: string; primary: boolean }
export interface Project { id: string; name: string; root: string; worktrees: Worktree[] }
export interface Task { id: string; title: string; details: string; status: TaskStatus; worktreeId: string | null }
export interface AddinInfo {
  id: string; name: string; version: string; scope: "project" | "personal";
  /** The view this tab shows, and what it was opened with (`views.open`). */
  view: string; args: Json;
  /** Initial interface colors; use onPaletteChanged for later theme changes. */
  palette?: Record<string, string>;
}

/** Read-only metadata for this add-in's owning window; never terminal output or chat drafts. */
export interface WorkspaceContext {
  projectId: string | null;
  windowId?: string;
  activePane: number | null;
  panes: { id: number; active: boolean; activeTab: string | null;
    tabs: { id: string; title: string; kind: string; status: string | null }[] }[];
  selectedWorktreeId: string | null;
  selectedTaskId: string | null;
}
export type WorkspaceCommand =
  | "openProjects" | "openFiles" | "openPreview" | "openSessions" | "openSchedules" | "openSettings" | "openProjectSettings"
  | "panel:worktrees" | "panel:tasks" | "panel:shells" | "panel:usage" | "panel:addins" | "openCustomization"
  | "newTab" | "newShell" | "newChat" | "newSubshell" | "openVim" | "appPreview"
  | "openCodex" | "openClaude" | "openGrok" | "openMuse"
  | "splitRight" | "splitDown" | "closeTab" | "closePane" | "gatherTabs" | "defaultLayout"
  | "toggleFocusMode" | "focusCentered" | "nextTab" | "previousTab"
  | "selectTab1" | "selectTab2" | "selectTab3" | "selectTab4" | "selectTab5" | "selectTab6" | "selectTab7" | "selectTab8" | "selectTab9"
  | "newProjectWindow" | "openProjectOrchestrator";

function workspaceCommand(id: "selectTab" | "switchProject", args: { id: string }): Promise<void>;
function workspaceCommand(id: "selectPane", args: { id: number }): Promise<void>;
function workspaceCommand(id: WorkspaceCommand): Promise<void>;
function workspaceCommand(id: string, args?: { id: string | number }): Promise<void> {
  return call<void>("workspace.command", { id, args: args ?? {} });
}

export const workspace = {
  /** workspace.read: the latest sanitized context, scoped to this view's window. */
  get: () => call<WorkspaceContext>("workspace.get"),
  /** workspace.read: delivers an initial snapshot and then changed snapshots.
   * Resolves to an unsubscribe function; rejects if access is not granted.
   * Updates do not rebuild or restart the add-in. */
  async subscribe(listener: (context: WorkspaceContext) => void): Promise<() => void> {
    let received = false;
    const stop = host.on("addin.workspace", (data) => { received = true; listener(data as unknown as WorkspaceContext); });
    try {
      const initial = await call<WorkspaceContext>("workspace.get");
      // A newer pushed snapshot may have arrived while the initial call ran.
      if (!received) listener(initial);
      return stop;
    } catch (error) { stop(); throw error; }
  },
  /** workspace.control: dispatches a validated command in this view's window.
   * Resolution confirms dispatch, not completion. Existing locks and dialogs apply.
   * Closing a tab detaches its view; it does not terminate its shell. */
  command: workspaceCommand,
};

function call<T>(name: string, args: Record<string, unknown> = {}): Promise<T> {
  return host.call(`addin.${name}`, args) as Promise<T>;
}

/** This add-in, and the view this tab shows. */
export function info(): AddinInfo {
  return host.params.addin as unknown as AddinInfo;
}

/** project.read */
export const project = {
  get: () => call<Project>("project"),
  /** Shows a file of the project (a path relative to its root, or inside it) in Files. */
  openFile: (path: string) => call<void>("files.open", { path }),
};

export const tasks = {
  /** project.read */
  list: () => call<Task[]>("tasks.list"),
  /** tasks.write */
  add: (title: string, details = "") => call<Task>("tasks.add", { title, details }),
  /** tasks.write */
  setStatus: (id: string, status: TaskStatus) => call<Task>("tasks.status", { id, status }),
};

/** shell.run: runs `command` in a new shell tab the person sees, in the worktree (by id) or the project root. */
export function runInShell(command: string, options: { worktree?: string; title?: string } = {}) {
  return call<void>("shell.run", { command, ...options });
}

/** notify */
export function notify(title: string, body = "") {
  return call<void>("notify", { title, body });
}

/** status: the add-in's item in the status bar; `null` removes it. */
export function setStatus(text: string | null, tone: "normal" | "warning" | "critical" = "normal") {
  return call<void>("status", { text, tone });
}

/** What the add-in keeps between runs (at most 1 MB), its own. */
export const storage = {
  get: <T extends Json = Json>(key: string) => call<T | null>("storage.get", { key }),
  set: (key: string, value: Json) => call<void>("storage.set", { key, value }),
  remove: (key: string) => call<void>("storage.set", { key, value: null }),
};

/** The values the person set for the add-in's `settings`, else their defaults. */
export function settings() {
  return call<Record<string, Json>>("settings");
}

/** Opens a view of this add-in (or of another, by id) in a new tab, with `args` for it. */
export function openView(view: string, args: Json = null, addin?: string) {
  return call<void>("views.open", { view, args, addin });
}

export interface Worker {
  /** Sends `method` with `params`; resolves with the worker's `result` or rejects with its `error`. */
  request<T = Json>(method: string, params?: Json): Promise<T>;
  /** Hears the worker's `{"event": name, "data": ...}` lines. */
  on(event: string, listener: (data: Json) => void): () => void;
  stop(): Promise<void>;
}

const workerListeners = new Map<string, Map<string, Set<(data: Json) => void>>>();
let workerEvents: { remove(): void } | (() => void) | undefined;

/** Starts a worker the manifest declares. It stops when the tab closes, or with `stop`. */
export async function startWorker(id: string, args: string[] = []): Promise<Worker> {
  workerEvents ??= host.on("addin.worker", (data) => {
    const event = data as { worker: string; event: string; data: Json };
    for (const listener of workerListeners.get(event.worker)?.get(event.event) ?? []) listener(event.data);
  }) as unknown as { remove(): void };
  const { worker } = await call<{ worker: string }>("worker.start", { id, args });
  const listeners = new Map<string, Set<(data: Json) => void>>();
  workerListeners.set(worker, listeners);
  return {
    request: (method, params = null) => call("worker.request", { worker, method, params }),
    on(event, listener) {
      const set = listeners.get(event) ?? new Set();
      listeners.set(event, set);
      set.add(listener);
      return () => { set.delete(listener); };
    },
    async stop() { workerListeners.delete(worker); await call<void>("worker.stop", { worker }); },
  };
}

/** Hears the add-in's `commands` (from the command list) and settings changes. */
export function onCommand(listener: (command: string) => void) {
  return host.on("addin.command", (data) => listener((data as { id: string }).id));
}
export function onSettingsChanged(listener: (settings: Record<string, Json>) => void) {
  return host.on("addin.settings", (data) => listener(data as Record<string, Json>));
}
/** Interface colors changed. No permission required; this does not reload the view. */
export function onPaletteChanged(listener: (palette: Record<string, string>) => void): () => void {
  return host.on("addin.palette", (data) => listener(data as Record<string, string>));
}
