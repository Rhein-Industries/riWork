import { Scroll, Text, View, effect, signal, type ViewRef } from "nsh";
import { Input } from "nsh/base";
import { Hosts } from "riwork/native";
import { Icon } from "../Icon";
import { KitButton, Segmented, StatusPill, searchBox, type Tone } from "../PanelKit";
import { PANEL } from "../panel_layout";
import { space, type InterfaceTheme } from "../theme";
import { connectionText, needsAttention, sessions, type AgentHost, type AgentSession, type Filter, type HostSnapshot } from "./model";

const FILTERS: [Filter, string][] = [["all", "All"], ["working", "Working"], ["attention", "Needs you"], ["unseen", "Unseen"]];

/** One demand-driven subscription, regardless of the number of displayed hosts. */
export function Agents(props: { visible: () => boolean; theme: () => InterfaceTheme; onOpen: (host: AgentHost, session: AgentSession) => void; onNotice: (message: string) => void; subscribe?: (listener: (event: {rev:number;data:HostSnapshot}) => void) => {close: () => void}; onSearch?: (node: {focus: () => void}) => void; markSeen?: (host: AgentHost,session: AgentSession) => Promise<void> }) {
  const snapshot = signal<HostSnapshot>({ version: 1, hosts: [], error: null });
  const filter = signal<Filter>("all");
  const query = signal("");
  const focused = signal(false);
  const limits = signal<Record<string,number>>({});
  let search: ViewRef | undefined;
  props.onSearch?.({focus: () => search?.focus()});
  effect(() => {
    if (!props.visible()) return;
    let revision = 0;
    const subscribe = props.subscribe ?? (listener => Hosts.subscribe(raw => listener(raw as unknown as {rev:number;data:HostSnapshot})));
    const subscription = subscribe((raw) => {
      const event = raw as unknown as { rev: number; data: HostSnapshot };
      if (event.rev > revision) { revision = event.rev; snapshot.set(event.data); }
    });
    return () => subscription.close();
  });
  const palette = () => props.theme().palette;
  const scale = () => props.theme().scale;
  const font = (size = 11, color = palette().text) => ({ fontFamily: props.theme().family, fontSize: size * scale(), color });
  const all = () => snapshot().hosts.flatMap((host) => host.snapshot?.sessions ?? []);
  const count = (id: Filter) => snapshot().hosts.reduce((sum, host) => sum + sessions(host, id, query()).length, 0);
  const shown = () => count(filter());
  /** What the person should know about a session at a glance, as a pill. */
  function state(session: AgentSession): { tone: Tone; label: string } {
    if (session.state === "error") return { tone: "failed", label: "Failed" };
    if (needsAttention(session)) return { tone: "needs", label: session.requests.length ? "Needs you" : session.state === "blocked" ? "Blocked" : "Waiting" };
    if (session.state === "working") return { tone: "working", label: "Working" };
    return { tone: "neutral", label: session.state ? session.state[0].toUpperCase() + session.state.slice(1) : "Idle" };
  }
  function markSeen(host: AgentHost, session: AgentSession) {
    if (!session.completion) return;
    void (props.markSeen ? props.markSeen(host,session) : Hosts.markSeen(host.id, session.kind, session.id, session.completion)).catch(error => props.onNotice(String(error)));
  }
  function row(host: () => AgentHost, session: () => AgentSession) {
    const detail = () => [session().provider, session().state, ...session().requests, session().subagents ? `${session().subagents} subagents` : null, session().source, host().freshness === "stale" ? "last known" : null].filter(Boolean).join(" · ");
    // The pill already says the state; the line under the title says the rest.
    const meta = () => [session().provider, ...session().requests, session().subagents ? `${session().subagents} subagents` : null, session().source, host().freshness === "stale" ? "last known" : null].filter(Boolean).join(" · ");
    return <View testID={`agent-row-${host().id}-${session().kind}-${session().id}`} style={{ flexDirection: "row", alignItems: "center", gap: space(6, scale()) }}>
      <View testID={`agent-${host().id}-${session().kind}-${session().id}`} role="button" accessibilityLabel={`${session().title}; ${detail()}`} focusable onPress={() => props.onOpen(host(),session())}
        hoverStyle={{ backgroundColor: palette().panelActive }} focusStyle={{ outlineStyle: "solid", outlineWidth: 2, outlineOffset: -2, outlineColor: palette().focus }}
        style={{ flex: 1, minWidth: 0, flexDirection: "row", alignItems: "flex-start", gap: space(8, scale()), paddingHorizontal: space(6, scale()), paddingVertical: space(6, scale()), borderRadius: space(8, scale()) }}>
        <View style={{ paddingTop: 1 }}><Icon name={session().kind === "chat" ? "chat" : "terminal"} size={13 * scale()} color={palette().muted} /></View>
        <View style={{ flex: 1, minWidth: 0, gap: space(4, scale()) }}>
          <Text style={{ ...font(11), fontWeight: session().unseen ? "600" : "400", numberOfLines: 1 }}>{session().title}</Text>
          <View style={{ flexDirection: "row", alignItems: "center", gap: space(6, scale()), minWidth: 0 }}>
            <StatusPill theme={props.theme} tone={state(session()).tone} label={state(session()).label} inline={true} />
            {meta() !== "" && <Text style={{ ...font(10, palette().muted), flexShrink: 1, minWidth: 0, numberOfLines: 1 }}>{meta()}</Text>}
          </View>
        </View>
      </View>
      {session().unseen && <KitButton theme={props.theme} kind="quiet" id={`agent-seen-${host().id}-${session().id}`} label="Mark seen" onPress={() => markSeen(host(),session())} />}
    </View>;
  }
  return <View testID="agents-overview" style={{ flex: 1, minHeight: 0, backgroundColor: palette().panel }}>
    <View style={{ minHeight: space(PANEL.header, scale()), paddingHorizontal: space(PANEL.inset, scale()), justifyContent: "center", gap: 1 }}>
      <Text style={{ ...font(13), fontWeight: "600", numberOfLines: 1 }}>Agents</Text>
      <Text testID="agents-count" style={{ ...font(10, palette().muted), numberOfLines: 1 }}>{snapshot().hosts.length === 0 ? "Reading hosts…"
        : `${shown() === all().length ? all().length : `${shown()} of ${all().length}`} ${all().length === 1 ? "session" : "sessions"} · ${snapshot().hosts.length} ${snapshot().hosts.length === 1 ? "host" : "hosts"}`}</Text>
    </View>
    <View style={{ flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: space(8, scale()), paddingHorizontal: space(12, scale()), paddingBottom: space(8, scale()) }}>
      <View style={{ ...searchBox(props.theme, focused()).style, flexGrow: 1, flexShrink: 1, flexBasis: space(100, scale()) }} hoverStyle={searchBox(props.theme, focused()).hoverStyle} onPress={() => search?.focus()}>
        <Icon name="search" size={13 * scale()} color={palette().muted} />
        <Input testID="agents-search" ref={node => {search=node;}} accessibilityLabel="Search hosts and agents" placeholder="Search" value={query()} onChange={({ value }) => query.set(value)}
          onFocus={() => focused.set(true)} onBlur={() => focused.set(false)}
          style={{ flex: 1, minWidth: 0, padding: 0, backgroundColor: "transparent", ...font(11) }} />
      </View>
      <Segmented theme={props.theme} role="radio" compact={true} accessibilityLabel="Show agents"
        segments={FILTERS.map(([id, label]) => ({ id: `agents-filter-${id}`, label, count: count(id), selected: filter() === id, onPress: () => filter.set(id) }))} />
    </View>
    <Scroll style={{ flex: 1, minHeight: 0 }}><View style={{ paddingHorizontal: space(PANEL.gutter, scale()), paddingBottom: space(12, scale()), gap: space(14, scale()) }}>
      {snapshot().error && <Text role="alert" style={{ ...font(11,palette().danger), paddingHorizontal: space(6, scale()) }}>{snapshot().error}</Text>}
      <Each of={snapshot().hosts} key={host => host.id}>{host => <View testID={`agents-host-${host().connectionId ?? "local"}`} style={{ gap: 2 }}>
        <View style={{ flexDirection: "row", gap: 8, alignItems: "center", paddingHorizontal: space(6, scale()), paddingVertical: space(4, scale()) }}>
          <Text style={{ ...font(10, palette().muted), fontWeight: "600", flexShrink: 1, numberOfLines: 1 }}>{host().label}</Text>
          <StatusPill theme={props.theme} tone={host().connection === "online" ? host().freshness === "stale" ? "needs" : "done" : host().connection === "blocked" ? "needs" : "neutral"} label={connectionText(host())} inline={true} />
        </View>
        {host().error && <Text style={{ ...font(10,palette().danger), paddingHorizontal: space(6, scale()) }}>{host().error}</Text>}
        <Each of={sessions(host(),filter(),query()).slice(0,limits()[host().id] ?? 80)} key={session => `${session.kind}:${session.id}`}>{session => row(host,session)}</Each>
        {sessions(host(),filter(),query()).length > (limits()[host().id] ?? 80) && <View style={{ alignSelf: "flex-start", paddingHorizontal: space(6, scale()) }}>
          <KitButton theme={props.theme} id={`agents-more-${host().id}`} label="Show more sessions" onPress={() => limits.set({...limits(),[host().id]:(limits()[host().id] ?? 80)+80})} />
        </View>}
        {sessions(host(),filter(),query()).length === 0 && <Text style={{ ...font(11,palette().muted), paddingHorizontal: space(6, scale()), paddingVertical: space(4, scale()) }}>{host().snapshot ? "No matching sessions" : "Waiting for session status"}</Text>}
        {host().snapshot?.partial && <Text style={{ ...font(10,palette().muted), paddingHorizontal: space(6, scale()) }}>Some session details are unavailable. Open a session for its complete activity.</Text>}
      </View>}</Each>
    </View></Scroll>
  </View>;
}
