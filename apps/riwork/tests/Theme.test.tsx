import { expect, open, test } from "nsh/test";
import { DEFAULT_THEME, NATIVE_PALETTE } from "../appearance";
import { options, settings } from "./appearance-options";

test("P1-THM-04: Native light first workspace paint uses its resolved palette", async () => {
  const opts = options("theme-first", "native");
  opts.host.params.appearance = "light";
  const app = await open("./Appearance.fixture.tsx", opts);
  try {
    await app.waitFor("id:workspace");
    // The selected tab reads in the palette's text color, the others in its muted color.
    expect(app.spanStyle("id:tab:panel:files >> text:Files").color).toBe("#1d1d1fff");
    expect(app.spanStyle("id:tab:panel:projects >> text:Projects").color).toBe("#636366ff");
  } finally { app.dispose(); }
});

test("P1-THM-04: a workspace without Appearance uses the Native light context palette", async () => {
  const app = await open("./Workspace.fixture.tsx", { platform: "macos", width: 1220, height: 780,
    host: { params: { windowId: "theme-before-appearance", projectId: "fixture-project" }, handlers: { "app.fixture.state": null } } });
  try {
    await app.waitFor("id:workspace-dock");
    // Native is the default palette, so the context before Appearance is Native light. The
    // selected tab reads in its text color; which one is selected depends on the saved layout.
    await app.press("id:tab:panel:projects");
    await app.waitUntil(() => app.spanStyle("id:tab:panel:projects >> text:Projects").color === "#1d1d1fff");
    expect(DEFAULT_THEME.palette).toBe(NATIVE_PALETTE);
    expect(DEFAULT_THEME.palette.text).toBe("#1d1d1f");
  } finally { app.dispose(); }
});

test("P1-THM-05: each window resolves Native on appearance events and both receive settings", async () => {
  const app = await open("./Appearance.fixture.tsx", options("theme-main", "native"));
  const peer = await open("./Appearance.fixture.tsx", options("theme-peer", "native"));
  try {
    await app.waitFor("id:workspace"); await peer.waitFor("id:workspace");
    await app.setColorScheme("dark");
    await peer.setColorScheme("dark");
    await app.waitUntil(() => app.spanStyle("id:tab:panel:files >> text:Files").color === "#f5f5f7ff");
    await peer.waitUntil(() => peer.spanStyle("id:tab:panel:files >> text:Files").color === "#f5f5f7ff");
    await settings(app, { theme: "ri_work" });
    await app.waitUntil(() => app.spanStyle("id:tab:panel:files >> text:Files").color === "#d3e1e6ff");
    await peer.waitUntil(() => peer.spanStyle("id:tab:panel:files >> text:Files").color === "#d3e1e6ff");
    // A palette changes colors only: the labels keep their sentence case.
    expect(app.shows("id:tab:panel:projects >> text:Projects")).toBe(true);
    expect(peer.shows("id:tab:panel:projects >> text:Projects")).toBe(true);
  } finally { peer.dispose(); app.dispose(); }
});

test("P1-THM-01: Settings offers the fourteen core-labelled palettes and changes selection", async () => {
  const app = await open("./Appearance.fixture.tsx", options("theme-choices"));
  try {
    await app.waitFor("id:workspace");
    await app.rightClick("id:tab:panel:files");
    await app.chooseMenuItem("Views > Settings");
    await app.waitFor("id:appearance-settings");
    // Follow Ghostty, Native and the twelve fixed palettes of color-palettes.json.
    expect(app.count("id:settings-theme-grid >> role:radio")).toBe(14);
    await app.press("id:theme-choice-native");
    // Settings is the selected tab now, so it reads in the palette's text color.
    await app.waitUntil(() => app.spanStyle("id:tab:panel:settings >> text:Settings").color === "#1d1d1fff");
  } finally { app.dispose(); }
});

test("P1-THM-05: theme changes keep panels mounted and install settings with their resolved palette", async () => {
  const opts = options("theme-stable-owner");
  const app = await open("./Appearance.fixture.tsx", { ...opts, host: { ...opts.host,
    params: { ...opts.host.params, diagnostics: true }, handlers: { ...opts.host.handlers, "app.fixture.theme": null } } });
  try {
    await app.waitFor("id:mount:panel:files");
    expect(app.texts()).toContain("mount 1");
    await settings(app, { theme: "native" });
    await app.waitUntil(() => app.spanStyle("id:tab:panel:files >> text:Files").color === "#1d1d1fff");
    await app.setColorScheme("dark");
    await app.waitUntil(() => app.spanStyle("id:tab:panel:files >> text:Files").color === "#f5f5f7ff");
    await settings(app, { theme: "hermes" });
    await app.waitUntil(() => app.spanStyle("id:tab:panel:files >> text:Files").color === "#ecdcc7ff");
    await settings(app, { theme: "ri_work" });
    await app.waitUntil(() => app.spanStyle("id:tab:panel:files >> text:Files").color === "#d3e1e6ff");
    expect(app.texts()).toContain("mount 1");
    expect(app.texts().some((text) => /^mount [2-9]/.test(text))).toBe(false);
    const paints = app.host.posts().filter((post) => post.name === "app.fixture.theme").map((post) => post.data as { selected: string; text: string });
    expect(paints.length).toBeGreaterThan(3);
    for (const paint of paints) {
      if (paint.selected === "native") expect(["#1d1d1f", "#f5f5f7"]).toContain(paint.text);
      else expect(paint.text).toBe(paint.selected === "hermes" ? "#ecdcc7" : "#d3e1e6");
    }
  } finally { app.dispose(); }
});

test("real project panels and editors use the resolved theme and interface text size", async () => {
  const app = await open("./Appearance.fixture.tsx", options("theme-real-panels", "native"));
  try {
    // The status bar's project item opens Project settings; the Projects tab shows the panel.
    await app.waitFor("id:tab:panel:projects");
    await app.press("id:tab:panel:projects");
    await app.waitFor("id:projects-panel");
    const header = "id:projects-panel >> text:Projects";
    expect(app.spanStyle(header).color).toBe("#1d1d1fff");
    expect(Math.round(app.spanStyle(header).fontSize * 100) / 100).toBe(15.36);
    await app.press("id:new-folder");
    await app.waitFor("id:panel-editor");
    expect(app.spanStyle("id:panel-editor >> text:New folder").color).toBe("#1d1d1fff");
    await app.press("label:Cancel");
    // The interface font, not the palette, sets the face: 12 pt in the system face draws at 14.
    await settings(app, { theme: "ri_work", uiTextSize: 12 });
    await app.waitUntil(() => app.spanStyle(header).color === "#d3e1e6ff");
    expect(Math.round(app.spanStyle(header).fontSize * 100) / 100).toBe(16.55);
  } finally { app.dispose(); }
});
