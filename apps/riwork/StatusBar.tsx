import { Clipboard, Overlay, Scroll, Text, View, cleanup, computed, effect, host, keymap, signal, type YNode } from "nsh";
import { Accounts, Dashboard, Sessions, Store, Usage, type AccountsDto, type LayoutSession, type SessionMetricsSnapshot, type SettingsDto, type ShellSessionDto, type StoreState, type UsageDto } from "riwork/native";
import { Settings } from "riwork/native";
import { DEFAULT_THEME, space, type InterfaceTheme } from "./appearance";
import { Icon } from "./Icon";
import type { Wire } from "./dashboard/model";
import { activePane, mainPaneSummary, type WorkspaceSnapshot } from "./model";
import {
  BALANCED_INHERIT, accountItemLabel, defaultChoices, knownCodexProfiles, projectSelection, reportedAccountLabel,
  resolveSelection, resourcesText, systemHome, usageAria, usageFocus, usageHint, usageItemText, usageSegments, whole, type AccountListLike, type ProjectChoice, type UsageMode, type UsageSegment,
} from "./usage";

export const DEFAULT_STATUS_ITEMS = [
  "project", "worktree", "agent_activity", "live_sessions", "resources", "usage", "codex_account", "session_id", "global_orchestrator", "project_orchestrator", "layout",
].map((kind) => ({ kind, enabled: kind !== "worktree" && kind !== "agent_activity", side: ["project", "worktree", "agent_activity"].includes(kind) ? "left" : "right" }));
export const STATUS_METRICS = { height: 22, text: 9, padding: 12, gap: 16, layoutWidth: 296, globalWidth: 300, edgeMargin: 6 };

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
  /** Usage already delivered by the focused native chat; never account quota. */
  nativeChat?: () => { provider: string; usageLabel?: string; usageDetail?: string } | null;
  /** A project on another Mac: its worktree branch and live shells as its host last
   * listed them (null before a list arrives). This Mac's sessions never stand in. */
  remote?: () => { branch: string | null; live: number | null } | null;
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
  const metrics = signal<SessionMetricsSnapshot["metrics"]>([]);
  const activity = signal<Wire | null>(null);
  const globalView = signal<"dashboard" | "chat" | null>(null);
  const layoutAnchor = signal<YNode | null>(null);
  const globalAnchor = signal<YNode | null>(null);
  const accountAnchor = signal<YNode | null>(null);
  const choosingAccount = signal(false);
  const focusedItem = signal<string | null>(null), focusedRow = signal<string | null>(null);
  const menuRows = new Map<string, { node?: YNode; action: () => (() => void) | undefined }>();
  let menuScroll: YNode | undefined, menuOffset = 0;
  let sessionRev = -1;
  let storeRev = -1;
  let usageRev = -1;
  let accountsRev = -1;
  let metricsRev = -1;
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
  // Sampling costs a `ps` run, so only a bar showing the item subscribes.
  // Another Mac's shells are not sampled; its item says so (main.rs).
  const resourcesShown = computed(() => settings().enabled && !props.snapshot().remoteProject
    && settings().items.some((item) => item.kind === "resources" && item.enabled));
  effect(() => {
    if (!resourcesShown()) return;
    const subscription = Sessions.subscribeMetrics((event) => {
      if (event.rev <= metricsRev) return;
      metricsRev = event.rev;
      metrics.set(event.data.metrics);
    });
    cleanup(() => subscription.close());
  });
  effect(() => {
    if (!settings().enabled || props.snapshot().remoteProject || !settings().items.some((item) => item.enabled && ["agent_activity", "global_orchestrator", "project_orchestrator"].includes(item.kind))) return;
    let revision = -1;
    const subscription = Dashboard.subscribe((event) => {
      if (event.rev <= revision && !event.resync) return;
      revision = event.rev;
      activity.set(event.data.wire as Wire | null);
    });
    return () => subscription.close();
  });
  const projectActivity = () => activity()?.projects?.find((project) => project.id === projectId());
  const agentSummary = () => {
    const counts = projectActivity()?.agents;
    if (!counts || props.snapshot().remoteProject) return "Agents · —";
    const parts = [];
    if (counts.working) { parts.push(`● ${counts.working} working`); if (counts.subagents) parts.push(`${counts.subagents} subagent${counts.subagents === 1 ? "" : "s"}`); }
    if (counts.done) parts.push(`✓ ${counts.done} done`);
    if (counts.waiting) parts.push(`◌ ${counts.waiting} waiting`);
    if (counts.unknown) parts.push(`? ${counts.unknown} unknown`);
    return parts.join(" · ") || "Agents · —";
  };
  const orchestratorMark = (state?: string) => state === "working" || state === "running" || state === "starting" ? "● " : state === "waiting" ? "◌ " : state === "done" || state === "idle" ? "✓ " : "";
  const orchestratorState = (kind: string) => {
    if (kind === "project_orchestrator") return projectActivity()?.orchestrator?.activity ?? projectActivity()?.orchestrator?.chat_state;
    const global = activity()?.global;
    return global?.kind === "chat" ? global.chat_state : global?.kind === "terminal" ? global.activity : undefined;
  };
  cleanup(() => { sessionSubscription.close(); storeSubscription.close(); usageSubscription.close(); accountsSubscription.close(); });
  cleanup(host.on("global.show", (event) => globalView.set((event as { view: "dashboard" | "chat" | null }).view)));
  // A remote project's bar draws no usage or account item.
  const shown = (kind: string) => !((kind === "codex_account" || kind === "usage") && props.snapshot().remoteProject);
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
  function closeMenu(restoreFocus = false) {
    const current = menu();
    const anchor = current === "layout" ? layoutAnchor() : current === "account" ? accountAnchor() : globalAnchor();
    menu.set(null);
    if (restoreFocus) anchor?.focus();
  }
  function focusMenuRow(id: string) {
    const entry = menuRows.get(id);
    if (!entry?.action()) return;
    focusedRow.set(id); entry.node?.focus();
    const scroll = menuScroll;
    if (!scroll || !entry.node) return;
    void Promise.all([entry.node.measure(), scroll.measure()]).then(([item, viewport]) => {
      if (!menu() || focusedRow() !== id || !item || !viewport || menuScroll !== scroll) return;
      const top = item.pagePosition.y - viewport.pagePosition.y;
      const bottom = top + item.bounds.size.height - viewport.bounds.size.height;
      if (top < 0 || bottom > 0) scroll.scrollTo({ y: Math.max(0, menuOffset + (top < 0 ? top : bottom)), animated: false });
    }).catch(() => {});
  }
  function moveMenuRow(step: number, edge?: "first" | "last") {
    const rows = [...menuRows].filter(([, entry]) => !!entry.action()).map(([id]) => id);
    if (!rows.length) return;
    const index = rows.indexOf(focusedRow() ?? "");
    const next = edge === "first" ? 0 : edge === "last" ? rows.length - 1 : index < 0 ? step > 0 ? 0 : rows.length - 1 : (index + step + rows.length) % rows.length;
    focusMenuRow(rows[next]);
  }
  effect(() => {
    if (!menu()) return;
    focusedRow.set(null); menuOffset = 0;
    const timer = setTimeout(() => moveMenuRow(1, "first"), 0);
    const unbind = keymap.bind({
      up: () => moveMenuRow(-1), down: () => moveMenuRow(1),
      home: () => moveMenuRow(1, "first"), end: () => moveMenuRow(-1, "last"),
      tab: () => moveMenuRow(1), "shift+tab": () => moveMenuRow(-1),
      enter: () => menuRows.get(focusedRow() ?? "")?.action()?.(),
      space: () => menuRows.get(focusedRow() ?? "")?.action()?.(),
      escape: () => closeMenu(true),
    }, { when: "always" });
    return () => { clearTimeout(timer); unbind(); focusedRow.set(null); menuScroll = undefined; };
  });
  function panel(name: string) {
    menu.set(null);
    const id = `panel:${name}`;
    props.action(() => props.snapshot().panes.some((pane) => pane.tabs.some((tab) => tab.id === id)) ? props.session.selectPanel(id) : props.session.openPanel(name));
  }
  function openGlobal(view: "dashboard" | "chat") {
    menu.set(null);
    void host.call("window.open", { kind: "global", view, start: view === "chat", activate: true }).catch(props.onError);
  }
  function activateItem(kind: string) {
    if (props.modalOpen?.()) return;
    if (kind === "layout") toggle("layout");
    else if (kind === "global_orchestrator") toggle("global");
    else if (kind === "codex_account") toggle("account");
    else if (kind === "usage") panel("usage");
    else if (kind === "project") panel("project_settings");
    else if (kind === "agent_activity") panel("shells");
    else if (kind === "worktree") panel("worktrees");
    else if (kind === "project_orchestrator") props.openProjectOrchestrator?.();
    else if (kind === "session_id" && selectedSession()) void Clipboard.writeText(selectedSession()!).catch(props.onError);
  }
  effect(() => {
    const kind = focusedItem();
    if (!kind || menu() || props.modalOpen?.() || !settings().enabled) return;
    return keymap.bind({ enter: () => activateItem(kind), space: () => activateItem(kind) });
  });
  const selectedSession = () => {
    const pane = activePane(props.snapshot());
    const id = pane?.activeTab;
    return id?.startsWith("shell:") ? id.slice(6) : id?.startsWith("remote:") ? id.split(":")[2] || undefined : undefined;
  };
  const textStyle = (kind = "") => ({ fontFamily: ["worktree", "live_sessions", "resources", "usage", "session_id"].includes(kind) ? theme().monoFamily : theme().family, fontSize: 9 * theme().scale,
    color: kind === "agent_activity" && (projectActivity()?.agents.working ?? 0) > 0 ? theme().palette.working : ["project", "session_id", "project_orchestrator"].includes(kind) ? theme().palette.cyan : theme().palette.muted });
  const accountList = (): AccountListLike => accounts() ?? NO_ACCOUNTS;
  const projectChoice = (): ProjectChoice | undefined => store()?.projects.find((project) => project.id === projectId())?.codexAccount;
  /** What Codex reported for the system default's home: the only name it has. */
  const systemLabel = () => reportedAccountLabel(usage()?.codex ?? [], systemHome(resolveSelection(null, accountList())));
  const segments = (): UsageSegment[] | null => {
    const mode = (props.settings?.()?.statusBar.usageMode ?? "current_chat") as UsageMode;
    const chat = props.nativeChat?.();
    if (mode === "current_chat" && chat) {
      const provider = chat.provider === "codex" ? "Codex" : chat.provider === "claude" ? "Claude" : chat.provider === "muse" ? "Muse" : "Chat";
      return [{ source: "chat_session", provider, figures: chat.usageLabel ? `Session ${chat.usageLabel}` : chat.provider === "unknown" ? "Loading" : "Session usage unavailable",
        freshness: chat.usageLabel ? "current" : chat.provider === "unknown" ? "loading" : "unavailable",
        note: `Session context and tokens. ${chat.usageDetail ?? "Subscription quota is not reported by this chat."}` }];
    }
    const list = accounts();
    const read = usage();
    if (!list || !read) return null;
    const shells = sessions() ?? [];
    return usageSegments({
      mode, focus: usageFocus(selectedSession() ? `shell:${selectedSession()}` : undefined, shells, list),
      projectDefault: projectSelection(projectChoice() ?? { source: "inherit" }, list.selectedId, list),
      accounts: mode === "all_accounts" ? knownCodexProfiles(list) : [],
      codex: read.codex, claude: read.claude.filter((entry) => entry.projectId === projectId()), now: read.now,
    });
  };
  const accountLabel = () => accountItemLabel(accounts()?.balancing ?? false, projectChoice(), accounts()?.selectedId, accountList(), systemLabel());
  function usageReadout(segment: UsageSegment, mode: UsageMode) {
    const windows = segment.windows?.filter((window) => Number.isFinite(window.usedPercent)).slice(0, 2) ?? [];
    const provider = segment.source === "claude" ? "Claude" : segment.source === "chat_session" ? segment.provider ?? "Chat" : segment.source === "grok_session" ? "Grok" : "Codex";
    const label = segment.source === "codex" && mode === "all_accounts" ? segment.account ?? provider
      : segment.source === "claude" && mode === "all_accounts" ? "Claude session"
      : `${provider}${segment.number === undefined ? "" : ` A${segment.number}`}`;
    return <View pointerEvents="none" accessible={false} style={{ flexDirection: "row", alignItems: "center", gap: space(5, theme().scale), flexShrink: 0 }}>
      <Text numberOfLines={1} style={{ ...textStyle("usage"), maxWidth: space(96, theme().scale) }}>{`${label}${segment.freshness === "stale" ? "*" : ""}`}</Text>
      {windows.length ? <View style={{ gap: space(1, theme().scale), opacity: segment.freshness === "stale" ? 0.55 : 1 }}>
        {windows.map((window) => {
          const remaining = Math.max(0, Math.min(100, 100 - window.usedPercent));
          return <View style={{ flexDirection: "row", alignItems: "center", gap: space(3, theme().scale), height: space(8, theme().scale) }}>
            <Text numberOfLines={1} style={{ ...textStyle("usage"), fontSize: 7 * theme().scale, width: space(16, theme().scale) }}>{window.label}</Text>
            <View style={{ width: space(30, theme().scale), height: space(3, theme().scale), borderRadius: 2, overflow: "hidden", backgroundColor: theme().palette.divider }}>
              <View style={{ width: `${remaining}%`, height: "100%", borderRadius: 2, backgroundColor: remaining < 15 ? theme().palette.gold : theme().palette.muted }} />
            </View>
            <Text style={{ ...textStyle("usage"), fontSize: 7 * theme().scale, width: space(24, theme().scale), textAlign: "right" }}>{`${whole(remaining)}%`}</Text>
          </View>;
        })}
      </View> : <Text numberOfLines={1} style={{ ...textStyle("usage"), maxWidth: space(250, theme().scale) }}>{Settings.sentenceCase(segment.figures)}</Text>}
    </View>;
  }
  function item(kind: string) {
    const key = kind.replaceAll("_", "-");
    const current = sessions()?.find((session) => session.id === selectedSession());
    const projectName = store()?.projects.find((project) => project.id === projectId())?.name ?? projectLabel();
    const worktreeId = props.snapshot().selectedWorktreeId ?? current?.worktreeId;
    const remote = props.snapshot().remoteProject ? props.remote?.() ?? { branch: null, live: null } : null;
    const branch = (remote ? remote.branch : store()?.worktrees.find((worktree) => worktree.id === worktreeId)?.branch) ?? "No worktree";
    const live = (remote ? remote.live : sessions()?.filter((session) => session.alive && session.projectId === projectId()).length) ?? "—";
    const readouts = kind === "usage" ? segments() : null;
    const mode = (props.settings?.()?.statusBar.usageMode ?? "current_chat") as UsageMode;
    const actionable = ["project", "worktree", "layout", "global_orchestrator", "session_id", "usage", "codex_account", "project_orchestrator", "agent_activity"].includes(kind);
    const disabled = (kind === "session_id" && !selectedSession()) || (kind === "project_orchestrator" && !props.openProjectOrchestrator);
    const labels: Record<string, string> = {
      project: projectName, worktree: branch, agent_activity: agentSummary(), live_sessions: `${live} live`,
      resources: props.snapshot().remoteProject ? "CPU — RAM —" : resourcesText(sessions() ?? [], metrics(), projectId()), usage: readouts ? usageItemText(mode, readouts) : "CODEX · LOADING", codex_account: accountLabel(), session_id: selectedSession()?.slice(0, 8) ?? "",
      global_orchestrator: props.settings?.()?.panelTabIcons ? "" : "Global", project_orchestrator: props.settings?.()?.panelTabIcons ? "" : "Project", layout: props.settings?.()?.panelTabIcons ? "" : "Layout",
    };
    const aria: Record<string, string> = {
      global_orchestrator: "Global: dashboard and chat", session_id: "Copy active session ID", project: `Project ${projectName}`,
      project_orchestrator: `Open ${projectName} orchestrator${orchestratorState(kind) ? ` · ${orchestratorState(kind)}` : ""}`,
      usage: readouts ? usageAria(readouts) : `Usage ${labels.usage}`, codex_account: `New Codex sessions: ${labels.codex_account}`,
    };
    return <View testID={kind === "global_orchestrator" ? "top-orchestrator" : `status-item-${key}`}
      ref={(node) => { if (kind === "layout") layoutAnchor.set(node); if (kind === "global_orchestrator") globalAnchor.set(node); if (kind === "codex_account") accountAnchor.set(node); }}
      accessibilityLabel={aria[kind] ?? labels[kind]}
      accessibilityHint={kind === "usage" && readouts ? usageHint(readouts) : kind === "codex_account" && menu() !== "account" ? "New Codex sessions in this project · click to choose" : undefined}
      tooltip={kind === "global_orchestrator" ? `Global · dashboard ⌘⌥G · chat ⌘⇧O${orchestratorState(kind) ? `\n${orchestratorState(kind)}` : ""}`
        : kind === "project_orchestrator" ? `${projectName} orchestrator · ⌘⌥O${orchestratorState(kind) ? `\n${orchestratorState(kind)}` : ""}`
        : kind === "usage" && readouts ? usageHint(readouts) : undefined}
      role={actionable ? "button" : "text"} disabled={disabled}
      focusable={actionable && !disabled} onFocus={() => focusedItem.set(kind)} onBlur={() => { if (focusedItem() === kind) focusedItem.set(null); }}
      hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.panelActive }}
      style={{ flexDirection: "row", alignItems: "center", gap: space(kind === "usage" ? 10 : 4, theme().scale), borderRadius: 4, minHeight: space(18, theme().scale),
        paddingHorizontal: kind === "global_orchestrator" || kind === "project_orchestrator" ? space(4, theme().scale) : 0,
        backgroundColor: kind === "global_orchestrator" && menu() === "global" ? theme().palette.panelActive : undefined,
        maxWidth: space(kind === "project" ? 240 : kind === "worktree" ? 220 : kind === "agent_activity" ? 250 : kind === "codex_account" ? 300 : 1000, theme().scale) }}
      onPress={() => activateItem(kind)}>
      {kind === "layout" && <Icon name="layout" size={11 * theme().scale} color={theme().palette.muted} />}
      {(kind === "global_orchestrator" || kind === "project_orchestrator") && <>
        {orchestratorMark(orchestratorState(kind)) && <Text style={{ ...textStyle(), color: ["working", "running", "starting", "waiting"].includes(orchestratorState(kind) ?? "") ? theme().palette.working : theme().palette.muted }}>{orchestratorMark(orchestratorState(kind)).trim()}</Text>}
        <Icon name={kind === "global_orchestrator" ? "globe" : "project-orchestrator"} size={12 * theme().scale} color={menu() === "global" && kind === "global_orchestrator" ? theme().palette.cyan : theme().palette.muted} />
      </>}
      {kind === "usage" && readouts ? readouts.map((segment) => usageReadout(segment, mode))
        : labels[kind] && <Text numberOfLines={1} style={textStyle(kind)}>{Settings.sentenceCase(labels[kind])}</Text>}
    </View>;
  }
  function row(id: string, label: string, shortcut: string, action?: () => void, detail?: string) {
    return <MenuRow id={id} label={label} shortcut={shortcut} action={action} detail={detail} />;
  }
  function MenuRow(props: { id: string; label: string; shortcut: string; action?: () => void; detail?: string }) {
    const entry = { node: undefined as YNode | undefined, action: () => props.action };
    menuRows.set(props.id, entry);
    cleanup(() => { if (menuRows.get(props.id) === entry) menuRows.delete(props.id); });
    return <View testID={props.id} ref={(node) => { entry.node = node; }} accessibilityLabel={props.label} role="button" disabled={!props.action} onPress={() => props.action?.()}
      focusable={!!props.action} onFocus={() => focusedRow.set(props.id)}
      hoverStyle={{ backgroundColor: props.action ? theme().palette.panelActive : "transparent" }} focusStyle={{ backgroundColor: theme().palette.panelActive }}
      style={{ paddingHorizontal: 12, paddingVertical: 6, gap: 3 }}>
      <View style={{ flexDirection: "row", justifyContent: "space-between", gap: 10 }}>
        <Text style={{ flex: 1, minWidth: 0, fontFamily: theme().family, fontSize: 11 * theme().scale, color: props.action ? theme().palette.text : theme().palette.muted }}>{props.label}</Text>
        <Text style={{ flexShrink: 0, fontFamily: theme().monoFamily, fontSize: 11 * theme().scale, color: theme().palette.muted }}>{props.shortcut}</Text>
      </View>
      {props.detail && <Text style={{ fontFamily: theme().family, fontSize: 9 * theme().scale, color: theme().palette.muted }}>{props.detail}</Text>}
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
    {settings().enabled && <View testID="status-bar" style={{ position: "relative", height: space(STATUS_METRICS.height, theme().scale), flexShrink: 0, flexDirection: "row", alignItems: "center", paddingHorizontal: space(STATUS_METRICS.padding, theme().scale), gap: 8, backgroundColor: theme().palette.panel }}>
      <View testID="status-bar-separator" pointerEvents="none" accessible={false}
        style={{ position: "absolute", top: 0, left: 0, right: 0, height: 1, backgroundColor: theme().palette.divider, opacity: 0.55 }} />
      <Scroll testID="status-left" horizontal={true} style={{ flexShrink: 0, maxWidth: items("right").length ? "40%" : "100%" }} contentStyle={{ flexDirection: "row", alignItems: "center", gap: space(STATUS_METRICS.gap, theme().scale) }}>
        {items("left").map((entry) => item(entry.kind))}
      </Scroll>
      <View style={{ flex: 1, minWidth: 4 }} />
      <Scroll testID="status-right" horizontal={true} style={{ flexShrink: 1, minWidth: 0 }} contentStyle={{ flexDirection: "row", alignItems: "center", gap: space(STATUS_METRICS.gap, theme().scale) }}>
        {items("right").map((entry) => item(entry.kind))}
      </Scroll>
    </View>}
    {menu() && <Overlay testID={`${menu()}-menu`} anchor={menu() === "layout" ? layoutAnchor() : menu() === "account" ? accountAnchor() : globalAnchor()} placement="top" align="end" offset={3} margin={6}
      onCancel={() => closeMenu(true)} onPressOutside={() => closeMenu()}
      style={{ width: space(menu() === "layout" ? 296 : menu() === "account" ? 320 : 300, theme().scale), maxWidth: "100%", maxHeight: "80%", backgroundColor: theme().palette.panel, borderColor: theme().palette.divider, borderWidth: 1, borderRadius: 8 }}>
      <Scroll ref={(node) => { menuScroll = node; }} onScroll={(event) => { menuOffset = event.offset.y; }} style={{ flexShrink: 1, minHeight: 0 }} contentStyle={{ paddingVertical: 4 }}>
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
      </Scroll>
    </Overlay>}
  </>;
}
