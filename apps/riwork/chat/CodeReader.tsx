import { Clipboard, Text, View, cleanup, host, signal } from "nsh";
import { Button, Dialog, Editor, type EditorRef } from "nsh/base";
import { space, type InterfaceTheme } from "../appearance";
import { utf8Bytes } from "../files_model";

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

/** A captured scratch editor. Its text is never written into the transcript,
 * composer, or a project file without a separate explicit Copy or Save. */
export function CodeReader(props: {
  snippet: CodeSnippet;
  directory?: string;
  theme: () => InterfaceTheme;
  onClose: () => void;
}) {
  const source = props.snippet.code;
  const draft = signal(source), busy = signal(false), confirming = signal(false), message = signal("");
  let editor: EditorRef | undefined;
  let alive = true;
  cleanup(() => { alive = false; });
  const palette = () => props.theme().palette;
  const size = (value: number) => space(value, props.theme().scale);
  const text = (value = 11, muted = false) => ({ fontFamily: props.theme().family, fontSize: value * props.theme().scale, color: muted ? palette().muted : palette().text });
  const dirty = () => draft() !== source;
  const bounded = () => utf8Bytes(draft()) <= MAX_SNIPPET;
  const action = () => ({ paddingHorizontal: size(10), paddingVertical: size(6), borderRadius: 5, borderWidth: 1, borderColor: palette().divider });
  function close() {
    if (busy()) return;
    if (dirty()) confirming.set(true);
    else props.onClose();
  }
  function keepEditing() { confirming.set(false); editor?.focus(); }
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
  return <Dialog testID="chat-code-reader" animation="none" closeOnBackdropPress={false} closeOnEscape={false}
    onOpen={() => editor?.focus()} onCancel={() => { if (confirming()) keepEditing(); else close(); }}
    backdropStyle={{ backgroundColor: "#00000066" }} style={{ width: size(860), maxWidth: "96%", height: "85%", maxHeight: "90%", padding: size(14), gap: size(10), borderRadius: 10, borderWidth: 1, borderColor: palette().divider, backgroundColor: palette().panel }}>
    <View style={{ flexDirection: "row", alignItems: "center", gap: size(10) }}>
      <Text role="header" numberOfLines={1} style={{ ...text(14), flex: 1, fontWeight: "600" }}>{filename(props.snippet)}</Text>
      <Text style={text(10, true)}>{dirty() ? "Edited scratch copy" : "Scratch copy"}</Text>
    </View>
    <Text style={text(10, true)}>Edit, copy or save this snippet. The conversation stays unchanged.</Text>
    <Editor testID="chat-code-editor" ref={(value) => { editor = value; }} value={draft()} onChange={(value) => { draft.set(value); message.set(""); }}
      language={props.snippet.language} readOnly={busy() || confirming()} autoFocus={true} softWrap={true} autoClose={false} scrollBeyondLastLine={false}
      fontFamily={props.theme().monoFamily} fontSize={12 * props.theme().scale} lineHeight={18 * props.theme().scale}
      accessibilityLabel="Snippet scratch copy" style={{ flex: 1, minHeight: 0, borderWidth: 1, borderRadius: 5, borderColor: palette().divider, backgroundColor: palette().bg }} />
    {!bounded() && <Text role="text" style={{ ...text(), color: palette().gold }}>Reader/export limit: 256 KiB. Shorten the scratch copy before saving.</Text>}
    {!!message() && <Text role="text" selectable={true} style={text()}>{message()}</Text>}
    {confirming() ? <View style={{ gap: size(8) }}>
      <Text style={{ ...text(12), fontWeight: "600" }}>Discard the edited scratch copy?</Text>
      <Text style={text(11, true)}>Copy or save the scratch copy to keep your edits. The conversation is unchanged.</Text>
      <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: size(8) }}>
        <Button ref={(node) => node.focus()} onPress={keepEditing} style={action()} focusStyle={{ borderColor: palette().cyan }}><Text style={text()}>Keep editing</Text></Button>
        <Button onPress={props.onClose} style={action()} focusStyle={{ borderColor: palette().cyan }}><Text style={{ ...text(), color: palette().gold }}>Discard and close</Text></Button>
      </View>
    </View> : <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: size(8) }}>
      <Button disabled={busy()} onPress={() => void copy()} style={action()} focusStyle={{ borderColor: palette().cyan }}><Text style={text()}>Copy</Text></Button>
      <Button disabled={busy() || !bounded()} onPress={() => void save()} style={action()} focusStyle={{ borderColor: palette().cyan }}><Text style={text()}>{busy() ? "Saving…" : "Save…"}</Text></Button>
      <Button disabled={busy()} onPress={close} style={action()} focusStyle={{ borderColor: palette().cyan }}><Text style={text()}>Close</Text></Button>
    </View>}
  </Dialog>;
}
