import { expect, test } from "nsh/test";
import { latest, namedWorkspace, workspace } from "./workspace-test-support";

const open = (app: Awaited<ReturnType<typeof workspace>>, id: string) => latest(app).snapshot.panes.flatMap((pane) => pane.tabs).some((tab) => tab.id === id);

test("P1-TAB-04 closing a tab asks nothing: its shell keeps running, and the notice offers Undo and End", async () => {
  const app = await workspace("close-tab");
  try {
    await app.keyboard.press("mod+w");
    await app.waitUntil(() => !open(app, "shell:S3"));
    expect(app.shows("id:tab-close-dialog")).toBe(false);
    expect(app.texts()).toContain("Closed Shell · S3. The shell keeps running.");
    await app.press("id:tab-closed-undo");
    await app.waitUntil(() => open(app, "shell:S3"));
    expect(app.shows("id:tab-closed-notice")).toBe(false);
    await app.keyboard.press("mod+w");
    await app.waitFor("id:tab-closed-end");
    await app.press("id:tab-closed-end");
    expect(app.shows("id:tab-closed-notice")).toBe(false);
    expect(app.shows("id:workspace-notice")).toBe(false);
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
    await app.waitFor(`id:tab-${id}`, { state: "gone" });
    await peer.waitFor(`id:tab-${id}`, { state: "gone" });
  } finally { peer.dispose(); app.dispose(); }
});
