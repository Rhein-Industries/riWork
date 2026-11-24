import { expect, open, test } from "nsh/test";
import { DEFAULT_CUSTOMIZATION, interfaceGeometry, type InterfaceNode } from "../customization_model";
import { latest, layout } from "./workspace-test-support";
import minimalist from "../mods/minimalist/mod.json";

const sidebar: InterfaceNode = { type: "row", id: "frame", children: [
  { type: "commands", id: "navigation", size: 200, items: [{ command: "nextTab", label: "Next session" }] },
  { type: "workspace", id: "workspace" },
] };

test("CUSTOMIZE geometry keeps the workspace reachable on small windows", () => {
  expect(interfaceGeometry(sidebar, 1000, 700).workspace).toEqual({ x: 200, y: 0, width: 800, height: 700 });
  expect(interfaceGeometry(sidebar, 100, 80).workspace).toEqual({ x: 50, y: 0, width: 50, height: 80 });
  const nested: InterfaceNode = { type: "column", id: "outer", children: [
    { type: "spacer", id: "top", size: 40 }, sidebar,
  ] };
  expect(interfaceGeometry(nested, 1000, 700).workspace).toEqual({ x: 200, y: 40, width: 800, height: 660 });
});

test("CUSTOMIZE live composition changes preserve terminal mounts and session identities", async () => {
  const app = await open("./Workspace.fixture.tsx", {
    platform: "macos", width: 1100, height: 800,
    host: { params: { windowId: "customization-lifetime", projectId: "customization-lifetime", restore: layout(), customize: true },
      handlers: { "app.fixture.state": null, "app.fixture.terminalMount": null, "window.open": null } },
  });
  try {
    await app.waitFor("id:workspace-dock");
    await app.waitFor("id:panel:shell:S3");
    const mounts = () => app.host.posts().filter((post) => post.name === "app.fixture.terminalMount");
    const originalMounts = mounts().length;
    expect(originalMounts > 0).toBe(true);
    const identities = () => latest(app).snapshot.panes.map((pane) => ({ id: pane.id, tabs: pane.tabs.map((tab) => tab.id) }));
    const original = identities();
    const customized = { ...DEFAULT_CUSTOMIZATION, selected: "sidebar", layout: { ...DEFAULT_CUSTOMIZATION.layout, name: "Sidebar", root: sidebar } };
    await app.host.send("app.fixture.customize", customized);
    await app.waitFor("id:customization-command:navigation:nextTab");
    expect(identities()).toEqual(original);
    expect(mounts().length).toBe(originalMounts);
    const before = latest(app).snapshot.panes.find((pane) => pane.id === 3)!.activeTab;
    await app.press("id:customization-command:navigation:nextTab");
    expect(latest(app).snapshot.panes.find((pane) => pane.id === 3)!.activeTab === before).toBe(false);
    const moved: InterfaceNode = { type: "column", id: "frame", children: [
      { type: "commands", id: "navigation", size: 50, items: [{ command: "nextTab", label: "Next session" }] },
      { type: "workspace", id: "workspace" },
    ] };
    await app.host.send("app.fixture.customize", { ...customized, layout: { ...customized.layout, root: moved } });
    expect(identities()).toEqual(original);
    expect(mounts().length).toBe(originalMounts);
    await app.host.send("app.fixture.customize", DEFAULT_CUSTOMIZATION);
    expect(identities()).toEqual(original);
    expect(mounts().length).toBe(originalMounts);
  } finally { app.dispose(); }
});

test("CUSTOMIZE Minimalist workspace mod hides restored tabs and can be disabled in Settings", async () => {
  const restored = layout();
  restored.panes["3"].active_tab_key = "panel:tasks";
  const app = await open("./Workspace.fixture.tsx", {
    platform: "macos", width: 1100, height: 800,
    host: { params: { windowId: "minimalist", projectId: "minimalist", restore: restored, customize: true },
      handlers: { "app.fixture.state": null, "app.fixture.terminalMount": null } },
  });
  try {
    await app.waitFor("id:tab:panel:tasks");
    const before = latest(app).snapshot.panes.find((pane) => pane.id === 3)!;
    expect(before.activeTab).toBe("panel:tasks");
    await app.host.send("app.fixture.customize", { ...DEFAULT_CUSTOMIZATION, workspaceMod: minimalist.id, mods: [minimalist] });
    await app.waitFor("id:workspace-navigation:cli");
    expect(app.shows("id:tab:panel:tasks")).toBe(false);
    expect(app.shows("id:status-bar")).toBe(false);
    expect(latest(app).snapshot.panes.find((pane) => pane.id === 3)).toEqual(before);
    await app.waitFor("id:panel:shell:S3");
    await app.press("id:workspace-navigation:files");
    await app.waitFor("id:files-panel");
    await app.host.send("command", { id: "openPreview" });
    await app.waitFor("id:file-preview-panel");
    expect(app.shows("id:tab:panel:preview")).toBe(false);
    await app.press("id:preview-close");
    expect(app.shows("id:file-preview-panel")).toBe(false);
    await app.press("id:workspace-navigation:projects");
    await app.waitFor("id:panel:panel:projects");
    await app.press("id:workspace-navigation:cli");
    expect(latest(app).snapshot.panes.find((pane) => pane.active)?.activeTab?.startsWith("shell:")).toBe(true);
    await app.press("id:workspace-navigation:settings");
    await app.waitFor("id:settings-panel");
    await app.host.send("command", { id: "openTasks" });
    expect(latest(app).snapshot.panes.find((pane) => pane.active)?.activeTab).toBe("panel:settings");
    await app.press(`id:workspace-mod:${minimalist.id}`);
    expect(app.shows("id:tab:panel:tasks")).toBe(true);
    await app.press(`id:workspace-mod:${minimalist.id}`);
    expect(app.shows("id:tab:panel:tasks")).toBe(false);
  } finally { app.dispose(); }
});

test("CUSTOMIZE window shortcut overrides remove old chords and restore defaults live", async () => {
  const app = await open("./Workspace.fixture.tsx", {
    platform: "macos", width: 1100, height: 800,
    host: { params: { windowId: "customization-keys", projectId: "customization-keys", restore: layout(), customize: true }, handlers: { "app.fixture.state": null } },
  });
  try {
    await app.waitFor("id:workspace-dock");
    await app.host.send("app.fixture.customize", { ...DEFAULT_CUSTOMIZATION, keys: { nextTab: "mod+alt+j" } });
    const active = () => latest(app).snapshot.panes.find((pane) => pane.id === 3)!.activeTab;
    const before = active();
    await app.keyboard.press("ctrl+tab");
    expect(active()).toBe(before);
    await app.keyboard.press("mod+alt+j");
    expect(active() === before).toBe(false);
    const remapped = active();
    await app.host.send("app.fixture.customize", DEFAULT_CUSTOMIZATION);
    await app.keyboard.press("mod+alt+j");
    expect(active()).toBe(remapped);
    await app.keyboard.press("ctrl+tab");
    expect(active()).toBe(before);
  } finally { app.dispose(); }
});
