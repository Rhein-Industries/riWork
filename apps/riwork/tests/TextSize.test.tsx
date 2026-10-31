import { expect, open, test, type App } from "nsh/test";
import { space } from "../appearance";
import { options, settings } from "./appearance-options";

// Tabs draw their 10 px titles at the text size over 11, and the system face draws every
// size 2 pt larger (11 pt at 13): 10 × 13 / 11 = 11.82 at the default size.
const tabSize = (app: App) => Math.round(app.spanStyle("id:tab:panel:projects >> text:Projects").fontSize * 100) / 100;

test("P1-TXT-02: View commands resize through the Settings owner and reset", async () => {
  const app = await open("./Appearance.fixture.tsx", options("text-commands"));
  try {
    await app.waitFor("id:workspace");
    await app.host.send("command", { id: "biggerText" });
    await app.waitUntil(() => tabSize(app) === 12.73);
    await app.host.send("command", { id: "actualSize" });
    await app.waitUntil(() => tabSize(app) === 11.82);
    await app.host.send("command", { id: "smallerText" });
    await app.waitUntil(() => tabSize(app) === 10.91);
  } finally { app.dispose(); }
});

test("P1-TXT-03: every palette uses the interface font's system text and its accents fall back to Menlo", async () => {
  const opts = options("text-faces", "native");
  opts.host.handlers["system.fonts"] = { families: ["Menlo"] };
  const app = await open("./Appearance.fixture.tsx", opts);
  try {
    await app.waitFor("id:workspace");
    expect(app.texts()).toContain("family:system-ui mono:Menlo");
    expect(tabSize(app)).toBe(11.82);
    await settings(app, { interfaceFont: "system" });
    await app.waitUntil(() => app.texts().includes("family:system-ui mono:system-ui"));
    // A palette changes colors only: Hermes and RiWork keep the interface font and its size.
    await settings(app, { theme: "hermes", uiTextMatchesTerminal: false });
    await app.waitUntil(() => app.spanStyle("id:tab:panel:files >> text:Files").color === "#ecdcc7ff");
    expect(tabSize(app)).toBe(11.82);
    await settings(app, { theme: "ri_work" });
    await app.waitUntil(() => app.spanStyle("id:tab:panel:files >> text:Files").color === "#d3e1e6ff");
    expect(app.texts()).toContain("family:system-ui mono:system-ui");
    expect(tabSize(app)).toBe(11.82);
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
    // 18 pt draws at 20 in the system face; matching takes Ghostty's 13 as it is.
    expect(tabSize(app)).toBe(18.18);
    await settings(app, { uiTextMatchesTerminal: true });
    await app.waitUntil(() => tabSize(app) === 11.82);
  } finally { app.dispose(); }
});
