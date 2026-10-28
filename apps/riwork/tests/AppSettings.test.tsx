import { expect, open, test, type App } from "nsh/test";
import type { AccountsDto, SettingsDto } from "riwork/native";

interface State { settings: SettingsDto; accounts: AccountsDto; keySaved: boolean | null; notice: string; fixtureChoice: string; importAttempts: number }
async function fixture(width = 1000, revealImmediately = false, extra: { keyboardFixture?: boolean; importScenario?: string } = {}) {
  const app = await open("./AppSettings.fixture.tsx", { platform: "macos", width, height: 900,
    clipboard: "keep clipboard", host: { params: { revealImmediately, ...extra }, handlers: { "app.fixture.state": null, "app.fixture.restored": null } } });
  await app.waitFor("id:settings-theme-grid");
  return app;
}
async function inspect(app: App): Promise<State> {
  const before = app.host.posts().length; await app.host.send("app.fixture.inspect");
  await app.waitUntil(() => app.host.posts().length > before);
  return app.host.posts().filter((post) => post.name === "app.fixture.state").slice(-1)[0].data as State;
}
async function waitState(app: App, condition: (state: State) => boolean) {
  let state = await inspect(app);
  for (let attempt = 0; attempt < 120 && !condition(state); attempt++) { await app.wait(50); state = await inspect(app); }
  expect(condition(state)).toBe(true);
}
async function restore(app: App) {
  try {
    const before = app.host.posts().filter((post) => post.name === "app.fixture.restored").length;
    await app.host.send("app.fixture.restore");
    await app.waitUntil(() => app.host.posts().filter((post) => post.name === "app.fixture.restored").length > before);
    const result = app.host.posts().filter((post) => post.name === "app.fixture.restored").slice(-1)[0].data as { error?: string };
    expect(result.error).toBe(undefined);
  } finally { app.dispose(); }
}

test("P6a global settings save appearance, agent choices, and only the selected delegation field", async () => {
  const app = await fixture();
  try {
    await app.press("id:theme-choice:native"); await waitState(app, (state) => state.settings.theme === "native");
    await app.press("id:interface-font-system"); await waitState(app, (state) => state.settings.interfaceFont === "system");
    await app.press("id:tab-close-detach"); await waitState(app, (state) => state.settings.tabCloseBehavior === "detach");
    const initial = await inspect(app), previous = initial.settings.agentTerminalsUnrestricted;
    await app.press("id:agent-terminals-unrestricted"); await waitState(app, (state) => state.settings.agentTerminalsUnrestricted !== previous);
    await app.press("id:orchestrator-mode-chat"); await waitState(app, (state) => state.settings.orchestratorMode === "chat");
    await app.press("id:orchestrator-provider-claude"); await waitState(app, (state) => state.settings.orchestratorChatProvider === "claude");
    expect((await inspect(app)).settings.orchestratorChatModel).toBe(null);
    await app.type("id:orchestrator-other-model", "x".repeat(101)); await app.submit("id:orchestrator-other-model");
    await app.waitFor("id:orchestrator-model-error"); expect(app.texts()).toContain("A model ID has at most 100 characters and no control characters.");
    await app.clear("id:orchestrator-other-model"); await app.type("id:orchestrator-other-model", "fixture-other-model"); await app.submit("id:orchestrator-other-model");
    await waitState(app, (state) => state.settings.orchestratorChatModel === "fixture-other-model");
    const policy = (await inspect(app)).settings.delegation;
    await app.press("id:delegation-spread-work"); await waitState(app, (state) => state.settings.delegation.spreadWork !== policy.spreadWork);
    const after = (await inspect(app)).settings;
    expect(after.delegation.design).toEqual(policy.design); expect(after.delegation.workhorse).toEqual(policy.workhorse); expect(after.delegation.parallelShells).toBe(policy.parallelShells);
  } finally { await restore(app); }
});

test("P6a status settings bound reordering, change usage mode, and reset the domain defaults", async () => {
  const app = await fixture();
  try {
    await app.press("id:settings-status-reset"); await waitState(app, (state) => state.settings.statusBar.enabled);
    const initial = (await inspect(app)).settings.statusBar;
    for (const side of ["left", "right"]) {
      const items = initial.items.filter((item) => item.side === side);
      if (!items.length) continue;
      expect(app.enabled(`id:settings-status-earlier-${items[0].kind}`)).toBe(false);
      expect(app.enabled(`id:settings-status-later-${items[items.length - 1].kind}`)).toBe(false);
    }
    await app.press("id:status-usage-all_accounts"); await waitState(app, (state) => state.settings.statusBar.usageMode === "all_accounts");
    const selected = initial.items.find((item) => item.side === "left")!;
    await app.press(`id:settings-status-side-${selected.kind}`); await waitState(app, (state) => state.settings.statusBar.items.find((item) => item.kind === selected.kind)?.side === "right");
    await app.press(`id:settings-status-visible-${selected.kind}`); await waitState(app, (state) => state.settings.statusBar.items.find((item) => item.kind === selected.kind)?.enabled !== selected.enabled);
    await app.press("id:settings-status-reset"); await waitState(app, (state) => state.settings.statusBar.usageMode === "current_chat");
    expect((await inspect(app)).settings.statusBar).toEqual(initial);
  } finally { await restore(app); }
});

test("P6a account rows retain unavailable choices and key input validates a masked paste without exposing it", async () => {
  const app = await fixture();
  try {
    await app.waitFor("id:codex-account-fixture-saved");
    const before = await inspect(app);
    expect(before.accounts.accounts.find((account) => account.id === "fixture-unavailable")?.available).toBe(false);
    await app.press("id:codex-account-fixture-saved"); await waitState(app, (state) => state.accounts.selectedId === "fixture-saved");
    await app.host.send("app.fixture.reveal-key"); await app.waitFor("id:claude-api-key-input");
    await app.type("id:claude-api-key-input", "bad key has spaces"); await app.press("id:claude-api-key-save");
    await app.waitFor("id:claude-api-key-error");
    expect(app.texts().join("\n").includes("bad key has spaces")).toBe(false);
    await app.clear("id:claude-api-key-input");
    await app.paste("id:claude-api-key-input", { text: "fixture-key-not-a-real-secret" });
    await app.keyboard.press("mod+a"); await app.keyboard.press("mod+c"); await app.keyboard.press("mod+x");
    expect(app.clipboard()).toBe("keep clipboard");
    await app.press("id:claude-api-key-save"); await waitState(app, (state) => state.keySaved === true);
    expect(app.value("id:claude-api-key-input")).toBe("");
    expect(app.texts().join("\n").includes("fixture-key-not-a-real-secret")).toBe(false);
    await app.press("id:claude-api-key-remove"); await waitState(app, (state) => state.keySaved === false);
  } finally { await restore(app); }
});

test("P6a an API key reveal requested before settings load focuses the mounted secure input", async () => {
  const app = await fixture(650, true);
  try {
    await app.waitFor("id:claude-api-key-input");
    // The reveal settles after refs have native geometry. No selector focuses
    // the input before this key, so its value proves the route kept the request.
    await app.wait(50); await app.keyboard.press("x");
    expect(app.value("id:claude-api-key-input")).toBe("x");
    expect(app.texts().includes("x")).toBe(false);
  } finally { await restore(app); }
});

test("P6a opening Settings focuses the selected theme after its asynchronous settings delivery", async () => {
  const app = await fixture(650);
  try {
    const initial = (await inspect(app)).settings;
    const choices = initial.themeChoices, index = choices.findIndex((choice) => choice.id === initial.theme);
    const next = choices[(index + 1) % choices.length].id;
    // No test action targets a theme cell: keyboard behavior proves the panel
    // moved focus from the workspace body after its selected ref was mounted.
    await app.wait(50); await app.keyboard.press("right");
    expect((await inspect(app)).settings.theme).toBe(initial.theme);
    await app.keyboard.press("enter"); await waitState(app, (state) => state.settings.theme === next);
  } finally { await restore(app); }
});

test("P6a theme arrows move focus before Enter while choice arrows save and skip unavailable chips", async () => {
  const app = await fixture(1000, false, { keyboardFixture: true });
  try {
    await app.press("id:theme-choice:ghostty"); await waitState(app, (state) => state.settings.theme === "ghostty");
    await app.keyboard.press("right"); expect((await inspect(app)).settings.theme).toBe("ghostty");
    await app.keyboard.press("enter"); await waitState(app, (state) => state.settings.theme === "native");
    await app.press("id:tab-close-ask"); await waitState(app, (state) => state.settings.tabCloseBehavior === "ask");
    await app.keyboard.press("left"); await waitState(app, (state) => state.settings.tabCloseBehavior === "exit");
    await app.press("id:fixture-choice-a"); await app.keyboard.press("right"); expect((await inspect(app)).fixtureChoice).toBe("fixture-choice-c");
    await app.keyboard.press("right"); expect((await inspect(app)).fixtureChoice).toBe("fixture-choice-a");
    expect(app.enabled("id:fixture-choice-disabled")).toBe(false);
  } finally { await restore(app); }
});

test("P6a Orca import keeps a stale preview from confirming and reports a successful refreshed import", async () => {
  const app = await fixture(1000, false, { importScenario: "stale" });
  try {
    await app.waitFor("id:orca-import-confirm");
    expect(app.texts()).toContain("2 projects · 3 worktrees");
    expect(app.texts()).toContain("Fixture warning: projects will be unfiled.");
    await app.press("id:orca-import-confirm"); await app.waitFor("id:orca-import-error");
    expect(app.shows("id:orca-import-confirm")).toBe(false);
    expect((await inspect(app)).importAttempts).toBe(1);
    await app.press("id:orca-import-preview"); await app.waitFor("id:orca-import-confirm");
    await app.press("id:orca-import-confirm"); await waitState(app, (state) => state.notice === "Orca import completed");
    expect(app.texts()).toContain("Imported"); expect(app.shows("id:orca-import-confirm")).toBe(false);
    expect((await inspect(app)).importAttempts).toBe(2);
  } finally { await restore(app); }
});
