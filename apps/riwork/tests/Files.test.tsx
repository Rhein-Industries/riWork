import { expect, open, test, type App } from "nsh/test";
import type { FilePreview } from "../files_model";

// Requires RIWORK_NSH_FILES_FIXTURE=1 and an isolated temporary RIWORK_HOME.
interface State { root: string | null; selected: string | null; rows: string[]; preview: FilePreview | null; previewPath: string | null; edits: string[]; opens: string[]; requests: number; query: string; memo: string; notice: string; exact: string | null }
let sequence = 0;
async function fixture() {
  const app = await open("./Files.fixture.tsx", { platform: "macos", width: 1100, height: 850,
    host: { params: { key: `${Date.now()}-${++sequence}` }, handlers: { "app.fixture.state": null } } });
  await app.waitFor("id:files-panel", { timeout: 15000 });
  await app.waitFor("id:file:note.txt", { timeout: 15000 });
  return app;
}
async function inspect(app: App): Promise<State> {
  const count = app.host.posts().length;
  await app.host.send("app.fixture.inspect");
  await app.waitUntil(() => app.host.posts().length > count);
  return app.host.posts().filter((post) => post.name === "app.fixture.state").slice(-1)[0].data as State;
}
async function waitState(app: App, condition: (state: State) => boolean) {
  let state = await inspect(app);
  for (let attempt = 0; attempt < 150 && !condition(state); attempt++) { await app.wait(50); state = await inspect(app); }
  expect(condition(state)).toBe(true);
}

test("P3b Files selected-row actions validate the exact spaced Unicode file and preserve root selection on relabel", async () => {
  const app = await fixture();
  try {
    await app.press("id:file:nested dir");
    await app.waitFor("id:file:nested dir/日本語.md");
    await app.press("id:file:nested dir/日本語.md");
    await app.waitFor("id:preview-markdown");
    expect(app.texts().join("\n")).toContain("Fixture heading");
    const before = await inspect(app);
    expect(before.preview?.kind).toBe("text"); expect(before.preview?.markdown).toBe(true);
    await app.press("id:files-edit:fixture");
    await waitState(app, (state) => state.edits.length === 1);
    expect((await inspect(app)).edits[0]).toBe(`${before.root}/nested dir/日本語.md`);
    await app.press("id:files-open:fixture");
    await waitState(app, (state) => state.opens.length === 1);
    expect((await inspect(app)).opens[0]).toBe(`${before.root}/nested dir/日本語.md`);
    await app.press("id:fixture-relabel");
    expect((await inspect(app)).selected).toBe("nested dir/日本語.md");
    await app.press("id:preview-markdown-mode");
    await app.waitFor("id:preview-code");
    await app.type("id:files-filter:fixture", "日本");
    expect((await inspect(app)).rows).toEqual(["nested dir", "nested dir/日本語.md"]);
    await app.press("id:fixture-root-none");
    expect((await inspect(app)).selected).toBe(null); expect((await inspect(app)).preview).toBe(null); expect((await inspect(app)).query).toBe("");
    expect(app.texts()).toContain("Select a worktree to browse its files.");
    await app.press("id:fixture-root-restore"); await app.waitFor("id:file:note.txt");
    expect((await inspect(app)).preview).toBe(null);
  } finally { app.dispose(); }
});

test("P3b preview byte and type boundaries handle text, binary, oversized text, images and explicit PDF", async () => {
  const app = await fixture();
  try {
    await app.press("id:file:note.txt"); await app.waitFor("id:preview-code");
    expect((await inspect(app)).preview?.text).toContain("fixture exact text");
    await app.press("id:fixture-read-exact"); await waitState(app, (state) => state.exact !== null);
    expect((await inspect(app)).exact).toBe("fixture exact text\r\nsecond line\n");
    await app.press("id:file:binary.bin"); await app.waitFor("id:preview-fallback");
    expect((await inspect(app)).preview?.kind).toBe("message");
    expect(app.texts().join("\n")).toContain("Binary");
    await app.press("id:file:large.txt"); await app.waitFor("id:preview-truncated");
    expect((await inspect(app)).preview?.truncated).toBe(true);
    await app.press("id:file:tiny.png"); await app.waitFor("id:preview-image");
    const image = (await inspect(app)).preview!;
    expect(image.mime).toBe("image/png"); expect(image.dataUrl?.startsWith("data:image/png;base64,")).toBe(true); expect(image.width).toBe(1); expect(image.height).toBe(1);
    await app.host.send("app.fixture.passive", { path: "doc.pdf" });
    await app.waitFor("id:preview-pdf-request");
    expect((await inspect(app)).preview?.dataUrl).toBe(undefined);
    await app.press("id:preview-pdf-request"); await app.waitFor("id:preview-pdf", { timeout: 15000 });
    const pdf = (await inspect(app)).preview!;
    expect(pdf.dataUrl?.startsWith("data:image/png;base64,")).toBe(true); expect(pdf.pages).toBe(2); expect(pdf.page).toBe(1);
    await app.press("id:preview-pdf-next"); await waitState(app, (state) => state.preview?.page === 2);
    expect((await inspect(app)).preview?.width).toBe(400); expect((await inspect(app)).preview?.height).toBe(600);
  } finally { app.dispose(); }
});

test("P3b memo autosave serializes edits, saves component removal, and enforces UTF-8 size", async () => {
  const app = await fixture();
  try {
    await app.press("id:fixture-memo-toggle"); await app.waitFor("id:project-memo-input");
    await app.type("id:project-memo-input", "First 日本語");
    await waitState(app, (state) => state.memo === "First 日本語");
    await app.waitUntil(() => app.texts().includes("Saved automatically"));
    expect(app.texts()).toContain("Saved automatically");
    await app.type("id:project-memo-input", " final");
    await app.press("id:fixture-memo-toggle");
    await waitState(app, (state) => state.memo === "First 日本語 final");
    await app.press("id:fixture-memo-toggle");
    expect(app.value("id:project-memo-input")).toBe("First 日本語 final");
    await app.clear("id:project-memo-input"); await app.paste("id:project-memo-input", { text: "é".repeat(16385) });
    await app.waitUntil(() => app.texts().join("\n").includes("32770 bytes"), { timeout: 10000 });
    expect(app.enabled("id:project-memo-save")).toBe(false);
    expect(app.texts().join("\n")).toContain("32770 bytes");
    expect((await inspect(app)).memo).toBe("First 日本語 final");
    await app.clear("id:project-memo-input"); await app.type("id:project-memo-input", "Within limit");
    await app.press("id:project-memo-save");
    await waitState(app, (state) => state.memo === "Within limit");
  } finally { app.dispose(); }
});
