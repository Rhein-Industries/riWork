import { expect, open, test, type App } from "nsh/test";
interface State { pending: boolean; confirmation: string | null; prompt: string | null; calls: string[]; notices: string[]; actions: string[]; copied: number; selected: string | null; name: string; relay: string; routes: string; subscribed: number; closed: number; unfolded: boolean; elsewhere: string }
const options = { platform: "macos" as const, width: 1100, height: 1100, host: { handlers: { "app.fixture.remote.state": null, "app.fixture.remote.layout": null } } };
async function inspect(app: App): Promise<State> {
  const count = app.host.posts().length;
  await app.host.send("app.fixture.remote.inspect", {});
  await app.waitUntil(() => app.host.posts().length > count);
  return app.host.posts().filter((post) => post.name === "app.fixture.remote.state").slice(-1)[0].data as State;
}
interface Layout { settings: { y: number; height: number }; projects: { y: number } }
async function layout(app: App): Promise<Layout> {
  const count = app.host.posts().length;
  await app.host.send("app.fixture.remote.measure", {});
  await app.waitUntil(() => app.host.posts().slice(count).some((post) => post.name === "app.fixture.remote.layout"));
  return app.host.posts().filter((post) => post.name === "app.fixture.remote.layout").slice(-1)[0].data as Layout;
}
// Every operation below stays inside Remote.fixture's fake API. The fixture has
// no native Remote import, CLI, credential, clipboard, filesystem or socket seam.
test("P6B-REMOTE-UI-01 host removal requires two activations and one pending request", async () => {
  const app = await open("./Remote.fixture.tsx", options);
  try {
    await app.waitFor("id:remote-host-disposable-mac");
    await app.press("id:remote-remove-disposable-mac");
    expect(app.texts()).toContain("Confirm remove");
    expect((await inspect(app)).calls.length).toBe(0);
    await app.host.send("app.fixture.remote.hold", {});
    await app.press("id:remote-remove-disposable-mac");
    expect((await inspect(app)).pending).toBe(true);
    expect(app.enabled("id:remote-remove-disposable-mac")).toBe(false);
    await app.keyboard.press("enter");
    expect((await inspect(app)).calls).toEqual(["remove:disposable-mac"]);
    await app.host.send("app.fixture.remote.release", {});
    await app.waitFor("text:No other Mac is paired yet.");
    await app.waitFor("text:This Mac is no longer paired. Add it again in Settings.");
  } finally { app.dispose(); }
});

test("P6B-REMOTE-UI-02 opening Add resets Remove and validates the masked link without sending it", async () => {
  const app = await open("./Remote.fixture.tsx", options);
  try {
    await app.waitFor("id:remote-host-disposable-mac");
    await app.press("id:remote-remove-disposable-mac");
    await app.press("id:remote-add");
    await app.waitFor("id:remote-pairing-link");
    expect((await inspect(app)).unfolded).toBe(true);
    expect((await inspect(app)).confirmation).toBe(null);
    await app.press("id:remote-submit");
    expect(app.texts()).toContain("Paste the pairing link from the other Mac.");
    await app.type("id:remote-pairing-link", "not-a-disposable-link");
    await app.submit("id:remote-pairing-link");
    expect(app.texts()).toContain("That is not a RiWork pairing link.");
    expect((await inspect(app)).calls.length).toBe(0);
    expect(app.texts().some((text) => text.includes("not-a-disposable-link"))).toBe(false);
    await app.keyboard.press("escape"); await app.waitUntil(() => !app.shows("id:remote-prompt"));
    expect((await inspect(app)).unfolded).toBe(false);
    await app.press("id:remote-add");
    expect(app.value("id:remote-pairing-link")).toBe("");
  } finally { app.dispose(); }
});

test("P6B-REMOTE-UI-03 pairing uses defaults, submits once, shows a mask and copies through fake clipboard", async () => {
  const app = await open("./Remote.fixture.tsx", options);
  try {
    await app.waitFor("id:remote-host-disposable-mac");
    await app.press("id:remote-pair");
    await app.waitUntil(() => app.value("id:remote-prompt-name") === "Remembered name");
    expect(app.value("id:remote-prompt-relay")).toBe("wss://invalid.example/v1/ws");
    expect(app.value("id:remote-prompt-routes")).toBe("/FAKE/DISPOSABLE/relay-routes.json");
    await app.host.send("app.fixture.remote.hold", {});
    await app.press("id:remote-submit");
    await app.keyboard.press("enter");
    expect((await inspect(app)).calls).toEqual(["pair"]);
    await app.host.send("app.fixture.remote.release", {});
    await app.waitFor("id:remote-pairing-mask");
    expect(app.texts().some((text) => text.includes("DISPOSABLE_FAKE_NOT_A_CREDENTIAL"))).toBe(false);
    await app.press("id:remote-copy-link");
    await app.waitFor("text:Copied");
    expect((await inspect(app)).copied).toBe(1);
    await app.press("id:remote-done");
    await app.waitUntil(() => !app.shows("id:remote-prompt"));
    expect((await inspect(app)).unfolded).toBe(false);
  } finally { app.dispose(); }
});

test("P6B-REMOTE-UI-04 stale remote projects remain selectable and local-only panels stay unavailable", async () => {
  const app = await open("./Remote.fixture.tsx", options);
  try {
    await app.waitFor("id:remote-project-remote:disposable-mac:disposable-project");
    await app.press("id:fixture-offline");
    expect(app.texts()).toContain("Remote fixture");
    expect(app.texts()).toContain("Not available for a project on Disposable Mac");
    await app.press("id:remote-project-remote:disposable-mac:disposable-project");
    expect((await inspect(app)).actions).toContain("select:remote:disposable-mac:disposable-project");
    await app.press("id:remote-shell-fixture-shell");
    expect((await inspect(app)).actions).toContain("shell:disposable-mac:fixture-shell");
    await app.press("id:remote-shell-fixture-chat");
    expect((await inspect(app)).notices).toContain("Project orchestrator runs as a chat on Disposable Mac; chats run on that Mac. Open it there, or in the iOS app.");
    await app.press("id:fixture-ended");
    await app.waitFor("id:remote-reconnect");
    await app.press("id:remote-reconnect");
    expect((await inspect(app)).actions).toContain("reattach-same-target");
  } finally { app.dispose(); }
});

test("P6B-REMOTE-UI-05 an unrelated layout delivery does not close/reopen the remote subscription", async () => {
  const app = await open("./Remote.fixture.tsx", options);
  try {
    await app.waitFor("id:remote-host-disposable-mac");
    const before = await inspect(app);
    for (let turn = 0; turn < 5; turn++) await app.host.send("app.fixture.remote.unrelated", {});
    const after = await inspect(app);
    expect(after.subscribed).toBe(before.subscribed);
    expect(after.closed).toBe(before.closed);
  } finally { app.dispose(); }
});

test("2CB-REMOTE-UI-06 a new remote project closes the prompt at once, is made in the background and never switches", async () => {
  const app = await open("./Remote.fixture.tsx", options);
  try {
    await app.waitFor("id:remote-new-project-disposable-mac");
    await app.press("id:remote-new-project-disposable-mac");
    await app.waitFor("id:remote-prompt-name");
    expect(app.texts()).toContain("The project is made in the host's default projects folder.");
    expect(app.texts()).toContain("Create  ↵");
    await app.host.send("app.fixture.remote.hold", {});
    await app.type("id:remote-prompt-name", "Fixture app");
    await app.submit("id:remote-prompt-name");
    await app.waitUntil(() => !app.shows("id:remote-prompt"));
    const before = await inspect(app);
    expect(before.calls).toEqual(["project:Fixture app"]);
    expect(before.unfolded).toBe(false);
    expect(before.notices).toEqual([]);
    await app.host.send("app.fixture.remote.release", {});
    for (let turn = 0; turn < 40 && !(await inspect(app)).notices.includes("Created Fixture app"); turn++) await app.wait(25);
    expect((await inspect(app)).notices).toEqual(["Created Fixture app"]);
    expect((await inspect(app)).actions.some((action) => action.startsWith("select:"))).toBe(false);
  } finally { app.dispose(); }
});

test("2CB-REMOTE-UI-07 unavailable panels say what and why, centered, as GPUI", async () => {
  const app = await open("./Remote.fixture.tsx", options);
  try {
    await app.waitFor("id:remote-unavailable-files");
    expect(app.texts()).toContain("Not available for a project on Disposable Mac");
    expect(app.texts()).toContain("Files works with this Mac's own projects. Select one of them to use it.");
  } finally { app.dispose(); }
});

test("2CB-REMOTE-UI-08 a form unfolds where it was opened, in the flow, and leaves the rest of the window usable", async () => {
  const app = await open("./Remote.fixture.tsx", options);
  try {
    await app.waitFor("id:remote-host-disposable-mac");
    const before = await layout(app);
    await app.press("id:remote-add");
    await app.waitFor("id:remote-pairing-link");
    // Under Add host, inside the Remote section; nothing is laid over the window:
    // what follows the section moves down by exactly what the section grew.
    expect(app.count("id:remote-settings >> id:remote-prompt")).toBe(1);
    expect(app.count("id:remote-projects >> id:remote-prompt")).toBe(0);
    const after = await layout(app);
    expect(after.settings.height).toBeGreaterThan(before.settings.height);
    expect(Math.abs((after.projects.y - before.projects.y) - (after.settings.height - before.settings.height))).toBeLessThan(0.5);
    expect(after.projects.y).toBeGreaterThanOrEqual(after.settings.y + after.settings.height);
    // Typing and clicking elsewhere work while it is open, and it stays open;
    // Escape typed elsewhere is not the form's.
    await app.type("id:fixture-elsewhere", "still typing");
    expect((await inspect(app)).elsewhere).toBe("still typing");
    await app.press("id:remote-project-remote:disposable-mac:disposable-project");
    expect((await inspect(app)).actions).toContain("select:remote:disposable-mac:disposable-project");
    await app.press("id:fixture-worktrees");
    await app.waitFor("id:remote-worktrees-panel");
    await app.keyboard.press("escape");
    expect(app.shows("id:remote-pairing-link")).toBe(true);
    // Another opener switches to its form; its own opener folds it.
    await app.press("id:remote-pair");
    await app.waitFor("id:remote-prompt-relay");
    expect(app.shows("id:remote-pairing-link")).toBe(false);
    await app.press("id:remote-pair");
    await app.waitUntil(() => !app.shows("id:remote-prompt"));
    expect((await inspect(app)).unfolded).toBe(false);
    // New project unfolds in its folder. Snapshots redraw the folder without
    // remaking the form: the keyboard stays on Cancel, which Return presses (a
    // remade form would have put it back in the field, where Return creates).
    await app.press("id:remote-new-project-disposable-mac");
    await app.waitFor("id:remote-prompt-name");
    expect(app.count("id:remote-folder-disposable-mac >> id:remote-prompt")).toBe(1);
    expect(app.count("id:remote-settings >> id:remote-prompt")).toBe(0);
    await app.type("id:remote-prompt-name", "Half typed");
    await app.keyboard.press("tab");
    for (let turn = 0; turn < 3; turn++) await app.host.send("app.fixture.remote.publish", {});
    expect(app.value("id:remote-prompt-name")).toBe("Half typed");
    await app.keyboard.press("enter");
    await app.waitUntil(() => !app.shows("id:remote-prompt"));
    expect((await inspect(app)).calls).toEqual([]);
    // Escape in the field folds it, however often snapshots came meanwhile.
    await app.press("id:remote-new-project-disposable-mac");
    await app.type("id:remote-prompt-name", "Again");
    for (let turn = 0; turn < 3; turn++) await app.host.send("app.fixture.remote.publish", {});
    await app.keyboard.press("escape");
    await app.waitUntil(() => !app.shows("id:remote-prompt"));
    expect((await inspect(app)).calls).toEqual([]);
    // Remove is another action: it folds an open form.
    await app.press("id:remote-add");
    await app.waitFor("id:remote-pairing-link");
    await app.press("id:remote-remove-disposable-mac");
    expect(app.shows("id:remote-prompt")).toBe(false);
    expect(app.texts()).toContain("Confirm remove");
  } finally { app.dispose(); }
});
