import { expect, test } from "nsh/test";
import type { FolderDto, ProjectDto, ShellSessionDto, StoreState, TaskDto, WorktreeDto } from "riwork/native";
import { EMPTY_STORE, countLabel, matches, projectRows, shellLabel, shellMatches, taskMatches, visibleWorktrees } from "./panel_model";

function project(id: string, name: string, folderId: string | null = null, root = `/fixture/${name}`): ProjectDto {
  return { id, name, root, folderId, createdAt: 1, repositoryRoots: [], notifyOnAgentDone: true, wakeOrchestratorsOnAgentDone: true, codexAccount: { source: "inherit" }, orchestrator: {}, delegationPolicy: {} };
}
function folder(id: string, name: string, parentId: string | null = null): FolderDto { return { id, name, parentId, createdAt: 1 }; }
function fixture(): StoreState {
  return { ...EMPTY_STORE, projects: [project("one", "Alpha", "child"), project("two", "Beta"), project("three", "Gamma", "root")], projectFolders: [folder("root", "Work"), folder("child", "Nested", "root")] };
}
function tree(id: string, path: string, isPrimary: boolean, present = true): WorktreeDto {
  return { id, projectId: "one", path, branch: id, isPrimary, present, repositoryRoot: null, createdAt: 1 };
}

test("project browsing preserves nested subtree counts and per-window collapse", () => {
  const state = fixture();
  const rows = projectRows(state, "", new Set(), "name", false);
  expect(rows.map((row) => row.id)).toEqual(["folder-unfiled", "project-two", "folder-root", "project-three", "folder-child", "project-one"]);
  expect(rows.filter((row) => row.kind === "folder").map((row) => row.count)).toEqual([1, 2, 1]);
  expect(projectRows(state, "", new Set(["root"])).map((row) => row.id)).toEqual(["folder-unfiled", "project-two", "folder-root"]);
  expect(projectRows(state, "", new Set()).length).toBe(6);
});

test("project search is flat, ignores folders and finds hidden projects with names ranked first", () => {
  const state = fixture();
  state.projects.push(project("path-hit", "Zulu", null, "/fixture/Alpha"));
  expect(projectRows(state, " alpha ", new Set(["root"]), "name", true).map((row) => row.id)).toEqual(["project-one", "project-path-hit"]);
  expect(projectRows(state, "Nested", new Set()).length).toBe(0);
  expect(projectRows(state, "root", new Set()).length).toBe(0);
});

test("orphan and cyclic folders appear once and empty folders remain navigable", () => {
  const state = fixture();
  state.projectFolders = [folder("b", "Beta", "a"), folder("a", "Alpha", "b"), folder("orphan", "Orphan", "missing")];
  const rows = projectRows(state, "", new Set());
  expect(rows.filter((row) => row.kind === "folder").map((row) => row.id)).toEqual(["folder-unfiled", "folder-a", "folder-b", "folder-orphan"]);
  expect(rows.filter((row) => row.kind === "empty").map((row) => row.id)).toEqual(["empty-b", "empty-orphan"]);
});

test("worktree presence only removes missing linked navigation rows", () => {
  const state = fixture();
  state.worktrees = [tree("primary", "/fixture", true, false), tree("missing", "/fixture/missing", false, false), tree("linked", "/fixture/linked", false)];
  expect(visibleWorktrees(state, "one").map((tree) => tree.id)).toEqual(["primary", "linked"]);
  expect(state.worktrees.length).toBe(3);
  state.worktrees[1].present = true;
  expect(visibleWorktrees(state, "one").map((tree) => tree.id)).toEqual(["primary", "missing", "linked"]);
});

test("task filtering includes exact status and assigned worktree while shell filtering follows deepest cwd", () => {
  const state = fixture();
  state.worktrees = [tree("main", "/fixture", true), tree("feature", "/fixture/linked", false)];
  const task: TaskDto = { id: "exact-task", projectId: "one", title: "Implement panel", details: "Stored detail", status: "in_progress", worktreeId: "feature", createdAt: 1, updatedAt: 1 };
  expect(taskMatches(task, state, " IN_PROGRESS ")).toBe(true);
  expect(taskMatches(task, state, "feature")).toBe(true);
  expect(taskMatches(task, state, "stored detail")).toBe(true);
  expect(taskMatches(task, state, "missing")).toBe(false);
  const shell = { id: "exact-shell", projectId: "one", worktreeId: "main", cwd: "/fixture/linked/deep", alive: false, command: "build widgets", harness: "codex" } as ShellSessionDto;
  expect(shellLabel(shell, state)).toBe("feature");
  expect(shellMatches(shell, state, "exited")).toBe(true);
  expect(shellMatches(shell, state, "widgets")).toBe(true);
  expect(shellMatches(shell, state, "live")).toBe(false);
  expect(shellMatches(shell, state, "shell")).toBe(true);
});

test("header counts and trimming are independent of rendered folder rows", () => {
  expect(countLabel(1, 1, "project")).toBe("1 project");
  expect(countLabel(1, 3, "project")).toBe("1 of 3 projects");
  expect(countLabel(0, 0, "session")).toBe("0 sessions");
  expect(matches(" aLpHa ", "Alpha")).toBe(true);
  expect(matches("   ", null)).toBe(true);
});
