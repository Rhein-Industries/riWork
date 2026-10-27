import { open, type App } from "nsh/test";
import type { DockAreaLayout } from "nsh/base";
import type { WorkspaceSnapshot } from "../model";

export interface FixtureEntry { name: string; projectId: string; tabs: string[]; activeTabs: string[]; expected: { panes: Record<string, { tabs: unknown[] }> } }
export interface FixtureManifest { scenario: string; aliases: Record<string, string>; fixtures: FixtureEntry[] }
export interface FixtureState { snapshot: WorkspaceSnapshot; dock: DockAreaLayout; manifest?: FixtureManifest; fixture?: FixtureEntry }
export function latest(app: App): FixtureState {
  const posts = app.host.posts().filter((post) => post.name === "app.fixture.state");
  return posts[posts.length - 1].data as FixtureState;
}
export async function namedWorkspace(name: string, windowId: string, width = 1280) {
  const app = await open("./Workspace.fixture.tsx", {
    platform: "macos", width, height: 800,
    host: { params: { windowId: `w1-${windowId}`, fixtureName: name }, handlers: { "app.fixture.state": null, "app.fixture.flushed": null, "window.open": null } },
  });
  await app.waitFor("id:workspace-dock");
  return app;
}
export function layout() {
  return {
    layout: { split: { axis: "side_by_side", ratio: 0.27, first: { pane: 1 }, second: { split: { axis: "stacked", ratio: 0.5, first: { pane: 2 }, second: { pane: 3 } } } } },
    active_pane: 3, main_pane: 2, panels_initialized: true, locked_panes: [1],
    panes: {
      "1": { tabs: [{ kind: "panel", panel: "projects" }, { kind: "panel", panel: "files" }], active_tab_key: "panel:projects" },
      "2": { tabs: [{ kind: "shell", shell_id: "S1" }, { kind: "shell", shell_id: "S2" }], active_tab_key: "shell:S1" },
      "3": { tabs: [{ kind: "shell", shell_id: "S3" }, { kind: "panel", panel: "tasks" }], active_tab_key: "shell:S3" },
    },
  };
}
export async function workspace(name: string, restore: unknown = layout(), width = 1280, windowId = name) {
  const app = await open("./Workspace.fixture.tsx", {
    platform: "macos", width, height: 800,
    host: { params: { windowId: `w1-${windowId}`, projectId: `w1-${name}`, restore }, handlers: { "app.fixture.state": null, "app.fixture.flushed": null, "window.open": null } },
  });
  await app.waitFor("id:workspace-dock");
  return app;
}
