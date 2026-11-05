import { expect, open, test, type App } from "nsh/test";

interface State { selected: boolean; focusRequests: number; rows: number; editors: number[]; panel: { link: string } }
async function inspect(app: App): Promise<State> {
  await app.host.send("app.fixture.inspect");
  return app.host.posts().filter((post) => post.name === "app.fixture.state").slice(-1)[0].data as State;
}
async function fixture(extra: Record<string, unknown> = {}) {
  const app = await open("./Chat.fixture.tsx", { platform: "macos", width: 900, height: 780,
    host: { params: { recording: "turn", inactive: true, deferFocus: true, ...extra }, handlers: { "app.fixture.state": null } } });
  await app.waitFor("id:chat-input", { timeout: 20000 });
  for (let tries = 0; (await inspect(app)).panel.link !== "live" && tries < 200; tries++) await app.wait(25);
  expect((await inspect(app)).panel.link).toBe("live");
  return app;
}
async function sent(app: App) {
  await app.waitUntil(() => app.value("id:chat-input") === "", { timeout: 5000 });
  await app.waitUntil(() => app.texts().some((text) => text.startsWith("Worked")), { timeout: 20000 });
  expect(app.texts().filter((text) => text === "hello").length).toBe(1);
  expect((await inspect(app)).editors.length).toBe(1);
}

test("CHAT-SEND-FOCUS selecting the tab recovers a visible inactive composer", async () => {
  const app = await fixture({ ignoreFocus: true });
  try {
    await app.type("id:chat-input", "hello");
    await app.keyboard.press("Enter");
    await app.press("id:chat-send");
    await app.wait(100);
    expect(app.value("id:chat-input")).toBe("hello");
    expect((await inspect(app)).rows).toBe(0);
    // The reported away/back workaround changes the workspace's selected pane.
    await app.host.send("app.fixture.select", true);
    await app.keyboard.press("Enter");
    await sent(app);
  } finally { app.dispose(); }
});

test("CHAT-SEND-FOCUS input focus then Enter waits for pane selection", async () => {
  const app = await fixture();
  try {
    await app.type("id:chat-input", "hello");
    await app.keyboard.press("Enter");
    expect(app.value("id:chat-input")).toBe("hello");
    expect((await inspect(app)).rows).toBe(0);
    await app.host.send("app.fixture.focusDelivered");
    await sent(app);
    expect((await inspect(app)).selected).toBe(true);
    expect((await inspect(app)).focusRequests).toBe(1);
  } finally { app.dispose(); }
});

test("CHAT-SEND-FOCUS direct Send in another pane waits once and never duplicates", async () => {
  const app = await fixture({ ignoreFocus: true });
  try {
    await app.type("id:chat-input", "hello");
    // A saved draft in an unselected pane: no input focus accompanies Send.
    await app.host.send("app.fixture.select", false);
    await app.host.send("app.fixture.ignoreFocus", false);
    await app.press("id:chat-send");
    await app.press("id:chat-send");
    expect(app.value("id:chat-input")).toBe("hello");
    expect((await inspect(app)).rows).toBe(0);
    await app.host.send("app.fixture.focusDelivered");
    await sent(app);
    expect((await inspect(app)).focusRequests).toBe(1);
  } finally { app.dispose(); }
});

for (const guard of ["visible", "frozen"]) test(`CHAT-SEND-FOCUS ${guard} guard survives a late pane selection`, async () => {
  const app = await fixture();
  try {
    await app.type("id:chat-input", "hello");
    await app.keyboard.press("Enter");
    await app.host.send(`app.fixture.${guard}`, guard === "frozen");
    await app.host.send("app.fixture.focusDelivered");
    await app.wait(100);
    await app.press("id:chat-send");
    expect(app.value("id:chat-input")).toBe("hello");
    expect((await inspect(app)).rows).toBe(0);
    await app.host.send(`app.fixture.${guard}`, guard === "visible");
    await app.host.send("app.fixture.select", true);
    await app.press("id:chat-send");
    await sent(app);
  } finally { app.dispose(); }
});
