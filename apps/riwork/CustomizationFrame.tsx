import { ErrorBoundary, Scroll, Text, View, computed, signal, type JSX } from "nsh";
import { AddinTab, type AddinAction } from "./AddinTab";
import { type InterfaceTheme } from "./appearance";
import { interfaceGeometry, type InterfaceLayout, type Rect } from "./customization_model";

const position = (rect: Rect) => ({ position: "absolute" as const, left: rect.x, top: rect.y, width: rect.width, height: rect.height, minWidth: 0, minHeight: 0, overflow: "hidden" as const });

/** User-owned chrome lives beside the persistent workspace, never above it in
 * a changing component tree. Only a changed mod is rebuilt by its own engine. */
export function CustomizationFrame(props: {
  layout: () => InterfaceLayout; custom: () => boolean; theme: () => InterfaceTheme;
  projectId: () => string; workspace: () => unknown; title: () => string;
  onCommand: (id: string) => void; onAction: (key: string, action: AddinAction) => void;
  onEdit: () => void; onReset: () => void; children?: JSX.Element;
}) {
  const extent = signal({ width: 0, height: 0 });
  const geometry = computed(() => interfaceGeometry(props.layout().root, extent().width, extent().height));
  const theme = props.theme;
  const text = () => ({ color: theme().palette.text, fontFamily: theme().family, fontSize: 11 * theme().scale });
  // Mod identity excludes geometry: resizing/reordering blocks only updates a
  // native frame. Context updates travel over the existing add-in host channel.
  let previousBlocks: ReturnType<typeof interfaceGeometry>["leaves"][number]["node"][] = [];
  let previousSignatures: string[] = [];
  const blocks = computed(() => {
    const next = interfaceGeometry(props.layout().root, 0, 0).leaves.filter(({ node }) => node.type !== "spacer").map(({ node }) => node);
    const signatures = next.map(({ size: _, ...content }) => JSON.stringify(content));
    if (signatures.length === previousSignatures.length && signatures.every((value, index) => value === previousSignatures[index])) return previousBlocks;
    const retained = next.map((node, index) => {
      const old = previousSignatures.indexOf(signatures[index]);
      return old < 0 ? node : previousBlocks[old];
    });
    previousSignatures = signatures; previousBlocks = retained;
    return retained;
  });
  return <View testID="customization-frame" style={{ flex: 1, minHeight: 0 }}>
    <View testID="customization-titlebar" desktop={{ windowDrag: true }} style={{ height: props.custom() ? 28 : 0, overflow: "hidden", flexDirection: "row", alignItems: "center", paddingLeft: 94, paddingRight: 10, gap: 12, backgroundColor: theme().palette.panel }}>
      <Text numberOfLines={1} style={{ ...text(), flex: 1 }}>{props.title()}</Text>
      <View role="button" accessibilityLabel="Edit interface" onPress={props.onEdit}><Text style={text()}>Edit</Text></View>
      <View role="button" accessibilityLabel="Reset interface" onPress={props.onReset}><Text style={text()}>Reset</Text></View>
    </View>
    <View testID="customization-body" onLayout={(event) => { const size = event.size; if (size.width !== extent().width || size.height !== extent().height) extent.set(size); }} style={{ flex: 1, minHeight: 0 }}>
      <View testID="customization-workspace" style={props.custom() ? position(geometry().workspace) : { flex: 1, minHeight: 0 }}>
        {props.children}
      </View>
      {blocks().map((block) => <View testID={`customization-block:${block.id}`} style={{ ...position(geometry().leaves.find(({ node }) => node.id === block.id)?.rect ?? { x: 0, y: 0, width: 0, height: 0 }), backgroundColor: theme().palette.panel, borderColor: theme().palette.divider, borderWidth: 1 }}>
        <ErrorBoundary fallback={(error) => <View style={{ padding: 8 }}><Text selectable={true} style={text()}>{`This interface block could not load: ${String(error)}`}</Text><View role="button" accessibilityLabel="Reset interface" onPress={props.onReset}><Text style={text()}>Reset interface</Text></View></View>}>
          {block.type === "mod" ? <AddinTab addin={{ key: block.addin, view: block.view, args: block.args ?? null }} projectId={props.projectId} theme={theme} visible={() => true} workspace={props.workspace} onAction={props.onAction} /> :
            block.type === "commands" ? <Scroll style={{ flex: 1 }} contentStyle={{ padding: 8, gap: 4 }}>
              {block.items.map((item) => <View testID={`customization-command:${block.id}:${item.command}`} role="button" accessibilityLabel={item.label} onPress={() => props.onCommand(item.command)} style={{ padding: 8, borderRadius: 5 }} hoverStyle={{ backgroundColor: theme().palette.panelActive }}><Text style={text()}>{item.label}</Text></View>)}
            </Scroll> : null}
        </ErrorBoundary>
      </View>)}
    </View>
  </View>;
}
