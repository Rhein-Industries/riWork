import { expect, open, test, type App } from "nsh/test";

// P4 chat tab tests. They need `RIWORK_NSH_CHAT_FIXTURE=1` and an isolated /tmp
// RIWORK_HOME: the native module then runs an in-process chat host whose Codex
// driver replays the engine's recorded provider streams (never a live host).

interface FixtureState { chatId: string | null; rows?: number; cells?: number; constructed?: number; bodies?: number; groups?: string[]; panel?: { state: string; running: boolean; approvals: unknown[] } }

async function chat(recording: string, chatId?: string, height = 780): Promise<App> {
  const app = await open("./Chat.fixture.tsx", {
    platform: "macos", width: 900, height,
    host: { params: { recording, chatId }, handlers: { "app.fixture.state": null } },
  });
  await app.waitFor("id:chat-input", { timeout: 20000 });
  await app.waitUntil(() => app.enabled("id:chat-input"), { timeout: 20000 });
  return app;
}

async function inspect(app: App): Promise<FixtureState> {
  await app.host.send("app.fixture.inspect");
  const posts = app.host.posts().filter((post) => post.name === "app.fixture.state");
  return posts[posts.length - 1].data as FixtureState;
}

async function send(app: App, text: string) {
  await app.type("id:chat-input", text);
  await app.press("id:chat-send");
}

test("P4-CHAT-01 an approval is visible and its Allow button answers the host", async () => {
  const app = await chat("approval_accept");
  try {
    await send(app, "write it");
    await app.waitFor("id:chat-approval", { timeout: 20000 });
    expect(app.texts().join("\n")).toContain("Run a command?");
    expect(app.shows("label:Allow")).toBe(true);
    expect(app.value("id:chat-input")).toBe("");
    await app.press("id:chat-decision:accept");
    await app.waitFor("text:Done", { timeout: 20000 });
    await app.waitFor("id:chat-approval", { state: "gone", timeout: 20000 });
  } finally { app.dispose(); }
});

test("P4-CHAT-02 ⏎ in an empty composer allows, and ⎋ denies a file change", async () => {
  const allow = await chat("approval_accept");
  try {
    await send(allow, "write it");
    await allow.waitFor("id:chat-approval", { timeout: 20000 });
    await allow.wait(800); // never within 700 ms of a send
    await allow.submit("id:chat-input");
    await allow.waitFor("text:Done", { timeout: 20000 });
  } finally { allow.dispose(); }
  const deny = await chat("approval_decline");
  try {
    await send(deny, "add a file");
    await deny.waitFor("id:chat-approval", { timeout: 20000 });
    expect(deny.texts().join("\n")).toContain("Change files?");
    await deny.wait(800);
    // Focus the empty box as a user does: typing, then erasing.
    await deny.type("id:chat-input", "x");
    await deny.clear("id:chat-input");
    await deny.keyboard.press("escape");
    await deny.waitFor("text:Understood, I did not write it.", { timeout: 20000 });
    await deny.waitFor("id:chat-approval", { state: "gone", timeout: 20000 });
  } finally { deny.dispose(); }
});

test("P4-CHAT-03 Stop interrupts a running turn, which closes as Interrupted", async () => {
  const app = await chat("interrupt");
  try {
    await send(app, "sleep");
    await app.waitFor("id:chat-interrupt", { timeout: 20000 });
    expect(app.texts()).toContain("Working…");
    await app.press("id:chat-interrupt");
    await app.waitUntil(() => app.texts().some((text) => text.includes("Interrupted")), { timeout: 20000 });
    await app.waitFor("id:chat-send", { timeout: 20000 });
    expect((await inspect(app)).panel?.running).toBe(false);
  } finally { app.dispose(); }
});

test("P4-CHAT-04 questions are answered from the panel, then access is allowed for the session", async () => {
  const app = await chat("question");
  try {
    await send(app, "ask me");
    await app.waitFor("id:chat-question", { timeout: 20000 });
    await app.press("id:chat-send-answers");
    expect(app.texts()).toContain("Answer every question first.");
    await app.press("label:Strict (Recommended)");
    await app.type("id:chat-answer:1", "no thanks");
    await app.press("id:chat-send-answers");
    await app.waitFor("id:chat-approval", { timeout: 20000 });
    expect(app.texts().join("\n")).toContain("Grant more access?");
    await app.press("label:Allow for session");
    await app.waitFor("id:chat-approval", { state: "gone", timeout: 20000 });
    await app.waitUntil(() => app.shows("id:chat-send") && !app.shows("id:chat-question"), { timeout: 20000 });
  } finally { app.dispose(); }
});

test("P4-CHAT-05 a draft survives closing the tab and comes back in an empty composer", async () => {
  const first = await chat("turn");
  let chatId = "";
  try {
    await first.type("id:chat-input", "half a thought\n  indented");
    chatId = (await inspect(first)).chatId!;
    await first.wait(200);
  } finally { first.dispose(); }
  const again = await chat("turn", chatId);
  try {
    await again.waitUntil(() => again.value("id:chat-input") === "half a thought\n  indented", { timeout: 20000 });
  } finally { again.dispose(); }
});

test("P4-CHAT-06 a 2,200-call disclosed turn keeps fewer than 100 rows materialized", async () => {
  const app = await chat("large_turn");
  try {
    await send(app, "read everything");
    await app.waitFor("text:Read 2,200 files.", { timeout: 60000 });
    await app.waitUntil(() => app.texts().some((text) => text.startsWith("Worked")), { timeout: 20000 });
    const header = app.texts().find((text) => text.startsWith("Worked"))!;
    await app.press(`label:${header}`);
    await app.waitUntil(() => app.count("text:▾") > 0, { timeout: 20000 });
    const groups = (await inspect(app)).groups ?? [];
    for (const group of groups) await app.press(`id:chat-header:${group}`);
    let state = await inspect(app);
    for (let tries = 0; (state.rows ?? 0) < 2200 && tries < 200; tries += 1) {
      await app.wait(100);
      state = await inspect(app);
    }
    const materialized = (at: string) => {
      if (!(state.cells! > 0 && state.cells! < 100 && state.bodies! < 100)) throw new Error(`${at}: ${JSON.stringify({ rows: state.rows, cells: state.cells, constructed: state.constructed, bodies: state.bodies })}`);
    };
    expect(state.rows! >= 2200).toBe(true);
    materialized("disclosed");
    // Scroll through the 2,200 calls, whichever end the list rests at.
    let direction = -1;
    for (let step = 0; step < 6; step += 1) {
      try { await app.scroll("id:chat-transcript", direction * 4000); }
      catch { direction = -direction; await app.scroll("id:chat-transcript", direction * 4000); }
    }
    await app.wait(100);
    state = await inspect(app);
    materialized("scrolled");
  } finally { app.dispose(); }
}, 120000);
