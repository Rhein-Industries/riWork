import { expect, open, test } from "nsh/test";

test("P6B-REMOTE-ROUTING-01 remote local-only panels render unavailable before mounting local editors", async () => {
  const app = await open("./Workspace.fixture.tsx", { platform: "macos", width: 1280, height: 800,
    host: { params: { key: String(Date.now()) }, handlers: { "app.fixture.state": null, "window.setDockInfo": null } } });
  try {
    await app.waitFor("id:show-panel:files", { timeout: 15000 });
    for (const [command, panel] of [["openFiles", "files"], ["openPreview", "preview"], ["openProjectSettings", "project_settings"], ["openSchedules", "schedules"], ["panel:usage", "usage"]]) {
      await app.host.send("command", { id: command });
      await app.waitFor(`id:remote-unavailable-${panel}`);
    }
    expect(app.shows("id:project-memo-editor")).toBe(false);
    expect(app.shows("id:automations-panel")).toBe(false);
    expect(app.shows("id:files-panel")).toBe(false);
    expect(app.texts().some((value) => value.includes("Not available for a project on fixture-"))).toBe(true);
  } finally { app.dispose(); }
});
