import { Scroll, Text, View } from "nsh";
import { Button } from "nsh/base";
import type { InterfaceTheme } from "../appearance";

/** Keep a failed chat view contained and expose the actual runtime error. A
 * retry remounts the reader; chat history and saved drafts remain native-owned. */
export function ChatFailure(props: { error: unknown; retry: () => void; theme: () => InterfaceTheme }) {
  const detail = props.error instanceof Error
    ? [props.error.message, props.error.stack].filter(Boolean).join("\n\n")
    : String(props.error);
  const style = () => ({ fontFamily: props.theme().family, fontSize: 12 * props.theme().scale, color: props.theme().palette.text });
  return <View testID="chat-runtime-error" style={{ flex: 1, minHeight: 0, padding: 18, gap: 12, backgroundColor: props.theme().palette.panel }}>
    <Text role="header" style={{ ...style(), fontWeight: "600" }}>The chat view could not render</Text>
    <Scroll style={{ flexShrink: 1, minHeight: 0 }}><Text selectable={true} role="text" style={{ ...style(), fontFamily: props.theme().monoFamily, color: props.theme().palette.gold }}>{detail}</Text></Scroll>
    <Text style={style()}>Retry reloads this conversation and its saved draft.</Text>
    <Button onPress={props.retry} style={{ alignSelf: "flex-start", paddingHorizontal: 12, paddingVertical: 7, borderWidth: 1, borderRadius: 5, borderColor: props.theme().palette.divider }} focusStyle={{ borderColor: props.theme().palette.cyan }}><Text style={style()}>Retry chat view</Text></Button>
  </View>;
}
