/** The global dashboard's words and rules over the engine's `dashboard.get` wire
 * (`riwork dashboard --json`, wire v1). Pure: ported from the GPUI dashboard
 * (crates/riwork-desktop/src/dashboard.rs), which stays the parity reference. */

export type Filter = "active" | "needs_you" | "working" | "in_progress" | "all";
export type Sort = "priority" | "recent" | "tasks" | "sessions" | "name";
export const FILTERS: Filter[] = ["active", "needs_you", "working", "in_progress", "all"];
export const SORTS: Sort[] = ["priority", "recent", "tasks", "sessions", "name"];
export const LIST_LIMIT = 12;
export const STALE_AFTER_SECS = 30;

export interface AgentCounts { working: number; waiting: number; done: number; unknown: number; exited: number; subagents: number }
export interface TaskCounts { todo: number; in_progress: number; done: number }
export type ChatStateWord = "starting" | "idle" | "running" | "waiting" | "stopped" | "failed";
export type Provider = "codex" | "claude" | "muse";
export interface Target { type: "shell" | "chat" | "project" | "task" | "automations"; id?: string; project_id?: string; orchestrator?: "global" | "project" }
export interface SessionRow { target: Target; agent: string; title?: string; project_id?: string; project_name?: string; branch?: string; orchestrator?: "global" | "project"; is_chat: boolean }
export interface WorkRow { session: SessionRow; activity: string; since_unix?: number; subagents: number }
export interface AttentionRow { kind: "waiting" | "chat_failed" | "automation"; target: Target; session?: SessionRow; project_id?: string; project_name?: string; at?: number; message?: string; title?: string; outcome?: string }
export interface ProjectRow {
  id: string; name: string; folder?: string; agents: AgentCounts; live_shells: number; live_chats?: number;
  tasks: TaskCounts; worktrees: number; attention: number;
  orchestrator?: { mode: "terminal" | "chat"; activity?: string; provider?: Provider; chat_state?: ChatStateWord; message?: string };
  last_activity?: number;
}
export interface TaskRow { id: string; title: string; project_id: string; project_name: string; branch?: string; updated_at: number }
export interface AutomationRow { id: string; title: string; project_id?: string; project_name?: string; paused: boolean; next_run?: number; last_run?: { outcome: string; at: number } }
export type GlobalOrchestrator =
  | { kind: "unknown" }
  | { kind: "none"; exited_terminal: boolean }
  | { kind: "terminal"; shell_id: string; activity?: string }
  | { kind: "chat"; chat_id: string; provider: Provider; chat_state: ChatStateWord; message?: string };
export interface Intelligence {
  state?: string; summary?: string; error?: string; stale?: boolean; generated_at?: number;
  highlights?: { text: string; source_event_ids?: unknown[] }[];
}
export interface DecisionRow { id: string; chat_id: string; request_id: string; session: SessionRow; observed_at: number; kind: "approval" | "question" }
export interface Wire {
  v: number; generated_at: number; chats_known: boolean;
  totals: { projects: number; active_projects: number; live_shells: number; live_chats?: number; agents: AgentCounts; tasks: TaskCounts; attention: number };
  global: GlobalOrchestrator;
  attention?: AttentionRow[]; attention_total?: number;
  working?: WorkRow[]; working_total?: number;
  finished?: WorkRow[]; finished_total?: number;
  projects?: ProjectRow[]; projects_total?: number;
  tasks_in_progress?: TaskRow[]; tasks_in_progress_total?: number;
  automations: { state: "not_read" } | { state: "unreadable"; message: string } | { state: "read"; total: number; paused: number; next?: { title: string; at: number; project_id?: string }; items?: AutomationRow[] };
  backends?: { state: "not_read" | "read" };
  decision_sources?: { state: "read" | "partial" | "unavailable"; chats_read: number; chats_complete: number; chats_total: number };
  decisions?: DecisionRow[];
  decisions_total?: number;
  recent_activity?: { session: SessionRow; source: { event_seq: number }; observed_at: number; log_updated_at: number; kind: string; text: string; status: string }[];
  recent_activity_total?: number;
  intelligence?: Intelligence;
}

export function filterLabel(filter: Filter): string {
  return { active: "Active", needs_you: "Needs you", working: "Working", in_progress: "In progress", all: "All" }[filter];
}
export function sortLabel(sort: Sort): string {
  return { priority: "Priority", recent: "Recent", tasks: "Tasks", sessions: "Sessions", name: "Name" }[sort];
}
export function nothingListed(filter: Filter): string {
  return {
    active: "No project has a live session, a live chat or an open task.",
    needs_you: "No project needs you right now.",
    working: "No agent is working in any project.",
    in_progress: "No project has a task in progress.",
    all: "No projects yet. Add one from a project window's Projects panel.",
  }[filter];
}

export function plural(count: number, noun: string): string {
  return count === 1 ? `1 ${noun}` : `${count} ${noun}s`;
}

/** `global_overview::short_duration`. */
export function shortDuration(seconds: number): string {
  if (seconds < 5) return "now";
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
  return `${Math.floor(seconds / 86400)}d`;
}

export function age(now: number, at?: number | null): string | undefined {
  if (at == null) return undefined;
  const short = shortDuration(Math.max(0, now - at));
  return short === "now" ? "just now" : `${short} ago`;
}

/** Every whitespace-separated word must appear, ignoring case, in some field. */
export function matches(query: string, fields: string[]): boolean {
  const lower = fields.map((field) => field.toLowerCase());
  return query.split(/\s+/).filter(Boolean).every((word) => lower.some((field) => field.includes(word.toLowerCase())));
}

/** `$HOME/x` → `~/x`, as `display_home` says a root. */
export function displayHome(path: string, home?: string): string {
  if (!home) return path;
  const trimmed = home.replace(/\/+$/, "");
  if (path === trimmed) return "~";
  return path.startsWith(`${trimmed}/`) ? `~${path.slice(trimmed.length)}` : path;
}

export function isQuiet(project: ProjectRow): boolean {
  return project.live_shells === 0 && (project.live_chats ?? 0) === 0 && project.tasks.todo + project.tasks.in_progress === 0;
}

export function passes(project: ProjectRow, filter: Filter): boolean {
  switch (filter) {
    case "active": return !isQuiet(project);
    case "needs_you": return project.attention > 0;
    case "working": return project.agents.working > 0;
    case "in_progress": return project.tasks.in_progress > 0;
    case "all": return true;
  }
}

function projectMatches(project: ProjectRow, query: string, roots: Record<string, string>): boolean {
  return matches(query, [project.name, project.folder ?? "", roots[project.id] ?? ""]);
}

/** The table's rows: the overview's order (busiest first) for Priority. */
export function visibleProjects(projects: ProjectRow[], query: string, filter: Filter, sort: Sort, roots: Record<string, string> = {}): ProjectRow[] {
  const shown = projects.filter((project) => passes(project, filter) && projectMatches(project, query, roots));
  const stable = <T,>(rows: T[], compare: (a: T, b: T) => number) =>
    rows.map((row, index) => ({ row, index })).sort((a, b) => compare(a.row, b.row) || a.index - b.index).map(({ row }) => row);
  switch (sort) {
    case "priority": return shown;
    case "recent": return stable(shown, (a, b) => (a.last_activity == null ? 1 : 0) - (b.last_activity == null ? 1 : 0) || (b.last_activity ?? 0) - (a.last_activity ?? 0));
    case "tasks": return stable(shown, (a, b) => b.tasks.in_progress - a.tasks.in_progress || (b.tasks.todo + b.tasks.in_progress) - (a.tasks.todo + a.tasks.in_progress));
    case "sessions": return stable(shown, (a, b) => (b.live_shells + (b.live_chats ?? 0)) - (a.live_shells + (a.live_chats ?? 0)));
    case "name": return stable(shown, (a, b) => a.name.toLowerCase() < b.name.toLowerCase() ? -1 : a.name.toLowerCase() > b.name.toLowerCase() ? 1 : a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
  }
}

export function filterCounts(projects: ProjectRow[], query: string, roots: Record<string, string> = {}): [Filter, number][] {
  const found = projects.filter((project) => projectMatches(project, query, roots));
  return FILTERS.map((filter) => [filter, found.filter((project) => passes(project, filter)).length]);
}

/** Why the table is empty, and what a button could show instead. */
export function emptyTable(projects: ProjectRow[], query: string, filter: Filter, roots: Record<string, string> = {}): { text: string; showAll?: string } {
  if (!query.trim()) return { text: nothingListed(filter) };
  const all = projects.filter((project) => projectMatches(project, query, roots)).length;
  if (filter === "all" || all === 0) return { text: `No project matches “${query.trim()}”.` };
  return {
    text: `No project in ${filterLabel(filter)} matches “${query.trim()}”; ${plural(all, "project")} ${all === 1 ? "does" : "do"} in All.`,
    showAll: `Show all ${plural(all, "project")}`,
  };
}

export function projectState(project: ProjectRow): { mark: string; tone: "gold" | "working" | "active" | "quiet"; word: string } {
  if (project.attention > 0) return { mark: "●", tone: "gold", word: "needs you" };
  if (project.agents.working > 0) return { mark: "●", tone: "working", word: "working" };
  if (!isQuiet(project)) return { mark: "●", tone: "active", word: "active" };
  return { mark: "○", tone: "quiet", word: "quiet" };
}

export function agentParts(project: ProjectRow): { text: string; tone: "gold" | "working" | "cyan" | "muted" }[] {
  const agents = project.agents;
  const parts: { text: string; tone: "gold" | "working" | "cyan" | "muted" }[] = [];
  if (agents.waiting > 0) parts.push({ text: `◌ ${agents.waiting} waiting`, tone: "gold" });
  if (agents.working > 0) parts.push({ text: `● ${agents.working} working`, tone: "working" });
  if (agents.done > 0) parts.push({ text: `✓ ${agents.done} done`, tone: "cyan" });
  if (agents.unknown > 0) parts.push({ text: `? ${agents.unknown} unknown`, tone: "muted" });
  return parts;
}

export function sessionsLine(project: ProjectRow, chatsKnown: boolean): string {
  const terminals = plural(project.live_shells, "terminal");
  if (!chatsKnown || project.live_chats == null) return `${terminals} · chats not read yet`;
  const live = project.live_shells + project.live_chats;
  return live === 0 ? "No live sessions" : `${live} live · ${terminals} · ${plural(project.live_chats, "chat")}`;
}

export function activityWord(activity?: string): string {
  switch (activity) {
    case "working": return "working";
    case "waiting": return "waiting for you";
    case "done": return "finished its turn";
    case "exited": return "agent exited";
    default: return "running";
  }
}

const PROVIDER: Record<Provider, string> = { codex: "Codex", claude: "Claude", muse: "Muse" };
const STATUS: Record<ChatStateWord, string> = { starting: "Starting", idle: "Idle", running: "Working", waiting: "Waiting", stopped: "Stopped", failed: "Failed" };
export function providerLabel(provider: Provider): string { return PROVIDER[provider] ?? provider; }
export function statusLabel(state: ChatStateWord): string { return STATUS[state] ?? state; }

export function orchestratorWords(project: ProjectRow): [string, string] | undefined {
  const orchestrator = project.orchestrator;
  if (!orchestrator) return undefined;
  if (orchestrator.mode === "terminal") return ["Terminal", activityWord(orchestrator.activity)];
  return [`${providerLabel(orchestrator.provider ?? "codex")} chat`, statusLabel(orchestrator.chat_state ?? "idle").toLowerCase()];
}

export function tasksWords(tasks: TaskCounts): [string, string] | undefined {
  const total = tasks.todo + tasks.in_progress + tasks.done;
  if (total === 0) return undefined;
  const open = tasks.in_progress === 0 && tasks.todo === 0 ? "all done"
    : tasks.in_progress === 0 ? `${tasks.todo} to do`
      : tasks.todo === 0 ? `${tasks.in_progress} in progress`
        : `${tasks.in_progress} in progress · ${tasks.todo} to do`;
  return [`${tasks.done}/${total} done`, open];
}

/** A project row's accessible name: every cell, then where a click goes. */
export function projectReaderName(project: ProjectRow, chatsKnown: boolean, now: number): string {
  const parts = [`Project ${project.name}`];
  if (project.folder) parts.push(`in ${project.folder}`);
  parts.push(project.attention > 0 ? (project.attention === 1 ? "1 needs you" : `${project.attention} need you`) : projectState(project).word);
  parts.push(...agentParts(project).map((part) => part.text.replace(/^[◌●✓? ]+/, "")));
  parts.push(sessionsLine(project, chatsKnown));
  const tasks = tasksWords(project.tasks);
  parts.push(tasks ? `tasks ${tasks[0]}, ${tasks[1]}` : "no tasks");
  const orchestrator = orchestratorWords(project);
  if (orchestrator) parts.push(`project orchestrator: ${orchestrator[0]}, ${orchestrator[1]}`);
  const when = age(now, project.last_activity);
  parts.push(when ? `last activity ${when}` : "no recorded activity");
  return `${parts.join(", ")}; opens the project`;
}

function place(projectId?: string, projectName?: string, branch?: string): string {
  const project = projectName ?? (projectId ? `Project ${projectId.slice(0, 8)}` : "Global");
  return branch ? `${project} · ${branch}` : project;
}

export function sessionName(session: SessionRow): string {
  const what = session.orchestrator === "global" ? `Global orchestrator (${session.agent})`
    : session.orchestrator === "project" ? `Project orchestrator (${session.agent})`
      : session.is_chat ? `${session.agent} chat` : session.agent;
  return session.title && !session.orchestrator ? `${what} · ${session.title}` : what;
}

export function sessionPlace(session: SessionRow): string {
  return place(session.project_id, session.project_name, session.branch);
}

export function oneLine(text: string): string {
  const line = (text.split("\n")[0] ?? "").trim();
  return [...line].length > 140 ? `${[...line].slice(0, 139).join("")}…` : line;
}

/** A needs-you row: its headline and detail line. */
export function attentionCopy(item: AttentionRow, now: number): [string, string] {
  const when = age(now, item.at);
  const where = place(item.project_id, item.project_name, item.session?.branch);
  const join = (parts: (string | undefined)[]) => parts.filter((part): part is string => !!part).join(" · ");
  switch (item.kind) {
    case "waiting":
      return [`${item.session ? sessionName(item.session) : "An agent"} waits for you`, join([where, when && `since ${when}`])];
    case "chat_failed":
      return [`${item.session ? sessionName(item.session) : "A chat"} failed`, join([where, oneLine(item.message ?? ""), when])];
    case "automation":
      return [`Automation “${item.title ?? ""}” ${item.outcome === "missed" ? "missed its run" : item.outcome === "uncertain" ? "may not have run" : "failed"}`,
        join([item.project_id ? where : "App", oneLine(item.message ?? ""), when])];
  }
}

/** What an empty attention list can honestly say: only what was read. */
export function quietAttention(wire: Wire): string {
  let text = "Nothing waits for you: no agent asks a question";
  const unknown: string[] = [];
  if (wire.chats_known) text += ", no chat failed";
  else unknown.push("Chats are still being read.");
  if (wire.automations.state === "read") text += ", no automation failed";
  else unknown.push(wire.automations.state === "not_read" ? "Automations are still being read." : "Automations could not be read.");
  return [`${text}.`, ...unknown].join(" ");
}

export function automationDetail(item: AutomationRow, now: number): string {
  const parts = [item.project_id ? place(item.project_id, item.project_name) : "App"];
  if (item.paused) parts.push("paused");
  else if (item.next_run == null) parts.push("no next run");
  else parts.push(item.next_run > now ? `next in ${shortDuration(item.next_run - now)}` : "due now");
  if (item.last_run) {
    const word = ({ failed: "last run failed", missed: "last run missed", uncertain: "last run uncertain", deferred: "last run deferred", dispatching: "running", submitted: "last run sent" } as Record<string, string>)[item.last_run.outcome] ?? item.last_run.outcome;
    const when = age(now, item.last_run.at);
    parts.push(when ? `${word} ${when}` : word);
  }
  return parts.join(" · ");
}

export function workDetail(work: WorkRow, now: number, finished: boolean): string {
  const parts = [sessionPlace(work.session)];
  if (work.since_unix != null) {
    parts.push(finished ? `finished ${age(now, work.since_unix)}` : `for ${shortDuration(Math.max(0, now - work.since_unix))}`);
  }
  if (work.subagents > 0) parts.push(plural(work.subagents, "subagent"));
  return parts.join(" · ");
}

/** The dashboard in a line (the status bar's Global menu shares it). */
export function summary(wire: Wire | null): string {
  if (!wire) return "Reading sessions…";
  const totals = wire.totals;
  const parts: string[] = [];
  if (totals.agents.working > 0) parts.push(`${totals.agents.working} working`);
  if (totals.attention > 0) parts.push(`${totals.attention} need you`);
  if (totals.tasks.in_progress > 0) parts.push(`${totals.tasks.in_progress} tasks in progress`);
  return parts.length ? parts.join(" · ") : `All quiet across ${plural(totals.projects, "project")}`;
}

/** The Overview (Haiku) section: the saved summary only. Hidden when off or
 * when there was nothing to summarize. */
export function intelligenceSection(intelligence: Intelligence | undefined, now: number):
  { summary?: string; highlights: string[]; status: string; error?: string } | null {
  if (!intelligence) return null;
  const state = intelligence.state ?? "unavailable";
  if (state === "disabled" || state === "no_evidence") return null;
  const stale = intelligence.stale === true;
  const status = stale && state === "refreshing" ? "Saved summary · updating"
    : stale ? "Saved summary · newer activity is available"
      : state === "ready" ? "From observed events"
        : state === "refreshing" ? "Updating summary…"
          : state === "rate_limited" ? "Waiting for the next summary refresh" : "Summary unavailable";
  const updated = age(now, intelligence.generated_at);
  return {
    summary: intelligence.summary || undefined,
    highlights: (intelligence.highlights ?? []).slice(0, 4).map((item) => `${item.text} · ${(item.source_event_ids ?? []).length} source events`),
    status: updated ? `${status} · ${updated}` : status,
    error: intelligence.error && (state === "not_configured" || state === "unavailable") ? intelligence.error : undefined,
  };
}

export interface Freshness { text: string; warn: boolean }
/** The toolbar: how old the figures are, and why they may be. */
export function freshness(now: number, readAt: number | null, error: string | null): Freshness {
  if (readAt == null) return error ? { text: `Sessions could not be read: ${error}`, warn: true } : { text: "Reading sessions…", warn: false };
  const old = Math.max(0, now - readAt);
  if (error) return { text: `Showing the read from ${shortDuration(old) === "now" ? "just now" : `${shortDuration(old)} ago`} · ${error}`, warn: true };
  if (old > STALE_AFTER_SECS) return { text: `Last read ${shortDuration(old)} ago`, warn: true };
  return { text: "Live", warn: false };
}

export interface Plan { mode: "automatic" | "terminal" | "chat"; provider?: string; model?: string; automatic: string[] }
export interface ChatCopy { action: string; status: string; note?: string; starts: boolean; enabled: boolean }

/** The Chat controls' words for the global orchestrator as it is (`dashboard::chat_copy`). */
export function chatCopy(global: GlobalOrchestrator, plan: Plan | null, starting: boolean): ChatCopy {
  const copy = (action: string, status: string, note: string | undefined, starts: boolean): ChatCopy => ({ action, status, note, starts, enabled: !starting });
  if (starting) return copy("Opening global chat…", "Finding the global orchestrator", undefined, false);
  switch (global.kind) {
    case "chat":
      return copy("Open global chat", `${providerLabel(global.provider)} · ${statusLabel(global.chat_state)}`,
        global.chat_state === "failed" ? `Its agent failed: ${global.message ?? ""}. The next message resumes it.`
          : global.chat_state === "stopped" ? "Stopped; the next message resumes it." : undefined, false);
    case "terminal":
      return copy("Open terminal orchestrator", `Runs in a terminal · ${activityWord(global.activity)}`,
        "RiWork keeps this terminal as it is; a chat takes over only after you end it.", false);
    case "none": {
      const status = global.exited_terminal ? "No global orchestrator · its last terminal has exited" : "No global orchestrator yet";
      const mode = plan?.mode ?? "automatic";
      if (mode === "terminal") return copy("Start global orchestrator", status, "Settings run new orchestrators in a terminal.", true);
      if (mode === "chat") {
        const provider = providerLabel((plan?.provider ?? "codex") as Provider);
        return copy("Start global chat", status, `Starts a ${provider} chat${plan?.model ? ` on ${plan.model}` : ""} that asks before commands and edits.`, true);
      }
      const [first, second] = (plan?.automatic ?? ["muse:muse-wonder-full-1.0", "codex:gpt-6.1-sol"]).map((entry) => {
        const [provider, ...model] = entry.split(":");
        return { provider: providerLabel(provider as Provider), model: model.join(":") };
      });
      return copy("Start global chat", status,
        `Starts a ${first.provider} chat on ${first.model} that asks before commands and edits, or a ${second.provider} chat on ${second.model} if ${first.provider} cannot start.`, true);
    }
    case "unknown":
      return copy("Open global chat", "Reading chats…", "Opens the global orchestrator there is, or starts one as Settings say.", true);
  }
}

// ---- Layout by the window's own width (`Layout::for_width`) --------------------------------

export type Arrangement = "wide" | "normal" | "compact";
export type Columns = "full" | "medium" | "narrow";
const PAGE = 10, RAIL = 340, GAP = 12, MARK = 10, CELL_GAP = 10, ROW_PAD = 8, PROJECT_MIN = 180;
export const COLUMN_WIDTHS: Record<Columns, { activity?: number; tasks: number; orchestrator?: number; last: number; actions: number }> = {
  full: { activity: 180, tasks: 170, orchestrator: 150, last: 76, actions: 124 },
  medium: { activity: 150, tasks: 130, last: 66, actions: 120 },
  narrow: { tasks: 120, last: 64, actions: 120 },
};
function minTable(columns: Columns): number {
  const w = COLUMN_WIDTHS[columns];
  const fixed = [MARK, w.activity, w.tasks, w.orchestrator, w.last, w.actions].filter((value): value is number => value != null);
  return fixed.reduce((sum, value) => sum + value, 0) + PROJECT_MIN + fixed.length * CELL_GAP + 2 * ROW_PAD;
}
export function layoutFor(width: number, scale = 1): { arrangement: Arrangement; columns: Columns } {
  const design = width / Math.max(scale, 1);
  const rails = (arrangement: Arrangement) => arrangement === "wide" ? 2 : arrangement === "normal" ? 1 : 0;
  const table = (arrangement: Arrangement) => design - 2 * PAGE - rails(arrangement) * (RAIL + GAP);
  const arrangement: Arrangement = table("wide") >= minTable("medium") ? "wide" : table("normal") >= minTable("medium") ? "normal" : "compact";
  const room = table(arrangement);
  const columns: Columns = room >= minTable("full") ? "full" : room >= minTable("medium") ? "medium" : "narrow";
  return { arrangement, columns };
}
export const DASHBOARD_METRICS = { page: PAGE, rail: RAIL, gap: GAP, mark: MARK, cellGap: CELL_GAP, rowPad: ROW_PAD, projectMin: PROJECT_MIN };

/** Where a click on a row goes, as the window routes it (`global_window::route`). */
export type Route =
  | { kind: "chat" }
  | { kind: "project"; projectId: string; step?: Record<string, unknown> }
  | { kind: "notice"; text: string };

export function routeTarget(target: Target, wire: Wire, shells: Record<string, string | null | undefined> = {}): Route {
  switch (target.type) {
    case "shell": {
      if (wire.global.kind === "terminal" && wire.global.shell_id === target.id) return { kind: "chat" };
      if (!(target.id! in shells)) return { kind: "notice", text: "That session has ended." };
      const project = shells[target.id!];
      return project ? { kind: "project", projectId: project, step: { kind: "showShell", shellId: target.id } } : { kind: "notice", text: "That session belongs to no project." };
    }
    case "chat":
      if (target.orchestrator === "global") return { kind: "chat" };
      return target.project_id ? { kind: "project", projectId: target.project_id, step: { kind: "chat", chatId: target.id } }
        : { kind: "notice", text: "This chat belongs to no project; open it with riwork chat." };
    case "project":
      return (wire.projects ?? []).some((project) => project.id === target.id) ? { kind: "project", projectId: target.id! } : { kind: "notice", text: "That project is gone." };
    case "task": {
      const task = (wire.tasks_in_progress ?? []).find((row) => row.id === target.id);
      return task ? { kind: "project", projectId: task.project_id, step: { kind: "task", taskId: task.id } } : { kind: "notice", text: "That task is gone." };
    }
    case "automations": {
      const project = target.project_id ?? wire.projects?.[0]?.id;
      return project ? { kind: "project", projectId: project, step: { kind: "panel", panel: "schedules" } } : { kind: "notice", text: "That project is gone." };
    }
  }
}
