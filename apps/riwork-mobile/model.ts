/** What Swift still sends as `mobile.snapshot` (`ios/RiWorkRemote/NSHMobileBridge.swift`): the All hosts
 * overview it reads over its own short connections, and the device state the pages need. The hosts
 * themselves come from `relay.hosts`, a host's data from `relay.call`. Pairing keys, connection tokens and
 * credential-bearing URLs never enter this JS engine. */
export type Tone = "muted" | "working" | "gold" | "error";
export type DashboardFilter = "all" | "needs_you" | "working" | "projects";
export type DashboardSection = "decisions" | "attention" | "working" | "finished" | "tasks" | "automations" | "projects" | "history";
export interface DashboardRow {
  id: string; section: DashboardSection; hostID: string; hostName?: string;
  title: string; subtitle?: string; status?: string; tone: Tone; enabled: boolean;
  folder?: string; stale?: boolean;
}
export interface DashboardIntelligence {
  id: string; hostName?: string; state: string; stale: boolean; summary?: string; error?: string;
  model?: string; generatedAt?: number; highlights: string[]; followups: string[];
}
export interface Dashboard {
  available: boolean; unavailableReason?: string; freshness: string; stale: boolean; readAt?: number;
  summary: { id: string; label: string; value: number | null; coverage?: string }[];
  overview?: string; rows: DashboardRow[];
  /** `automationsError`: the automations could not be read, and why; drawn in the error tone, apart from the muted `notes`. */
  notes?: string[]; automationsError?: string; more?: Partial<Record<DashboardSection, number>>; intelligence?: DashboardIntelligence[];
}
export interface OverviewSnapshot {
  schema: 2; revision: number;
  /** A native Swift screen (a chat, the global orchestrator, a sheet) is over the pages. */
  nativePresented: boolean;
  /** The saved pairings could not be read: the Hosts page offers recovery. */
  libraryUnavailable: boolean;
  loading: boolean; error?: string;
  /** Per saved host, how fresh the overview's last read of it is ("Updated 2 min ago"). */
  details: Record<string, string>;
  /** Across your desktops: every saved host's dashboard, read by Swift. */
  homeDashboard?: Dashboard;
  /** A hardware keyboard is attached (arrow keys move through lists), and it is a Clicks keyboard. */
  keyboard: { hardware: boolean; clicks: boolean };
  /** How wide the window's own close, minimize and tile controls are at its top leading corner (iPadOS 26, a
   * resizable window), in points: the top row leaves that much clear. 0 or missing: none. */
  windowControls?: number;
  /** The interactive host's global orchestrator from `orchestrators.list`, for the page of a Mac without `dashboard.get`. */
  global?: { host: string; summary: string; action: "open" | "start" | "unavailable"; actionTitle: string };
}
/** Where Swift sends the pages (`nav.open`): a host's page after pairing or an overview row, a project at a tab. */
export interface NavOpen { page: "hosts" | "dashboard" | "projects" | "project"; host?: string; projectId?: string; name?: string; tab?: string; newTerminal?: boolean }

export const EMPTY_OVERVIEW: OverviewSnapshot = {
  schema: 2, revision: -1, nativePresented: false, libraryUnavailable: false, loading: false, details: {},
  keyboard: { hardware: false, clicks: false },
};

export const SECTION_LABELS: Record<DashboardSection, string> = {
  decisions: "Needs your decision", attention: "Needs attention", working: "Working now",
  finished: "Finished in the last hour", tasks: "Tasks in progress", automations: "Automations",
  projects: "Projects", history: "Recent activity",
};
export const SECTION_ORDER: DashboardSection[] = ["decisions", "attention", "working", "finished", "tasks", "automations", "projects", "history"];
