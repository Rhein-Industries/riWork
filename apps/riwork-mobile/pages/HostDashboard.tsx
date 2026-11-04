/** One host's dashboard: the desktop's own DashboardView over the relay, read only while this page is on
 * screen. A row opens the project's page at its terminal or chat, as the Mac's global window routes it
 * (`routeTarget`); the global orchestrator is a native Swift screen. On the iPad it is the window's main
 * area beside the sidebar, under the top row's Dashboard (as the Mac's dashboard tab), with no title bar;
 * a desktop that is not connected says so there, with Connect. */
import { Scroll, Text, View, computed, effect, keymap, signal } from "nsh";
import { appChord } from "../../riwork/app_chords";
import { Icon } from "../../riwork/Icon";
import type { DashboardDto } from "../../riwork/client/types";
import { DashboardView } from "../../riwork/dashboard/Dashboard";
import { routeTarget, type Filter, type Sort, type Target, type Wire } from "../../riwork/dashboard/model";
import type { InterfaceTheme } from "../../riwork/theme";
import type { MobileApp } from "../app_state";
import { Leading, TopRow, type PadChrome } from "../shell/chrome";
import { Symbol } from "../Symbol";
import type { Kit, Layout } from "../ui";

/** The desktop dashboard's sizes at touch density: its 11-point rows become 15. */
const TOUCH_SCALE = 15 / 11;

export function HostDashboardPage(props: {
  app: MobileApp; ui: Kit; layout: Layout; host: string; onTop: () => boolean;
  /** The iPad's window around the page: its sidebar. */
  pad?: PadChrome;
}) {
  const { app, ui } = props;
  const { Action, Section, Note, Failure, palette, pt, target, text } = ui;
  const pad = props.layout === "pad" ? props.pad ?? null : null;
  const hostId = props.host;
  const services = app.services(hostId);
  const dto = signal<DashboardDto | null>(null);
  const filter = signal<Filter>("active"), sort = signal<Sort>("priority"), query = signal("");
  const notice = signal("");
  const bottom = ui.bottomInset();
  const info = () => app.hosts().find((entry) => entry.id === hostId);
  const connected = () => info()?.link.state === "connected";
  // A Mac whose Somtus predates `dashboard.get` is never asked; its projects still open.
  const unsupported = computed(() => connected() && !info()!.features.includes("dashboard"));
  const onScreen = computed(() => props.onTop() && !app.blocked());
  effect(() => {
    if (!onScreen() || unsupported()) return;
    const subscription = services.dashboard.subscribe((next) => dto.set(next));
    return () => subscription.close();
  });
  /** Everything this page shows, read again: the dashboard here, and in Swift the lists its rows open with. */
  function reload() {
    if (!unsupported()) void services.dashboard.refresh();
    void app.act({ type: "refresh" });
  }
  effect(() => {
    if (!onScreen()) return;
    app.refresher.current = reload;
    return () => { if (app.refresher.current === reload) app.refresher.current = null; };
  });
  let search: { focus: () => void } | undefined;
  // On the iPad ⌘F searches the projects while the sidebar is up (its field says so), as on the Mac.
  effect(() => { if (onScreen() && !unsupported() && !(pad && (pad.docked() || pad.overlay()))) return keymap.bind("mod+f", () => search?.focus()); });
  // ⌘B shows or hides the iPad's sidebar, from the keymap and from Swift's chords.
  effect(() => {
    if (!pad || !onScreen() || pad.menuOpen()) return;
    const fromSwift = (id: string) => { if (appChord(id)?.type === "sidebar") pad.toggle(); };
    app.chords.current = fromSwift;
    const unbind = keymap.bind("mod+b", pad.toggle, { when: "always" });
    return () => { unbind(); if (app.chords.current === fromSwift) app.chords.current = null; };
  });
  const global = () => app.overview().global?.host === hostId ? app.overview().global : undefined;
  const theme = (): InterfaceTheme => { const current = app.theme().theme; return { ...current, scale: current.scale * TOUCH_SCALE }; };
  const projectName = (id: string) => (dto()?.wire as Wire | null | undefined)?.projects?.find((project) => project.id === id)?.name ?? "Project";
  function openProject(projectId: string, tab?: string) {
    notice.set("");
    app.openProject(hostId, { id: projectId, name: projectName(projectId) }, { tab });
  }
  /** Where a row leads, as the Mac's global window decides it (`routeTarget`). */
  function open(target: Target) {
    const wire = dto()?.wire as Wire | null | undefined;
    if (!wire) return;
    // The global orchestrator's own chat, even where its row does not say so.
    if (target.type === "chat" && wire.global.kind === "chat" && wire.global.chat_id === target.id) { void app.present({ name: "global", host: hostId }); return; }
    const route = routeTarget(target, wire, shellProjects(wire));
    if (route.kind === "notice") notice.set(route.text);
    else if (route.kind === "chat") void app.present({ name: "global", host: hostId });
    else {
      const step = route.step as { kind?: string; shellId?: string; chatId?: string } | undefined;
      openProject(route.projectId, step?.kind === "showShell" ? `shell:${step.shellId}` : step?.kind === "chat" ? `chat:${step.chatId}` : undefined);
    }
  }
  function Connection() {
    const link = () => info()?.link;
    return <View style={{ padding: pt(14), backgroundColor: palette().panel, borderRadius: pt(12), gap: pt(8) }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: pt(9) }}>
        <Symbol name={connected() ? "checkmark.circle" : link()?.state === "connecting" ? "arrow.triangle.2.circlepath" : "network"}
          color={connected() ? palette().muted : link()?.state === "failed" ? palette().danger : palette().gold} size={pt(18)} />
        <Text style={{ ...text(14), flex: 1 }}>{link()?.label ?? "Not connected"}</Text>
        {connected() && <Action compact={true} label="Disconnect" disabled={app.blocked()}
          action={() => void app.device.hosts.disconnect(hostId).catch((error) => app.failure.set(String(error)))} />}
        {!connected() && link()?.state !== "connecting" && <Action compact={true} label="Connect" disabled={app.blocked()} action={() => app.connect(hostId)} />}
      </View>
      {link()?.error && <Text style={text(13, "error")}>{link()!.error!}</Text>}
    </View>;
  }
  function Notice() {
    return <>{notice() && <Note message={notice()} error={true}>
      <Action label="Dismiss" compact={true} action={() => notice.set("")} />
    </Note>}</>;
  }
  /** The dashboard, or on a Mac without one why, with its global orchestrator. */
  function Body() {
    return <>{unsupported() ? <Scroll style={{ flex: 1, backgroundColor: palette().bg }} contentStyle={{ padding: pt(16), gap: pt(12) }}
      refreshing={app.overview().loading} onRefresh={reload}>
      <Note title="Dashboard unavailable" message="The Somtus on your Mac does not send its dashboard yet. Update Somtus on the Mac to see what needs you, what is working, tasks and automations here. Projects and the global orchestrator still open." />
      {global() && <Section title="Global orchestrator">
        <View style={{ padding: pt(14), gap: pt(8) }}>
          <Text style={text(14, "muted")}>{global()!.summary}</Text>
          {/* Start only when the button said so: an explicit tap, never a side effect of Open. */}
          <Action label={global()!.actionTitle} symbol={pad ? undefined : "globe"} icon={pad ? "globe" : undefined} prominent={true}
            disabled={app.blocked() || !connected() || global()!.action === "unavailable"}
            action={() => void app.present({ name: "global", host: hostId, start: global()?.action === "start" })} />
        </View>
      </Section>}
      {props.layout === "phone" && <Action label="All projects" symbol="folder" prominent={true} disabled={app.blocked()} action={() => app.openProjects(hostId)} />}
    </Scroll> : <DashboardView dto={dto} theme={theme} filter={filter} sort={sort} query={query} touch={true}
      onFilter={(next) => filter.set(next)} onSort={(next) => sort.set(next)} onSearch={(node) => { search = node; }}
      onOpen={open} onPanel={(projectId) => openProject(projectId)}
      onGlobalChat={() => void app.present({ name: "global", host: hostId, start: true })}
      onRefresh={reload}
      plan={() => null} starting={() => false} decisions={services.dashboard.decisions} />}</>;
  }

  if (pad) {
    const link = () => app.link(hostId);
    const notes = () => !!(app.failure() || notice() || !connected());
    return <View testID={`page:dashboard:${hostId}`} style={{ flex: 1, minWidth: 0, minHeight: 0, backgroundColor: theme().palette.panel }}>
      <TopRow ui={ui}>
        <Leading app={app} ui={ui} chrome={pad} host={hostId} />
        <View role="header" accessibilityLabel="Dashboard" style={{ height: ui.bar() - 1, flexDirection: "row", alignItems: "center", gap: pt(6), paddingHorizontal: pt(12),
          backgroundColor: palette().bg, borderRightWidth: 1, borderColor: palette().divider }}>
          <Icon name="globe" size={pt(15)} color={palette().text} />
          <Text numberOfLines={1} style={{ ...text(13), fontWeight: "500" }}>Dashboard</Text>
        </View>
      </TopRow>
      {notes() && <View style={{ paddingHorizontal: pt(16), paddingVertical: pt(8), gap: pt(8), backgroundColor: palette().bg }}>
        <Failure />
        {!connected() && <Note title={app.hostName(hostId)} message={link().error ? `${link().label} · ${link().error}` : link().label}>
          {link().state !== "connecting" && <Action label="Connect" compact={true} disabled={app.blocked()} action={() => app.connect(hostId)} />}
        </Note>}
        <Notice />
      </View>}
      <Body />
    </View>;
  }

  return <ui.Page layout={props.layout} title={app.hostName(hostId)} testID={`page:dashboard:${hostId}`} backTitle="Dashboard"
    right={<Action label="Projects" symbol="folder" compact={true} disabled={app.blocked()} action={() => app.openProjects(hostId)} />}>
    <View style={{ flex: 1, paddingBottom: bottom(), backgroundColor: theme().palette.panel }}>
      <View style={{ paddingHorizontal: pt(16), paddingVertical: pt(8), gap: pt(8), backgroundColor: palette().bg }}>
        <Failure />
        <Connection />
        <Notice />
      </View>
      <Body />
    </View>
  </ui.Page>;
}

/** Each shell a dashboard row names, with its project (`routeTarget` needs them). */
function shellProjects(wire: Wire): Record<string, string | null> {
  const shells: Record<string, string | null> = {};
  const sessions = [
    ...(wire.working ?? []), ...(wire.finished ?? []), ...(wire.recent_activity ?? []), ...(wire.decisions ?? []),
  ].map((row) => row.session).concat((wire.attention ?? []).flatMap((row) => row.session ? [row.session] : []));
  for (const session of sessions) {
    if (session.target.type === "shell" && session.target.id) shells[session.target.id] = session.project_id ?? shells[session.target.id] ?? null;
  }
  return shells;
}
