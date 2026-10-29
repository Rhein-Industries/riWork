import { Scroll, Text, View, cleanup, signal, type YNode } from "nsh";
import { Button, Dialog, Input } from "nsh/base";
import { Accounts, Chat, Handoff as HandoffBridge, type AccountsDto } from "riwork/native";
import { space, type InterfaceTheme } from "./appearance";
import { Icon } from "./Icon";

type Provider = "codex" | "claude" | "grok" | "muse";
type Kind = "chat" | "shell";
type Mode = "default" | "supervised" | "auto_edit" | "full" | "plan";
interface HandoffSource {
  id: string; label: string; askable: boolean; kind: Kind; provider: Provider;
  projectId?: string | null; worktreeId?: string | null; cwd: string;
}
export interface HandoffResult {
  id: string; kind: Kind; projectId?: string | null; worktreeId?: string | null;
  document: string; context: "transcript" | "summary"; fallback?: string | null;
}
interface HandoffRequest {
  sourceId: string; kind: Kind; provider: Provider; model?: string | null; effort?: string | null;
  account?: string | null; mode?: Mode | null; context: "transcript" | "summary"; note?: string | null;
}
interface HandoffApi {
  source(id: string): Promise<HandoffSource>;
  run(request: HandoffRequest): Promise<HandoffResult>;
}
export interface HandoffProps {
  sourceId: string;
  theme: () => InterfaceTheme;
  onDone: (result: HandoffResult) => void;
  onCancel: () => void;
  onError: (message: string) => void;
  onNotice?: (message: string) => void;
}

const LABELS: Record<Provider, string> = { codex: "Codex", claude: "Claude", grok: "Grok", muse: "Muse" };
const MODES: { id: Mode; label: string }[] = [
  { id: "default", label: "Default" }, { id: "supervised", label: "Supervised" },
  { id: "auto_edit", label: "Auto-edit" }, { id: "full", label: "Full" }, { id: "plan", label: "Plan" },
];

/** Opening this form reads the source. Only Hand off collects context or starts an agent. */
export function Handoff(props: HandoffProps) {
  // The bridge types kind and provider as plain strings; it sends only these values.
  const api = HandoffBridge as unknown as HandoffApi;
  const sourceId = props.sourceId;
  const source = signal<HandoffSource | null>(null), accounts = signal<AccountsDto | null>(null);
  const kind = signal<Kind>("chat"), provider = signal<Provider>("codex"), context = signal<"transcript" | "summary">("transcript");
  const model = signal(""), effort = signal(""), account = signal(""), mode = signal<Mode>("default"), note = signal("");
  const models = signal<{ provider: string; model?: string }[]>([]);
  const pending = signal(false), error = signal("");
  let alive = true, submitted = false;
  let modelInput: YNode | undefined;
  cleanup(() => { alive = false; });
  const theme = props.theme;
  const size = (value: number) => space(value, theme().scale);
  const text = (value = 11, muted = false) => ({ fontFamily: theme().family, fontSize: value * theme().scale, color: muted ? theme().palette.muted : theme().palette.text });
  const field = () => ({ ...text(), minHeight: size(28), paddingHorizontal: size(8), paddingVertical: size(5), borderRadius: 6, backgroundColor: theme().palette.bg });
  const providers = (): Provider[] => kind() === "chat" ? ["codex", "claude", "muse"] : ["codex", "claude", "grok", "muse"];
  const suggestions = () => [...new Set(models().filter((chat) => chat.provider === provider() && chat.model).map((chat) => chat.model!))].slice(0, 8);

  void api.source(sourceId).then((value) => {
    if (!alive) return;
    source.set(value); kind.set(value.kind); provider.set(value.provider); modelInput?.focus();
  }).catch((reason) => { if (alive) error.set(String(reason).replace(/^Error: /, "")); });
  void Accounts.list().then((value) => { if (alive) accounts.set(value); }).catch(() => {});
  void Chat.list().then((value) => { if (alive) models.set(value); }).catch(() => {});

  function dismiss() {
    if (pending()) props.onNotice?.("Handing off in the background. The new session opens when it is ready.");
    props.onCancel();
  }
  function chooseProvider(value: Provider) { if (!pending()) { provider.set(value); model.set(""); effort.set(""); error.set(""); } }
  function chooseKind(value: Kind) {
    if (pending()) return;
    kind.set(value);
    if (!providers().includes(provider())) chooseProvider("codex");
  }
  async function start() {
    if (!alive || !source() || pending() || submitted) return;
    const modelValue = model().trim(), effortValue = effort().trim(), noteValue = note().trim();
    if ([...modelValue].length > 100 || /[\x00-\x1f\x7f-\x9f]/.test(modelValue)) { error.set("Use a model ID of at most 100 characters without control characters."); return; }
    if ([...effortValue].length > 100 || /[\x00-\x1f\x7f-\x9f]/.test(effortValue)) { error.set("Use an effort of at most 100 characters without control characters."); return; }
    if ([...noteValue].length > 2000) { error.set("A note is at most 2000 characters."); return; }
    pending.set(true); error.set("");
    const request: HandoffRequest = { sourceId: source()!.id, kind: kind(), provider: provider(), model: modelValue || null,
      effort: effortValue || null, account: provider() === "codex" ? account() || null : null,
      mode: mode() === "default" ? null : mode(), context: source()!.askable ? context() : "transcript", note: noteValue || null };
    try {
      const result = await api.run(request);
      submitted = true;
      if (result.fallback) props.onNotice?.(result.fallback);
      // Closing the dialog while busy hides it; completion must still open the new tab.
      props.onDone(result);
    } catch (reason) {
      const message = String(reason).replace(/^Error: /, "");
      if (alive) error.set(message);
      props.onError(message);
    } finally { if (alive) pending.set(false); }
  }
  function choice(id: string, label: string, selected: boolean, onPress: () => void, disabled = false) {
    return <View testID={id} role="radio" accessibilityLabel={label} accessibilityState={{ checked: selected }} disabled={pending() || disabled} onPress={onPress}
      style={{ paddingHorizontal: size(9), paddingVertical: size(5), borderRadius: 6, opacity: disabled ? 0.45 : 1, backgroundColor: selected ? theme().palette.bg : "transparent" }} hoverStyle={{ backgroundColor: theme().palette.bg }}>
      <Text style={text(11, !selected)}>{label}</Text>
    </View>;
  }
  const group = () => ({ flexDirection: "row" as const, flexWrap: "wrap" as const, alignSelf: "flex-start" as const, gap: 2, padding: 2, borderRadius: 8, backgroundColor: theme().palette.panelActive });
  const heading = (label: string) => <Text style={{ ...text(10, true), fontWeight: "600" }}>{label}</Text>;
  return <Dialog testID="handoff-dialog" animation="none" closeOnBackdropPress={false} closeOnEscape={true}
    onOpen={() => modelInput?.focus()} onOpenChange={(open) => { if (!open) dismiss(); }} backdropStyle={{ backgroundColor: "#00000066" }}
    style={{ width: size(560), maxWidth: "100%", maxHeight: "90%", padding: size(20), gap: size(12), backgroundColor: theme().palette.panel, borderRadius: 10 }}>
    <View style={{ flexDirection: "row", alignItems: "center", gap: size(8) }}><Icon name="chat" size={17 * theme().scale} color={theme().palette.muted} /><Text role="header" style={{ ...text(17), fontWeight: "600" }}>Hand off conversation</Text></View>
    <Text numberOfLines={2} style={text(11, true)}>{source()?.label ?? (error() ? "Source unavailable" : "Reading the conversation's source…")}</Text>
    <Scroll style={{ flexShrink: 1, minHeight: 0 }} contentStyle={{ gap: size(10) }}>
      {source() && <>
        {heading("Destination")}
        <View role="radiogroup" accessibilityLabel="Destination" style={group()}>
          {choice("handoff-chat", "Chat", kind() === "chat", () => chooseKind("chat"))}
          {choice("handoff-shell", "Shell", kind() === "shell", () => chooseKind("shell"), !source()!.projectId)}
        </View>
        {heading("Agent")}
        <View role="radiogroup" accessibilityLabel="Agent" style={group()}>{providers().map((value) => choice(`handoff-provider-${value}`, LABELS[value], provider() === value, () => chooseProvider(value)))}</View>
        {heading("Model")}
        <Input testID="handoff-model" ref={(node) => { modelInput = node; }} accessibilityLabel="Model" placeholder="The agent's own default" value={model()} onChange={(value) => model.set(value)} disabled={pending()} style={field()} />
        {!!suggestions().length && <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 4 }}>{suggestions().map((value) => choice(`handoff-model-${value}`, value, model().trim() === value, () => model.set(value)))}</View>}
        <Input testID="handoff-effort" accessibilityLabel="Reasoning effort" placeholder="Reasoning effort (optional)" value={effort()} onChange={(value) => effort.set(value)} disabled={pending()} style={field()} />
        {provider() === "codex" && <>
          {heading("Codex account")}
          <View role="radiogroup" accessibilityLabel="Codex account" style={group()}>
            {choice("handoff-account-project", "Project default", account() === "", () => account.set(""))}
            {choice("handoff-account-system", "System default", account() === "system-default", () => account.set("system-default"))}
            {(accounts()?.accounts ?? []).filter((item) => !item.system && item.available).map((item) => choice(`handoff-account-${item.id}`, item.label, account() === item.id, () => account.set(item.id)))}
          </View>
        </>}
        {heading("Permissions")}
        <View role="radiogroup" accessibilityLabel="Permissions" style={group()}>{MODES.map((item) => choice(`handoff-mode-${item.id}`, item.label, mode() === item.id, () => mode.set(item.id)))}</View>
        {source()!.askable && <>
          {heading("Pass the conversation as")}
          <View role="radiogroup" accessibilityLabel="Conversation context" style={group()}>
            {choice("handoff-transcript", "Transcript", context() === "transcript", () => context.set("transcript"))}
            {choice("handoff-summary", "Summary", context() === "summary", () => context.set("summary"))}
          </View>
          <Text style={text(10, true)}>{context() === "summary" ? "Asks the source agent to write a summary. You can hide this dialog if it needs your input; the transcript is used if a summary is unavailable." : "Passes the recorded conversation. The source session stays as it is."}</Text>
        </>}
        {heading("Note")}
        <Input testID="handoff-note" accessibilityLabel="Note for the new agent" placeholder="Anything the new agent should know" multiline={true} value={note()} onChange={(value) => note.set(value)} disabled={pending()} style={{ ...field(), minHeight: size(56), maxHeight: size(120) }} />
        <Text selectable={true} numberOfLines={2} style={{ ...text(10, true), fontFamily: theme().monoFamily }}>{`Same project, worktree and directory · ${source()!.cwd}`}</Text>
      </>}
      {pending() && <Text role="text" style={{ ...text(), color: theme().palette.gold }}>{context() === "summary" ? "Asking for a summary and preparing the new session…" : "Reading the conversation and preparing the new session…"}</Text>}
      {error() && <Text role="alert" style={{ ...text(), color: theme().palette.gold }}>{error()}</Text>}
    </Scroll>
    <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: size(8) }}>
      <Button testID="handoff-cancel" onPress={dismiss} style={{ paddingHorizontal: size(12), paddingVertical: size(7), borderRadius: 6 }}><Text style={text()}>{pending() ? "Continue in background" : "Cancel"}</Text></Button>
      <Button testID="handoff-start" disabled={!source() || pending()} onPress={() => void start()} style={{ paddingHorizontal: size(12), paddingVertical: size(7), borderRadius: 6, backgroundColor: theme().palette.panelActive }}><Text style={{ ...text(), fontWeight: "600" }}>{pending() ? "Handing off…" : "Hand off"}</Text></Button>
    </View>
  </Dialog>;
}
