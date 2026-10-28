import { Text, cleanup, effect, signal, type YNode } from "nsh";
import { Dialog } from "nsh/base";
import { Button, Input, View } from "../SettingsFocus";
import { DEFAULT_THEME, type InterfaceTheme } from "../appearance";
import {
  EMPTY_REMOTE, REMOTE_HISTORY_NOTE, checkPairingLink, filteredFolders, linkPresentation,
  maskedPairingLink, projectConnectionNote, projectNameError, remoteErrorNotice, remoteHostLabel, remoteProjectKey,
  remoteStrip, shellOpenRefusal,
  type PairingLink, type PairRequest, type RemoteDefaults, type RemoteLists, type RemotePromptKind,
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
  onProjectCreated?: (key: string) => void;
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
      projectKey: value.projectKey, worktrees: value.worktrees, tasks: value.tasks, shells: value.shells });
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
    const asked = generation;
    pending.set(true); error.set(null);
    try {
      if (current.kind === "add") {
        const label = name().trim(), secret = link().trim();
        await options.api.addHost(secret, label);
        if (asked === generation) { clearSecret(); options.notice?.(label ? `Added host ${label}` : "Host added"); finishPrompt(); }
      } else if (current.kind === "pair") {
        const request = { name: name().trim(), relay: relay().trim(), routes: routes().trim() };
        const result = await options.api.pairDesktop(request);
        if (asked === generation) {
          defaults.set(request); paired.set({ link: result.link, masked: maskedPairingLink(result.link) });
        }
      } else {
        const result = await options.api.createProject(current.host, name().trim());
        if (asked === generation) { options.notice?.(`Created ${result.name}`); finishPrompt(); options.onProjectCreated?.(result.key); }
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
  function openShell(host: string, shell: RemoteShell, action: (host: string, shell: RemoteShell) => void) {
    const refusal = shellOpenRefusal(shell, remoteHostLabel(data(), host));
    if (refusal) options.notice?.(refusal); else action(host, shell);
  }
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
  controller: RemoteController; query?: () => string; selected?: () => string | null;
  collapsed?: () => ReadonlySet<string>; onToggleFolder?: (key: string) => void; onSelectProject: (key: string) => void;
}) {
  const c = props.controller;
  return <View testID="remote-projects" style={{ gap: 6 }}>
    {filteredFolders(c.data(), props.query?.() ?? "").map((folder) => {
      const key = `remote:${folder.host}`;
      const collapsed = !props.query?.().trim() && props.collapsed?.().has(key);
      return <View testID={`remote-folder-${folder.host}`} style={{ gap: 4 }}>
        <View tooltip="A paired Mac. Its projects open here like local ones." style={{ flexDirection: "row", alignItems: "center", gap: 6, paddingHorizontal: 8, paddingVertical: 5 }}>
          {action(c, `remote-folder-toggle-${folder.host}`, `${collapsed ? "▸" : "▾"} ${folder.label}`, () => props.onToggleFolder?.(key))}
          {status(c, `remote-folder-status-${folder.host}`, folder.link)}
          <Text style={font(c, 10, c.theme().palette.muted)}>{folder.creating ? "creating…" : `${folder.count}`}</Text>
          {action(c, `remote-new-project-${folder.host}`, "+", folder.link === "online" && !c.pending() && !folder.creating ? () => void c.open({ kind: "project", host: folder.host, label: folder.label }) : undefined, undefined, "New project on this Mac")}
        </View>
        {folder.failure && <View style={{ flexDirection: "row", alignItems: "center" }}><Text style={font(c, 10, c.theme().palette.gold)}>{folder.failure}</Text>{action(c, `remote-dismiss-project-${folder.host}`, "Dismiss", () => c.dismiss(folder.host, null))}</View>}
        {folder.note && <Text testID={`remote-folder-note-${folder.host}`} style={font(c, 10, folder.link === "offline" ? c.theme().palette.gold : c.theme().palette.muted)}>{folder.note}</Text>}
        {!collapsed && folder.projects.map((project) => <Button testID={`remote-project-${project.key}`} accessibilityLabel={`${project.name} on ${folder.label}`}
          selected={props.selected?.() === project.key} onPress={() => props.onSelectProject(project.key)}
          style={{ opacity: project.dimmed ? 0.5 : 1, paddingHorizontal: 16, paddingVertical: 5, gap: 3, backgroundColor: props.selected?.() === project.key ? c.theme().palette.panelActive : "transparent" }}>
          <Text style={font(c)}>{project.name}</Text><Text style={font(c, 10, c.theme().palette.muted)}>{project.stats}</Text>
        </Button>)}
      </View>;
    })}
  </View>;
}

/** A real remote namespace must be gated before constructing any local controller. */
export function RemoteUnavailable(props: { hostLabel: string; panel: string; theme?: () => InterfaceTheme }) {
  const theme = props.theme ?? (() => DEFAULT_THEME);
  return <View testID={`remote-unavailable-${props.panel}`} style={{ flex: 1, padding: 16, gap: 6 }}>
    <Text style={{ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.muted }}>{`Not available for a project on ${props.hostLabel}`}</Text>
  </View>;
}

export function RemoteProjectPanel(props: {
  controller: RemoteController; projectKey: () => string; panel: "worktrees" | "tasks" | "shells";
  query?: () => string; selectedWorktree?: () => string | null;
  onWorktree: (id: string) => void; onTask: (id: string) => void; onShell: (host: string, shell: RemoteShell) => void;
}) {
  const c = props.controller;
  const read = () => c.data().projects.find((project) => project.key === props.projectKey());
  const matches = (...parts: (string | null)[]) => !props.query?.().trim() || parts.filter(Boolean).join("\n").toLocaleLowerCase().includes(props.query!().trim().toLocaleLowerCase());
  const listNote = (project: RemoteLists) => {
    const loading = props.panel === "worktrees" ? project.worktreesLoading : props.panel === "tasks" ? project.tasksLoading : project.shellsLoading;
    const error = props.panel === "worktrees" ? project.worktreesError : props.panel === "tasks" ? project.tasksError : project.shellsError;
    return error ? <Text style={font(c, 10, c.theme().palette.gold)}>{error}</Text> : loading ? <Text style={font(c, 10, c.theme().palette.muted)}>Loading…</Text> : null;
  };
  return <View testID={`remote-${props.panel}-panel`} style={{ gap: 6, padding: 10 }}>
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
          {props.panel === "worktrees" && read()!.worktrees.filter((w) => matches(w.id, w.branch, w.path)).map((w) => <Button testID={`remote-worktree-${w.id}`} accessibilityLabel={w.branch}
            selected={props.selectedWorktree?.() === w.id} onPress={() => props.onWorktree(w.id)} style={{ gap: 3 }}>
            <Text style={font(c, 11, props.selectedWorktree?.() === w.id ? c.theme().palette.cyan : c.theme().palette.text)}>{`${w.primary ? "◆" : "◇"} ${w.branch}`}</Text><Text style={font(c, 10, c.theme().palette.muted)}>{w.path}</Text>
          </Button>)}
          {props.panel === "tasks" && read()!.tasks.filter((task) => matches(task.id, task.title, task.details, task.status)).map((task) => <Button testID={`remote-task-${task.id}`} accessibilityLabel={task.title} onPress={() => props.onTask(task.id)} style={{ gap: 3 }}>
            <Text style={font(c)}>{task.title}</Text><Text style={font(c, 10, c.theme().palette.muted)}>{`${task.status} · @ ${read()!.worktrees.find((w) => w.id === task.worktreeId)?.branch ?? "unassigned"}`}</Text>
          </Button>)}
          {props.panel === "shells" && read()!.shells.filter((shell) => matches(shell.id, shell.display, shell.cwd, shell.harness, shell.alive ? "live" : "exited")).map((shell) => <Button testID={`remote-shell-${shell.id}`} accessibilityLabel={shell.display} onPress={() => c.openShell(read()!.host, shell, props.onShell)} style={{ gap: 3 }}>
            <Text style={font(c)}>{`${shell.orchestrator ? shell.projectId ? "P·ORCH" : "G·ORCH" : shell.id.slice(0, 8)} · ${shell.harness ?? "shell"} · ${shell.alive ? "live" : "exited"}`}</Text><Text style={font(c, 10, c.theme().palette.muted)}>{shell.cwd}</Text>
          </Button>)}
        </View>
      </>}
    </>}
  </View>;
}

/** Render above normal content, alongside the workspace's modal lifecycle. */
export function RemotePrompt(props: { controller: RemoteController }) {
  const c = props.controller;
  let first: YNode | undefined;
  const title = () => c.prompt()?.kind === "add" ? "Add host" : c.prompt()?.kind === "pair" ? "Pair another Mac" : `New project on ${(c.prompt() as { label?: string } | null)?.label ?? "this Mac"}`;
  const inputStyle = () => ({ ...font(c), padding: 8, borderWidth: 1, borderColor: c.theme().palette.divider, borderRadius: 5 });
  return <>{c.prompt() && <Dialog testID="remote-prompt" animation="none" closeOnBackdropPress={false} closeOnEscape={!c.pending()} onOpen={() => first?.focus()}
    onOpenChange={(open) => { if (!open) c.close(); }} backdropStyle={{ backgroundColor: "#00000070" }}
    style={{ width: 480, maxWidth: "95%", padding: 18, gap: 10, backgroundColor: c.theme().palette.panel, borderRadius: 10 }}>
    <Text style={font(c, 14)}>{title()}</Text>
    {c.paired() ? <>
      <Text testID="remote-pairing-mask" style={font(c, 11, c.theme().palette.cyan)}>{c.paired()!.masked}</Text>
      <Text style={font(c, 10, c.theme().palette.muted)}>Paste it on the other Mac under Settings → Remote → Add host. It works once, for about ten minutes, and is not saved here. Add the new route to your relay's routes file and restart the relay before using it.</Text>
      <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
        <Button testID="remote-copy-link" ref={(node) => { node.focus(); }} disabled={c.pending()} onPress={() => void c.copy()}><Text style={font(c)}>{c.copied() ? "Copied" : "Copy link"}</Text></Button>
        {action(c, "remote-done", "Done  ↵", c.pending() ? undefined : c.close)}
      </View>
    </> : <>
      {c.prompt()!.kind === "add" && <>
        <Text style={font(c, 10, c.theme().palette.muted)}>Pairing link</Text>
        <Input testID="remote-pairing-link" accessibilityLabel="Pairing link" masked={true} autoFocus={true} value={c.link()} readonly={c.pending()} placeholder="Paste the link (⌘V)" ref={(node) => { first = node; }} onChange={(value) => c.link.set(value)} onSubmit={() => void c.submit()} style={inputStyle()} />
      </>}
      <Text style={font(c, 10, c.theme().palette.muted)}>{c.prompt()!.kind === "add" ? "Name (optional)" : c.prompt()!.kind === "pair" ? "Name of the other Mac" : "Project name"}</Text>
      <Input testID="remote-prompt-name" accessibilityLabel={c.prompt()!.kind === "add" ? "Name (optional)" : c.prompt()!.kind === "pair" ? "Name of the other Mac" : "Project name"} value={c.name()} readonly={c.pending()}
        autoFocus={c.prompt()!.kind !== "add"} ref={(node) => { if (c.prompt()?.kind !== "add") first = node; }} onChange={(value) => c.name.set(value)} onSubmit={() => void c.submit()} style={inputStyle()} />
      {c.prompt()!.kind === "pair" && <>
        <Text style={font(c, 10, c.theme().palette.muted)}>Relay</Text>
        <Input testID="remote-prompt-relay" accessibilityLabel="Relay" placeholder="wss://relay.example.com/v1/ws" value={c.relay()} readonly={c.pending()} onChange={(value) => c.relay.set(value)} onSubmit={() => void c.submit()} style={inputStyle()} />
        <Text style={font(c, 10, c.theme().palette.muted)}>Routes file</Text>
        <Input testID="remote-prompt-routes" accessibilityLabel="Routes file" value={c.routes()} readonly={c.pending()} onChange={(value) => c.routes.set(value)} onSubmit={() => void c.submit()} style={inputStyle()} />
      </>}
      {c.error() && <Text testID="remote-prompt-error" style={font(c, 10, c.theme().palette.gold)}>{c.error()}</Text>}
      <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
        {action(c, "remote-cancel", "Cancel", c.pending() ? undefined : c.close)}
        {action(c, "remote-submit", c.pending() ? c.prompt()!.kind === "pair" ? "Making a pairing link…" : c.prompt()!.kind === "add" ? "Pairing…" : "Creating…" : c.prompt()!.kind === "pair" ? "Make link" : c.prompt()!.kind === "add" ? "Add host" : "Create project", c.pending() ? undefined : () => void c.submit())}
      </View>
    </>}
  </Dialog>}</>;
}

/** Optional remote terminal chrome; native reattach is injected by its owner. */
export function RemoteStrip(props: { label: string; link: RemoteHostLink; ended: boolean; onReconnect: () => void; theme?: () => InterfaceTheme }) {
  const theme = props.theme ?? (() => DEFAULT_THEME), strip = () => remoteStrip(props.label, props.link, props.ended);
  return <>{strip() && <View testID="remote-terminal-strip" style={{ height: 24 * theme().scale, flexDirection: "row", alignItems: "center", justifyContent: "space-between", paddingHorizontal: 10 * theme().scale }}>
    <Text style={{ fontFamily: theme().family, fontSize: 10 * theme().scale, color: theme().palette[strip()!.tone] }}>{strip()!.text}</Text>
    {strip()!.reconnect && <Button testID="remote-reconnect" accessibilityLabel="Reconnect remote session" onPress={props.onReconnect}><Text style={{ fontSize: 10 * theme().scale, color: theme().palette.magenta }}>Reconnect</Text></Button>}
  </View>}</>;
}

export { remoteProjectKey };
