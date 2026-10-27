import { expect, open, test, type App } from "nsh/test";
import type { StoreState } from "riwork/native";

interface State { state: StoreState; projectId: string; selectedTask: string | null; selectedWorktree: string | null; query: string; pending: boolean }
let sequence = 0;
const key = () => `${Date.now()}-${++sequence}`;
const options = (fixtureKey: string) => ({ platform: "macos" as const, width: 1000, height: 800,
  host: { params: { fixtureKey }, handlers: { "app.fixture.state": null } } });
async function inspect(app: App): Promise<State> {
  const before = app.host.posts().length;
  await app.host.send("app.fixture.inspect", {});
  await app.waitUntil(() => app.host.posts().length > before);
  return app.host.posts().filter((post) => post.name === "app.fixture.state").slice(-1)[0].data as State;
}
async function replace(app: App, selector: string, text: string) {
  await app.clear(selector);
  await app.type(selector, text);
}

test("panels share search, preserve exact selection, and switch only their own window", async () => {
  const fixtureKey = key();
  const app = await open("./tests/Panel.fixture.tsx", options(fixtureKey));
  const peer = await open("./tests/Panel.fixture.tsx", options(fixtureKey));
  try {
    await app.waitFor("id:projects-panel", { timeout: 15000 }); await peer.waitFor("id:projects-panel", { timeout: 15000 });
    await app.waitUntil(() => app.texts().includes(`Alpha-${fixtureKey}`));
    const initial = await inspect(app);
    const beta = initial.state.projects.find((project) => project.name === `Beta-${fixtureKey}`)!;
    await app.type("id:search:fixture", beta.name);
    expect(app.shows(`id:project-${beta.id}`)).toBe(true);
    expect(app.shows(`id:project-${initial.projectId}`)).toBe(false);
    await app.press("id:show-worktrees");
    expect((await inspect(app)).query).toBe(beta.name);
    expect(app.texts()).toContain("No matches");
    await app.press("id:show-projects");
    await app.press(`id:project-${beta.id}`);
    await app.waitUntil(() => !app.shows("id:clear-search:fixture"));
    expect((await inspect(app)).projectId).toBe(beta.id);
    expect((await inspect(peer)).projectId).toBe(initial.projectId);
    await app.press("id:show-worktrees");
    const next = await inspect(app);
    expect(next.state.worktrees.find((tree) => tree.id === next.selectedWorktree)?.projectId).toBe(beta.id);
  } finally { peer.dispose(); app.dispose(); }
});

test("task form, assignment and status changes reach a peer through Store subscriptions", async () => {
  const fixtureKey = key();
  const app = await open("./tests/Panel.fixture.tsx", options(fixtureKey));
  const peer = await open("./tests/Panel.fixture.tsx", options(fixtureKey));
  try {
    await app.waitFor("id:projects-panel", { timeout: 15000 }); await peer.waitFor("id:projects-panel", { timeout: 15000 });
    await app.press("id:show-tasks"); await peer.press("id:show-tasks");
    await app.press("id:new-task");
    await app.type("id:task-title", `Panel lifecycle ${fixtureKey}`);
    await app.type("id:task-details", "Saved by the real native store");
    await app.press("id:save-task");
    await app.waitFor("id:task-detail");
    const created = await inspect(app);
    const taskId = created.selectedTask!;
    await peer.waitFor(`id:task-${taskId}`);
    await app.press("id:assign-task");
    const primary = created.state.worktrees.find((tree) => tree.projectId === created.projectId && tree.isPrimary)!;
    await app.chooseMenuItem(primary.branch);
    await app.waitUntil(() => app.texts().some((text) => text === `todo · @ ${primary.branch}`));
    await app.press("id:task-status:in_progress");
    await peer.waitUntil(() => peer.texts().includes(`in_progress · @ ${primary.branch}`));
    // A peer can observe the Store push before this window's own call settles.
    await app.waitUntil(() => app.enabled("id:task-status:done"));
    await app.press("id:task-status:done");
    await peer.waitUntil(() => peer.texts().includes(`done · @ ${primary.branch}`));
    await app.waitUntil(() => app.enabled("id:assign-task"));
    await app.press("id:assign-task"); await app.chooseMenuItem("UNASSIGNED");
    await peer.waitUntil(() => peer.texts().includes("done · @ UNASSIGNED"));
    const observed = (await inspect(peer)).state.tasks.find((task) => task.id === taskId)!;
    expect(observed.worktreeId).toBe(null);
    expect(observed.details).toBe("Saved by the real native store");
  } finally { peer.dispose(); app.dispose(); }
});

test("folder forms and project creation persist and leave terminal attachment gated", async () => {
  const fixtureKey = key();
  const app = await open("./tests/Panel.fixture.tsx", options(fixtureKey));
  try {
    await app.waitFor("id:projects-panel", { timeout: 15000 });
    await app.press("id:new-folder");
    await app.type("id:folder-name", `Group ${fixtureKey}`); await app.press("id:save-folder");
    await app.waitUntil(() => !app.shows("id:panel-editor"));
    const folder = (await inspect(app)).state.projectFolders.find((folder) => folder.name === `Group ${fixtureKey}`)!;
    await app.press(`id:rename-folder:${folder.id}`);
    await replace(app, "id:folder-name", `Renamed ${fixtureKey}`); await app.press("id:save-folder");
    await app.waitUntil(() => app.texts().some((text) => text.includes(`Renamed ${fixtureKey}`)));
    await app.press(`id:remove-folder:${folder.id}`);
    await app.waitUntil(() => !app.shows(`id:folder-${folder.id}`));
    await app.press("id:new-project");
    await app.type("id:project-name", `Created-${fixtureKey}`);
    await app.waitUntil(() => app.texts().includes("New folder · no repositories"));
    await app.press("id:project-init-git");
    await app.press("id:create-project");
    await app.waitUntil(() => !app.shows("id:panel-editor"));
    const final = await inspect(app);
    const project = final.state.projects.find((project) => project.id === final.projectId)!;
    expect(project.name).toBe(`Created-${fixtureKey}`);
    expect(project.repositoryRoots).toEqual([]);
    expect(project.root.startsWith("/private/tmp/") || project.root.startsWith("/tmp/")).toBe(true);
    await app.host.send("app.fixture.intent", {});
    await app.press("id:show-shells");
    await app.waitFor("id:shell-aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    await app.press("id:shell-bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
    expect(app.texts()).toContain("Shell bbbbbbbb has exited");
    expect(app.shows("id:shell-intent:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb")).toBe(false);
    await app.press("id:shell-aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    await app.waitFor("id:shell-intent:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    expect(app.texts()).toContain("Shell selected. Terminal attachment is pending Phase 2.");
    expect(app.host.posts().some((post) => post.name.includes("terminal"))).toBe(false);
  } finally { app.dispose(); }
});
