import { List, Text, View, cleanup, effect, signal } from "nsh";
import { Button, Input } from "nsh/base";
import { Addins } from "riwork/native";
import type { AddinInfo } from "./AddinTab";
import { space, type InterfaceTheme } from "./appearance";
import { Icon } from "./Icon";
import { Segmented, StatusPill, buttonLook, cardStyle, focusRing, type ButtonKind } from "./PanelKit";
import { PANEL } from "./panel_layout";

export type AddinsPanelApi = Pick<typeof Addins, "list" | "subscribe" | "create" | "trust" | "revoke" | "reveal" | "setSetting">;

/** The add-ins of the window's project (`.ontack/addins`) and the person's own: open their
 * views, trust them, set their settings, or start a new one from the template. */
export function AddinsPanel(props: {
  projectId: () => string; theme: () => InterfaceTheme; visible: () => boolean;
  open: (key: string, view: string, title: string) => void; notice: (message: string) => void;
  api?: AddinsPanelApi;
}) {
  const theme = props.theme, api = props.api ?? Addins;
  const addins = signal<AddinInfo[]>([]), loading = signal(true), expanded = signal<string | null>(null);
  const creating = signal(false), name = signal(""), personal = signal(false), busy = signal(false);
  let alive = true, request = 0;
  cleanup(() => { alive = false; });
  function load() {
    const project = props.projectId(), mine = ++request;
    void (api.list(project) as Promise<unknown> as Promise<AddinInfo[]>).then((next) => {
      if (alive && mine === request) { addins.set(next); loading.set(false); }
    }).catch((error) => { if (alive) { loading.set(false); props.notice(String(error)); } });
  }
  effect(() => { props.projectId(); load(); });
  const changes = api.subscribe(() => load());
  cleanup(() => changes.close());

  async function run(work: () => Promise<unknown>) {
    if (busy()) return;
    busy.set(true);
    try { await work(); load(); } catch (error) { props.notice(String(error)); } finally { if (alive) busy.set(false); }
  }
  async function create() {
    const wanted = name().trim();
    if (!wanted) return;
    await run(async () => {
      const made = await api.create(props.projectId(), wanted, personal()) as { key: string };
      creating.set(false); name.set("");
      props.open(made.key, "main", wanted);
    });
  }

  const text = (size = 11, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color });
  /** A button: plain (the card color with a hairline), or the view's one `primary` action. */
  const pill = (label: string, action: () => void, testID?: string, kind: ButtonKind = "default") => {
    const look = buttonLook(theme, kind);
    return <View testID={testID} role="button" focusable={true} accessibilityLabel={label} onPress={() => { if (!busy()) action(); }}
      style={{ ...look.style, opacity: busy() ? 0.5 : 1 }} hoverStyle={look.hoverStyle} focusStyle={look.focusStyle}>
      <Text style={{ ...look.label, numberOfLines: 1 }}>{label}</Text>
    </View>;
  };

  function Row(rowProps: { item: () => AddinInfo }) {
    const addin = rowProps.item;
    const open = () => expanded() === addin().key;
    return <View testID={`addin-row:${addin().key}`} style={{ ...cardStyle(theme), marginBottom: 8, gap: 8, borderColor: open() ? theme().palette.text : theme().palette.divider }}
      hoverStyle={{ borderColor: open() ? theme().palette.text : theme().palette.muted }}>
      <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 12, alignItems: "center" }}>
      <View role="button" focusable={true} accessibilityLabel={addin().name ?? addin().key} accessibilityState={{ expanded: open() }} onPress={() => expanded.set(open() ? null : addin().key)}
        style={{ flex: 1, minWidth: 160, gap: 6, borderRadius: space(6, theme().scale) }} focusStyle={focusRing(theme().palette, 2)}>
        <View style={{ flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 6 }}>
          <Icon name="addins" size={18 * theme().scale} color={addin().problem ? theme().palette.danger : addin().trusted ? theme().palette.text : theme().palette.muted} />
          <Text style={{ ...text(12), flex: 1, numberOfLines: 1 }}>{addin().name ?? addin().key}</Text>
          <Text style={text(9, theme().palette.muted)}>{`${addin().scope === "project" ? "Project" : addin().installed ? "Installed" : "Personal"}${addin().version ? ` · ${addin().version}` : ""}`}</Text>
        </View>
        {!!addin().description && <Text style={{ ...text(10, theme().palette.muted), numberOfLines: open() ? 6 : 2 }}>{addin().description!}</Text>}
        {!!addin().problem && <Text style={{ ...text(10, theme().palette.danger), userSelect: "text" }}>{addin().problem!}</Text>}
        {!addin().problem && !addin().trusted && <View style={{ flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 6 }}>
          <StatusPill theme={theme} tone="needs" label="Not trusted yet" />
          <Text style={text(10, theme().palette.muted)}>Opening it asks first.</Text>
        </View>}
      </View>
      {!addin().problem && <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 4, paddingTop: 2 }}>
        {(addin().views ?? []).map((view) => pill(addin().views!.length > 1 ? view.title : "Open", () => props.open(addin().key, view.id, view.title), `addin-open:${addin().key}:${view.id}`))}
        {open() && (addin().trusted
          ? pill("Revoke trust", () => void run(() => api.revoke(props.projectId(), addin().key)))
          : pill("Trust", () => void run(() => api.trust(props.projectId(), addin().key)), `addin-trust:${addin().key}`))}
        {open() && pill("Show folder", () => void api.reveal(props.projectId(), addin().key))}
      </View>}
      </View>
      {open() && !addin().problem && <View style={{ gap: 6, paddingTop: 4 }}>
        {(addin().permissions ?? []).map((permission) => <Text style={text(10, theme().palette.muted)}>{`· ${permission.text}`}</Text>)}
        {(addin().workers ?? []).length > 0 && <Text style={text(10, theme().palette.text)}>{`Runs native code: ${addin().workers!.map((worker) => worker.id).join(", ")}`}</Text>}
        {(addin().tools ?? []).length > 0 && <Text style={text(10, theme().palette.muted)}>{`Agent tools: ${addin().tools!.map((tool) => tool.name).join(", ")}`}</Text>}
        {(addin().settings ?? []).map((setting) => <View style={{ gap: 3 }}>
          <Text style={text(10)}>{setting.label}</Text>
          {setting.type === "boolean"
            ? <View style={{ alignSelf: "flex-start" }}>{pill(setting.value === true ? "On" : "Off", () => void run(() => api.setSetting(props.projectId(), addin().key, setting.key, setting.value !== true)))}</View>
            : <Input testID={`addin-setting:${addin().key}:${setting.key}`} accessibilityLabel={setting.label} autoCorrect={false}
                value={setting.value == null ? "" : String(setting.value)} placeholder={setting.description}
                onSubmit={({ text: value }) => void run(() => api.setSetting(props.projectId(), addin().key, setting.key,
                  value === "" ? null : setting.type === "number" ? Number(value) : value))}
                style={{ ...text(11), paddingHorizontal: 8, paddingVertical: 5, borderRadius: space(8, theme().scale), borderWidth: 1, borderColor: theme().palette.divider, backgroundColor: theme().palette.bg }}
                focusStyle={{ borderColor: theme().palette.focus }} />}
        </View>)}
        <Text style={{ ...text(9, theme().palette.muted), fontFamily: theme().monoFamily, userSelect: "text", numberOfLines: 2 }}>{addin().dir}</Text>
      </View>}
    </View>;
  }

  return <View testID="addins-panel" style={{ flex: 1, minHeight: 0, backgroundColor: theme().palette.panel }}>
    <View style={{ minHeight: space(PANEL.header, theme().scale), paddingHorizontal: space(PANEL.inset, theme().scale), flexDirection: "row", alignItems: "center", gap: 8 }}>
      <View style={{ flex: 1, gap: 1 }}>
        <Text style={{ ...text(13), fontWeight: "600" }}>Add-ins</Text>
        <Text style={text(10, theme().palette.muted)}>{loading() ? "Looking…" : `${addins().length} ${addins().length === 1 ? "add-in" : "add-ins"}`}</Text>
      </View>
      <Button testID="addin-new" accessibilityLabel={creating() ? "Cancel new add-in" : "New add-in"} onPress={() => { if (!busy()) creating.set(!creating()); }} style={{ width: space(PANEL.control, theme().scale), height: space(PANEL.control, theme().scale), borderRadius: space(8, theme().scale), alignItems: "center", justifyContent: "center" }} hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={focusRing(theme().palette, 1)}><Icon name={creating() ? "close" : "add"} size={PANEL.icon * theme().scale} color={theme().palette.muted} /></Button>
    </View>
    {creating() && <View style={{ paddingHorizontal: 12, paddingBottom: 10, gap: 6 }}>
      <Input testID="addin-new-name" accessibilityLabel="Add-in name" autoFocus={true} autoCorrect={false} placeholder="Name, e.g. Security dashboard"
        value={name()} onChange={({ value }) => name.set(value)} onSubmit={() => void create()}
        style={{ ...text(12), paddingHorizontal: 8, paddingVertical: 5, borderRadius: space(8, theme().scale), borderWidth: 1, borderColor: theme().palette.divider, backgroundColor: theme().palette.bg }}
        focusStyle={{ borderColor: theme().palette.focus }} />
      <View style={{ flexDirection: "row", gap: 6, alignItems: "center" }}>
        <Segmented theme={theme} role="radio" accessibilityLabel="Where the add-in is kept" segments={[
          { id: "addin-new-project", label: "In this project", selected: !personal(), onPress: () => { if (!busy()) personal.set(false); } },
          { id: "addin-new-personal", label: "Just for me", selected: personal(), onPress: () => { if (!busy()) personal.set(true); } },
        ]} />
        <View style={{ flex: 1 }} />
        {pill("Create", () => void create(), "addin-create", "primary")}
      </View>
      <Text style={text(10, theme().palette.muted)}>{personal() ? "Kept with your Somtus data, for every project." : "Kept in the project's .ontack/addins, with its code."}</Text>
    </View>}
    <List each={addins()} keyExtractor={(addin) => addin.key} estimatedItemSize={space(56, theme().scale)} style={{ flex: 1, minHeight: 0 }}
      contentStyle={{ paddingHorizontal: space(PANEL.inset, theme().scale), paddingBottom: 8 }}
      empty={<View style={{ padding: 24, gap: 8, alignItems: "center" }}>
        <Icon name="addins" size={22 * theme().scale} color={theme().palette.muted} />
        <Text style={text(11, theme().palette.muted)}>{loading() ? "" : "No add-ins yet"}</Text>
      </View>}>
      {(item) => <Row item={item} />}
    </List>
    <Text style={{ ...text(10, theme().palette.muted), paddingHorizontal: 12, paddingVertical: 8 }}>Add project tools and views. Open an add-in to review its requested permissions.</Text>
  </View>;
}
