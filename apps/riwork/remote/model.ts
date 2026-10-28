/** Paired-Mac data only. Remote IDs never pass through a local Store operation. */
export type RemoteLink = "online" | "connecting" | "offline" | null;
export interface RemoteHost { id: string; label: string; link: RemoteLink; status: string }
export interface RemoteProject { key: string; id: string; name: string; dimmed: boolean; stats: string }
export interface RemoteFolder {
  host: string; label: string; link: RemoteLink; count: number; creating: boolean;
  failure: string | null; note: string | null; projects: RemoteProject[];
}
export interface RemoteWorktree { id: string; branch: string; path: string; primary: boolean }
export interface RemoteTask { id: string; title: string; details: string; status: string; worktreeId: string | null }
export interface RemoteShell {
  id: string; projectId: string | null; worktreeId: string | null; orchestrator: boolean;
  chat: boolean; harness: string | null; alive: boolean; cwd: string; display: string;
}
export interface RemoteLists {
  key: string; host: string; project: string; hostLabel: string; projectName: string; link: RemoteLink; unpaired: boolean;
  worktrees: RemoteWorktree[]; worktreesLoading: boolean; worktreesError: string | null;
  tasks: RemoteTask[]; tasksLoading: boolean; tasksError: string | null;
  shells: RemoteShell[]; shellsLoading: boolean; shellsError: string | null;
  creating: boolean; failure: string | null;
}
export interface RemoteSnapshot {
  schema: number; hosts: RemoteHost[]; hostsError: string | null; folders: RemoteFolder[]; projects: RemoteLists[]; selected: string | null;
}
export interface RemoteWants {
  folders: boolean; settings: boolean; collapsed: string[]; tabHosts: string[];
  projectKey: string | null; worktrees: boolean; tasks: boolean; shells: boolean;
}
export interface RemoteDefaults { name: string; relay: string; routes: string }
export interface PairRequest { name: string; relay: string; routes: string }
export interface PairingLink { link: string; masked: string }
export type RemotePromptKind = { kind: "add" } | { kind: "pair" } | { kind: "project"; host: string; label: string };
export const EMPTY_REMOTE: RemoteSnapshot = { schema: 1, hosts: [], hostsError: null, folders: [], projects: [], selected: null };
export const EMPTY_WANTS: RemoteWants = { folders: false, settings: false, collapsed: [], tabHosts: [], projectKey: null, worktrees: false, tasks: false, shells: false };
export const REMOTE_HISTORY_NOTE = "Native Codex and Claude chat history is unavailable on remote hosts. Showing remote shells.";
export const REMOTE_UNPAIRED = "This Mac is no longer paired. Add it again in Settings.";

/** Classify exactly like remote_tree::parse_project_key (split once). Transport
 * validation is stricter, but an unusual saved remote key never becomes local. */
export function remoteProjectKey(key: string | null | undefined): { host: string; project: string } | null {
  if (!key?.startsWith("remote:")) return null;
  const suffix = key.slice(7), separator = suffix.indexOf(":");
  return separator > 0 && separator < suffix.length - 1 ? { host: suffix.slice(0, separator), project: suffix.slice(separator + 1) } : null;
}
export function remoteHostLabel(data: RemoteSnapshot, host: string): string {
  return data.hosts.find((row) => row.id === host)?.label ?? data.folders.find((row) => row.host === host)?.label ?? host.slice(0, 8);
}
export function remoteTitle(data: RemoteSnapshot, key: string): string | null {
  const parts = remoteProjectKey(key); if (!parts) return null;
  const project = data.projects.find((row) => row.key === key)?.projectName ?? data.folders.find((row) => row.host === parts.host)?.projects.find((row) => row.key === key)?.name ?? parts.project.slice(0, 8);
  return `${project} (on ${remoteHostLabel(data, parts.host)})`;
}
export function remoteLocalOnly(data: RemoteSnapshot, key: string | null, action?: "handoff"): string | null {
  const parts = remoteProjectKey(key); if (!parts) return null;
  return action === "handoff" ? "Hand off works on the projects of this Mac." : `Not available for a project on ${remoteHostLabel(data, parts.host)}`;
}
export function remotePanelUnavailable(panel: string): boolean {
  return ["files", "preview", "memo", "project_settings", "projectSettings", "schedules", "usage"].includes(panel);
}
export function linkPresentation(link: RemoteLink): { dot: string; tone: "cyan" | "gold" | "muted"; text: string } {
  if (link === "online") return { dot: "●", tone: "cyan", text: "Online" };
  if (link === "offline") return { dot: "○", tone: "muted", text: "Offline (or access revoked)" };
  return { dot: "●", tone: "gold", text: "Connecting…" };
}
export function remoteStrip(label: string, link: RemoteLink, ended: boolean): { text: string; tone: "magenta" | "gold"; reconnect: boolean } | null {
  if (ended) return { text: `Ended on ${label}`, tone: "magenta", reconnect: true };
  if (link === "connecting") return { text: `RECONNECTING · ${label}`, tone: "gold", reconnect: false };
  if (link === "offline") return { text: `RECONNECTING · ${label} · offline, or access was revoked`, tone: "gold", reconnect: false };
  return null; // no initial flash before the first status
}
export function filteredFolders(data: RemoteSnapshot, query: string): RemoteFolder[] {
  const needle = query.trim().toLocaleLowerCase(); if (!needle) return data.folders;
  return data.folders.map((folder) => {
    const all = folder.label.toLocaleLowerCase().includes(needle);
    const projects = folder.projects.filter((project) => all || `${project.name}\n${project.id}`.toLocaleLowerCase().includes(needle));
    projects.sort((a, b) => Number(!a.name.toLocaleLowerCase().includes(needle)) - Number(!b.name.toLocaleLowerCase().includes(needle)));
    return { ...folder, projects };
  }).filter((folder) => folder.projects.length > 0 || folder.label.toLocaleLowerCase().includes(needle));
}
export function projectConnectionNote(project: RemoteLists): string | null {
  if (project.unpaired) return REMOTE_UNPAIRED;
  if (project.link === "offline") return linkPresentation("offline").text;
  if (project.link !== "online") return `Connecting to ${project.hostLabel}…`;
  return null;
}
export function shellOpenRefusal(shell: RemoteShell, host: string): string | null {
  if (shell.chat) return `${shell.display} runs as a chat on ${host}; chats run on that Mac. Open it there, or in the iOS app.`;
  if (!shell.alive) return `${shell.display} has ended on ${host}`;
  return null;
}
export function checkPairingLink(text: string): string | null {
  const link = text.trim();
  if (!link) return "Paste the pairing link from the other Mac.";
  if (!link.startsWith("riwork://pair?") || /\s/u.test(link)) return "That is not a RiWork pairing link.";
  return null;
}
export function maskedPairingLink(link: string): string {
  const characters = [...link]; return characters.length ? `${characters.slice(0, 16).join("")}…  (${characters.length} characters)` : "";
}
export function projectNameError(text: string): string | null {
  const name = text.trim();
  if (!name) return "Enter a project name.";
  let bytes = 0; for (const char of name) { const code = char.codePointAt(0)!; bytes += code < 128 ? 1 : code < 2048 ? 2 : code < 65536 ? 3 : 4; }
  if ([...name].length > 100) return "A project name can have at most 100 characters.";
  if (bytes > 255) return "This name is too long for a folder name.";
  if (/[\x00-\x1f\x7f-\x9f\u2028\u2029]/u.test(name)) return "A project name cannot contain control characters.";
  if (/[\/\\]/u.test(name)) return "A project name cannot contain / or \\.";
  if (name.startsWith(".")) return "A project name cannot start with a dot.";
  if (name.startsWith("-")) return "A project name cannot start with a dash.";
  return null;
}

/** Shared by pointer/keyboard handlers: a pending operation cannot be submitted
 * twice; stale completion never clears a newly opened dialog. No automatic retry. */
export function singleFlight() {
  let pending = false;
  return {
    pending: () => pending,
    async run<T>(operation: () => Promise<T>): Promise<{ accepted: false } | { accepted: true; value: T }> {
      if (pending) return { accepted: false };
      pending = true;
      try { return { accepted: true, value: await operation() }; } finally { pending = false; }
    },
  };
}

/** Remove is a two-activation action; opening a prompt or losing the host resets it. */
export function removalConfirmation() {
  let selected: string | null = null;
  return {
    selected: () => selected,
    reset: () => { selected = null; },
    reconcile: (hosts: RemoteHost[]) => { if (!hosts.some((host) => host.id === selected)) selected = null; },
    press: (host: string, pending: boolean): boolean => {
      if (pending) return false;
      if (selected !== host) { selected = host; return false; }
      selected = null; return true;
    },
  };
}
