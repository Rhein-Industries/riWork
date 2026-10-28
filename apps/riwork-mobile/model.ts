/** Sanitized display protocol shared with MobileNshBridge.swift. Pairing keys,
 * connection tokens and credential-bearing URLs never enter this JS engine. */
export type Page = "hosts" | "dashboard" | "projects";
export type Tone = "muted" | "working" | "gold" | "error";
export interface MobilePalette {
  bg: string; panel: string; panelActive: string; divider: string; text: string;
  muted: string; cyan: string; gold: string; working: string; magenta: string; error: string;
}
export interface MobileTheme { scale: number; fontFamily: "system"; dark: boolean; palette: MobilePalette }
export interface Desktop {
  id: string; name: string; address: string; selected: boolean; connection: string; detail?: string;
}
export interface Project {
  id: string; name: string; path: string; selected: boolean; activity?: string; tone: Tone; folder?: string;
}
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
  notes?: string[]; more?: Partial<Record<DashboardSection, number>>; intelligence?: DashboardIntelligence[];
  global?: { summary: string; action: "open" | "start" | "unavailable"; actionTitle: string };
}
export interface MobileSnapshot {
  schema: 1; revision: number; route: Page; selectedHostID?: string;
  desktops: Desktop[];
  connection: { state: "disconnected" | "connecting" | "connected" | "suspended" | "failed"; label: string; error?: string };
  loading: boolean; error?: string; nativePresented: boolean; libraryUnavailable?: boolean;
  projects: Project[]; dashboard: Dashboard; homeDashboard?: Dashboard;
  projectSort?: { value: string; title: string; detail?: string };
  theme: MobileTheme; canCreateProject: boolean;
  projectsStale?: boolean;
  keyboard?: { hardware: boolean; clicks: boolean };
}
export type MobileAction =
  | { type: "connect"; hostID: string }
  | { type: "disconnect" | "refresh" | "showProjects" | "showDashboard" | "openGlobal" | "back" | "cycleProjectSort" }
  | { type: "navigate"; page: Page }
  | { type: "openProject" | "newTerminal"; projectID: string; hostID?: string }
  | { type: "setProjectSort"; value: "recent" | "name" | "dateAdded" }
  | { type: "openDashboardRow"; rowID: string }
  | { type: "present"; name: "addHost" | "settings" | "hostDetails" | "newProject" | "libraryRecovery"; hostID?: string };

export const EMPTY_DASHBOARD: Dashboard = { available: false, freshness: "Not read", stale: false, summary: [], rows: [] };
export const INITIAL_SNAPSHOT: MobileSnapshot = {
  schema: 1, revision: -1, route: "hosts", desktops: [], projects: [],
  connection: { state: "disconnected", label: "Not connected" }, loading: false, nativePresented: false,
  dashboard: EMPTY_DASHBOARD, canCreateProject: false,
  theme: { scale: 1, fontFamily: "system", dark: false, palette: {
    bg: "#ffffff", panel: "#f5f5f7", panelActive: "#e8e8ed", divider: "#d2d2d7",
    text: "#1d1d1f", muted: "#636366", cyan: "#007aff", gold: "#b34000",
    working: "#b34000", magenta: "#3a3a3c", error: "#c02927",
  } },
};

export const SECTION_LABELS: Record<DashboardSection, string> = {
  decisions: "Needs your decision", attention: "Needs attention", working: "Working now",
  finished: "Finished in the last hour", tasks: "Tasks in progress", automations: "Automations",
  projects: "Projects", history: "Recent activity",
};
export const SECTION_ORDER: DashboardSection[] = ["decisions", "attention", "working", "finished", "tasks", "automations", "projects", "history"];
