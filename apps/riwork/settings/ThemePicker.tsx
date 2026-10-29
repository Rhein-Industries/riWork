import { Text, cleanup, colorScheme, effect, signal, type Signal, type YNode } from "nsh";
import { Button, View } from "../SettingsFocus";
import { Icon } from "../Icon";
import { space, type InterfaceTheme } from "../appearance";
import { textStyle } from "./SettingsControls";

export interface PaletteChoice {
  id: string;
  label: string;
  description: string;
  palette: InterfaceTheme["palette"];
  dark: boolean;
  error?: string | null;
}
export interface PaletteCatalogSource {
  catalog(systemDark: boolean): Promise<PaletteChoice[]>;
  subscribe(listener: (event: { rev: number }) => void): { close(): void };
}

/** Colors and labels come from core's catalog, including the cached Ghostty
 * projection. Stable row signals keep focus on a card while its colors change. */
export function createPaletteCatalog(source: PaletteCatalogSource | undefined, visible: () => boolean) {
  const ids = signal<string[]>([]), error = signal(""), loading = signal(false), revision = signal(0);
  const entries = new Map<string, Signal<PaletteChoice>>();
  let alive = true, generation = 0, lastRevision = -1;
  const subscription = source?.subscribe((event) => {
    if (!alive || event.rev <= lastRevision) return;
    lastRevision = event.rev;
    revision.set(revision() + 1);
  });
  cleanup(() => { alive = false; generation++; subscription?.close(); entries.clear(); });
  effect(() => {
    const dark = colorScheme() === "dark";
    revision();
    if (!visible()) return;
    if (!source) { error.set("Color palettes are unavailable in this host."); return; }
    const request = ++generation;
    loading.set(true);
    void source.catalog(dark).then((choices) => {
      if (!alive || request !== generation) return;
      const nextIds: string[] = [];
      for (const choice of choices) {
        if (nextIds.includes(choice.id)) continue;
        nextIds.push(choice.id);
        const existing = entries.get(choice.id);
        if (!existing) entries.set(choice.id, signal(choice));
        else if (JSON.stringify(existing()) !== JSON.stringify(choice)) existing.set(choice);
      }
      // This callback is outside the reactive read: changing a swatch does not
      // subscribe the loader to its own result or reconstruct every card.
      if (ids().join("\u0000") !== nextIds.join("\u0000")) ids.set(nextIds);
      error.set(""); loading.set(false);
    }).catch((reason) => {
      if (alive && request === generation) { error.set(String(reason)); loading.set(false); }
    });
    return () => { generation++; };
  });
  return { ids, entry: (id: string) => entries.get(id)!, error, loading, refresh: () => revision.set(revision() + 1) };
}

export type PaletteCatalog = ReturnType<typeof createPaletteCatalog>;

export function ThemePicker(props: {
  catalog: PaletteCatalog;
  selected: () => string;
  theme: () => InterfaceTheme;
  onChoose: (id: string) => void;
  onNode: (id: string, node: YNode) => void;
  onFocus: (id: string) => void;
  onBlur: (id: string) => void;
}) {
  const theme = props.theme;
  const size = (value: number) => space(value, theme().scale);
  function Card(card: { id: string }) {
    const entry = props.catalog.entry(card.id);
    const selected = () => props.selected() === card.id;
    const colors = () => entry().palette;
    return <Button testID={`theme-choice-${card.id}`} role="radio" accessibilityLabel={entry().label}
      selected={selected()}
      ref={(node) => props.onNode(card.id, node)} onFocus={() => props.onFocus(card.id)} onBlur={() => props.onBlur(card.id)}
      onPress={() => props.onChoose(card.id)}
      style={{ flexGrow: 1, flexBasis: "44%", minWidth: size(145), padding: size(9), gap: size(7), alignItems: "stretch", borderWidth: 1,
        borderColor: selected() ? theme().palette.focus : theme().palette.divider, borderRadius: 8, backgroundColor: selected() ? theme().palette.panelActive : "transparent" }}
      focusStyle={{ borderColor: theme().palette.focus }} hoverStyle={{ backgroundColor: theme().palette.panelActive }}>
      <View accessible={false} pointerEvents="none" style={{ height: size(61), borderRadius: 5, overflow: "hidden", borderWidth: 1, borderColor: colors().divider, backgroundColor: colors().bg }}>
        <View style={{ height: size(13), backgroundColor: colors().panel, flexDirection: "row", alignItems: "center", gap: size(3), paddingHorizontal: size(6) }}>
          {[0, 1, 2].map(() => <View style={{ width: size(3), height: size(3), borderRadius: size(2), backgroundColor: colors().muted }} />)}
        </View>
        <View style={{ flex: 1, flexDirection: "row", gap: size(8), padding: size(7) }}>
          <View style={{ width: "19%", borderRadius: 2, backgroundColor: colors().panelActive }} />
          <View style={{ flex: 1, gap: size(4), justifyContent: "center" }}>
            <View style={{ height: size(3), width: "78%", borderRadius: 2, backgroundColor: colors().text }} />
            <View style={{ height: size(3), width: "58%", borderRadius: 2, backgroundColor: colors().muted }} />
            <View style={{ flexDirection: "row", gap: size(4), marginTop: size(2) }}>
              {["cyan", "magenta", "gold", "working"].map((name) => <View style={{ width: size(13), height: size(5), borderRadius: 2, backgroundColor: colors()[name as keyof InterfaceTheme["palette"]] }} />)}
            </View>
          </View>
        </View>
      </View>
      <View style={{ flexDirection: "row", alignItems: "center", gap: size(5) }}>
        <Text numberOfLines={2} style={{ ...textStyle(theme, 11), flex: 1, fontWeight: selected() ? "600" : "400" }}>{entry().label}</Text>
        {selected() && <Icon name="check" size={12 * theme().scale} color={theme().palette.text} />}
      </View>
      <Text numberOfLines={2} style={textStyle(theme, 10, true)}>{entry().description}</Text>
      {!!entry().error && <Text style={{ ...textStyle(theme, 10, true), color: theme().palette.gold }}>{entry().error}</Text>}
    </Button>;
  }
  return <View style={{ gap: size(8) }}>
    <Text style={textStyle(theme)}>Color palette</Text>
    <Text style={textStyle(theme, 10, true)}>Choose the colors for RiWork. Interface font, text size and layout keep their own settings.</Text>
    <View role="radiogroup" accessibilityLabel="Color palette" style={{ flexDirection: "row", flexWrap: "wrap", gap: size(8) }}>
      {props.catalog.ids().map((id) => <Card id={id} />)}
    </View>
    {props.catalog.loading() && !props.catalog.ids().length && <Text style={textStyle(theme, 10, true)}>Loading color palettes…</Text>}
    {!!props.catalog.error() && <View style={{ gap: size(5) }}>
      <Text style={{ ...textStyle(theme, 10, true), color: theme().palette.gold }}>{props.catalog.error()}</Text>
      <Button onPress={() => props.catalog.refresh()} style={{ alignSelf: "flex-start", paddingHorizontal: size(9), paddingVertical: size(5), borderRadius: 5 }}><Text style={textStyle(theme, 10)}>Reload palettes</Text></Button>
    </View>}
  </View>;
}
