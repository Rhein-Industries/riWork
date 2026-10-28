import { Text, View, cleanup, colorScheme, effect, signal } from "nsh";
import { Appearance, Orchestrators, type AppearanceDto, type SettingsDto } from "riwork/native";
import { DEFAULT_THEME, type InterfaceTheme } from "../appearance";
import { ChatTab } from "../chat/ChatTab";
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
}) {
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const shown = signal<Shown>(null);
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
  const palette = () => theme().palette;
  const font = (size: number) => ({ fontFamily: theme().family, fontSize: size * theme().scale });
  return <>{props.visible() && <View testID="global-chat" style={{ flex: 1, minHeight: 0 }}>
    {shown()?.kind === "chat" ?
      <ChatTab chatId={(shown() as { id: string }).id} visible={true} theme={theme}
        onClose={() => { shown.set(null); }} /> :
      shown()?.kind === "terminal" ?
        <View style={{ flex: 1, minHeight: 0 }}>
          <View testID="global-terminal-label" style={{ paddingHorizontal: 12, paddingVertical: 6, gap: 2, backgroundColor: palette().panel }}>
            <Text style={{ ...font(11), color: palette().text }}>{TERMINAL_LABEL}</Text>
            <Text style={{ ...font(10), color: palette().muted }}>{TERMINAL_NOTE}</Text>
          </View>
          <View style={{ flex: 1, minHeight: 0 }}>
            <LocalTerminal shellId={(shown() as { shellId: string }).shellId} visible={props.visible()} pinned={false}
              focusOnAttach={true} colors={terminalColors()} frozen={false}
              onFocus={() => {}} onAttached={() => {}} onLink={() => {}} onNotice={props.onNotice} />
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
