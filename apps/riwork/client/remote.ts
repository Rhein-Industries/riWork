/** The ports of one host reached over the relay (`createRemoteServices`). Shared
 * code: nothing here may import the Mac's native module. Reads only while a
 * screen listens, never long-polls, and never repeats a call on its own. */
import {
  PortError, type DashboardPort, type DecisionApi, type Fresh, type HostRef, type LinkState, type ProjectsPort, type Services, type Subscription,
} from "./ports";
import type { RemoteTransport } from "./transport";
import { DASHBOARD_SCHEMA, type DashboardDto, type ProjectSummary } from "./types";

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
  return rows.flatMap((row: Record<string, unknown>) => {
    if (typeof row?.id !== "string" || typeof row.name !== "string" || typeof row.root !== "string") return [];
    const project: ProjectSummary = { id: row.id, name: row.name, root: row.root };
    if (typeof row.created_at === "number") project.createdAt = row.created_at;
    if (typeof row.last_activity_unix === "number") project.lastActivity = row.last_activity_unix;
    return [project];
  });
}

function remoteProjects(transport: RemoteTransport, clock: () => number): ProjectsPort {
  let fresh: Fresh<ProjectSummary[]> = { value: null, readAt: null, error: null };
  return {
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
