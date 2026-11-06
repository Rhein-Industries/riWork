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

test("P4-MODEL-05 Markdown splice keeps slice bounds and owns its array", () => {
  type Markdown = Extract<ChatBody, { kind: "markdown" }>;
  const body: Markdown = { kind: "markdown", blocks: [
    { type: "paragraph", content: [{ type: "text", text: "one" }] },
    { type: "paragraph", content: [{ type: "text", text: "two" }] },
    { type: "paragraph", content: [{ type: "text", text: "three" }] },
  ] };
  const added: Markdown["blocks"] = [{ type: "paragraph", content: [{ type: "text", text: "tail" }] }];
  Object.freeze(body.blocks);
  Object.freeze(added);
  const bounds = [-Infinity, -5, -1, -0, 0, 0.5, 1, 1.5, 3, 4, 9, Infinity, NaN];
  for (const from of bounds) for (const len of bounds) {
    const expected = body.blocks.slice(0, from).concat(added).slice(0, len);
    const changes = bodyChanges(() => body, patch({ splice: [{ key: "m", from, len, blocks: added }] }));
    const next = changes[0][1] as Markdown;
    expect(next.blocks).toEqual(expected);
    expect(next.blocks !== body.blocks && next.blocks !== added).toBe(true);
    expect(next.blocks.every((block, index) => block === expected[index])).toBe(true);
  }
  expect(body.blocks.length).toBe(3);
  expect(added.length).toBe(1);
});

test("P4-MODEL-06 repeated Markdown splices compose immutably after drops and replacements", () => {
  type Markdown = Extract<ChatBody, { kind: "markdown" }>;
  const blocks: Markdown["blocks"] = ["one", "two", "three"].map((text) => ({ type: "paragraph", content: [{ type: "text", text }] }));
  const first: Markdown["blocks"] = [{ type: "paragraph", content: [{ type: "text", text: "first" }] }];
  const final: Markdown["blocks"] = [{ type: "paragraph", content: [{ type: "text", text: "final" }] }];
  Object.freeze(blocks); Object.freeze(first); Object.freeze(final);
  const replacement: Markdown = { kind: "markdown", blocks };
  const changes = bodyChanges((key) => key === "text" ? { kind: "text", text: "before", tone: "reasoning" } : undefined, patch({
    drop: ["m", "gone", "m"],
    bodies: { m: replacement },
    splice: [
      { key: "m", from: 2, len: 3, blocks: first },
      { key: "m", from: 1, len: 2, blocks: final },
      { key: "m", from: 2, len: 2, blocks: [] },
      { key: "gone", from: 0, len: 1, blocks: first },
    ],
    append: [{ key: "text", text: " after" }, { key: "m", text: "ignored" }],
  }));
  expect(changes.map(([key]) => key)).toEqual(["m", "gone", "text"]);
  const next = changes[0][1] as Markdown;
  expect(next.blocks).toEqual([blocks[0], final[0]]);
  expect(next.blocks[0] === blocks[0] && next.blocks[1] === final[0]).toBe(true);
  expect(next !== replacement && next.blocks !== blocks && next.blocks !== final).toBe(true);
  expect(changes[1][1]).toBe(undefined);
  expect(changes[2][1]).toEqual({ kind: "text", text: "before after", tone: "reasoning" });
  expect(blocks.length).toBe(3);
});

test("P4-MODEL-07 duplicate row keys retain last-object and remove-before-upsert semantics", () => {
  const first = row("a", "first"), middle = row("b"), last = row("a", "last");
  const rows = [first, middle, last];
  Object.freeze(rows);
  const canonical = applyRows(rows, patch({ upsert: [row("unknown")] }));
  expect(canonical.length).toBe(3);
  expect(canonical[0] === last && canonical[1] === middle && canonical[2] === last).toBe(true);
  expect(canonical !== rows).toBe(true);
  const revived = row("a", "revived");
  const next = applyRows(rows, patch({ remove: ["a", "missing"], upsert: [row("a", "older"), revived] }));
  expect(next.length).toBe(3);
  expect(next[0] === revived && next[1] === middle && next[2] === revived).toBe(true);
  expect(rows[0] === first && rows[2] === last).toBe(true);
});

test("P4-MODEL-08 explicit row order and resets preserve identities without retaining unseen rows", () => {
  const rows = [row("a"), row("b")], unseen = row("new");
  const next = applyRows(rows, patch({ upsert: [unseen] }));
  expect(next[0] === rows[0] && next[1] === rows[1]).toBe(true);
  expect(applyRows(next, patch({ order: ["new"] }))).toEqual([]);
  const repeated = applyRows(rows, patch({ upsert: [unseen], order: ["b", "new", "missing", "b"] }));
  expect(repeated.length).toBe(3);
  expect(repeated[0] === rows[1] && repeated[1] === unseen && repeated[2] === rows[1]).toBe(true);
  const empty = applyRows(rows, patch({ order: [] }));
  expect(empty.length === 0 && empty !== rows).toBe(true);
  expect(applyRows(rows, patch({ append: [{ key: "body", text: "delta" }] })) === rows).toBe(true);
  const reset = [unseen];
  expect(applyRows(rows, { kind: "reset", rev: 3, seq: 3, rows: reset }) === reset).toBe(true);
  expect(applyRows(rows, { kind: "reset", rev: 4, seq: 4 })).toEqual([]);
});
