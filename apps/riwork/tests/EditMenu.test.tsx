import { expect, open, test } from "nsh/test";

test("P1-MENU-02: host Copy with no selection leaves the clipboard unchanged", async () => {
  const app = await open("../App.tsx", {
    platform: "macos", clipboard: "M5 isolated clipboard sentinel",
    host: { params: { schema: 1, windowId: "m5-edit-menu", kind: "global", projectId: null } },
  });
  try {
    await app.host.send("command", { id: "copy" });
    expect(app.clipboard()).toBe("M5 isolated clipboard sentinel");
  } finally { app.dispose(); }
});
