import { expect, open, test, type App } from "nsh/test";
import type { DockAreaLayout, DockNode } from "nsh/base";
import type { WorkspaceSnapshot } from "./model";

interface State { snapshot: WorkspaceSnapshot; dock: DockAreaLayout }
function latest(app: App): State {
  const posts = app.host.posts().filter((post) => post.name === "app.fixture.state");
  return posts[posts.length - 1].data as State;
}
function panelIds(node: DockNode): string[] {
  return node.type === "tabs" ? node.panels : node.children.flatMap(panelIds);
}
const options = (windowId: string) => ({
  platform: "macos" as const, width: 1280, height: 800,
  host: { params: { windowId, projectId: "fixture-project" }, handlers: { "app.fixture.state": null, "app.fixture.flushed": null, "app.fixture.geometry": null } },
});

test("opens a core-written layout with shell identity, empty unknown pane, and window chrome", async () => {
  const app = await open("./tests/Workspace.fixture.tsx", options("fixture-one"));
  try {
    await app.waitFor("id:workspace-dock");
    expect(app.shows("id:title-bar")).toBe(true);
    expect(app.shows("id:status-bar")).toBe(true);
    expect(app.texts()).toContain("Sessions");
    expect(app.texts()).toContain("Empty pane");
    const { snapshot, dock } = latest(app);
    expect(snapshot.panes.map((pane) => pane.id)).toEqual([1, 2, 3]);
    expect(snapshot.panes.filter((pane) => pane.locked).map((pane) => pane.id)).toEqual([1, 3]);
    expect(snapshot.panes.find((pane) => pane.main)?.id).toBe(2);
    expect(panelIds(dock.center)).toContain("empty:3");
    expect(panelIds(dock.center)).toContain("shell:first");
    expect(dock.left ?? null).toBe(null);
    expect(dock.right ?? null).toBe(null);
    expect(dock.bottom ?? null).toBe(null);
  } finally { app.dispose(); }
});

test("DockArea selection and keyboard changes go through the session and remain window-local", async () => {
  const app = await open("./tests/Workspace.fixture.tsx", options("fixture-select"));
  const peer = await open("./tests/Workspace.fixture.tsx", options("fixture-peer"));
  try {
    await app.waitFor("id:workspace-dock");
    await peer.waitFor("id:workspace-dock");
    await app.press("id:tab:shell:first");
    expect(latest(app).snapshot.activePane).toBe(2);
    expect(latest(app).snapshot.panes.find((pane) => pane.id === 2)?.activeTab).toBe("shell:first");
    expect(latest(peer).snapshot.panes.find((pane) => pane.id === 2)?.activeTab).toBe("shell:second");
    await app.keyboard.press("mod+9");
    expect(latest(app).snapshot.panes.find((pane) => pane.id === 2)?.activeTab).toBe("shell:second");
    await app.host.send("app.fixture.flush", {});
    await app.waitUntil(() => app.host.posts().some((post) => post.name === "app.fixture.flushed"));
  } finally { peer.dispose(); app.dispose(); }
});

test("locked pixel geometry survives resize and pane menu edits remain in Rust", async () => {
  const app = await open("./tests/Workspace.fixture.tsx", options("fixture-resize"));
  try {
    await app.waitFor("id:workspace-dock");
    const before = latest(app).dock.center;
    await app.setViewport(1600, 900);
    const after = latest(app).dock.center;
    expect(before.type).toBe("split");
    expect(after.type).toBe("split");
    if (before.type === "split" && after.type === "split") expect(after.sizes?.[0]).toBe(before.sizes?.[0]);
    await app.rightClick("id:tab:panel:projects");
    expect(app.menu()?.some((item) => item.label === "Unlock pane")).toBe(true);
    await app.chooseMenuItem("Unlock pane");
    expect(latest(app).snapshot.panes.find((pane) => pane.id === 1)?.locked).toBe(false);
    // Restore fixture before disposal so following tests see its original lock set.
    await app.rightClick("id:tab:panel:projects");
    await app.chooseMenuItem("Lock pane");
  } finally { app.dispose(); }
});

test("production entry routes global windows without opening a project DockArea", async () => {
  const app = await open("./App.tsx", { platform: "macos", host: { params: { schema: 1, windowId: "global-fixture", kind: "global", projectId: null, path: "/fixture/global-window" } } });
  try {
    expect(app.texts()).toContain("Global workspace");
    expect(app.shows("id:workspace-dock")).toBe(false);
    expect(app.host.posts().filter((post) => post.name === "app.restoreReady").length).toBe(0);
  } finally { app.dispose(); }
});

test("production restore installs its native layout before confirming exactly once", async () => {
  const app = await open("./App.tsx", {
    platform: "macos", width: 1280, height: 800,
    host: {
      params: {
        schema: 1, windowId: "production-restore", kind: "project", projectId: "restored-project", path: "/fixture/Restored",
        restore: { layout: {
          layout: { pane: 21 }, active_pane: 21, main_pane: 21, panels_initialized: true, locked_panes: [],
          panes: { "21": { tabs: [{ kind: "shell", shell_id: "restored" }], active_tab_key: "shell:restored" } },
        } },
      },
      handlers: { "app.restoreReady": null },
    },
  });
  try {
    await app.waitFor("id:tab:shell:restored");
    expect(app.host.posts().filter((post) => post.name === "app.restoreReady").length).toBe(1);
    await app.setViewport(1400, 900);
    expect(app.host.posts().filter((post) => post.name === "app.restoreReady").length).toBe(1);
  } finally { app.dispose(); }
});

test("a restored global entry confirms without opening a project session", async () => {
  const app = await open("./App.tsx", {
    platform: "macos",
    host: {
      params: { schema: 1, windowId: "restored-global", kind: "global", projectId: null, path: "/fixture/global-window", restore: {} },
      handlers: { "app.restoreReady": null },
    },
  });
  try {
    expect(app.texts()).toContain("Global workspace");
    expect(app.shows("id:workspace-dock")).toBe(false);
    expect(app.host.posts().filter((post) => post.name === "app.restoreReady").length).toBe(1);
  } finally { app.dispose(); }
});

test("clicking an already selected tab and an empty pane changes the active pane", async () => {
  const app = await open("./tests/Workspace.fixture.tsx", options("fixture-activation"));
  try {
    await app.waitFor("id:workspace-dock");
    await app.press("id:tab:panel:files");
    await app.press("id:tab:shell:second");
    await app.press("id:tab:panel:files");
    expect(latest(app).snapshot.activePane).toBe(1);
    await app.keyboard.press("mod+1");
    expect(latest(app).snapshot.panes.find((pane) => pane.id === 1)?.activeTab).toBe("panel:projects");
    await app.press("id:panel:empty:3");
    expect(latest(app).snapshot.activePane).toBe(3);
  } finally { app.dispose(); }
});

test("a real DockArea drag moves one tab into the opaque empty pane through Rust", async () => {
  const app = await open("./tests/Workspace.fixture.tsx", options("fixture-drag"));
  try {
    await app.waitFor("id:workspace-dock");
    // Files is still a deferred surface. Its construction/visibility markers
    // prove DockArea keeps mounted panel identity while the live panels coexist.
    await app.press("id:tab:panel:files");
    await app.press("id:tab:shell:first");
    expect(app.texts()).toContain("mount 1");
    expect(app.texts()).toContain("visible: true");
    expect(app.texts().includes("mount 2")).toBe(false);
    await app.host.send("app.fixture.measure", {});
    await app.waitUntil(() => app.host.posts().some((post) => post.name === "app.fixture.geometry"));
    type Measurement = { pagePosition: { x: number; y: number }; bounds: { size: { width: number; height: number } } };
    const geometry = app.host.posts().find((post) => post.name === "app.fixture.geometry")!.data as { source: Measurement; destination: Measurement };
    const center = (view: Measurement) => ({ x: view.pagePosition.x + view.bounds.size.width / 2, y: view.pagePosition.y + view.bounds.size.height / 2 });
    const source = center(geometry.source), destination = center(geometry.destination);
    await app.drag("id:tab:shell:first", { x: destination.x - source.x, y: destination.y - source.y });
    const snapshot = latest(app).snapshot;
    expect(snapshot.panes.find((pane) => pane.id === 2)?.tabs.map((tab) => tab.id)).toEqual(["shell:second"]);
    expect(snapshot.panes.find((pane) => pane.id === 3)?.tabs.map((tab) => tab.id)).toEqual(["shell:first"]);
    expect(snapshot.activePane).toBe(3);
    expect(panelIds(latest(app).dock.center)).toContain("empty:3");
    expect(app.texts()).toContain("mount 1");
    expect(app.texts().includes("mount 2")).toBe(false);
    await app.setViewport(1400, 900);
    expect(app.texts().includes("mount 2")).toBe(false);
    await app.host.send("app.fixture.flush", {});
    await app.waitUntil(() => app.host.posts().some((post) => post.name === "app.fixture.flushed"));
  } finally { app.dispose(); }
});
