import { expect, test } from "nsh/test";
import { latest, namedWorkspace } from "./workspace-test-support";
import { stripTabs } from "../model";

test("P1-STRIP-01/02 shared tabs draw panels first as icons and named session cells; unreadable inventory falls back", async () => {
  const app = await namedWorkspace("Alpha", "strip");
  const classic = await namedWorkspace("Beta", "classic");
  try {
    // The window-wide shared strip left with the GPUI chrome: shared tabs draw in
    // each pane's own tab bar, where panel tabs are icon cells named by their
    // labels. Beta's unreadable inventory falls back to classic tabs, whose panel
    // tabs read their titles.
    expect(app.shows("id:tab-panel:projects")).toBe(true);
    expect(app.shows("id:tab-panel:projects >> text:Projects")).toBe(false);
    expect(classic.shows("id:tab-panel:projects >> text:Projects")).toBe(true);
    const tabs = stripTabs(latest(app).snapshot);
    expect(tabs.slice(0, 5).map((tab) => tab.title)).toEqual(["Projects", "Files", "Worktrees", "Tasks", "Sessions"]);
    expect(tabs.filter((tab) => tab.kind !== "panel").slice(0, 3).map((tab) => tab.title)).toEqual(["Alpha-S1", "Alpha-S2", "Alpha-S3"]);
    expect(app.shows("label:Alpha-S1, idle")).toBe(true);
    expect(app.shows("label:Chat 2, waiting")).toBe(true);
    expect(app.count("role:tablist")).toBe(latest(app).snapshot.panes.length);
    expect(classic.count("role:tablist")).toBe(latest(classic).snapshot.panes.length);
  } finally { classic.dispose(); app.dispose(); }
});
test("P1-STRIP-03 shared menu disables boundary moves and rename persists into a peer", async () => {
  const app = await namedWorkspace("Alpha", "rename");
  try {
    const id = `shell:${latest(app).manifest!.aliases["Alpha-S1"]}`;
    await app.rightClick(`id:tab-${id}`);
    const menu = app.menu() ?? [];
    expect(menu.filter((item) => item.label).map((item) => item.label)).toEqual(["Rename…", "Move left", "Move right", "Close", "Cancel"]);
    expect(menu.find((item) => item.label === "Move left")?.disabled).toBe(true);
    await app.chooseMenuItem("Cancel");
    await app.press(`id:tab-${id}`);
    await app.keyboard.press("shift+f10");
    expect(app.menu()?.filter((item) => item.label).map((item) => item.label)).toEqual(menu.filter((item) => item.label).map((item) => item.label));
    await app.chooseMenuItem("Cancel");
    await app.host.send("command", { id: "tabContextMenu" });
    expect(app.menu()?.filter((item) => item.label).map((item) => item.label)).toEqual(menu.filter((item) => item.label).map((item) => item.label));
    await app.chooseMenuItem("Rename…");
    expect(app.value("id:tab-rename-input")).toBe("Alpha-S1");
    await app.clear("id:tab-rename-input");
    await app.type("id:tab-rename-input", "Renamed Alpha");
    await app.submit("id:tab-rename-input");
    await app.waitFor("label:Renamed Alpha, idle");
    const peer = await namedWorkspace("Alpha", "renamed-peer");
    try { expect(peer.shows("label:Renamed Alpha, idle")).toBe(true); } finally { peer.dispose(); }
    await app.rightClick(`id:tab-${id}`); await app.chooseMenuItem("Rename…");
    await app.clear("id:tab-rename-input"); await app.type("id:tab-rename-input", "Alpha-S1"); await app.submit("id:tab-rename-input");
    await app.waitFor("label:Alpha-S1, idle");
  } finally { app.dispose(); }
});
test("P1-STRIP-04 overflow menu survives width change while open, wraps keys, closes on selection", async () => {
  const app = await namedWorkspace("Busy", "overflow", 800);
  try {
    // Each pane whose tabs overflow has its own All tabs menu (the shared strip's
    // single one left with the GPUI chrome); Busy's sessions all sit in one pane,
    // which needs a wider window than the strip did before its 24 tabs fit.
    const aliases = latest(app).manifest!.aliases;
    const busy = latest(app).snapshot.panes.find((pane) => pane.tabs.some((tab) => tab.id === `shell:${aliases["Busy-S01"]}`))!.id;
    const allTabs = `id:strip-all-tabs:${busy}`;
    expect(app.shows(allTabs)).toBe(true);
    await app.press(allTabs);
    expect(app.count("role:menuitem") > 0).toBe(true);
    await app.setViewport(2560, 780);
    expect(app.shows(allTabs)).toBe(true);
    expect(app.count("role:menuitem") > 0).toBe(true);
    await app.keyboard.press("escape");
    expect(app.count("role:menuitem")).toBe(0);
    expect(app.shows(allTabs)).toBe(false);
    await app.setViewport(800, 780);
    await app.press(allTabs);
    await app.keyboard.press("up"); await app.keyboard.press("enter");
    expect(latest(app).snapshot.panes.some((pane) => pane.activeTab === `shell:${aliases["Busy-S24"]}`)).toBe(true);
    await app.keyboard.press("mod+1");
    await app.keyboard.press("mod+9");
    expect(app.shows(`id:tab-shell:${aliases["Busy-S24"]}`)).toBe(true);
    await app.press(allTabs);
    expect(app.count("role:menuitem") > 0).toBe(true);
    // Focus mode closes the open menu.
    await app.keyboard.press("mod+shift+f");
    expect(app.count("role:menuitem")).toBe(0);
    await app.keyboard.press("mod+shift+f");
  } finally { app.dispose(); }
});
