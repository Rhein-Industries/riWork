import { Text, View, cleanup, signal, type YNode } from "nsh";
import { Input } from "nsh/base";
import { DEFAULT_THEME, space, type InterfaceTheme } from "../appearance";
import { Icon } from "../Icon";
import { Terminal, terminalStateMessage, type TerminalColors, type TerminalController, type TerminalEvents } from "../Terminal";
import { RemoteStrip } from "./Remote";
import type { RemoteLink } from "./model";

export interface RemoteTerminalProps {
  desktopId: string; shellId: string; label: string; link: RemoteLink;
  visible: boolean; focusOnAttach: boolean; colors: TerminalColors; frozen: boolean;
  theme?: () => InterfaceTheme;
  onControl?: (control: TerminalController) => (() => void) | void;
  onFocus: () => void; onAttached: () => void; onNotice: (message: string) => void;
}

/** Native remote target, with host-attributed state above the terminal. No local
 * links, file drops, uploaded images, Store actions or local shell IDs are routed
 * through this view. Reconnect asks the component to attach the exact same pair. */
export function RemoteTerminal(props: RemoteTerminalProps) {
  const state = signal<TerminalEvents["state"]>({ state: "detached" });
  const ended = signal(false), finding = signal(false), query = signal("");
  const result = signal<TerminalEvents["findResult"]>({ total: 0, current: 0 });
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const key = () => `remote:${props.desktopId}:${props.shellId}`;
  const text = (size: number, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color });
  let view: YNode | undefined, search: YNode | undefined;
  function find(value?: string) {
    if (value !== undefined) query.set(value);
    finding.set(true); view?.command("find", { query: query() }); search?.focus();
  }
  function findClose() { finding.set(false); view?.command("findClose"); view?.command("focus"); }
  const control: TerminalController = { finding, find, findClose,
    findNext: () => view?.command("findNext"), findPrevious: () => view?.command("findPrevious") };
  const unregister = props.onControl?.(control);
  cleanup(() => { unregister?.(); view = undefined; search = undefined; });
  function reconnect() {
    if (props.frozen) return;
    ended.set(false); state.set({ state: "attaching" });
    view?.command("reattach"); view?.command("focus");
  }
  const feedback = () => {
    // The ended and reconnecting cases belong to the host strip, preserving the
    // final screen below it; first attachment/failed attachment uses a notice.
    if (state().state === "ended" || state().state === "reconnecting") return "";
    if (state().state === "attaching") return state().message ?? "Connecting…";
    return terminalStateMessage(state());
  };
  return <View testID={`remote-terminal-panel:${key()}`} style={{ flex: 1, minHeight: 0 }}>
    <RemoteStrip label={props.label} link={props.link} ended={ended()} onReconnect={reconnect} theme={theme} />
    {finding() && <View testID={`terminal-find:${key()}`} style={{ flexDirection: "row", alignItems: "center", padding: space(6, theme().scale), gap: 8, backgroundColor: theme().palette.panel, borderBottomWidth: 1, borderColor: theme().palette.divider }}>
      <Icon name="search" color={theme().palette.muted} />
      <Input testID={`terminal-find-input:${key()}`} accessibilityLabel="Find in terminal" placeholder="Find in terminal" value={query()}
        ref={(node) => { search = node; node.focus(); }} onChange={(value) => { query.set(value); view?.command("find", { query: value }); }} onSubmit={control.findNext} style={{ ...text(11), flex: 1 }} />
      <Text testID={`terminal-find-result:${key()}`} style={text(11, theme().palette.muted)}>{result().current > 0 ? `${result().current} / ${result().total}` : `${result().total} matches`}</Text>
      <View testID={`terminal-find-previous:${key()}`} role="button" accessibilityLabel="Previous match" onPress={control.findPrevious}><Text style={text(11, theme().palette.cyan)}>↑</Text></View>
      <View testID={`terminal-find-next:${key()}`} role="button" accessibilityLabel="Next match" onPress={control.findNext}><Text style={text(11, theme().palette.cyan)}>↓</Text></View>
      <View testID={`terminal-find-close:${key()}`} role="button" accessibilityLabel="Close terminal Find" tooltip="Close Find" onPress={findClose}><Icon name="close" color={theme().palette.muted} /></View>
    </View>}
    <Terminal testID={`terminal:${key()}`} ref={(node) => { view = node; }} style={{ flex: 1, minHeight: 0 }}
      target={{ kind: "remote", desktopId: props.desktopId, shellId: props.shellId }} visible={props.visible} pinned={ended()}
      focusOnAttach={props.focusOnAttach} colors={props.colors} frozen={props.frozen}
      onState={(next) => {
        state.set(next);
        if (next.state === "ended") ended.set(true);
        if (next.state === "attached") { ended.set(false); props.onAttached(); }
        if (next.state === "failed" && next.message) props.onNotice(next.message);
      }}
      onFocusChange={(event) => { if (event.focused && props.visible && !props.frozen) props.onFocus(); }}
      onInputRefused={(event) => props.onNotice(event.reason)}
      onFindOpen={(event) => { query.set(event.query); finding.set(true); search?.focus(); }} onFindResult={(event) => result.set(event)} />
    {feedback() !== "" && <View testID={`terminal-state:${key()}`} style={{ flexDirection: "row", padding: 8, gap: 8, backgroundColor: theme().palette.panel }}>
      <Text style={{ ...text(11, theme().palette.muted), flex: 1 }}>{feedback()}</Text>
      {state().state === "failed" && <View testID={`terminal-retry:${key()}`} role="button" accessibilityLabel="Retry terminal attachment" onPress={reconnect}><Text style={text(11, theme().palette.cyan)}>Retry</Text></View>}
    </View>}
  </View>;
}
