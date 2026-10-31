import { expect, open, test } from "nsh/test";
import { latest, namedWorkspace } from "./workspace-test-support";

const tabs = (app: Awaited<ReturnType<typeof namedWorkspace>>) => latest(app).snapshot.panes.flatMap((pane) => pane.tabs);

test("IN-PLACE-01 New chat opens a tab of its own that blocks nothing, and Cancel closes it", async () => {
  // A project of the store: New chat needs its folder.
  const app = await namedWorkspace("Alpha", "new-chat-tab");
  try {
    await app.host.send("command", { id: "newChat" });
    await app.waitFor("id:new-chat-setup");
    expect(tabs(app).some((tab) => tab.id.startsWith("chat-pending:"))).toBe(true);
    expect(app.texts()).toContain("New chat");
    // The rest of the window keeps working while the new chat's tab is open.
    await app.host.send("command", { id: "openSettings" });
    expect(app.shows("id:panel:panel:settings")).toBe(true);
    const setup = tabs(app).find((tab) => tab.id.startsWith("chat-pending:"))!;
    await app.press(`id:tab:${setup.id}`);
    await app.press("id:new-chat-cancel");
    await app.waitUntil(() => !tabs(app).some((tab) => tab.id.startsWith("chat-pending:")));
    expect(app.shows("id:new-chat-setup")).toBe(false);
  } finally { app.dispose(); }
});

test("IN-PLACE-02 New chat saves its choices as the defaults the next New chat starts with", async () => {
  const app = await open("./NewChatDefaults.fixture.tsx", { platform: "macos", width: 900, height: 1300 });
  try {
    await app.waitFor("id:new-chat-setup");
    expect(app.texts()).toContain("Save as default");
    await app.press("id:new-chat-provider-claude");
    await app.press("id:new-chat-mode-plan");
    await app.type("id:new-chat-model-input", "fixture-model");
    await app.press("id:new-chat-save-defaults");
    await app.waitUntil(() => app.texts().includes("These are your defaults"));
    await app.host.send("app.fixture.reopen", {});
    await app.waitFor("id:new-chat-setup");
    expect(app.value("id:new-chat-model-input")).toBe("fixture-model");
    expect(app.texts()).toContain("These are your defaults");
    // Choosing differently offers to save again.
    await app.press("id:new-chat-provider-codex");
    expect(app.texts()).toContain("Save as default");
  } finally { app.dispose(); }
});
