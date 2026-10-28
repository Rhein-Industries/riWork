import { expect, open, test, type App } from "nsh/test";
import { cuaPresentation, type CuaState, type CuaStatus } from "../CuaSetup";
import { framePoint, previewViewport } from "../AppPreviewTab";
import type { WorkspaceSnapshot } from "../model";
import { layout } from "./workspace-test-support";

const ready: CuaStatus = { installed: true, version: "fixture", ready: true, running: true, accessibility: true, screen_recording: true, direct_capture_verified: true,
  message: "Cua Driver is ready.", skills: { setup_needed: false, message: "New sessions discover linked skills.", targets: [
    { name: "Codex", state: "linked", label: "linked" }, { name: "Claude", state: "home_absent", label: "no home" }, { name: "Muse", state: "blocked_by_foreign_path", label: "blocked by foreign path" }, { name: "Grok", state: "unsupported", label: "unsupported" },
  ] } };
const state = (change: Partial<CuaStatus> = {}, extra: Partial<CuaState> = {}): CuaState => ({ status: { ...ready, ...change }, pending: null, error: null, ...extra });
const states: [string, CuaState, string, string][] = [
  ["initial", { status: null, pending: null, error: null }, "Set up Cua", "install"],
  ["missing", state({ installed: false, ready: false, running: false, version: null, message: "Cua Driver is not installed. Select Set up Cua or run `riwork setup`." }), "Set up Cua", "install"],
  ["signature", state({ version: null, ready: false, message: "Cua Driver signature does not match. Repair Cua." }), "Repair Cua", "install"],
  ["version", state({ version: null, ready: false, message: "Cannot read the driver version. Repair Cua." }), "Repair Cua", "install"],
  ["service-stopped", state({ running: false, ready: false, message: "Cua Driver service is not running." }), "Repair Cua", "install"],
  ["accessibility", state({ ready: false, accessibility: false, message: "Accessibility permission is needed." }), "Grant macOS access", "permissions"],
  ["recording", state({ ready: false, screen_recording: false, message: "Screen Recording permission is needed." }), "Grant macOS access", "permissions"],
  ["both-permissions", state({ ready: false, accessibility: false, screen_recording: false, message: "Accessibility and Screen Recording are needed." }), "Grant macOS access", "permissions"],
  ["capture-unverified", state({ ready: false, direct_capture_verified: false, message: "Verify direct screen capture." }), "Verify screen capture", "permissions"],
  ["ready", state(), "Check Cua", "check"],
  ["custom-driver", state({ message: "Custom Cua Driver is ready; signature was not verified." }), "Check Cua", "check"],
  ["skill-link", state({ skills: { ...ready.skills, setup_needed: true } }), "Link Cua skill", "install"],
  ["probe-error", state({}, { error: "Cua Driver permissions could not be checked: fixture failure" }), "Check Cua", "check"],
  ["pending", state({}, { pending: "Setting up Cua.ai…", error: "hidden old error" }), "Check Cua", "check"],
];
for (const [name, value, label, action] of states) test(`P6C Cua renders ${name}`, async () => {
  const app = await open("./Phase6c.fixture.tsx", { platform: "macos", width: 900, height: 600, host: { params: { surface: "cua", cua: value, windowId: name }, handlers: { "app.fixture.action": null } } });
  try {
    await app.waitFor("id:cua-setup");
    expect(app.shows(`text:${label}`)).toBe(true);
    expect(app.shows(`text:${cuaPresentation(value).message}`)).toBe(true);
    if (value.status?.installed) expect(app.shows(`text:Accessibility ${value.status.accessibility ? "ready" : "needed"}`)).toBe(true);
    if (name === "ready") for (const target of ready.skills.targets) expect(app.shows(`text:${target.name} ${target.label}`)).toBe(true);
    if (value.pending) expect(app.enabled("id:cua-primary")).toBe(false);
    else await app.press("id:cua-primary");
    const actions = app.host.posts().filter((post) => post.name === "app.fixture.action");
    expect(actions.map((post) => post.data)).toEqual(value.pending ? [] : [{ action }]);
  } finally { app.dispose(); }
});

function latest(app: App): WorkspaceSnapshot { const posts = app.host.posts().filter((post) => post.name === "app.fixture.state"); return posts[posts.length - 1].data as WorkspaceSnapshot; }
async function workspace(id: string, restore?: unknown, worktree = false) {
  const app = await open("./Phase6c.fixture.tsx", { platform: "macos", width: 1280, height: 850, host: { params: { windowId: id, restore, worktree }, handlers: { "app.fixture.state": null, "app.fixture.flushed": null, "app.fixture.cua": null, "app.fixture.cwd": null, "window.open": null } } });
  await app.waitFor("id:workspace-dock");
  return app;
}

test("P6C preview opens in main pane, persists committed spec/title, runs only on Run, restores idle", async () => {
  const app = await workspace("6c-preview-first", layout());
  let id = "";
  try {
    // Wait for the real Store subscription before asking for a default cwd.
    await app.wait(300);
    await app.host.send("command", { id: "appPreview" });
    await app.waitFor("id:app-preview");
    const snapshot = latest(app);
    const pane = snapshot.panes.find((pane) => pane.tabs.some((tab) => tab.kind === "app_preview"))!;
    expect(pane.id).toBe(2);
    expect(snapshot.activePane).toBe(2);
    const tab = pane.tabs.find((tab) => tab.kind === "app_preview")!;
    id = tab.id;
    expect(tab.previewSpec?.mode).toBe("cooperative");
    expect(app.shows("text:idle")).toBe(true);
    await app.type("id:preview-command", "fixture-command");
    expect(latest(app).panes.flatMap((pane) => pane.tabs).find((tab) => tab.id === id)?.previewSpec?.command).toBe("");
    await app.submit("id:preview-command");
    expect(latest(app).panes.flatMap((pane) => pane.tabs).find((tab) => tab.id === id)?.previewSpec?.command).toBe("fixture-command");
    await app.press("id:preview-mode:window_capture");
    expect(latest(app).panes.flatMap((pane) => pane.tabs).find((tab) => tab.id === id)?.previewSpec?.mode).toBe("window_capture");
    expect(app.shows("text:idle")).toBe(true);
    await app.press("id:preview-mode:ios_simulator");
    await app.type("id:preview-ios:scheme", "FixtureScheme");
    await app.clear("id:preview-ios:scheme");
    await app.submit("id:preview-ios:scheme");
    expect(latest(app).panes.flatMap((pane) => pane.tabs).find((tab) => tab.id === id)?.previewSpec?.ios?.scheme).toBe("");
    expect(app.shows("text:idle")).toBe(true);
    await app.press("id:preview-mode:cooperative");
    await app.press("id:preview-run");
    await app.waitFor("text:streaming");
    expect(app.enabled("id:preview-command")).toBe(false);
    expect(latest(app).panes.flatMap((pane) => pane.tabs).find((tab) => tab.id === id)?.previewSpec?.command).toBe("fixture-command");
    expect(latest(app).panes.flatMap((pane) => pane.tabs).find((tab) => tab.id === id)?.title.startsWith("Preview · ")).toBe(true);
    await app.host.send("window.state", { visible: false, appActive: false });
    await app.waitFor("text:paused");
    await app.host.send("window.state", { visible: true, appActive: true });
    await app.waitFor("text:streaming");
    await app.press("id:preview-stop");
    await app.waitFor("text:stopped");
    await app.host.send("app.fixture.flush", {});
    await app.waitUntil(() => app.host.posts().some((post) => post.name === "app.fixture.flushed"));
  } finally { app.dispose(); }
  const restored = await workspace("6c-preview-restored");
  try {
    await restored.waitFor("id:app-preview");
    expect(restored.value("id:preview-command")).toBe("fixture-command");
    expect(restored.shows("text:idle")).toBe(true);
    expect(latest(restored).panes.flatMap((pane) => pane.tabs).some((tab) => tab.id === id)).toBe(true);
    await restored.host.send("command", { id: "closeTab" });
    await restored.waitUntil(() => !restored.shows("id:app-preview"));
  } finally { restored.dispose(); }
});

test("P6C aspect-fit frame mapping rejects letterboxing and clamps captured release", () => {
  expect(framePoint({ x: 10, y: 50 }, { width: 200, height: 100 }, { width: 100, height: 100 })).toBe(null);
  expect(framePoint({ x: 75, y: 50 }, { width: 200, height: 100 }, { width: 100, height: 100 })).toEqual({ x: 25, y: 50 });
  expect(framePoint({ x: 250, y: -20 }, { width: 200, height: 100 }, { width: 100, height: 100 }, true)).toEqual({ x: 99, y: 0 });
});

// Run in its own native process with a missing-driver JSON fixture, so no real
// status/permissions probe can leak into this startup routing assertion.
test("CuaMissing opens Settings in the active pane using fixture status only", async () => {
  const app = await workspace("6c-missing", layout());
  try {
    const checked = app.host.posts().find((post) => post.name === "app.fixture.cua")!.data as CuaState;
    if (checked.status?.installed) { expect(latest(app).panes.find((pane) => pane.id === 3)?.activeTab).toBe("shell:S3"); return; }
    await app.waitFor("id:cua-setup");
    expect(app.shows("text:Set up Cua")).toBe(true);
    expect(latest(app).panes.find((pane) => pane.id === 3)?.activeTab).toBe("panel:settings");
    expect(app.shows("text:Cua Driver is not installed. Select Set up Cua or run `riwork setup`.")).toBe(true);
  } finally { app.dispose(); }
});

test("P6C viewport preserves aspect ratio and protocol bounds", () => {
  expect(previewViewport(3840, 2400)).toEqual({ width: 1920, height: 1200 });
  expect(previewViewport(100, 40)).toEqual({ width: 100, height: 40 });
  expect(previewViewport(12, 400)).toBe(null);
  expect(previewViewport(Infinity, 400)).toBe(null);
});

test("P6C preview defaults to the selected worktree folder", async () => {
  const app = await workspace("6c-worktree", layout(), true);
  try {
    await app.wait(300);
    await app.host.send("command", { id: "appPreview" });
    await app.waitFor("id:app-preview");
    const expected = app.host.posts().find((post) => post.name === "app.fixture.cwd")!.data as { cwd: string };
    expect(app.value("id:preview-cwd")).toBe(expected.cwd);
    expect(app.shows("text:idle")).toBe(true);
  } finally { app.dispose(); }
});
