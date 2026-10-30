import { Image, Scroll, Text, View, cleanup, colorScheme, computed, effect, signal } from "nsh";
import { Button, Input } from "nsh/base";
import { Flows, type FlowDiagramDto, type FlowDto, type FlowRunDto, type FlowsSnapshot } from "riwork/native";
import { DEFAULT_THEME, space, type InterfaceTheme } from "../appearance";
import { Icon } from "../Icon";
import { RunCard, flowText } from "./RunCard";

export interface FlowChat { id: string; title: string }
export type FlowsApi = Pick<typeof Flows, "subscribe" | "diagram" | "create" | "start" | "approve" | "deny" | "stop">;

/** A project's flows: statecharts in `.ontack/flows` that lead its chats step by step. The
 * native side reads them again every second while the panel is visible. */
export function FlowsPanel(props: {
  projectId: () => string;
  visible?: () => boolean;
  theme?: () => InterfaceTheme;
  /** The project's chats a flow can lead. */
  chats: () => FlowChat[];
  openChat: (id: string) => void;
  editFile: (path: string) => void;
  /** Fixtures supply an in-memory API; the app uses the native module. */
  api?: FlowsApi;
}) {
  const api: FlowsApi = props.api ?? Flows;
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const palette = () => theme().palette;
  const text = flowText(theme);
  const visible = () => props.visible?.() ?? true;
  const remote = () => props.projectId().startsWith("remote:");
  const data = signal<FlowsSnapshot | null>(null);
  const selected = signal<string | null>(null);
  const shownRun = signal<string | null>(null);
  const picking = signal(false);
  const creating = signal(false);
  const name = signal("");
  const busy = signal(false);
  const error = signal("");
  const width = signal(480);
  let alive = true;
  cleanup(() => { alive = false; });

  effect(() => {
    if (!visible() || remote()) return;
    const project = props.projectId();
    data.set(null); error.set("");
    const subscription = api.subscribe(project, (next) => {
      if (!alive || project !== props.projectId()) return;
      data.set(next);
      if (!next.flows.some((flow) => flow.id === selected())) selected.set(next.flows[0]?.id ?? null);
    });
    return () => subscription.close();
  });

  const flow = computed<FlowDto | null>(() => data()?.flows.find((flow) => flow.id === selected()) ?? null);
  const runs = computed<FlowRunDto[]>(() => (data()?.runs ?? []).filter((run) => run.flowId === selected()));
  // The diagram follows the run chosen, else this flow's first live run, else the flow alone.
  const run = computed<FlowRunDto | null>(() => runs().find((run) => run.id === shownRun())
    ?? runs().find((run) => run.status === "active" || run.status === "waiting") ?? null);
  const source = computed(() => (run()?.mermaid || flow()?.mermaid) ?? "");

  const diagram = signal<FlowDiagramDto | null>(null);
  const diagramError = signal("");
  let drawing = 0;
  effect(() => {
    const text = source(), dark = colorScheme() === "dark";
    const token = ++drawing;
    diagramError.set("");
    if (!text || !visible()) { diagram.set(null); return; }
    api.diagram(text, dark).then((png) => { if (alive && token === drawing) diagram.set(png); })
      .catch((reason) => { if (alive && token === drawing) { diagram.set(null); diagramError.set(String(reason)); } });
  });

  async function act(work: () => Promise<unknown>) {
    if (busy()) return;
    busy.set(true); error.set("");
    try { await work(); } catch (reason) { if (alive) error.set(String(reason)); } finally { if (alive) busy.set(false); }
  }
  function create() {
    const value = name().trim();
    if (!value) return;
    void act(async () => {
      const made = await api.create(props.projectId(), value);
      creating.set(false); name.set(""); selected.set(made.id);
      props.editFile(made.path);
    });
  }
  function start(chat: FlowChat) {
    const id = selected();
    if (!id) return;
    void act(async () => {
      const started = await api.start(props.projectId(), id, chat.id);
      picking.set(false); shownRun.set(started.id);
      props.openChat(chat.id);
    });
  }

  function button(id: string, label: string, onPress: () => void, primary = false, disabled = busy()) {
    return <Button testID={id} accessibilityLabel={label} disabled={disabled} onPress={onPress}
      style={{ paddingHorizontal: space(12, theme().scale), paddingVertical: space(4, theme().scale), borderRadius: space(14, theme().scale), backgroundColor: primary && !disabled ? palette().cyan : palette().panelActive }}
      hoverStyle={{ backgroundColor: primary ? palette().magenta : palette().divider }} focusStyle={{ backgroundColor: palette().divider }}>
      <Text style={text(11, disabled ? palette().muted : primary ? palette().bg : palette().text)}>{label}</Text>
    </Button>;
  }
  const chats = () => props.chats();
  const live = (run: FlowRunDto) => run.status === "active" || run.status === "waiting";

  return <View testID="flows-panel" onLayout={(bounds) => width.set(bounds.size.width)} style={{ flex: 1, minHeight: 0, backgroundColor: palette().panel }}>
    {remote() ? <Text style={{ ...text(11), padding: 16 }}>Flows work with this Mac's own projects. Select one of them to use it.</Text> : <>
      <View style={{ height: space(46, theme().scale), flexDirection: "row", alignItems: "center", paddingLeft: space(12, theme().scale), paddingRight: space(8, theme().scale), gap: 8 }}>
        <View style={{ flex: 1, minWidth: 0, gap: 1 }}>
          <Text style={{ ...text(13), fontWeight: "600" }}>Flows</Text>
          <Text testID="flows-count" style={text(10, palette().muted)}>{!data() ? "Reading flows…" : data()!.flows.length === 0 ? "No flows" : data()!.flows.length === 1 ? "1 flow" : `${data()!.flows.length} flows`}</Text>
        </View>
        <Button testID="flow-new" accessibilityLabel="New flow" disabled={busy()} onPress={() => { creating.set(!creating()); picking.set(false); }}
          tooltip={<Text style={{ ...text(11), padding: 8, backgroundColor: palette().panelActive }}>New flow</Text>}
          style={{ width: space(26, theme().scale), height: space(26, theme().scale), borderRadius: 13, alignItems: "center", justifyContent: "center" }} hoverStyle={{ backgroundColor: palette().panelActive }} focusStyle={{ backgroundColor: palette().divider }}>
          <Icon name="add" size={14 * theme().scale} color={palette().muted} />
        </Button>
      </View>
      <Scroll testID="flows-scroll" style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(10, theme().scale), paddingBottom: space(12, theme().scale) }}>
        <View style={{ width: "100%", maxWidth: space(820, theme().scale), gap: space(12, theme().scale) }}>
          {!!error() && <Text testID="flows-error" role="alert" style={text(11, palette().gold)}>{error()}</Text>}
          {creating() && <View testID="flow-create" style={{ gap: 8, padding: space(12, theme().scale), borderRadius: space(12, theme().scale), backgroundColor: palette().bg }}>
            <Text style={{ ...text(12), fontWeight: "600" }}>New flow</Text>
            <Input testID="flow-name" accessibilityLabel="Flow name" placeholder="Name, such as Code review" value={name()} onChange={(value) => name.set(value)}
              onSubmit={() => create()} ref={(node) => node.focus()}
              style={{ ...text(11), minHeight: space(28, theme().scale), backgroundColor: palette().panel, borderRadius: space(6, theme().scale), paddingHorizontal: space(8, theme().scale), paddingVertical: space(5, theme().scale) }} />
            <Text style={text(10, palette().muted)}>Starts from a plan → build → review flow in .ontack/flows, opened for editing. You can also ask an agent to write one.</Text>
            <View style={{ flexDirection: "row", gap: 8 }}>
              {button("flow-create-confirm", "Create", create, true, busy() || !name().trim())}
              {button("flow-create-cancel", "Cancel", () => { creating.set(false); name.set(""); })}
            </View>
          </View>}
          {data() && data()!.flows.length === 0 && !creating() && <View style={{ minHeight: space(140, theme().scale), padding: 24, gap: 8, alignItems: "center", justifyContent: "center" }}>
            <Icon name="flows" size={22 * theme().scale} color={palette().muted} />
            <Text testID="flows-empty" style={{ ...text(11, palette().muted), textAlign: "center", maxWidth: space(320, theme().scale) }}>
              {"A flow leads a chat through steps: each step tells the agent what to do, and it moves on only when a check passes or you approve."}
            </Text>
            {button("flow-empty-new", "New flow", () => creating.set(true), true)}
          </View>}
          {(data()?.flows.length ?? 0) > 0 && <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
            {data()!.flows.map((item) => <Button testID={`flow:${item.id}`} accessibilityLabel={item.name} selected={item.id === selected()}
              onPress={() => { selected.set(item.id); shownRun.set(null); picking.set(false); }}
              style={{ paddingHorizontal: space(10, theme().scale), paddingVertical: space(4, theme().scale), borderRadius: space(8, theme().scale), backgroundColor: item.id === selected() ? palette().panelActive : "transparent", borderWidth: 1, borderColor: palette().divider }}
              hoverStyle={{ backgroundColor: palette().panelActive }}>
              <Text style={text(11, item.error ? palette().gold : palette().text)}>{`${item.name}${(data()!.runs.some((run) => run.flowId === item.id && live(run))) ? " ●" : ""}`}</Text>
            </Button>)}
          </View>}
          {flow() && <View testID="flow-detail" style={{ gap: 8, padding: space(12, theme().scale), borderRadius: space(12, theme().scale), backgroundColor: palette().bg }}>
            <View style={{ flexDirection: "row", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
              <Text style={{ ...text(13), fontWeight: "600", flex: 1, minWidth: 120 }}>{flow()!.name}</Text>
              {button("flow-edit", "Edit file", () => props.editFile(flow()!.path))}
              {button("flow-run", "Run in a chat…", () => { picking.set(!picking()); creating.set(false); }, true, busy() || !!flow()!.error)}
            </View>
            {!!flow()!.description && <Text style={text(11, palette().muted)}>{flow()!.description}</Text>}
            {!!flow()!.error && <Text testID="flow-invalid" selectable={true} style={text(11, palette().gold)}>{`This flow cannot run: ${flow()!.error}`}</Text>}
            {picking() && <View testID="flow-chats" style={{ gap: 6 }}>
              <Text style={text(10, palette().muted)}>Which chat should follow it? Its first step goes to the chat as a message.</Text>
              {chats().length === 0 && <Text style={text(11, palette().gold)}>This project has no chat yet. Start one, then come back.</Text>}
              <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
                {chats().map((chat) => button(`flow-chat:${chat.id}`, chat.title || chat.id.slice(0, 8), () => start(chat),
                  false, busy() || (data()?.runs ?? []).some((run) => run.chatId === chat.id && live(run))))}
              </View>
            </View>}
            {run() && <Text style={text(10, palette().muted)}>{`Showing ${run()!.chatTitle || "the chat"}'s run: ${run()!.location}`}</Text>}
            {diagram() && <Image testID="flow-diagram" src={diagram()!.dataUrl} resizeMode="contain" accessibilityLabel={`Diagram of ${flow()!.name}`}
              style={{ width: Math.max(1, Math.min(width() - space(44, theme().scale), diagram()!.width / 2)), height: Math.max(1, Math.min(width() - space(44, theme().scale), diagram()!.width / 2)) * diagram()!.height / diagram()!.width, alignSelf: "center" }} />}
            {!diagram() && !diagramError() && !!source() && <Text style={text(10, palette().muted)}>Drawing the diagram…</Text>}
            {!!diagramError() && <Text testID="flow-diagram-error" selectable={true} style={text(10, palette().gold)}>{`The diagram could not be drawn: ${diagramError()}`}</Text>}
          </View>}
          {runs().length > 0 && <Text style={{ ...text(10, palette().muted), fontWeight: "600", paddingHorizontal: space(4, theme().scale) }}>Runs</Text>}
          {runs().map((item) => <RunCard run={item} theme={theme} shown={run()?.id === item.id} busy={busy}
            onShow={() => shownRun.set(item.id)} onOpenChat={() => props.openChat(item.chatId)}
            onApprove={(note) => void act(() => api.approve(item.id, note))}
            onDeny={(note) => void act(() => api.deny(item.id, note))}
            onStop={() => void act(() => api.stop(item.id))} />)}
          {data() && <Text selectable={true} style={{ ...text(9, palette().muted), paddingHorizontal: space(4, theme().scale) }}>{`Flows are JSON files in ${data()!.folder}. An agent moves on with riwork flow next EVENT; riwork flow help says more.`}</Text>}
        </View>
      </Scroll>
    </>}
  </View>;
}
