import { expect, open, test } from "nsh/test";

const CARD = "id:usage-codex:/Users/fixture/.codex";
const OPTIONS = { width: 700, height: 1000, clock: {}, host: { handlers: { "app.fixture.usage.state": null } } };

test("P5-USAGE-13: the panel ages a passed reset without a new usage snapshot or data read", async () => {
  const app = await open("./UsagePanel.fixture.tsx", OPTIONS);
  try {
    await app.waitFor(CARD);
    await app.host.send("app.fixture.usage.report", { mode: "reset" });
    expect(app.shows(`${CARD} >> text:5h · 20% left`)).toBe(true);
    expect(app.shows(`${CARD} >> text:Quota reset passed; waiting for an updated report`)).toBe(false);
    await app.clock.runFor(61_000);
    expect(app.shows(`${CARD} >> text:Quota reset passed; waiting for an updated report`)).toBe(true);
    expect(app.shows(`${CARD} >> text:Awaiting reset update`)).toBe(true);
    expect(app.shows(`${CARD} >> text:5h · 20% left`)).toBe(true);
    // The display clock advances at whole-minute boundaries, which may fall
    // less than 60 seconds after the report was delivered.
    expect(app.shows(`${CARD} >> text:Stale · just now`) || app.shows(`${CARD} >> text:Stale · 1m ago`)).toBe(true);
    await app.host.send("app.fixture.usage.inspect", {});
    await app.waitUntil(() => app.host.posts().some((post) => post.name === "app.fixture.usage.state"));
    const state = app.host.posts().find((post) => post.name === "app.fixture.usage.state")?.data as { calls: string[] };
    expect(state.calls.some((call) => call.startsWith("refresh:"))).toBe(false);
  } finally { app.dispose(); }
});

test("P5-USAGE-14: the panel distinguishes absent quota, cached refresh and failed refresh", async () => {
  const app = await open("./UsagePanel.fixture.tsx", OPTIONS);
  try {
    await app.waitFor(CARD);
    await app.host.send("app.fixture.usage.report", { mode: "empty" });
    expect(app.shows(`${CARD} >> text:Quota windows unavailable`)).toBe(true);
    expect(app.shows(`${CARD} >> text:No subscription quota was reported`)).toBe(true);
    expect(app.count(`${CARD} >> role:progressbar`)).toBe(0);
    await app.host.send("app.fixture.usage.report", { mode: "refreshing" });
    expect(app.shows(`${CARD} >> text:Refreshing usage…`)).toBe(true);
    expect(app.shows(`${CARD} >> text:5h · 20% left`)).toBe(true);
    await app.host.send("app.fixture.usage.report", { mode: "failed" });
    expect(app.shows(`${CARD} >> text:Last refresh failed: Fixture read failed`)).toBe(true);
    expect(app.shows(`${CARD} >> text:Stale · just now`)).toBe(true);
    expect(app.shows(`${CARD} >> text:5h · 20% left`)).toBe(true);
  } finally { app.dispose(); }
});

test("P5-USAGE-16: forgotten live bindings cannot recreate cards and native unlisted targets remain visible", async () => {
  const app = await open("./UsagePanel.fixture.tsx", OPTIONS);
  try {
    await app.waitFor(CARD);
    const oldCard = "id:usage-codex:/Users/fixture/.riwork/codex/old";
    // Catalog-only unavailable accounts still show their actionable reason.
    expect(app.shows(`${oldCard} >> text:Signed out. Run codex login for this account.`)).toBe(true);
    await app.host.send("app.fixture.usage.forget", {});
    expect(app.count(oldCard)).toBe(0);
    expect(app.shows("text:Old team")).toBe(false);
    expect(app.shows(CARD)).toBe(true);
    expect(app.shows("id:usage-codex:/Users/fixture/.riwork/codex/work")).toBe(true);
    expect(app.shows("id:usage-codex:/Users/fixture/custom-codex >> text:Custom session")).toBe(true);
    expect(app.shows("id:usage-codex:/Users/fixture/custom-codex >> text:Reading account usage…")).toBe(true);
    expect(app.count("id:usage-loading")).toBe(0);
  } finally { app.dispose(); }
});
