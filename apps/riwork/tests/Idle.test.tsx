import { expect, open, test } from "nsh/test";

test("P1-POLL-02: a settled project sends no host work during 60 seconds of idle time", async () => {
  const app = await open("../App.tsx", {
    platform: "macos", width: 1220, height: 780, clock: {},
    host: {
      params: {
        schema: 1, windowId: "m5-idle", kind: "project", projectId: "m5-idle", path: "/fixture/Idle",
        restore: { layout: {
          layout: { pane: 1 }, active_pane: 1, main_pane: 1, panels_initialized: true, locked_panes: [],
          panes: { "1": { tabs: [{ kind: "shell", shell_id: "idle" }], active_tab_key: "shell:idle" } },
        } },
      },
      handlers: { "app.restoreReady": null },
    },
  });
  try {
    await app.waitFor("id:tab:shell:idle");
    await app.clock.runFor(1_000);
    const calls = app.host.calls();
    const posts = app.host.posts();
    await app.clock.runFor(60_000);
    expect(app.host.calls()).toEqual(calls);
    expect(app.host.posts()).toEqual(posts);
  } finally { app.dispose(); }
});
