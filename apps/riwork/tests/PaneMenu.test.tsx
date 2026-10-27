import { expect, test } from "nsh/test";
import { layout, workspace } from "./workspace-test-support";

test("P1-PANE-12 pane menu retains disabled deferred rows and locked conditional rows", async () => {
  const app = await workspace("pane-menu", layout(), 640);
  try {
    await app.press("id:pane-menu-1");
    const items = app.menu() ?? [];
    expect(items.some((item) => item.label === "Codex" && item.disabled)).toBe(true);
    expect(items.some((item) => item.label === "Close pane")).toBe(false);
    expect(items.some((item) => item.label === "Unlock pane")).toBe(true);
    expect(items.filter((item) => item.label).map((item) => item.label).slice(0, 5)).toEqual(["Existing sessions", "Existing Claude chats", "Existing Muse chats", "New tab", "Shell"]);
  } finally { app.dispose(); }
});
