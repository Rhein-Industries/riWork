import { expect, open, test } from "nsh/test";

const restore = {
  layout: { pane: 1 }, active_pane: 1, panels_initialized: true, locked_panes: [],
  panes: { "1": { tabs: [{ kind: "shell", shell_id: "find-parent" }, { kind: "shell", shell_id: "find-child" }, { kind: "panel", panel: "projects" }], active_tab_key: "shell:find-parent" } },
};

test("P2b Find is local to each terminal and closes on Escape without switching tabs", async () => {
  const app = await open("../App.tsx", { platform: "macos", width: 1220, height: 780,
    host: { params: { schema: 1, windowId: "p2b-find", kind: "project", projectId: "p2b-find", path: "/fixture/Find", restore: { layout: restore } },
      handlers: { "app.restoreReady": null, "window.setDockInfo": null } } });
  try {
    await app.waitFor("id:terminal:find-parent");
    await app.host.send("command", { id: "find", args: { query: "needle" } });
    expect(app.shows("id:terminal-find:find-parent")).toBe(true);
    await app.host.send("command", { id: "findNext" });
    await app.keyboard.press("escape");
    expect(app.shows("id:terminal-find:find-parent")).toBe(false);
    expect(app.shows("id:terminal:find-parent")).toBe(true);
    await app.press("id:tab:shell:find-child");
    await app.keyboard.press("mod+f");
    expect(app.shows("id:terminal-find:find-child")).toBe(true);
    expect(app.shows("id:terminal-find:find-parent")).toBe(false);
    await app.host.send("command", { id: "findClose" });
    expect(app.shows("id:terminal-find:find-child")).toBe(false);
    await app.press("id:tab:panel:projects");
    await app.host.send("command", { id: "find" });
    expect(app.shows("id:terminal-find:find-child")).toBe(false);
    expect(app.shows("id:search:panel:projects")).toBe(true);
  } finally { app.dispose(); }
});

test("P2b Open Vim is reachable and its dialog blocks window commands", async () => {
  const app = await open("../App.tsx", { platform: "macos", width: 1220, height: 780,
    host: { params: { schema: 1, windowId: "p2b-vim-dialog", kind: "project", projectId: "p2b-vim-dialog", path: "/fixture/Vim", restore: { layout: restore } },
      handlers: { "app.restoreReady": null, "window.setDockInfo": null } } });
  try {
    await app.waitFor("id:terminal:find-parent");
    await app.host.send("command", { id: "openVim" });
    expect(app.shows("id:vim-open-dialog")).toBe(true);
    await app.host.send("command", { id: "openSettings" });
    expect(app.shows("id:panel:panel:settings")).toBe(false);
    await app.press("id:vim-open-cancel");
    expect(app.shows("id:vim-open-dialog")).toBe(false);
    await app.host.send("command", { id: "openSettings" });
    expect(app.shows("id:panel:panel:settings")).toBe(true);
  } finally { app.dispose(); }
});
