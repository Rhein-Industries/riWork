import { expect, open, test, type App } from "nsh/test";
import type { CreateRequest } from "./model";

const ID = "33333333-3333-4333-8333-333333333333";
const options = { platform: "macos" as const, width: 1200, height: 1800,
  host: { handlers: { "app.fixture.schedules.state": null } } };
async function inspect(app: App) {
  const count = app.host.posts().length;
  await app.host.send("app.fixture.schedules.inspect", {});
  await app.waitUntil(() => app.host.posts().length > count);
  return app.host.posts().filter((p) => p.name === "app.fixture.schedules.state").slice(-1)[0].data as { calls: string[]; requests: CreateRequest[] };
}

test("P6B-SCHED-PARITY-01 synchronous validation keeps the current minutes input focused", async () => {
  const app = await open("./Automations.fixture.tsx", options);
  try {
    await app.waitFor("id:automation-new"); await app.press("id:automation-new");
    await app.type("id:automation-title", "Native fixture"); await app.type("id:automation-prompt", "Inert prompt");
    await app.press("id:automation-repeat-minutes");
    await app.clear("id:automation-minutes"); await app.type("id:automation-minutes", "0");
    await app.keyboard.press("mod+s"); await app.waitFor("id:automation-error");
    await app.keyboard.press("7");
    expect(app.value("id:automation-minutes")).toBe("07");
    expect(app.value("id:automation-title")).toBe("Native fixture");
    expect((await inspect(app)).calls).toEqual([]);
  } finally { app.dispose(); }
});

test("P6B-SCHED-PARITY-02 asynchronous save rejection keeps the current prompt input focused", async () => {
  const app = await open("./Automations.fixture.tsx", options);
  try {
    await app.waitFor(`id:automation-row-${ID}`); await app.press(`id:automation-edit-${ID}`);
    await app.clear("id:automation-prompt"); await app.type("id:automation-prompt", "Retained prompt");
    await app.host.send("app.fixture.schedules.conflict", {});
    await app.host.send("app.fixture.schedules.hold", {});
    await app.keyboard.press("mod+s");
    expect(app.enabled("id:automation-save")).toBe(false);
    await app.host.send("app.fixture.schedules.release", {});
    await app.waitFor("id:automation-error");
    await app.waitUntil(() => app.enabled("id:automation-save"));
    await app.keyboard.press("x");
    expect(app.value("id:automation-prompt")).toBe("Retained promptx");
    expect(app.value("id:automation-title")).toBe("Pinned fixture");
    expect((await inspect(app)).calls).toEqual(["update:4"]);
  } finally { app.dispose(); }
});

test("P6B-SCHED-PARITY-03 Muse is offered for fresh chats with Fast off", async () => {
  const app = await open("./Automations.fixture.tsx", options);
  try {
    await app.waitFor("id:automation-new"); await app.press("id:automation-new");
    await app.press("id:automation-provider-muse");
    await app.type("id:automation-title", "Muse fixture"); await app.type("id:automation-prompt", "Inert prompt");
    await app.press("id:automation-save"); await app.waitUntil(() => !app.shows("id:automation-editor"));
    const state = await inspect(app);
    expect(state.calls).toEqual(["createChat"]);
    expect(state.requests[0].options?.provider).toBe("muse");
    expect(state.requests[0].options?.fast).toBe(false);
    expect(state.requests[0].options?.accountId).toBe(null);
  } finally { app.dispose(); }
});

test("P6B-SCHED-PARITY-04 pinned NSH focus API cannot traverse action controls", async () => {
  const app = await open("./TraversalCapability.fixture.tsx", options);
  try {
    await app.type("id:capability-input", "a");
    for (const target of ["button", "radio", "view"]) {
      await app.host.send("app.fixture.schedules.focus", target);
      await app.keyboard.press("x");
    }
    expect(app.value("id:capability-input")).toBe("axxx");
    // Positive control: the same ref.focus call DOES move to another Input.
    await app.host.send("app.fixture.schedules.focus", "other-input");
    await app.keyboard.press("y");
    expect(app.value("id:capability-other-input")).toBe("y");
    expect(app.value("id:capability-input")).toBe("axxx");
  } finally { app.dispose(); }
});
