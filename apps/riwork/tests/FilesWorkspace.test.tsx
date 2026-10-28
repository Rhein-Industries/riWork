import { expect, open, test } from "nsh/test";
import type { WorkspaceSnapshot } from "../model";

test("P3b workspace Files selection places Preview in main, routes Open, and exposes the memo", async () => {
  const app = await open("./FilesWorkspace.fixture.tsx", { platform: "macos", width: 1280, height: 800,
    host: { params: { key: `workspace-${Date.now()}` }, handlers: {
      "app.fixture.state": null, "window.setDockInfo": null, "system.openWith": {},
    } } });
  const latest = () => app.host.posts().filter((post) => post.name === "app.fixture.state").slice(-1)[0].data as WorkspaceSnapshot;
  try {
    await app.waitFor("id:show-panel:files", { timeout: 15000 });
    await app.press("id:show-panel:files");
    await app.waitFor("id:file:note.txt", { timeout: 15000 });
    const before = latest();
    await app.press("id:file:note.txt");
    await app.waitFor("id:preview-code");
    const after = latest();
    expect(after.panes.length).toBe(before.panes.length);
    expect(after.activePane).toBe(before.activePane);
    expect(after.panes.find((pane) => pane.main)?.activeTab).toBe("panel:preview");
    expect(after.panes.find((pane) => pane.id === before.activePane)?.activeTab).toBe("panel:files");
    await app.press("id:preview-open");
    await app.waitUntil(() => app.host.calls().some((call) => call.name === "system.openWith"));
    const target = app.host.calls().find((call) => call.name === "system.openWith")!.data as { path: string };
    expect(target.path.endsWith("/note.txt")).toBe(true);
    await app.host.send("command", { id: "openProjectSettings" });
    await app.waitFor("id:project-memo-editor");
    expect(app.shows("id:project-memo-input")).toBe(true);
  } finally { app.dispose(); }
});

test("P3b an unavailable selected worktree never falls back to the project's files", async () => {
  const app = await open("./FilesWorkspace.fixture.tsx", { platform: "macos", width: 1280, height: 800,
    host: { params: { key: `unavailable-${Date.now()}`, unavailable: true }, handlers: {
      "app.fixture.state": null, "window.setDockInfo": null,
    } } });
  try {
    await app.waitFor("id:show-panel:files", { timeout: 15000 });
    await app.press("id:show-panel:files");
    await app.waitFor("id:files-empty");
    expect(app.texts()).toContain("Select a worktree to browse its files.");
    expect(app.shows("id:file:note.txt")).toBe(false);
  } finally { app.dispose(); }
});
