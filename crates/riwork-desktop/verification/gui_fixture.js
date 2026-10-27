// A source bundle loaded only by gui_acceptance.py into its disposable home.
// The script substitutes this literal; no filesystem capability is granted.
const FIXTURE = __FIXTURE_JSON__;

// This is already JavaScript, so use the renderer ABI emitted by NSH's JSX
// compiler. `nsh/jsx-runtime` exists only for TypeScript JSX typing.
import { component } from "nsh:rt";
import { Text, View, host, mount, signal } from "nsh";
import { Button } from "nsh/base";
import { Window } from "nsh/desktop";
import "riwork/native";

const params = host.params;
const count = signal(0);
const status = signal("Checking host bridge…");
const title = `ISOLATED NSH ${params.kind} ${params.windowId}`;
const record = (kind, data = {}) => console.warn(
  "RIWORK_NSH_FIXTURE " + JSON.stringify({ kind, windowId: params.windowId, ...data }),
);
const check = (condition, message) => {
  if (!condition) throw new Error(message);
};
const failed = (error) => {
  status.set(`FAILED: ${String(error)}`);
  record("failed", { message: String(error) });
};

Window.setTitle(title);
Window.setTitleBar("hidden");
mount(() => component(View, {
  style: { flex: 1, padding: 40, gap: 14, backgroundColor: "#f4f7fb" },
  children: [
    component(Text, { style: { fontSize: 24, fontWeight: "bold" }, children: `Isolated ${params.kind} window` }),
    component(Text, { children: "AppKit / NSH Phase 1b acceptance fixture" }),
    component(Text, { children: `Window ID: ${params.windowId}` }),
    component(Text, { children: `Project ID: ${params.projectId ?? "null (global)"}` }),
    component(Text, { children: `Path: ${params.path}` }),
    component(Text, { children: () => status() }),
    component(Button, {
      onPress: () => count.set(count() + 1),
      children: component(Text, { children: () => `Isolated counter: ${count()}` }),
    }),
  ],
}));

let resolveRoute;
const receivedRoute = new Promise((resolve) => { resolveRoute = resolve; });
host.on("route", ({ step }) => {
  record("routeReceived", { step });
  resolveRoute(step);
});
host.on("global.show", (data) => record("globalShow", { data }));
host.on("command", (data) => record("command", { data }));

async function verify() {
  check(params.schema === 1, "host.params schema is not 1");
  check(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(params.windowId), "windowId is not UUID v4");
  check(params.appearance === "system", "unexpected initial appearance");
  check(params.restore === undefined, "fresh fixture unexpectedly restored state");
  check(params.build.version === "0.1.0", "unexpected application version");
  check(params.kind === "project" || params.kind === "global", "invalid window kind");
  if (params.kind === "global") {
    check(params.projectId === null, "global window has a projectId");
    check(params.path === `${FIXTURE.stateHome}/global-window`, "global path mismatch");
    check(params.global.view === "dashboard" && params.global.start === false, "global startup options mismatch");
    record("params", { params });
    status.set("Global params verified · independent AppView");
    host.post("app.restoreReady", {});
    return;
  }
  check(params.projectId === FIXTURE.projectId, "projectId mismatch");
  check(params.path === FIXTURE.projectPath, "project path mismatch");
  record("params", { params });
  const opened = await host.call("window.open", { kind: "global", view: "dashboard", start: false });
  check(opened.created === true && opened.windowId !== params.windowId, "global window was not newly created with a distinct ID");
  record("opened", { result: opened });
  const singleton = await host.call("window.open", { kind: "global", view: "dashboard", start: false });
  check(singleton.created === false && singleton.windowId === opened.windowId, "global singleton was not reused");
  record("singleton", { result: singleton });
  const windows = await host.call("window.list", {});
  check(windows.length === 2, "window.list must contain exactly two windows");
  check(windows.some((w) => w.windowId === params.windowId && w.kind === "project" && w.projectId === FIXTURE.projectId), "project missing from window.list");
  check(windows.some((w) => w.windowId === opened.windowId && w.kind === "global" && w.projectId === null), "global missing from window.list");
  record("listed", { windows });
  const placement = await host.call("window.placement", {});
  check(["windowed", "maximized", "fullscreen"].includes(placement.mode), "invalid placement mode");
  for (const key of ["frame", "normalFrame", "visibleFrame"]) {
    check(placement[key].width > 0 && placement[key].height > 0, `${key} is empty`);
  }
  record("placement", { placement });
  const route = await host.call("window.route", { projectId: params.projectId, step: { kind: "projectMemo" } });
  check(route.windowId === params.windowId, "route opened or selected another project window");
  check((await receivedRoute).kind === "projectMemo", "route event payload mismatch");
  record("routed", { result: route });
  host.post("app.restoreReady", {});
  status.set("PASS · two engines, distinct params, global singleton, list, placement and route");
  record("checksPassed", { projectWindowId: params.windowId, globalWindowId: opened.windowId });
  // A one-shot fixture lifetime; product subscriptions do not use this timer.
  setTimeout(() => {
    record("quitting");
    host.post("app.quit", {});
  }, FIXTURE.lingerSeconds * 1000);
}

verify().catch(failed);
