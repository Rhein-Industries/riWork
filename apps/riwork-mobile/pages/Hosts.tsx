/** The paired hosts (`relay.hosts`) and, under them, the All hosts overview Swift reads over its own short
 * connections (`mobile.snapshot` `homeDashboard`). Pairing, a host's details and the display settings are
 * native Swift sheets (`nav.present`). */
import { Scroll, Text, View, effect, keymap, signal } from "nsh";
import type { MobileApp } from "../app_state";
import { SECTION_LABELS, SECTION_ORDER, type Dashboard, type DashboardFilter, type DashboardRow, type DashboardSection } from "../model";
import { Symbol } from "../Symbol";
import type { Kit, Layout } from "../ui";

export function HostsPage(props: { app: MobileApp; ui: Kit; layout: Layout; onTop: () => boolean }) {
  const { app, ui } = props;
  const { Action, Section, Divider, Note, Failure, palette, pt, target, text } = ui;
  const filter = signal<DashboardFilter>("all");
  const bottom = ui.bottomInset();
  const blocked = () => !app.loaded() || app.blocked();
  const refresh = () => { void app.act({ type: "refresh" }); void app.device.hosts.list().catch(() => {}); };
  effect(() => {
    if (!props.onTop()) return;
    app.refresher.current = refresh;
    return () => { if (app.refresher.current === refresh) app.refresher.current = null; };
  });
  effect(() => { if (props.onTop() && !blocked()) return keymap.bind("mod+,", () => void app.present({ name: "settings" })); });

  function HostRow(rowProps: { id: string; index: number }) {
    const info = () => app.hosts().find((entry) => entry.id === rowProps.id);
    const detail = () => app.overview().details[rowProps.id];
    const status = () => {
      const link = info()?.link;
      if (!link || link.state === "offline") return detail() ?? link?.label ?? "Not connected";
      return link.error ? `${link.label} · ${link.error}` : link.label;
    };
    return <>
      {rowProps.index > 0 && <Divider />}
      <View style={{ flexDirection: "row", alignItems: "center" }}>
        <View testID={`host:${rowProps.id}`} role="button" accessibilityLabel={`${info()?.name ?? "Desktop"}. ${status()}. ${info()?.address ?? ""}`}
          disabled={blocked()} disabledStyle={{ opacity: 0.5 }} activeStyle={{ backgroundColor: palette().panelActive }}
          onPress={() => app.openHost(rowProps.id)}
          style={{ flex: 1, minHeight: target(), padding: pt(16), flexDirection: "row", alignItems: "center", gap: pt(13) }}>
          <Symbol name="desktopcomputer" color={palette().cyan} size={pt(24)} />
          <View style={{ flex: 1, gap: pt(4) }}>
            <Text style={{ ...text(16), fontWeight: "600" }}>{info()?.name ?? "Desktop"}</Text>
            {info()?.address && <Text style={text(12, "muted")}>{info()!.address!}</Text>}
            <Text style={text(12, info()?.link.state === "failed" ? "error" : "muted")}>{status()}</Text>
          </View>
          <Symbol name="chevron.right" color={palette().muted} size={pt(13)} />
        </View>
        <View role="button" accessibilityLabel={`Manage ${info()?.name ?? "desktop"}`} disabled={blocked()}
          onPress={() => void app.present({ name: "hostDetails", host: rowProps.id })}
          activeStyle={{ backgroundColor: palette().panelActive }} style={{ minWidth: target(), minHeight: target(), alignItems: "center", justifyContent: "center", marginRight: pt(4) }}>
          <Symbol name="ellipsis.circle" color={palette().muted} size={pt(21)} />
        </View>
      </View>
    </>;
  }

  return <ui.Page layout={props.layout} title="RiWork" testID="page:hosts" backTitle="Hosts" addButton={true}
    right={<Action label="Add desktop" symbol={props.layout === "pad" ? "plus" : undefined} compact={props.layout === "pad"}
      action={() => void app.present({ name: "addHost" })} disabled={blocked() || app.overview().libraryUnavailable} />}>
    <Scroll style={{ flex: 1 }} contentStyle={{ padding: pt(16), paddingBottom: Math.max(pt(24), bottom() + pt(16)), gap: pt(20) }}
      refreshing={app.overview().loading} onRefresh={refresh} keyboardDismissMode="interactive" keyboardShouldPersistTaps="handled">
      <Failure />
      {app.overview().error && <Note message={app.overview().error!} error={true} />}
      {!app.loaded() && <Note message="Loading your desktops…" />}
      {app.overview().libraryUnavailable ? <Note title="Saved desktops unavailable" message="The saved desktop library could not be read. Open recovery options to restore access.">
        <Action label="Recovery options" action={() => void app.present({ name: "libraryRecovery" })} disabled={app.blocked()} />
      </Note> : <>
        {app.loaded() && app.hosts().length === 0 && <Note title="Your desktop, wherever you are" message="Pair your Mac to see its projects, review work, and continue in your chats and terminals.">
          <Action label="Pair a desktop" symbol="plus" prominent={true} action={() => void app.present({ name: "addHost" })} disabled={blocked()} />
        </Note>}
        {app.hosts().length > 0 && <Section title="Desktops">
          {app.hosts().map((info, index) => <HostRow id={info.id} index={index} />)}
        </Section>}
        {app.overview().homeDashboard && app.hosts().length > 0 && <Overview app={app} ui={ui} dashboard={app.overview().homeDashboard!} filter={filter()}
          onFilter={(value) => filter.set(value)} blocked={blocked()} />}
      </>}
      <Section>
        <Action label="Display and keyboard settings" symbol="slider.horizontal.3" disabled={app.blocked()} action={() => void app.present({ name: "settings" })} />
      </Section>
    </Scroll>
  </ui.Page>;
}

/** Across your desktops: totals, the Haiku overviews and every host's rows, a row opening where it leads
 * (Swift connects to its host first). */
function Overview(props: { app: MobileApp; ui: Kit; dashboard: Dashboard; filter: DashboardFilter; onFilter: (value: DashboardFilter) => void; blocked: boolean }) {
  const { ui } = props;
  const { Action, Section, Divider, Note, palette, pt, target, text } = ui;
  const expanded = signal<DashboardSection[]>([]);
  const includes = (section: DashboardSection) => props.filter === "all" || (props.filter === "needs_you" ? section === "decisions" || section === "attention" : section === props.filter);
  const localLimit = (section: DashboardSection) => expanded().includes(section) ? Number.MAX_SAFE_INTEGER : section === "projects" ? (props.filter === "projects" ? 150 : 8) : section === "decisions" ? 20 : 12;
  const anyShown = () => props.dashboard.rows.some((row) => includes(row.section));
  const empty = () => props.filter === "needs_you" ? "No pending decisions or attention rows in the latest read."
    : props.filter === "working" ? "No working agents in the latest read."
    : props.filter === "projects" ? "No projects in the latest dashboard."
    : "No activity in the latest dashboard.";
  const choices: { value: DashboardFilter; label: string }[] = [
    { value: "all", label: "All" }, { value: "needs_you", label: "Needs you" }, { value: "working", label: "Working" }, { value: "projects", label: "Projects" },
  ];
  function Cell(cell: { row: DashboardRow }) {
    return <View role={cell.row.enabled ? "button" : "text"} accessibilityLabel={[cell.row.hostName, cell.row.stale ? "Saved data" : undefined, cell.row.title, cell.row.subtitle, cell.row.status].filter(Boolean).join(". ")}
      disabled={cell.row.enabled && props.blocked} disabledStyle={{ opacity: 0.5 }} activeStyle={cell.row.enabled ? { backgroundColor: palette().panelActive } : undefined}
      onPress={cell.row.enabled ? () => void props.app.act({ type: "openDashboardRow", rowID: cell.row.id }) : undefined}
      style={{ minHeight: target(), padding: pt(16), flexDirection: "row", alignItems: "center", gap: pt(12) }}>
      <View style={{ flex: 1, gap: pt(5) }}>
        {cell.row.hostName && <Text style={text(12, cell.row.stale ? "gold" : "accent")}>{`${cell.row.hostName}${cell.row.stale ? " · Saved" : ""}`}</Text>}
        <Text style={{ ...text(15), fontWeight: "500" }}>{cell.row.title}</Text>
        {cell.row.subtitle && <Text style={text(13, "muted")}>{cell.row.subtitle}</Text>}
        {cell.row.status && <Text style={text(12, cell.row.tone)}>{cell.row.status}</Text>}
      </View>
      {cell.row.enabled && <Symbol name="chevron.right" color={palette().muted} size={pt(13)} />}
    </View>;
  }
  return <>
    <Text role="header" style={{ ...text(20), fontWeight: "600" }}>Across your desktops</Text>
    {props.dashboard.summary.length > 0 && <View style={{ flexDirection: "row", flexWrap: "wrap", gap: pt(10) }}>
      {props.dashboard.summary.map((metric) => <View style={{ flexGrow: 1, flexBasis: pt(120), padding: pt(14), gap: pt(5), borderRadius: pt(12), backgroundColor: palette().panel }}>
        <Text style={text(12, "muted")}>{metric.label}</Text>
        <Text style={{ ...text(25), fontWeight: "600" }}>{metric.value == null ? "—" : String(metric.value)}</Text>
        {metric.coverage && <Text style={text(11, "muted")}>{metric.coverage}</Text>}
      </View>)}
    </View>}
    {props.dashboard.freshness && <Text style={text(12, props.dashboard.stale ? "gold" : "muted")}>{props.dashboard.freshness}</Text>}
    {props.dashboard.automationsError && <Text style={text(12, "error")}>{props.dashboard.automationsError}</Text>}
    {(props.dashboard.notes?.length ?? 0) > 0 && <View style={{ gap: pt(5) }}>
      {props.dashboard.notes!.map((note) => <Text style={text(12, "muted")}>{note}</Text>)}
    </View>}
    <Scroll horizontal={true} scrollbar={false} style={{ flexShrink: 0 }} contentStyle={{ flexDirection: "row", gap: pt(4) }}>
      <View role="tablist" accessibilityLabel="Dashboard filter" style={{ flexDirection: "row", padding: pt(3), gap: pt(3), borderRadius: pt(11), backgroundColor: palette().panel }}>
        {choices.map((choice) => <View role="tab" accessibilityLabel={choice.label} accessibilityState={{ selected: props.filter === choice.value }}
          onPress={() => props.onFilter(choice.value)} activeStyle={{ backgroundColor: palette().panelActive }}
          style={{ minHeight: target(), minWidth: pt(62), paddingHorizontal: pt(12), paddingVertical: pt(8), justifyContent: "center", alignItems: "center",
            borderRadius: pt(8), backgroundColor: props.filter === choice.value ? palette().panelActive : "transparent" }}>
          <Text style={{ ...text(14, props.filter === choice.value ? "text" : "muted"), fontWeight: props.filter === choice.value ? "600" : "400" }}>{choice.label}</Text>
        </View>)}
      </View>
    </Scroll>
    {props.filter === "all" && props.dashboard.intelligence?.map((report) => <Section title={report.hostName ? `${report.hostName} · Haiku overview` : "Haiku overview"}>
      <View style={{ padding: pt(16), gap: pt(10) }}>
        {report.stale && <Text style={text(12, "gold")}>Saved summary</Text>}
        {report.summary ? <Text selectable={true} style={text(15)}>{report.summary}</Text>
          : <Text style={text(14, report.error ? "gold" : "muted")}>{report.error ?? "Gathering observed work for the overview…"}</Text>}
        {report.highlights.map((line) => <View style={{ flexDirection: "row", gap: pt(8) }}>
          <Text accessible={false} style={text(14, "accent")}>•</Text><Text selectable={true} style={{ ...text(14), flex: 1 }}>{line}</Text>
        </View>)}
        {report.followups.length > 0 && <Text role="header" style={{ ...text(12, "muted"), fontWeight: "600" }}>Suggested follow-ups</Text>}
        {report.followups.map((line) => <View style={{ flexDirection: "row", gap: pt(8) }}>
          <Symbol name="arrow.turn.down.right" color={palette().muted} size={pt(15)} /><Text selectable={true} style={{ ...text(14, "muted"), flex: 1 }}>{line}</Text>
        </View>)}
        {report.summary && report.error && <Text style={text(12, "gold")}>{report.error}</Text>}
        <Text style={text(11, "muted")}>{`AI summary of observed activity${report.generatedAt ? ` · ${new Date(report.generatedAt * 1000).toLocaleString()}` : ""}`}</Text>
      </View>
    </Section>)}
    {!props.dashboard.available && props.dashboard.unavailableReason && <Note message={props.dashboard.unavailableReason} />}
    {SECTION_ORDER.filter(includes).map((section) => {
      const rows = () => props.dashboard.rows.filter((row) => row.section === section);
      return (rows().length > 0 || (props.dashboard.more?.[section] ?? 0) > 0) && <Section title={SECTION_LABELS[section]}>
        {rows().slice(0, localLimit(section)).map((row, index) => <>
          {index > 0 && <Divider />}
          <Cell row={row} />
        </>)}
        {rows().length > localLimit(section) && <Action label={`Show ${rows().length - localLimit(section)} more`}
          action={() => expanded.set([...expanded(), section])} />}
        {(props.dashboard.more?.[section] ?? 0) > 0 && <Text style={{ ...text(12, "muted"), padding: pt(14) }}>{`${props.dashboard.more![section]} more on the desktops`}</Text>}
      </Section>;
    })}
    {props.dashboard.available && !anyShown() && <Note message={empty()} />}
  </>;
}
