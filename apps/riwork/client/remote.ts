/** The ports of one host reached over the relay (`createRemoteServices`). Shared
 * code: nothing here may import the Mac's native module. Reads only while a
 * screen listens, never long-polls, and never repeats a call on its own. */
import {
  PortError, type DashboardPort, type DecisionApi, type Fresh, type HostRef, type LinkState, type ProjectsPort, type Services, type Subscription,
  type WorkspacePort,
} from "./ports";
import type { RemoteTransport } from "./transport";
import {
  DASHBOARD_SCHEMA, type DashboardDto, type HostChat, type HostShell, type ProjectSummary, type StoreTree, type TabEntry, type TabsReply,
} from "./types";

/** How often the dashboard is read while a screen shows it: the Mac's own
 * reads come every 2 to 10 s, and its freshness line calls a read older than
 * 30 s stale. */
export const DASHBOARD_POLL_MS = 10_000;

export interface RemoteOptions {
  pollMs?: number;
  /** Milliseconds since the epoch (tests pass their own). */
  clock?: () => number;
}

export function createRemoteServices(transport: RemoteTransport, host: HostRef, options: RemoteOptions = {}): Services {
  const clock = options.clock ?? (() => Date.now());
  return {
    host,
    link: { current: () => transport.link(), subscribe: (listener) => transport.onLink(listener) },
    dashboard: remoteDashboard(transport, options.pollMs ?? DASHBOARD_POLL_MS, clock),
    projects: remoteProjects(transport, clock),
    workspace: remoteWorkspace(transport),
    features: () => transport.features(),
  };
}

/** A call that needs the link: `offline` at once without it, so a write fails fast and never waits. */
async function linked(transport: RemoteTransport, method: string, params: Record<string, unknown>, options?: { timeoutMs?: number }): Promise<unknown> {
  const link = transport.link();
  if (link.state !== "connected") throw new PortError("offline", link.error ?? link.label);
  try { return await transport.call(method, params, options); }
  catch (cause) { throw PortError.from(cause); }
}

/** `shell.create` and `project.create` may take a minute and a half on the Mac (protocol "Terminal creation"). */
const CREATE_TIMEOUT_MS = 90_000;
const str = (value: unknown): string | undefined => typeof value === "string" ? value : undefined;
const num = (value: unknown): number | undefined => typeof value === "number" && Number.isFinite(value) ? value : undefined;
const record = (value: unknown): Record<string, unknown> => value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};

/** A `tabs.*` reply; entries without a key or kind are dropped, children are nested as the wire nests them. */
export function tabsReply(result: unknown): TabsReply {
  const raw = record(result);
  if (!Array.isArray(raw.entries)) throw new PortError("failed", "The host's tab list could not be read.");
  const entry = (value: unknown): TabEntry[] => {
    const row = record(value);
    const key = str(row.key), kind = str(row.kind);
    if (!key || !kind) return [];
    const children = Array.isArray(row.children) ? row.children.flatMap(entry) : [];
    return [{
      key, kind, title: str(row.title) ?? "", status: str(row.status) ?? "unknown", hidden: row.hidden === true, worker: row.worker === true,
      order: num(row.order) ?? 0, parent: str(row.parent) ?? null, children, childCount: num(row.child_count) ?? children.length,
    }];
  };
  return { epoch: str(raw.epoch) ?? null, revision: num(raw.revision) ?? null, entries: raw.entries.flatMap(entry) };
}

/** A `shells.list` Session (also `shell.create`'s `shell`). */
export function hostShell(value: unknown): HostShell | null {
  const row = record(value);
  const id = str(row.id);
  if (!id) return null;
  const shell: HostShell = {
    id, projectId: str(row.project_id) ?? null, worktreeId: str(row.worktree_id) ?? null, kind: str(row.kind) ?? "project",
    cwd: str(row.cwd) ?? "", harness: str(row.harness) ?? null, alive: row.alive !== false, createdAt: num(row.created_at_unix) ?? 0,
  };
  if (str(row.activity)) shell.activity = str(row.activity);
  if (num(row.last_activity_unix) != null) shell.lastActivity = num(row.last_activity_unix);
  if (str(row.mode)) shell.mode = str(row.mode);
  if (shell.mode === "chat" && str(row.chat_id)) shell.chatId = str(row.chat_id);
  if (str(row.provider)) shell.provider = str(row.provider);
  return shell;
}

/** A `chats.list` ChatInfo: the title it is shown by (a rename or explicit name, the provider's, the first message, then the kind). */
export function hostChat(value: unknown): HostChat | null {
  const row = record(value);
  const id = str(row.id), provider = str(row.provider);
  if (!id || !provider) return null;
  const state = record(row.state);
  const kind = provider === "codex" ? "Codex chat" : provider === "claude" ? "Claude chat" : provider === "muse" ? "Muse chat" : provider === "acp" ? "ACP chat" : "Chat";
  const chat: HostChat = {
    id, provider, title: str(row.user_title) || str(row.provider_title) || str(row.title) || str(row.first_user_message) || kind,
    projectId: str(row.project_id) ?? null, worktreeId: str(row.worktree_id) ?? null,
    state: str(state.state) ?? str(row.state) ?? "unknown", createdAt: num(row.created_at_unix) ?? 0,
  };
  if (str(state.message)) chat.message = str(state.message);
  return chat;
}

function remoteWorkspace(transport: RemoteTransport): WorkspacePort {
  const has = (feature: string) => transport.features().includes(feature);
  const list = <T>(result: unknown, key: string, map: (value: unknown) => T | null): T[] => {
    const rows = record(result)[key];
    if (!Array.isArray(rows)) throw new PortError("failed", "The host's answer could not be read.");
    return rows.flatMap((row) => { const value = map(row); return value ? [value] : []; });
  };
  return {
    async tabs(projectId) {
      if (!has("tabs")) return null;
      return tabsReply(await linked(transport, "tabs.list", { project_id: projectId }));
    },
    openTab: async (projectId, key) => tabsReply(await linked(transport, "tabs.open", { project_id: projectId, key })),
    updateTabs: async (projectId, update) => tabsReply(await linked(transport, "tabs.update", { project_id: projectId, update })),
    shells: async (projectId) => list(await linked(transport, "shells.list", { project_id: projectId }), "shells", hostShell),
    async chats(projectId) {
      if (!has("chat")) return [];
      return list(await linked(transport, "chats.list", { project_id: projectId }), "chats", hostChat);
    },
    worktrees: async (projectId) => list(await linked(transport, "worktrees.list", { project_id: projectId }), "worktrees", (value) => {
      const row = record(value);
      const id = str(row.id), branch = str(row.branch);
      return id && branch ? { id, projectId: str(row.project_id) ?? projectId, branch, path: str(row.path) ?? "", isPrimary: row.is_primary === true } : null;
    }),
    async createShell(request) {
      const params: Record<string, unknown> = { kind: request.kind };
      if (request.worktreeId) params.worktree_id = request.worktreeId; else params.project_id = request.projectId;
      if (request.kind !== "shell") {
        if (request.asSettings) params.as_settings = true;
        else if (request.unrestricted) params.unrestricted = true;
      }
      const result = record(await linked(transport, "shell.create", params, { timeoutMs: CREATE_TIMEOUT_MS }));
      const shell = hostShell(result.shell) ?? (str(result.shell_id) ? hostShell({ id: result.shell_id, project_id: request.projectId ?? null, worktree_id: request.worktreeId ?? null }) : null);
      if (!shell) throw new PortError("uncertain", "The Mac's answer could not be read. Look at the tabs before making another terminal.");
      return shell;
    },
    async closeShell(shellId) {
      await linked(transport, "shell.close", { shell_id: shellId }, { timeoutMs: 30_000 });
    },
  };
}

/** The decision calls of a device without a responder of its own: nothing can be answered from here. */
const NO_DECISIONS: DecisionApi = {
  reviewDecision: () => Promise.reject(new PortError("unsupported", "Requests on this host are answered in their chat.")),
  respondDecision: () => Promise.reject(new PortError("unsupported", "Requests on this host are answered in their chat.")),
};

/** `dashboard.get` as the Mac's dashboard snapshots: the fields the wire lacks
 * are filled in here. `now` and `readAt` are on the host's clock (the wire's
 * own `generated_at`), so every age is measured as the Mac measures it;
 * `backendLines` stay null, as `dashboard.get` has none. */
function remoteDashboard(transport: RemoteTransport, pollMs: number, clock: () => number): DashboardPort {
  const listeners = new Set<(dto: DashboardDto) => void>();
  let last: DashboardDto | null = null;
  let wire: unknown = null, readAt: number | null = null, error: string | null = null;
  /** The host's clock minus this device's, in seconds, from the last good read. */
  let offset = 0;
  let reading: Promise<void> | null = null, again = false;
  let timer: number | null = null, linkSubscription: Subscription | null = null, linkState: LinkState["state"] | null = null;
  const deviceNow = () => clock() / 1000;

  function publish() {
    last = { schema: DASHBOARD_SCHEMA, now: Math.floor(deviceNow() + offset), readAt, error, wire, backendLines: null };
    for (const listener of [...listeners]) listener(last);
  }
  /** Why nothing is read now: a link on its way says nothing yet; any other says what it is. */
  function unread(link: LinkState) { if (link.state !== "connecting") error = link.error ?? link.label; }
  async function readOnce() {
    const link = transport.link();
    if (link.state !== "connected") { unread(link); publish(); return; }
    try {
      const result = await transport.call("dashboard.get", {});
      const arrived = deviceNow();
      const generated = (result as { generated_at?: unknown } | null)?.generated_at;
      const at = typeof generated === "number" && Number.isFinite(generated) ? generated : arrived;
      offset = at - arrived;
      wire = result; readAt = Math.floor(at); error = null;
    } catch (cause) {
      // The last good wire stays; the freshness line says how old it is and why.
      error = PortError.from(cause).message;
    }
    publish();
  }
  /** A read asked for during one runs once more after it, as on the Mac. */
  function read(): Promise<void> {
    if (reading) { again = true; return reading; }
    const run = async () => {
      do { again = false; await readOnce(); } while (again);
    };
    reading = run().finally(() => { reading = null; });
    return reading;
  }
  function start() {
    linkState = transport.link().state;
    linkSubscription = transport.onLink((link) => {
      const previous = linkState;
      linkState = link.state;
      if (link.state === "connected" && previous !== "connected") void read();
      else if (link.state !== previous) { unread(link); publish(); }
    });
    void read();
    timer = setInterval(() => { void read(); }, pollMs);
  }
  function stop() {
    if (timer != null) clearInterval(timer);
    timer = null;
    linkSubscription?.close();
    linkSubscription = null;
  }
  return {
    subscribe(listener) {
      listeners.add(listener);
      if (last) listener(last);
      if (listeners.size === 1) start();
      let open = true;
      return { close() { if (open && listeners.delete(listener) && listeners.size === 0) stop(); open = false; } };
    },
    refresh: () => read(),
    decisions: transport.decisions ?? NO_DECISIONS,
  };
}

/** `projects.list` entries as the shared screens list them. */
export function projectSummaries(result: unknown): ProjectSummary[] {
  const rows = (result as { projects?: unknown } | null)?.projects;
  if (!Array.isArray(rows)) throw new PortError("failed", "The host's project list could not be read.");
  return rows.flatMap((row: unknown) => {
    const project = projectSummary(record(row));
    return project ? [project] : [];
  });
}

/** One `projects.list` Project (also `project.create`'s `project`). */
function projectSummary(row: Record<string, unknown>): ProjectSummary | null {
  if (typeof row.id !== "string" || typeof row.name !== "string" || typeof row.root !== "string") return null;
  const project: ProjectSummary = { id: row.id, name: row.name, root: row.root };
  if (typeof row.created_at === "number") project.createdAt = row.created_at;
  if (typeof row.last_activity_unix === "number") project.lastActivity = row.last_activity_unix;
  if (typeof row.last_edited_unix === "number") project.lastEdited = row.last_edited_unix;
  const agents = record(row.agents);
  if (num(agents.working) != null || num(agents.waiting) != null) {
    project.agents = { working: num(agents.working) ?? 0, waiting: num(agents.waiting) ?? 0 };
    if (num(agents.done) != null) project.agents.done = num(agents.done);
  }
  return project;
}

/** `Store.snapshot`'s value as the shared Projects page reads it (the Mac's `StoreState`). */
export function storeTree(value: unknown): StoreTree {
  const state = record(value);
  if (!Array.isArray(state.projects)) throw new PortError("failed", "The host's projects could not be read.");
  const projects = state.projects.flatMap((value) => {
    const row = record(value);
    const id = str(row.id), name = str(row.name), root = str(row.root);
    return id && name && root != null ? [{ id, name, root, folderId: str(row.folderId) ?? null, createdAt: num(row.createdAt) ?? 0 }] : [];
  });
  const projectFolders = (Array.isArray(state.projectFolders) ? state.projectFolders : []).flatMap((value) => {
    const row = record(value);
    const id = str(row.id), name = str(row.name);
    return id && name ? [{ id, name, parentId: str(row.parentId) ?? null, createdAt: num(row.createdAt) ?? 0 }] : [];
  });
  const projectRecency: Record<string, number> = {};
  for (const [id, at] of Object.entries(record(state.projectRecency))) if (num(at) != null) projectRecency[id] = at as number;
  return { projects, projectFolders, projectRecency };
}

function remoteProjects(transport: RemoteTransport, clock: () => number): ProjectsPort {
  let fresh: Fresh<ProjectSummary[]> = { value: null, readAt: null, error: null };
  const native = () => transport.features().includes("native:Store.snapshot");
  return {
    // Only on a host whose connection lists the method: an older one has no tree, and the page lists without folders.
    get tree() {
      if (!native()) return undefined;
      return async () => storeTree(record(await linked(transport, "native.invoke", { module: "Store", method: "snapshot", args: {} })).value);
    },
    async create(name, git) {
      const params: Record<string, unknown> = { name };
      if (!git) params.git = false;
      const result = record(await linked(transport, "project.create", params, { timeoutMs: CREATE_TIMEOUT_MS }));
      const project = projectSummary(record(result.project));
      if (!project) throw new PortError("uncertain", "The Mac's answer could not be read. Look at the project list before making it again.");
      return project;
    },
    async list() {
      const link = transport.link();
      try {
        if (link.state !== "connected") throw new PortError("offline", link.error ?? link.label);
        const value = projectSummaries(await transport.call("projects.list", {}));
        fresh = { value, readAt: Math.floor(clock() / 1000), error: null };
        return value;
      } catch (cause) {
        const failure = PortError.from(cause);
        fresh = { ...fresh, error: failure };
        throw failure;
      }
    },
    last: () => fresh,
  };
}
