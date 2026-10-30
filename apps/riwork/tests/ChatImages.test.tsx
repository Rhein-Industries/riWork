import { expect, open, test, type App } from "nsh/test";

// Real ChatHandle over the private replay host. No GUI or production provider.
async function chat(chatId?: string): Promise<App> {
  const app = await open("./Chat.fixture.tsx", {
    platform: "macos", width: 900, height: 1800,
    host: { params: { recording: "image_turn", chatId }, handlers: { "app.fixture.state": null, "app.fixture.draft": null } },
  });
  await app.waitFor("id:chat-input", { timeout: 20000 });
  await app.waitUntil(() => app.enabled("id:chat-input"), { timeout: 20000 });
  return app;
}

test("Chat images render Cua bytes without a click and render again from persisted history", async () => {
  let app = await chat();
  try {
    await app.type("id:chat-input", "show screenshots");
    await app.press("id:chat-send");
    // This id belongs to the actual NSH Image, whose src is the native PNG data URL.
    await app.waitFor("id:chat-image:cua-shot:0", { timeout: 20000 });
    await app.waitFor("id:chat-image-error:cua-shot:1", { timeout: 20000 });
    expect(app.texts().join("\n")).toContain("Invalid image encoding");
    // A local path must not be read just because a provider returned it.
    expect(app.shows("id:chat-image:cua-shot:2")).toBe(false);
    expect(app.shows("id:chat-image-error:cua-shot:2")).toBe(false);
    expect(app.shows("id:chat-image-error:cua-shot:3")).toBe(false);
    await app.press("id:chat-image-load:cua-shot:2");
    await app.waitFor("id:chat-image-error:cua-shot:2");
    expect(app.texts().join("\n")).toContain("Cannot open this file");
    expect(app.shows("id:chat-image:cua-shot:2")).toBe(false);
    await app.press("id:chat-image-load:cua-shot:3");
    await app.waitFor("id:chat-image-error:cua-shot:3");
    expect(app.texts().join("\n")).toContain("outside the folders of this chat");
    expect(app.shows("id:chat-image:cua-shot:3")).toBe(false);
    await app.host.send("app.fixture.inspect");
    const reports = app.host.posts().filter((post) => post.name === "app.fixture.state");
    const { chatId } = reports[reports.length - 1].data as { chatId: string };
    app.dispose();
    app = await chat(chatId);
    await app.waitFor("id:chat-image:cua-shot:0", { timeout: 20000 });
    await app.waitFor("id:chat-image-error:cua-shot:1");
    // Preview state is owned by the List cell: local access requires a new click.
    expect(app.shows("id:chat-image-error:cua-shot:2")).toBe(false);
  } finally { app.dispose(); }
});

interface Frame { x: number; y: number; width: number; height: number }
type Parts = { main?: Frame; side?: Frame; bottom?: Frame };

/** The tab's parts, once `ready` holds of them. */
async function parts(app: App, ready: (parts: Parts) => boolean): Promise<Required<Parts>> {
  const latest = () => (app.host.posts().filter((post) => post.name === "app.fixture.state").at(-1)?.data as { layout?: Parts } | undefined)?.layout ?? {};
  for (let tries = 0; tries < 200; tries += 1) {
    await app.host.send("app.fixture.inspect");
    if (ready(latest())) return latest() as Required<Parts>;
    await app.wait(25);
  }
  throw new Error(`layout never settled: ${JSON.stringify(latest())}`);
}

/** `a` ends where `b` begins or before, along `axis` (half a point of rounding). */
const ends = (a: Frame, b: Frame, axis: "x" | "y") => axis === "x" ? a.x + a.width <= b.x + 0.5 : a.y + a.height <= b.y + 0.5;

test("Chat images open beside the transcript, and under it in a narrow chat, leaving the composer usable", async () => {
  const app = await chat();
  try {
    await app.type("id:chat-input", "show screenshots");
    await app.press("id:chat-send");
    await app.waitFor("id:chat-image:cua-shot:0", { timeout: 20000 });
    await app.press("label:Expand Image");
    await app.waitFor("id:chat-side-image");
    // Wide: a column of its own on the right; the screenshot stays in the transcript.
    let { main, side, bottom } = await parts(app, (shown) => !!shown.side && !!shown.main && shown.main.width < 900);
    expect(ends(main, side, "x") && ends(bottom, side, "x") && side.y <= main.y).toBe(true);
    expect(app.shows("id:chat-image:cua-shot:0")).toBe(true);
    // Narrow: between the transcript and the composer, about half the height.
    await app.setViewport(600, 1200);
    ({ main, side, bottom } = await parts(app, (shown) => !!shown.side && !!shown.main && shown.side.y > 0));
    expect(ends(main, side, "y") && ends(side, bottom, "y")).toBe(true);
    expect(Math.abs(side.width - main.width) <= 0.5 && Math.abs(side.height - main.height) <= 1).toBe(true);
    // The composer still takes typing, and its ⎋ leaves the preview; Close closes it.
    // (⎋ with the focus on the preview's Close is not shown here: NSH 0.3's
    // headless driver focuses only text inputs. P4-CHAT-13 folds a review so.)
    await app.type("id:chat-input", "a follow-up");
    expect(app.value("id:chat-input")).toBe("a follow-up");
    await app.keyboard.press("escape");
    expect(app.shows("id:chat-side-image")).toBe(true);
    await app.press("id:chat-side-close");
    await app.waitFor("id:chat-side-image", { state: "gone" });
    expect(app.value("id:chat-input")).toBe("a follow-up");
  } finally { app.dispose(); }
});
