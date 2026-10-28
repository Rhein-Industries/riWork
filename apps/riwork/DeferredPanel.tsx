import { Text, View } from "nsh";
import { DEFAULT_THEME, type InterfaceTheme } from "./appearance";

// Module state belongs to one NSH engine (one window). Counting actual component
// construction detects remounts; caching the number on the SavedTab would hide them.
const mounts = new Map<string, number>();

export function DeferredPanel(props: {
  id: string;
  title: string;
  visible: boolean;
  textSize?: number;
  onPress: () => void;
  theme?: () => InterfaceTheme;
  diagnostics?: boolean;
}) {
  const mount = (mounts.get(props.id) ?? 0) + 1;
  mounts.set(props.id, mount);
  const empty = props.id.startsWith("empty:");
  const theme = props.theme ?? (() => DEFAULT_THEME);
  // An empty pane is one muted line at the top left, as main.rs draws it.
  if (empty) return <View testID={`panel:${props.id}`} onPress={props.onPress} style={{ flex: 1, minHeight: 0, minWidth: 0, padding: 14, overflow: "hidden" }}>
    <Text style={{ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.muted }}>Drop a tab here or use the pane menu</Text>
  </View>;
  return <View testID={`panel:${props.id}`} onPress={props.onPress}
    style={{ flex: 1, minHeight: 0, padding: 16, gap: 8 }}>
    <Text style={{ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.text }}>{props.title}</Text>
    <Text style={{ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.muted }}>This view is unavailable.</Text>
    {props.diagnostics && <Text testID={`mount:${props.id}`} style={{ fontFamily: theme().monoFamily, fontSize: 11 * theme().scale, color: theme().palette.muted }}>{`mount ${mount}`}</Text>}
    {props.diagnostics && <Text testID={`visibility:${props.id}`} style={{ fontFamily: theme().monoFamily, fontSize: 11 * theme().scale, color: theme().palette.muted }}>{`visible: ${props.visible}`}</Text>}
  </View>;
}

/** Fixture-only mount/visibility probe for content that is not a DeferredPanel. */
export function MountProbe(props: { id: string; visible: boolean; theme?: () => InterfaceTheme }) {
  const mount = (mounts.get(props.id) ?? 0) + 1;
  mounts.set(props.id, mount);
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const style = () => ({ fontFamily: theme().monoFamily, fontSize: 9 * theme().scale, color: theme().palette.muted });
  return <View style={{ flexDirection: "row", gap: 8, paddingHorizontal: 8 }}>
    <Text testID={`mount:${props.id}`} style={style()}>{`mount ${mount}`}</Text>
    <Text testID={`visibility:${props.id}`} style={style()}>{`visible: ${props.visible}`}</Text>
  </View>;
}
