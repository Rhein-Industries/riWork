import { expect, test } from "nsh/test";
import { fileRows, reconcileListings, utf8Bytes, memoBytes, omittedLabel, type DirectoryListing, type FileEntry } from "./files_model";

const root = "/tmp/isolated files";
const entry = (relativePath: string, kind: FileEntry["kind"] = "file"): FileEntry => ({ name: relativePath.split("/").slice(-1)[0], path: `${root}/${relativePath}`, relativePath, kind, identity: kind === "file" ? `identity:${relativePath}` : undefined });
const listing = (path: string, entries: FileEntry[]): DirectoryListing => ({ root, path, entries, omitted: 0, scanLimited: false });

test("Files loaded filter retains nested Unicode ancestors and hides dot components", () => {
  const nested = entry("nested dir", "directory"), unicode = entry("nested dir/日本語.md"), dot = entry(".hidden", "directory"), secret = entry(".hidden/日本語.md");
  const state = new Map([[root, listing(root, [nested, dot, entry("binary.bin")])], [nested.path, listing(nested.path, [unicode])], [dot.path, listing(dot.path, [secret])]]);
  expect(fileRows(root, state, new Set(), "日本", false).map((row) => [row.entry.relativePath, row.depth])).toEqual([["nested dir", 0], ["nested dir/日本語.md", 1]]);
  expect(fileRows(root, state, new Set(), "日本", true).map((row) => row.entry.relativePath)).toEqual(["nested dir", "nested dir/日本語.md", ".hidden", ".hidden/日本語.md"]);
  expect(fileRows(root, state, new Set(), "", false).map((row) => row.entry.relativePath)).toEqual(["nested dir", "binary.bin"]);
  expect(fileRows(root, state, new Set([nested.path]), "", false).map((row) => row.entry.relativePath)).toEqual(["nested dir", "nested dir/日本語.md", "binary.bin"]);
});

test("Files replacement by a symlink evicts all cached descendants", () => {
  const folder = entry("folder", "directory"), child = entry("folder/child", "directory");
  const previous = new Map([[root, listing(root, [folder])], [folder.path, listing(folder.path, [child])], [child.path, listing(child.path, [entry("folder/child/content.txt")])]]);
  const result = reconcileListings(previous, listing(root, [entry("folder", "symlink")]));
  expect([...result.keys()]).toEqual([root]);
  expect(fileRows(root, result, new Set([folder.path, child.path]), "", true).map((row) => row.entry.kind)).toEqual(["symlink"]);
});

test("Files model counts memo limits in UTF-8 bytes and reports capped listings", () => {
  expect(utf8Bytes("A日本語😀")).toBe(14);
  expect(utf8Bytes("é".repeat(16384))).toBe(32768);
  expect(memoBytes("A\r\nB\rC\x00\x7f\x85\t")).toBe(6);
  expect(omittedLabel(1, false)).toBe("1 more entry not shown");
  expect(omittedLabel(5000, true)).toBe("5000+ more entries not shown");
});
