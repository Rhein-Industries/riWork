import { Scroll, Text, View, cleanup, effect, keymap, signal, type JSX } from "nsh";
import { Button } from "nsh/base";
import {
  Accounts, Sessions, Store, Usage, type AccountsDto, type CodexUsageDto, type ProviderUsageDto,
  type ShellSessionDto, type UsageDto, type UsageWindowDto, type GrokTokenCountsDto, type GrokSessionUsageDto, type GrokUsageDto, type StoreState,
} from "riwork/native";
import { space, type InterfaceTheme } from "./appearance";
import { Icon } from "./Icon";
import { EMPTY_STORE, shellLabel } from "./panel_model";
import { PANEL } from "./panel_layout";
import { quotaReadout, quotaWindows, stale as quotaStale } from "./usage";

function resetSummary(deadline: number | null | undefined, now: number): string {
  if (deadline == null) return "Reset unavailable";
  const seconds = Math.max(0, deadline - now);
  if (seconds === 0) return "Awaiting reset update";
  if (seconds >= 86400) return `Resets in ${Math.floor(seconds / 86400)}d ${Math.floor(seconds % 86400 / 3600)}h`;
  if (seconds >= 3600) return `Resets in ${Math.floor(seconds / 3600)}h ${Math.floor(seconds % 3600 / 60)}m`;
  return `Resets in ${Math.ceil(seconds / 60)}m`;
}

/** How long ago `at` was, to the minute: "just now", "4m ago", "2h ago", "3d ago". */
function ago(at: number, now: number): string {
  const seconds = Math.max(0, now - at);
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

/** Dollars to the cent; a cost under half a cent is not shown as free. */
function money(value: number): string {
  return value > 0 && value < 0.005 ? "<$0.01" : `$${value.toFixed(2)}`;
}

/** The native reads the panel subscribes to; fixtures supply in-memory ones. */
export interface UsagePanelApi {
  usage: Pick<typeof Usage, "subscribe" | "refresh">;
  accounts: Pick<typeof Accounts, "subscribe">;
  sessions: Pick<typeof Sessions, "subscribe">;
  store?: Pick<typeof Store, "subscribe">;
}
const NATIVE_API: UsagePanelApi = { usage: Usage, accounts: Accounts, sessions: Sessions, store: Store };

/** Account usage is read by the process service; this panel only owns its visible subscriptions. */
export function UsagePanel(props: {
  theme: () => InterfaceTheme;
  projectId: () => string;
  visible: () => boolean;
  api?: UsagePanelApi;
}) {
  const api = props.api ?? NATIVE_API;
  const usage = signal<UsageDto | null>(null), accounts = signal<AccountsDto | null>(null);
  const store = signal<StoreState>(EMPTY_STORE);
  const shells = signal<ShellSessionDto[]>([]), notice = signal("");
  const refreshing = signal(false);
  // The service publishes only when figures change (its `now` can be 15 minutes old), so
  // "ago" and "Resets in" read this display clock: set on each snapshot and when shown.
  const clock = signal(Date.now());
  const tick = () => clock.set(Date.now());
  let alive = true;
  cleanup(() => { alive = false; });
  effect(() => {
    if (!props.visible()) return;
    tick();
    const listeners = [
      api.usage.subscribe((event) => { usage.set(event.data); tick(); }, props.projectId()),
      api.accounts.subscribe((event) => accounts.set(event.data)),
      api.sessions.subscribe((event) => shells.set(event.data.sessions)),
    ];
    if (api.store) listeners.push(api.store.subscribe((event) => store.set(event.data)));
    return () => listeners.forEach((listener) => listener.close());
  });
  // Not a data read (P1-POLL-03): one timer to the next whole minute, re-armed by the tick.
  effect(() => {
    if (!props.visible() || !usage()) return;
    clock();
    const timer = setTimeout(() => { if (alive) tick(); }, 60_000 - Date.now() % 60_000 + 50);
    return () => clearTimeout(timer);
  });
  async function refresh() {
    if (refreshing()) return;
    refreshing.set(true); notice.set("");
    try { await api.usage.refresh(true); }
    catch (reason) { if (alive) notice.set(String(reason)); }
    finally { if (alive) refreshing.set(false); }
  }
  effect(() => { if (props.visible()) return keymap.bind("mod+r", () => void refresh()); });
  const theme = props.theme, palette = () => theme().palette;
  const text = (size = 11, color = palette().text) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color });
  const busy = () => refreshing() || usage()?.refreshing;
  const now = () => Math.max(Math.floor(clock() / 1000), usage()?.now ?? 0);
  const claude = () => usage()?.claude.filter((entry) => entry.projectId === props.projectId()) ?? [];
  const grok = () => shells().filter((shell) => shell.alive && shell.harness === "grok" && shell.projectId === props.projectId());
  const grokEntry = (shellId: string) => usage()?.grok.find((entry) => entry.shellId === shellId);
  const tokens = (value: number) => value >= 1_000_000 ? `${(value / 1_000_000).toFixed(2)}M` : value >= 1000 ? `${(value / 1000).toFixed(1)}k` : String(value);
  const tokenBreakdown = (value: GrokTokenCountsDto) => `in ${tokens(value.input)} · out ${tokens(value.output)} · cached ${tokens(value.cachedRead)} · cache writes ${tokens(value.cacheCreation)} · reasoning ${tokens(value.reasoning)}`;
  const grokTotal = () => {
    const unique = new Map<string, GrokSessionUsageDto>();
    for (const shell of grok()) { const report = grokEntry(shell.id)?.usage; if (report) unique.set(report.sessionId, report); }
    return { sessions: unique.size, tokens: [...unique.values()].reduce((sum, report) => sum + report.tokens.total, 0), cost: [...unique.values()].reduce((sum, report) => sum + (report.costUsd ?? 0), 0) };
  };
  const codex = (): { home: string; label: string; entry?: CodexUsageDto; unavailable?: string }[] => {
    const profiles = new Map<string, { home: string; label: string; entry?: CodexUsageDto; unavailable?: string }>();
    for (const account of accounts()?.accounts ?? []) profiles.set(account.homePath, {
      home: account.homePath, label: account.label,
      ...(account.available ? {} : { unavailable: account.unavailableReason ?? "Account unavailable" }),
    });
    // The service includes eligible live-session homes and excludes forgotten accounts.
    // Shell bindings retain their original identity, so they must not recreate account cards.
    for (const entry of usage()?.codex ?? []) {
      const profile = profiles.get(entry.home);
      profiles.set(entry.home, { home: entry.home, label: profile?.label ?? entry.label, entry, unavailable: profile?.unavailable });
    }
    return [...profiles.values()];
  };
  const stale = (value: ProviderUsageDto, flagged: boolean, error?: string | null) => flagged || !!error || quotaStale(value, now());
  const sessionBase = (id: string, provider: string) => {
    const shell = shells().find((shell) => shell.id === id);
    if (!shell) return `${provider} · ${id.slice(0, 8)}`;
    const project = store().projects.find((project) => project.id === shell.projectId);
    const label = shellLabel(shell, store());
    const context = label && label !== shell.harness && label !== "Shell" ? label : shell.cwd.split("/").filter(Boolean).pop();
    return `${provider} · ${project?.name ? `${project.name} · ` : ""}${context || id.slice(0, 8)}`;
  };
  const sessionTitle = (id: string, provider: string) => {
    const base = sessionBase(id, provider);
    const duplicate = shells().some((shell) => shell.id !== id && shell.projectId === props.projectId() && shell.harness === provider.toLowerCase() && sessionBase(shell.id, provider) === base);
    return duplicate ? `${base} · ${id.slice(0, 8)}` : base;
  };
  const bodyStyle = () => ({ padding: space(12, theme().scale), gap: space(4, theme().scale), borderRadius: space(12, theme().scale), backgroundColor: palette().bg });
  const inset = () => space(6, theme().scale);

  function Section(props: { title: string; note?: string; children?: JSX.Element }) {
    return <View style={{ gap: 6, paddingTop: space(6, theme().scale) }}>
      <Text role="header" style={{ ...text(10, palette().muted), fontWeight: "600", paddingHorizontal: inset() }}>{props.title}</Text>
      {props.children}
      {props.note && <Text style={{ ...text(10, palette().muted), paddingHorizontal: inset() }}>{props.note}</Text>}
    </View>;
  }
  function Empty(props: { id: string; children: string }) {
    return <Text testID={props.id} style={{ ...text(11, palette().muted), paddingHorizontal: inset() }}>{props.children}</Text>;
  }
  function Card(props: { id: string; title: string; aside?: JSX.Element; children?: JSX.Element }) {
    return <View testID={props.id} style={bodyStyle()}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
        <Text style={{ ...text(11), fontWeight: "600", flex: 1, minWidth: 0 }}>{props.title}</Text>
        {props.aside}
      </View>
      {props.children}
    </View>;
  }
  function Freshness(props: { at: number; stale: boolean }) {
    return <Text style={text(10, props.stale ? palette().gold : palette().muted)}>{`${props.stale ? "Stale · " : ""}${ago(props.at, now())}`}</Text>;
  }
  function WindowGauge(props: { window: UsageWindowDto }) {
    const remaining = () => Math.max(0, Math.min(100, 100 - props.window.usedPercent));
    return <View style={{ gap: 5, paddingTop: space(8, theme().scale) }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
        <Text style={{ ...text(12), fontWeight: "600", flexGrow: 1 }}>{`${props.window.label} · ${Math.round(remaining())}% left`}</Text>
        <Text style={{ ...text(11, palette().muted), flexShrink: 0, marginLeft: "auto", textAlign: "right" }}>{resetSummary(props.window.resetsAt, now())}</Text>
      </View>
      <View role="progressbar" accessibilityLabel={`${props.window.label}: ${Math.round(remaining())}% remaining`} style={{ height: space(5, theme().scale), borderRadius: 5, backgroundColor: palette().panelActive, overflow: "hidden" }}>
        <View style={{ height: "100%", width: `${remaining()}%`, borderRadius: 5, backgroundColor: remaining() < 15 ? palette().gold : palette().cyan }} />
      </View>
    </View>;
  }
  function Provider(props: { value: ProviderUsageDto; state?: Parameters<typeof quotaReadout>[2] }) {
    const readout = () => quotaReadout(props.value, now(), props.state);
    const session = () => [
      props.value.contextUsedPercent != null ? `Context ${Math.round(props.value.contextUsedPercent)}% used` : "",
      props.value.sessionCostUsd != null ? `Estimated session cost ${money(props.value.sessionCostUsd)}` : "",
    ].filter(Boolean).join(" · ");
    return <>
      {props.value.accountLabel && <Text selectable={true} style={text(11, palette().muted)}>{props.value.accountLabel}</Text>}
      {quotaWindows(props.value).map((window) => <WindowGauge window={window} />)}
      {readout().windows?.length === 0 && <Text style={{ ...text(11, palette().muted), paddingTop: 8 }}>Quota windows unavailable</Text>}
      {readout().note && <Text style={{ ...text(10, readout().freshness === "stale" ? palette().gold : palette().muted), paddingTop: 6 }}>{readout().note}</Text>}
      {session() !== "" && <Text style={{ ...text(10, palette().muted), paddingTop: 8 }}>{session()}</Text>}
    </>;
  }
  function GrokFigures(props: { report: GrokSessionUsageDto }) {
    const report = () => props.report, details = signal(false);
    const metric = (label: string, value: string) => <View style={{ minWidth: 100, flexGrow: 1, gap: 3, padding: 8, borderRadius: 6, backgroundColor: palette().panel }}><Text style={text(10, palette().muted)}>{label}</Text><Text style={{ ...text(14), fontWeight: "600" }}>{value}</Text></View>;
    return <>
      <View testID="grok-metrics" style={{ flexDirection: "row", flexWrap: "wrap", gap: 8, paddingTop: 8 }}>
        {metric("Session cost", report().costUsd == null ? "Unavailable" : money(report().costUsd!))}
        {metric("Total tokens", tokens(report().tokens.total))}
        {metric("Turns", String(report().turns))}{metric("Model calls", String(report().modelCalls))}
      </View>
      <Text style={text(11, palette().muted)}>{report().primaryModel ?? "Model unavailable"}</Text>
      <Button testID="grok-usage-details" accessibilityLabel="Token and model details" onPress={() => details.set(!details())} style={{ alignSelf: "flex-start", paddingVertical: 8 }}>
        <Text style={text(11)}>{details() ? "Hide token and model details" : "Token and model details"}</Text>
      </Button>
      {details() && <View style={{ gap: 8 }}>
        <Text style={text(11, palette().muted)}>{tokenBreakdown(report().tokens)}</Text>
        {report().models.map((model) => <View style={{ padding: 8, gap: 4, borderRadius: 6, backgroundColor: palette().panel }}>
          <Text style={{ ...text(12), fontWeight: "600" }}>{model.model}</Text>
          <Text style={text(11)}>{`${tokens(model.tokens.total)} tokens · ${model.modelCalls} calls${model.costUsd != null ? ` · ${money(model.costUsd)}` : ""}`}</Text>
          <Text style={text(11, palette().muted)}>{tokenBreakdown(model.tokens)}</Text>
        </View>)}
      </View>}
    </>;
  }
  function GrokSession(props: { id: string; entry?: GrokUsageDto }) {
    const entry = () => props.entry;
    return <Card id={`usage-grok:${props.id}`} title={sessionTitle(props.id, "Grok")}
      aside={entry()?.usage ? <Freshness at={entry()!.fetchedAtUnix} stale={entry()!.stale} /> : undefined}>
      {entry()?.usage ? <GrokFigures report={entry()!.usage!} />
        : <Text style={{ ...text(11, palette().muted), paddingTop: 6 }}>{entry()?.pending ? "Reading session usage…" : entry()?.error ?? "Waiting for session usage…"}</Text>}
      {entry()?.usage && entry()?.error && <Text style={{ ...text(10, palette().gold), paddingTop: 6 }}>{`Last refresh: ${entry()!.error}`}</Text>}
    </Card>;
  }
  return <View testID="usage-panel" style={{ flex: 1, minHeight: 0, backgroundColor: palette().panel }}>
    <View style={{ minHeight: space(PANEL.header, theme().scale), paddingHorizontal: space(PANEL.inset, theme().scale), flexDirection: "row", alignItems: "center", gap: 8 }}>
      <View style={{ flex: 1, minWidth: 0, gap: 1 }}>
        <Text style={{ ...text(13), fontWeight: "600" }}>Usage</Text>
        <Text style={text(10, palette().muted)}>{busy() ? "Refreshing…" : "Account usage"}</Text>
      </View>
      <Button testID="refresh-account-usage" accessibilityLabel="Refresh usage" disabled={!!busy()} onPress={() => void refresh()}
        tooltip={<Text style={{ ...text(11), padding: 8, backgroundColor: palette().panelActive }}>Refresh usage · ⌘R</Text>}
        style={{ width: space(PANEL.control, theme().scale), height: space(PANEL.control, theme().scale), borderRadius: 13, alignItems: "center", justifyContent: "center" }} hoverStyle={{ backgroundColor: palette().panelActive }} focusStyle={{ backgroundColor: palette().divider }}>
        <Icon name="refresh" size={PANEL.icon * theme().scale} color={palette().muted} />
      </Button>
    </View>
    {notice() !== "" && <Text role="alert" style={{ ...text(11, palette().gold), paddingHorizontal: 12, paddingBottom: 8 }}>{notice()}</Text>}
    <Scroll testID="usage-panel-scroll" style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, theme().scale), paddingBottom: space(12, theme().scale), gap: 6 }}>
      {!usage() ? <Card id="usage-loading" title="Account usage"><Text style={text(11, palette().muted)}>Reading account usage…</Text></Card> : <>
        <Section title="Codex accounts" note={codex().length > 0 ? "Quota is shared by every session on the same account." : undefined}>
          {codex().map((profile) => <Card id={`usage-codex:${profile.home}`} title={profile.label}
            aside={profile.entry?.usage ? <Freshness at={profile.entry.usage.updatedAtUnix} stale={stale(profile.entry.usage, profile.entry.stale, profile.entry.error)} /> : undefined}>
            {profile.entry?.usage ? <Provider value={profile.entry.usage} state={profile.entry} />
              : <Text style={{ ...text(11, profile.unavailable ? palette().gold : palette().muted), paddingTop: 6 }}>{profile.unavailable ?? (profile.entry?.pending ? "Reading account usage…" : profile.entry?.error ?? "Account usage is unavailable")}</Text>}
          </Card>)}
          {codex().length === 0 && <Empty id="usage-codex-unavailable">Account usage is unavailable</Empty>}
        </Section>
        <Section title="Claude sessions" note={claude().length > 0 ? "Subscription quota needs a Pro or Max account." : undefined}>
          {claude().map((entry) => <Card id={`usage-claude:${entry.shellId}`} title={sessionTitle(entry.shellId, "Claude")}
            aside={entry.usage ? <Freshness at={entry.usage.updatedAtUnix} stale={stale(entry.usage, entry.stale, entry.error)} /> : undefined}>
            {entry.usage ? <Provider value={entry.usage} state={entry} /> : <Text style={{ ...text(11, palette().muted), paddingTop: 6 }}>{entry.error ?? "Waiting for Claude usage after its first response"}</Text>}
          </Card>)}
          {claude().length === 0 && <Empty id="usage-claude-empty">Open a Claude session to see its usage here</Empty>}
        </Section>
        <Section title="Grok sessions" note={grok().length > 0 ? "Grok shows its account allowance only in its own /usage screen." : undefined}>
          {grok().map((shell) => <GrokSession id={shell.id} entry={grokEntry(shell.id)} />)}
          {grok().length > 1 && grokTotal().sessions > 0 && <Card id="usage-grok-total" title="Combined total">
            <Text style={{ ...text(12), paddingTop: 4 }}>{`${money(grokTotal().cost)} · ${tokens(grokTotal().tokens)} tokens · ${grokTotal().sessions} ${grokTotal().sessions === 1 ? "session" : "sessions"}`}</Text>
            <Text style={text(10, palette().muted)}>Each session is counted once, including history from resumes.</Text>
          </Card>}
          {grok().length === 0 && <Empty id="usage-grok-empty">No live Grok sessions</Empty>}
        </Section>
      </>}
    </Scroll>
  </View>;
}
