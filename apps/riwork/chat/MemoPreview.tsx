import { Scroll, Text, View, cleanup, signal } from "nsh";
import { Button } from "nsh/base";
import { Store } from "riwork/native";
import { space, type InterfaceTheme } from "../appearance";

type Preview =
  | { kind: "noProject" }
  | { kind: "loading"; projectId: string }
  | { kind: "memo"; projectId: string; text: string }
  | { kind: "error"; projectId: string; error: string };

/** Same last-word command and blank-line insertion as project_memo.rs. */
export function memoCommandRest(draft: string): string | null {
  const trimmed = draft.trimEnd();
  if (!trimmed.endsWith("/memo")) return null;
  const rest = trimmed.slice(0, -5);
  return !rest || /\s$/.test(rest) ? rest.trimEnd() : null;
}

export function appendMemo(draft: string, memo: string): string {
  const head = draft.trimEnd(), body = memo.replace(/[\r\n]+$/, "");
  return head ? `${head}\n\n${body}` : body;
}

/** Read on the explicit command only. The displayed snapshot is exactly the
 * text Insert uses, even if the saved memo changes while the preview is open. */
export function createMemoPreview() {
  const preview = signal<Preview | null>(null);
  let generation = 0, alive = true;
  cleanup(() => { alive = false; generation++; });
  function close() { generation++; preview.set(null); }
  function open(projectId?: string) {
    const request = ++generation;
    if (!projectId) { preview.set({ kind: "noProject" }); return; }
    preview.set({ kind: "loading", projectId });
    void Store.snapshot().then((state) => {
      if (alive && request === generation) preview.set({ kind: "memo", projectId, text: state.projectMemos[projectId]?.text ?? "" });
    }).catch((error) => {
      if (alive && request === generation) preview.set({ kind: "error", projectId, error: String(error) });
    });
  }
  return { preview, open, close };
}

export function MemoPreview(props: {
  preview: () => Preview | null;
  theme: () => InterfaceTheme;
  active: () => boolean;
  onInsert: () => void;
  onCancel: () => void;
  onEdit?: (projectId: string) => void;
}) {
  const palette = () => props.theme().palette;
  const size = (value: number) => space(value, props.theme().scale);
  const text = (muted = false) => ({ fontFamily: props.theme().family, fontSize: 11 * props.theme().scale, color: muted ? palette().muted : palette().text });
  const project = () => { const value = props.preview(); return value && value.kind !== "noProject" ? value.projectId : undefined; };
  const contents = () => { const value = props.preview(); return value?.kind === "memo" ? value.text : ""; };
  const note = () => {
    const value = props.preview();
    if (value?.kind === "noProject") return "This chat is not part of a project, so it has no project memo. Use /memo in a project's chat to insert its memo.";
    if (value?.kind === "loading") return "Reading the saved project memo…";
    if (value?.kind === "error") return `The memo could not be read: ${value.error}`;
    if (value?.kind === "memo" && !value.text.trim()) return "This project's memo is empty. Edit it to write notes you can insert into any of its chats.";
    return "";
  };
  const button = () => ({ minHeight: size(28), paddingHorizontal: size(10), justifyContent: "center" as const, borderRadius: 5 });
  return <View testID="chat-memo-preview" accessibilityLabel="Project memo preview"
    style={{ borderWidth: 1, borderColor: palette().divider, borderRadius: 8, backgroundColor: palette().panelActive, gap: size(6), marginBottom: size(6) }}>
    <Text role="header" style={{ ...text(), fontWeight: "600", paddingHorizontal: size(10), paddingTop: size(8) }}>{props.preview()?.kind === "noProject" ? "No project memo here" : "Project memo"}</Text>
    {!!note() && <Text role="text" selectable={true} style={{ ...text(true), paddingHorizontal: size(10), paddingVertical: size(4) }}>{note()}</Text>}
    {!!contents().trim() && <Scroll style={{ maxHeight: size(220) }} contentStyle={{ paddingHorizontal: size(10), paddingVertical: size(4) }}>
      <Text testID="chat-memo-preview-text" selectable={true} style={{ ...text(), fontFamily: props.theme().monoFamily }}>{contents()}</Text>
    </Scroll>}
    <View style={{ flexDirection: "row", flexWrap: "wrap", justifyContent: "flex-end", gap: size(6), padding: size(6), borderTopWidth: 1, borderColor: palette().divider }}>
      {!!project() && !!props.onEdit && <Button testID="chat-memo-edit" disabled={!props.active()} onPress={() => { const id = project(); if (id && props.active()) props.onEdit?.(id); }} style={button()} focusStyle={{ backgroundColor: palette().panel }}><Text style={text()}>Edit</Text></Button>}
      {!!contents().trim() && <Button testID="chat-memo-insert" disabled={!props.active()} onPress={() => { if (props.active()) props.onInsert(); }} style={button()} focusStyle={{ backgroundColor: palette().panel }}><Text style={{ ...text(), color: palette().cyan }}>Insert into composer</Text></Button>}
      <Button testID="chat-memo-cancel" onPress={props.onCancel} style={button()} focusStyle={{ backgroundColor: palette().panel }}><Text style={text()}>Cancel</Text></Button>
    </View>
  </View>;
}
