import { expect, test } from "nsh/test";
import { latest, namedWorkspace } from "./workspace-test-support";

test("P1-LAY-03 every GPUI-written L fixture renders golden tab identities and selection", async () => {
  const index = await namedWorkspace("L-NAV", "reopen-index");
  const manifest = latest(index).manifest!;
  index.dispose();
  const fixtures = manifest.fixtures.filter((entry) => entry.name.startsWith("L-"));
  expect(fixtures.length).toBe(13);
  for (const fixture of fixtures) {
    const app = await namedWorkspace(fixture.name, `reopen-${fixture.name}`);
    try {
      const state = latest(app);
      const ids = state.snapshot.panes.flatMap((pane) => pane.tabs.map((tab) => tab.id));
      expect(ids).toEqual(fixture.tabs);
      expect(state.snapshot.panes.map((pane) => pane.activeTab).filter(Boolean)).toEqual(fixture.activeTabs);
      expect(app.shows("id:workspace-dock")).toBe(true);
      expect(app.host.calls().some((call) => call.name === "window.open")).toBe(false);
      if (fixture.name === "L-EMPTY") expect(app.texts()).toContain("Drop a tab here or use the pane menu");
      if (fixture.name === "L-GLOBAL-TABS") expect(app.texts()).toContain("The dashboard and the global chat now have their own window: View › Global Dashboard (⌘⌥G) or Global Chat (⌘⇧O).");
    } finally { app.dispose(); }
  }
}, 60000);

test("P1-DRAG-04/P1-LAY-09 project switch carries locked panel mounts without replacing DockArea", async () => {
  const app = await namedWorkspace("L-CARRY", "carry-identity");
  try {
    const before = latest(app);
    const carried = before.snapshot.panes.filter((pane) => pane.locked).flatMap((pane) => pane.tabs.map((tab) => tab.id));
    const destination = before.manifest!.fixtures.find((fixture) => fixture.name === "L-CARRY-DEST")!;
    await app.host.send("project.open", { projectId: destination.projectId });
    await app.waitUntil(() => latest(app).snapshot.projectId === destination.projectId);
    const after = latest(app).snapshot.panes.flatMap((pane) => pane.tabs.map((tab) => tab.id));
    for (const id of carried) {
      expect(after.includes(id)).toBe(true);
      await app.press(`id:tab-${id}`);
      expect(app.shows(`id:panel:${id} >> text:mount 1`)).toBe(true);
      expect(app.shows(`id:panel:${id} >> text:visible: true`)).toBe(true);
    }
    expect(app.texts().includes("mount 2")).toBe(false);
    expect(app.texts()).toContain("mount 1");
  } finally { app.dispose(); }
});
