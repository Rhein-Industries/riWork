import { expect, open, test, type App } from "nsh/test";

// P4 chat tab tests. They need `RIWORK_NSH_CHAT_FIXTURE=1` and an isolated /tmp
// RIWORK_HOME: the native module then runs an in-process chat host whose Codex
// driver replays the engine's recorded provider streams (never a live host).

interface Frame { x: number; y: number; width: number; height: number }
interface FixtureState { chatId: string | null; rows?: number; cells?: number; constructed?: number; bodies?: number; groups?: string[]; panel?: { state: string; running: boolean; approvals: unknown[] }; editors?: number[]; bodyViews?: number; imageCards?: number; composer?: { box: number; input: number; controls: number }; layout?: { main?: Frame; side?: Frame; bottom?: Frame } }

async function chat(recording: string, chatId?: string, height = 780): Promise<App> {
  const app = await open("./Chat.fixture.tsx", {
    platform: "macos", width: 900, height,
    host: { params: { recording, chatId }, handlers: { "app.fixture.state": null, "app.fixture.draft": null } },
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

/** The chat's draft text as the native store holds it (`null`: no draft). */
async function storedDraft(app: App): Promise<string | null> {
  const reports = () => app.host.posts().filter((post) => post.name === "app.fixture.draft");
  const before = reports().length;
  await app.host.send("app.fixture.draft");
  for (let tries = 0; reports().length === before && tries < 200; tries += 1) await app.wait(25);
  const report = reports()[before]?.data as { text?: string | null; error?: string } | undefined;
  if (!report || report.error !== undefined) throw new Error(`no draft report: ${report?.error}`);
  return report.text ?? null;
}

/** The stored draft once it is `wanted`, or what it is after 5 s: saves are queued. */
async function storedDraftBecomes(app: App, wanted: string): Promise<string | null> {
  let stored = await storedDraft(app);
  for (let tries = 0; stored !== wanted && tries < 100; tries += 1) { await app.wait(50); stored = await storedDraft(app); }
  return stored;
}

async function send(app: App, text: string) {
  await app.type("id:chat-input", text);
  await app.press("id:chat-send");
}

/** The tab's parts once the side panel is laid out. */
async function sideLayout(app: App): Promise<{ main: Frame; side: Frame; bottom: Frame }> {
  let state = await inspect(app);
  for (let tries = 0; !state.layout?.side && tries < 200; tries += 1) { await app.wait(25); state = await inspect(app); }
  const { main, side, bottom } = state.layout ?? {};
  if (!main || !side || !bottom) throw new Error(`no side panel layout: ${JSON.stringify(state.layout)}`);
  return { main, side, bottom };
}

/** `a` ends where `b` begins or before, along `axis` (half a point of rounding). */
function ends(a: Frame, b: Frame, axis: "x" | "y") {
  return axis === "x" ? a.x + a.width <= b.x + 0.5 : a.y + a.height <= b.y + 0.5;
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

test("P4-CHAT-07 two windows' first composers never clear each other's draft", async () => {
  // Two apps are two JS runtimes in one process, sharing the native draft store.
  // Each one's composer is its runtime's first; equal-length texts put both at
  // the same edit generation, so only the editor identity tells them apart.
  const first = await chat("turn");
  const chatId = (await inspect(first)).chatId!;
  const second = await chat("turn", chatId);
  try {
    await first.wait(200);
    await first.type("id:chat-input", "hello");
    await second.type("id:chat-input", "draft");
    expect(await storedDraftBecomes(first, "draft")).toBe("draft");
    await first.press("id:chat-send");
    await first.waitUntil(() => first.texts().some((text) => text.startsWith("Worked")), { timeout: 20000 });
    await first.waitUntil(() => first.value("id:chat-input") === "", { timeout: 20000 });
    expect(await storedDraft(first)).toBe("draft");
    expect(second.value("id:chat-input")).toBe("draft");
    // One composer each for the whole turn, and two identities.
    const [a, b] = [(await inspect(first)).editors ?? [], (await inspect(second)).editors ?? []];
    expect([a.length, b.length]).toEqual([1, 1]);
    expect(a[0] !== b[0]).toBe(true);
  } finally { second.dispose(); first.dispose(); }
  const third = await chat("turn", chatId);
  try {
    await third.waitUntil(() => third.value("id:chat-input") === "draft", { timeout: 20000 });
  } finally { third.dispose(); }
});

test("P4-CHAT-08 a composer's own submission clears its saved draft", async () => {
  const app = await chat("turn");
  try {
    await app.type("id:chat-input", "hello");
    expect(await storedDraftBecomes(app, "hello")).toBe("hello");
    await app.press("id:chat-send");
    await app.waitUntil(() => app.texts().some((text) => text.startsWith("Worked")), { timeout: 20000 });
    await app.waitUntil(() => app.value("id:chat-input") === "", { timeout: 20000 });
    expect(await storedDraft(app)).toBe(null);
    expect((await inspect(app)).editors?.length).toBe(1);
  } finally { app.dispose(); }
});

/** The provider's last warning of a `*_refresh` recording is on screen. */
async function refreshed(app: App) {
  await app.waitUntil(() => app.texts().some((text) => text.includes("Still working (8 of 8)")), { timeout: 20000 });
}

test("P4-CHAT-09 picked and typed answers survive provider refreshes while the question waits", async () => {
  const app = await chat("question_refresh");
  try {
    await send(app, "ask me");
    await app.waitFor("id:chat-question", { timeout: 20000 });
    await app.press("label:Strict (Recommended)");
    await app.type("id:chat-answer:1", "no thanks");
    // The running turn's provider warns while the question waits: panel
    // deliveries that leave the question alone.
    await refreshed(app);
    expect(app.value("id:chat-answer:1")).toBe("no thanks");
    expect(app.texts().some((text) => text.startsWith("✓ Strict (Recommended)"))).toBe(true);
    // The answer box is the same input, still focused: a key lands in it.
    await app.keyboard.press("s");
    expect(app.value("id:chat-answer:1")).toBe("no thankss");
    await app.keyboard.press("backspace");
    expect(app.value("id:chat-answer:1")).toBe("no thanks");
    await app.press("id:chat-send-answers");
    await app.waitFor("id:chat-question", { state: "gone", timeout: 20000 });
    await app.waitFor("id:chat-approval", { timeout: 20000 });
    await app.press("label:Allow for session");
    await app.waitFor("id:chat-approval", { state: "gone", timeout: 20000 });
  } finally { app.dispose(); }
});

test("P4-CHAT-10 an expanded approval stays expanded through provider refreshes", async () => {
  const app = await chat("approval_refresh");
  try {
    await send(app, "write it");
    await app.waitFor("id:chat-approval", { timeout: 20000 });
    expect(app.texts().join("\n")).not.toContain("It can be undone.");
    await app.press("label:show 2 more lines");
    await app.waitFor("label:show less", { timeout: 20000 });
    await refreshed(app);
    expect(app.shows("label:show less")).toBe(true);
    expect(app.texts().join("\n")).toContain("It can be undone.");
    await app.press("id:chat-decision:accept");
    await app.waitFor("text:Done", { timeout: 20000 });
    await app.waitFor("id:chat-approval", { state: "gone", timeout: 20000 });
  } finally { app.dispose(); }
});

test("P4-CHAT-11 a streaming answer and its turn's screenshot keep their views", async () => {
  const app = await chat("streaming_turn", undefined, 1400);
  try {
    await send(app, "stream an answer");
    await app.waitFor("id:chat-image:cua-shot:0", { timeout: 20000 });
    await app.waitUntil(() => app.texts().some((text) => text.includes("chunk 8.")), { timeout: 20000 });
    const before = await inspect(app);
    // 24 deltas and at least two ticks of the running activity row's clock.
    await app.waitUntil(() => app.texts().some((text) => text.includes("chunk 32.")), { timeout: 20000 });
    const after = await inspect(app);
    expect(after.panel?.running).toBe(true);
    expect({ bodyViews: after.bodyViews, imageCards: after.imageCards })
      .toEqual({ bodyViews: before.bodyViews, imageCards: before.imageCards });
    expect(app.shows("id:chat-image:cua-shot:0")).toBe(true);
    await app.waitUntil(() => app.texts().some((text) => text.startsWith("Worked")), { timeout: 20000 });
  } finally { app.dispose(); }
});

test("P4-CHAT-12 a long draft stops the composer at nine lines, with nothing below its controls", async () => {
  const app = await chat("turn");
  try {
    await app.type("id:chat-input", "word ".repeat(600));
    await app.wait(200);
    const { input, controls, box } = (await inspect(app)).composer!;
    expect(input).toBe(9 * 21);
    // The box holds the message box and controls: two 1 pt borders, 10 pt of
    // padding each side, and two 8 pt gaps (after the empty dictation and
    // suggestion slots, and before the controls).
    expect(box).toBeLessThanOrEqual(input + controls + 2 * 11 + 2 * 8);
  } finally { app.dispose(); }
});

test("P4-CHAT-13 changed files open for review beside the transcript, and the composer stays usable", async () => {
  const app = await chat("approval_decline");
  try {
    // The declined change is still a reported file change.
    await send(app, "add a file");
    await app.waitFor("id:chat-approval", { timeout: 20000 });
    await app.wait(800);
    await app.type("id:chat-input", "x");
    await app.clear("id:chat-input");
    await app.keyboard.press("escape");
    await app.waitFor("text:Understood, I did not write it.", { timeout: 20000 });
    await app.press("label:Review changed files");
    await app.waitFor("id:chat-file-review", { timeout: 20000 });
    expect(app.texts()).toContain("Reported file changes");
    // Beside the transcript and its composer, the tab's whole height: it covers neither.
    const { main, side, bottom } = await sideLayout(app);
    expect(ends(main, side, "x") && ends(bottom, side, "x") && ends(main, bottom, "y")).toBe(true);
    expect(side.y <= main.y && side.y + side.height >= bottom.y + bottom.height - 0.5).toBe(true);
    expect(app.shows("text:Understood, I did not write it.")).toBe(true);
    // The composer takes typing while the review is open, and its ⎋ leaves the review alone.
    await app.type("id:chat-input", "still typing");
    expect(app.value("id:chat-input")).toBe("still typing");
    await app.keyboard.press("escape");
    expect(app.shows("id:chat-file-review")).toBe(true);
    // ⎋ in the review folds it away; the draft and the composer stay.
    await app.type("label:Filter changed files", "new");
    await app.keyboard.press("escape");
    await app.waitFor("id:chat-file-review", { state: "gone" });
    expect(app.value("id:chat-input")).toBe("still typing");
    expect((await inspect(app)).editors?.length).toBe(1);
  } finally { app.dispose(); }
});

test("P4-CHAT-14 a code block's Copy to: row copies to the terminal chosen in place", async () => {
  const app = await open("./CodeTerminalRow.fixture.tsx", {
    platform: "macos", width: 700, height: 240, host: { handlers: { "app.fixture.copied": null, "app.fixture.closed": null } },
  });
  const posts = (name: string) => app.host.posts().filter((post) => post.name === name).map((post) => post.data);
  try {
    await app.waitFor("text:Copy to:");
    expect(app.shows("label:Copy snippet and open zsh api")).toBe(true);
    // A failed copy keeps the row, with the error.
    await app.press("label:Copy snippet and open zsh broken");
    await app.waitUntil(() => app.texts().some((text) => text.includes("Could not reach the terminal")));
    expect(app.shows("id:chat-copy-to-terminal")).toBe(true);
    expect(posts("app.fixture.copied")).toEqual([]);
    await app.press("label:Copy snippet and open zsh api");
    await app.waitFor("id:chat-copy-to-terminal", { state: "gone" });
    expect(posts("app.fixture.copied")).toEqual([{ id: "shell:api", text: "npm test" }]);
    // The chosen terminal has the focus now: none goes back to the code block.
    expect(posts("app.fixture.closed")).toEqual([{ focused: false }]);
    // ⎋ once the row's Cancel has the focus is not shown here: NSH 0.3's
    // headless driver focuses only text inputs (P4-CHAT-13 folds a review so).
    // With no terminal open it says so, and Cancel folds it away.
    await app.host.send("app.fixture.targets", []);
    await app.host.send("app.fixture.open");
    await app.waitFor("text:No local terminals are open in this project window.");
    await app.press("label:Cancel");
    await app.waitFor("id:chat-copy-to-terminal", { state: "gone" });
  } finally { app.dispose(); }
});
