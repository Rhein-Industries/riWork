import { expect, open, test, type App } from "nsh/test";
interface State { calls: string[]; routes: { type: string; id?: string }[]; refreshed: number; query: string; filter: string }
const options = { platform: "macos" as const, width: 1280, height: 900, host: { handlers: { "app.fixture.dashboard.state": null } } };
async function inspect(app: App): Promise<State> {
  const count = app.host.posts().length;
  await app.host.send("app.fixture.dashboard.inspect", {});
  await app.waitUntil(() => app.host.posts().length > count);
  return app.host.posts().filter((post) => post.name === "app.fixture.dashboard.state").slice(-1)[0].data as State;
}
const SENT = "Sent. The inbox updates when the provider confirms the answer.";
// Every review call below is Decision.fixture's own; no native read or chat host.

test("P5-DECISION-01 a decision unfolds its review in its own row and leaves the dashboard usable", async () => {
  const app = await open("./Decision.fixture.tsx", options);
  try {
    await app.waitFor("id:dashboard-decision-fixture-approval");
    await app.press("id:dashboard-decision-fixture-approval");
    await app.waitFor("text:cargo test --offline");
    // In place: in the decision's own row, in the Decisions list; the next row stays.
    expect(app.count("id:dashboard-decision-row-fixture-approval >> id:dashboard-decision-review")).toBe(1);
    expect(app.count("id:dashboard-decisions >> id:dashboard-decision-review")).toBe(1);
    expect(app.shows("id:dashboard-decision-fixture-question")).toBe(true);
    // Nothing covers or holds the window: its project row, search and filters work,
    // and Escape typed in the search is not the review's.
    await app.press("id:dashboard-project-fixture-project");
    expect((await inspect(app)).routes).toEqual([{ type: "project", id: "fixture-project" }]);
    await app.type("id:dashboard-search", "Fixture");
    expect((await inspect(app)).query).toBe("Fixture");
    await app.keyboard.press("escape");
    expect(app.shows("id:dashboard-decision-review")).toBe(true);
    await app.press("id:dashboard-filter-active");
    expect((await inspect(app)).filter).toBe("active");
    // New reads redraw the row around the same review: the request is read once.
    for (let turn = 0; turn < 3; turn++) await app.host.send("app.fixture.dashboard.read", {});
    expect(app.shows("text:cargo test --offline")).toBe(true);
    expect((await inspect(app)).calls).toEqual(["read:chat-a"]);
    // Allow answers through the review token once and refreshes the dashboard.
    await app.press("id:dashboard-decision-accept");
    await app.waitFor(`text:${SENT}`);
    expect((await inspect(app)).calls).toEqual(["read:chat-a", "respond:review-a:accept"]);
    expect((await inspect(app)).refreshed).toBe(1);
    // Its request leaves the list; the review stays where it was until folded.
    await app.host.send("app.fixture.dashboard.read", { only: ["fixture-question"] });
    expect(app.shows(`text:${SENT}`)).toBe(true);
    await app.press("id:dashboard-review-close");
    await app.waitUntil(() => !app.shows("id:dashboard-decision-review"));
    expect(app.shows("id:dashboard-decision-fixture-approval")).toBe(false);
    expect(app.shows("id:dashboard-decision-fixture-question")).toBe(true);
  } finally { app.dispose(); }
});

test("P5-DECISION-02 a question keeps its answer across reads, folds on its own Escape and holds while sending", async () => {
  const app = await open("./Decision.fixture.tsx", options);
  try {
    await app.waitFor("id:dashboard-decision-fixture-question");
    await app.press("id:dashboard-decision-fixture-question");
    await app.waitFor("id:dashboard-answer-0");
    await app.type("id:dashboard-answer-0", "MySQL");
    for (let turn = 0; turn < 3; turn++) await app.host.send("app.fixture.dashboard.read", {});
    expect(app.value("id:dashboard-answer-0")).toBe("MySQL");
    // The focus stayed in the answer through the reads: Escape there folds it.
    await app.keyboard.press("escape");
    await app.waitUntil(() => !app.shows("id:dashboard-decision-review"));
    // Unfolding again reads the request again; its row's Review toggles it.
    await app.press("id:dashboard-decision-fixture-question");
    await app.waitFor("id:dashboard-answer-0");
    expect(app.value("id:dashboard-answer-0")).toBe("");
    expect((await inspect(app)).calls).toEqual(["read:chat-b", "read:chat-b"]);
    await app.press("id:dashboard-answer-option-0-SQLite");
    await app.host.send("app.fixture.dashboard.hold", {});
    await app.press("id:dashboard-send-answer");
    // While it sends, nothing folds or switches it away.
    expect(app.enabled("id:dashboard-review-close")).toBe(false);
    expect(app.enabled("id:dashboard-decision-fixture-question")).toBe(false);
    expect(app.enabled("id:dashboard-decision-fixture-approval")).toBe(false);
    await app.host.send("app.fixture.dashboard.release", {});
    await app.waitFor(`text:${SENT}`);
    expect((await inspect(app)).calls).toEqual(["read:chat-b", "read:chat-b", 'respond:review-b:[["SQLite"]]']);
    // Another row switches to its own review; Open chat folds it and routes the chat.
    await app.press("id:dashboard-decision-fixture-approval");
    await app.waitFor("text:cargo test --offline");
    expect(app.count("id:dashboard-decision-review")).toBe(1);
    await app.press("id:dashboard-review-open-chat");
    await app.waitUntil(() => !app.shows("id:dashboard-decision-review"));
    expect((await inspect(app)).routes).toEqual([{ type: "chat", id: "chat-a" }]);
  } finally { app.dispose(); }
});
