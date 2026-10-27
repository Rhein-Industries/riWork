import { Text, View } from "nsh";

// Module state belongs to one NSH engine (one window). Counting actual component
// construction detects remounts; caching the number on the SavedTab would hide them.
const mounts = new Map<string, number>();

export function DeferredPanel(props: {
  id: string;
  title: string;
  visible: boolean;
  onPress: () => void;
}) {
  const mount = (mounts.get(props.id) ?? 0) + 1;
  mounts.set(props.id, mount);
  const empty = props.id.startsWith("empty:");
  return <View testID={`panel:${props.id}`} onPress={props.onPress}
    style={{ flex: 1, minHeight: 0, padding: 16, gap: 8 }}>
    <Text style={{ fontSize: 13, color: "#eef2f9" }}>{empty ? "Empty pane" : props.title}</Text>
    <Text style={{ fontSize: 11, color: "#8c95a5" }}>{empty ? "Drop a tab here or use the pane menu" : "This view is unavailable."}</Text>
    {!empty && <Text testID={`mount:${props.id}`} style={{ fontSize: 11, color: "#8c95a5" }}>{`mount ${mount}`}</Text>}
    {!empty && <Text testID={`visibility:${props.id}`} style={{ fontSize: 11, color: "#8c95a5" }}>{`visible: ${props.visible}`}</Text>}
  </View>;
}
