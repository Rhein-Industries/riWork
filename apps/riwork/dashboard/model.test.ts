import { expect, test } from "nsh/test";
import {
  attentionCopy, automationDetail, chatCopy, emptyTable, filterCounts, freshness, intelligenceSection,
  layoutFor, matches, projectReaderName, quietAttention, routeTarget, sessionsLine, summary, tasksWords,
  visibleProjects, type ProjectRow, type Wire,
} from "./model";

function project(id: string, name: string, extra: Partial<ProjectRow> = {}): ProjectRow {
  return {
    id, name, agents: { working: 0, waiting: 0, done: 0, unknown: 0, exited: 0, subagents: 0 },
    live_shells: 0, live_chats: 0, tasks: { todo: 0, in_progress: 0, done: 0 }, worktrees: 0, attention: 0, ...extra,
  };
}

function wire(extra: Partial<Wire> = {}): Wire {
  return {
    v: 1, generated_at: 1000, chats_known: true,
    totals: { projects: 2, active_projects: 1, live_shells: 1, live_chats: 0, agents: { working: 0, waiting: 0, done: 0, unknown: 0, exited: 0, subagents: 0 }, tasks: { todo: 0, in_progress: 0, done: 0 }, attention: 0 },
    global: { kind: "none", exited_terminal: false }, automations: { state: "read", total: 0, paused: 0, items: [] }, ...extra,
  };
}

const busy = project("b", "Beta", { live_shells: 2, agents: { working: 1, waiting: 0, done: 0, unknown: 0, exited: 0, subagents: 0 }, last_activity: 900 });
const waiting = project("a", "alpha", { attention: 1, tasks: { todo: 2, in_progress: 1, done: 3 }, last_activity: 950 });
const quiet = project("c", "Gamma", { folder: "Clients" });

test("P5-DASH-01 a search needs every word somewhere and ignores case", () => {
  expect(matches("ALP repo", ["alpha", "", "~/repos/alpha"])).toBe(true);
  expect(matches("alpha beta", ["alpha", "", ""])).toBe(false);
  expect(matches("  ", ["x"])).toBe(true);
  expect(visibleProjects([busy, waiting, quiet], "clients", "all", "priority").map((p) => p.id)).toEqual(["c"]);
  expect(visibleProjects([busy, waiting, quiet], "repos", "all", "priority", { a: "~/repos/alpha" }).map((p) => p.id)).toEqual(["a"]);
});

test("P5-DASH-02 each filter lists what it names and the counts follow the search", () => {
  const all = [busy, waiting, quiet];
  expect(filterCounts(all, "")).toEqual([["active", 2], ["needs_you", 1], ["working", 1], ["in_progress", 1], ["all", 3]]);
  expect(filterCounts(all, "gam")).toEqual([["active", 0], ["needs_you", 0], ["working", 0], ["in_progress", 0], ["all", 1]]);
  expect(visibleProjects(all, "", "active", "priority").map((p) => p.id)).toEqual(["b", "a"]);
  expect(visibleProjects(all, "", "all", "name").map((p) => p.id)).toEqual(["a", "b", "c"]);
  expect(visibleProjects(all, "", "all", "recent").map((p) => p.id)).toEqual(["a", "b", "c"]);
  expect(visibleProjects(all, "", "all", "tasks").map((p) => p.id)).toEqual(["a", "b", "c"]);
  expect(visibleProjects(all, "", "all", "sessions").map((p) => p.id)).toEqual(["b", "a", "c"]);
});

test("P5-DASH-03 an empty table says why and leads to what is hidden", () => {
  expect(emptyTable([quiet], "", "needs_you").text).toBe("No project needs you right now.");
  expect(emptyTable([quiet], "zzz", "all").text).toBe("No project matches “zzz”.");
  const hidden = emptyTable([quiet], "gam", "active");
  expect(hidden.text).toBe("No project in Active matches “gam”; 1 project does in All.");
  expect(hidden.showAll).toBe("Show all 1 project");
});

test("P5-DASH-04 a project row reads out what it shows and says what is unknown", () => {
  expect(sessionsLine(busy, true)).toBe("2 live · 2 terminals · 0 chats");
  expect(sessionsLine(busy, false)).toBe("2 terminals · chats not read yet");
  expect(sessionsLine(quiet, true)).toBe("No live sessions");
  expect(tasksWords(waiting.tasks)).toEqual(["3/6 done", "1 in progress · 2 to do"]);
  expect(projectReaderName(waiting, true, 1000)).toBe(
    "Project alpha, 1 needs you, No live sessions, tasks 3/6 done, 1 in progress · 2 to do, last activity 50s ago; opens the project");
});

test("P5-DASH-05 the toolbar says how old the figures are and why they may be", () => {
  expect(freshness(100, null, null)).toEqual({ text: "Reading sessions…", warn: false });
  expect(freshness(100, null, "tmux is required")).toEqual({ text: "Sessions could not be read: tmux is required", warn: true });
  expect(freshness(100, 95, null)).toEqual({ text: "Live", warn: false });
  expect(freshness(200, 100, null)).toEqual({ text: "Last read 1m ago", warn: true });
  expect(freshness(200, 100, "broken").text).toBe("Showing the read from 1m ago · broken");
});

test("P5-DASH-06 the dashboard is laid out by the window's own width", () => {
  expect(layoutFor(800).arrangement).toBe("compact");
  expect(layoutFor(1100).arrangement).toBe("normal");
  expect(layoutFor(1900).arrangement).toBe("wide");
  // Native's 13/11 interface scale needs proportionally more room.
  expect(layoutFor(1100, 13 / 11).arrangement).toBe("compact");
  expect(layoutFor(1440, 13 / 11).arrangement).toBe("normal");
});

test("P5-DASH-07 attention, automation and empty lists say only what was read", () => {
  expect(attentionCopy({ kind: "waiting", target: { type: "shell", id: "s" }, session: { target: { type: "shell", id: "s" }, agent: "Codex", is_chat: false, project_name: "alpha" }, project_name: "alpha", at: 940 }, 1000))
    .toEqual(["Codex waits for you", "alpha · since 1m ago"]);
  expect(attentionCopy({ kind: "automation", target: { type: "automations" }, title: "Nightly", outcome: "missed", message: "host was asleep", at: 1000 }, 1000))
    .toEqual(["Automation “Nightly” missed its run", "App · host was asleep · just now"]);
  expect(automationDetail({ id: "n", title: "Nightly", paused: false, next_run: 1600, last_run: { outcome: "submitted", at: 400 } }, 1000))
    .toBe("App · next in 10m · last run sent 10m ago");
  expect(quietAttention(wire({ chats_known: false, automations: { state: "not_read" } })))
    .toBe("Nothing waits for you: no agent asks a question. Chats are still being read. Automations are still being read.");
  expect(summary(null)).toBe("Reading sessions…");
  expect(summary(wire())).toBe("All quiet across 2 projects");
});

test("P5-DASH-08 chat controls say whether a click opens or starts, and a terminal is itself", () => {
  const plan = { mode: "automatic" as const, automatic: ["muse:muse-wonder-full-1.0", "codex:gpt-6.1-sol"] };
  expect(chatCopy({ kind: "none", exited_terminal: false }, plan, false)).toEqual({
    action: "Start global chat", status: "No global orchestrator yet", starts: true, enabled: true,
    note: "Starts a Muse chat on muse-wonder-full-1.0 that asks before commands and edits, or a Codex chat on gpt-6.1-sol if Muse cannot start.",
  });
  expect(chatCopy({ kind: "none", exited_terminal: true }, { mode: "terminal", automatic: [] }, false).action).toBe("Start global orchestrator");
  expect(chatCopy({ kind: "terminal", shell_id: "t", activity: "working" }, plan, false).status).toBe("Runs in a terminal · working");
  expect(chatCopy({ kind: "chat", chat_id: "c", provider: "claude", chat_state: "waiting" }, plan, false).status).toBe("Claude · Waiting");
  expect(chatCopy({ kind: "unknown" }, plan, true)).toEqual({ action: "Opening global chat…", status: "Finding the global orchestrator", note: undefined, starts: false, enabled: false });
});

test("P5-DASH-09 rows go to their own project and the global orchestrator stays global", () => {
  const read = wire({ global: { kind: "terminal", shell_id: "g" }, projects: [waiting], tasks_in_progress: [{ id: "t", title: "T", project_id: "a", project_name: "alpha", updated_at: 1 }] });
  expect(routeTarget({ type: "shell", id: "g" }, read)).toEqual({ kind: "chat" });
  expect(routeTarget({ type: "shell", id: "s" }, read, { s: "a" })).toEqual({ kind: "project", projectId: "a", step: { kind: "showShell", shellId: "s" } });
  expect(routeTarget({ type: "shell", id: "gone" }, read)).toEqual({ kind: "notice", text: "That session has ended." });
  expect(routeTarget({ type: "chat", id: "c", orchestrator: "global" }, read)).toEqual({ kind: "chat" });
  expect(routeTarget({ type: "chat", id: "c" }, read)).toEqual({ kind: "notice", text: "This chat belongs to no project; open it with riwork chat." });
  expect(routeTarget({ type: "project", id: "zz" }, read)).toEqual({ kind: "notice", text: "That project is gone." });
  expect(routeTarget({ type: "task", id: "t" }, read)).toEqual({ kind: "project", projectId: "a", step: { kind: "task", taskId: "t" } });
});

test("P5-DASH-10 the Haiku overview shows only a saved summary and hides when off", () => {
  expect(intelligenceSection(undefined, 0)).toBe(null);
  expect(intelligenceSection({ state: "disabled" }, 0)).toBe(null);
  expect(intelligenceSection({ state: "no_evidence" }, 0)).toBe(null);
  expect(intelligenceSection({ state: "ready", summary: "Two agents finished.", generated_at: 940, highlights: [{ text: "alpha shipped", source_event_ids: ["e1", "e2"] }] }, 1000))
    .toEqual({ summary: "Two agents finished.", highlights: ["alpha shipped · 2 source events"], status: "From observed events · 1m ago", error: undefined });
  expect(intelligenceSection({ state: "not_configured", error: "No API key saved" }, 0)?.error).toBe("No API key saved");
  expect(intelligenceSection({ state: "ready", stale: true }, 0)?.status).toBe("Saved summary · newer activity is available");
});
