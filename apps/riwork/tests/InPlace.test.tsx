import { expect, test } from "nsh/test";
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
