import { Scroll, Text, View, cleanup, mount, signal } from "nsh";
import { hosts, info, onPaletteChanged, type HostsSnapshot } from "./addin-api";

function HostAgents() {
  const palette = signal(info().palette ?? {});
  cleanup(onPaletteChanged(value => palette.set(value)));
  const snapshot = signal<HostsSnapshot | null>(null), error = signal("");
  let alive = true, stop: (() => void) | undefined;
  cleanup(() => { alive = false; stop?.(); });
  void hosts.subscribe(value => snapshot.set(value)).then(unsubscribe => {
    if (alive) stop = unsubscribe; else unsubscribe();
  }).catch(reason => { if (alive) error.set(String(reason)); });
  const ink = () => palette().text ?? "#222", muted = () => palette().muted ?? "#777";
  return <Scroll style={{flex:1,backgroundColor:palette().bg ?? "transparent"}}><View style={{padding:14,gap:12}}>
    <Text style={{fontSize:17,fontWeight:"600",color:ink()}}>Agents across hosts</Text>
    {error() && <Text role="alert" style={{color:palette().gold ?? "#b60"}}>{error()}</Text>}
    {snapshot()?.hosts.map(host => <View style={{gap:6}}>
      <Text style={{fontSize:13,fontWeight:"600",color:ink()}}>{host.label}</Text>
      <Text style={{fontSize:11,color:muted()}}>{`${host.connection} · ${host.freshness === "stale" ? "last known state" : host.freshness}`}</Text>
      {host.snapshot?.sessions.map(session => <View style={{padding:8,gap:3,backgroundColor:"#8881",borderRadius:5}}>
        <Text style={{color:ink()}}>{session.title}</Text>
        <Text style={{fontSize:11,color:muted()}}>{[session.state,...session.requests,session.subagents ? `${session.subagents} subagents` : null,session.unseen ? "unseen completion" : null].filter(Boolean).join(" · ")}</Text>
      </View>)}
    </View>)}
  </View></Scroll>;
}
mount(HostAgents);
