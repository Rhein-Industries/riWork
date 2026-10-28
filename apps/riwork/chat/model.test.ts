import { expect, test } from "nsh/test";
import type { ChatBody, ChatDelivery, ChatRow } from "riwork/native";
import { answersOf, applyRows, bodyChanges, decisionForKey, detailLines, tailOf, viewport } from "./model";

function row(key: string, title = key): ChatRow {
  return { key, id: key, kind: "shell", depth: 0, title, open: false, disclosure: true, failedCount: 0, running: false, provisional: false, hasBody: false };
}
const patch = (part: Partial<ChatDelivery>): ChatDelivery => ({ kind: "patch", rev: 2, seq: 2, ...part });

test("P4-MODEL-01 a patch keeps unchanged rows identical and splices only what changed", () => {
  const rows = [row("a"), row("b"), row("c")];
  const next = applyRows(rows, patch({ upsert: [row("b", "B")] }));
  expect(next.map((r) => r.title)).toEqual(["a", "B", "c"]);
  expect(next[0] === rows[0] && next[2] === rows[2]).toBe(true);
  expect(applyRows(rows, patch({})) === rows).toBe(true);
  const reordered = applyRows(rows, patch({ remove: ["a"], upsert: [row("d")], order: ["c", "d", "b"] }));
  expect(reordered.map((r) => r.key)).toEqual(["c", "d", "b"]);
  expect(reordered[0] === rows[2]).toBe(true);
  expect(applyRows(rows, { kind: "reset", rev: 1, seq: 0, rows: [row("x")] }).map((r) => r.key)).toEqual(["x"]);
});

test("P4-MODEL-02 streaming bodies splice Markdown from a block and append text, never a whole message", () => {
  const bodies = new Map<string, ChatBody>([
    ["m", { kind: "markdown", blocks: [{ type: "paragraph", content: [{ type: "text", text: "one" }] }, { type: "paragraph", content: [{ type: "text", text: "tw" }] }] }],
    ["r", { kind: "text", text: "thinking", tone: "reasoning" }],
    ["c", { kind: "command", command: "ls", output: "a\n", omitted: 0, running: true }],
  ]);
  const changes = new Map(bodyChanges((key) => bodies.get(key), patch({
    splice: [{ key: "m", from: 1, len: 2, blocks: [{ type: "paragraph", content: [{ type: "text", text: "two" }] }] }],
    append: [{ key: "r", text: " more" }, { key: "c", text: "b\n" }],
    drop: ["gone"],
  })));
  const markdown = changes.get("m") as Extract<ChatBody, { kind: "markdown" }>;
  expect(markdown.blocks.length).toBe(2);
  expect(markdown.blocks[0] === (bodies.get("m") as Extract<ChatBody, { kind: "markdown" }>).blocks[0]).toBe(true);
  expect((changes.get("r") as Extract<ChatBody, { kind: "text" }>).text).toBe("thinking more");
  expect((changes.get("c") as Extract<ChatBody, { kind: "command" }>).output).toBe("a\nb\n");
  expect(changes.has("gone") && changes.get("gone") === undefined).toBe(true);
  // A shrink truncates.
  const shrunk = new Map(bodyChanges((key) => bodies.get(key), patch({ splice: [{ key: "m", from: 0, len: 0, blocks: [] }] })));
  expect((shrunk.get("m") as Extract<ChatBody, { kind: "markdown" }>).blocks.length).toBe(0);
  expect(tailOf("abcdef", 3)).toBe("def");
});

test("P4-MODEL-03 ⏎ allows and ⎋ denies only what the provider offers", () => {
  const all = ["accept", "accept_for_session", "decline", "cancel"];
  expect(decisionForKey("enter", all)).toBe("accept");
  expect(decisionForKey("escape", all)).toBe("decline");
  expect(decisionForKey("escape", ["accept", "cancel"])).toBe("cancel");
  expect(decisionForKey("escape", ["accept"])).toBe(null);
  expect(decisionForKey("enter", ["decline"])).toBe(null);
});

test("P4-MODEL-04 approval detail folds to three lines; answers need every prompt", () => {
  expect(detailLines("a\nb\nc\nd\ne\n", false)).toEqual({ lines: ["a", "b", "c"], hidden: 2 });
  expect(detailLines("a\nb\nc\nd", true).hidden).toBe(0);
  expect(detailLines("", false)).toEqual({ lines: [], hidden: 0 });
  expect(answersOf([["Strict (Recommended)"], []], ["", " no thanks "])).toEqual([["Strict (Recommended)"], ["no thanks"]]);
  expect(answersOf([["x"], []], ["", " "])).toBe(null);
  expect(viewport([5, 2, 9])).toEqual([2, 9]);
  expect(viewport([])).toBe(null);
});
