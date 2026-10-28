import { expect, open, test, type App } from "nsh/test";
import { GLOBAL_BAR, NO_PROJECT_NOTICE, commandProject } from "../global/GlobalWindow";
import { TERMINAL_LABEL } from "../global/GlobalChat";
import { namedWorkspace, latest } from "./workspace-test-support";

type Calls = { name: string; data: unknown }[];
let windows: unknown[] = [];

/** The production entry as a global window: `kind: "global"`, no project. */
function global(windowId: string, params: Record<string, unknown> = {}) {
  return open("../App.tsx", {
    platform: "macos", width: 1280, height: 800,
    host: {
      params: { schema: 1, windowId, kind: "global", projectId: null, path: "/fixture/global-window", ...params },
      handlers: {
        "window.setDockInfo": null, "app.restoreReady": null, "system.fonts": { families: [] },
        "window.close": {}, "window.list": () => windows, "window.open": { windowId: "opened", created: true },
        "window.route": { windowId: "routed" },
      },
    },
  });
}
const named = (calls: Calls, name: string) => calls.filter((call) => call.name === name).map((call) => call.data);

/** Tests share one native process and its global-window.json: leave Dashboard saved. */
async function restoreDashboard(app: App) {
  await app.press("id:global-view-dashboard");
  await app.waitFor("label:Dashboard, shown");
  await app.wait(300);
}

test("P1-GLB-01 global.show selects the requested view of the one global window", async () => {
  const app = await global("glb-01");
  try {
    await app.waitFor("id:global-view-chat");
    await app.host.send("global.show", { view: "chat", start: false });
    expect(app.shows("label:Chat, shown")).toBe(true);
    expect(app.shows("label:Dashboard")).toBe(true);
    await app.host.send("global.show", { view: "dashboard", start: false });
    expect(app.shows("label:Dashboard, shown")).toBe(true);
    await app.wait(300);
  } finally { app.dispose(); }
});

test("P1-GLB-02 the Dock lists the global window as Global with no branch", async () => {
  const app = await global("glb-02");
  try {
    await app.waitFor("id:global-bar");
    expect(named(app.host.posts(), "window.setDockInfo")).toEqual([{ project: "Global", branch: "" }]);
  } finally { app.dispose(); }
});

test("P1-GLB-03 a view switch is saved and a second global app reopens on it; search is not saved", async () => {
  const app = await global("glb-03a");
  try {
    await app.waitFor("id:global-view-chat");
    await app.press("id:global-view-chat");
    await app.wait(300);
  } finally { app.dispose(); }
  const second = await global("glb-03b");
  try {
    await second.waitFor("label:Chat, shown");
    await restoreDashboard(second);
    await second.waitFor("id:dashboard-search");
    expect(second.value("id:dashboard-search") ?? "").toBe("");
  } finally { second.dispose(); }
});

test("P1-GLB-04 handed-over restore state wins over the file and confirms once", async () => {
  const app = await global("glb-04", { restore: { global: { view: "chat", showQuiet: false, dashboardFilter: "active", dashboardSort: "priority" } } });
  try {
    await app.waitFor("label:Chat, shown");
    await app.wait(200);
    expect(app.shows("label:Chat, shown")).toBe(true);
    expect(named(app.host.posts(), "app.restoreReady").length).toBe(1);
    await restoreDashboard(app);
  } finally { app.dispose(); }
});

test("P1-GLB-05 the global window belongs to no project and asks nothing of one", async () => {
  const app = await global("glb-05");
  try {
    await app.waitFor("id:global-bar");
    await app.wait(500);
    expect(app.shows("id:workspace-dock")).toBe(false);
    const asked = app.host.calls().map((call) => call.name);
    for (const name of ["window.route", "window.open", "window.close"]) expect(asked.includes(name)).toBe(false);
    expect(named(app.host.posts(), "app.restoreReady").length).toBe(0);
  } finally { app.dispose(); }
});

test("P1-GLB-06 the bar: Global in magenta, Dashboard | Chat, a drag region, This Mac · every project", async () => {
  const app = await global("glb-06");
  try {
    await app.waitFor("id:global-bar");
    expect(app.texts().some((text) => text === "Global" || text === "GLOBAL")).toBe(true);
    expect(app.texts()).toContain("This Mac · every project");
    expect(app.shows("id:global-view-dashboard")).toBe(true);
    expect(app.shows("id:global-view-chat")).toBe(true);
    expect(app.shows("id:global-drag")).toBe(true);
    expect(GLOBAL_BAR).toEqual({ height: 34, insetWithControls: 92, inset: 10 });
    await app.press("id:global-view-chat");
    expect(app.shows("label:Chat, shown")).toBe(true);
    await restoreDashboard(app);
  } finally { app.dispose(); }
});

test("P1-GLB-07 ⌘W right after open closes the global window", async () => {
  const app = await global("glb-07");
  try {
    await app.keyboard.press("mod+w");
    expect(named(app.host.calls(), "window.close")).toEqual([{}]);
  } finally { app.dispose(); }
});

test("P1-GLB-08 ⌘⇧N, Dock New Window, ⌘, and ⌘⇧S go to the frontmost local project window's project", async () => {
  expect(commandProject([], null)).toBe(null);
  expect(NO_PROJECT_NOTICE).toBe("Open one of this Mac's projects first.");
  windows = [
    { windowId: "g", kind: "global", projectId: null, focusSerial: 9 },
    { windowId: "a", kind: "project", projectId: "older", focusSerial: 3 },
    { windowId: "b", kind: "project", projectId: "newer", focusSerial: 5 },
  ];
  const app = await global("glb-08");
  try {
    await app.waitFor("id:global-bar");
    await app.keyboard.press("mod+shift+n");
    await app.waitUntil(() => named(app.host.calls(), "window.open").length === 1);
    expect(named(app.host.calls(), "window.open")).toEqual([{ kind: "project", projectId: "newer" }]);
    await app.host.send("command", { id: "newWindow" });
    await app.waitUntil(() => named(app.host.calls(), "window.open").length === 2);
    await app.keyboard.press("mod+,");
    await app.keyboard.press("mod+shift+s");
    await app.waitUntil(() => named(app.host.calls(), "window.route").length === 2);
    expect(named(app.host.calls(), "window.route")).toEqual([
      { projectId: "newer", step: { kind: "panel", panel: "settings" } },
      { projectId: "newer", step: { kind: "panel", panel: "schedules" } },
    ]);
    // With no project window, the store's active project; nothing is registered.
    windows = [];
    await app.keyboard.press("mod+shift+n");
    await app.waitUntil(() => named(app.host.calls(), "window.open").length === 3);
    const third = named(app.host.calls(), "window.open")[2] as { kind: string; projectId: string };
    expect(third.kind).toBe("project");
    expect(/^[0-9a-f-]{36}$/.test(third.projectId)).toBe(true);
  } finally { windows = []; app.dispose(); }
});

test("P1-GLB-09 a route event selects its panel in the window that showed the project", async () => {
  const app = await namedWorkspace("Phase5", "glb-09");
  try {
    await app.host.send("route", { step: { panel: "settings" } });
    await app.host.send("app.fixture.inspect", {});
    const pane = latest(app).snapshot.panes.find((pane) => pane.tabs.some((tab) => tab.id === "panel:settings"));
    expect(pane?.activeTab).toBe("panel:settings");
    expect(latest(app).snapshot.activePane).toBe(pane?.id);
  } finally { app.dispose(); }
});

test("P1-GLB-10 the bottom notice bar shows a startup notice in gold and Dismiss removes it", async () => {
  const notice = "Cannot open project folder /missing: No such file or directory";
  const app = await global("glb-10", { notice });
  try {
    await app.waitFor("id:global-notice-dismiss");
    expect(app.shows(`label:${notice}`)).toBe(true);
    await app.press("id:global-notice-dismiss");
    expect(app.shows("id:global-notice")).toBe(false);
  } finally { app.dispose(); }
});

test("P5-GLOBAL-01 the dashboard draws projects from store facts with a live freshness line", async () => {
  const app = await global("p5-global-01");
  try {
    await app.waitFor("id:dashboard-summary", { timeout: 15_000 });
    expect(app.shows("id:dashboard-freshness")).toBe(true);
    expect(app.texts()).toContain("Live");
    await app.press("id:dashboard-filter-all");
    expect(app.shows("label:Showing All")).toBe(true);
    expect(app.shows("text:L-NAV")).toBe(true);
    expect(app.shows("text:L-NESTED")).toBe(true);
    // A refresh is explicit; it reads again and keeps the window as it is.
    await app.press("id:dashboard-refresh");
    await app.wait(200);
    expect(app.shows("id:dashboard-summary")).toBe(true);
  } finally { app.dispose(); }
});

test("P5-GLOBAL-02 filter and order are the window's own and survive a reopen", async () => {
  const app = await global("p5-global-02a");
  try {
    await app.waitFor("id:dashboard-sorts", { timeout: 15_000 });
    await app.press("id:dashboard-filter-all");
    await app.press("id:dashboard-sort-name");
    await app.wait(300);
  } finally { app.dispose(); }
  const second = await global("p5-global-02b");
  try {
    await second.waitFor("label:Ordered by Name", { timeout: 15_000 });
    expect(second.shows("label:Showing All")).toBe(true);
    await second.press("id:dashboard-filter-active");
    await second.press("id:dashboard-sort-priority");
    await second.wait(300);
  } finally { second.dispose(); }
});

test("P5-GLOBAL-03 a search narrows the table and Clear brings everything back", async () => {
  const app = await global("p5-global-03");
  try {
    await app.waitFor("id:dashboard-filters", { timeout: 15_000 });
    await app.press("id:dashboard-filter-all");
    await app.type("id:dashboard-search", "L-NESTED");
    expect(app.shows("text:L-NESTED")).toBe(true);
    expect(app.shows("text:L-NAV")).toBe(false);
    await app.press("id:dashboard-clear-search");
    expect(app.shows("text:L-NAV")).toBe(true);
    await app.press("id:dashboard-filter-active");
    await app.wait(300);
  } finally { app.dispose(); }
});

test("P5-GLOBAL-04 the Chat view shows the terminal orchestrator there is, as itself", async () => {
  const app = await global("p5-global-04");
  try {
    await app.waitFor("id:dashboard-summary", { timeout: 15_000 });
    await app.press("id:global-view-chat");
    await app.waitFor(`text:${TERMINAL_LABEL}`);
    // Nothing was started for it: a live terminal is never replaced by a chat.
    expect(app.shows("id:global-chat-start")).toBe(false);
    await restoreDashboard(app);
  } finally { app.dispose(); }
});
