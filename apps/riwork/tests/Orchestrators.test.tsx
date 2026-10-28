import { expect, test } from "nsh/test";
import { latest, namedWorkspace } from "./workspace-test-support";

// Needs the fixture chat host (RIWORK_NSH_CHAT_FIXTURE=1 on an isolated /tmp home).
test("P5-ORCH-01 ⌘⌥O opens the project's orchestrator as a chat tab, and again reuses it", async () => {
  const app = await namedWorkspace("Phase5", "p5-orch-01");
  try {
    const chats = () => latest(app).snapshot.panes.flatMap((pane) => pane.tabs).filter((tab) => tab.id.startsWith("chat:"));
    await app.host.send("app.fixture.inspect", {});
    const before = chats().map((tab) => tab.id);
    await app.keyboard.press("mod+alt+o");
    await app.waitFor("id:chat-input", { timeout: 20_000 });
    await app.host.send("app.fixture.inspect", {});
    const added = chats().filter((tab) => !before.includes(tab.id));
    expect(added.length).toBe(1);
    const first = added[0].id;
    expect(latest(app).snapshot.panes.find((pane) => pane.tabs.some((tab) => tab.id === first))?.activeTab).toBe(first);
    // One persistent orchestrator per scope: the second request finds the first.
    await app.keyboard.press("mod+alt+o");
    await app.wait(500);
    await app.host.send("app.fixture.inspect", {});
    expect(chats().map((tab) => tab.id).sort()).toEqual([...before, first].sort());
  } finally { app.dispose(); }
});

test("P5-ORCH-02 the pane menu's New tab offers both orchestrators", async () => {
  const app = await namedWorkspace("Phase5", "p5-orch-02");
  try {
    await app.host.send("app.fixture.inspect", {});
    const pane = latest(app).snapshot.activePane;
    await app.press(`id:pane-menu-${pane}`);
    const menu = app.menu() ?? [];
    expect(menu.find((item) => item.label === "Global orchestrator")?.disabled).toBe(false);
    expect(menu.find((item) => item.label === "Project orchestrator")?.disabled).toBe(false);
    await app.closeMenu();
  } finally { app.dispose(); }
});
