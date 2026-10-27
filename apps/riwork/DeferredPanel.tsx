import { Scroll, Text, View } from "nsh";
import { DEFAULT_THEME, type InterfaceTheme } from "./appearance";
import { Settings, type SettingsDto } from "riwork/native";

// Module state belongs to one NSH engine (one window). Counting actual component
// construction detects remounts; caching the number on the SavedTab would hide them.
const mounts = new Map<string, number>();

export function DeferredPanel(props: {
  id: string;
  title: string;
  visible: boolean;
  onPress: () => void;
  theme?: () => InterfaceTheme;
  settings?: () => SettingsDto | null;
  onError?: (reason: unknown) => void;
  diagnostics?: boolean;
}) {
  const mount = (mounts.get(props.id) ?? 0) + 1;
  mounts.set(props.id, mount);
  const empty = props.id.startsWith("empty:");
  const theme = props.theme ?? (() => DEFAULT_THEME);
  const textStyle = (muted = false) => ({ fontFamily: theme().family, fontSize: 11 * theme().scale, color: muted ? theme().palette.muted : theme().palette.text });
  return <View testID={`panel:${props.id}`} onPress={props.onPress}
    style={{ flex: 1, minHeight: 0, padding: 16, gap: 8 }}>
    <Text style={{ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.text }}>{empty ? "Empty pane" : props.title}</Text>
    <Text style={{ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.muted }}>{empty ? "Drop a tab here or use the pane menu" : props.id === "panel:settings" ? "Appearance and status bar" : "This view is unavailable."}</Text>
    {props.id === "panel:settings" && props.settings?.() && <Scroll testID="appearance-settings" style={{ flex: 1, minHeight: 0 }} contentStyle={{ gap: 8 }}>
      <Text style={{ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.text }}>Theme</Text>
      {props.settings()!.themeChoices.map((choice) => <View testID={`theme-choice:${choice.id}`} role="radio" accessibilityLabel={choice.label}
        accessibilityState={{ checked: props.settings!()!.theme === choice.id }}
        onPress={() => { void Settings.set({ theme: choice.id }).catch(props.onError); }}>
        <Text style={{ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.text }}>{choice.label}</Text>
        <Text style={{ fontFamily: theme().family, fontSize: 9 * theme().scale, color: theme().palette.muted }}>{choice.description}</Text>
      </View>)}
      <Text style={{ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.text }}>Interface text size</Text>
      <View style={{ flexDirection: "row", gap: 12 }}>
        {([ ["smaller", "Smaller text"], ["bigger", "Bigger text"], ["reset", "Actual size"] ] as const).map(([action, title]) =>
          <View role="button" accessibilityLabel={title} onPress={() => { void Settings.changeTextSize(action).catch(props.onError); }}><Text style={textStyle()}>{title}</Text></View>)}
      </View>
      <View role="checkbox" accessibilityLabel="Show status bar" accessibilityState={{ checked: props.settings()!.statusBar.enabled }}
        onPress={() => { void Settings.setEnabled(!props.settings!()!.statusBar.enabled).catch(props.onError); }}>
        <Text style={{ fontFamily: theme().family, fontSize: 11 * theme().scale, color: theme().palette.text }}>Show status bar</Text>
      </View>
      {props.settings()!.statusBar.items.map((item) => <View style={{ flexDirection: "row", gap: 10 }}>
        <View role="checkbox" accessibilityLabel={item.label} accessibilityState={{ checked: item.enabled }}
          onPress={() => { void Settings.setVisible(item.kind, !item.enabled).catch(props.onError); }}>
          <Text style={textStyle()}>{item.label}</Text>
        </View>
        <View role="button" accessibilityLabel={`Move ${item.label} to ${item.side === "left" ? "right" : "left"}`}
          onPress={() => { void Settings.setSide(item.kind, item.side === "left" ? "right" : "left").catch(props.onError); }}><Text style={textStyle(true)}>{item.side}</Text></View>
        <View role="button" accessibilityLabel={`Move ${item.label} earlier`} onPress={() => { void Settings.moveItem(item.kind, true).catch(props.onError); }}><Text style={textStyle(true)}>↑</Text></View>
        <View role="button" accessibilityLabel={`Move ${item.label} later`} onPress={() => { void Settings.moveItem(item.kind, false).catch(props.onError); }}><Text style={textStyle(true)}>↓</Text></View>
      </View>)}
    </Scroll>}
    {!empty && props.diagnostics && <Text testID={`mount:${props.id}`} style={{ fontFamily: theme().monoFamily, fontSize: 11 * theme().scale, color: theme().palette.muted }}>{`mount ${mount}`}</Text>}
    {!empty && props.diagnostics && <Text testID={`visibility:${props.id}`} style={{ fontFamily: theme().monoFamily, fontSize: 11 * theme().scale, color: theme().palette.muted }}>{`visible: ${props.visible}`}</Text>}
  </View>;
}
