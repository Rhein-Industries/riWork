import { Text, View, type YNode } from "nsh";
import { Dialog } from "nsh/base";
import { DEFAULT_THEME, type InterfaceTheme } from "./appearance";
import { WORKSPACE_METRICS } from "./metrics";

// D-COPY-1 remains open. Preserve the shipping GPUI wording until copy is decided.
export const CLOSE_DIALOG_BODY = "Detach hides the tab and keeps the session running; reopen it from ＋ › Open a worker or shell. Exit stops it; chat history is kept.";

export function CloseDialog(props: { title: string; theme?: () => InterfaceTheme; onChoice: (choice: "detach" | "exit" | null) => void }) {
  let detach: YNode | undefined;
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const text = (base: number, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: base * theme().scale, color });
  return <Dialog testID="tab-close-dialog" animation="none" closeOnBackdropPress={false}
    closeOnEscape={true} onOpen={() => detach?.focus()} onOpenChange={() => props.onChoice(null)}
    backdropTestID="tab-close-backdrop" backdropStyle={{ backgroundColor: "#00000066" }}
    style={{ width: WORKSPACE_METRICS.closeDialogWidth, maxWidth: "100%", padding: 18, gap: 10, backgroundColor: theme().palette.panel, borderRadius: 12 }}>
    <Text style={text(13)}>{`Close ${props.title}?`}</Text>
    <Text style={text(11, theme().palette.muted)}>{CLOSE_DIALOG_BODY}</Text>
    <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 8, paddingTop: 6 }}>
      <View role="button" accessibilityLabel="Cancel" onPress={() => props.onChoice(null)} style={{ padding: 8 }}><Text style={text(11)}>Cancel</Text></View>
      <View role="button" accessibilityLabel="Exit" onPress={() => props.onChoice("exit")} style={{ padding: 8 }}><Text style={text(11, "#ed7777")}>Exit</Text></View>
      <View ref={(node) => { detach = node; }} focusable={true} role="button" accessibilityLabel="Detach" onPress={() => props.onChoice("detach")} style={{ padding: 8, backgroundColor: theme().palette.divider, borderRadius: 3 }}><Text style={text(11)}>Detach</Text></View>
    </View>
  </Dialog>;
}
