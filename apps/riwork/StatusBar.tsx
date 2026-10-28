import { Clipboard, Overlay, Scroll, Text, View, cleanup, effect, host, signal, type YNode } from "nsh";
import { Accounts, Sessions, Store, Usage, type AccountsDto, type LayoutSession, type SettingsDto, type ShellSessionDto, type StoreState, type UsageDto } from "riwork/native";
import { Settings } from "riwork/native";
import { DEFAULT_THEME, space, type InterfaceTheme } from "./appearance";
import { activePane, mainPaneSummary, type WorkspaceSnapshot } from "./model";
import {
  BALANCED_INHERIT, accountItemLabel, defaultChoices, knownCodexProfiles, projectSelection, reportedAccountLabel,
  resolveSelection, systemHome, usageAria, usageFocus, usageHint, usageItemText, usageSegments, type AccountListLike, type ProjectChoice, type UsageMode,
} from "./usage";

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
  /** Workspace focus presentation rows for the layout menu. */
  focus?: { mode: () => boolean; centered: () => boolean; toggle: () => void; toggleCentered: () => void };
  /** Opens this project's orchestrator; the item is disabled without it. */
  openProjectOrchestrator?: () => void;
}

type Menu = "layout" | "global" | "account";
const MENU_ITEM: Record<Menu, string> = { layout: "layout", global: "global_orchestrator", account: "codex_account" };
const NO_ACCOUNTS: AccountListLike = { listed: false, accounts: [] };

export function StatusBar(props: Props) {
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const projectId = () => typeof props.projectId === "function" ? props.projectId() : props.projectId ?? props.snapshot().projectId;
  const projectLabel = () => typeof props.projectLabel === "function" ? props.projectLabel() : props.projectLabel;
  const settings = () => props.settings?.()?.statusBar ?? { enabled: true, items: DEFAULT_STATUS_ITEMS };
  const menu = signal<Menu | null>(null);
  const sessions = signal<ShellSessionDto[] | null>(null);
  const store = signal<StoreState | null>(null);
  const usage = signal<UsageDto | null>(null);
  const accounts = signal<AccountsDto | null>(null);
  const globalView = signal<"dashboard" | "chat" | null>(null);
  const layoutAnchor = signal<YNode | null>(null);
  const globalAnchor = signal<YNode | null>(null);
  const accountAnchor = signal<YNode | null>(null);
  const choosingAccount = signal(false);
  let sessionRev = -1;
  let storeRev = -1;
  let usageRev = -1;
  let accountsRev = -1;
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
  // One process service reads usage (with today's backoff) while any window
  // subscribes; the account list is cache-only until a refresh.
  const usageSubscription = Usage.subscribe((event) => {
    if (event.rev <= usageRev) return;
    usageRev = event.rev;
    usage.set(event.data);
  });
  const accountsSubscription = Accounts.subscribe((event) => {
    if (event.rev <= accountsRev) return;
    accountsRev = event.rev;
    accounts.set(event.data);
  });
  cleanup(() => { sessionSubscription.close(); storeSubscription.close(); usageSubscription.close(); accountsSubscription.close(); });
  cleanup(host.on("global.show", (event) => globalView.set((event as { view: "dashboard" | "chat" | null }).view)));
  // A remote project's bar draws no account item.
  const shown = (kind: string) => !(kind === "codex_account" && props.snapshot().remoteProject);
  const items = (side: string) => settings().items.filter((item) => item.enabled && item.side === side && shown(item.kind));
  effect(() => {
    const open = menu();
    const wanted = MENU_ITEM[open ?? "layout"];
    if (!settings().enabled || !settings().items.some((item) => item.kind === wanted && item.enabled) || !shown(wanted) || props.modalOpen?.()) menu.set(null);
  });
  effect(() => props.onMenu?.(menu() !== null));
  function toggle(which: Menu) {
    if (props.modalOpen?.()) return;
    const opening = menu() !== which;
    menu.set(opening ? which : null);
    // PANEL-187: with no account list yet, opening the menu asks for one.
    const list = accounts();
    if (opening && which === "account" && list && !list.listed && !list.refreshing) void Accounts.refresh().catch(props.onError);
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
  const accountList = (): AccountListLike => accounts() ?? NO_ACCOUNTS;
  const projectChoice = (): ProjectChoice | undefined => store()?.projects.find((project) => project.id === projectId())?.codexAccount;
  /** What Codex reported for the system default's home: the only name it has. */
  const systemLabel = () => reportedAccountLabel(usage()?.codex ?? [], systemHome(resolveSelection(null, accountList())));
  const segments = () => {
    const list = accounts();
    const read = usage();
    if (!list || !read) return null;
    const mode = (props.settings?.()?.statusBar.usageMode ?? "current_chat") as UsageMode;
    const shells = sessions() ?? [];
    return usageSegments({
      mode, focus: usageFocus(selectedSession() ? `shell:${selectedSession()}` : undefined, shells, list),
      projectDefault: projectSelection(projectChoice() ?? { source: "inherit" }, list.selectedId, list),
      accounts: mode === "all_accounts" ? knownCodexProfiles(list) : [],
      codex: read.codex, claude: read.claude.filter((entry) => entry.projectId === projectId()), now: read.now,
    });
  };
  const accountLabel = () => accountItemLabel(accounts()?.balancing ?? false, projectChoice(), accounts()?.selectedId, accountList(), systemLabel());
  function item(kind: string) {
    const key = kind.replaceAll("_", "-");
    const current = sessions()?.find((session) => session.id === selectedSession());
    const projectName = store()?.projects.find((project) => project.id === projectId())?.name ?? projectLabel();
    const worktreeId = props.snapshot().selectedWorktreeId ?? current?.worktreeId;
    const branch = store()?.worktrees.find((worktree) => worktree.id === worktreeId)?.branch ?? "No worktree";
    const live = sessions()?.filter((session) => session.alive && session.projectId === projectId()).length ?? "—";
    const readouts = kind === "usage" ? segments() : null;
    const mode = (props.settings?.()?.statusBar.usageMode ?? "current_chat") as UsageMode;
    const labels: Record<string, string> = {
      project: projectName, worktree: branch, agent_activity: "Agents · —", live_sessions: `${live} ${theme().native ? "live" : "LIVE"}`,
      resources: "CPU — RAM —", usage: readouts ? usageItemText(mode, readouts) : "CODEX · LOADING", codex_account: accountLabel(), session_id: selectedSession()?.slice(0, 8) ?? "",
      global_orchestrator: "Global", project_orchestrator: "P·ORCH", layout: "Layout",
    };
    const aria: Record<string, string> = {
      global_orchestrator: "Global: dashboard and chat", session_id: "Copy active session ID", project: `Project ${projectName}`,
      usage: readouts ? usageAria(readouts) : `Usage ${labels.usage}`, codex_account: `New Codex sessions: ${labels.codex_account}`,
    };
    return <View testID={kind === "global_orchestrator" ? "top-orchestrator" : `status-item-${key}`}
      ref={(node) => { if (kind === "layout") layoutAnchor.set(node); if (kind === "global_orchestrator") globalAnchor.set(node); if (kind === "codex_account") accountAnchor.set(node); }}
      accessibilityLabel={aria[kind] ?? labels[kind]}
      accessibilityHint={kind === "usage" && readouts ? usageHint(readouts) : kind === "codex_account" && menu() !== "account" ? "New Codex sessions in this project · click to choose" : undefined}
      role={["project", "worktree", "layout", "global_orchestrator", "session_id", "usage", "codex_account", "project_orchestrator"].includes(kind) ? "button" : "text"}
      disabled={(kind === "session_id" && !selectedSession()) || (kind === "project_orchestrator" && !props.openProjectOrchestrator)}
      style={kind === "session_id" ? { minWidth: 8, minHeight: 9 * theme().scale } : kind === "codex_account" ? { minWidth: 48, maxWidth: 300 } : kind === "usage" ? { minWidth: 48 } : undefined}
      onPress={() => {
        if (kind === "layout") toggle("layout");
        else if (kind === "global_orchestrator") toggle("global");
        else if (kind === "codex_account") toggle("account");
        else if (kind === "usage") panel("usage");
        else if (kind === "project") panel("projects");
        else if (kind === "worktree") panel("worktrees");
        else if (kind === "project_orchestrator") props.openProjectOrchestrator?.();
        else if (kind === "session_id" && selectedSession()) void Clipboard.writeText(selectedSession()!);
      }}>
      <Text numberOfLines={1} style={kind === "usage" || kind === "codex_account" ? { ...textStyle(), color: theme().native || menu() === "account" && kind === "codex_account" ? textStyle().color : theme().palette.cyan } : textStyle()}>{theme().native ? Settings.sentenceCase(labels[kind]) : labels[kind]}</Text>
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
  function note(id: string, text: string, color: string) {
    return <Text testID={id} style={{ paddingHorizontal: 12, paddingBottom: 4, fontFamily: theme().family, fontSize: 9 * theme().scale, color }}>{text}</Text>;
  }
  /** PANEL-188: writes only the project's choice for new sessions. */
  function accountMenu() {
    const list = accountList();
    const choices = defaultChoices(projectChoice() ?? { source: "inherit" }, accounts()?.selectedId, list, systemLabel());
    if (accounts()?.balancing && choices.rows[0]) choices.rows[0].detail = BALANCED_INHERIT;
    const rowId = (choice: ProjectChoice) => choice.source === "inherit" ? "account-menu-inherit" : choice.source === "system_default" ? "account-menu-system" : `account-menu-saved-${choice.accountId}`;
    return <>
      {row("account-menu-title", "New Codex sessions in this project", "")}
      {note("account-menu-note", "Running chats and shells keep their account; --account still chooses one.", theme().palette.muted)}
      {choices.status && note("account-menu-status", choices.status, theme().palette.gold)}
      {choices.rows.map((choice) => row(rowId(choice.choice), choice.label, choice.current ? "✓" : "", choice.selectable && !choosingAccount() ? () => {
        choosingAccount.set(true);
        const id = projectId();
        if (!id) { choosingAccount.set(false); props.onError("Open a local project to choose its account."); return; }
        void Store.setProjectCodexAccount(id, choice.choice).then(() => menu.set(null)).catch(props.onError).finally(() => choosingAccount.set(false));
      } : undefined, choice.detail))}
      {row("account-menu-project-heading", "Project", "")}
      {row("account-menu-project-settings", "Project Settings…", "", () => panel("project_settings"))}
    </>;
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
    {menu() && <Overlay testID={`${menu()}-menu`} anchor={menu() === "layout" ? layoutAnchor() : menu() === "account" ? accountAnchor() : globalAnchor()} placement="top" align="end" offset={3} margin={6}
      onCancel={() => menu.set(null)} onPressOutside={() => menu.set(null)}
      style={{ width: space(menu() === "layout" ? 296 : menu() === "account" ? 320 : 300, theme().scale), maxWidth: "100%", paddingVertical: 4, backgroundColor: theme().palette.panel, borderColor: theme().palette.divider, borderWidth: 1, borderRadius: 8 }}>
      {menu() === "account" ? accountMenu() : menu() === "layout" ? <>
        {row("layout-menu-default", "Default layout", "⌘⌥L", () => { menu.set(null); props.action(() => props.session.defaultLayout()); })}
        {row("layout-menu-gather", "Gather into main pane", "⌘⇧M", props.snapshot().panes.some((pane) => pane.main) ? () => { menu.set(null); props.action(() => props.session.gather()); } : undefined,
          props.snapshot().panes.some((pane) => pane.main) ? undefined : "Choose a main pane from its pane menu first")}
        {props.focus && row("layout-menu-focus", "Focus tab", `⌘⇧F${props.focus.mode() ? " ✓" : ""}`, () => { menu.set(null); props.focus!.toggle(); })}
        {props.focus && row("layout-menu-centered", "Center focused tab", props.focus.centered() ? "✓" : "", () => { menu.set(null); props.focus!.toggleCentered(); })}
        {row("layout-menu-summary", `Main pane: ${mainPaneSummary(props.snapshot())}`, "")}
      </> : <>
        {row("global-menu-title", "Global", "")}
        {row("global-menu-dashboard", "Dashboard", `⌘⌥G${globalView() === "dashboard" ? " ✓" : ""}`, () => openGlobal("dashboard"))}
        {row("global-menu-chat", "Chat", `⌘⇧O${globalView() === "chat" ? " ✓" : ""}`, () => openGlobal("chat"))}
      </>}
    </Overlay>}
  </>;
}
