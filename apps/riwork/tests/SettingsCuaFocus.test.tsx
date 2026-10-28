import { expect, open, test, type App } from "nsh/test";

async function fixture(ready: boolean) {
  const app = await open("./SettingsCuaFocus.fixture.tsx", { platform: "macos", width: 1000, height: 900,
    host: { params: { ready }, handlers: { "app.fixture.state": null, "app.fixture.cua-action": null } } });
  await app.waitFor("id:settings-theme-grid");
  await app.waitFor("id:cua-primary");
  return app;
}
async function show(app: App) { await app.host.send("app.fixture.show", true); await app.wait(50); }
const actions = (app: App) => app.host.posts().filter((post) => post.name === "app.fixture.cua-action").map((post) => post.data);

// Button/theme focus commands are asserted by the Rust AppView fixture: NSH
// 0.3's JS headless driver applies programmatic focus only to text inputs.
for (const ready of [false, true]) test(`P6C Focus preserves an in-panel input on show with Cua ready=${ready}`, async () => {
  const app = await fixture(ready);
  try {
    await app.type("id:claude-api-key-input", "draft");
    await show(app);
    await app.keyboard.press("x");
    expect(app.value("id:claude-api-key-input")).toBe("draftx");
    expect(actions(app)).toEqual([]);
  } finally { app.dispose(); }
});
