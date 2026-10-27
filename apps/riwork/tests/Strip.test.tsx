import { expect, test } from "nsh/test";
import { latest, namedWorkspace } from "./workspace-test-support";
import { stripTabs } from "../model";

test("P1-STRIP-01/02 shared strip draws panels first and named session cells; unreadable inventory falls back", async () => {
  const app = await namedWorkspace("Alpha", "strip");
  const classic = await namedWorkspace("Beta", "classic");
  try {
    expect(app.shows("id:shared-strip")).toBe(true);
    expect(classic.shows("id:shared-strip")).toBe(false);
    const tabs = stripTabs(latest(app).snapshot);
    expect(tabs.slice(0, 5).map((tab) => tab.title)).toEqual(["Projects", "Files", "Worktrees", "Tasks", "Sessions"]);
    expect(tabs.filter((tab) => tab.kind !== "panel").slice(0, 3).map((tab) => tab.title)).toEqual(["Alpha-S1", "Alpha-S2", "Alpha-S3"]);
    expect(app.shows("label:Alpha-S1, idle")).toBe(true);
    expect(app.shows("label:Chat 2, waiting")).toBe(true);
    expect(app.count("role:tablist")).toBe(1);
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
    expect(app.shows("id:strip-all-tabs")).toBe(true);
    await app.press("id:strip-all-tabs");
    expect(app.count("role:menuitem") > 0).toBe(true);
    await app.setViewport(1920, 780);
    expect(app.shows("id:strip-all-tabs")).toBe(true);
    await app.keyboard.press("escape");
    expect(app.shows("id:strip-all-tabs")).toBe(false);
    await app.setViewport(800, 780);
    await app.press("id:strip-all-tabs");
    await app.keyboard.press("up"); await app.keyboard.press("enter");
    expect(latest(app).snapshot.panes.some((pane) => pane.activeTab === `shell:${latest(app).manifest!.aliases["Busy-S24"]}`)).toBe(true);
    await app.keyboard.press("mod+1");
    await app.keyboard.press("mod+9");
    expect(app.shows(`id:tab-shell:${latest(app).manifest!.aliases["Busy-S24"]}`)).toBe(true);
    await app.press("id:strip-all-tabs");
    await app.keyboard.press("mod+shift+f");
    expect(app.shows("id:strip-all-tabs")).toBe(false);
    await app.keyboard.press("mod+shift+f");
  } finally { app.dispose(); }
});
