import { Scroll, Text, View, cleanup, computed, effect, signal, type YNode } from "nsh";
import type { ChatHandle, ChatSuggestion, ChatSuggestionResult } from "riwork/native";
import { Icon, type IconName } from "../Icon";
import type { InterfaceTheme } from "../appearance";

type Selection = { start: number; end: number };
type Open = { text: string; caret: number; basis: string; start: number; sigil: string; loading: boolean; result?: ChatSuggestionResult; error?: string };

// Only a deliberate sigil asks native code for a catalog. Native suggest::trigger
// remains authoritative (including token ends and replacement coordinates).
const whitespace = /[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]/;
function candidate(text: string, caret: number) {
  if (caret < 0 || caret > text.length) return null;
  let start = caret;
  while (start > 0 && !whitespace.test(text[start - 1])) start--;
  if (start === caret) return null;
  const sigil = text[start];
  if (sigil !== "/" && sigil !== "@" && sigil !== "&") return null;
  if (sigil === "/") {
    if ([...text.slice(0, start)].some((character) => !whitespace.test(character))) return null;
    let end = caret;
    while (end < text.length && !whitespace.test(text[end])) end++;
    if (text.slice(start + 1, end).includes("/")) return null;
  }
  return { start, sigil };
}

/** All edits are synchronous and checked against the exact draft/caret that
 * requested the list. A late catalog never inserts into a newer draft. */
export function createSuggestions(props: {
  handle: ChatHandle;
  value: () => string;
  selection: () => Selection;
  focused: () => boolean;
  composing: () => boolean;
  active: () => boolean;
  basis?: () => string;
  replace: (text: string, selection: Selection) => void;
  focus: () => void;
}) {
  const open = signal<Open | null>(null);
  const highlighted = signal(0);
  const dismissed = signal<number | null>(null);
  // Streaming updates can replace the panel without changing whether the
  // composer accepts input. Catalog reads depend on these scalar values only.
  const enabled = computed(props.active);
  const basis = computed(() => props.basis?.() ?? "");
  const caret = computed(() => props.selection().end);
  const collapsed = computed(() => props.selection().start === props.selection().end);
  let generation = 0;
  let alive = true;
  cleanup(() => { alive = false; generation++; });

  effect(() => {
    const text = props.value(), at = caret(), context = basis();
    const found = enabled() && props.focused() && !props.composing() && collapsed()
      ? candidate(text, at) : null;
    const request = ++generation;
    if (!found) { open.set(null); dismissed.set(null); return; }
    if (dismissed() === found.start) { open.set(null); return; }
    highlighted.set(0);
    open.set({ text, caret: at, basis: context, ...found, loading: true });
    const timer = setTimeout(() => {
      void props.handle.suggestions(text, at).then((result) => {
        if (!alive || request !== generation) return;
        open.set(result ? { text, caret: at, basis: context, ...found, loading: false, result } : null);
      }).catch((error) => {
        if (alive && request === generation) open.set({ text, caret: at, basis: context, ...found, loading: false, error: String(error) });
      });
    }, 75);
    return () => { clearTimeout(timer); generation++; };
  });

  function current() {
    const value = open(), selected = props.selection();
    return value && enabled() && !props.composing() && basis() === value.basis && props.value() === value.text
      && selected.start === selected.end && selected.end === value.caret ? value : null;
  }
  function close() {
    const value = open();
    if (value) dismissed.set(value.start);
    generation++;
    open.set(null);
  }
  function accept(index = highlighted()): boolean {
    const value = current();
    if (!value) return false;
    // While the first listing is arriving, Return must not accidentally send
    // the unfinished token. No selection is guessed from an old response.
    if (value.loading) return true;
    const result = value.result, row = result?.items[index];
    if (!result || !row) return false;
    const rest = value.text.slice(result.end);
    let head = value.text.slice(0, result.start) + row.insert;
    if (!rest.startsWith(" ")) head += " ";
    const caret = head.length + (rest.startsWith(" ") ? 1 : 0);
    close();
    props.replace(head + rest, { start: caret, end: caret });
    props.focus();
    return true;
  }
  // Input.onSubmit is native and preserves marked text. The SDK's global key
  // monitor cannot synchronously exempt an IME candidate session, so this
  // helper deliberately does not intercept arrows, Tab, Escape or Return.
  return { open, highlighted, accept, close };
}

export type SuggestionsController = ReturnType<typeof createSuggestions>;

export function Suggestions(props: { control: SuggestionsController; theme: () => InterfaceTheme }) {
  const theme = props.theme, control = props.control;
  const rows = () => control.open()?.result?.items ?? [];
  const current = () => rows()[control.highlighted()];
  const text = (size = 11, muted = false) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color: muted ? theme().palette.muted : theme().palette.text });
  const rowHeight = () => 40 * theme().scale;
  let scroll: YNode | undefined;
  let offset = 0, viewport = 160;
  effect(() => {
    const top = control.highlighted() * rowHeight(), bottom = top + rowHeight();
    if (top < offset) { offset = top; scroll?.scrollTo({ y: top }); }
    else if (bottom > offset + viewport) { offset = bottom - viewport; scroll?.scrollTo({ y: offset }); }
  });
  const label = () => control.open()?.sigil === "@" ? "Files in this chat’s folder" : control.open()?.sigil === "&" ? "Mention a model" : "Commands";
  const icon = (item: ChatSuggestion): IconName => item.kind === "folder" ? "folder" : item.kind === "file" ? "file" : item.kind === "model" ? "chat" : "terminal";
  return <View>{control.open() && <View testID="chat-suggestions" accessibilityLabel={label()}
    style={{ marginBottom: 6, borderWidth: 1, borderColor: theme().palette.divider, borderRadius: 7, backgroundColor: theme().palette.panelActive, overflow: "hidden" }}>
    <View style={{ flexDirection: "row", alignItems: "center", paddingHorizontal: 8, paddingVertical: 5, gap: 6 }}>
      <Text style={{ ...text(10, true), flex: 1 }}>{label()}</Text>
      <Text style={text(10, true)}>Click or ↵ to insert</Text>
      <View role="button" focusable={false} accessibilityLabel="Close suggestions" onPress={control.close} style={{ padding: 3 }}><Icon name="close" size={11 * theme().scale} color={theme().palette.muted} /></View>
    </View>
    {control.open()?.loading && <Text role="text" style={{ ...text(11, true), padding: 8 }}>Loading suggestions…</Text>}
    {!!control.open()?.error && <Text role="text" style={{ ...text(11, true), padding: 8 }}>{`Suggestions unavailable: ${control.open()?.error}`}</Text>}
    {!control.open()?.loading && !control.open()?.error && !rows().length && <Text style={{ ...text(11, true), padding: 8 }}>No matching suggestions</Text>}
    {!!rows().length && <Scroll ref={(node) => { scroll = node; offset = 0; }} style={{ maxHeight: 160 * theme().scale }}
      onLayout={(bounds) => { viewport = bounds.size.height; }} onScroll={(event) => { offset = event.offset.y; }}>
      {rows().map((row, index) => <View role="button" focusable={false} accessibilityLabel={`${row.title}, ${row.detail}`} accessibilityState={{ selected: control.highlighted() === index }}
        onHoverIn={() => control.highlighted.set(index)} onPress={() => { control.accept(index); }}
        style={{ height: rowHeight(), paddingHorizontal: 8, flexDirection: "row", alignItems: "center", gap: 8, backgroundColor: control.highlighted() === index ? theme().palette.panel : "transparent" }}>
        <Icon name={icon(row)} size={14 * theme().scale} color={control.highlighted() === index ? theme().palette.cyan : theme().palette.muted} />
        <View style={{ flex: 1, gap: 2 }}><Text numberOfLines={1} style={text()}>{row.title}</Text><Text numberOfLines={1} style={text(10, true)}>{row.detail}</Text></View>
      </View>)}
    </Scroll>}
    {!!current() && <View style={{ padding: 8, gap: 3, borderTopWidth: 1, borderColor: theme().palette.divider }}>
      {!!current()?.body.length && <Text numberOfLines={2} style={text(10, true)}>{current()?.body.join(" · ")}</Text>}
      <Text numberOfLines={2} style={text(10, true)}>{current()?.effect}</Text>
    </View>}
    {control.open()?.result?.truncated && <Text style={{ ...text(10, true), paddingHorizontal: 8, paddingBottom: 6 }}>This folder’s listing reached its scan limit.</Text>}
  </View>}</View>;
}
