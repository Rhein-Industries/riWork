import type { ChatBody, ChatDelivery, ChatPanel, ChatRow } from "riwork/native";

/** Rows after a delivery. Unchanged rows keep their object identity: the List
 * diffs `each` by identity, so a patch that touches one row splices one row. */
export function applyRows(rows: readonly ChatRow[], delivery: ChatDelivery): ChatRow[] {
  if (delivery.kind === "reset") return delivery.rows ?? [];
  const upsert = delivery.upsert ?? [];
  const remove = delivery.remove ?? [];
  if (!upsert.length && !remove.length && !delivery.order) return rows as ChatRow[];
  const byKey = new Map(rows.map((row) => [row.key, row]));
  for (const key of remove) byKey.delete(key);
  for (const row of upsert) byKey.set(row.key, row);
  const order = delivery.order ?? rows.map((row) => row.key);
  const next: ChatRow[] = [];
  for (const key of order) {
    const row = byKey.get(key);
    if (row) next.push(row);
  }
  return next;
}

/** Body changes of a delivery, as `[key, body | undefined]`: a reset or `drop`
 * forgets, `bodies` replaces, `splice` rewrites Markdown from a block and
 * `append` extends text. Streaming never resends a whole message. */
export function bodyChanges(
  current: (key: string) => ChatBody | undefined,
  delivery: ChatDelivery,
): [string, ChatBody | undefined][] {
  const changed = new Map<string, ChatBody | undefined>();
  const read = (key: string) => changed.has(key) ? changed.get(key) : current(key);
  for (const key of delivery.drop ?? []) changed.set(key, undefined);
  for (const [key, body] of Object.entries(delivery.bodies ?? {})) changed.set(key, body);
  for (const splice of delivery.splice ?? []) {
    const body = read(splice.key);
    if (body?.kind !== "markdown") continue;
    const blocks = body.blocks.slice(0, splice.from).concat(splice.blocks);
    changed.set(splice.key, { kind: "markdown", blocks: blocks.slice(0, splice.len) });
  }
  for (const append of delivery.append ?? []) {
    const body = read(append.key);
    if (body?.kind === "text") changed.set(append.key, { ...body, text: body.text + append.text });
    else if (body?.kind === "command") changed.set(append.key, { ...body, output: tailOf(body.output + append.text) });
  }
  return [...changed];
}

/** Command output kept on screen: the end, at most this many UTF-16 units. */
export const OUTPUT_KEEP = 64 * 1024;

export function tailOf(text: string, keep = OUTPUT_KEEP): string {
  return text.length <= keep ? text : text.slice(text.length - keep);
}

/** The first and last visible indices of a List viewability report. */
export function viewport(indices: readonly number[]): [number, number] | null {
  if (!indices.length) return null;
  let first = indices[0], last = indices[0];
  for (const index of indices) { if (index < first) first = index; if (index > last) last = index; }
  return [first, last];
}

export type Decision = "accept" | "accept_for_session" | "decline" | "cancel";

/** The decision ⏎ or ⎋ makes among those offered; none when not offered
 * (CHATT-207): ⏎ allows, ⎋ denies, or stops when denying is not offered. */
export function decisionForKey(key: "enter" | "escape", offered: readonly string[]): Decision | null {
  const wanted: Decision = key === "enter" ? "accept" : offered.includes("decline") ? "decline" : "cancel";
  return offered.includes(wanted) ? wanted : null;
}

/** A composer key never answers an approval within this long of a send (CHATC-026). */
export const APPROVAL_GUARD_MS = 700;

/** The approval detail folded to its first lines (CHATT-206). */
export function detailLines(detail: string, expanded: boolean): { lines: string[]; hidden: number } {
  const trimmed = detail.replace(/\s+$/, "");
  const lines = trimmed ? trimmed.split("\n") : [];
  if (expanded || lines.length <= 3) return { lines, hidden: 0 };
  return { lines: lines.slice(0, 3), hidden: lines.length - 3 };
}

/** One answer per prompt: picked option labels, then the trimmed typed text. */
export function answersOf(picked: readonly (readonly string[])[], typed: readonly string[]): string[][] | null {
  const answers = picked.map((labels, index) => {
    const text = (typed[index] ?? "").trim();
    return text ? [...labels, text] : [...labels];
  });
  return answers.every((answer) => answer.length > 0) ? answers : null;
}

export const SEND_TIP = "Send · ⏎ · ⇧⏎ new line";
export const STEER_TIP = "Send · ⏎ steers the turn · ⇧⏎ new line · ⌘. interrupts";
export const STOP_TIP = "Interrupt · ⌘.";

export function emptyPanel(chatId: string): ChatPanel {
  return {
    chatId, title: "Chat", state: "starting", link: "connecting", running: false, empty: true,
    approvals: [], questions: [], notices: [], mode: "normal", failures: [],
  };
}
