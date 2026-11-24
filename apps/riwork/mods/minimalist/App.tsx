import { Text } from "nsh";
import { Button } from "nsh/base";
import { space } from "../../appearance";
import type { WorkspaceMod, WorkspaceNavigation } from "..";
import manifest from "./mod.json";

const entries = [
  { key: "projects", label: "Projects" },
  { key: "files", label: "Files" },
  { key: "cli", label: "CLI" },
  { key: "settings", label: "Settings" },
];

function Navigation(props: WorkspaceNavigation) {
  return <>{entries.map(({ key, label }) => {
    const theme = props.theme;
    const selected = () => props.active(key);
    return <Button testID={`workspace-navigation:${key}`} selected={selected()} onPress={() => props.open(key)}
      style={{ paddingHorizontal: space(9, theme().scale), paddingVertical: space(4, theme().scale), borderRadius: 5,
        backgroundColor: selected() ? theme().palette.panelActive : "transparent" }}>
      <Text style={{ fontFamily: theme().family, fontSize: 11 * theme().scale, color: selected() ? theme().palette.text : theme().palette.muted }}>{label}</Text>
    </Button>;
  })}</>;
}

export const minimalist: WorkspaceMod = {
  id: manifest.id,
  visibleTabs: new Set(entries.map(({ key }) => key)),
  showStatusBar: false,
  Navigation,
};
