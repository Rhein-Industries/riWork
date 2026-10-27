import { expect, test } from "nsh/test";
import { CLOSE_DIALOG_BODY } from "../CloseDialog";
import { latest, namedWorkspace, workspace } from "./workspace-test-support";

test("P1-TAB-04 Ask retains shipping copy, backdrop does nothing, Escape cancels", async () => {
  const app = await workspace("close-dialog");
  try {
    await app.keyboard.press("mod+w");
    expect(app.texts()).toContain("Close Shell · S3?");
    expect(app.texts()).toContain(CLOSE_DIALOG_BODY);
    await app.press("id:tab-close-backdrop");
    expect(app.shows("id:tab-close-dialog")).toBe(true);
    await app.keyboard.press("escape");
    expect(latest(app).snapshot.panes.flatMap((pane) => pane.tabs).some((tab) => tab.id === "shell:S3")).toBe(true);
    expect(app.shows("id:tab-close-dialog")).toBe(false);
  } finally { app.dispose(); }
});
test("P1-TAB-04/P1-STRIP-05 Detach removes a new shell from both windows through the shared watcher", async () => {
  const app = await namedWorkspace("Alpha", "detach-first");
  const peer = await namedWorkspace("Alpha", "detach-peer");
  try {
    const before = new Set(latest(app).snapshot.panes.flatMap((pane) => pane.tabs.map((tab) => tab.id)));
    await app.keyboard.press("mod+t");
    await app.waitUntil(() => latest(app).snapshot.panes.some((pane) => pane.tabs.some((tab) => !before.has(tab.id))));
    const id = latest(app).snapshot.panes.flatMap((pane) => pane.tabs).find((tab) => !before.has(tab.id))!.id;
    await peer.waitFor(`id:tab-${id}`);
    await app.keyboard.press("mod+w");
    await app.press("label:Detach");
    await app.waitFor(`id:tab-${id}`, { state: "gone" });
    await peer.waitFor(`id:tab-${id}`, { state: "gone" });
  } finally { peer.dispose(); app.dispose(); }
});
