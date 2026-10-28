import { expect, open, test, type App } from "nsh/test";
interface State { pending: boolean; confirmation: string | null; prompt: string | null; calls: string[]; notices: string[]; actions: string[]; copied: number; selected: string | null; name: string; relay: string; routes: string; subscribed: number; closed: number; frozen: boolean }
const options = { platform: "macos" as const, width: 1100, height: 1100, host: { handlers: { "app.fixture.remote.state": null } } };
async function inspect(app: App): Promise<State> {
  const count = app.host.posts().length;
  await app.host.send("app.fixture.remote.inspect", {});
  await app.waitUntil(() => app.host.posts().length > count);
  return app.host.posts().filter((post) => post.name === "app.fixture.remote.state").slice(-1)[0].data as State;
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
    expect((await inspect(app)).frozen).toBe(true);
    expect((await inspect(app)).confirmation).toBe(null);
    await app.press("id:remote-submit");
    expect(app.texts()).toContain("Paste the pairing link from the other Mac.");
    await app.type("id:remote-pairing-link", "not-a-disposable-link");
    await app.submit("id:remote-pairing-link");
    expect(app.texts()).toContain("That is not a RiWork pairing link.");
    expect((await inspect(app)).calls.length).toBe(0);
    expect(app.texts().some((text) => text.includes("not-a-disposable-link"))).toBe(false);
    await app.keyboard.press("escape"); await app.waitUntil(() => !app.shows("id:remote-prompt"));
    expect((await inspect(app)).frozen).toBe(false);
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
    expect((await inspect(app)).frozen).toBe(false);
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
