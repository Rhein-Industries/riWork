import { expect, open, test } from "nsh/test";
import { effectiveWindowBindings } from "../keyboard";

test("custom keys replace all cycle aliases and keep native app commands out of the UI", () => {
  const bindings = effectiveWindowBindings({ nextTab: "mod+k", closeTab: null, quit: "mod+shift+q" });
  expect(bindings.filter(([, id]) => id === "nextTab")).toEqual([["mod+k", "nextTab"]]);
  expect(bindings.some(([, id]) => id === "closeTab" || id === "quit")).toBe(false);
});

test("custom keys rebind a live dispatcher, remove the old chord, and restore defaults", async () => {
  const app = await open("./CustomKeys.fixture.tsx");
  try {
    await app.waitFor("id:key-result");
    await app.keyboard.press("mod+t");
    expect(app.texts()).toContain("new:1");
    await app.host.send("app.fixture.keys", { newTab: "mod+k", closeTab: null });
    await app.keyboard.press("mod+t");
    await app.keyboard.press("mod+w");
    expect(app.texts()).toContain("new:1");
    await app.keyboard.press("mod+k");
    expect(app.texts()).toContain("new:2");
    await app.host.send("app.fixture.keys", {});
    await app.keyboard.press("mod+k");
    expect(app.texts()).toContain("new:2");
    await app.keyboard.press("mod+t");
    expect(app.texts()).toContain("new:3");
    await app.keyboard.press("mod+w");
    expect(app.texts()).toContain("close:4");
  } finally { app.dispose(); }
});
