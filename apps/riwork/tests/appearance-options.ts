import type { App } from "nsh/test";
import { DEFAULT_STATUS_ITEMS } from "../StatusBar";

export function status(enabled = true) {
  return { enabled, usageMode: "current_chat", items: DEFAULT_STATUS_ITEMS.map((item) => ({ ...item, label: item.kind })) };
}

export function options(windowId: string, theme = "ri_work") {
  return {
    platform: "macos" as const, width: 1220, height: 780, clock: {},
    host: {
      params: {
        schema: 1, windowId, kind: "project", projectId: "appearance-project", path: "/fixture/Project Alpha", appearance: "system",
        initial: { theme, uiTextSize: 11, uiTextMatchesTerminal: false, interfaceFont: "system_mono", statusBar: status() },
        restore: { layout: {
          layout: { pane: 1 }, active_pane: 1, main_pane: 1, panels_initialized: true, locked_panes: [],
          panes: { "1": { tabs: [{ kind: "panel", panel: "projects" }, { kind: "panel", panel: "files" }], active_tab_key: "panel:files" } },
        } },
      },
      handlers: { "system.fonts": { families: ["SF Mono", "Menlo"] }, "window.open": { windowId: "global-fixture", created: true }, "app.fixture.saved": null },
    },
  };
}

export async function settings(app: App, patch: unknown) {
  const count = app.host.posts().filter((post) => post.name === "app.fixture.saved").length;
  await app.host.send("app.fixture.settings", patch);
  await app.waitUntil(() => app.host.posts().filter((post) => post.name === "app.fixture.saved").length > count);
}

export async function statusEdit(app: App, edit: unknown) {
  const count = app.host.posts().filter((post) => post.name === "app.fixture.saved").length;
  await app.host.send("app.fixture.status", edit);
  await app.waitUntil(() => app.host.posts().filter((post) => post.name === "app.fixture.saved").length > count);
}
