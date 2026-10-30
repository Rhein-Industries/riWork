import { expect, open, test, type App } from "nsh/test";

const options = { platform: "macos" as const, width: 900, height: 1400, host: { handlers: { "app.fixture.flows.state": null } } };
interface State { calls: string[]; opened: string[]; edited: string[]; listeners: number }
async function inspect(app: App): Promise<State> {
  const count = app.host.posts().length; await app.host.send("app.fixture.flows.inspect", {});
  await app.waitUntil(() => app.host.posts().length > count);
  return app.host.posts().filter((post) => post.name === "app.fixture.flows.state").slice(-1)[0].data as State;
}

test("FLOWS-UI-01 a flow runs in a chat, waits for approval and moves on", async () => {
  const app = await open("./Flows.fixture.tsx", options);
  try {
    await app.waitFor("id:flow:code-review");
    expect(app.texts()).toContain("2 flows");
    await app.waitFor("id:flow-diagram");
    await app.press("id:flow-run");
    await app.press("id:flow-chat:chat-a");
    await app.waitFor("id:flow-run:run-1");
    let state = await inspect(app);
    expect(state.calls).toContain("start:code-review:chat-a");
    expect(state.opened).toEqual(["chat-a"]);
    await app.host.send("app.fixture.flows.ask", {});
    await app.waitFor("id:flow-decision");
    expect(app.texts()).toContain("Evidence: Plan in PLAN.md");
    await app.type("id:flow-note", "Keep it small");
    await app.press("id:flow-approve");
    await app.waitUntil(() => !app.shows("id:flow-decision"));
    state = await inspect(app);
    expect(state.calls).toContain("approve:run-1:Keep it small");
    await app.press("id:flow-stop:run-1");
    expect((await inspect(app)).calls).toContain("stop:run-1");
  } finally { app.dispose(); }
});

test("FLOWS-UI-02 an invalid flow says why and cannot run; a new flow opens for editing", async () => {
  const app = await open("./Flows.fixture.tsx", options);
  try {
    await app.waitFor("id:flow:broken");
    await app.press("id:flow:broken");
    await app.waitFor("id:flow-invalid");
    expect(app.enabled("id:flow-run")).toBe(false);
    await app.press("id:flow-new");
    await app.type("id:flow-name", "Deploy");
    await app.press("id:flow-create-confirm");
    await app.waitFor("id:flow:deploy");
    const state = await inspect(app);
    expect(state.calls).toContain("create:Deploy");
    expect(state.edited).toEqual(["/fixture/.ontack/flows/deploy.json"]);
  } finally { app.dispose(); }
});

test("FLOWS-UI-03 the native module creates flows and its watch shows new ones, from here or elsewhere", async () => {
  const app = await open("./FlowsNative.fixture.tsx", { platform: "macos", width: 900, height: 1400,
    host: { params: { key: `flows-${Date.now()}` }, handlers: { "app.fixture.flows.state": null } } });
  try {
    await app.waitFor("id:flows-empty", { timeout: 15000 });
    await app.press("id:flow-empty-new");
    await app.type("id:flow-name", "Code review");
    await app.press("id:flow-create-confirm");
    await app.waitFor("id:flow:code-review", { timeout: 10000 });
    const count = app.host.posts().length; await app.host.send("app.fixture.flows.inspect", {});
    await app.waitUntil(() => app.host.posts().length > count);
    const state = app.host.posts().filter((post) => post.name === "app.fixture.flows.state").slice(-1)[0].data as { edited: string[] };
    expect(state.edited.length).toBe(1);
    expect(state.edited[0].endsWith("/.ontack/flows/code-review.json")).toBe(true);
    // The test runner has no diagram worker beside it: the panel says so instead of drawing.
    await app.waitUntil(() => app.shows("id:flow-diagram") || app.shows("id:flow-diagram-error"), { timeout: 20000 });
    await app.host.send("app.fixture.flows.external", {});
    await app.waitFor("id:flow:written-elsewhere", { timeout: 10000 });
    expect(app.texts()).toContain("2 flows");
  } finally { app.dispose(); }
});
