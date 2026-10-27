import { Clipboard, Overlay, Scroll, Text, View, cleanup, effect, host, signal, type YNode } from "nsh";
import { Sessions, Store, type LayoutSession, type SettingsDto, type ShellSessionDto, type StoreState } from "riwork/native";
import { Settings } from "riwork/native";
import { DEFAULT_THEME, space, type InterfaceTheme } from "./appearance";
import { activePane, mainPaneSummary, type WorkspaceSnapshot } from "./model";

export const DEFAULT_STATUS_ITEMS = [
  "project", "worktree", "agent_activity", "live_sessions", "resources", "usage", "codex_account", "session_id", "global_orchestrator", "project_orchestrator", "layout",
].map((kind) => ({ kind, enabled: kind !== "worktree" && kind !== "agent_activity", side: ["project", "worktree", "agent_activity"].includes(kind) ? "left" : "right" }));
export const STATUS_METRICS = { height: 22, text: 9, nativePadding: 12, colorfulPadding: 8, nativeGap: 16, colorfulGap: 10, layoutWidth: 296, globalWidth: 300, edgeMargin: 6 };

interface Props {
  session: LayoutSession;
  snapshot: () => WorkspaceSnapshot;
  projectLabel: string | (() => string);
  projectId?: string | (() => string);
  settings?: () => SettingsDto | null;
  theme?: () => InterfaceTheme;
  action: (run: () => unknown) => void;
  onError: (error: unknown) => void;
  modalOpen?: () => boolean;
  /** Local shell creation for project windows; omitted in fixtures without a project. */
  newShell?: { busy: () => boolean; create: () => void };
  /** Anchored menus draw over native terminals, which freeze while one is open. */
  onMenu?: (open: boolean) => void;
}

export function StatusBar(props: Props) {
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const projectId = () => typeof props.projectId === "function" ? props.projectId() : props.projectId ?? props.snapshot().projectId;
  const projectLabel = () => typeof props.projectLabel === "function" ? props.projectLabel() : props.projectLabel;
  const settings = () => props.settings?.()?.statusBar ?? { enabled: true, items: DEFAULT_STATUS_ITEMS };
  const menu = signal<"layout" | "global" | null>(null);
  const sessions = signal<ShellSessionDto[] | null>(null);
  const store = signal<StoreState | null>(null);
  const globalView = signal<"dashboard" | "chat" | null>(null);
  const layoutAnchor = signal<YNode | null>(null);
  const globalAnchor = signal<YNode | null>(null);
  let sessionRev = -1;
  let storeRev = -1;
  const sessionSubscription = Sessions.subscribe((event) => {
    if (event.rev <= sessionRev) return;
    sessionRev = event.rev;
    sessions.set(event.data.sessions);
  });
  const storeSubscription = Store.subscribe((event) => {
    if (event.rev <= storeRev) return;
    storeRev = event.rev;
    store.set(event.data);
  });
  cleanup(() => { sessionSubscription.close(); storeSubscription.close(); });
  cleanup(host.on("global.show", (event) => globalView.set((event as { view: "dashboard" | "chat" | null }).view)));
  const items = (side: string) => settings().items.filter((item) => item.enabled && item.side === side);
  effect(() => {
    const wanted = menu() === "global" ? "global_orchestrator" : "layout";
    if (!settings().enabled || !settings().items.some((item) => item.kind === wanted && item.enabled) || props.modalOpen?.()) menu.set(null);
  });
  effect(() => props.onMenu?.(menu() !== null));
  function toggle(which: "layout" | "global") {
    if (props.modalOpen?.()) return;
    menu.set(menu() === which ? null : which);
  }
  function panel(name: string) {
    menu.set(null);
    const id = `panel:${name}`;
    props.action(() => props.snapshot().panes.some((pane) => pane.tabs.some((tab) => tab.id === id)) ? props.session.selectPanel(id) : props.session.openPanel(name));
  }
  function openGlobal(view: "dashboard" | "chat") {
    menu.set(null);
    void host.call("window.open", { kind: "global", view, start: view === "chat", activate: true }).catch(props.onError);
  }
  const selectedSession = () => {
    const pane = activePane(props.snapshot());
    const id = pane?.activeTab;
    return id?.startsWith("shell:") ? id.slice(6) : undefined;
  };
  const textStyle = () => ({ fontFamily: theme().monoFamily, fontSize: 9 * theme().scale, color: theme().palette.muted });
  function item(kind: string) {
    const key = kind.replaceAll("_", "-");
    const current = sessions()?.find((session) => session.id === selectedSession());
    const projectName = store()?.projects.find((project) => project.id === projectId())?.name ?? projectLabel();
    const worktreeId = props.snapshot().selectedWorktreeId ?? current?.worktreeId;
    const branch = store()?.worktrees.find((worktree) => worktree.id === worktreeId)?.branch ?? "No worktree";
    const live = sessions()?.filter((session) => session.alive && session.projectId === projectId()).length ?? "—";
    const labels: Record<string, string> = {
      project: projectName, worktree: branch, agent_activity: "Agents · —", live_sessions: `${live} ${theme().native ? "live" : "LIVE"}`,
      resources: "CPU — RAM —", usage: "CODEX · ACCOUNT UNKNOWN", codex_account: "DEFAULT (APP) · Email unknown", session_id: selectedSession()?.slice(0, 8) ?? "",
      global_orchestrator: "Global", project_orchestrator: "P·ORCH", layout: "Layout",
    };
    return <View testID={kind === "global_orchestrator" ? "top-orchestrator" : `status-item-${key}`}
      ref={(node) => { if (kind === "layout") layoutAnchor.set(node); if (kind === "global_orchestrator") globalAnchor.set(node); }}
      accessibilityLabel={kind === "global_orchestrator" ? "Global: dashboard and chat" : kind === "session_id" ? "Copy active session ID" : kind === "project" ? `Project ${projectName}` : labels[kind]}
      role={["project", "worktree", "layout", "global_orchestrator", "session_id"].includes(kind) ? "button" : "text"}
      disabled={kind === "session_id" && !selectedSession()}
      style={kind === "session_id" ? { minWidth: 8, minHeight: 9 * theme().scale } : undefined}
      onPress={() => {
        if (kind === "layout") toggle("layout");
        else if (kind === "global_orchestrator") toggle("global");
        else if (kind === "project") panel("projects");
        else if (kind === "worktree") panel("worktrees");
        else if (kind === "session_id" && selectedSession()) void Clipboard.writeText(selectedSession()!);
      }}>
      <Text numberOfLines={1} style={textStyle()}>{theme().native ? Settings.sentenceCase(labels[kind]) : labels[kind]}</Text>
    </View>;
  }
  function row(id: string, label: string, shortcut: string, action?: () => void, detail?: string) {
    return <View testID={id} accessibilityLabel={label} role="button" disabled={!action} onPress={action}
      style={{ paddingHorizontal: 12, paddingVertical: 8, gap: 3 }}>
      <View style={{ flexDirection: "row", justifyContent: "space-between", gap: 10 }}>
        <Text style={{ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.text }}>{label}</Text>
        <Text style={{ fontFamily: theme().monoFamily, fontSize: 11 * theme().scale, color: theme().palette.muted }}>{shortcut}</Text>
      </View>
      {detail && <Text style={{ fontFamily: theme().family, fontSize: 9 * theme().scale, color: theme().palette.muted }}>{detail}</Text>}
    </View>;
  }
  return <>
    {settings().enabled && <View testID="status-bar" style={{ height: space(STATUS_METRICS.height, theme().scale), flexDirection: "row", alignItems: "center", paddingHorizontal: space(theme().native ? STATUS_METRICS.nativePadding : STATUS_METRICS.colorfulPadding, theme().scale), gap: 8, backgroundColor: theme().palette.panel }}>
      <Scroll testID="status-left" horizontal={true} style={{ flexShrink: 0, maxWidth: items("right").length ? "40%" : "100%" }} contentStyle={{ flexDirection: "row", alignItems: "center", gap: space(theme().native ? 16 : 10, theme().scale) }}>
        {items("left").map((entry) => item(entry.kind))}
        {props.newShell && <View testID="new-shell-status" accessibilityLabel="New local shell" role="button" onPress={props.newShell.create}>
          <Text numberOfLines={1} style={{ ...textStyle(), color: theme().palette.cyan }}>{props.newShell.busy() ? "Starting…" : "+ Shell"}</Text>
        </View>}
      </Scroll>
      <View style={{ flex: 1, minWidth: 4 }} />
      <Scroll testID="status-right" horizontal={true} style={{ flexShrink: 1, minWidth: 0 }} contentStyle={{ flexDirection: "row", alignItems: "center", gap: space(theme().native ? 16 : 10, theme().scale) }}>
        {items("right").map((entry) => item(entry.kind))}
      </Scroll>
    </View>}
    {menu() && <Overlay testID={`${menu()}-menu`} anchor={menu() === "layout" ? layoutAnchor() : globalAnchor()} placement="top" align="end" offset={3} margin={6}
      onCancel={() => menu.set(null)} onPressOutside={() => menu.set(null)}
      style={{ width: space(menu() === "layout" ? 296 : 300, theme().scale), maxWidth: "100%", paddingVertical: 4, backgroundColor: theme().palette.panel, borderColor: theme().palette.divider, borderWidth: 1, borderRadius: 8 }}>
      {menu() === "layout" ? <>
        {row("layout-menu-default", "Default layout", "⌘⌥L", () => { menu.set(null); props.action(() => props.session.defaultLayout()); })}
        {row("layout-menu-gather", "Gather into main pane", "⌘⇧M", props.snapshot().panes.some((pane) => pane.main) ? () => { menu.set(null); props.action(() => props.session.gather()); } : undefined,
          props.snapshot().panes.some((pane) => pane.main) ? undefined : "Choose a main pane from its pane menu first")}
        {row("layout-menu-summary", `Main pane: ${mainPaneSummary(props.snapshot())}`, "")}
      </> : <>
        {row("global-menu-title", "Global", "")}
        {row("global-menu-dashboard", "Dashboard", `⌘⌥G${globalView() === "dashboard" ? " ✓" : ""}`, () => openGlobal("dashboard"))}
        {row("global-menu-chat", "Chat", `⌘⇧O${globalView() === "chat" ? " ✓" : ""}`, () => openGlobal("chat"))}
      </>}
    </Overlay>}
  </>;
}
