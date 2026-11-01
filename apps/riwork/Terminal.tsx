// NSH 0.3 consolidates the nsh/native declarations into its nsh export.
import { Text, View, cleanup, signal, type YNode } from "nsh";
import { Input } from "nsh/base";
import { DEFAULT_THEME, space, type InterfaceTheme } from "./appearance";
import { Icon } from "./Icon";

export * from "./terminal_view";
import { Terminal, terminalStateMessage, type TerminalColors, type TerminalEvents } from "./terminal_view";

export interface TerminalController {
  finding: () => boolean;
  find: (query?: string) => void;
  findNext: () => void;
  findPrevious: () => void;
  findClose: () => void;
}


interface LocalTerminalProps {
  theme?: () => InterfaceTheme;
  shellId: string;
  visible: boolean;
  pinned: boolean;
  focusOnAttach: boolean;
  colors: TerminalColors;
  frozen: boolean;
  /** The window is switching project: see `TerminalProps.keepOnUnmount`. */
  keepOnUnmount?: boolean;
  onMount?: () => void;
  onControl?: (control: TerminalController) => (() => void) | void;
  onFocus: () => void;
  onAttached: () => void;
  onLink: (event: TerminalEvents["linkOpen"]) => void;
  onNotice: (message: string) => void;
}

export function LocalTerminal(props: LocalTerminalProps) {
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const text = () => ({ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.muted });
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
    {finding() && <View testID={`terminal-find:${props.shellId}`} style={{ flexDirection: "row", alignItems: "center", padding: space(6, theme().scale), gap: 8, backgroundColor: theme().palette.panel, borderBottomWidth: 1, borderColor: theme().palette.divider }}>
      <Icon name="search" color={theme().palette.muted} />
      <Input testID={`terminal-find-input:${props.shellId}`} accessibilityLabel="Find in terminal" placeholder="Find in terminal" value={query()}
        ref={(node) => { search = node; node.focus(); }} onChange={(value) => { query.set(value); view?.command("find", { query: value }); }}
        onSubmit={control.findNext} style={{ ...text(), flex: 1, color: theme().palette.text }} />
      <Text testID={`terminal-find-result:${props.shellId}`} style={text()}>{result().current > 0 ? `${result().current} / ${result().total}` : `${result().total} matches`}</Text>
      <View testID={`terminal-find-previous:${props.shellId}`} role="button" accessibilityLabel="Previous match" tooltip="Previous match" onPress={control.findPrevious}><Text style={text()}>↑</Text></View>
      <View testID={`terminal-find-next:${props.shellId}`} role="button" accessibilityLabel="Next match" tooltip="Next match" onPress={control.findNext}><Text style={text()}>↓</Text></View>
      <View testID={`terminal-find-close:${props.shellId}`} role="button" accessibilityLabel="Close terminal Find" tooltip="Close Find" onPress={findClose}><Icon name="close" color={theme().palette.muted} /></View>
    </View>}
    <Terminal testID={`terminal:${props.shellId}`} ref={(node) => { view = node; props.onMount?.(); }}
      style={{ flex: 1, minHeight: 0 }} target={{ kind: "local", shellId: props.shellId }}
      visible={props.visible} pinned={props.pinned} focusOnAttach={props.focusOnAttach}
      colors={props.colors} frozen={props.frozen} keepOnUnmount={props.keepOnUnmount ?? false}
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
    {terminalStateMessage(state()) !== "" && <View testID={`terminal-state:${props.shellId}`} style={{ padding: 8, gap: 8, flexDirection: "row", backgroundColor: theme().palette.panel }}>
      <Text style={{ ...text(), flex: 1 }}>{terminalStateMessage(state())}</Text>
      {(state().state === "failed" || state().state === "claimed") && <View testID={`terminal-retry:${props.shellId}`} accessibilityLabel="Retry terminal attachment" onPress={() => view?.command("reattach")}>
        <Text style={{ ...text(), color: theme().palette.text }}>Retry</Text>
      </View>}
    </View>}
  </View>;
}
