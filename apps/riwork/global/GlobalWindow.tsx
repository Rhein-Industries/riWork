import { Text, View, cleanup, effect, host, keymap, signal } from "nsh";
import { Window } from "nsh/desktop";
import { Dashboard, Sessions, Settings, Store, type DashboardEvent, type GlobalWindowDto, type SettingsDto } from "riwork/native";
import { DEFAULT_THEME, type InterfaceTheme } from "../appearance";
import type { HostParams } from "../model";
import { DashboardView } from "../dashboard/Dashboard";
import { routeTarget, type Filter, type Sort, type Target, type Wire } from "../dashboard/model";
import { GlobalChat, type ChatControl } from "./GlobalChat";

export type GlobalView = "dashboard" | "chat";
export const NO_PROJECT_NOTICE = "Open one of this Mac's projects first.";
/** 28 pt of controls plus 6 (`global_window.rs`); traffic lights at (10, 8). */
export const GLOBAL_BAR = { height: 34, insetWithControls: 92, inset: 10 } as const;

interface WindowEntry { windowId: string; kind: "project" | "global"; projectId: string | null; key?: boolean; focusSerial?: number }
interface WindowState { mode?: string; occluded?: boolean; visible?: boolean }

/** The project a global-window command goes to: the most recently focused
 * local project window's, else the store's active project. */
export function commandProject(windows: WindowEntry[], activeProjectId: string | null | undefined): string | null {
  const projects = windows.filter((window) => window.kind === "project" && window.projectId);
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
  const filter = signal<Filter>("active");
  const sort = signal<Sort>("priority");
  const query = signal("");
  const notice = signal(params.notice ?? "");
  const windowState = signal<WindowState>({ mode: "windowed" });
  const dashboard = signal<DashboardEvent["data"] | null>(null);
  let saved: GlobalWindowDto | null = null;
  let loaded = false;
  let alive = true;
  let saving: Promise<unknown> = Promise.resolve();
  cleanup(() => { alive = false; });
  Window.setTitleBar("hidden", GLOBAL_BAR.height);
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
    if (!visible()) return;
    let rev = -1;
    const subscription = Dashboard.subscribe((event) => {
      if (event.rev <= rev && !event.resync) return;
      rev = event.rev;
      dashboard.set(event.data);
    });
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
  function openPanel(projectId: string, panel: string) {
    void host.call("window.route", { projectId, step: { kind: "panel", panel } }).catch((error: unknown) => notice.set(String(error)));
  }

  let search: { focus: () => void } | undefined;
  function command(id: string) {
    if (id === "closeTab" || id === "closeWindow") void host.call("window.close", {}).catch(() => {});
    else if (id === "newProjectWindow" || id === "newWindow") newWindow();
    else if (id === "openSettings") routePanel("settings");
    else if (id === "openSchedules") routePanel("schedules");
    else if ((id === "search" || id === "find") && view() === "dashboard") search?.focus();
  }
  for (const [key, id] of [["mod+w", "closeTab"], ["mod+shift+n", "newProjectWindow"], ["mod+,", "openSettings"], ["mod+shift+s", "openSchedules"]] as const) {
    cleanup(keymap.bind(key, () => command(id), { when: "always" }));
  }
  // ⌘F belongs to the dashboard's search only in the Dashboard view; in Chat it
  // reaches the chat's own find.
  cleanup(keymap.bind("mod+f", () => command("search")));
  cleanup(host.on("command", (data) => {
    const id = (data as { id?: unknown }).id;
    if (typeof id === "string") command(id);
  }));

  const palette = () => theme().palette;
  const cased = (text: string) => Settings.cased(text, theme().native);
  const inset = () => windowState().mode === "fullscreen" ? GLOBAL_BAR.inset : GLOBAL_BAR.insetWithControls;
  const font = (size: number) => ({ fontFamily: theme().family, fontSize: size * theme().scale });
  function segment(id: GlobalView, label: string) {
    const selected = () => view() === id;
    return <View testID={`global-view-${id}`} role="tab" accessibilityLabel={selected() ? `${label}, shown` : label}
      accessibilityState={{ selected: selected() }} onPress={() => show(id)}
      style={{ paddingHorizontal: 10, height: 22, justifyContent: "center", borderRadius: 5, backgroundColor: selected() ? palette().panelActive : "transparent" }}>
      <Text style={{ ...font(11), color: selected() ? palette().text : palette().muted }}>{cased(label)}</Text>
    </View>;
  }
  return <View testID="global-window" style={{ flex: 1, backgroundColor: palette().bg }}>
    <View testID="global-bar" style={{ height: GLOBAL_BAR.height, flexDirection: "row", alignItems: "center", gap: 10, paddingLeft: inset(), paddingRight: 10, backgroundColor: palette().panel, borderBottomWidth: 1, borderColor: palette().divider }}>
      <View testID="global-title" style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
        <Text style={{ ...font(12), color: palette().magenta }}>▦</Text>
        <Text style={{ ...font(12), color: palette().magenta }}>{cased("Global")}</Text>
      </View>
      <View testID="global-view" role="tablist" accessibilityLabel="Global view" style={{ flexDirection: "row", gap: 2, padding: 2, borderRadius: 7, borderWidth: 1, borderColor: palette().divider }}>
        {segment("dashboard", "Dashboard")}
        {segment("chat", "Chat")}
      </View>
      <View testID="global-drag" desktop={{ windowDrag: true }} style={{ flex: 1, alignSelf: "stretch" }} />
      <Text style={{ ...font(10), color: palette().muted }}>This Mac · every project</Text>
    </View>
    <View style={{ flex: 1, minHeight: 0 }}>
      {view() === "dashboard" ?
        <DashboardView dto={dashboard} theme={theme} filter={filter} sort={sort} query={query}
          onFilter={setFilter} onSort={setSort} onSearch={(node) => { search = node; }}
          onOpen={route} onPanel={openPanel} onGlobalChat={() => show("chat", true)}
          onRefresh={() => { void Dashboard.refresh().catch((error: unknown) => notice.set(String(error))); }}
          plan={() => chat.control?.plan() ?? null} starting={() => chat.control?.starting() ?? false} /> : null}
      <GlobalChat visible={() => view() === "chat"} global={() => wire()?.global ?? { kind: "unknown" }} theme={theme}
        settings={props.settings} startNow={params.global?.start === true && view() === "chat"}
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
