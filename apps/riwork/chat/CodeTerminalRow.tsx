import { Text, View, cleanup, signal, type YNode } from "nsh";
import { space, type InterfaceTheme } from "../appearance";
import { escapeWithin, focusWithin } from "./SidePanel";

/** A snippet stays inert: choosing a currently open local terminal only copies
 * the text and focuses that terminal. The user decides when and where to paste.
 * The row opens in the transcript, under the reply whose code block asked. */
export function CodeTerminalRow(props: {
  text: string;
  targets: () => { id: string; label: string }[];
  onCopy: (id: string, text: string) => Promise<void>;
  theme: () => InterfaceTheme;
  active: () => boolean;
  /** Take the focus: the row just opened (a cell scrolled back shows it again without). */
  focus: boolean;
  /** `focused`: the focus was in the row, so it goes back to what opened it. */
  onClose: (focused: boolean) => void;
}) {
  const busy = signal(false), error = signal("");
  const focus = focusWithin();
  let alive = true;
  cleanup(() => { alive = false; });
  const palette = () => props.theme().palette;
  const size = (value: number) => space(value, props.theme().scale);
  const text = (value = 11, color = palette().text) => ({ fontFamily: props.theme().family, fontSize: value * props.theme().scale, color });
  const close = () => { if (!busy()) props.onClose(focus.inside()); };
  escapeWithin(focus, () => props.active() && !busy(), close);
  async function choose(id: string) {
    if (busy()) return;
    if (!props.targets().some((target) => target.id === id)) {
      error.set("That terminal is no longer open. Choose another terminal.");
      return;
    }
    busy.set(true); error.set("");
    try {
      await props.onCopy(id, props.text);
      if (alive) props.onClose(false);
    } catch (cause) {
      if (alive) error.set(String(cause));
    } finally { if (alive) busy.set(false); }
  }
  // Busy is read in place, so a copy keeps the buttons (and the focus).
  const button = (label: string, accessibilityLabel: string, onPress: () => void, tooltip?: string, ref?: (node: YNode) => void) =>
    <View role="button" ref={(node) => ref?.(node)} focusable={!busy()} tooltip={tooltip} accessibilityLabel={accessibilityLabel} accessibilityState={{ disabled: busy() }}
      onPress={() => { if (!busy()) onPress(); }} {...focus.track(accessibilityLabel)}
      style={{ maxWidth: "100%", paddingHorizontal: size(8), paddingVertical: size(4), borderRadius: 5, borderWidth: 1, borderColor: palette().divider, opacity: busy() ? 0.5 : 1 }}
      hoverStyle={{ backgroundColor: palette().panelActive }} focusStyle={{ borderColor: palette().cyan }}>
      <Text numberOfLines={1} style={text()}>{label}</Text>
    </View>;
  return <View testID="chat-copy-to-terminal" style={{ marginTop: size(6), gap: size(5) }}>
    <View style={{ flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: size(6) }}>
      <Text style={text(11, palette().muted)}>Copy to:</Text>
      {!props.targets().length && <Text style={text(11, palette().muted)}>No local terminals are open in this project window.</Text>}
      {props.targets().map((target) => button(target.label, `Copy snippet and open ${target.label}`, () => void choose(target.id),
        "Copies the snippet and opens this terminal. Paste it there when ready."))}
      {button("Cancel", "Cancel", close, undefined, (node) => { if (props.focus) node.focus(); })}
    </View>
    {!!error() && <Text role="text" style={{ ...text(11), color: palette().gold }}>{error()}</Text>}
  </View>;
}
