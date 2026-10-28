import { expect, test } from "nsh/test";
import type { FolderDto } from "riwork/native";
import { ProjectDraft, SAVE_PROJECT_PROMPT } from "./project_model";

const project = { id: "isolated-project", name: "Original", folderId: null as string | null };
const folder = (id: string, name: string, parentId: string | null = null): FolderDto => ({ id, name, parentId, createdAt: 1 });

test("P6a Project settings immediate choices and external refresh keep explicit metadata edits pending", () => {
  const draft = new ProjectDraft(project);
  draft.editName("  Unsaved 日本語  "); draft.chooseFolder("child");
  draft.refresh({ ...project, name: "Elsewhere" }, [folder("child", "Child")]);
  draft.immediateSaved("Codex account saved");
  expect(draft.name).toBe("  Unsaved 日本語  ");
  expect(draft.folderId).toBe("child");
  expect(draft.pending).toBe(true); expect(draft.message()).toBe(SAVE_PROJECT_PROMPT);
  const request = draft.request();
  expect(request.name).toBe("  Unsaved 日本語  ");
  draft.saved(request, { ...project, name: "Unsaved 日本語", folderId: "child" });
  expect(draft.name).toBe("Unsaved 日本語"); expect(draft.pending).toBe(false);
  expect(draft.message()).toBe("Settings saved");
  draft.immediateSaved("Delegation policy saved"); expect(draft.message()).toBe("Delegation policy saved");
});

test("P6a Project settings metadata receipts preserve edits made during save", () => {
  const draft = new ProjectDraft(project);
  draft.editName("B"); const request = draft.request();
  draft.editName("Original"); draft.chooseFolder("second");
  draft.saved(request, { ...project, name: "B" });
  expect(draft.name).toBe("Original"); expect(draft.folderId).toBe("second");
  expect(draft.pending).toBe(true); expect(draft.message()).toBe(SAVE_PROJECT_PROMPT);
  draft.saved(draft.request(), { ...project, folderId: "second" });
  expect(draft.pending).toBe(false);
});

test("P6a Project settings refresh follows untouched metadata and drops deleted folder choices", () => {
  const draft = new ProjectDraft(project);
  draft.refresh({ ...project, name: "Renamed", folderId: "one" }, [folder("one", "One")]);
  expect(draft.name).toBe("Renamed"); expect(draft.folderId).toBe("one");
  draft.editName("Still typing"); draft.chooseFolder("two");
  draft.refresh({ ...project, folderId: "one" }, [folder("one", "One")]);
  expect(draft.name).toBe("Still typing"); expect(draft.folderId).toBe(null);
});

test("P6a Project settings folder A-B-A edits survive a B snapshot before its save receipt", () => {
  const folders = [folder("A", "A"), folder("B", "B")];
  const draft = new ProjectDraft({ ...project, folderId: "A" });
  draft.chooseFolder("B"); const request = draft.request();
  draft.chooseFolder("A");
  draft.refresh({ ...project, folderId: "B" }, folders);
  draft.saved(request, { ...project, folderId: "B" });
  expect(draft.folderId).toBe("A"); expect(draft.pending).toBe(true);
  expect(draft.message()).toBe(SAVE_PROJECT_PROMPT);
  draft.saved(draft.request(), { ...project, folderId: "A" });
  expect(draft.folderId).toBe("A"); expect(draft.folderTouched).toBe(false);
  expect(draft.pending).toBe(false); expect(draft.message()).toBe("Settings saved");
});
