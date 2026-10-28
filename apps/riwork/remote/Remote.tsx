import { Scroll, Text, cleanup, effect, signal, type YNode } from "nsh";
import { Dialog } from "nsh/base";
import { Button, Input, View } from "../SettingsFocus";
import { DEFAULT_THEME, space, type InterfaceTheme } from "../appearance";
import { Icon } from "../Icon";
import {
  EMPTY_REMOTE, REMOTE_HISTORY_NOTE, checkPairingLink, filteredFolders, linkPresentation,
  maskedPairingLink, projectConnectionNote, projectNameError, remoteErrorNotice, remoteHostLabel, remoteProjectKey,
  remoteStrip, shellOpenRefusal,
  type PairingLink, type PairRequest, type RemoteDefaults, type RemoteLists, type RemoteOrder, type RemotePromptKind,
  type RemoteShell, type RemoteSnapshot, type RemoteWants,
} from "./model";

/** Required dependency injection keeps fixture screens entirely in memory. The
 * workspace passes riwork/native Remote here; nothing imports a transport. */
export interface RemoteApi {
  subscribe(wants: RemoteWants, listener: (event: { data: RemoteSnapshot; rev?: number }) => void): { close(): void };
  defaults(): Promise<RemoteDefaults>;
  addHost(link: string, label: string): Promise<void>;
  removeHost(host: string): Promise<void>;
  pairDesktop(request: PairRequest): Promise<PairingLink>;
  createProject(host: string, name: string): Promise<{ key: string; name: string }>;
  dismissFailure(host: string, project: string | null): Promise<void>;
}
export function createRemoteController(options: {
  api: RemoteApi;
  wants: () => RemoteWants;
  theme?: () => InterfaceTheme;
  notice?: (message: string) => void;
  modalBlocked?: () => boolean;
  beforePrompt?: () => void;
  afterPrompt?: () => void;
  copyText: (text: string) => Promise<void> | void;
}) {
  const data = signal<RemoteSnapshot>(EMPTY_REMOTE), pending = signal(false), prompt = signal<RemotePromptKind | null>(null);
  const confirmation = signal<string | null>(null), error = signal<string | null>(null), copied = signal(false);
  const name = signal(""), link = signal(""), relay = signal(""), routes = signal(""), paired = signal<PairingLink | null>(null);
  const defaults = signal<RemoteDefaults>({ name: "", relay: "", routes: "" });
  const wantsSignature = signal<string | null>(null);
  let previousWants = "";
  let generation = 0;
  let lastRevision = 0;
  const theme = options.theme ?? (() => DEFAULT_THEME);
  const report = (message: string) => { error.set(message); options.notice?.(message); };
  const message = (value: unknown) => String(value).replace(/^(?:Error: )?(?:invalid|io|refused|broken|unavailable|conflict): /u, "");
  effect(() => {
    const value = options.wants();
    const signature = JSON.stringify({ folders: value.folders, settings: value.settings,
      collapsed: [...new Set(value.collapsed)].sort(), tabHosts: [...new Set(value.tabHosts)].sort(),
      projectKey: value.projectKey, worktrees: value.worktrees, tasks: value.tasks, shells: value.shells,
      tabs: [...new Set(value.tabs ?? [])].sort(), order: value.order ?? "", descending: !!value.descending });
    if (signature !== previousWants) { previousWants = signature; wantsSignature.set(signature); }
  });
  effect(() => {
    const signature = wantsSignature(); if (!signature) return;
    const wants = JSON.parse(signature) as RemoteWants;
    let subscription: { close(): void } | undefined;
    try {
      subscription = options.api.subscribe(wants, (event) => {
        if (event.rev !== undefined && event.rev < lastRevision) return;
        if (event.rev !== undefined) lastRevision = event.rev;
        data.set(event.data);
        if (!event.data.hosts.some((host) => host.id === confirmation())) confirmation.set(null);
      });
    } catch (value) { report(message(value)); }
    return () => subscription?.close();
  });
  function clearSecret() { link.set(""); paired.set(null); copied.set(false); }
  function finishPrompt() { prompt.set(null); options.afterPrompt?.(); }
  cleanup(() => { generation++; clearSecret(); });
  function close() {
    if (pending() || !prompt()) return;
    generation++; clearSecret(); finishPrompt(); error.set(null);
  }
  async function open(kind: RemotePromptKind) {
    if (pending() || prompt() || options.modalBlocked?.()) return;
    options.beforePrompt?.(); // workspace closes menus/freezes retained terminals
    generation++; const asked = generation;
    confirmation.set(null); error.set(null); clearSecret();
    name.set(kind.kind === "pair" ? defaults().name : "");
    relay.set(defaults().relay); routes.set(defaults().routes); prompt.set(kind);
    if (kind.kind === "pair") {
      try {
        const value = await options.api.defaults();
        if (asked !== generation || prompt()?.kind !== "pair") return;
        defaults.set(value);
        // A delayed defaults read must not overwrite edits already made.
        if (!name()) name.set(value.name);
        if (!relay()) relay.set(value.relay);
        if (!routes()) routes.set(value.routes);
      } catch (value) { if (asked === generation) report(message(value)); }
    }
  }
  async function submit() {
    const current = prompt(); if (!current || pending() || paired()) return;
    const validation = current.kind === "add" ? checkPairingLink(link()) : current.kind === "project" ? projectNameError(name()) :
      (!name().trim() || !relay().trim() || !routes().trim() ? "Fill in the name, the relay and the routes file." : null);
    if (validation) { error.set(validation); return; }
    if (current.kind === "project") {
      // As GPUI: the prompt closes at once; the folder shows "creating…" and any
      // failure with its Dismiss; only success says so. The window stays put.
      const host = current.host, project = name().trim();
      generation++; clearSecret(); finishPrompt(); error.set(null);
      void options.api.createProject(host, project).then((result) => options.notice?.(`Created ${result.name}`), () => {});
      return;
    }
    const asked = generation;
    pending.set(true); error.set(null);
    try {
      if (current.kind === "add") {
        const label = name().trim(), secret = link().trim();
        await options.api.addHost(secret, label);
        if (asked === generation) { clearSecret(); options.notice?.(label ? `Added host ${label}` : "Host added"); finishPrompt(); }
      } else {
        const request = { name: name().trim(), relay: relay().trim(), routes: routes().trim() };
        const result = await options.api.pairDesktop(request);
        if (asked === generation) {
          defaults.set(request); paired.set({ link: result.link, masked: maskedPairingLink(result.link) });
        }
      }
    } catch (value) {
      if (asked === generation) {
        // Even a fake backend must not echo the pending clipboard secret.
        const secret = link().trim();
        const text = message(value);
        report(secret ? text.split(secret).join("[pairing link hidden]") : text.replace(/riwork:\/\/pair\?\S+/gu, "[pairing link hidden]"));
      }
    } finally { if (asked === generation) pending.set(false); }
  }
  async function remove(host: string) {
    if (pending() || prompt()) return;
    if (confirmation() !== host) { confirmation.set(host); return; }
    pending.set(true); error.set(null);
    try { await options.api.removeHost(host); confirmation.set(null); }
    catch (value) { report(message(value)); }
    finally { pending.set(false); }
  }
  async function copy() {
    const value = paired(); if (!value || pending()) return;
    pending.set(true);
    try { await options.copyText(value.link); copied.set(true); }
    catch { error.set("Cannot copy the pairing link."); }
    finally { pending.set(false); }
  }
  // The opener brings up an open tab before refusing a chat or an ended shell.
  function openShell(host: string, shell: RemoteShell, action: (host: string, shell: RemoteShell) => void) { action(host, shell); }
  function dismiss(host: string, project: string | null) { void options.api.dismissFailure(host, project).catch((value) => report(message(value))); }
  return { data, theme, pending, prompt, confirmation, error, copied, name, link, relay, routes, paired, close, open, submit, remove, copy, openShell, dismiss };
}
export type RemoteController = ReturnType<typeof createRemoteController>;
function font(c: RemoteController, size = 11, color = c.theme().palette.text) {
  return { fontFamily: c.theme().family, fontSize: size * c.theme().scale, color };
}
function action(c: RemoteController, id: string, label: string, callback: (() => void) | undefined, tone?: string, accessibilityLabel = label) {
  return <Button testID={id} accessibilityLabel={accessibilityLabel} disabled={!callback} onPress={callback}
    style={{ paddingHorizontal: 8, paddingVertical: 4 }}><Text style={font(c, 10, tone)}>{label}</Text></Button>;
}
function status(c: RemoteController, id: string, link: RemoteHostLink) {
  const state = linkPresentation(link);
  return <Text testID={id} accessibilityLabel={state.text} style={font(c, 10, c.theme().palette[state.tone])}>{`${state.dot} ${state.text}`}</Text>;
}
type RemoteHostLink = RemoteSnapshot["hosts"][number]["link"];

/** One short line; the full text (paths, the build recipe) behind Details. */
function RemoteError(props: { controller: RemoteController; text: string }) {
  const c = props.controller, open = signal(false);
  const notice = () => remoteErrorNotice(props.text);
  return <View testID="remote-hosts-error" accessibilityLabel={props.text} style={{ gap: 4 }}>
    <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
      <Text numberOfLines={2} style={{ ...font(c, 10, c.theme().palette.gold), flex: 1, minWidth: 0 }}>{notice().summary}</Text>
      {notice().detail && action(c, "remote-hosts-error-details", open() ? "Hide details" : "Details", () => open.set(!open()))}
    </View>
    {open() && notice().detail && <Text testID="remote-hosts-error-detail" style={font(c, 10, c.theme().palette.muted)}>{notice().detail}</Text>}
  </View>;
}

/** AppSettings' remote slot shares the page scroll and tracked focus scope. */
export function RemoteSettings(props: { controller: RemoteController }) {
  const c = props.controller;
  return <View testID="remote-settings" style={{ gap: 8, padding: 14 }}>
    <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between" }}>
      <Text style={font(c, 13)}>Other Macs</Text>
      <Text testID="remote-encrypted" style={font(c, 10, c.theme().palette.cyan)}>{c.pending() ? "Working" : "Encrypted"}</Text>
    </View>
    {!c.data().hosts.length && <Text style={font(c, 11, c.theme().palette.muted)}>No other Mac is paired yet.</Text>}
    {c.data().hosts.map((host) => <View testID={`remote-host-${host.id}`} style={{ flexDirection: "row", gap: 8, alignItems: "center", justifyContent: "space-between" }}>
      <View style={{ flex: 1, gap: 3 }}><Text style={font(c)}>{host.label}</Text>{status(c, `remote-host-status-${host.id}`, host.link)}</View>
      {action(c, `remote-remove-${host.id}`, c.pending() && c.confirmation() === host.id ? "Removing…" : c.confirmation() === host.id ? "Confirm remove" : "Remove",
        c.pending() ? undefined : () => void c.remove(host.id), c.confirmation() === host.id ? c.theme().palette.gold : undefined)}
    </View>)}
    {(c.data().hostsError || (!c.prompt() && c.error())) && <RemoteError controller={c} text={c.data().hostsError ?? c.error()!} />}
    <View style={{ flexDirection: "row", gap: 8 }}>
      {action(c, "remote-pair", "Pair another Mac", c.pending() ? undefined : () => void c.open({ kind: "pair" }))}
      {action(c, "remote-add", "Add host", c.pending() ? undefined : () => void c.open({ kind: "add" }))}
    </View>
  </View>;
}

/** Append after local folders. Search expands matches without changing saved collapse state. */
export function RemoteProjects(props: {
  controller: RemoteController; query?: () => string; selected?: () => string | null; order?: () => RemoteOrder;
  collapsed?: () => ReadonlySet<string>; onToggleFolder?: (key: string) => void; onSelectProject: (key: string) => void;
}) {
  const c = props.controller;
  return <View testID="remote-projects" style={{ gap: 2 }}>
    {filteredFolders(c.data(), props.query?.() ?? "", props.order?.()).map((folder) => {
      const key = `remote:${folder.host}`;
      const collapsed = !props.query?.().trim() && props.collapsed?.().has(key);
      return <View testID={`remote-folder-${folder.host}`} style={{ gap: 2 }}>
        <View tooltip="A paired Mac. Its projects open here like local ones." style={{ flexDirection: "row", alignItems: "center", gap: 3, paddingHorizontal: space(6, c.theme().scale), height: space(30, c.theme().scale) }}>
          <View testID={`remote-folder-toggle-${folder.host}`} role="button" focusable={true} accessibilityLabel={folder.label} accessibilityState={{ expanded: !collapsed }} onPress={() => props.onToggleFolder?.(key)}
            style={{ flex: 1, minWidth: 0, height: "100%", flexDirection: "row", alignItems: "center", gap: space(6, c.theme().scale) }}>
            <Icon name={collapsed ? "right" : "down"} size={10 * c.theme().scale} color={c.theme().palette.muted} />
            <Text testID={`remote-folder-status-${folder.host}`} tooltip={linkPresentation(folder.link).text} accessibilityLabel={linkPresentation(folder.link).text} style={font(c, 10, c.theme().palette[linkPresentation(folder.link).tone])}>{linkPresentation(folder.link).dot}</Text>
            <Text numberOfLines={1} style={{ ...font(c, 10, c.theme().palette.muted), fontWeight: "600", flex: 1, minWidth: 0 }}>{folder.label}</Text>
            {folder.creating && <Text style={font(c, 9, c.theme().palette.muted)}>creating…</Text>}
            <Text style={font(c, 9, c.theme().palette.muted)}>{String(folder.count).padStart(2, "0")}</Text>
          </View>
          {folder.link === "online" && <Button testID={`remote-new-project-${folder.host}`} accessibilityLabel="New project on this Mac" disabled={c.pending() || folder.creating} onPress={() => void c.open({ kind: "project", host: folder.host, label: folder.label })}
            style={{ width: space(24, c.theme().scale), height: space(24, c.theme().scale), borderRadius: 12, alignItems: "center", justifyContent: "center" }} hoverStyle={{ backgroundColor: c.theme().palette.panelActive }}>
            <Icon name="add" size={13 * c.theme().scale} color={c.theme().palette.muted} />
          </Button>}
        </View>
        {folder.failure && <View style={{ flexDirection: "row", alignItems: "center" }}><Text style={font(c, 10, c.theme().palette.gold)}>{folder.failure}</Text>{action(c, `remote-dismiss-project-${folder.host}`, "Dismiss", () => c.dismiss(folder.host, null))}</View>}
        {!collapsed && folder.note && <Text testID={`remote-folder-note-${folder.host}`} style={{ ...font(c, 10, folder.link === "offline" ? c.theme().palette.gold : c.theme().palette.muted), paddingLeft: space(22, c.theme().scale), paddingVertical: 5 }}>{folder.note}</Text>}
        {!collapsed && folder.projects.map((project) => <Button testID={`remote-project-${project.key}`} accessibilityLabel={`${project.name} on ${folder.label}`}
          selected={props.selected?.() === project.key} onPress={() => props.onSelectProject(project.key)}
          style={{ opacity: project.dimmed ? 0.5 : 1, marginLeft: space(16, c.theme().scale), paddingHorizontal: space(8, c.theme().scale), paddingVertical: space(8, c.theme().scale), gap: 3, borderRadius: space(5, c.theme().scale), backgroundColor: props.selected?.() === project.key ? c.theme().palette.panelActive : "transparent" }} hoverStyle={{ backgroundColor: c.theme().palette.panelActive }}>
          <Text numberOfLines={1} style={font(c)}>{project.name}</Text><Text numberOfLines={1} style={font(c, 9, c.theme().palette.muted)}>{project.stats}</Text>
        </Button>)}
      </View>;
    })}
  </View>;
}

const UNAVAILABLE_WHAT: Record<string, string> = {
  files: "Files", preview: "Preview", memo: "Preview", project_settings: "Project settings", projectSettings: "Project settings", schedules: "Schedules", usage: "Usage",
};
/** A real remote namespace must be gated before constructing any local controller.
 * As GPUI's panels::unavailable: what is missing, and why, centered. */
export function RemoteUnavailable(props: { hostLabel: string; panel: string; theme?: () => InterfaceTheme }) {
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const text = (size: number, color: string) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color });
  return <View testID={`remote-unavailable-${props.panel}`} style={{ flex: 1, alignItems: "center", justifyContent: "center", padding: 16 * theme().scale, gap: 6 * theme().scale, backgroundColor: theme().palette.panel }}>
    <Text style={text(11, theme().palette.text)}>{`Not available for a project on ${props.hostLabel}`}</Text>
    <Text style={{ ...text(10, theme().palette.muted), maxWidth: 360 * theme().scale }}>{`${UNAVAILABLE_WHAT[props.panel] ?? "This panel"} works with this Mac's own projects. Select one of them to use it.`}</Text>
  </View>;
}

export function RemoteProjectPanel(props: {
  controller: RemoteController; projectKey: () => string; panel: "worktrees" | "tasks" | "shells";
  query?: () => string; selectedWorktree?: () => string | null;
  onQuery?: (query: string) => void; onSearch?: (node: YNode) => (() => void) | void;
  onNewShell?: () => void;
  onWorktree: (id: string) => void; onTask: (id: string) => void; onShell: (host: string, shell: RemoteShell) => void;
}) {
  const c = props.controller;
  const read = () => c.data().projects.find((project) => project.key === props.projectKey());
  const matches = (...parts: (string | null)[]) => !props.query?.().trim() || parts.filter(Boolean).join("\n").toLocaleLowerCase().includes(props.query!().trim().toLocaleLowerCase());
  const worktrees = () => read()?.worktrees.filter((w) => matches(w.id, w.branch, w.path)) ?? [];
  const tasks = () => read()?.tasks.filter((task) => matches(task.id, task.title, task.details, task.status)) ?? [];
  const shells = () => read()?.shells.filter((shell) => matches(shell.id, shell.display, shell.cwd, shell.harness, shell.alive ? "live" : "exited")) ?? [];
  const count = () => props.panel === "worktrees" ? worktrees().length : props.panel === "tasks" ? tasks().length : shells().length;
  const total = () => read()?.[props.panel].length ?? 0;
  const title = props.panel === "shells" ? "Sessions" : props.panel === "tasks" ? "Tasks" : "Worktrees";
  const noun = props.panel === "shells" ? "session" : props.panel === "tasks" ? "task" : "worktree";
  const row = () => ({ paddingHorizontal: space(8, c.theme().scale), paddingVertical: space(8, c.theme().scale), gap: 3, borderRadius: space(5, c.theme().scale) });
  let search: YNode | undefined;
  let removeSearch: (() => void) | void;
  cleanup(() => removeSearch?.());
  const listNote = (project: RemoteLists) => {
    const loading = props.panel === "worktrees" ? project.worktreesLoading : props.panel === "tasks" ? project.tasksLoading : project.shellsLoading;
    const error = props.panel === "worktrees" ? project.worktreesError : props.panel === "tasks" ? project.tasksError : project.shellsError;
    return error ? <Text style={font(c, 10, c.theme().palette.gold)}>{error}</Text> : loading ? <Text style={font(c, 10, c.theme().palette.muted)}>Loading…</Text> : null;
  };
  return <View testID={`remote-${props.panel}-panel`} style={{ flex: 1, minHeight: 0, backgroundColor: c.theme().palette.panel }}>
    <View style={{ height: space(46, c.theme().scale), paddingHorizontal: space(12, c.theme().scale), flexDirection: "row", alignItems: "center", gap: 8 }}>
      <View style={{ flex: 1, minWidth: 0, gap: 1 }}>
        <Text numberOfLines={1} style={{ ...font(c, 13), fontWeight: "600" }}>{title}</Text>
        <Text numberOfLines={1} style={font(c, 10, c.theme().palette.muted)}>{`${count() === total() ? total() : `${count()} of ${total()}`} ${noun}${total() === 1 ? "" : "s"}${read() ? ` · ${read()!.projectName}` : ""}`}</Text>
      </View>
      {props.panel === "shells" && props.onNewShell && <Button accessibilityLabel="New shell" disabled={read()?.link !== "online" || read()?.unpaired} onPress={props.onNewShell}
        style={{ width: 24, height: 24, borderRadius: 12, alignItems: "center", justifyContent: "center" }} hoverStyle={{ backgroundColor: c.theme().palette.panelActive }}>
        <Icon name="add" size={13 * c.theme().scale} color={c.theme().palette.muted} />
      </Button>}
    </View>
    <View style={{ paddingHorizontal: space(12, c.theme().scale), paddingBottom: space(8, c.theme().scale) }}>
      <View onPress={() => search?.focus()} style={{ flexDirection: "row", alignItems: "center", gap: 5, paddingHorizontal: space(9, c.theme().scale), height: space(28, c.theme().scale), borderRadius: space(14, c.theme().scale), backgroundColor: c.theme().palette.panelActive }}>
        <Icon name="search" size={13 * c.theme().scale} color={c.theme().palette.muted} />
        <Input testID={`search:remote-${props.panel}`} accessibilityLabel={`Search ${title.toLowerCase()}`} placeholder="Search  ⌘F" value={props.query?.() ?? ""} onChange={(value) => props.onQuery?.(value)}
          ref={(node) => { search = node; removeSearch?.(); removeSearch = props.onSearch?.(node); }} style={{ ...font(c), flex: 1, minWidth: 0, padding: 0, backgroundColor: "transparent" }} />
        {!!props.query?.() && <Button accessibilityLabel="Clear search" onPress={() => { props.onQuery?.(""); search?.focus(); }} style={{ width: 20, height: 20, alignItems: "center", justifyContent: "center" }}>
          <Icon name="close" size={11 * c.theme().scale} color={c.theme().palette.muted} />
        </Button>}
      </View>
    </View>
    <Scroll style={{ flex: 1, minHeight: 0 }} contentStyle={{ paddingHorizontal: space(6, c.theme().scale), paddingBottom: 8, gap: 6 }}>
    {!read() ? <Text style={font(c, 11, c.theme().palette.muted)}>Loading…</Text> : <>
      {projectConnectionNote(read()!) && <Text testID="remote-project-connection" style={font(c, 10, read()!.link === "offline" || read()!.unpaired ? c.theme().palette.gold : c.theme().palette.muted)}>{projectConnectionNote(read()!)}</Text>}
      {!read()!.unpaired && <>
        {listNote(read()!)}
        {props.panel === "shells" && <>
          <Text style={font(c, 10, c.theme().palette.muted)}>{REMOTE_HISTORY_NOTE}</Text>
          {read()!.creating && <Text style={font(c, 10, c.theme().palette.cyan)}>Starting a terminal…</Text>}
          {read()!.failure && <View style={{ flexDirection: "row" }}><Text style={font(c, 10, c.theme().palette.gold)}>{read()!.failure}</Text>{action(c, "remote-dismiss-shell", "Dismiss", () => c.dismiss(read()!.host, read()!.project))}</View>}
        </>}
        <View testID="remote-project-rows" style={{ gap: 5, opacity: read()!.link === "offline" ? 0.5 : 1 }}>
          {props.panel === "worktrees" && worktrees().map((w) => <Button testID={`remote-worktree-${w.id}`} accessibilityLabel={w.branch}
            selected={props.selectedWorktree?.() === w.id} onPress={() => props.onWorktree(w.id)} style={row()} hoverStyle={{ backgroundColor: c.theme().palette.panelActive }} selectedStyle={{ backgroundColor: c.theme().palette.panelActive }}>
            <Text style={font(c, 11, props.selectedWorktree?.() === w.id ? c.theme().palette.cyan : c.theme().palette.text)}>{`${w.primary ? "◆" : "◇"} ${w.branch}`}</Text><Text style={font(c, 10, c.theme().palette.muted)}>{w.path}</Text>
          </Button>)}
          {props.panel === "tasks" && tasks().map((task) => <Button testID={`remote-task-${task.id}`} accessibilityLabel={task.title} onPress={() => props.onTask(task.id)} style={row()} hoverStyle={{ backgroundColor: c.theme().palette.panelActive }}>
            <Text style={font(c)}>{task.title}</Text><Text style={font(c, 10, c.theme().palette.muted)}>{`${task.status} · @ ${read()!.worktrees.find((w) => w.id === task.worktreeId)?.branch ?? "unassigned"}`}</Text>
          </Button>)}
          {props.panel === "shells" && shells().map((shell) => <Button testID={`remote-shell-${shell.id}`} accessibilityLabel={shell.display} onPress={() => c.openShell(read()!.host, shell, props.onShell)} style={row()} hoverStyle={{ backgroundColor: c.theme().palette.panelActive }}>
            <Text style={font(c)}>{`${shell.orchestrator ? shell.projectId ? "P·ORCH" : "G·ORCH" : shell.id.slice(0, 8)} · ${shell.harness ?? "shell"} · ${shell.alive ? "live" : "exited"}`}</Text><Text style={font(c, 10, c.theme().palette.muted)}>{shell.cwd}</Text>
          </Button>)}
        </View>
        {!count() && !read()![props.panel === "worktrees" ? "worktreesLoading" : props.panel === "tasks" ? "tasksLoading" : "shellsLoading"] && <View style={{ padding: 24, gap: 8, alignItems: "center" }}>
          <Icon name={props.query?.().trim() ? "search" : "folder"} size={22 * c.theme().scale} color={c.theme().palette.muted} />
          <Text style={font(c, 11, c.theme().palette.muted)}>{props.query?.().trim() ? "No matches" : `No ${title.toLowerCase()}`}</Text>
        </View>}
      </>}
    </>}
    </Scroll>
  </View>;
}

const PROMPT_DESCRIPTION = {
  add: "On the other Mac, open Settings → Remote → Pair another Mac, and paste the link it shows.",
  pair: "Makes a one-time link. Whoever has it can control this Mac's terminals once it is redeemed.",
  paired: "Paste it on the other Mac under Settings → Remote → Add host. It works once, for about ten minutes, and is not saved here. Add the new route to your relay's routes file and restart the relay before using it.",
  project: "The project is made in the host's default projects folder.",
} as const;

/** Render above normal content, alongside the workspace's modal lifecycle.
 * Texts, fields and buttons as GPUI's RemotePrompt; the busy line is separate. */
export function RemotePrompt(props: { controller: RemoteController }) {
  const c = props.controller;
  const fields: (YNode | undefined)[] = [];
  const kind = () => c.prompt()!.kind;
  const title = () => kind() === "add" ? "Add host" : kind() === "pair" ? "Pair another Mac" : `New project on ${(c.prompt() as { label?: string } | null)?.label ?? "this Mac"}`;
  const space = (value: number) => value * c.theme().scale;
  const inputStyle = () => ({ ...font(c), padding: 8, borderWidth: 1, borderColor: c.theme().palette.divider, borderRadius: 5 });
  // A pairing starts on the first field that is still empty.
  function focusFirst() {
    const values = kind() === "add" ? [c.link(), c.name()] : kind() === "pair" ? [c.name(), c.relay(), c.routes()] : [c.name()];
    fields[Math.max(0, values.findIndex((value) => !value))]?.focus();
  }
  const label = (text: string) => <Text style={font(c, 9, c.theme().palette.muted)}>{text}</Text>;
  function field(index: number, id: string, name: string, placeholder: string, value: () => string, change: (value: string) => void, masked = false) {
    return <View style={{ gap: space(4) }}>
      {label(name)}
      <Input testID={id} accessibilityLabel={name} placeholder={placeholder} masked={masked} value={value()} readonly={c.pending()}
        ref={(node) => { fields[index] = node; }} onChange={change} onSubmit={() => void c.submit()} style={inputStyle()} />
    </View>;
  }
  function button(id: string, text: string, primary: boolean, press: (() => void) | undefined, focus = false) {
    const disabled = c.pending() || !press;
    return <Button testID={id} accessibilityLabel={text.replace(/\s+↵$/u, "")} disabled={disabled} onPress={press} ref={focus ? (node) => { node.focus(); } : undefined}
      style={{ paddingHorizontal: space(12), paddingVertical: space(8), borderWidth: 1, borderColor: primary ? c.theme().palette.cyan : c.theme().palette.panel,
        backgroundColor: primary ? c.theme().palette.panelActive : c.theme().palette.panel }} focusStyle={{ borderColor: c.theme().palette.focus }}>
      <Text style={font(c, 11, disabled ? c.theme().palette.muted : primary ? c.theme().palette.cyan : c.theme().palette.muted)}>{text}</Text>
    </Button>;
  }
  return <>{c.prompt() && <Dialog testID="remote-prompt" animation="none" closeOnBackdropPress={false} closeOnEscape={!c.pending()} onOpen={focusFirst}
    onOpenChange={(open) => { if (!open) c.close(); }} backdropStyle={{ backgroundColor: "#00000070" }}
    style={{ width: space(480), maxWidth: "95%", padding: space(18), gap: space(14), backgroundColor: c.theme().palette.panel, borderWidth: 1, borderColor: c.theme().palette.magenta }}>
    <Text style={font(c, 11, c.theme().palette.cyan)}>{title()}</Text>
    <Text style={font(c, 10, c.theme().palette.muted)}>{PROMPT_DESCRIPTION[c.paired() ? "paired" : kind()]}</Text>
    {c.paired() ? <Text testID="remote-pairing-mask" numberOfLines={1} style={{ ...font(c, 11, c.theme().palette.cyan), padding: space(10), backgroundColor: c.theme().palette.bg, borderWidth: 1, borderColor: c.theme().palette.divider }}>{c.paired()!.masked}</Text> : <>
      {kind() === "add" && <>
        {field(0, "remote-pairing-link", "Pairing link", "Paste the link (⌘V)", c.link, (value) => c.link.set(value), true)}
        {field(1, "remote-prompt-name", "Name (optional)", "How this Mac is shown here", c.name, (value) => c.name.set(value))}
      </>}
      {kind() === "pair" && <>
        {field(0, "remote-prompt-name", "Name of the other Mac", "MacBook", c.name, (value) => c.name.set(value))}
        {field(1, "remote-prompt-relay", "Relay", "wss://relay.example.com/v1/ws", c.relay, (value) => c.relay.set(value))}
        {field(2, "remote-prompt-routes", "Relay routes file", "relay-routes.json", c.routes, (value) => c.routes.set(value))}
      </>}
      {kind() === "project" && field(0, "remote-prompt-name", "Project name", "My App", c.name, (value) => c.name.set(value))}
    </>}
    {c.pending() && !c.paired() && <Text testID="remote-prompt-busy" style={font(c, 11, c.theme().palette.cyan)}>{kind() === "pair" ? "Making a pairing link…" : "Pairing…"}</Text>}
    {c.error() && <Text testID="remote-prompt-error" style={font(c, 11, c.theme().palette.gold)}>{c.error()}</Text>}
    <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: space(10) }}>
      {c.paired() ? <>
        {button("remote-copy-link", c.copied() ? "Copied" : "Copy link", false, () => void c.copy(), true)}
        {button("remote-done", "Done  ↵", true, c.close)}
      </> : <>
        {button("remote-cancel", "Cancel", false, c.close)}
        {button("remote-submit", kind() === "pair" ? "Make link  ↵" : kind() === "add" ? "Add host  ↵" : "Create  ↵", true, () => void c.submit())}
      </>}
    </View>
  </Dialog>}</>;
}

/** Optional remote terminal chrome; native reattach is injected by its owner.
 * Above (never over) the terminal, as GPUI's remote_strip draws it. */
export function RemoteStrip(props: { label: string; link: RemoteHostLink; ended: boolean; onReconnect: () => void; theme?: () => InterfaceTheme }) {
  const theme = props.theme ?? (() => DEFAULT_THEME), strip = () => remoteStrip(props.label, props.link, props.ended);
  const space = (value: number) => value * theme().scale, tone = () => theme().palette[strip()!.tone];
  return <>{strip() && <View testID="remote-terminal-strip" style={{ flexShrink: 0, height: space(24), flexDirection: "row", alignItems: "center", gap: space(10), paddingHorizontal: space(10),
    backgroundColor: theme().palette.panelActive, borderBottomWidth: 1, borderColor: tone() }}>
    <Text numberOfLines={1} style={{ flex: 1, minWidth: 0, fontFamily: theme().family, fontSize: space(10), color: tone() }}>{strip()!.text}</Text>
    {strip()!.reconnect && <Button testID="remote-reconnect" accessibilityLabel="Reconnect remote session" onPress={props.onReconnect}
      style={{ paddingHorizontal: space(8), paddingVertical: space(2), borderWidth: 1, borderColor: theme().palette.divider }} hoverStyle={{ backgroundColor: theme().palette.divider }}>
      <Text style={{ fontFamily: theme().family, fontSize: space(10), color: theme().palette.text }}>Reconnect</Text>
    </Button>}
  </View>}</>;
}

export { remoteProjectKey };
