/** The global dashboard on a busy afternoon, in memory: six projects, agents that
 * work and wait, two provider requests, failed and missed automations and a Haiku
 * overview. No native read, chat host, provider or window is reached. */
import { View, mount, signal } from "nsh";
import type { DashboardDto } from "riwork/native";
import { DEFAULT_THEME } from "../appearance";
import { DashboardView } from "./Dashboard";
import type { DecisionApi, Review } from "./DecisionReview";
import type { AgentCounts, DecisionRow, Filter, ProjectRow, SessionRow, Sort, TaskCounts, Wire } from "./model";

const NOW = 1_793_400_000;
const ago = (seconds: number) => NOW - seconds;
const agents = (counts: Partial<AgentCounts> = {}): AgentCounts => ({ working: 0, waiting: 0, done: 0, unknown: 0, exited: 0, subagents: 0, ...counts });
const tasks = (todo: number, in_progress: number, done: number): TaskCounts => ({ todo, in_progress, done });
const chat = (id: string, project_id: string, project_name: string, title: string, branch?: string, agent = "claude"): SessionRow =>
  ({ target: { type: "chat", id, project_id }, agent, title, project_id, project_name, branch, is_chat: true });
const shell = (id: string, project_id: string, project_name: string, agent: string, branch?: string): SessionRow =>
  ({ target: { type: "shell", id }, agent, project_id, project_name, branch, is_chat: false });

const PROJECTS: ProjectRow[] = [
  { id: "riwork", name: "riWork", folder: "~/orca/projects/riWork", agents: agents({ working: 2, waiting: 1, done: 1, subagents: 3 }), live_shells: 3, live_chats: 2,
    tasks: tasks(4, 2, 11), worktrees: 3, attention: 2, orchestrator: { mode: "chat", provider: "claude", chat_state: "running" }, last_activity: ago(20) },
  { id: "nsh", name: "nsh", folder: "~/orca/projects/nsh", agents: agents({ working: 1 }), live_shells: 1, live_chats: 1,
    tasks: tasks(2, 1, 7), worktrees: 1, attention: 0, orchestrator: { mode: "terminal", activity: "working" }, last_activity: ago(95) },
  { id: "ios", name: "Somtus companion for iPhone and iPad", folder: "~/orca/projects/riWork/ios", agents: agents({ waiting: 1 }), live_shells: 0, live_chats: 1,
    tasks: tasks(3, 0, 2), worktrees: 0, attention: 1, last_activity: ago(14 * 60) },
  { id: "site", name: "orca-site", folder: "~/orca/projects/orca-site", agents: agents({ done: 2 }), live_shells: 1, live_chats: 0,
    tasks: tasks(0, 1, 5), worktrees: 2, attention: 0, last_activity: ago(2 * 3600) },
  { id: "pixel", name: "pixel-editor", folder: "~/code/pixel-editor", agents: agents(), live_shells: 0, live_chats: 0,
    tasks: tasks(0, 0, 9), worktrees: 0, attention: 0, last_activity: ago(3 * 86400) },
  { id: "notes", name: "notes", folder: "~/notes", agents: agents(), live_shells: 0, live_chats: 0,
    tasks: tasks(0, 0, 0), worktrees: 0, attention: 0 },
];

const BUILDER = chat("chat-builder", "riwork", "riWork", "Dashboard polish", "panel/dashboard");
const PLANNER = chat("chat-planner", "ios", "Somtus companion for iPhone and iPad", "Sync plan");
const DECISIONS: DecisionRow[] = [
  { id: "approval-1", chat_id: "chat-builder", request_id: "request-1", session: BUILDER, observed_at: ago(40), kind: "approval" },
  { id: "question-1", chat_id: "chat-planner", request_id: "request-2", session: PLANNER, observed_at: ago(6 * 60), kind: "question" },
];

const WIRE: Wire = {
  v: 1, generated_at: NOW, chats_known: true,
  global: { kind: "chat", chat_id: "global-chat", provider: "claude", chat_state: "idle" },
  totals: { projects: 6, active_projects: 4, live_shells: 5, live_chats: 4, agents: agents({ working: 3, waiting: 2, done: 3, subagents: 3 }), tasks: tasks(9, 4, 34), attention: 4 },
  projects: PROJECTS, projects_total: PROJECTS.length,
  attention: [
    { kind: "waiting", target: BUILDER.target, session: BUILDER, project_id: "riwork", project_name: "riWork", at: ago(40) },
    { kind: "waiting", target: PLANNER.target, session: PLANNER, project_id: "ios", project_name: "Somtus companion for iPhone and iPad", at: ago(6 * 60) },
    { kind: "chat_failed", target: { type: "chat", id: "chat-docs", project_id: "riwork" }, session: chat("chat-docs", "riwork", "riWork", "Docs pass", undefined, "codex"),
      project_id: "riwork", project_name: "riWork", at: ago(22 * 60), message: "The provider stopped: rate limit reached for gpt-6.1-sol, try again in 4 minutes" },
    { kind: "automation", target: { type: "automations", project_id: "site" }, project_id: "site", project_name: "orca-site", title: "Nightly link check", outcome: "missed", at: ago(5 * 3600) },
  ],
  attention_total: 4,
  working: [
    { session: shell("shell-1", "riwork", "riWork", "claude", "panel/dashboard"), activity: "working", since_unix: ago(7 * 60), subagents: 3 },
    { session: chat("chat-tests", "riwork", "riWork", "Fix flaky layout test", "main", "codex"), activity: "working", since_unix: ago(2 * 60), subagents: 0 },
    { session: shell("shell-2", "nsh", "nsh", "codex", "text-shaping"), activity: "working", since_unix: ago(31 * 60), subagents: 0 },
  ],
  working_total: 3,
  finished: [
    { session: chat("chat-copy", "site", "orca-site", "Pricing page copy"), activity: "done", since_unix: ago(12 * 60), subagents: 0 },
    { session: shell("shell-3", "riwork", "riWork", "claude", "status-bar"), activity: "done", since_unix: ago(48 * 60), subagents: 1 },
  ],
  finished_total: 2,
  tasks_in_progress: [
    { id: "task-1", title: "Dashboard: clearer decision rows", project_id: "riwork", project_name: "riWork", branch: "panel/dashboard", updated_at: ago(3 * 60) },
    { id: "task-2", title: "Status bar shows the global summary", project_id: "riwork", project_name: "riWork", branch: "status-bar", updated_at: ago(50 * 60) },
    { id: "task-3", title: "Shape Arabic and Devanagari runs", project_id: "nsh", project_name: "nsh", branch: "text-shaping", updated_at: ago(31 * 60) },
    { id: "task-4", title: "Move the blog to the new layout", project_id: "site", project_name: "orca-site", updated_at: ago(2 * 3600) },
  ],
  tasks_in_progress_total: 4,
  automations: { state: "read", total: 3, paused: 1, items: [
    { id: "auto-1", title: "Nightly link check", project_id: "site", project_name: "orca-site", paused: false, next_run: NOW + 9 * 3600, last_run: { outcome: "missed", at: ago(5 * 3600) } },
    { id: "auto-2", title: "Morning brief", paused: false, next_run: NOW + 16 * 3600, last_run: { outcome: "submitted", at: ago(8 * 3600) } },
    { id: "auto-3", title: "Weekly dependency update", project_id: "riwork", project_name: "riWork", paused: true, last_run: { outcome: "failed", at: ago(4 * 86400) } },
  ] },
  backends: { state: "read" },
  decision_sources: { state: "read", chats_read: 4, chats_complete: 4, chats_total: 4 },
  decisions: DECISIONS, decisions_total: DECISIONS.length,
  recent_activity: [
    { session: BUILDER, source: { event_seq: 4182 }, observed_at: ago(40), log_updated_at: ago(40), kind: "approval", text: "Asked to run cargo test -p riwork-engine", status: "waiting_for_approval" },
    { session: shell("shell-2", "nsh", "nsh", "codex", "text-shaping"), source: { event_seq: 977 }, observed_at: ago(3 * 60), log_updated_at: ago(3 * 60), kind: "edit", text: "Edited crates/text/src/shape.rs", status: "working" },
  ],
  recent_activity_total: 2,
  intelligence: { state: "ready", generated_at: ago(4 * 60), summary: "Three agents are busy in riWork and nsh; two wait on you, one for a test run and one for a sync choice.",
    highlights: [{ text: "riWork's dashboard branch is close to review", source_event_ids: [1, 2, 3] }, { text: "nsh text shaping is still failing two tests", source_event_ids: [4, 5] }] },
};

const BACKENDS: NonNullable<DashboardDto["backendLines"]> = [
  { key: "app", title: "Somtus 0.1.0", detail: "Current · built today", tone: "current" },
  { key: "chat-host", title: "Chat host", detail: "Running · 4 chats", tone: "current" },
  { key: "claude", title: "Claude Code 2.4.1", detail: "Current", tone: "current" },
  { key: "codex", title: "Codex 0.71.0", detail: "0.73.2 is available · somtus update", tone: "stale" },
];

const APPROVAL: Review = { reviewId: "review-1", chatId: "chat-builder", requestId: "request-1", title: "Run tests", kind: "approval", status: "pending",
  approval: { requestId: "request-1", kind: "command", heading: "Run a command?", title: "cargo test -p riwork-engine --offline", detail: "In ~/orca/projects/riWork · panel/dashboard",
    choices: [{ decision: "accept", label: "Allow" }, { decision: "accept_for_session", label: "Allow for this chat" }, { decision: "decline", label: "Deny" }] } };
const QUESTION: Review = { reviewId: "review-2", chatId: "chat-planner", requestId: "request-2", title: "Sync plan", kind: "question", status: "pending",
  question: { requestId: "request-2", prompts: [{ header: "Sync", question: "Where should the phone keep its copy of the chats?", multiSelect: false,
    options: [{ label: "SQLite", description: "On the device, synced over the LAN" }, { label: "iCloud", description: "CloudKit private database" }] }] } };

function Fixture() {
  const dto = signal<DashboardDto | null>({ schema: 3, now: NOW, readAt: NOW, error: null, wire: WIRE, backendLines: BACKENDS });
  const filter = signal<Filter>("active"), sort = signal<Sort>("priority"), query = signal("");
  const api: DecisionApi = {
    reviewDecision: async (chatId) => chatId === "chat-builder" ? APPROVAL : QUESTION,
    respondDecision: async (reviewId) => ({ ...(reviewId === "review-1" ? APPROVAL : QUESTION), status: "sent" }),
  };
  return <View style={{ flex: 1 }}>
    <DashboardView dto={dto} theme={() => DEFAULT_THEME} filter={filter} sort={sort} query={query} onFilter={(next) => filter.set(next)} onSort={(next) => sort.set(next)}
      onSearch={() => {}} onOpen={() => {}} onPanel={() => {}} onGlobalChat={() => {}} onRefresh={() => {}}
      plan={() => null} starting={() => false} decisions={api} />
  </View>;
}
mount(Fixture);
