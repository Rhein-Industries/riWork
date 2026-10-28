import { expect, open, test, type App } from "nsh/test";
import type { AccountsDto, SettingsDto, StoreState } from "riwork/native";
import type { WorkspaceSnapshot } from "../model";

const options = (key: string) => ({ platform: "macos" as const, width: 1400, height: 1000,
  host: { params: { key }, handlers: { "app.fixture.state": null, "app.fixture.settingsState": null, "system.fonts": { families: ["SF Mono", "Menlo"] } } } });
const latest = (app: App) => app.host.posts().filter((post) => post.name === "app.fixture.state").slice(-1)[0].data as WorkspaceSnapshot;
async function inspect(app: App) {
  const count = app.host.posts().filter((post) => post.name === "app.fixture.settingsState").length;
  await app.host.send("app.fixture.inspect", {});
  await app.waitUntil(() => app.host.posts().filter((post) => post.name === "app.fixture.settingsState").length > count);
  return app.host.posts().filter((post) => post.name === "app.fixture.settingsState").slice(-1)[0].data as { store: StoreState; settings: SettingsDto; accounts: AccountsDto };
}

test("P6a Settings keyboard and host routes reuse panels and status preferences update the workspace", async () => {
  const app = await open("./SettingsRouting.fixture.tsx", options("routing"));
  try {
    await app.waitFor("id:workspace-dock");
    await app.keyboard.press("mod+,");
    await app.waitFor("id:settings-panel");
    await app.waitFor("id:remote-settings");
    await app.waitFor("id:remote-add");
    await app.waitFor("id:cua-setup");
    expect(app.count("id:settings-section-08 >> id:remote-settings")).toBe(1);
    await app.host.send("route", { step: { kind: "panel", panel: "settings" } });
    await app.waitFor("id:remote-pair");
    expect(latest(app).panes.flatMap((pane) => pane.tabs).filter((tab) => tab.id === "panel:settings").length).toBe(1);
    await app.press("id:settings-status-enabled");
    await app.waitUntil(() => !app.shows("id:status-bar"));
    expect((await inspect(app)).settings.statusBar.enabled).toBe(false);
    await app.press("id:settings-status-enabled");
    await app.waitFor("id:status-bar");
    await app.host.send("route", { step: { kind: "apiKeySettings" } });
    await app.waitFor("id:claude-api-key-input");
    await app.host.send("route", { step: { kind: "projectMemo" } });
    await app.waitFor("id:project-settings-panel");
    await app.waitFor("id:project-memo-input");
    expect(latest(app).panes.flatMap((pane) => pane.tabs).filter((tab) => tab.id === "panel:project_settings").length).toBe(1);
  } finally { app.dispose(); }
});

test("P6B Settings singleton route mounts Remote in AppSettings and ordinary Settings reuses it", async () => {
  const app = await open("./SettingsRouting.fixture.tsx", options("remote-singleton-route"));
  try {
    await app.waitFor("id:workspace-dock");
    await app.host.send("route", { step: { kind: "panel", panel: "settings" } });
    await app.waitFor("id:settings-panel");
    await app.waitFor("id:remote-settings");
    await app.waitFor("id:cua-setup");
    await app.keyboard.press("mod+,");
    await app.waitFor("id:remote-add");
    expect(latest(app).panes.flatMap((pane) => pane.tabs).filter((tab) => tab.id === "panel:settings").length).toBe(1);
    expect(app.count("id:settings-section-08 >> id:remote-settings")).toBe(1);
    expect(app.count("id:cua-setup")).toBe(1);
    expect(app.shows("id:project-settings-panel")).toBe(false);
  } finally { app.dispose(); }
});

test("P6a status account picker writes project scope and keeps the app default", async () => {
  const app = await open("./SettingsRouting.fixture.tsx", options("account-scope"));
  try {
    await app.waitFor("id:status-item-codex-account");
    const before = await inspect(app);
    await app.press("id:status-item-codex-account");
    await app.waitFor("id:account-menu");
    await app.waitFor("id:account-menu-saved-fixture-saved");
    expect(app.enabled("id:account-menu-saved-fixture-unavailable")).toBe(false);
    await app.press("id:account-menu-saved-fixture-saved");
    await app.waitUntil(() => !app.shows("id:account-menu"));
    await app.waitUntil(() => app.texts().some((text) => text.includes("Default (project)") && text.includes("fixture@example.invalid")));
    let state = await inspect(app);
    const projectId = latest(app).projectId;
    expect(state.store.projects.find((project) => project.id === projectId)?.codexAccount).toEqual({ source: "saved", accountId: "fixture-saved" });
    expect(state.accounts.selectedId).toBe(before.accounts.selectedId);
    await app.press("id:status-item-codex-account");
    await app.press("id:account-menu-system");
    await app.waitUntil(() => !app.shows("id:account-menu"));
    state = await inspect(app);
    expect(state.store.projects.find((project) => project.id === projectId)?.codexAccount.source).toBe("system_default");
    await app.press("id:status-item-codex-account"); await app.press("id:account-menu-inherit");
    await app.waitUntil(() => !app.shows("id:account-menu"));
    state = await inspect(app);
    expect(state.store.projects.find((project) => project.id === projectId)?.codexAccount.source).toBe("inherit");
    expect(state.accounts.selectedId).toBe(before.accounts.selectedId);
  } finally { app.dispose(); }
});
