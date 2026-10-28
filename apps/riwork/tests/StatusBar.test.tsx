import { expect, open, test } from "nsh/test";
import { options, statusEdit } from "./appearance-options";

test("P1-STAT-01: default kinds, order, sides and master visibility reach both windows", async () => {
  const app = await open("./Appearance.fixture.tsx", options("status-main"));
  const peer = await open("./Appearance.fixture.tsx", options("status-peer"));
  try {
    await app.waitFor("id:status-bar"); await peer.waitFor("id:status-bar");
    await app.waitFor("text:0 LIVE"); await peer.waitFor("text:0 LIVE");
    await app.waitFor("text:CODEX · System default · UNAVAILABLE");
    expect(app.shows("id:status-left >> id:status-item-project")).toBe(true);
    for (const kind of ["live-sessions", "resources", "usage", "codex-account", "session-id", "project-orchestrator", "layout"]) {
      expect(app.shows(`id:status-right >> id:status-item-${kind}`)).toBe(true);
    }
    expect(app.shows("id:status-right >> id:top-orchestrator")).toBe(true);
    // Files now also names the project. Assert each status label in its own
    // surface, then compare the final status-bar sequence after panel content.
    // P5: the fixture backend's system default (no saved list, no figures).
    const labels = ["Project Alpha", "0 LIVE", "CPU — RAM —", "CODEX · System default · UNAVAILABLE", "DEFAULT (APP) · System default · email unknown", "Global", "P·ORCH", "Layout"];
    for (const text of labels) expect(app.count(`id:status-bar >> text:${text}`)).toBe(1);
    const defaults = app.texts().filter((text) => labels.includes(text)).slice(-labels.length);
    expect(defaults).toEqual(labels);
    await statusEdit(app, { action: "visible", kind: "worktree", value: true });
    await statusEdit(app, { action: "side", kind: "layout", side: "left" });
    for (let step = 0; step < 3; step++) await statusEdit(app, { action: "move", kind: "layout", value: true });
    await peer.waitFor("id:status-left >> id:status-item-layout");
    expect(app.shows("id:status-left >> id:status-item-worktree")).toBe(true);
    expect(peer.shows("id:status-item-worktree")).toBe(true);
    expect(app.texts().filter((text) => ["Layout", "Project Alpha", "No worktree"].includes(text)).slice(-3)).toEqual(["Layout", "Project Alpha", "No worktree"]);
    await statusEdit(app, { action: "enabled", value: false });
    await peer.waitUntil(() => !peer.shows("id:status-bar"));
    expect(app.shows("id:status-bar")).toBe(false);
    await statusEdit(app, { action: "enabled", value: true });
    await peer.waitFor("id:status-left >> id:status-item-layout");
  } finally { peer.dispose(); app.dispose(); }
});

test("P1-STAT-02: project and worktree open panels and missing data keeps GPUI forms", async () => {
  const opts = options("status-data");
  opts.host.params.initial.statusBar.items.find((item) => item.kind === "worktree")!.enabled = true;
  opts.host.params.initial.statusBar.items.find((item) => item.kind === "agent_activity")!.enabled = true;
  const app = await open("./Appearance.fixture.tsx", opts);
  try {
    await app.waitFor("id:status-bar");
    expect(app.texts()).toContain("Project Alpha");
    expect(app.texts()).toContain("Agents · —");
    expect(app.texts()).toContain("CPU — RAM —");
    expect(app.enabled("id:status-item-session-id")).toBe(false);
    await app.press("id:status-item-project");
    expect(app.shows("id:panel:panel:projects")).toBe(true);
    await app.press("id:status-item-worktree");
    await app.waitFor("id:panel:panel:worktrees");
  } finally { app.dispose(); }
});

test("P1-STAT-03: layout menu toggles, applies the default and closes when hidden", async () => {
  const app = await open("./Appearance.fixture.tsx", options("status-layout"));
  try {
    await app.waitFor("id:status-bar");
    await app.press("id:status-item-layout");
    expect(app.texts()).toContain("Default layout");
    expect(app.texts()).toContain("⌘⌥L");
    expect(app.texts()).toContain("Gather into main pane");
    expect(app.texts()).toContain("Main pane: Files · 2 tabs");
    await app.press("id:status-item-layout");
    expect(app.shows("id:layout-menu")).toBe(false);
    await app.press("id:status-item-layout");
    await app.press("id:layout-menu-default");
    expect(app.shows("id:layout-menu")).toBe(false);
    expect(app.count("id:workspace-dock")).toBe(1);
    await app.press("id:status-item-layout");
    await statusEdit(app, { action: "visible", kind: "layout", value: false });
    await app.waitUntil(() => !app.shows("id:layout-menu"));
  } finally { app.dispose(); }
});

test("P1-STAT-04: newer notices replace, stay for a minute and dismiss", async () => {
  const app = await open("./Appearance.fixture.tsx", options("status-notice"));
  try {
    await app.waitFor("id:workspace");
    await app.host.send("app.fixture.notice", { text: "First notice" });
    await app.host.send("app.fixture.notice", { text: "Second notice" });
    expect(app.shows("text:First notice")).toBe(false);
    expect(app.count("id:workspace-notice")).toBe(1);
    await app.clock.runFor(60_000);
    expect(app.texts()).toContain("Second notice");
    await app.press("label:Dismiss");
    expect(app.shows("id:workspace-notice")).toBe(false);
    await app.host.send("app.fixture.notice", { text: "Escape notice" });
    await app.keyboard.press("escape");
    expect(app.shows("id:workspace-notice")).toBe(false);
  } finally { app.dispose(); }
});

test("P1-STAT-05: global menu routes both views and closes on escape, outside, hidden or modal", async () => {
  const app = await open("./Appearance.fixture.tsx", options("status-global"));
  try {
    await app.waitFor("id:top-orchestrator");
    await app.host.send("global.show", { view: "dashboard", start: false });
    await app.press("id:top-orchestrator");
    expect(app.texts()).toContain("Dashboard");
    expect(app.texts()).toContain("⌘⌥G ✓");
    await app.press("id:global-menu-dashboard");
    expect(app.host.calls().some((call) => call.name === "window.open" && (call.data as { view: string; start: boolean }).view === "dashboard" && (call.data as { start: boolean }).start === false)).toBe(true);
    await app.host.send("global.show", { view: null, start: false });
    await app.press("id:top-orchestrator");
    expect(app.texts()).toContain("⌘⌥G");
    await app.press("id:global-menu-chat");
    expect(app.host.calls().some((call) => call.name === "window.open" && (call.data as { view: string; start: boolean }).view === "chat" && (call.data as { start: boolean }).start === true)).toBe(true);
    await app.press("id:top-orchestrator");
    await app.keyboard.press("escape");
    expect(app.shows("id:global-menu")).toBe(false);
    await app.press("id:top-orchestrator");
    await app.press("id:tab:panel:files");
    expect(app.shows("id:global-menu")).toBe(false);
    await app.press("id:status-item-layout");
    await app.press("id:top-orchestrator");
    expect(app.shows("id:layout-menu")).toBe(false);
    await statusEdit(app, { action: "visible", kind: "global_orchestrator", value: false });
    await app.waitUntil(() => !app.shows("id:global-menu"));
    await statusEdit(app, { action: "visible", kind: "global_orchestrator", value: true });
    await app.host.send("app.fixture.modal", { open: true });
    await app.press("id:top-orchestrator");
    expect(app.shows("id:global-menu")).toBe(false);
  } finally { app.dispose(); }
});

test("P5-USAGE-07: the usage item reads the fixture account, names it in its label and opens the Usage panel", async () => {
  const app = await open("./Appearance.fixture.tsx", options("usage-current"));
  try {
    await app.waitFor("text:CODEX · System default · UNAVAILABLE");
    expect(app.shows("label:Usage CODEX · System default · UNAVAILABLE")).toBe(true);
    expect(app.shows("label:New Codex sessions: DEFAULT (APP) · System default · email unknown")).toBe(true);
    expect(app.enabled("id:status-item-project-orchestrator")).toBe(false);
    await app.press("id:status-item-usage");
    await app.waitFor("id:panel:panel:usage");
    expect(app.shows("id:status-item-usage")).toBe(true);
  } finally { app.dispose(); }
});

test("P5-USAGE-08: the account item opens a read-only menu, asks for the list and closes like the other menus", async () => {
  const app = await open("./Appearance.fixture.tsx", options("usage-account"));
  try {
    await app.waitFor("text:DEFAULT (APP) · System default · email unknown");
    await app.press("id:status-item-codex-account");
    await app.waitFor("id:account-menu");
    for (const text of ["New Codex sessions in this project", "Running chats and shells keep their account; --account still chooses one.",
      "Inherit app default", "Uses System default", "System default", "Project Settings…"]) expect(app.texts()).toContain(text);
    // Opening with no list starts the (fixture) discovery once; its error is the status.
    await app.waitFor("text:Orca is not consulted in this fixture.");
    expect(app.texts()).toContain("✓");
    expect(app.enabled("id:account-menu-inherit")).toBe(false);
    expect(app.enabled("id:account-menu-system")).toBe(false);
    await app.press("id:status-item-codex-account");
    expect(app.shows("id:account-menu")).toBe(false);
    await app.press("id:status-item-codex-account");
    await app.keyboard.press("escape");
    expect(app.shows("id:account-menu")).toBe(false);
    // A press outside an open menu closes it, as for the layout and global menus.
    await app.press("id:status-item-layout");
    await app.press("id:status-item-codex-account");
    expect(app.shows("id:layout-menu")).toBe(false);
    await app.press("id:status-item-codex-account");
    expect(app.shows("id:account-menu")).toBe(true);
    await app.press("id:account-menu-project-settings");
    expect(app.shows("id:account-menu")).toBe(false);
    await app.press("id:status-item-codex-account");
    await statusEdit(app, { action: "visible", kind: "codex_account", value: false });
    await app.waitUntil(() => !app.shows("id:account-menu"));
    await statusEdit(app, { action: "visible", kind: "codex_account", value: true });
    await app.host.send("app.fixture.modal", { open: true });
    await app.press("id:status-item-codex-account");
    expect(app.shows("id:account-menu")).toBe(false);
  } finally { app.dispose(); }
});

test("P5-USAGE-09: all accounts reads every known account compactly and Native sentence-cases only the text", async () => {
  const opts = options("usage-all", "native");
  opts.host.params.initial.statusBar.usageMode = "all_accounts";
  const app = await open("./Appearance.fixture.tsx", opts);
  try {
    await app.waitFor("text:Codex  System default unavailable");
    expect(app.shows("label:Usage CODEX · System default · UNAVAILABLE")).toBe(true);
    expect(app.texts()).toContain("Default (app) · System default · email unknown");
  } finally { app.dispose(); }
});
