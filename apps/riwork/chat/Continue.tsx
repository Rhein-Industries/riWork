import { Text, View, cleanup, computed, signal, type YNode } from "nsh";
import { Button, Input } from "nsh/base";
import type { MenuItem } from "nsh/desktop";
import { Chat, Handoff, type ChatPanel } from "riwork/native";
import { space, type InterfaceTheme } from "../appearance";
import { Icon } from "../Icon";

/** The agents a chat can go on with. */
export type Agent = "codex" | "claude" | "muse";
export const AGENTS: { id: Agent; label: string }[] = [
  { id: "codex", label: "Codex" }, { id: "claude", label: "Claude" }, { id: "muse", label: "Muse" },
];
export const agentLabel = (agent: string) => AGENTS.find((item) => item.id === agent)?.label ?? agent;

export interface AgentModel { id: string; label: string; detail?: string }
/** Another agent (and model) to go on with; no model is the agent's own default. */
export interface AgentChoice { provider: Agent; model?: string; label: string }

/** At most this many models of an agent are offered in a menu or the picker. */
const MODELS_SHOWN = 8;

/** Errors from the bridge read as sentences. */
const sentence = (error: unknown) => String(error).replace(/^(?:Error: )?(?:refused|broken|unavailable|invalid|io|not_found|conflict): /u, "");

/** Going on with another agent in place (the model picker's "Continue with another agent"), and
 * forking (the ⋯ menu, a message's actions). Both are the engine's handoff: the new chat says
 * where it came from, and this chat gets a marker naming it. */
export function createContinue(options: {
  chatId: string;
  panel: () => ChatPanel;
  notice: (message: string) => void;
  /** The handoff made `id` this tab's chat. */
  onReplace?: (id: string) => void;
  /** Open another chat (a fork) beside this one. */
  onOpen?: (id: string) => void;
}) {
  const choice = signal<AgentChoice | null>(null);
  const context = signal<"transcript" | "summary">("transcript");
  const note = signal("");
  const pending = signal(false), error = signal("");
  const forking = signal(false);
  const catalogs = signal<Partial<Record<Agent, AgentModel[]>>>({});
  let alive = true, asked = 0;
  cleanup(() => { alive = false; asked++; });

  /** The saved model lists of every agent (`riwork chat models`): read from saved chats, never
   * by starting one. */
  function loadCatalogs() {
    const token = ++asked;
    const projectId = options.panel().projectId ?? null;
    for (const agent of AGENTS) {
      void Chat.models(agent.id, projectId).then((catalog) => {
        if (!alive || token !== asked) return;
        const seen = new Set<string>();
        const models: AgentModel[] = [];
        for (const model of catalog.supported) {
          if (seen.has(model.id)) continue;
          seen.add(model.id);
          models.push({ id: model.id, label: model.name || model.id, detail: model.description || undefined });
        }
        for (const id of catalog.configured) if (!seen.has(id)) { seen.add(id); models.push({ id, label: id }); }
        catalogs.set({ ...catalogs(), [agent.id]: models });
      }).catch(() => {});
    }
  }

  /** An agent's models: this chat's own list for its agent, the saved catalog for another. */
  function models(agent: Agent): AgentModel[] {
    const bar = options.panel().toolbar;
    if (bar && bar.provider === agent && bar.models.length) return bar.models.map((model) => ({ id: model.id, label: model.label, detail: model.detail }));
    return catalogs()[agent] ?? [];
  }

  /** The other agents, each with its default and its known models. */
  function others(): { agent: Agent; label: string; choices: AgentChoice[] }[] {
    const current = options.panel().provider;
    return AGENTS.filter((agent) => agent.id !== current).map((agent) => ({
      agent: agent.id, label: agent.label,
      choices: [{ provider: agent.id, label: `${agent.label} · default model` },
        ...models(agent.id).slice(0, MODELS_SHOWN).map((model) => ({ provider: agent.id, model: model.id, label: `${agent.label} · ${model.label}` }))],
    }));
  }

  function open(next: AgentChoice) {
    if (pending()) return;
    choice.set(next); error.set(""); note.set("");
    // A summary is asked of an agent that is idle; one at work passes what it wrote.
    context.set("transcript");
  }
  function cancel() { if (!pending()) { choice.set(null); error.set(""); } }
  /** A summary is written by the agent when asked, which it can be only while idle. */
  const askable = () => options.panel().state === "idle";

  async function handOff() {
    const chosen = choice();
    if (!chosen || pending()) return;
    const text = note().trim();
    if ([...text].length > 2000) { error.set("A note is at most 2000 characters."); return; }
    pending.set(true); error.set("");
    try {
      const result = await Handoff.run({
        sourceId: options.chatId, kind: "chat", provider: chosen.provider, model: chosen.model ?? null,
        context: askable() ? context() : "transcript", note: text || null, inPlace: true,
      });
      if (!alive) return;
      choice.set(null);
      if (result.fallback) options.notice(`No summary (${result.fallback}); the new chat got the transcript.`);
      if (options.onReplace) options.onReplace(result.id);
      else if (options.onOpen) options.onOpen(result.id);
    } catch (reason) {
      if (alive) error.set(sentence(reason));
    } finally { if (alive) pending.set(false); }
  }

  /** Forks the chat at `at` (that item included), or whole; with another agent when chosen. */
  async function fork(at?: string, agent?: AgentChoice) {
    if (forking()) return;
    forking.set(true);
    try {
      const result = await Chat.fork({ chatId: options.chatId, at: at ?? null, provider: agent?.provider ?? null, model: agent?.model ?? null });
      if (!alive) return;
      if (result.fallback) options.notice(result.fallback);
      if (options.onOpen) options.onOpen(result.id);
      else options.notice(`Forked into “${result.title}”.`);
    } catch (reason) {
      if (alive) options.notice(`Could not fork: ${sentence(reason)}`);
    } finally { if (alive) forking.set(false); }
  }

  /** "Fork from here" (or "Fork chat" for the whole), and "Fork with…" another agent or model. */
  function forkItems(at?: string): MenuItem[] {
    const current = options.panel().provider;
    const agents: MenuItem[] = AGENTS.map((agent) => ({
      label: agent.id === current ? `${agent.label} (this agent)` : agent.label,
      submenu: [
        { label: "Default model", onSelect: () => void fork(at, { provider: agent.id, label: agent.label }) },
        ...models(agent.id).slice(0, MODELS_SHOWN).map((model) => ({
          label: model.label, onSelect: () => void fork(at, { provider: agent.id, model: model.id, label: `${agent.label} · ${model.label}` }),
        })),
      ],
    }));
    return [
      { label: at ? "Fork from here" : "Fork chat", disabled: forking(), onSelect: () => void fork(at) },
      { label: "Fork with…", disabled: forking(), submenu: agents },
    ];
  }

  return { choice, context, note, pending, error, forking, catalogs, loadCatalogs, models, others, open, cancel, askable, handOff, fork, forkItems };
}

export type ContinueControl = ReturnType<typeof createContinue>;

/** The inline confirmation by the composer: which agent, what it reads, a note, Hand off. */
export function ContinueConfirm(props: { control: ContinueControl; theme: () => InterfaceTheme; panel: () => ChatPanel; onFocus?: (node: YNode) => void }) {
  const control = props.control;
  const palette = () => props.theme().palette;
  const size = (value: number) => space(value, props.theme().scale);
  const text = (muted = false, base = 11) => ({ fontFamily: props.theme().family, fontSize: base * props.theme().scale, color: muted ? palette().muted : palette().text });
  const button = () => ({ minHeight: size(28), paddingHorizontal: size(10), justifyContent: "center" as const, borderRadius: 5 });
  const busy = computed(() => props.panel().running);
  const pill = (id: string, label: string, selected: boolean, onPress: () => void) =>
    <View testID={id} role="radio" accessibilityLabel={label} accessibilityState={{ checked: selected, disabled: control.pending() }}
      onPress={() => { if (!control.pending()) onPress(); }}
      style={{ paddingHorizontal: size(9), paddingVertical: size(4), borderRadius: 6, backgroundColor: selected ? palette().bg : "transparent" }}
      hoverStyle={{ backgroundColor: palette().bg }}>
      <Text style={text(!selected)}>{label}</Text>
    </View>;
  return <View testID="chat-continue" accessibilityLabel="Continue with another agent"
    style={{ borderWidth: 1, borderColor: palette().divider, borderRadius: 8, backgroundColor: palette().panelActive, gap: size(8), padding: size(10), marginBottom: size(6) }}>
    <View style={{ flexDirection: "row", alignItems: "center", gap: size(6) }}>
      <Icon name="worktrees" size={13 * props.theme().scale} color={palette().muted} />
      <Text role="header" numberOfLines={1} style={{ ...text(false, 12), fontWeight: "600", flexShrink: 1 }}>{`Continue with ${control.choice()?.label ?? ""}`}</Text>
    </View>
    <Text style={text(true)}>{busy()
      ? "A new chat takes this tab with the conversation so far; the turn still running stays here. This chat stays reachable from the new one."
      : "A new chat takes this tab with the conversation so far. This chat stays as it is, reachable from the new one."}</Text>
    <View style={{ flexDirection: "row", alignItems: "center", flexWrap: "wrap", gap: size(6) }}>
      <Text style={text(true)}>It reads</Text>
      <View role="radiogroup" accessibilityLabel="What the new agent reads" style={{ flexDirection: "row", gap: 2, padding: 2, borderRadius: 8, backgroundColor: palette().panel }}>
        {pill("chat-continue-transcript", "Transcript", control.context() === "transcript" || !control.askable(), () => control.context.set("transcript"))}
        {control.askable() && pill("chat-continue-summary", "Summary", control.context() === "summary", () => control.context.set("summary"))}
      </View>
      {control.context() === "summary" && control.askable() && <Text style={text(true)}>This agent writes it first.</Text>}
    </View>
    <Input testID="chat-continue-note" ref={(node) => { node.focus(); props.onFocus?.(node); }} accessibilityLabel="Note for the new agent" placeholder="Note for the new agent (optional)"
      value={control.note()} onChange={(value) => control.note.set(value)} onSubmit={() => void control.handOff()} disabled={control.pending()}
      style={{ ...text(), minHeight: size(28), paddingHorizontal: size(8), paddingVertical: size(5), borderRadius: 6, borderWidth: 1, borderColor: palette().divider, backgroundColor: palette().bg }} />
    {control.pending() && <Text role="text" style={{ ...text(), color: palette().gold }}>{control.context() === "summary" && control.askable() ? "Asking for a summary and starting the new chat…" : "Reading the conversation and starting the new chat…"}</Text>}
    {!!control.error() && <Text testID="chat-continue-error" role="alert" selectable={true} style={{ ...text(), color: palette().danger }}>{control.error()}</Text>}
    <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: size(6) }}>
      <Button testID="chat-continue-cancel" disabled={control.pending()} onPress={control.cancel} style={button()} focusStyle={{ backgroundColor: palette().panel }}><Text style={text()}>Cancel</Text></Button>
      <Button testID="chat-continue-start" disabled={control.pending()} onPress={() => void control.handOff()} style={{ ...button(), backgroundColor: palette().panel }} focusStyle={{ backgroundColor: palette().bg }}>
        <Text style={{ ...text(), fontWeight: "600", color: palette().cyan }}>{control.pending() ? "Handing off…" : "Hand off"}</Text>
      </Button>
    </View>
  </View>;
}
