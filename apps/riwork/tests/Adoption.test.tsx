import { expect, test } from "nsh/test";
import { latest, namedWorkspace } from "./workspace-test-support";

test("P1-TAB-05 delayed external shell delivery appends without changing selection", async () => {
  const app = await namedWorkspace("Alpha", "adopt-delivery");
  try {
    const before = latest(app).snapshot;
    const id = `shell:${latest(app).manifest!.aliases["Alpha-later"]}`;
    await app.waitFor(`id:tab-${id}`);
    const after = latest(app).snapshot;
    expect(after.activePane).toBe(before.activePane);
    expect(after.panes.map((pane) => pane.activeTab)).toEqual(before.panes.map((pane) => pane.activeTab));
    expect(app.shows(`id:panel:${before.panes.find((pane) => pane.id === before.activePane)!.activeTab}`)).toBe(true);
  } finally { app.dispose(); }
});
