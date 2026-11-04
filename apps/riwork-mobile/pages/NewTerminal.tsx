/** New terminal: a plain shell or an agent (Codex, Claude, Grok, Muse) in the project or one of its
 * worktrees (`shell.create`), the kind remembered (`riwork.newTerminal.kind`, through `platform.prefs`).
 * One request at a time, never repeated: an answer that was lost says the terminal may have been made and
 * reads the tabs again. A chat or an orchestrator is made in Swift's sheet (`nav.present` `newChat`). */
import { Scroll, Text, View, cleanup, effect, signal } from "nsh";
import { Dialog } from "nsh/base";
import { PortError } from "../../riwork/client/ports";
import type { HostShell, HostWorktree, ShellKind } from "../../riwork/client/types";
import type { MobileApp } from "../app_state";
import { Symbol } from "../Symbol";
import type { Kit } from "../ui";

export const SHELL_KINDS: readonly { kind: ShellKind; label: string; detail: string; symbol: string }[] = [
  { kind: "shell", label: "Shell", detail: "A plain terminal", symbol: "terminal" },
  { kind: "codex", label: "Codex", detail: "OpenAI's agent in a terminal", symbol: "sparkles" },
  { kind: "claude", label: "Claude", detail: "Anthropic's agent in a terminal", symbol: "sparkle" },
  { kind: "grok", label: "Grok", detail: "xAI's agent in a terminal", symbol: "bolt" },
  { kind: "muse", label: "Muse", detail: "Muse's agent in a terminal", symbol: "wand.and.stars" },
];
/** A remembered kind this form offers; the chat and orchestrator kinds (Swift's sheet) fall back to a shell. */
export function shellKind(value: unknown): ShellKind {
  return SHELL_KINDS.some((entry) => entry.kind === value) ? value as ShellKind : "shell";
}

export function NewTerminalForm(props: {
  app: MobileApp; ui: Kit; host: string; projectId: string; projectName: string; open: boolean;
  onClose: () => void; onCreated: (shell: HostShell) => void; onUncertain: () => void;
}) {
  const { app, ui } = props;
  const { Action, Section, Divider, Toggle, palette, pt, target, text } = ui;
  const workspace = app.services(props.host).workspace!;
  const kind = signal<ShellKind>("shell"), unrestricted = signal(false), busy = signal(false);
  const worktrees = signal<HostWorktree[]>([]), target_ = signal<string | null>(null);
  const message = signal<string | null>(null);
  let alive = true;
  cleanup(() => { alive = false; });
  const features = () => app.hosts().find((info) => info.id === props.host)?.features ?? [];
  const asSettings = () => features().includes("shell_create_as_settings");
  const chats = () => features().includes("chat") || features().includes("orchestrator_create");
  const connected = () => app.link(props.host).state === "connected";
  effect(() => {
    if (!props.open) return;
    message.set(null); unrestricted.set(false); target_.set(null);
    void app.device.prefs.get("newTerminal.kind").then((value) => { if (alive) kind.set(shellKind(value)); }).catch(() => {});
    void workspace.worktrees(props.projectId).then((value) => { if (alive) worktrees.set(value.filter((tree) => !tree.isPrimary)); }).catch(() => worktrees.set([]));
  });
  function close() { if (!busy()) props.onClose(); }
  async function create() {
    if (busy() || !connected()) return;
    busy.set(true); message.set(null);
    const chosen = kind(), tree = target_();
    try {
      const shell = await workspace.createShell({
        kind: chosen, ...(tree ? { worktreeId: tree } : { projectId: props.projectId }),
        ...(chosen !== "shell" ? (asSettings() ? { asSettings: true } : { unrestricted: unrestricted() }) : {}),
      });
      busy.set(false);
      void app.device.prefs.set("newTerminal.kind", chosen).catch(() => {});
      props.onClose();
      props.onCreated(shell);
    } catch (cause) {
      busy.set(false);
      const failure = PortError.from(cause);
      if (failure.code === "uncertain") {
        message.set("The Mac may have opened this terminal, but its answer was lost. The tabs were read again: look for it there before opening another.");
        props.onUncertain();
      } else if (failure.code === "unsupported") message.set("Update Somtus on the Mac to open terminals from here.");
      else message.set(failure.message);
    }
  }
  function moreKinds() {
    if (busy()) return;
    props.onClose();
    void app.present({ name: "newChat", host: props.host, projectId: props.projectId });
  }
  return <Dialog open={props.open} presentation="pageSheet" onOpenChange={(open) => { if (!open) close(); }} closeOnSwipe={!busy()} closeOnEscape={!busy()}
    style={{ backgroundColor: palette().bg }}>
    <View style={{ flexDirection: "row", alignItems: "center", gap: pt(8), padding: pt(12) }}>
      <Action label="Cancel" compact={true} disabled={busy()} action={close} />
      <Text role="header" style={{ ...text(17), fontWeight: "600", flex: 1, textAlign: "center" }}>New terminal</Text>
      <Action testID="new-terminal-create" label={busy() ? "Opening…" : "Open"} compact={true} prominent={true} disabled={busy() || !connected()} action={() => void create()} />
    </View>
    <Scroll style={{ flex: 1 }} contentStyle={{ padding: pt(16), paddingTop: 0, gap: pt(18) }}>
      <Text style={text(13, "muted")}>{`In ${props.projectName} on ${app.hostName(props.host)}`}</Text>
      <Section title="Kind">
        {SHELL_KINDS.map((entry, index) => <>
          {index > 0 && <Divider />}
          <View testID={`new-terminal-kind:${entry.kind}`} role="radio" accessibilityLabel={`${entry.label}. ${entry.detail}`} accessibilityState={{ checked: kind() === entry.kind }}
            disabled={busy()} onPress={() => kind.set(entry.kind)} activeStyle={{ backgroundColor: palette().panelActive }}
            style={{ minHeight: target(), padding: pt(12), flexDirection: "row", alignItems: "center", gap: pt(12) }}>
            <Symbol name={entry.symbol} color={palette().cyan} size={pt(19)} />
            <View style={{ flex: 1, gap: pt(2) }}><Text style={text(15)}>{entry.label}</Text><Text style={text(12, "muted")}>{entry.detail}</Text></View>
            {kind() === entry.kind && <Symbol name="checkmark" color={palette().cyan} size={pt(16)} />}
          </View>
        </>)}
      </Section>
      {worktrees().length > 0 && <Section title="Where">
        {[{ id: null as string | null, label: "Project", detail: "Its main worktree" },
          ...worktrees().map((tree) => ({ id: tree.id as string | null, label: tree.branch, detail: tree.path }))].map((entry, index) => <>
          {index > 0 && <Divider />}
          <View role="radio" accessibilityLabel={`${entry.label}. ${entry.detail}`} accessibilityState={{ checked: target_() === entry.id }}
            disabled={busy()} onPress={() => target_.set(entry.id)} activeStyle={{ backgroundColor: palette().panelActive }}
            style={{ minHeight: target(), padding: pt(12), flexDirection: "row", alignItems: "center", gap: pt(12) }}>
            <View style={{ flex: 1, gap: pt(2) }}><Text style={text(15)}>{entry.label}</Text><Text numberOfLines={1} style={text(12, "muted")}>{entry.detail}</Text></View>
            {target_() === entry.id && <Symbol name="checkmark" color={palette().cyan} size={pt(16)} />}
          </View>
        </>)}
      </Section>}
      {kind() !== "shell" && (asSettings()
        ? <Text style={text(12, "muted")}>The agent starts unrestricted or not as the Mac's Agent terminals setting says.</Text>
        : <Toggle label="Unrestricted" detail="The agent runs without asking for approval." checked={unrestricted()} disabled={busy()} onChange={(value) => unrestricted.set(value)} />)}
      {message() && <Text style={text(13, "error")}>{message()!}</Text>}
      {!connected() && <Text style={text(13, "muted")}>Connect to this desktop to open a terminal.</Text>}
      {chats() && <Section>
        <Action label="New chat or orchestrator…" symbol="bubble.left.and.text.bubble.right" disabled={busy() || !connected()} action={moreKinds} />
      </Section>}
    </Scroll>
  </Dialog>;
}
