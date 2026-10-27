import { expect, open, test } from "nsh/test";
import { DEFERRED_COMMANDS, DEFERRED_KEYMAP, TAB_CYCLE_KEYS } from "../keyboard";

test("deferred chords retain their context and host-only global keys are absent", () => {
  const chords = DEFERRED_KEYMAP.map(([key, , scope]) => `${scope}:${key}`);
  expect(new Set(chords).size).toBe(chords.length);
  for (const [key, command] of DEFERRED_KEYMAP) {
    expect(DEFERRED_COMMANDS.has(command)).toBe(true);
    expect(["mod+alt+g", "mod+shift+o", "mod+q"].includes(key)).toBe(false);
  }
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
