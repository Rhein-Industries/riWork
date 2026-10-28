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
