/** A disposable in-memory dashboard with two provider requests. The review calls
 * are this file's: no native read, chat host, provider or window is reached. */
import { View, host, mount, signal } from "nsh";
import type { DashboardDto } from "riwork/native";
import { DEFAULT_THEME } from "../appearance";
import { DashboardView } from "./Dashboard";
import type { DecisionApi, Review } from "./DecisionReview";
import type { DecisionRow, Filter, Sort, Target, Wire } from "./model";

const session = (id: string, title: string): DecisionRow["session"] => ({ target: { type: "chat", id }, agent: "claude", title, project_id: "fixture-project", project_name: "Fixture project", is_chat: true });
const DECISIONS: DecisionRow[] = [
  { id: "fixture-approval", chat_id: "chat-a", request_id: "request-a", session: session("chat-a", "Builder"), observed_at: 90, kind: "approval" },
  { id: "fixture-question", chat_id: "chat-b", request_id: "request-b", session: session("chat-b", "Planner"), observed_at: 95, kind: "question" },
];
const agents = { working: 0, waiting: 0, done: 0, unknown: 0, exited: 0, subagents: 0 }, tasks = { todo: 0, in_progress: 0, done: 0 };
function read(decisions: DecisionRow[], now: number): DashboardDto {
  const wire: Wire = { v: 1, generated_at: now, chats_known: true, global: { kind: "none", exited_terminal: false },
    totals: { projects: 1, active_projects: 1, live_shells: 0, agents, tasks, attention: decisions.length },
    projects: [{ id: "fixture-project", name: "Fixture project", agents, live_shells: 0, tasks, worktrees: 1, attention: 0 }], projects_total: 1,
    automations: { state: "read", total: 0, paused: 0, items: [] }, attention: [], working: [], finished: [],
    decision_sources: { state: "read", chats_read: 2, chats_complete: 2, chats_total: 2 }, decisions, decisions_total: decisions.length };
  return { schema: 3, now, readAt: now, error: null, wire, backendLines: [] };
}
const APPROVAL: Review = { reviewId: "review-a", chatId: "chat-a", requestId: "request-a", title: "Run tests", kind: "approval", status: "pending",
  approval: { requestId: "request-a", kind: "command", heading: "Run a command?", title: "cargo test --offline", detail: "In /FAKE/fixture-project",
    choices: [{ decision: "accept", label: "Allow" }, { decision: "decline", label: "Deny" }] } };
const QUESTION: Review = { reviewId: "review-b", chatId: "chat-b", requestId: "request-b", title: "Which way?", kind: "question", status: "pending",
  question: { requestId: "request-b", prompts: [{ question: "Which database?", options: [{ label: "SQLite", description: "Embedded" }, { label: "Postgres", description: "A server" }], multiSelect: false }] } };

function Fixture() {
  let now = 100, refreshed = 0, hold = false, resume: (() => void) | null = null;
  const dto = signal<DashboardDto | null>(read(DECISIONS, now));
  const filter = signal<Filter>("all"), sort = signal<Sort>("priority"), query = signal("");
  const calls = signal<string[]>([]), routes = signal<Target[]>([]);
  const wait = async () => { if (hold) await new Promise<void>((resolve) => { resume = resolve; }); };
  const api: DecisionApi = {
    reviewDecision: async (chatId) => { calls.set([...calls(), `read:${chatId}`]); return chatId === "chat-a" ? APPROVAL : QUESTION; },
    respondDecision: async (reviewId, decision, answers) => {
      calls.set([...calls(), `respond:${reviewId}:${decision ?? JSON.stringify(answers)}`]); await wait();
      return { ...(reviewId === "review-a" ? APPROVAL : QUESTION), status: "sent" };
    },
  };
  host.on("app.fixture.dashboard.inspect", () => host.post("app.fixture.dashboard.state", { calls: calls(), routes: routes(), refreshed, query: query(), filter: filter() }));
  // A new read: the same requests as new objects (or only `only`), the clock on.
  host.on("app.fixture.dashboard.read", (data) => {
    const only = (data as { only?: string[] } | null)?.only;
    now += 2; dto.set(read(DECISIONS.filter((row) => !only || only.includes(row.id)).map((row) => ({ ...row })), now));
  });
  host.on("app.fixture.dashboard.hold", () => { hold = true; });
  host.on("app.fixture.dashboard.release", () => { hold = false; resume?.(); resume = null; });
  return <View style={{ flex: 1 }}>
    <DashboardView dto={dto} theme={() => DEFAULT_THEME} filter={filter} sort={sort} query={query} onFilter={(next) => filter.set(next)} onSort={(next) => sort.set(next)}
      onSearch={() => {}} onOpen={(target) => routes.set([...routes(), target])} onPanel={() => {}} onGlobalChat={() => {}}
      onRefresh={() => { refreshed++; }} plan={() => null} starting={() => false} decisions={api} />
  </View>;
}
mount(Fixture);
