import { ErrorBoundary, Text, View, cleanup, colorScheme, effect, host, signal } from "nsh";
import { Appearance, Chat, Orchestrators, Sessions, type AppearanceDto, type SettingsDto } from "riwork/native";
import { DEFAULT_THEME, type InterfaceTheme } from "../appearance";
import { ChatTab } from "../chat/ChatTab";
import { ChatFailure } from "../chat/ChatFailure";
import { chatCopy, type GlobalOrchestrator, type Plan } from "../dashboard/model";
import { LocalTerminal, type TerminalColors } from "../Terminal";

export interface ChatControl {
  /** Only an explicit Start may make one (the button, ⌘⇧O, the dashboard's Chat). */
  start: () => void;
  starting: () => boolean;
  plan: () => Plan | null;
}

type Shown = { kind: "chat"; id: string } | { kind: "terminal"; shellId: string } | null;

export const TERMINAL_LABEL = "Terminal orchestrator: the global orchestrator runs in a terminal";
export const TERMINAL_NOTE = "RiWork keeps this terminal as it is; a chat takes over only after you end it.";

/** The Chat view: the global orchestrator there is, as the last read says.
 * Unknown changes nothing; none lets go of what was shown (SHELL-053..057). */
export function GlobalChat(props: {
  visible: () => boolean;
  global: () => GlobalOrchestrator;
  theme?: () => InterfaceTheme;
  settings?: () => SettingsDto | null;
  startNow?: boolean;
  onControl: (control: ChatControl) => void;
  onNotice: (text: string) => void;
  onApiKeySettings?: () => void;
}) {
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const shown = signal<Shown>(null);
  const visited = signal(props.visible());
  effect(() => { if (props.visible()) visited.set(true); });
  const starting = signal(false);
  const plan = signal<Plan | null>(null);
  const appearance = signal<AppearanceDto | null>(null);
  let alive = true;
  cleanup(() => { alive = false; });
  const subscription = Appearance.subscribe((event) => appearance.set(event.data));
  cleanup(() => subscription.close());
  // The Start button's words follow Settings; reading the plan starts nothing.
  effect(() => {
    props.settings?.();
    void Orchestrators.plan(null).then((next) => { if (alive) plan.set(next as Plan); }).catch(() => {});
  });

  effect(() => {
    if (!props.visible() || starting()) return;
    const global = props.global();
    const current = shown();
    if (global.kind === "chat" && !(current?.kind === "chat" && current.id === global.chat_id)) shown.set({ kind: "chat", id: global.chat_id });
    else if (global.kind === "terminal" && !(current?.kind === "terminal" && current.shellId === global.shell_id)) shown.set({ kind: "terminal", shellId: global.shell_id });
    else if (global.kind === "none" && current) shown.set(null);
  });

  function start() {
    if (starting()) return;
    const global = props.global();
    if (shown() || global.kind === "chat" || (global.kind === "terminal")) {
      if (!shown() && global.kind === "chat") shown.set({ kind: "chat", id: global.chat_id });
      if (!shown() && global.kind === "terminal") shown.set({ kind: "terminal", shellId: global.shell_id });
      return;
    }
    starting.set(true);
    void Orchestrators.start(null).then((made) => {
      if (!alive) return;
      if (made.fallback) props.onNotice(made.fallback);
      if (made.kind === "chat" && made.chatId) shown.set({ kind: "chat", id: made.chatId });
      else if (made.kind === "terminal" && made.shellId) shown.set({ kind: "terminal", shellId: made.shellId });
    }).catch((error: unknown) => { if (alive) props.onNotice(String(error).replace(/^Error: /, "")); })
      .finally(() => { if (alive) starting.set(false); });
  }
  props.onControl({ start, starting, plan });
  if (props.startNow) start();

  function terminalColors(): TerminalColors {
    const value = appearance();
    return ((value ? colorScheme() === "dark" ? value.terminalDark : value.terminalLight : null) ?? { source: "ghostty" }) as TerminalColors;
  }
  async function openWorker(id: string, kind: "shell" | "chat") {
    try {
      const projectId = kind === "shell" ? (await Sessions.list()).sessions.find((row) => row.id === id)?.projectId : (await Chat.list()).find((row) => row.id === id)?.projectId;
      if (!alive) return;
      if (!projectId) { props.onNotice("This worker has no project workspace to open."); return; }
      await host.call("window.route", { projectId, step: kind === "chat" ? { kind: "chat", chatId: id } : { kind: "showShell", shellId: id } });
    } catch (error) { if (alive) props.onNotice(String(error)); }
  }
  const palette = () => theme().palette;
  const font = (size: number) => ({ fontFamily: theme().family, fontSize: size * theme().scale });
  return <>{visited() && <View testID="global-chat" style={{ flex: 1, minHeight: 0, display: props.visible() ? "flex" : "none" }}>
    {shown()?.kind === "chat" ?
      <ErrorBoundary fallback={(error, retry) => <ChatFailure error={error} retry={retry} theme={theme} />}><ChatTab chatId={(shown() as { id: string }).id} visible={props.visible()} theme={theme}
        onLink={(path) => { void host.call("system.openWith", { path }).catch((error) => props.onNotice(String(error))); }}
        onOpenWorker={(id, kind) => { void openWorker(id, kind); }}
        onApiKeySettings={props.onApiKeySettings}
        onClose={() => { shown.set(null); }} /></ErrorBoundary> :
      shown()?.kind === "terminal" ?
        <View style={{ flex: 1, minHeight: 0 }}>
          <View testID="global-terminal-label" style={{ paddingHorizontal: 12, paddingVertical: 6, gap: 2, backgroundColor: palette().panel }}>
            <Text style={{ ...font(11), color: palette().text }}>{TERMINAL_LABEL}</Text>
            <Text style={{ ...font(10), color: palette().muted }}>{TERMINAL_NOTE}</Text>
          </View>
          <View style={{ flex: 1, minHeight: 0 }}>
            <LocalTerminal shellId={(shown() as { shellId: string }).shellId} visible={props.visible()} pinned={false} theme={theme}
              focusOnAttach={true} colors={terminalColors()} frozen={false}
              onFocus={() => {}} onAttached={() => {}} onLink={(event) => {
                void host.call(event.kind === "folder" ? "system.revealInFinder" : "system.openWith", { path: event.target }).catch((error) => props.onNotice(String(error)));
              }} onNotice={props.onNotice} />
          </View>
        </View> :
        <StartCard copy={chatCopy(props.global(), plan(), starting())} theme={theme} onPress={start} />}
  </View>}</>;
}

function StartCard(props: { copy: ReturnType<typeof chatCopy>; theme: () => InterfaceTheme; onPress: () => void }) {
  const palette = () => props.theme().palette;
  const font = (size: number) => ({ fontFamily: props.theme().family, fontSize: size * props.theme().scale });
  // Primary when it starts one, secondary when it opens one, disabled otherwise.
  const primary = () => props.copy.enabled && props.copy.starts;
  return <View testID="global-chat-card" style={{ flex: 1, alignItems: "center", justifyContent: "center", padding: 24 }}>
    <View style={{ maxWidth: 460, gap: 10, alignItems: "center" }}>
      <Text testID="global-chat-status" style={{ ...font(12), color: palette().text }}>{props.copy.status}</Text>
      {props.copy.note && <Text testID="global-chat-note" style={{ ...font(11), color: palette().muted, textAlign: "center" }}>{props.copy.note}</Text>}
      <View testID="global-chat-start" role="button" accessibilityLabel={props.copy.action} disabled={!props.copy.enabled}
        onPress={props.copy.enabled ? props.onPress : undefined}
        style={{ paddingHorizontal: 14, paddingVertical: 6, borderRadius: 6, borderWidth: 1,
          borderColor: primary() ? palette().cyan : palette().divider,
          backgroundColor: primary() ? palette().panelActive : "transparent", opacity: props.copy.enabled ? 1 : 0.5 }}>
        <Text style={{ ...font(12), color: primary() ? palette().cyan : palette().text }}>{props.copy.action}</Text>
      </View>
    </View>
  </View>;
}
