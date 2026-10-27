import { expect, open, test } from "nsh/test";

for (const theme of ["ghostty", "native", "ri_work", "catppuccin", "tokyo_night", "gruvbox_light", "hermes"]) {
  test(`P1-WIN-06 ${theme}: island remains a drag target until fullscreen`, async () => {
    const app = await open("./Chrome.fixture.tsx", { platform: "macos", width: 600, height: 400,
      host: { params: {theme, controlsIsland: theme !== "native" && theme !== "hermes"} } });
    try {
      expect(app.shows("id:window-controls-island")).toBe(true);
      expect(app.shows("id:window-drag-handle:1")).toBe(true);
      await app.host.send("window.state", {mode:"fullscreen"});
      expect(app.shows("id:window-controls-island")).toBe(false);
      await app.host.send("window.state", {mode:"windowed"});
      expect(app.shows("id:window-controls-island")).toBe(true);
    } finally { app.dispose(); }
  });
}
