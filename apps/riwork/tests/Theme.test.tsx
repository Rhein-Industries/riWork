import { expect, open, test } from "nsh/test";
import { DEFAULT_THEME } from "../appearance";
import { options, settings } from "./appearance-options";

test("P1-THM-04: Native light first workspace paint uses its resolved palette", async () => {
  const opts = options("theme-first", "native");
  opts.host.params.appearance = "light";
  const app = await open("./Appearance.fixture.tsx", opts);
  try {
    await app.waitFor("id:workspace");
    expect(app.spanStyle("id:tab:panel:projects >> text:Projects").color).toBe("#1d1d1fff");
  } finally { app.dispose(); }
});

test("P1-THM-04: a workspace without Appearance uses the RiWork context palette", async () => {
  const app = await open("./Workspace.fixture.tsx", { platform: "macos", width: 1220, height: 780,
    host: { params: { windowId: "theme-before-appearance", projectId: "fixture-project" }, handlers: { "app.fixture.state": null } } });
  try {
    await app.waitFor("id:workspace-dock");
    expect(app.spanStyle("id:tab:panel:projects >> text:PROJECTS").color).toBe("#d3e1e6ff");
    expect(DEFAULT_THEME.palette.text).toBe("#d3e1e6");
  } finally { app.dispose(); }
});

test("P1-THM-05: each window resolves Native on appearance events and both receive settings", async () => {
  const app = await open("./Appearance.fixture.tsx", options("theme-main", "native"));
  const peer = await open("./Appearance.fixture.tsx", options("theme-peer", "native"));
  try {
    await app.waitFor("id:workspace"); await peer.waitFor("id:workspace");
    await app.setColorScheme("dark");
    await peer.setColorScheme("dark");
    await app.waitUntil(() => app.spanStyle("id:tab:panel:projects >> text:Projects").color === "#f5f5f7ff");
    await peer.waitUntil(() => peer.spanStyle("id:tab:panel:projects >> text:Projects").color === "#f5f5f7ff");
    await settings(app, { theme: "ri_work" });
    await app.waitUntil(() => app.shows("text:PROJECTS") && app.spanStyle("id:tab:panel:projects >> text:PROJECTS").color === "#d3e1e6ff");
    await peer.waitUntil(() => peer.shows("text:PROJECTS") && peer.spanStyle("id:tab:panel:projects >> text:PROJECTS").color === "#d3e1e6ff");
  } finally { peer.dispose(); app.dispose(); }
});

test("P1-THM-01: Settings offers the seven core-labelled themes and changes selection", async () => {
  const app = await open("./Appearance.fixture.tsx", options("theme-choices"));
  try {
    await app.waitFor("id:workspace");
    await app.rightClick("id:tab:panel:files");
    await app.chooseMenuItem("Settings");
    await app.waitFor("id:appearance-settings");
    expect(app.count("role:radio")).toBe(7);
    await app.press("id:theme-choice:native");
    await app.waitUntil(() => app.shows("text:Projects") && app.spanStyle("id:tab:panel:projects >> text:Projects").color === "#1d1d1fff");
  } finally { app.dispose(); }
});

test("P1-THM-05: theme changes keep panels mounted and install font/casing with their resolved palette", async () => {
  const opts = options("theme-stable-owner");
  const app = await open("./Appearance.fixture.tsx", { ...opts, host: { ...opts.host,
    params: { ...opts.host.params, diagnostics: true }, handlers: { ...opts.host.handlers, "app.fixture.theme": null } } });
  try {
    await app.waitFor("id:mount:panel:files");
    expect(app.texts()).toContain("mount 1");
    await settings(app, { theme: "native" });
    await app.waitUntil(() => app.shows("text:Projects") && app.spanStyle("id:tab:panel:projects >> text:Projects").color === "#1d1d1fff");
    await app.setColorScheme("dark");
    await app.waitUntil(() => app.spanStyle("id:tab:panel:projects >> text:Projects").color === "#f5f5f7ff");
    await settings(app, { theme: "hermes" });
    await app.waitUntil(() => app.shows("text:PROJECTS") && app.spanStyle("id:tab:panel:projects >> text:PROJECTS").color === "#ecdcc7ff");
    await settings(app, { theme: "ri_work" });
    await app.waitUntil(() => app.spanStyle("id:tab:panel:projects >> text:PROJECTS").color === "#d3e1e6ff");
    expect(app.texts()).toContain("mount 1");
    expect(app.texts().some((text) => /^mount [2-9]/.test(text))).toBe(false);
    const paints = app.host.posts().filter((post) => post.name === "app.fixture.theme").map((post) => post.data as { selected: string; native: boolean; text: string });
    expect(paints.length).toBeGreaterThan(3);
    for (const paint of paints) {
      expect(paint.native).toBe(paint.selected === "native");
      if (paint.selected === "native") expect(["#1d1d1f", "#f5f5f7"]).toContain(paint.text);
      else expect(paint.text).toBe(paint.selected === "hermes" ? "#ecdcc7" : "#d3e1e6");
    }
  } finally { app.dispose(); }
});

test("real project panels and editors use the resolved theme and interface text size", async () => {
  const app = await open("./Appearance.fixture.tsx", options("theme-real-panels", "native"));
  try {
    await app.waitFor("id:status-item-project");
    await app.press("id:status-item-project");
    await app.waitFor("id:projects-panel");
    const header = "id:projects-panel >> text:Projects";
    expect(app.spanStyle(header).color).toBe("#1d1d1fff");
    expect(Math.round(app.spanStyle(header).fontSize * 100) / 100).toBe(15.36);
    await app.press("id:new-folder");
    await app.waitFor("id:panel-editor");
    expect(app.spanStyle("id:panel-editor >> text:New virtual folder").color).toBe("#1d1d1fff");
    await app.press("label:Cancel");
    await settings(app, { theme: "ri_work", uiTextSize: 12 });
    await app.waitUntil(() => app.spanStyle(header).color === "#d3e1e6ff");
    expect(Math.round(app.spanStyle(header).fontSize * 100) / 100).toBe(14.18);
  } finally { app.dispose(); }
});
