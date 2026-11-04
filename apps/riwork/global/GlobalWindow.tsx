import { Text, View, cleanup, effect, host, keymap, signal } from "nsh";
import { Window } from "nsh/desktop";
import { Dashboard, Sessions, Store, type GlobalWindowDto, type SettingsDto } from "riwork/native";
import { Brand } from "../Brand";
import { DEFAULT_THEME, type InterfaceTheme } from "../appearance";
import type { HostParams } from "../model";
import { createLocalServices } from "../adapters/local";
import type { DashboardDto } from "../client/types";
import { DashboardView } from "../dashboard/Dashboard";
import { routeTarget, type Filter, type Sort, type Target, type Wire } from "../dashboard/model";
import { GlobalChat, type ChatControl } from "./GlobalChat";
import { effectiveWindowBindings } from "../keyboard";
import { useCustomization } from "../customization";
import { Agents } from "../hosts/Agents";
import { projectKey, routeStep, type AgentHost, type AgentSession } from "../hosts/model";

export type GlobalView = "dashboard" | "chat";
export const NO_PROJECT_NOTICE = "Open one of this Mac's projects first.";
/** 28 pt of controls plus 6 (`global_window.rs`); traffic lights at (10, 8). */
export const GLOBAL_BAR = { height: 34, insetWithControls: 92, inset: 10 } as const;

interface WindowEntry { windowId: string; kind: "project" | "global"; projectId: string | null; key?: boolean; focusSerial?: number }
interface WindowState { mode?: string; occluded?: boolean; visible?: boolean }

/** The project a global-window command goes to: the most recently focused
 * local project window's, else the store's active project. */
export function commandProject(windows: WindowEntry[], activeProjectId: string | null | undefined): string | null {
  const projects = windows.filter((window) => window.kind === "project" && window.projectId && !window.projectId.startsWith("remote:"));
  projects.sort((a, b) => (b.key ? 1 : 0) - (a.key ? 1 : 0) || (b.focusSerial ?? 0) - (a.focusSerial ?? 0));
  return projects[0]?.projectId ?? activeProjectId ?? null;
}

/** The global window: Dashboard | Chat, with no project of its own (SHELL-034..067).
 * It never registers, selects or switches a project and never writes layouts,
 * shared tabs or chat requests; opening it starts nothing. */
/** Its notice bar says what this window did or could not do (SHELL-066); the
 * appearance diagnostic belongs to the project windows. */
export function GlobalWindow(props: { params: HostParams; theme?: () => InterfaceTheme; settings?: () => SettingsDto | null }) {
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const params = props.params;
  const restored = params.restore?.global as Partial<GlobalWindowDto> | undefined;
  const requested = restored?.view ?? params.global?.view;
  const view = signal<GlobalView>(requested === "chat" ? "chat" : "dashboard");
  const agentsShown = signal(false);
  const dashboardVisited = signal(view() === "dashboard");
  effect(() => { if (view() === "dashboard") dashboardVisited.set(true); });
  const filter = signal<Filter>("active");
  const sort = signal<Sort>("priority");
  const query = signal("");
  const notice = signal(params.notice ?? "");
  const windowState = signal<WindowState>({ mode: "windowed" });
  const dashboard = signal<DashboardDto | null>(null);
  // This Mac's own ports: the shared dashboard reads and answers through them.
  const services = createLocalServices();
  let saved: GlobalWindowDto | null = null;
  let loaded = false;
  let alive = true;
  let saving: Promise<unknown> = Promise.resolve();
  cleanup(() => { alive = false; });
  // Native host owns the traffic lights; a height here installs an AppKit
  // toolbar that competes with its positioning while the window resizes.
  Window.setTitleBar("hidden");
  Window.setTitle("Somtus · Global");
  host.post("window.setDockInfo", { project: "Global", branch: "" });
  cleanup(host.on("window.state", (next) => windowState.set(next as WindowState)));

  function apply(state: Partial<GlobalWindowDto>) {
    if (state.dashboardFilter && (["active", "needs_you", "working", "in_progress", "all"] as string[]).includes(state.dashboardFilter)) filter.set(state.dashboardFilter as Filter);
    else if (state.showQuiet !== undefined) filter.set(state.showQuiet ? "all" : "active");
    if (state.dashboardSort) sort.set(state.dashboardSort as Sort);
  }
  if (restored) apply(restored);
  void Dashboard.getGlobalWindow().then((state) => {
    if (!alive) return;
    saved = state;
    loaded = true;
    // A reload's handed-over state, then an explicit request, win over the file.
    if (!restored) apply(state);
    if (!requested) view.set(state.view === "chat" ? "chat" : "dashboard");
  }).catch((error: unknown) => notice.set(String(error)));

  /** Saved on every view, filter or sort change; the size is the host's. */
  function save() {
    if (!loaded || !saved) return;
    const next: GlobalWindowDto = {
      ...saved, view: view(), dashboardFilter: filter(), dashboardSort: sort(), showQuiet: filter() === "all",
    };
    if (JSON.stringify(next) === JSON.stringify(saved)) return;
    saved = next;
    saving = saving.then(() => Dashboard.saveGlobalWindow(next)).catch((error: unknown) => { if (alive) notice.set(String(error)); });
  }
  const chat: { control?: ChatControl } = {};
  function show(next: GlobalView, start = false) {
    agentsShown.set(false);
    view.set(next);
    save();
    if (next === "chat" && start) chat.control?.start();
  }
  function setFilter(next: Filter) { filter.set(next); save(); }
  function setSort(next: Sort) { sort.set(next); save(); }

  cleanup(host.on("global.show", (data) => {
    const event = data as { view?: GlobalView | null; start?: boolean };
    if (event.view === "dashboard" || event.view === "chat") show(event.view, event.start === true);
  }));

  // Reading is gated by visibility: an occluded window holds no subscription.
  const visible = () => windowState().occluded !== true && windowState().visible !== false;
  effect(() => {
    if (!visible() || agentsShown()) return;
    const subscription = services.dashboard.subscribe((dto) => dashboard.set(dto));
    return () => subscription.close();
  });
  const wire = () => (dashboard()?.wire ?? null) as Wire | null;

  async function target(): Promise<string | null> {
    const [windows, store] = await Promise.all([
      host.call("window.list", {}) as Promise<WindowEntry[]>,
      Store.snapshot().catch(() => null),
    ]);
    return commandProject(windows ?? [], store?.activeProjectId ?? null);
  }
  function newWindow() {
    void target().then((projectId) => {
      if (!projectId) { notice.set(NO_PROJECT_NOTICE); return; }
      return host.call("window.open", { kind: "project", projectId });
    }).catch((error: unknown) => notice.set(String(error)));
  }
  function routePanel(panel: string) {
    void target().then((projectId) => {
      if (!projectId) { notice.set(NO_PROJECT_NOTICE); return; }
      return host.call("window.route", { projectId, step: { kind: "panel", panel } });
    }).catch((error: unknown) => notice.set(String(error)));
  }
  function route(row: Target) {
    const current = wire();
    if (!current) return;
    void Sessions.list().catch(() => null).then(async (sessions) => {
      const shells: Record<string, string | null> = {};
      for (const session of sessions?.sessions ?? []) shells[session.id] = session.projectId ?? null;
      const next = routeTarget(row, current, shells);
      if (next.kind === "chat") show("chat");
      else if (next.kind === "notice") notice.set(next.text);
      else if (next.step) await host.call("window.route", { projectId: next.projectId, step: next.step });
      else await host.call("window.open", { kind: "project", projectId: next.projectId });
    }).catch((error: unknown) => notice.set(String(error)));
  }
  function openAgent(machine: AgentHost, session: AgentSession) {
    const projectId = projectKey(machine,session);
    if (!projectId) {
      if (!machine.connectionId && session.kind === "chat") { show("chat"); return; }
      notice.set("This session has no project window. Its status is available here."); return;
    }
    void host.call("window.route", { projectId, step: routeStep(machine,session) }).catch((error: unknown) => notice.set(String(error)));
  }
  function openPanel(projectId: string, panel: string) {
    void host.call("window.route", { projectId, step: { kind: "panel", panel } }).catch((error: unknown) => notice.set(String(error)));
  }

  const customization = useCustomization();
  let search: { focus: () => void } | undefined;
  let agentsSearch: {focus: () => void} | undefined;
  function command(id: string) {
    if (id === "closeTab" || id === "closeWindow") void host.call("window.close", {}).catch(() => {});
    else if (id === "newProjectWindow" || id === "newWindow") newWindow();
    else if (id === "openGlobalDashboard") show("dashboard");
    else if (id === "openGlobalChat" || id === "openOrchestrator") show("chat", true);
    else if (id === "openSettings") routePanel("settings");
    else if (id === "openSchedules") routePanel("schedules");
    else if (id === "openCustomization") void customization.edit();
    else if (id === "resetCustomization") void customization.reset();
    else if ((id === "search" || id === "find") && view() === "dashboard") (agentsShown() ? agentsSearch : search)?.focus();
  }
  effect(() => {
    const bindings = effectiveWindowBindings(customization.state().keys)
      .filter(([, id]) => ["closeTab", "newProjectWindow", "openSettings", "openSchedules", "openCustomization"].includes(id))
      .map(([key, id]) => keymap.bind(key, () => command(id), { when: "always" }));
    return () => { for (const unbind of bindings) unbind(); };
  });
  // ⌘F belongs to the dashboard's search only in the Dashboard view; in Chat it
  // reaches the chat's own find.
  effect(() => {
    if (view() !== "dashboard") return;
    return keymap.bind("mod+f", () => (agentsShown() ? agentsSearch : search)?.focus());
  });
  cleanup(host.on("command", (data) => {
    const id = (data as { id?: unknown }).id;
    if (typeof id === "string") command(id);
  }));

  const palette = () => theme().palette;
  const inset = () => windowState().mode === "fullscreen" ? GLOBAL_BAR.inset : GLOBAL_BAR.insetWithControls;
  const font = (size: number) => ({ fontFamily: theme().family, fontSize: size * theme().scale });
  function segment(id: GlobalView, label: string) {
    const selected = () => view() === id && !agentsShown();
    return <View testID={`global-view-${id}`} role="tab" accessibilityLabel={selected() ? `${label}, shown` : label}
      focusable={true} accessibilityState={{ selected: selected() }} onPress={() => show(id)}
      style={{ paddingHorizontal: 10, height: 22, justifyContent: "center", borderRadius: 5, backgroundColor: selected() ? palette().bg : "transparent" }}>
      <Text style={{ ...font(11), color: selected() ? palette().text : palette().muted }}>{label}</Text>
    </View>;
  }
  return <View testID="global-window" style={{ flex: 1, backgroundColor: palette().bg }}>
    <View testID="global-bar" desktop={{ windowDrag: true }} style={{ height: GLOBAL_BAR.height, flexDirection: "row", alignItems: "center", gap: 10, paddingLeft: inset(), paddingRight: 10, backgroundColor: palette().panel, borderBottomWidth: 1, borderColor: palette().divider }}>
      <View testID="global-title" style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
        <Brand theme={theme} size={18} wordmark={false} />
        <Text style={{ ...font(12), color: palette().text, fontWeight: "600" }}>Global</Text>
      </View>
      <View testID="global-view" role="tablist" accessibilityLabel="Global view" style={{ flexDirection: "row", gap: 2, padding: 2, borderRadius: 7, backgroundColor: palette().panelActive }}>
        {segment("dashboard", "Dashboard")}
        {segment("chat", "Chat")}
        <View testID="global-view-agents" role="tab" accessibilityLabel="Agents and hosts" focusable accessibilityState={{selected: agentsShown()}} onPress={() => { view.set("dashboard"); agentsShown.set(true); }}
          style={{paddingHorizontal:10,height:22,justifyContent:"center",borderRadius:5,backgroundColor:agentsShown() ? palette().bg : "transparent"}}>
          <Text style={{...font(11),color:agentsShown() ? palette().text : palette().muted}}>Agents & hosts</Text>
        </View>
      </View>
      <View testID="global-drag" desktop={{ windowDrag: true }} style={{ flex: 1, alignSelf: "stretch" }} />
      <Text style={{ ...font(10), color: palette().muted }}>{agentsShown() ? "All connected hosts" : "This Mac · every project"}</Text>
    </View>
    <View style={{ flex: 1, minHeight: 0 }}>
      {dashboardVisited() && <View style={{ flex: 1, minHeight: 0, display: view() === "dashboard" && !agentsShown() ? "flex" : "none" }}>
        <DashboardView dto={dashboard} theme={theme} filter={filter} sort={sort} query={query}
          onFilter={setFilter} onSort={setSort} onSearch={(node) => { search = node; }}
          onOpen={route} onPanel={openPanel} onGlobalChat={() => show("chat", true)}
          onRefresh={() => { void services.dashboard.refresh().catch((error: unknown) => notice.set(String(error))); }}
          plan={() => chat.control?.plan() ?? null} starting={() => chat.control?.starting() ?? false} decisions={services.dashboard.decisions} /></View>}
      <View style={{flex:1,minHeight:0,display:agentsShown() ? "flex" : "none"}}>
        <Agents visible={() => visible() && agentsShown()} theme={theme} onOpen={openAgent} onNotice={text => notice.set(text)} onSearch={node => {agentsSearch=node;}} />
      </View>
      <GlobalChat visible={() => view() === "chat"} global={() => wire()?.global ?? { kind: "unknown" }} theme={theme}
        settings={props.settings} startNow={params.global?.start === true && view() === "chat"}
        onApiKeySettings={() => {
          void target().then((projectId) => {
            if (!projectId) { notice.set(NO_PROJECT_NOTICE); return; }
            return host.call("window.route", { projectId, step: { kind: "apiKeySettings" } });
          }).catch((error) => notice.set(String(error)));
        }}
        onControl={(control) => { chat.control = control; }} onNotice={(text) => notice.set(text)} />
    </View>
    {notice() !== "" && <View testID="global-notice" role="alert" accessibilityLabel={notice()}
      style={{ flexDirection: "row", alignItems: "center", gap: 10, paddingHorizontal: 10, paddingVertical: 6, backgroundColor: palette().panel, borderTopWidth: 1, borderColor: palette().divider }}>
      <Text testID="global-notice-text" style={{ ...font(11), flex: 1, color: palette().gold }}>{notice()}</Text>
      <View testID="global-notice-dismiss" role="button" accessibilityLabel="Dismiss" onPress={() => notice.set("")}>
        <Text style={{ ...font(11), color: palette().gold }}>Dismiss</Text>
      </View>
    </View>}
  </View>;
}
