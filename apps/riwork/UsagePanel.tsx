import { Scroll, Text, View, cleanup, effect, keymap, signal, type JSX } from "nsh";
import { Button } from "nsh/base";
import {
  Accounts, Sessions, Usage, type AccountsDto, type CodexUsageDto, type ProviderUsageDto,
  type ShellSessionDto, type UsageDto, type UsageWindowDto, type GrokTokenCountsDto,
} from "riwork/native";
import { space, type InterfaceTheme } from "./appearance";
import { Icon } from "./Icon";

function resetSummary(deadline: number | null | undefined, now: number): string {
  if (deadline == null) return "Reset unavailable";
  const seconds = Math.max(0, deadline - now);
  if (seconds === 0) return "Awaiting reset update";
  if (seconds >= 86400) return `Resets in ${Math.floor(seconds / 86400)}d ${Math.floor(seconds % 86400 / 3600)}h`;
  if (seconds >= 3600) return `Resets in ${Math.floor(seconds / 3600)}h ${Math.floor(seconds % 3600 / 60)}m`;
  return `Resets in ${Math.ceil(seconds / 60)}m`;
}

/** Account usage is read by the process service; this panel only owns its visible subscriptions. */
export function UsagePanel(props: {
  theme: () => InterfaceTheme;
  projectId: () => string;
  visible: () => boolean;
}) {
  const usage = signal<UsageDto | null>(null), accounts = signal<AccountsDto | null>(null);
  const shells = signal<ShellSessionDto[]>([]), notice = signal("");
  const refreshing = signal(false);
  let alive = true;
  cleanup(() => { alive = false; });
  effect(() => {
    if (!props.visible()) return;
    const listeners = [
      Usage.subscribe((event) => usage.set(event.data), props.projectId()),
      Accounts.subscribe((event) => accounts.set(event.data)),
      Sessions.subscribe((event) => shells.set(event.data.sessions)),
    ];
    return () => listeners.forEach((listener) => listener.close());
  });
  async function refresh() {
    if (refreshing()) return;
    refreshing.set(true); notice.set("");
    try { await Usage.refresh(true); }
    catch (reason) { if (alive) notice.set(String(reason)); }
    finally { if (alive) refreshing.set(false); }
  }
  effect(() => { if (props.visible()) return keymap.bind("mod+r", () => void refresh()); });
  const theme = props.theme, palette = () => theme().palette;
  const text = (size = 11, color = palette().text) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color });
  const busy = () => refreshing() || usage()?.refreshing;
  const now = () => usage()?.now ?? Math.floor(Date.now() / 1000);
  const claude = () => usage()?.claude.filter((entry) => entry.projectId === props.projectId()) ?? [];
  const grok = () => shells().filter((shell) => shell.alive && shell.harness === "grok" && shell.projectId === props.projectId());
  const grokEntry = (shellId: string) => usage()?.grok.find((entry) => entry.shellId === shellId);
  const tokens = (value: number) => value >= 1_000_000 ? `${(value / 1_000_000).toFixed(2)}M` : value >= 1000 ? `${(value / 1000).toFixed(1)}k` : String(value);
  const tokenBreakdown = (value: GrokTokenCountsDto) => `in ${tokens(value.input)} · out ${tokens(value.output)} · cached ${tokens(value.cachedRead)} · cache writes ${tokens(value.cacheCreation)} · reasoning ${tokens(value.reasoning)}`;
  const grokTotal = () => {
    const unique = new Map<string, NonNullable<ReturnType<typeof grokEntry>>["usage"]>();
    for (const shell of grok()) { const report = grokEntry(shell.id)?.usage; if (report) unique.set(report.sessionId, report); }
    return { sessions: unique.size, tokens: [...unique.values()].reduce((sum, report) => sum + (report?.tokens.total ?? 0), 0), cost: [...unique.values()].reduce((sum, report) => sum + (report?.costUsd ?? 0), 0) };
  };
  const codex = (): { home: string; label: string; entry?: CodexUsageDto; unavailable?: string }[] => {
    const profiles = new Map<string, { home: string; label: string; entry?: CodexUsageDto; unavailable?: string }>();
    for (const account of accounts()?.accounts ?? []) profiles.set(account.homePath, {
      home: account.homePath, label: account.label,
      ...(account.available ? {} : { unavailable: account.unavailableReason ?? "Account unavailable" }),
    });
    for (const shell of shells()) if (shell.harness === "codex" && shell.codexHome && !profiles.has(shell.codexHome)) profiles.set(shell.codexHome, {
      home: shell.codexHome, label: shell.codexAccountLabel ?? "Session account",
    });
    for (const entry of usage()?.codex ?? []) {
      const profile = profiles.get(entry.home);
      profiles.set(entry.home, { home: entry.home, label: profile?.label ?? entry.label, entry, unavailable: profile?.unavailable });
    }
    return [...profiles.values()];
  };
  const bodyStyle = () => ({ padding: space(12, theme().scale), gap: space(4, theme().scale), borderRadius: space(12, theme().scale), backgroundColor: palette().bg });

  function Card(props: { id: string; title: string; children?: JSX.Element }) {
    return <View testID={props.id} style={bodyStyle()}>
      <Text style={{ ...text(11), fontWeight: "600" }}>{props.title}</Text>
      {props.children}
    </View>;
  }
  function WindowGauge(props: { window: UsageWindowDto }) {
    const remaining = () => Math.max(0, Math.min(100, 100 - props.window.usedPercent));
    return <View style={{ gap: 5, paddingTop: space(8, theme().scale) }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
        <Text style={{ ...text(11), flex: 1 }}>{`${props.window.label} · ${Math.round(remaining())}% left`}</Text>
        <Text style={text(9, palette().muted)}>{resetSummary(props.window.resetsAt, now())}</Text>
      </View>
      <View role="progressbar" accessibilityLabel={`${props.window.label}: ${Math.round(remaining())}% remaining`} style={{ height: space(5, theme().scale), borderRadius: 5, backgroundColor: palette().panelActive, overflow: "hidden" }}>
        <View style={{ height: "100%", width: `${remaining()}%`, borderRadius: 5, backgroundColor: remaining() < 15 ? palette().gold : palette().cyan }} />
      </View>
    </View>;
  }
  function Provider(props: { value: ProviderUsageDto; stale?: boolean }) {
    const age = () => Math.max(0, now() - props.value.updatedAtUnix);
    const stale = () => props.stale || age() > 900;
    return <>
      {props.value.accountLabel && <Text selectable={true} style={text(11, palette().muted)}>{props.value.accountLabel}</Text>}
      <Text style={text(9, stale() ? palette().gold : palette().muted)}>{`${stale() ? "Stale · " : ""}Updated ${Math.floor(age() / 60)}m ago`}</Text>
      {props.value.windows.map((window) => <WindowGauge window={window} />)}
      {props.value.windows.length === 0 && <Text style={{ ...text(11, palette().muted), paddingTop: 8 }}>Quota windows unavailable</Text>}
      {props.value.contextUsedPercent != null && <Text style={{ ...text(11, palette().muted), paddingTop: 8 }}>{`Context · ${Math.round(props.value.contextUsedPercent)}% used`}</Text>}
      {props.value.sessionCostUsd != null && <Text style={text(11, palette().muted)}>{`Estimated session cost · $${props.value.sessionCostUsd.toFixed(2)}`}</Text>}
    </>;
  }
  return <View testID="usage-panel" style={{ flex: 1, minHeight: 0, backgroundColor: palette().panel }}>
    <View style={{ height: space(46, theme().scale), paddingLeft: space(12, theme().scale), paddingRight: space(8, theme().scale), flexDirection: "row", alignItems: "center", gap: 8 }}>
      <View style={{ flex: 1, minWidth: 0, gap: 1 }}>
        <Text style={{ ...text(13), fontWeight: "600" }}>Usage</Text>
        <Text style={text(10, palette().muted)}>{busy() ? "Refreshing…" : "Account usage"}</Text>
      </View>
      <Button testID="refresh-account-usage" accessibilityLabel="Refresh usage" disabled={!!busy()} onPress={() => void refresh()}
        tooltip={<Text style={{ ...text(11), padding: 8, backgroundColor: palette().panelActive }}>Refresh usage · ⌘R</Text>}
        style={{ width: space(26, theme().scale), height: space(26, theme().scale), borderRadius: 13, alignItems: "center", justifyContent: "center" }} hoverStyle={{ backgroundColor: palette().panelActive }} focusStyle={{ backgroundColor: palette().divider }}>
        <Icon name="refresh" size={14 * theme().scale} color={palette().muted} />
      </Button>
    </View>
    {notice() !== "" && <Text role="alert" style={{ ...text(11, palette().gold), paddingHorizontal: 12, paddingBottom: 8 }}>{notice()}</Text>}
    <Scroll testID="usage-panel-scroll" style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, theme().scale), paddingBottom: 6, gap: 6 }}>
      {!usage() && <Card id="usage-loading" title="Account usage"><Text style={text(11, palette().muted)}>Reading account usage…</Text></Card>}
      {codex().map((profile) => <Card id={`usage-codex:${profile.home}`} title={`Codex · ${profile.label}`}>
        {profile.entry?.usage ? <Provider value={profile.entry.usage} stale={profile.entry.stale} /> : <Text style={{ ...text(11, palette().muted), paddingTop: 6 }}>{profile.unavailable ?? (profile.entry?.pending || !usage() ? "Reading account usage…" : profile.entry?.error ?? "Account usage is unavailable")}</Text>}
        {profile.entry?.usage && profile.entry.error && <Text style={{ ...text(10, palette().gold), paddingTop: 6 }}>{`Last refresh: ${profile.entry.error}`}</Text>}
      </Card>)}
      {usage() && codex().length === 0 && <Card id="usage-codex-unavailable" title="Codex"><Text style={text(11, palette().muted)}>Account usage is unavailable</Text></Card>}
      {claude().map((entry) => <Card id={`usage-claude:${entry.shellId}`} title={`Claude · ${entry.shellId.slice(0, 8)}`}>
        {entry.usage ? <Provider value={entry.usage} stale={entry.stale} /> : <Text style={{ ...text(11, palette().muted), paddingTop: 6 }}>{entry.error ?? "Waiting for Claude usage after its first response"}</Text>}
        {entry.usage && entry.error && <Text style={text(10, palette().gold)}>{`Last refresh: ${entry.error}`}</Text>}
      </Card>)}
      {usage() && claude().length === 0 && <Card id="usage-claude-empty" title="Claude"><Text style={text(11, palette().muted)}>Open a Claude session to see its usage here</Text></Card>}
      {grok().map((shell) => <Card id={`usage-grok:${shell.id}`} title={grokEntry(shell.id)?.label ?? `Grok · ${shell.id.slice(0, 8)}`}>
        {grokEntry(shell.id)?.usage ? <>
          <Text style={text(11)}>{grokEntry(shell.id)!.usage!.primaryModel ?? "Model unavailable"}</Text>
          <Text style={text(11, palette().muted)}>{`${tokens(grokEntry(shell.id)!.usage!.tokens.total)} tokens · ${grokEntry(shell.id)!.usage!.turns} turns · ${grokEntry(shell.id)!.usage!.modelCalls} model calls`}</Text>
          <Text style={text(10, palette().muted)}>{tokenBreakdown(grokEntry(shell.id)!.usage!.tokens)}</Text>
          <Text style={text(11, palette().muted)}>{grokEntry(shell.id)!.usage!.costUsd != null ? `Session cost · $${grokEntry(shell.id)!.usage!.costUsd!.toFixed(4)}` : "Session cost unavailable"}</Text>
          <Text style={text(9, grokEntry(shell.id)?.stale ? palette().gold : palette().muted)}>{`${grokEntry(shell.id)?.stale ? "Stale · " : ""}Read ${Math.max(0, Math.floor((now() - (grokEntry(shell.id)?.fetchedAtUnix ?? now())) / 60))}m ago`}</Text>
          {grokEntry(shell.id)!.usage!.models.map((model) => <View style={{ paddingTop: 6, gap: 2 }}>
            <Text style={text(10)}>{`${model.model} · ${tokens(model.tokens.total)} tokens · ${model.modelCalls} calls${model.costUsd != null ? ` · $${model.costUsd.toFixed(4)}` : ""}`}</Text>
            <Text style={text(9, palette().muted)}>{tokenBreakdown(model.tokens)}</Text>
          </View>)}
        </> : <Text style={text(11, palette().muted)}>{grokEntry(shell.id)?.pending ? "Reading session usage…" : grokEntry(shell.id)?.error ?? "Waiting for session usage…"}</Text>}
        {grokEntry(shell.id)?.usage && grokEntry(shell.id)?.error && <Text style={text(10, palette().gold)}>{`Last refresh: ${grokEntry(shell.id)!.error}`}</Text>}
      </Card>)}
      {grokTotal().sessions > 0 && <Card id="usage-grok-total" title="Grok · Combined total">
        <Text style={text(11)}>{`$${grokTotal().cost.toFixed(4)} · ${tokens(grokTotal().tokens)} tokens · ${grokTotal().sessions} ${grokTotal().sessions === 1 ? "session" : "sessions"}`}</Text>
        <Text style={text(9, palette().muted)}>Each session is counted once, including history from resumes.</Text>
      </Card>}
      {grok().length === 0 && <Card id="usage-grok-empty" title="Grok"><Text style={text(11, palette().muted)}>No live Grok sessions</Text></Card>}
    </Scroll>
    <Text style={{ ...text(9, palette().muted), paddingHorizontal: space(12, theme().scale), paddingVertical: 8 }}>Quota is shared by all sessions on the same account. Missing windows are unavailable. Claude subscription quota requires a supported Pro/Max account. Grok’s account allowance is only available in its own /usage screen.</Text>
  </View>;
}
