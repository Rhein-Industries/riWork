import { expect, open, test } from "nsh/test";
import { options, settings } from "./appearance-options";

test("P1-STAT-01: rendered status text scales and its right side scrolls at narrow widths", async () => {
  const app = await open("./Appearance.fixture.tsx", options("status-rendered-metrics"));
  try {
    await app.waitFor("text:0 LIVE");
    expect(app.spanStyle("text:0 LIVE").fontSize).toBe(9);
    await settings(app, { theme: "native" });
    await app.waitFor("text:0 live");
    expect(Math.round(app.spanStyle("text:0 live").fontSize * 100) / 100).toBe(10.64);
    await app.setViewport(420, 500);
    await app.scroll("id:status-right", 1000);
    await app.press("id:status-item-layout");
    expect(app.texts()).toContain("Default layout");
  } finally { app.dispose(); }
});
