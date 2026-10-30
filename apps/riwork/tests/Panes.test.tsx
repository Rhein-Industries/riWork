import { expect, test } from "nsh/test";
import { latest, namedWorkspace, workspace } from "./workspace-test-support";

test("P1-PANE-03 a real divider drag saves its ratio and reopens without rebuilding", async () => {
  const app = await workspace("divider-reopen");
  try {
    await app.drag("label:Pane divider >> nth=0", { x: 200, y: 0 });
    const after = latest(app).dock;
    await app.wait(300);
    const peer = await workspace("divider-reopen", null, 1280, "divider-reopen-peer");
    try { expect(latest(peer).dock).toEqual(after); } finally { peer.dispose(); }
  } finally { app.dispose(); }
});

test("P1-PANE-01 Gamma initializes the core navigation and one newly created shell", async () => {
  const app = await namedWorkspace("Gamma", "default-shell");
  try {
    const state = latest(app).snapshot;
    expect(state.panes.length).toBe(2);
    expect(state.panes[0].locked).toBe(true);
    expect(state.panes[0].tabs.map((tab) => tab.title)).toEqual(["Projects", "Files", "Worktrees", "Tasks", "Sessions"]);
    expect(state.panes[1].tabs.filter((tab) => tab.kind === "shell").length).toBe(1);
  } finally { app.dispose(); }
});
test("P1-PANE-02 split creates a selected shell and failed creation leaves the tree unchanged", async () => {
  const app = await namedWorkspace("Alpha", "split-create");
  const beta = await namedWorkspace("Beta", "split-fail");
  try {
    const before = latest(app).snapshot.panes.length;
    await app.keyboard.press("mod+shift+f");
    await app.keyboard.press("mod+d");
    await app.waitUntil(() => latest(app).snapshot.panes.length === before + 1);
    const state = latest(app).snapshot;
    expect(state.focusMode).toBe(false);
    expect(state.panes.find((pane) => pane.id === state.activePane)?.tabs.length).toBe(1);
    const betaBefore = latest(beta).snapshot.panes;
    await beta.keyboard.press("mod+shift+d");
    await beta.waitUntil(() => beta.texts().some((text) => text.includes("Fixture shell creation failed")));
    // The error notice occupies vertical space; pane contents and the split
    // tree must be unchanged, while its pixel projection fits the smaller area.
    expect(latest(beta).snapshot.panes).toEqual(betaBefore);
  } finally { beta.dispose(); app.dispose(); }
});
test("P1-PANE-06 new shells route to main except while focused", async () => {
  const app = await namedWorkspace("Alpha", "main-route");
  try {
    await app.press("id:tab-panel:projects");
    const before = latest(app).snapshot.panes.find((pane) => pane.main)!.tabs.length;
    await app.keyboard.press("mod+t");
    await app.waitUntil(() => latest(app).snapshot.panes.find((pane) => pane.main)!.tabs.length > before);
    const main = latest(app).snapshot.panes.find((pane) => pane.main)!;
    expect(latest(app).snapshot.activePane).toBe(main.id);
    await app.press("id:tab-panel:projects");
    await app.keyboard.press("mod+shift+f");
    const navCount = latest(app).snapshot.panes[0].tabs.length;
    await app.keyboard.press("mod+t");
    await app.waitUntil(() => latest(app).snapshot.panes[0].tabs.length > navCount);
    expect(latest(app).snapshot.activePane).toBe(latest(app).snapshot.panes[0].id);
    await app.keyboard.press("mod+shift+f");
  } finally { app.dispose(); }
});

test("P1-PANE-05 locked panes refuse tab and pane closes", async () => {
  const app = await workspace("locked");
  try {
    await app.press("id:tab:panel:projects");
    await app.keyboard.press("mod+w");
    expect(app.texts()).toContain("Unlock the pane to close its tabs");
    expect(app.shows("id:tab-close-panel:projects")).toBe(false);
    await app.keyboard.press("mod+shift+w");
    expect(app.texts()).toContain("Unlock the pane to close it");
    expect(latest(app).snapshot.panes.length).toBe(3);
  } finally { app.dispose(); }
});
test("P1-PANE-07 closing a non-main pane appends its tabs and focuses main", async () => {
  const app = await workspace("close-pane");
  try {
    await app.keyboard.press("mod+shift+w");
    expect(latest(app).snapshot.panes.length).toBe(2);
    expect(latest(app).snapshot.activePane).toBe(2);
    expect(latest(app).snapshot.panes.find((pane) => pane.id === 2)?.tabs.map((tab) => tab.id)).toEqual(["shell:S1", "shell:S2", "shell:S3", "panel:tasks"]);
  } finally { app.dispose(); }
});
test("P1-PANE-08 and P1-PANE-09 gather/default commands show core notices", async () => {
  const app = await workspace("gather-default");
  try {
    await app.keyboard.press("mod+shift+m");
    expect(app.texts().some((text) => text.startsWith("Gathered "))).toBe(true);
    await app.keyboard.press("mod+alt+l");
    expect(latest(app).snapshot.panes.length).toBe(2);
    expect(app.texts().some((text) => text.startsWith("Applied the default layout"))).toBe(true);
    await app.host.send("command", { id: "defaultLayout" });
    expect(app.texts()).toContain("The default layout is already in place");
  } finally { app.dispose(); }
});
test("P1-PANE-10 focus hides other groups without changing the saved tree", async () => {
  const app = await workspace("focus");
  try {
    await app.wait(2100);
    const before = latest(app).dock;
    await app.keyboard.press("mod+shift+f");
    expect(app.texts()).toContain("↙ RESTORE  ⌘⇧F");
    expect(app.count("role:tablist")).toBe(1);
    expect(latest(app).dock).toEqual(before);
    await app.host.send("command", { id: "focusCentered" });
    await app.keyboard.press("mod+shift+f");
    expect(app.count("role:tablist")).toBe(3);
    expect(latest(app).dock).toEqual(before);
  } finally { app.dispose(); }
});
test("P1-PANE-11 pane body activation routes a shell to that pane when main is unset", async () => {
  const app = await namedWorkspace("Alpha", "body-route");
  try {
    // Alpha is shared by earlier shell/split fixtures. Restore this test's
    // starting geometry before exercising pane-body routing without a main.
    await app.host.send("command", { id: "defaultLayout" });
    if (app.shows("id:dismiss-notice")) await app.press("id:dismiss-notice");
    const main = latest(app).snapshot.panes.find((pane) => pane.main)!;
    await app.press(`id:pane-menu-${main.id}`);
    expect(app.menu()?.some((item) => item.label === "Pane > Main pane")).toBe(true);
    await app.chooseMenuItem("Pane > Main pane");
    expect(latest(app).snapshot.panes.some((pane) => pane.main)).toBe(false);
    const target = latest(app).snapshot.panes.find((pane) => !pane.locked)!;
    await app.press("id:tab-panel:projects");
    await app.press(`id:pane-${target.id}`);
    const before = target.tabs.length;
    await app.keyboard.press("mod+t");
    await app.waitUntil(() => latest(app).snapshot.panes.find((pane) => pane.id === target.id)!.tabs.length > before);
    expect(latest(app).snapshot.activePane).toBe(target.id);
    await app.press(`id:pane-menu-${main.id}`);
    expect(app.menu()?.some((item) => item.label === "Pane > Main pane")).toBe(true);
    await app.chooseMenuItem("Pane > Main pane");
  } finally { app.dispose(); }
});
