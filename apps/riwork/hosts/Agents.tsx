import { Scroll, Text, View, effect, signal, type YNode } from "nsh";
import { Input } from "nsh/base";
import { Hosts } from "riwork/native";
import type { InterfaceTheme } from "../theme";
import { connectionText, needsAttention, sessions, type AgentHost, type AgentSession, type Filter, type HostSnapshot } from "./model";

/** One demand-driven subscription, regardless of the number of displayed hosts. */
export function Agents(props: { visible: () => boolean; theme: () => InterfaceTheme; onOpen: (host: AgentHost, session: AgentSession) => void; onNotice: (message: string) => void; subscribe?: (listener: (event: {rev:number;data:HostSnapshot}) => void) => {close: () => void}; onSearch?: (node: {focus: () => void}) => void; markSeen?: (host: AgentHost,session: AgentSession) => Promise<void> }) {
  const snapshot = signal<HostSnapshot>({ version: 1, hosts: [], error: null });
  const filter = signal<Filter>("all");
  const query = signal("");
  const limits = signal<Record<string,number>>({});
  let search: YNode | undefined;
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
  const font = (size = 11, color = palette().text) => ({ fontFamily: props.theme().family, fontSize: size * props.theme().scale, color });
  function button(id: string, text: string, action: () => void, selected = false) {
    return <View testID={id} role="button" accessibilityLabel={text} focusable onPress={action}
      style={{ paddingHorizontal: 9, paddingVertical: 5, borderRadius: 5, backgroundColor: selected ? palette().panelActive : "transparent" }}>
      <Text style={font(11,selected ? palette().text : palette().muted)}>{text}</Text>
    </View>;
  }
  function markSeen(host: AgentHost, session: AgentSession) {
    if (!session.completion) return;
    void (props.markSeen ? props.markSeen(host,session) : Hosts.markSeen(host.id, session.kind, session.id, session.completion)).catch(error => props.onNotice(String(error)));
  }
  function row(host: () => AgentHost, session: () => AgentSession) {
    const detail = () => [session().provider, session().state, ...session().requests, session().subagents ? `${session().subagents} subagents` : null, session().source, host().freshness === "stale" ? "last known" : null].filter(Boolean).join(" · ");
    return <View testID={`agent-row-${host().id}-${session().kind}-${session().id}`} style={{ flexDirection: "row", alignItems: "center", paddingVertical: 7, gap: 8, borderBottomWidth: 1, borderColor: palette().divider }}>
      <View testID={`agent-${host().id}-${session().kind}-${session().id}`} role="button" accessibilityLabel={`${session().title}; ${detail()}`} focusable onPress={() => props.onOpen(host(),session())} hoverStyle={{ backgroundColor: palette().panelActive }} style={{ flex: 1, paddingHorizontal: 8, paddingVertical: 4, borderRadius: 5 }}>
        <Text numberOfLines={1} style={{ ...font(12,needsAttention(session()) ? palette().gold : palette().text), fontWeight: "500" }}>{session().title}</Text>
        <Text numberOfLines={2} style={font(10,palette().muted)}>{detail()}</Text>
      </View>
      {session().unseen && button(`agent-seen-${host().id}-${session().id}`,"Mark seen",() => markSeen(host(),session()))}
    </View>;
  }
  return <View testID="agents-overview" style={{ flex: 1, minHeight: 0 }}>
    <View style={{ flexDirection: "row", alignItems: "center", gap: 5, padding: 10, borderBottomWidth: 1, borderColor: palette().divider }}>
      <View style={{ flex: 1 }}><Input testID="agents-search" ref={node => {search=node;}} accessibilityLabel="Search hosts and agents" placeholder="Search hosts and agents" value={query()} onChange={value => query.set(value)} /></View>
      {([ ["all","All"], ["working","Working"], ["attention","Needs you"], ["unseen","Unseen"] ] as [Filter,string][]).map(([id,label]) => button(`agents-filter-${id}`,label,() => filter.set(id),filter() === id))}
    </View>
    <Scroll style={{ flex: 1 }}><View style={{ padding: 14, gap: 16 }}>
      {snapshot().error && <Text role="alert" style={font(11,palette().gold)}>{snapshot().error}</Text>}
      {snapshot().hosts.length === 0 && <Text style={font(11,palette().muted)}>Reading hosts…</Text>}
      <Each of={snapshot().hosts} key={host => host.id}>{host => <View testID={`agents-host-${host().connectionId ?? "local"}`} style={{ gap: 4 }}>
        <View style={{ flexDirection: "row", gap: 8, alignItems: "center" }}>
          <Text style={{ ...font(13), fontWeight: "600" }}>{host().label}</Text>
          <Text style={font(10,palette().muted)}>{connectionText(host())}</Text>
        </View>
        {host().error && <Text style={font(10,palette().gold)}>{host().error}</Text>}
        <Each of={sessions(host(),filter(),query()).slice(0,limits()[host().id] ?? 80)} key={session => `${session.kind}:${session.id}`}>{session => row(host,session)}</Each>
        {sessions(host(),filter(),query()).length > (limits()[host().id] ?? 80) && button(`agents-more-${host().id}`,"Show more sessions",() => limits.set({...limits(),[host().id]:(limits()[host().id] ?? 80)+80}))}
        {sessions(host(),filter(),query()).length === 0 && <Text style={font(11,palette().muted)}>{host().snapshot ? "No matching sessions" : "Waiting for session status"}</Text>}
        {host().snapshot?.partial && <Text style={font(10,palette().muted)}>Some session details are unavailable. Open a session for its complete activity.</Text>}
      </View>}</Each>
    </View></Scroll>
  </View>;
}
