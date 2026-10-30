import { expect, test } from "nsh/test";
import { latest, workspace } from "./workspace-test-support";

test("P1-TAB-02 opening an existing panel selects it and never duplicates it", async () => {
  const app = await workspace("tabs-open");
  try {
    await app.keyboard.press("mod+shift+e");
    await app.keyboard.press("mod+shift+e");
    expect(latest(app).snapshot.panes.flatMap((pane) => pane.tabs).filter((tab) => tab.id === "panel:files").length).toBe(1);
    expect(latest(app).snapshot.activePane).toBe(1);
    expect(app.shows("id:panel:panel:files")).toBe(true);
    expect(app.shows("id:panel:panel:projects")).toBe(false);
  } finally { app.dispose(); }
});
test("P1-TAB-03 clicking tabs changes only the selected surface", async () => {
  const app = await workspace("tabs-select");
  try {
    await app.press("id:tab:shell:S2");
    expect(app.shows("id:panel:shell:S2")).toBe(true);
    expect(app.shows("id:panel:shell:S1")).toBe(false);
    expect(latest(app).snapshot.activePane).toBe(2);
  } finally { app.dispose(); }
});
test("P1-TAB-02 Dashboard routes globally and panel tooltips retain their natural titles", async () => {
  const app = await workspace("tabs-dashboard");
  try {
    await app.press("id:pane-menu-1");
    await app.chooseMenuItem("Views > Dashboard");
    expect(app.host.calls().find((call) => call.name === "window.open")?.data).toEqual({ kind: "global", view: "dashboard", start: false });
    expect(latest(app).snapshot.panes.flatMap((pane) => pane.tabs).some((tab) => tab.id === "panel:dashboard")).toBe(false);
    await app.hover("id:tab-panel:projects");
    await app.wait(600);
    await app.waitFor("text:Projects · ⌘B");
    await app.hover("id:status-bar");
    expect(app.shows("text:Projects · ⌘B")).toBe(false);
  } finally { app.dispose(); }
});
