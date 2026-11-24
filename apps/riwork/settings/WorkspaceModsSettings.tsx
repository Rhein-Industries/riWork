import { Text, signal } from "nsh";
import { Button, View } from "../SettingsFocus";
import { space, type InterfaceTheme } from "../appearance";
import type { useCustomization } from "../customization";
import { buttonStates, buttonStyle, textStyle } from "./SettingsControls";

export function WorkspaceModsSettings(props: {
  theme: () => InterfaceTheme;
  customization: Pick<ReturnType<typeof useCustomization>, "state" | "setWorkspaceMod" | "error">;
}) {
  const { customization, theme } = props;
  const saving = signal(false);
  async function select(id: string | null) {
    if (saving()) return;
    saving.set(true);
    try { await customization.setWorkspaceMod(id); }
    finally { saving.set(false); }
  }
  return <View testID="workspace-mods" style={{ gap: space(12, theme().scale) }}>
    {customization.state().mods.map((mod) => {
      const enabled = () => customization.state().workspaceMod === mod.id;
      return <View style={{ gap: space(6, theme().scale) }}>
        <Text style={{ ...textStyle(theme, 12), fontWeight: "600" }}>{mod.name}</Text>
        <Text style={{ ...textStyle(theme, 11), color: theme().palette.muted }}>{mod.description}</Text>
        <View style={{ flexDirection: "row", alignItems: "center", gap: space(10, theme().scale) }}>
          <Button testID={`workspace-mod:${mod.id}`} disabled={saving()} onPress={() => void select(enabled() ? null : mod.id)}
            style={buttonStyle(theme)} {...buttonStates(theme)}>
            <Text style={textStyle(theme)}>{enabled() ? "Disable" : "Enable"}</Text>
          </Button>
          <Text style={{ ...textStyle(theme, 11), color: theme().palette.muted }}>{enabled() ? "Enabled" : "Included with Somtus"}</Text>
        </View>
      </View>;
    })}
    {customization.error() && <Text role="alert" style={{ ...textStyle(theme, 11), color: theme().palette.gold }}>{customization.error()}</Text>}
  </View>;
}
