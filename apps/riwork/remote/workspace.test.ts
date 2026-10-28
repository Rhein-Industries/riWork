import { expect, test } from "nsh/test";
import { workspaceRemote, type WorkspaceRemoteApi } from "./workspace";
import { EMPTY_REMOTE, type RemoteShell } from "./model";
import type { WorkspaceSnapshot } from "../model";

const shell: RemoteShell = { id: "shell", projectId: "project", worktreeId: null, alive: true, chat: false, orchestrator: false, harness: null, cwd: "/other/mac", display: "Shell" };
function fixture(overrides: Partial<WorkspaceRemoteApi> = {}) {
  let project = "remote:host:project", generation = 1, alive = true;
  let snapshot: WorkspaceSnapshot = { activePane: 7, panes: [{ id: 7, main: false, locked: false, active: true, tabs: [] }] };
  const calls: unknown[] = [], notices: string[] = [];
  const api: WorkspaceRemoteApi = {
    createShell: async (request) => { calls.push(request); return shell; },
    clickWorktree: async () => ({ action: "unknown", shellId: null, shell: null }),
    orchestrators: async () => [], ...overrides,
  };
  const actions = workspaceRemote({ api,
    session: { openRemoteShell: (host, id) => { calls.push(["open", host, id]); return {}; }, selectPane: (id) => { calls.push(["pane", id]); return {}; }, selectPanel: (id) => { calls.push(["tab", id]); return {}; } },
    projectId: () => project, generation: () => generation, alive: () => alive, snapshot: () => snapshot,
    data: () => EMPTY_REMOTE, selectedWorktree: () => "tree", install: () => {}, focus: (id) => calls.push(["focus", id]), notice: (text) => notices.push(text),
  });
  return { actions, calls, notices, switch: (value: string) => { project = value; generation++; }, close: () => { alive = false; }, snapshot: (value: WorkspaceSnapshot) => { snapshot = value; } };
}
test("P6B-REMOTE-WORKSPACE-01 create dispatches once, uses the captured pane, and never a local API", async () => {
  let finish!: (shell: RemoteShell) => void;
  let count = 0;
  const f = fixture({ createShell: () => { count++; return new Promise((resolve) => { finish = resolve; }); } });
  const first = f.actions.create();
  await f.actions.create();
  expect(count).toBe(1);
  f.snapshot({ activePane: 8, panes: [{ id: 7, main: false, locked: false, active: false, tabs: [] }, { id: 8, main: false, locked: false, active: true, tabs: [] }] });
  finish(shell); await first;
  expect(f.calls).toEqual([["pane", 7], ["open", "host", "shell"], ["focus", "remote:host:shell"]]);
});
test("P6B-REMOTE-WORKSPACE-02 a result settles but cannot open after switch away and back or close", async () => {
  let finish!: (shell: RemoteShell) => void;
  const f = fixture({ createShell: () => new Promise((resolve) => { finish = resolve; }) });
  const first = f.actions.create();
  f.switch("local"); f.switch("remote:host:project"); finish(shell); await first;
  expect(f.calls).toEqual([]);
  const second = f.actions.create(); f.close(); finish(shell); await second;
  expect(f.calls).toEqual([]);
});
test("P6B-REMOTE-WORKSPACE-03 failure is never automatically retried and dead/chat shells are refused", async () => {
  let count = 0;
  const f = fixture({ createShell: async () => { count++; throw new Error("broken: outcome unknown"); } });
  let error = ""; try { await f.actions.create(); } catch (reason) { error = String(reason); }
  expect(error).toContain("outcome unknown"); expect(count).toBe(1);
  f.actions.open("host", { ...shell, alive: false }); f.actions.open("host", { ...shell, chat: true });
  expect(f.calls.length).toBe(0); expect(f.notices.length).toBe(2);
});
test("P6B-REMOTE-WORKSPACE-04 worktree selects the exact host tab before starting anything", async () => {
  const f = fixture({ clickWorktree: async () => ({ action: "tab", shellId: "shell", shell: null }) });
  await f.actions.worktree("tree");
  expect(f.calls).toEqual([["tab", "remote:host:shell"]]);
  f.switch("local"); await f.actions.create(); expect(f.calls.length).toBe(1);
});
