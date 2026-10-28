import { Scroll, Text, View, cleanup, signal, type YNode } from "nsh";
import { Button, Dialog } from "nsh/base";
import { space, type InterfaceTheme } from "../appearance";

/** A snippet stays inert: choosing a currently open local terminal only copies
 * the text and focuses that terminal. The user decides when and where to paste. */
export function CodeTerminalDialog(props: {
  text: string;
  targets: () => { id: string; label: string }[];
  onCopy: (id: string, text: string) => Promise<void>;
  theme: () => InterfaceTheme;
  onClose: () => void;
}) {
  const busy = signal(false), error = signal("");
  let cancel: YNode | undefined;
  let alive = true;
  cleanup(() => { alive = false; });
  const palette = () => props.theme().palette;
  const size = (value: number) => space(value, props.theme().scale);
  const text = (value = 12, muted = false) => ({ fontFamily: props.theme().family,
    fontSize: value * props.theme().scale, color: muted ? palette().muted : palette().text });
  const close = () => { if (!busy()) props.onClose(); };
  async function choose(id: string) {
    if (busy()) return;
    if (!props.targets().some((target) => target.id === id)) {
      error.set("That terminal is no longer open. Choose another terminal.");
      return;
    }
    busy.set(true); error.set("");
    try {
      await props.onCopy(id, props.text);
      if (alive) props.onClose();
    } catch (cause) {
      if (alive) error.set(String(cause));
    } finally { if (alive) busy.set(false); }
  }
  return <Dialog animation="none" closeOnBackdropPress={false} closeOnEscape={!busy()}
    onOpen={() => cancel?.focus()} onOpenChange={(open) => { if (!open) close(); }}
    backdropStyle={{ backgroundColor: "#00000066" }}
    style={{ width: size(600), maxWidth: "100%", maxHeight: "90%", padding: size(16), gap: size(12),
      borderRadius: 10, borderWidth: 1, borderColor: palette().divider, backgroundColor: palette().panel }}>
    <Text role="header" style={{ ...text(15), fontWeight: "600" }}>Copy snippet and open a terminal</Text>
    <Text style={text(11, true)}>Choose a terminal. The snippet will be copied for you to paste when ready.</Text>
    <Scroll style={{ maxHeight: size(220), minHeight: 0, flexShrink: 1, padding: size(10), borderRadius: 5, backgroundColor: palette().bg }}>
      <Text selectable={true} style={{ ...text(11), fontFamily: props.theme().monoFamily }}>{props.text}</Text>
    </Scroll>
    <Scroll style={{ maxHeight: size(260), minHeight: 0, flexShrink: 1 }} contentStyle={{ gap: size(6) }}>
      {!props.targets().length && <Text style={text(12, true)}>No local terminals are open in this project window.</Text>}
      {props.targets().map((target) => <Button disabled={busy()} onPress={() => void choose(target.id)}
        accessibilityLabel={`Copy snippet and open ${target.label}`}
        style={{ padding: size(9), borderRadius: 5, borderWidth: 1, borderColor: palette().divider, opacity: busy() ? 0.5 : 1 }}
        focusStyle={{ borderColor: palette().cyan }}>
        <Text style={text()}>{target.label}</Text>
      </Button>)}
    </Scroll>
    {!!error() && <Text role="text" style={{ ...text(11), color: palette().gold }}>{error()}</Text>}
    <View style={{ flexDirection: "row", justifyContent: "flex-end" }}>
      <Button ref={(node) => { cancel = node; }} disabled={busy()} onPress={close}
        style={{ paddingHorizontal: size(12), paddingVertical: size(7), borderRadius: 5, borderWidth: 1, borderColor: palette().divider }}
        focusStyle={{ borderColor: palette().cyan }}><Text style={text()}>Cancel</Text></Button>
    </View>
  </Dialog>;
}
