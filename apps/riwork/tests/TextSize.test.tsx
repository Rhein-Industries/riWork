import { expect, open, test } from "nsh/test";
import { space } from "../appearance";
import { options, settings } from "./appearance-options";

test("P1-TXT-02: View commands resize through the Settings owner and reset", async () => {
  const app = await open("./Appearance.fixture.tsx", options("text-commands"));
  try {
    await app.waitFor("id:workspace");
    await app.host.send("command", { id: "biggerText" });
    await app.waitUntil(() => Math.round(app.spanStyle("text:PROJECTS").fontSize) === 12);
    await app.host.send("command", { id: "actualSize" });
    await app.waitUntil(() => Math.round(app.spanStyle("text:PROJECTS").fontSize) === 11);
    await app.host.send("command", { id: "smallerText" });
    await app.waitUntil(() => Math.round(app.spanStyle("text:PROJECTS").fontSize) === 10);
  } finally { app.dispose(); }
});

test("P1-TXT-03: Native and Hermes use system text and Native accents fall back to Menlo", async () => {
  const opts = options("text-faces", "native");
  opts.host.handlers["system.fonts"] = { families: ["Menlo"] };
  const app = await open("./Appearance.fixture.tsx", opts);
  try {
    await app.waitFor("id:workspace");
    expect(app.texts()).toContain("family:system-ui mono:Menlo");
    expect(Math.round(app.spanStyle("text:Projects").fontSize)).toBe(13);
    await settings(app, { interfaceFont: "system" });
    await app.waitUntil(() => app.texts().includes("family:system-ui mono:system-ui"));
    await settings(app, { theme: "hermes", uiTextMatchesTerminal: false });
    await app.waitUntil(() => app.shows("text:PROJECTS") && Math.round(app.spanStyle("text:PROJECTS").fontSize) === 13);
    await settings(app, { theme: "ri_work" });
    await app.waitUntil(() => app.texts().includes("family:Menlo mono:Menlo"));
  } finally { app.dispose(); }
});

test("P1-TXT-03: spacing grows in whole points and never shrinks chrome", () => {
  expect(space(28, 0.8)).toBe(28);
  expect(space(28, 13 / 11)).toBe(33);
});

test("P1-TXT-03: SF Mono accents are selected when installed and matching suppresses the face offset", async () => {
  const opts = options("text-native-matching", "native");
  opts.host.params.initial.uiTextSize = 18;
  const app = await open("./Appearance.fixture.tsx", opts);
  try {
    await app.waitFor("id:workspace");
    expect(app.texts()).toContain("family:system-ui mono:SF Mono");
    expect(Math.round(app.spanStyle("text:Projects").fontSize)).toBe(20);
    await settings(app, { uiTextMatchesTerminal: true });
    await app.waitUntil(() => Math.round(app.spanStyle("text:Projects").fontSize) === 13);
  } finally { app.dispose(); }
});
