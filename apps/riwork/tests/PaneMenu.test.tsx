import { expect, test } from "nsh/test";
import { layout, workspace } from "./workspace-test-support";

test("P1-PANE-12 the pane menu keeps common actions on top and groups the rest; a locked pane cannot be closed from it", async () => {
  const app = await workspace("pane-menu", layout());
  try {
    await app.press("id:pane-menu-1");
    const items = app.menu() ?? [];
    const top = items.map((item) => item.label).filter((label) => !label.includes(" > "));
    expect(top).toEqual(["Shell", "New chat…", "Split right", "Split down"]);
    const groups = [...new Set(items.map((item) => item.label).filter((label) => label.includes(" > ")).map((label) => label.split(" > ")[0]))];
    expect(groups).toEqual(["New", "Sessions", "Orchestrators", "Views", "Pane"]);
    for (const label of ["New > Codex", "New > Muse", "Sessions > Existing sessions", "Sessions > Hand off…", "Orchestrators > Global", "Views > Flows", "Views > Settings"]) {
      expect(items.some((item) => item.label === label)).toBe(true);
    }
    // Pane 1 is locked: it cannot be closed from here. At this width its header has the lock
    // button, so the menu leaves out its Unlock row.
    expect(items.some((item) => item.label === "Pane > Close pane")).toBe(false);
    expect(items.some((item) => item.label === "Pane > Main pane")).toBe(true);
    expect(items.some((item) => item.label === "Pane > Unlock pane")).toBe(false);
    await app.chooseMenuItem("Views > Flows");
    await app.waitFor("id:panel:panel:flows");
  } finally { app.dispose(); }
});
