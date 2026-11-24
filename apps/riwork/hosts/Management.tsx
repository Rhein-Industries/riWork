import { Scroll, Text, View, cleanup, effect, signal, untracked, type ViewRef } from "nsh";
import { Button, Input } from "nsh/base";
import type { HostManagement } from "riwork/native";
import type { InterfaceTheme } from "../theme";

export type HostManagementApi = Pick<typeof HostManagement, "discoverTailscaleMacs" | "sshRoutes" | "addSsh">;
type Mac = Awaited<ReturnType<HostManagementApi["discoverTailscaleMacs"]>>[number];
type Route = Awaited<ReturnType<HostManagementApi["sshRoutes"]>>[number];

function unregisteredMacs(macs: Mac[], routes: Route[]) {
  const normalize = (value: string) => value.trim().toLowerCase().replace(/\.$/, "").replace(/^\[|\]$/g, "");
  const destinations = new Set(routes.map(route => normalize(route.target.split("@").pop() ?? "")));
  return macs.filter(mac => {
    const dns = normalize(mac.dnsName ?? "");
    return ![normalize(mac.target), dns, dns.split(".")[0]].some(name => name && destinations.has(name));
  });
}

export function HostManagementPanel(props: {
  visible: () => boolean; theme: () => InterfaceTheme; api: HostManagementApi; onSearch?: (node: ViewRef) => void;
}) {
  const macs = signal<Mac[]>([]), routes = signal<Route[]>([]), query = signal("");
  const discoveryError = signal(""), routeError = signal(""), busy = signal(false);
  const editing = signal(false), connecting = signal(false), error = signal(""), notice = signal("");
  const label = signal(""), target = signal(""), companion = signal(""), cli = signal("");
  let generation = 0, alive = true;
  cleanup(() => { alive = false; generation++; });

  async function refresh() {
    const request = ++generation;
    busy.set(true);
    const [discovered, saved] = await Promise.allSettled([props.api.discoverTailscaleMacs(), props.api.sshRoutes()]);
    if (!alive || request !== generation) return;
    busy.set(false);
    if (discovered.status === "fulfilled") { macs.set(discovered.value); discoveryError.set(""); }
    else { macs.set([]); discoveryError.set(String(discovered.reason).replace(/^Error: /, "")); }
    if (saved.status === "fulfilled") { routes.set(saved.value); routeError.set(""); }
    else { routeError.set(String(saved.reason).replace(/^Error: /, "")); }
  }
  effect(() => {
    if (!props.visible()) return;
    untracked(() => { void refresh(); });
    return () => { generation++; };
  });
  function openForm(mac?: Mac) {
    target.set(mac?.target ?? ""); label.set(mac?.label ?? "");
    companion.set(mac ? "/Applications/Somtus.app/Contents/MacOS/somtus-remote" : "");
    cli.set(mac ? "/Applications/Somtus.app/Contents/MacOS/somtus" : "");
    error.set(""); notice.set(""); editing.set(true);
  }
  async function connect() {
    if (connecting() || !target().trim() || !label().trim()) return;
    connecting.set(true); error.set("");
    const name = label().trim();
    try {
      await props.api.addSsh({ target: target().trim(), label: name,
        ...(companion().trim() ? { remoteBin: companion().trim() } : {}),
        ...(cli().trim() ? { somtus: cli().trim() } : {}) });
      if (alive) {
        editing.set(false); notice.set(`Connected ${name}. Choose it in the host selector to open its projects.`);
        await refresh();
      }
    } catch (cause) { if (alive) error.set(String(cause).replace(/^Error: /, "")); }
    finally { if (alive) connecting.set(false); }
  }
  const palette = () => props.theme().palette;
  const font = (size = 12, color = palette().text) => ({ fontFamily: props.theme().family, fontSize: size * props.theme().scale, color });
  const row = { flexDirection: "row" as const, alignItems: "center" as const, flexWrap: "wrap" as const, gap: 8 };
  const card = () => ({ padding: 12, gap: 8, borderRadius: 8, backgroundColor: palette().panel, borderWidth: 1, borderColor: palette().divider });
  const input = () => ({ ...font(), paddingHorizontal: 8, paddingVertical: 6, borderWidth: 1, borderRadius: 5, borderColor: palette().divider, backgroundColor: palette().bg });
  function button(id: string, title: string, action: () => void, disabled = false) {
    return <Button testID={id} accessibilityLabel={title} disabled={disabled || connecting()} onPress={action}
      style={{ paddingHorizontal: 9, paddingVertical: 5, borderRadius: 5, backgroundColor: palette().panelActive }}>
      <Text style={font(11)}>{title}</Text></Button>;
  }
  function field(id: string, title: string, value: typeof target, placeholder?: string) {
    return <View style={{ gap: 3 }}><Text style={font(11, palette().muted)}>{title}</Text>
      <Input testID={id} accessibilityLabel={title} autoCorrect={false} value={value()} placeholder={placeholder}
        disabled={connecting()} onChange={({ value: text }) => { value.set(text); error.set(""); }} style={input()} /></View>;
  }
  const matches = (...values: (string | null | undefined)[]) => values.join(" ").toLocaleLowerCase().includes(query().trim().toLocaleLowerCase());
  const candidates = () => unregisteredMacs(macs(), routes()).filter(mac => matches(mac.label, mac.target, mac.dnsName));
  const saved = () => routes().filter(route => matches(route.target, route.label));
  return <View testID="host-management" style={{ flex: 1 }}>
    <View style={{ ...row, padding: 12, borderBottomWidth: 1, borderColor: palette().divider }}>
      <Text role="header" style={{ ...font(15), fontWeight: "600" }}>Hosts</Text>
      <View style={{ flex: 1 }}><Input testID="host-management-search" ref={node => props.onSearch?.(node)} accessibilityLabel="Search hosts"
        placeholder="Search hosts" value={query()} onChange={({ value }) => query.set(value)} style={input()} /></View>
      {button("hosts-refresh", busy() ? "Refreshing…" : "Refresh", () => void refresh(), busy())}
      {button("hosts-add", "Add SSH host", () => openForm())}
    </View>
    <Scroll style={{ flex: 1 }}><View style={{ padding: 12, gap: 12 }}>
      {!!notice() && <Text role="alert" style={font(12, palette().cyan)}>{notice()}</Text>}
      {editing() && <View testID="host-connect-form" style={card()}>
        <Text role="header" style={{ ...font(13), fontWeight: "600" }}>Connect an SSH host</Text>
        <Text style={font(11, palette().muted)}>Enable Remote Login on the other Mac and verify its SSH host key. If your login there is different, enter username@host.</Text>
        {field("host-target", "SSH target", target, "account@studio-mac")}
        {field("host-label", "Host label", label)}
        {field("host-remote-bin", "Remote companion path (optional)", companion, "somtus-remote")}
        {field("host-somtus", "Remote Somtus CLI path (optional)", cli)}
        {!!error() && <Text role="alert" style={{ ...font(11, palette().gold), userSelect: "text" }}>{error()}</Text>}
        <View style={row}>{button("host-connect", connecting() ? "Connecting…" : "Connect host", () => void connect(), !target().trim() || !label().trim())}
          {button("host-form-cancel", "Cancel", () => editing.set(false))}</View>
      </View>}
      <View testID="tailscale-macs" style={card()}>
        <Text role="header" style={{ ...font(13), fontWeight: "600" }}>Your Macs on Tailscale</Text>
        {!!discoveryError() && <Text testID="tailscale-discovery-error" style={font(11, palette().muted)}>{discoveryError()}</Text>}
        {!discoveryError() && !candidates().length && <Text style={font(11, palette().muted)}>{busy() ? "Looking for Macs…" : query().trim() ? "No matching Macs." : "No other Macs to add on this Tailscale account."}</Text>}
        {candidates().map(mac => <View testID={`tailscale-peer-${mac.id}`} style={row}>
          <View style={{ flex: 1, gap: 3 }}><Text style={font()}>{mac.label}</Text><Text style={{ ...font(10, palette().muted), userSelect: "text" }}>{mac.dnsName || mac.target}</Text></View>
          <Text style={font(11, mac.online ? palette().cyan : palette().muted)}>{mac.online ? "Online on Tailscale" : "Offline on Tailscale"}</Text>
          {button(`tailscale-setup-${mac.id}`, "Set up SSH", () => openForm(mac))}
        </View>)}
      </View>
      {(!!routeError() || saved().length > 0) && <View style={card()}>
        <Text role="header" style={{ ...font(13), fontWeight: "600" }}>Saved SSH hosts</Text>
        {!!routeError() && <Text role="alert" style={font(11, palette().gold)}>{routeError()}</Text>}
        {saved().map(route => <View testID={`managed-host-${route.id}`} style={row}><Text style={font()}>{route.label}</Text><Text style={{ ...font(11, palette().muted), userSelect: "text" }}>{route.target}</Text></View>)}
      </View>}
    </View></Scroll>
  </View>;
}
