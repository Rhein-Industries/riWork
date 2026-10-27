// NSH 0.3 consolidates the nsh/native declarations into its nsh export.
import { Text, View, nativeComponent, signal, type YNode } from "nsh";

export type TerminalTarget = { kind: "local"; shellId: string } | { kind: "remote"; desktopId: string; shellId: string };
export type TerminalColors = { source: "ghostty" } | { source: "override"; background: string; foreground: string; palette: string[] };
export type TerminalState = "detached" | "attaching" | "attached" | "failed" | "claimed" | "reconnecting" | "ended" | "gone";
export interface TerminalProps {
  target: TerminalTarget;
  visible: boolean;
  pinned: boolean;
  focusOnAttach: boolean;
  colors: TerminalColors;
  frozen: boolean;
}
export interface TerminalEvents {
  state: { state: TerminalState; message?: string; attempts?: number };
  focusChange: { focused: boolean };
  linkOpen: { action: "open" | "edit"; kind: "url" | "file" | "folder"; target: string; line?: number; column?: number };
  drop: { paths: string[]; images: string[]; accepted: boolean; reason?: string };
  inputRefused: { reason: string };
}
/** Frozen schema 1. Input, drawing and link detection stay in the native view. */
export const Terminal = nativeComponent<TerminalProps, TerminalEvents>("riwork.terminal", {
  props: ["target", "visible", "pinned", "focusOnAttach", "colors", "frozen"],
  events: ["state", "focusChange", "linkOpen", "drop", "inputRefused"],
});
export type TerminalCommand = "focus" | "blur" | "reattach" | "copy" | "paste" | "selectAll";

export function terminalStateMessage(state: TerminalEvents["state"]): string {
  if (state.message) return state.message;
  switch (state.state) {
    case "attaching": return "Connecting to session…";
    case "failed": return "Could not attach to this session.";
    case "claimed": return "This session is displayed in another window.";
    case "gone": return "This session has ended.";
    case "ended": return "Session ended.";
    case "reconnecting": return "Reconnecting…";
    default: return "";
  }
}

interface LocalTerminalProps {
  shellId: string;
  visible: boolean;
  pinned: boolean;
  focusOnAttach: boolean;
  colors: TerminalColors;
  frozen: boolean;
  onMount?: () => void;
  onFocus: () => void;
  onAttached: () => void;
  onLink: (event: TerminalEvents["linkOpen"]) => void;
  onNotice: (message: string) => void;
}

export function LocalTerminal(props: LocalTerminalProps) {
  const state = signal<TerminalEvents["state"]>({ state: "detached" });
  let view: YNode | undefined;
  return <View testID={`terminal-panel:${props.shellId}`} style={{ flex: 1, minHeight: 0 }}>
    <Terminal testID={`terminal:${props.shellId}`} ref={(node) => { view = node; props.onMount?.(); }}
      style={{ flex: 1, minHeight: 0 }} target={{ kind: "local", shellId: props.shellId }}
      visible={props.visible} pinned={props.pinned} focusOnAttach={props.focusOnAttach}
      colors={props.colors} frozen={props.frozen}
      onState={(next) => { state.set(next); if (next.state === "attached") props.onAttached(); }}
      onFocusChange={(event) => { if (event.focused) props.onFocus(); }}
      onLinkOpen={props.onLink}
      onDrop={(event) => { if (event.accepted) props.onFocus(); else if (event.reason) props.onNotice(event.reason); }}
      onInputRefused={(event) => props.onNotice(event.reason)} />
    {terminalStateMessage(state()) !== "" && <View testID={`terminal-state:${props.shellId}`} style={{ padding: 8, gap: 8, flexDirection: "row", backgroundColor: "#21242b" }}>
      <Text style={{ flex: 1, fontSize: 11, color: "#aeb5c2" }}>{terminalStateMessage(state())}</Text>
      {(state().state === "failed" || state().state === "claimed") && <View testID={`terminal-retry:${props.shellId}`} accessibilityLabel="Retry terminal attachment" onPress={() => view?.command("reattach")}>
        <Text style={{ fontSize: 11, color: "#7acddd" }}>Retry</Text>
      </View>}
    </View>}
  </View>;
}
