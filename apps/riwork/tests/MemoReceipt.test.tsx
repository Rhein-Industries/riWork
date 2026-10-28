import { expect, open, test, type App } from "nsh/test";

interface State { memo: string; requests: { text: string; phase: string }[]; error: string }
async function inspect(app: App): Promise<State> {
  const count = app.host.posts().length;
  await app.host.send("app.fixture.inspect");
  await app.waitUntil(() => app.host.posts().length > count);
  return app.host.posts().filter((post) => post.name === "app.fixture.state").slice(-1)[0].data as State;
}
async function waitState(app: App, predicate: (state: State) => boolean) {
  let state = await inspect(app);
  for (let attempt = 0; attempt < 40 && !predicate(state); attempt++) {
    await app.wait(50); state = await inspect(app);
  }
  expect(state.error).toBe("");
  if (!predicate(state)) throw new Error(`Memo state ${JSON.stringify(state)}; visible ${JSON.stringify(app.value("id:project-memo-input"))}; status ${app.texts().join(" | ")}`);
  return state;
}

test("P3b memo delayed receipt preserves dirty A-B-A edits until visible text is saved", async () => {
  const app = await open("./MemoReceipt.fixture.tsx", { platform: "macos", width: 800, height: 600,
    host: { params: { key: `receipt-${Date.now()}` }, handlers: { "app.fixture.state": null } } });
  try {
    await app.waitFor("id:project-memo-input", { timeout: 15000 });
    await app.waitUntil(() => app.value("id:project-memo-input") === "A" && app.texts().includes("Saved automatically"));
    expect((await inspect(app)).memo).toBe("A");
    await app.clear("id:project-memo-input"); await app.paste("id:project-memo-input", { text: "B" });
    await app.press("id:project-memo-save");
    await waitState(app, (state) => state.requests[0]?.text === "B");
    await app.host.send("app.fixture.commit", { index: 0 });
    await waitState(app, (state) => state.memo === "B" && state.requests[0]?.phase === "committed");

    // B is on disk but its receipt is pending. Restore the old saved A.
    await app.clear("id:project-memo-input"); await app.paste("id:project-memo-input", { text: "A" });
    expect(app.value("id:project-memo-input")).toBe("A");
    expect(app.texts()).toContain("Saving…");
    await app.host.send("app.fixture.receipt", { index: 0 });
    const pending = await waitState(app, (state) => state.requests.length === 2);
    expect(pending.memo).toBe("B");
    expect(pending.requests[1]).toEqual({ text: "A", phase: "requested" });
    expect(app.value("id:project-memo-input")).toBe("A");
    expect(app.texts()).toContain("Saving…");
    expect(app.texts().includes("Saved automatically")).toBe(false);
    expect(app.enabled("id:project-memo-save")).toBe(false);

    await app.host.send("app.fixture.commit", { index: 1 });
    await waitState(app, (state) => state.memo === "A" && state.requests[1]?.phase === "committed");
    expect(app.texts()).toContain("Saving…");
    await app.host.send("app.fixture.receipt", { index: 1 });
    await app.waitUntil(() => app.texts().includes("Saved automatically"));
    const saved = await inspect(app);
    expect(saved.memo).toBe(app.value("id:project-memo-input"));
    expect(saved.memo).toBe("A");
    expect(saved.requests.map((request) => request.text)).toEqual(["B", "A"]);
    expect(app.enabled("id:project-memo-save")).toBe(true);
    // All assertions precede disposal; a close-time native flush cannot hide the race.
  } finally { app.dispose(); }
});
