import { expect, open, test, type App } from "nsh/test";
import { FOOTNOTE, type CreateRequest, type UpdateRequest } from "./model";

const ID = "33333333-3333-4333-8333-333333333333", TREE = "55555555-5555-4555-8555-555555555555";
const options = { platform: "macos" as const, width: 1200, height: 1600, host: { handlers: { "app.fixture.schedules.state": null } } };
interface State { calls: string[]; requests: (CreateRequest | UpdateRequest)[]; opened: string[]; subscribes: number; closes: number; listeners: number; visible: boolean; project: string }
async function inspect(app: App): Promise<State> {
  const count = app.host.posts().length; await app.host.send("app.fixture.schedules.inspect", {});
  await app.waitUntil(() => app.host.posts().length > count);
  return app.host.posts().filter((post) => post.name === "app.fixture.schedules.state").slice(-1)[0].data as State;
}
// The injected fixture API records calls in memory. These tests cannot reach the
// native schedule store, ticker, tmux, provider, credentials or network.
test("P6B-SCHED-UI-01 visible subscriptions close and reopen; real status and earlier chats remain visible", async () => {
  const app = await open("./Automations.fixture.tsx", options);
  try {
    await app.waitFor(`id:automation-row-${ID}`);
    expect(app.texts()).toContain("1 automation"); expect(app.texts()).toContain(FOOTNOTE);
    expect(app.texts()).toContain("LAST fixture observed · submitted · due fixture due");
    await app.press(`id:automation-created-chat-${ID}`);
    await app.press("id:automation-project-chat-earlier-fixture-chat");
    expect((await inspect(app)).opened).toEqual(["created-fixture-chat", "earlier-fixture-chat"]);
    await app.press("id:fixture-toggle-visible");
    expect((await inspect(app)).listeners).toBe(0);
    await app.press("id:fixture-toggle-visible");
    const state = await inspect(app); expect(state.subscribes).toBe(2); expect(state.closes).toBe(1); expect(state.listeners).toBe(1);
    await app.press("id:fixture-runtime-error");
    await app.waitFor("text:SCHEDULER ERROR / Disposable ticker error");
    expect(app.shows(`id:automation-row-${ID}`)).toBe(true);
  } finally { app.dispose(); }
});

test("P6B-SCHED-UI-02 defaults, all permission and repeat controls save once while pending", async () => {
  const app = await open("./Automations.fixture.tsx", options);
  try {
    await app.waitFor(`id:automation-row-${ID}`); await app.press("id:automation-new"); await app.waitFor("id:automation-title");
    expect(app.texts()).toContain("Fast mode: Off"); expect(app.texts()).toContain("Account: fake-account");
    await app.type("id:automation-title", "Review fixture"); await app.type("id:automation-prompt", "Inert automation prompt");
    await app.press("id:automation-permission-plan"); await app.press("id:automation-repeat-minutes");
    await app.clear("id:automation-minutes"); await app.type("id:automation-minutes", "17");
    await app.keyboard.press("mod+shift+s"); expect((await inspect(app)).calls).toEqual([]);
    await app.host.send("app.fixture.schedules.hold", {}); await app.press("id:automation-save");
    expect(app.enabled("id:automation-save")).toBe(false); await app.keyboard.press("mod+s"); await app.keyboard.press("escape");
    const state = await inspect(app); expect(state.calls).toEqual(["createChat"]);
    const request = state.requests[0] as CreateRequest;
    expect(request.options?.provider).toBe("codex"); expect(request.options?.fast).toBe(false); expect(request.options?.permission).toBe("plan");
    expect(request.options?.accountId).toBe("fake-account"); expect(request.everyMinutes).toBe(17); expect(request.binding).toBe(null);
    expect(app.shows("id:automation-editor")).toBe(true);
    await app.host.send("app.fixture.schedules.release", {}); await app.waitUntil(() => !app.shows("id:automation-editor"));
  } finally { app.dispose(); }
});

test("P6B-SCHED-UI-03 scope changes require a worktree and explicit shell; invalid repeat zero never dispatches", async () => {
  const app = await open("./Automations.fixture.tsx", options);
  try {
    await app.waitFor(`id:automation-row-${ID}`); await app.press("id:automation-new");
    await app.type("id:automation-title", "Explicit fixture"); await app.type("id:automation-prompt", "Inert prompt");
    await app.press("id:automation-destination-shell"); await app.press("id:automation-scope-workspace"); await app.press("id:automation-save");
    expect(app.texts().some((text) => text.includes("Select a worktree"))).toBe(true);
    expect((await inspect(app)).calls).toEqual([]);
    await app.press(`id:automation-worktree-${TREE}`); await app.press("id:automation-save");
    expect(app.texts().some((text) => text.includes("Choose an existing target session"))).toBe(true);
    await app.press("id:automation-target-shell-44444444-4444-4444-8444-444444444444");
    await app.press("id:automation-repeat-minutes"); await app.clear("id:automation-minutes"); await app.type("id:automation-minutes", "0"); await app.press("id:automation-save");
    expect((await inspect(app)).calls).toEqual([]);
    await app.clear("id:automation-minutes"); await app.type("id:automation-minutes", "5"); await app.press("id:automation-save");
    await app.waitUntil(() => !app.shows("id:automation-editor"));
    const state = await inspect(app), request = state.requests[0] as CreateRequest;
    expect(state.calls).toEqual(["create"]); expect(request.scope.worktreeId).toBe(TREE); expect(request.binding?.kind).toBe("shell"); expect(request.options).toBe(null);
  } finally { app.dispose(); }
});

test("P6B-SCHED-UI-04 revision conflicts retain the draft; Escape resets two-click delete", async () => {
  const app = await open("./Automations.fixture.tsx", options);
  try {
    await app.waitFor(`id:automation-row-${ID}`); await app.press(`id:automation-edit-${ID}`);
    expect(app.texts()).toContain("PINNED TARGET / 44444444-4444-4444-8444-444444444444");
    await app.clear("id:automation-prompt"); await app.type("id:automation-prompt", "Unsaved fixture edit");
    await app.host.send("app.fixture.schedules.conflict", {}); await app.keyboard.press("mod+s");
    await app.waitFor("id:automation-error"); expect(app.value("id:automation-prompt")).toBe("Unsaved fixture edit");
    expect((await inspect(app)).calls).toEqual(["update:4"]);
    await app.keyboard.press("escape"); await app.waitUntil(() => !app.shows("id:automation-editor"));
    await app.press(`id:automation-delete-${ID}`); expect(app.texts()).toContain("Confirm delete");
    await app.keyboard.press("escape"); expect(app.texts()).toContain("Delete");
    await app.press(`id:automation-delete-${ID}`); await app.press(`id:automation-delete-${ID}`);
    await app.waitUntil(() => !app.shows(`id:automation-row-${ID}`));
    expect((await inspect(app)).calls).toEqual(["update:4", "delete:4"]);
  } finally { app.dispose(); }
});
