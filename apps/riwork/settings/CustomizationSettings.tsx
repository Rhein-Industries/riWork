import { Text } from "nsh";
import { View } from "../SettingsFocus";
import { useCustomization, type CustomizationService } from "../customization";
import { type InterfaceTheme } from "../appearance";

export function CustomizationSettings(props: { theme: () => InterfaceTheme; service?: CustomizationService }) {
  const customization = useCustomization(props.service);
  const text = () => ({ color: props.theme().palette.text, fontFamily: props.theme().family, fontSize: 12 * props.theme().scale });
  const button = (label: string, onPress: () => void) => <View role="button" focusable={true} accessibilityLabel={label} onPress={onPress} style={{ paddingHorizontal: 10, paddingVertical: 7, borderRadius: 5, backgroundColor: props.theme().palette.panelActive }}><Text style={text()}>{label}</Text></View>;
  return <View testID="customization-settings" style={{ gap: 9 }}>
    <Text style={text()}>Choose an interface, or edit layouts and shortcuts with your agent. Changes appear as you save.</Text>
    <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
      {customization.state().layouts.map((layout) => <View testID={`customization-select:${layout.id}`} role="radio" focusable={true} accessibilityLabel={layout.name} accessibilityState={{ checked: customization.state().selected === layout.id }} onPress={() => void customization.select(layout.id)} style={{ padding: 8, borderRadius: 5, borderWidth: 1, borderColor: customization.state().selected === layout.id ? props.theme().palette.cyan : props.theme().palette.divider }}><Text style={text()}>{layout.name}</Text></View>)}
    </View>
    <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
      {button("Open customization folder", () => void customization.edit())}
      {button("Reset interface and shortcuts", () => void customization.reset())}
    </View>
    <Text style={{ ...text(), color: props.theme().palette.muted }}>Control–Option–Shift–Escape always restores the default interface and shortcuts. Your tabs and sessions stay in place.</Text>
    {customization.error() !== "" && <Text testID="customization-settings-error" role="alert" selectable={true} style={{ ...text(), color: props.theme().palette.gold }}>{customization.error()}</Text>}
  </View>;
}
