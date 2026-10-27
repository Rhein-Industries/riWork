import { expect, open, test } from "nsh/test";
import { DEFERRED_COMMANDS, DEFERRED_KEYMAP, PANEL_COMMANDS, TAB_CYCLE_KEYS, WINDOW_KEYMAP } from "../keyboard";

test("deferred chords retain their context and host-only global keys are absent", () => {
  const chords = DEFERRED_KEYMAP.map(([key, , scope]) => `${scope}:${key}`);
  expect(new Set(chords).size).toBe(chords.length);
  for (const [key, command] of DEFERRED_KEYMAP) {
    expect(DEFERRED_COMMANDS.has(command)).toBe(true);
    expect(["mod+alt+g", "mod+shift+o", "mod+q"].includes(key)).toBe(false);
  }
});

test("P1-MENU-03 P1-KEYS-03 panel chords and host commands select one placeholder and new window routes this project", async () => {
  const app = await open("../App.tsx", {
    platform: "macos", width: 1220, height: 780,
    host: { params: {
      schema: 1, windowId: "w2-panels", kind: "project", projectId: "w2-panels", path: "/fixture/Panels",
      restore: { layout: { layout: { pane: 1 }, active_pane: 1, panels_initialized: true, locked_panes: [], panes: { "1": { tabs: [] } } } },
    }, handlers: { "app.restoreReady": null, "window.setDockInfo": null, "window.open": { windowId: "new", created: true } } },
  });
  try {
    await app.waitFor("id:workspace-dock");
    for (const [key, command] of WINDOW_KEYMAP) {
      const panel = PANEL_COMMANDS[command];
      if (!panel) continue;
      await app.keyboard.press(key);
      expect(app.shows(`id:panel:panel:${panel}`)).toBe(true);
      await app.host.send("command", { id: command });
      expect(app.shows(`id:tab:panel:${panel}`)).toBe(true);
    }
    const before = app.host.calls().length;
    await app.keyboard.press("mod+shift+n");
    await app.waitUntil(() => app.host.calls().length > before);
    expect(app.host.calls().some((call) => call.name === "window.open" && (call.data as { projectId: string }).projectId === "w2-panels")).toBe(true);
    await app.host.send("command", { id: "openProjectSettings" });
    for (const id of DEFERRED_COMMANDS) await app.host.send("command", { id });
    expect(app.shows("id:panel:panel:project_settings")).toBe(true);
  } finally { app.dispose(); }
});

test("P1-KEYS-04 close modal blocks window actions and modifier shortcuts dismiss All tabs before selecting", async () => {
  const app = await open("../App.tsx", {
    platform: "macos", width: 1220, height: 780,
    host: { params: {
      schema: 1, windowId: "w2-modal", kind: "project", projectId: "w2-modal", path: "/fixture/Modal",
      restore: { layout: { layout: { pane: 1 }, active_pane: 1, panels_initialized: true, locked_panes: [], panes: { "1": { tabs: [{ kind: "shell", shell_id: "first" }, { kind: "chat", chat_id: "second" }], active_tab_key: "shell:first" } } } },
    }, handlers: { "app.restoreReady": null, "window.setDockInfo": null } },
  });
  try {
    await app.waitFor("id:tab:shell:first");
    await app.keyboard.press("mod+w");
    expect(app.shows("id:close-dialog")).toBe(true);
    await app.keyboard.press("mod+d");
    await app.keyboard.press("mod+2");
    await app.host.send("command", { id: "openSettings" });
    expect(app.texts()).toContain("1 panes");
    expect(app.shows("id:panel:panel:settings")).toBe(false);
    await app.keyboard.press("escape");
    expect(app.shows("id:close-dialog")).toBe(false);
    expect(app.shows("id:panel:shell:first")).toBe(true);
    await app.press("id:all-tabs");
    expect(app.shows("id:all-tabs-menu")).toBe(true);
    await app.keyboard.press("mod+2");
    expect(app.shows("id:all-tabs-menu")).toBe(false);
    expect(app.shows("id:panel:chat:second")).toBe(true);
    expect(app.host.posts().filter((post) => post.name === "window.setDockInfo").length).toBe(1);
  } finally { app.dispose(); }
});

test("deferred keys are inert and all tab-cycle aliases preserve the selected placeholder", async () => {
  const app = await open("../App.tsx", {
    platform: "macos", width: 1220, height: 780,
    host: {
      params: {
        schema: 1, windowId: "m5-keyboard", kind: "project", projectId: "m5-keyboard", path: "/fixture/Keys",
        restore: { layout: {
          layout: { pane: 1 }, active_pane: 1, main_pane: 1, panels_initialized: true, locked_panes: [],
          panes: { "1": { tabs: [{ kind: "shell", shell_id: "first" }, { kind: "chat", chat_id: "second" }], active_tab_key: "shell:first" } },
        } },
      }, handlers: { "app.restoreReady": null },
    },
  });
  try {
    await app.waitFor("id:tab:shell:first");
    const calls = app.host.calls();
    const posts = app.host.posts();
    for (const key of new Set(DEFERRED_KEYMAP.map(([key]) => key))) {
      await app.keyboard.press(key);
      expect(app.shows("id:panel:shell:first")).toBe(true);
      expect(app.shows("id:panel:chat:second")).toBe(false);
    }
    expect(app.host.calls()).toEqual(calls);
    expect(app.host.posts()).toEqual(posts);
    for (const [key] of TAB_CYCLE_KEYS) {
      await app.keyboard.press(key);
      expect(app.shows("id:panel:chat:second")).toBe(true);
      await app.keyboard.press(key);
      expect(app.shows("id:panel:shell:first")).toBe(true);
    }
  } finally { app.dispose(); }
});

test("P1-KEYS-04 arrows stay inside All tabs; Escape closes it and restores window shortcuts", async () => {
  const app = await open("../App.tsx", {
    platform: "macos", width: 1220, height: 780,
    host: { params: {
      schema: 1, windowId: "w2-strip-keys", kind: "project", projectId: "w2-strip-keys", path: "/fixture/Strip",
      restore: { layout: { layout: { pane: 1 }, active_pane: 1, panels_initialized: true, locked_panes: [], panes: { "1": { tabs: [{ kind: "shell", shell_id: "first" }, { kind: "chat", chat_id: "second" }], active_tab_key: "shell:first" } } } },
    }, handlers: { "app.restoreReady": null, "window.setDockInfo": null } },
  });
  try {
    await app.waitFor("id:tab:shell:first");
    await app.press("id:all-tabs");
    await app.keyboard.press("down");
    expect(app.shows("id:all-tabs-menu")).toBe(true);
    expect(app.shows("id:panel:shell:first")).toBe(true);
    expect(app.shows("id:panel:chat:second")).toBe(false);
    await app.keyboard.press("enter");
    expect(app.shows("id:all-tabs-menu")).toBe(false);
    expect(app.shows("id:panel:chat:second")).toBe(true);

    // Down wraps from the second row to the first, but Escape cancels that
    // tentative highlight and leaves the visible session untouched.
    await app.press("id:all-tabs");
    await app.keyboard.press("down");
    await app.keyboard.press("escape");
    expect(app.shows("id:all-tabs-menu")).toBe(false);
    expect(app.shows("id:panel:chat:second")).toBe(true);
    await app.keyboard.press("ctrl+shift+tab");
    expect(app.shows("id:panel:shell:first")).toBe(true);

    // Both axes belong to the overlay while it is open, including wraparound.
    await app.press("id:all-tabs");
    for (const key of ["up", "left", "right"]) {
      await app.keyboard.press(key);
      expect(app.shows("id:panel:shell:first")).toBe(true);
    }
    await app.keyboard.press("space");
    expect(app.shows("id:all-tabs-menu")).toBe(false);
    expect(app.shows("id:panel:chat:second")).toBe(true);
    await app.press("id:all-tabs");
    await app.keyboard.press("ctrl+tab");
    expect(app.shows("id:all-tabs-menu")).toBe(false);
    expect(app.shows("id:panel:shell:first")).toBe(true);
  } finally { app.dispose(); }
});
