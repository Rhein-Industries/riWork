// NSH 0.3 consolidates the nsh/native declarations into its nsh export.
import { Text, View, cleanup, nativeComponent, signal, type YNode } from "nsh";
import { Input } from "nsh/base";

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
  findResult: { total: number; current: number };
  findOpen: { query: string };
}
/** Frozen schema 1. Input, drawing and link detection stay in the native view. */
export const Terminal = nativeComponent<TerminalProps, TerminalEvents>("riwork.terminal", {
  props: ["target", "visible", "pinned", "focusOnAttach", "colors", "frozen"],
  events: ["state", "focusChange", "linkOpen", "drop", "inputRefused", "findResult", "findOpen"],
});
export type TerminalCommand = "focus" | "blur" | "reattach" | "copy" | "paste" | "selectAll" | "find" | "findNext" | "findPrevious" | "findClose";

export interface TerminalController {
  finding: () => boolean;
  find: (query?: string) => void;
  findNext: () => void;
  findPrevious: () => void;
  findClose: () => void;
}

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
  onControl?: (control: TerminalController) => (() => void) | void;
  onFocus: () => void;
  onAttached: () => void;
  onLink: (event: TerminalEvents["linkOpen"]) => void;
  onNotice: (message: string) => void;
}

export function LocalTerminal(props: LocalTerminalProps) {
  const state = signal<TerminalEvents["state"]>({ state: "detached" });
  const finding = signal(false);
  const query = signal("");
  const result = signal<TerminalEvents["findResult"]>({ total: 0, current: 0 });
  let view: YNode | undefined;
  let search: YNode | undefined;
  function find(text?: string) {
    if (text !== undefined) query.set(text);
    finding.set(true);
    view?.command("find", { query: query() });
    search?.focus();
  }
  function findClose() { finding.set(false); view?.command("findClose"); view?.command("focus"); }
  const control: TerminalController = {
    finding, find, findClose,
    findNext: () => view?.command("findNext"),
    findPrevious: () => view?.command("findPrevious"),
  };
  const unregister = props.onControl?.(control);
  cleanup(() => { unregister?.(); view = undefined; search = undefined; });
  return <View testID={`terminal-panel:${props.shellId}`} style={{ flex: 1, minHeight: 0 }}>
    {finding() && <View testID={`terminal-find:${props.shellId}`} style={{ flexDirection: "row", alignItems: "center", padding: 6, gap: 8, backgroundColor: "#21242b" }}>
      <Input testID={`terminal-find-input:${props.shellId}`} accessibilityLabel="Find in terminal" placeholder="Find in terminal" value={query()}
        ref={(node) => { search = node; node.focus(); }} onChange={(value) => { query.set(value); view?.command("find", { query: value }); }}
        onSubmit={control.findNext} style={{ flex: 1 }} />
      <Text testID={`terminal-find-result:${props.shellId}`} style={{ fontSize: 11, color: "#aeb5c2" }}>{result().current > 0 ? `${result().current} / ${result().total}` : `${result().total} matches`}</Text>
      <View testID={`terminal-find-previous:${props.shellId}`} role="button" accessibilityLabel="Previous match" onPress={control.findPrevious}><Text style={{ color: "#7acddd" }}>↑</Text></View>
      <View testID={`terminal-find-next:${props.shellId}`} role="button" accessibilityLabel="Next match" onPress={control.findNext}><Text style={{ color: "#7acddd" }}>↓</Text></View>
      <View testID={`terminal-find-close:${props.shellId}`} role="button" accessibilityLabel="Close terminal Find" onPress={findClose}><Text style={{ color: "#aeb5c2" }}>×</Text></View>
    </View>}
    <Terminal testID={`terminal:${props.shellId}`} ref={(node) => { view = node; props.onMount?.(); }}
      style={{ flex: 1, minHeight: 0 }} target={{ kind: "local", shellId: props.shellId }}
      visible={props.visible} pinned={props.pinned} focusOnAttach={props.focusOnAttach}
      colors={props.colors} frozen={props.frozen}
      onState={(next) => { state.set(next); if (next.state === "attached") props.onAttached(); }}
      // AppKit focus notifications can be queued behind a tab selection. A
      // previously shown terminal must not select itself after it was hidden.
      onFocusChange={(event) => { if (event.focused && props.visible && !props.frozen) props.onFocus(); }}
      onLinkOpen={props.onLink}
      onDrop={(event) => {
        if (event.accepted) { if (props.visible && !props.frozen) props.onFocus(); }
        else if (event.reason) props.onNotice(event.reason);
      }}
      onInputRefused={(event) => props.onNotice(event.reason)}
      onFindOpen={(event) => { query.set(event.query); finding.set(true); search?.focus(); }}
      onFindResult={(event) => result.set(event)} />
    {terminalStateMessage(state()) !== "" && <View testID={`terminal-state:${props.shellId}`} style={{ padding: 8, gap: 8, flexDirection: "row", backgroundColor: "#21242b" }}>
      <Text style={{ flex: 1, fontSize: 11, color: "#aeb5c2" }}>{terminalStateMessage(state())}</Text>
      {(state().state === "failed" || state().state === "claimed") && <View testID={`terminal-retry:${props.shellId}`} accessibilityLabel="Retry terminal attachment" onPress={() => view?.command("reattach")}>
        <Text style={{ fontSize: 11, color: "#7acddd" }}>Retry</Text>
      </View>}
    </View>}
  </View>;
}
