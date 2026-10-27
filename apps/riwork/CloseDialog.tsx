import { Text, View, type YNode } from "nsh";
import { Dialog } from "nsh/base";
import { WORKSPACE_METRICS } from "./metrics";

// D-COPY-1 remains open. Preserve the shipping GPUI wording until copy is decided.
export const CLOSE_DIALOG_BODY = "Detach hides the tab and keeps the session running; reopen it from ＋ › Open a worker or shell. Exit stops it; chat history is kept.";

export function CloseDialog(props: { title: string; onChoice: (choice: "detach" | "exit" | null) => void }) {
  let detach: YNode | undefined;
  return <Dialog testID="tab-close-dialog" animation="none" closeOnBackdropPress={false}
    closeOnEscape={true} onOpen={() => detach?.focus()} onOpenChange={() => props.onChoice(null)}
    backdropTestID="tab-close-backdrop" backdropStyle={{ backgroundColor: "#00000066" }}
    style={{ width: WORKSPACE_METRICS.closeDialogWidth, maxWidth: "100%", padding: 18, gap: 10, backgroundColor: "#21242b", borderRadius: 12 }}>
    <Text style={{ fontSize: 13, color: "#eef2f9" }}>{`Close ${props.title}?`}</Text>
    <Text style={{ fontSize: 11, color: "#aeb5c2" }}>{CLOSE_DIALOG_BODY}</Text>
    <View style={{ flexDirection: "row", justifyContent: "flex-end", gap: 8, paddingTop: 6 }}>
      <View role="button" accessibilityLabel="Cancel" onPress={() => props.onChoice(null)} style={{ padding: 8 }}><Text style={{ color: "#eef2f9" }}>Cancel</Text></View>
      <View role="button" accessibilityLabel="Exit" onPress={() => props.onChoice("exit")} style={{ padding: 8 }}><Text style={{ color: "#ed7777" }}>Exit</Text></View>
      <View ref={(node) => { detach = node; }} focusable={true} role="button" accessibilityLabel="Detach" onPress={() => props.onChoice("detach")} style={{ padding: 8, backgroundColor: "#375576", borderRadius: 3 }}><Text style={{ color: "#eef2f9" }}>Detach</Text></View>
    </View>
  </Dialog>;
}
