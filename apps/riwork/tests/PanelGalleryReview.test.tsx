import { expect, open, test, type App } from "nsh/test";
import type { StoreState } from "riwork/native";
const options = (width = 800, params: Record<string, unknown> = {}) => ({ platform: "macos" as const, width, height: 900, host: { params, handlers: { "app.fixture.state": null, "app.fixture.dashboard.state": null } } });
async function inspect(app: App) {
  const before = app.host.posts().length;
  await app.host.send("app.fixture.inspect", {});
  await app.waitUntil(() => app.host.posts().length > before);
  return app.host.posts().filter((post) => post.name === "app.fixture.state").slice(-1)[0].data as { state: StoreState; projectId: string };
}
test("panel review pins and view modes survive reopening and retain project actions", async () => {
  const params = { fixtureKey: `review-${Date.now()}` };
  const app = await open("./Panel.fixture.tsx", options(800, params));
  let peer: App | undefined;
  try {
    await app.waitFor("id:projects-panel", { timeout: 20000 });
    const initial = await inspect(app), id = initial.projectId;
    await app.waitFor(`id:pin-project:${id}`);
    const state = await inspect(app);
    await app.host.send("app.fixture.group-projects", {});
    await app.waitFor(`text:Review group ${id}`);
    await app.press(`id:pin-project:${id}`);
    await app.waitFor("text:Pinned");
    await app.waitUntil(() => app.enabled("id:project-view-grid"));
    await app.press("id:project-view-grid");
    await app.waitFor("id:projects-grid");
    await app.waitUntil(() => app.enabled("id:project-view-columns"));
    await app.press("id:project-view-columns");
    await app.waitFor("id:projects-columns");
    peer = await open("./Panel.fixture.tsx", options(800, params));
    await peer.waitFor("id:projects-columns", { timeout: 20000 });
    expect(peer.shows("text:Pinned")).toBe(true);
    await peer.wait(150);
    await peer.type("id:search:fixture", params.fixtureKey);
    await peer.waitFor("text:Matches");
    expect(peer.shows(`id:project-${id}`)).toBe(true);
    const other = state.state.projects.find((project) => project.id !== id && project.name.endsWith(params.fixtureKey))!;
    expect(peer.shows(`id:project-${other.id}`)).toBe(true);
    await peer.clear("id:search:fixture");
    await peer.press("id:folder-pinned");
    await peer.type("id:search:fixture", state.state.projects.find((project) => project.id === id)!.name);
    await peer.waitFor(`id:project-${id}`);
    expect((await inspect(peer)).state.projects.some((project) => project.id === id)).toBe(true);
    await peer.press(`id:project-${id}`);
    expect(peer.shows(`id:project-settings:${id}`)).toBe(true);
  } finally { peer?.dispose(); app.dispose(); }
});
test("panel review terminal cleanup freezes scope, cancels safely, and expires on project switch", async () => {
  const app = await open("./Sessions.fixture.tsx", options(800));
  try {
    await app.waitFor("id:shells-panel"); await app.wait(200);
    await app.host.send("app.fixture.seed", {});
    expect(app.shows("label:New terminal")).toBe(true);
    await app.press("id:nuke-shells");
    expect(app.texts()).toContain("End 3 project terminals?");
    await app.host.send("app.fixture.add-shell", {});
    expect(app.texts()).toContain("End 3 project terminals?");
    await app.keyboard.press("escape");
    expect(app.shows("id:end-terminals-confirm")).toBe(false);
    await app.press("id:nuke-shells");
    expect(app.texts()).toContain("End 4 project terminals?");
    await app.host.send("app.fixture.project-switch", {});
    expect(app.shows("id:end-terminals-confirm")).toBe(false);
    // The destructive confirmation action is deliberately never pressed.
  } finally { app.dispose(); }
});
test("panel review decisions and review drafts remain usable across 320 800 1440 widths", async () => {
  const app = await open("../dashboard/Decision.fixture.tsx", options(800));
  try {
    await app.waitFor("id:dashboard-decision-fixture-question");
    await app.press("id:dashboard-decision-fixture-question");
    await app.waitFor("id:dashboard-answer-0");
    await app.type("id:dashboard-answer-0", "MySQL");
    for (const width of [320, 800, 1440]) {
      await app.setViewport(width, 900);
      expect(app.value("id:dashboard-answer-0")).toBe("MySQL");
      expect(app.shows("id:dashboard-priority")).toBe(true);
      expect(app.shows("id:dashboard-decision-fixture-approval")).toBe(true);
      expect(app.shows("id:dashboard-projects-region")).toBe(true);
    }
    await app.press("id:dashboard-send-answer");
    await app.waitFor("text:Sent. The inbox updates when the provider confirms the answer.");
    const before = app.host.posts().length;
    await app.host.send("app.fixture.dashboard.inspect", {});
    await app.waitUntil(() => app.host.posts().length > before);
    const result = app.host.posts().filter((post) => post.name === "app.fixture.dashboard.state").slice(-1)[0].data as { calls: string[] };
    expect(result.calls.some((call) => call.startsWith("respond:review-b:") && call.includes("MySQL"))).toBe(true);
  } finally { app.dispose(); }
});
test("panel review add-in creation and opening remain separate from trust", async () => {
  const app = await open("./PanelReview.fixture.tsx", options(320, { panel: "addins" }));
  try {
    await app.waitFor("id:addin-open:overview:main");
    await app.press("id:addin-open:overview:main");
    expect(app.texts()).toContain("Not trusted yet: opening it asks first.");
    await app.press("id:addin-new");
    expect(app.shows("id:addin-new-name")).toBe(true);
    await app.press("id:addin-new");
    expect(app.shows("id:addin-new-name")).toBe(false);
  } finally { app.dispose(); }
});

test("panel review terminal cleanup closes only captured project terminals with fake failures", async () => {
  const app = await open("./Sessions.fixture.tsx", options(800));
  try {
    await app.waitFor("id:shells-panel"); await app.wait(200);
    await app.host.send("app.fixture.seed", {});
    await app.host.send("app.fixture.failure", {});
    await app.press("id:nuke-shells");
    await app.host.send("app.fixture.add-shell", {});
    await app.press("id:end-terminals-confirm-action");
    await app.waitUntil(() => app.texts().some((text) => text.startsWith("Ended 2 terminals.")));
    const before = app.host.posts().length;
    await app.host.send("app.fixture.inspect", {});
    await app.waitUntil(() => app.host.posts().length > before);
    const result = app.host.posts().filter((post) => post.name === "app.fixture.state").slice(-1)[0].data as { attempted: string[]; closed: string[]; shells: { id: string; kind: string; projectId: string }[]; chats: unknown[]; notice: string };
    expect(result.attempted).toEqual(["1b7e4c2a-9d3f-4a85-b0c6-8e2f5d1a7c40", "4d9a2f6e-1c8b-4e73-a5d0-9b3c7e2f1a86", "8c3f1e7b-5a2d-4b96-9e04-6d1b8a3c5f22"]);
    expect(result.closed.length).toBe(2);
    expect(result.shells.some((shell) => shell.kind === "orchestrator")).toBe(true);
    expect(result.shells.some((shell) => shell.projectId === "fixture-other")).toBe(true);
    expect(result.shells.some((shell) => shell.id === "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")).toBe(true);
    expect(result.chats.length).toBe(5);
    expect(result.notice.includes("Fixture exit refused")).toBe(true);
  } finally { app.dispose(); }
});
