import { expect, test } from "nsh/test";
import { treeRows } from "./project_tree";

test("deep project folders count each descendant and retain collapse semantics", () => {
  const count = 10000;
  const folders = Array.from({ length: count }, (_, index) => ({
    id: `f${index}`, name: String(index).padStart(5, "0"), parentId: index ? `f${index - 1}` : null,
  }));
  const projects = folders.map((folder) => ({ id: `p${folder.id}`, name: folder.name, root: "/fixture", folderId: folder.id }));
  const rows = treeRows(projects, folders, "", new Set());
  expect(rows.length).toBe(count * 2 + 2);
  const folderRows = rows.filter((row) => row.kind === "folder");
  expect(folderRows.length).toBe(count + 1);
  for (let index = 0; index < count; index++) {
    const row = folderRows[index + 1];
    expect(row.kind === "folder" && row.count).toBe(count - index);
    expect(row.depth).toBe(index);
  }
  const collapsed = treeRows(projects, folders, "", new Set(["f1"]));
  expect(collapsed.map((row) => row.id)).toEqual(["folder-unfiled", "empty-unfiled", "folder-f0", "project-pf0", "folder-f1"]);
  expect(collapsed[4].kind === "folder" && collapsed[4].count).toBe(count - 1);
});

test("cyclic imports break at the first name-ranked cycle member and keep project order", () => {
  const folders = [
    { id: "outside", name: "A outside", parentId: "b" },
    { id: "b", name: "C second", parentId: "a" },
    { id: "a", name: "B first", parentId: "b" },
    { id: "orphan", name: "D orphan", parentId: "missing" },
    { id: "self", name: "E self", parentId: "self" },
  ];
  const projects = [
    { id: "z", name: "Z", root: "/fixture", folderId: "b" },
    { id: "a", name: "A", root: "/fixture", folderId: "b" },
    { id: "orphan", name: "Orphan", root: "/fixture", folderId: "missing" },
  ];
  const rows = treeRows(projects, folders, "", new Set());
  expect(rows.map((row) => row.id)).toEqual([
    "folder-unfiled", "project-orphan", "folder-a", "folder-b", "project-z", "project-a",
    "folder-outside", "empty-outside", "folder-orphan", "empty-orphan", "folder-self", "empty-self",
  ]);
  expect(rows.filter((row) => row.kind === "folder").map((row) => row.count)).toEqual([1, 2, 2, 0, 0, 0]);
});
