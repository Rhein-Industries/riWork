import { expect, open, test, type App } from "nsh/test";
import type { FolderDto, ProjectDto } from "riwork/native";

interface State { project: ProjectDto; folders: FolderDto[]; groupId: string; childId: string; memo: string }
let sequence = 0;
async function fixture() {
  const app = await open("./ProjectSettings.fixture.tsx", { platform: "macos", width: 1000, height: 2600,
    host: { params: { key: `project-settings-${Date.now()}-${++sequence}` }, handlers: { "app.fixture.state": null } } });
  await app.waitFor("id:project-settings-name", { timeout: 15000 });
  return app;
}
async function inspect(app: App): Promise<State> {
  const count = app.host.posts().length;
  await app.host.send("app.fixture.inspect");
  await app.waitUntil(() => app.host.posts().length > count);
  return app.host.posts().filter((post) => post.name === "app.fixture.state").slice(-1)[0].data as State;
}
async function waitState(app: App, predicate: (state: State) => boolean) {
  let state = await inspect(app);
  for (let attempt = 0; attempt < 80 && !predicate(state); attempt++) { await app.wait(50); state = await inspect(app); }
  if (!predicate(state)) throw new Error(`Project settings did not settle: ${JSON.stringify({ state, visibleName: app.value("id:project-settings-name"), visibleMemo: app.value("id:project-memo-input"), texts: app.texts() })}`);
  return state;
}

test("P6a Project settings explicit metadata survives immediate account saves and external refresh", async () => {
  const app = await fixture();
  try {
    const original = await inspect(app);
    expect(app.texts()).toContain(`${original.folders.find((folder) => folder.id === original.groupId)!.name}/日本語 child`);
    await app.clear("id:project-settings-name"); await app.paste("id:project-settings-name", { text: "  Unsaved 日本語  " });
    await app.press(`id:${original.childId}`);
    expect((await inspect(app)).project.name).toBe("Original project");
    expect((await inspect(app)).project.folderId ?? null).toBe(null);
    await app.press("id:project-codex-system");
    await waitState(app, (state) => state.project.codexAccount.source === "system_default");
    expect(app.value("id:project-settings-name")).toBe("  Unsaved 日本語  ");
    expect(app.texts()).toContain("Unsaved");
    expect(app.texts()).toContain("Save changes  ↵ / CMD+S");
    expect(app.texts().includes("Codex account saved")).toBe(false);
    await app.host.send("app.fixture.external");
    await waitState(app, (state) => state.project.name === "Changed elsewhere");
    expect(app.value("id:project-settings-name")).toBe("  Unsaved 日本語  ");
    await app.waitUntil(() => app.enabled("id:project-settings-save"));
    await app.submit("id:project-settings-name");
    await waitState(app, (state) => state.project.name === "Unsaved 日本語" && state.project.folderId === original.childId);
    await app.waitUntil(() => app.texts().includes("Settings saved"));
    expect(app.value("id:project-settings-name")).toBe("Unsaved 日本語");
    expect(app.texts()).toContain("Local project");
    await app.waitUntil(() => app.enabled("id:project-codex-inherit"));
    await app.press("id:project-codex-inherit");
    await waitState(app, (state) => state.project.codexAccount.source === "inherit");
    await app.waitUntil(() => app.texts().includes("Codex account saved"));
  } finally { app.dispose(); }
});

test("P6a Project settings inline subfolders wait for explicit Save and Cmd+S saves memo too", async () => {
  const app = await fixture();
  try {
    const initial = await inspect(app);
    await app.press(`id:${initial.groupId}`);
    await app.type("id:project-settings-folder-name", "Created child");
    await app.press("id:project-settings-create-folder");
    const created = await waitState(app, (state) => state.folders.some((folder) => folder.name === "Created child" && folder.parentId === initial.groupId));
    const folder = created.folders.find((folder) => folder.name === "Created child" && folder.parentId === initial.groupId)!;
    expect(created.project.folderId ?? null).toBe(null);
    await app.waitUntil(() => app.value("id:project-settings-folder-name") === "" && app.enabled("id:project-settings-save"));
    expect(app.value("id:project-settings-folder-name")).toBe("");
    expect(app.texts()).toContain("Unsaved");
    await app.clear("id:project-settings-name"); await app.type("id:project-settings-name", "Saved together");
    expect(app.value("id:project-settings-name")).toBe("Saved together");
    await app.host.send("app.fixture.memo");
    await app.paste("id:project-memo-input", { text: "Memo saved with project" });
    await app.keyboard.press("mod+s");
    await waitState(app, (state) => state.project.name === "Saved together" && state.project.folderId === folder.id && state.memo === "Memo saved with project");
    expect(app.texts()).toContain("Settings saved");
  } finally { app.dispose(); }
});
