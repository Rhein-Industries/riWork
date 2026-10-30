import { Clipboard, Text, View, cleanup, host, signal, type YNode } from "nsh";
import { Editor, type EditorRef } from "nsh/base";
import { space, type InterfaceTheme } from "../appearance";
import { utf8Bytes } from "../files_model";
import { SideHeader, escapeWithin, type FocusWithin } from "./SidePanel";

export const MAX_SNIPPET = 256 * 1024;
export interface CodeSnippet { code: string; language?: string; meta?: string }

function filename(snippet: CodeSnippet) {
  const title = snippet.meta?.trim().split(/[\\/]/).pop()?.replace(/[\x00-\x1f\x7f-\x9f:]/g, "").slice(0, 100);
  if (title && title !== "." && title !== "..") return title;
  const extensions: Record<string, string> = { rust: "rs", rs: "rs", python: "py", py: "py", javascript: "js", js: "js", typescript: "ts", ts: "ts", tsx: "tsx", jsx: "jsx", json: "json", html: "html", css: "css", sh: "sh", shell: "sh", bash: "sh", zsh: "sh", fish: "fish", powershell: "ps1", pwsh: "ps1", swift: "swift", yaml: "yaml", yml: "yaml", mermaid: "mmd", sql: "sql" };
  return `snippet.${extensions[snippet.language?.toLowerCase() ?? ""] ?? "txt"}`;
}

export async function saveSnippet(snippet: CodeSnippet, directory?: string): Promise<{ saved: boolean; path?: string }> {
  if (utf8Bytes(snippet.code) > MAX_SNIPPET) throw new Error("Reader/export limit: 256 KiB");
  return host.call("system.exportSnippet", { name: filename(snippet), text: snippet.code, directory }) as Promise<{ saved: boolean; path?: string }>;
}

/** A captured scratch editor, in the chat tab's side panel. Its text is never
 * written into the transcript, composer, or a project file without a separate
 * explicit Copy or Save. */
export function CodeReader(props: {
  snippet: CodeSnippet;
  directory?: string;
  theme: () => InterfaceTheme;
  active: () => boolean;
  focus: FocusWithin;
  onClose: () => void;
  /** Hears how to close it: the tab asks before it shows another panel in its place. */
  guard?: (close: (then: () => void) => void) => void;
}) {
  const source = props.snippet.code;
  const draft = signal(source), busy = signal(false), confirming = signal(false), message = signal("");
  let editor: EditorRef | undefined;
  let alive = true;
  // What Discard does once confirmed: close, or show what the tab opened instead.
  let discard = props.onClose;
  cleanup(() => { alive = false; });
  const palette = () => props.theme().palette;
  const size = (value: number) => space(value, props.theme().scale);
  const text = (value = 11, muted = false) => ({ fontFamily: props.theme().family, fontSize: value * props.theme().scale, color: muted ? palette().muted : palette().text });
  const dirty = () => draft() !== source;
  const bounded = () => utf8Bytes(draft()) <= MAX_SNIPPET;
  const action = () => ({ paddingHorizontal: size(10), paddingVertical: size(6), borderRadius: 5, borderWidth: 1, borderColor: palette().divider });
  function close(then: () => void = props.onClose) {
    if (busy()) return;
    if (dirty()) { discard = then; confirming.set(true); }
    else then();
  }
  props.guard?.(close);
  function keepEditing() { confirming.set(false); editor?.focus(); }
  escapeWithin(props.focus, props.active, () => { if (confirming()) keepEditing(); else close(); });
  async function save() {
    if (busy() || !bounded()) return;
    busy.set(true); message.set("");
    const snippet = { ...props.snippet, code: draft() };
    try {
      const result = await saveSnippet(snippet, props.directory);
      if (alive && result.saved) message.set(`Saved ${result.path ?? filename(snippet)}`);
    } catch (cause) { if (alive) message.set(String(cause)); }
    finally { if (alive) { busy.set(false); editor?.focus(); } }
  }
  async function copy() {
    if (busy()) return;
    try { await Clipboard.writeText(draft()); if (alive) message.set("Scratch copy copied."); }
    catch (cause) { if (alive) message.set(`Could not copy: ${String(cause)}`); }
  }
  // The label and the disabled state are read in place, so a save keeps the
  // button (and its focus); `key` reports the focus whatever the label says.
  const button = (key: string, label: () => string, onPress: () => void, disabled: () => boolean = () => false, color = palette().text, ref?: (node: YNode) => void) =>
    <View role="button" ref={(node) => ref?.(node)} focusable={!disabled()} accessibilityLabel={label()} accessibilityState={{ disabled: disabled() }}
      onPress={() => { if (!disabled()) onPress(); }} {...props.focus.track(key)} style={{ ...action(), opacity: disabled() ? 0.4 : 1 }}
      hoverStyle={{ backgroundColor: palette().panelActive }} focusStyle={{ borderColor: palette().cyan }}>
      <Text style={{ ...text(), color }}>{label()}</Text>
    </View>;
  return <View testID="chat-code-reader" style={{ flex: 1, minWidth: 0, minHeight: 0 }}>
    <SideHeader theme={props.theme} title={() => filename(props.snippet)} detail={() => dirty() ? "Edited scratch copy" : "Scratch copy"}
      focus={props.focus} onClose={() => close()} disabled={busy} />
    <View style={{ flex: 1, minHeight: 0, padding: size(12), gap: size(10) }}>
      <Text style={text(10, true)}>Edit, copy or save this snippet. The conversation stays unchanged.</Text>
      <Editor testID="chat-code-editor" ref={(value) => { editor = value; }} value={draft()} onChange={(value) => { draft.set(value); message.set(""); }}
        language={props.snippet.language} readOnly={busy() || confirming()} autoFocus={true} softWrap={true} autoClose={false} scrollBeyondLastLine={false}
        fontFamily={props.theme().monoFamily} fontSize={12 * props.theme().scale} lineHeight={18 * props.theme().scale} {...props.focus.track("editor")}
        accessibilityLabel="Snippet scratch copy" style={{ flex: 1, minHeight: 0, borderWidth: 1, borderRadius: 5, borderColor: palette().divider, backgroundColor: palette().bg }} />
      {!bounded() && <Text role="text" style={{ ...text(), color: palette().gold }}>Reader/export limit: 256 KiB. Shorten the scratch copy before saving.</Text>}
      {!!message() && <Text role="text" selectable={true} style={text()}>{message()}</Text>}
      {confirming() ? <View style={{ gap: size(8) }}>
        <Text style={{ ...text(12), fontWeight: "600" }}>Discard the edited scratch copy?</Text>
        <Text style={text(11, true)}>Copy or save the scratch copy to keep your edits. The conversation is unchanged.</Text>
        <View style={{ flexDirection: "row", flexWrap: "wrap", justifyContent: "flex-end", gap: size(8) }}>
          {button("keep", () => "Keep editing", keepEditing, undefined, undefined, (node) => node.focus())}
          {button("discard", () => "Discard and close", () => discard(), undefined, palette().gold)}
        </View>
      </View> : <View style={{ flexDirection: "row", flexWrap: "wrap", justifyContent: "flex-end", gap: size(8) }}>
        {button("copy", () => "Copy", () => void copy(), busy)}
        {button("save", () => busy() ? "Saving…" : "Save…", () => void save(), () => busy() || !bounded())}
      </View>}
    </View>
  </View>;
}
