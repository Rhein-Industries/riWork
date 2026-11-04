import { Canvas, Path2D, Text, View } from "nsh";
import type { InterfaceTheme } from "./appearance";

/** The supplied SOMTUS mark, from assets/app-icon/somtus-icon.svg. */
export function Brand(props: { theme: () => InterfaceTheme; size?: number; wordmark?: boolean }) {
  const side = () => (props.size ?? 20) * props.theme().scale;
  return <View accessibilityLabel="SOMTUS" style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
    <Canvas pointerEvents="none" style={{ width: side(), height: side(), flexShrink: 0 }} draw={(cx, size) => {
      cx.scale(size.width / 824, size.height / 824);
      cx.translate(-100, -100);
      cx.fillStyle = "#FF4A14";
      cx.fillRect(100, 100, 824, 824);
      cx.fillStyle = "#0A0807";
      cx.fill(new Path2D("M228 312V712L486 512Z"));
      cx.fill(new Path2D("M796 312V712L538 512Z"));
      cx.fillRect(486, 236, 52, 552);
    }} />
    {props.wordmark !== false && <Text style={{ fontFamily: props.theme().family, fontSize: 12 * props.theme().scale, fontWeight: "700", color: props.theme().palette.text }}>SOMTUS</Text>}
  </View>;
}
