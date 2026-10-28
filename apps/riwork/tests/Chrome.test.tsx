import { expect, open, test } from "nsh/test";

test("P1-WIN-06: island remains a drag target until fullscreen", async () => {
  const app = await open("./Chrome.fixture.tsx", { platform: "macos", width: 600, height: 400 });
  try {
    expect(app.shows("id:window-controls-island")).toBe(true);
    expect(app.shows("id:window-drag-handle:1")).toBe(true);
    await app.host.send("window.state", {mode:"fullscreen"});
    expect(app.shows("id:window-controls-island")).toBe(false);
    await app.host.send("window.state", {mode:"windowed"});
    expect(app.shows("id:window-controls-island")).toBe(true);
  } finally { app.dispose(); }
});
