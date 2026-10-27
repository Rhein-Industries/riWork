/** Frozen host launch payload from docs/nsh-contracts.md §2.2. */
export interface HostParams {
  schema: 1;
  windowId: string;
  kind: "project" | "global";
  projectId: string | null;
  path: string;
  restore?: {
    /** Opaque ProjectLayout: passed to Rust unchanged, never interpreted by TSX. */
    layout?: unknown;
    focusMode?: boolean;
    focusCentered?: boolean;
    global?: unknown;
  };
  cascade?: number;
  global?: { view: "dashboard" | "chat"; start: boolean };
  notice?: string | null;
  appearance?: "light" | "dark" | "system";
  build?: { version: string; commit: string; debug: boolean };
}

export type PanelKind = "shell" | "remote_shell" | "panel" | "chat" | "app_preview" | "empty";
export type PanelStatus = "working" | "waiting" | "error" | "done" | "idle" | "stopped";

/** View data from Rust; id is the stable DockArea key, not a display label. */
export interface PanelModel {
  id: string;
  kind: PanelKind;
  title: string;
  status?: PanelStatus;
}

export interface PaneModel {
  id: number;
  locked: boolean;
  main: boolean;
  active: boolean;
  /** Already in drawn order: shared membership ranking belongs to Rust. */
  tabs: PanelModel[];
  activeTab?: string;
}

export interface WorkspaceSnapshot {
  panes: PaneModel[];
  activePane: number;
  notice?: string | null;
}

export function windowTitle(
  projectName?: string | null,
  remoteHost?: string | null,
  kind: "project" | "global" = "project",
): string {
  if (kind === "global") return "RiWork · Global";
  const title = projectName ? `RiWork · ${projectName}` : "RiWork";
  return remoteHost ? `${title} (on ${remoteHost})` : title;
}

/** Avoid repeated AppKit title-bar relayout (SHELL-023). Scoped to one window. */
export function createTitleUpdater(setTitle: (title: string) => void): (title: string) => void {
  let previous: string | undefined;
  return (title) => {
    if (title === previous) return;
    setTitle(title);
    previous = title;
  };
}

/** Wait for project metadata, then announce only actual Dock-label changes. */
export function createDockUpdater(post: (info: { project: string; branch: string | null }) => void) {
  let previous: string | undefined;
  return (resolved: boolean, project: string, branch: string | null = null) => {
    if (!resolved) return;
    const identity = JSON.stringify([project, branch]);
    if (identity === previous) return;
    previous = identity;
    post({ project, branch });
  };
}

const PANEL_TITLES: Record<string, string> = {
  projects: "Projects",
  worktrees: "Worktrees",
  files: "Files",
  preview: "Preview",
  tasks: "Tasks",
  shells: "Sessions",
  usage: "Usage",
  settings: "Settings",
  project_settings: "Project settings",
  schedules: "Automations",
  dashboard: "Dashboard",
};

/** Labels keep legacy saved identities (shells, schedules) unchanged. */
export function panelTitle(panelId: string): string {
  const key = panelId.startsWith("panel:") ? panelId.slice(6) : panelId;
  return PANEL_TITLES[key] ?? key;
}

export function activePane(snapshot: WorkspaceSnapshot): PaneModel | undefined {
  return snapshot.panes.find((pane) => pane.id === snapshot.activePane);
}

export function findPanel(snapshot: WorkspaceSnapshot, id: string): PanelModel | undefined {
  for (const pane of snapshot.panes) {
    const panel = pane.tabs.find((tab) => tab.id === id);
    if (panel) return panel;
  }
  return undefined;
}

/** Empty sentinels hold geometry and unknown disk entries, but are not user tabs. */
export function drawnTabs(pane: PaneModel): PanelModel[] {
  return pane.tabs.filter((tab) => tab.kind !== "empty" && !tab.id.startsWith("empty:"));
}

/** Cmd+1…8 and Cmd+9 follow the Rust-projected strip (L5, SHELL-116). */
export function numberedTab(
  pane: PaneModel,
  number: number,
  sharedMembership: boolean,
  modalOpen = false,
): PanelModel | undefined {
  if (modalOpen || !Number.isInteger(number) || number < 1 || number > 9) return undefined;
  const tabs = drawnTabs(pane).filter((tab) => !sharedMembership || tab.kind !== "panel");
  return number === 9 ? tabs[tabs.length - 1] : tabs[number - 1];
}

/** Cycling includes panels and never changes the Rust-owned pane tree. */
export function cycledTab(
  pane: PaneModel,
  activeId: string | undefined,
  direction: 1 | -1,
  modalOpen = false,
): PanelModel | undefined {
  if (modalOpen) return undefined;
  const tabs = drawnTabs(pane);
  if (tabs.length === 0) return undefined;
  const current = tabs.findIndex((tab) => tab.id === activeId);
  if (current < 0) return direction === 1 ? tabs[0] : tabs[tabs.length - 1];
  return tabs[(current + direction + tabs.length) % tabs.length];
}

export function mainPaneSummary(snapshot: WorkspaceSnapshot): string {
  const main = snapshot.panes.find((pane) => pane.main);
  if (!main) return "none";
  const tabs = drawnTabs(main);
  if (tabs.length === 0) return "empty";
  const selected = tabs.find((tab) => tab.id === main.activeTab) ?? tabs[0];
  return tabs.length === 1 ? selected.title : `${selected.title} · ${tabs.length} tabs`;
}
