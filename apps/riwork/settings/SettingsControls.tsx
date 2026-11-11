import { Text, cleanup, effect, keymap, signal, type JSX, type YNode } from "nsh";
import { Button, Switch, View } from "../SettingsFocus";
import { DEFAULT_THEME, space, type InterfaceTheme } from "../appearance";
import { PANEL } from "../panel_layout";
import { focusRing, mix, strongLine, switchParts } from "../PanelKit";

export type Theme = () => InterfaceTheme;
export const defaultTheme: Theme = () => DEFAULT_THEME;
export const textStyle = (theme: Theme, size = 11, muted = false) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color: muted ? theme().palette.muted : theme().palette.text });
/** A plain button: an 8-point rounded rectangle on the card color with a hairline. */
export const buttonStyle = (theme: Theme) => ({ paddingHorizontal: space(10, theme().scale), paddingVertical: space(5, theme().scale), borderRadius: space(8, theme().scale), borderWidth: 1, borderColor: theme().palette.divider, backgroundColor: theme().palette.bg });
/** A plain button's hover, focus and disabled looks: a darker hairline, a ring, dimmed. */
export const buttonStates = (theme: Theme) => ({ hoverStyle: { borderColor: strongLine(theme().palette) }, focusStyle: focusRing(theme().palette), disabledStyle: { opacity: 0.45 } });

export function Section(props: { number: string; title: string; description?: string; theme?: Theme; padding?: number; children?: JSX.Element }) {
  const theme = props.theme ?? defaultTheme;
  return <View testID={`settings-section-${props.number}`} style={{ paddingHorizontal: space(PANEL.gutter, theme().scale), paddingTop: space(14, theme().scale), paddingBottom: space(18, theme().scale), gap: space(12, theme().scale), borderWidth: 0, borderBottomWidth: 1, borderColor: theme().palette.divider, borderRadius: 0, backgroundColor: "transparent" }}>
    <View style={{ gap: space(3, theme().scale) }}>
      {/* Above the 11 pt row labels and the 11 pt semibold subheadings inside a section. */}
      <Text role="header" style={{ ...textStyle(theme, 12), fontWeight: "700" }}>{props.title}</Text>
      {props.description && <Text style={{ ...textStyle(theme, 10, true), maxWidth: 560 }}>{props.description}</Text>}
    </View>
    {props.children}
  </View>;
}

const TRACK_HEIGHT = 22;
/** One semantic switch: the label (and the description under it) on the left, the switch on the
 * right, at least 44 points tall. The track is the hairline when off and ink when on. */
export function ToggleRow(props: { id: string; label: string; description?: string; checked: boolean; disabled?: boolean; onChange: (value: boolean) => void; theme?: Theme; onRef?: (node: YNode) => void }) {
  const theme = props.theme ?? defaultTheme;
  // The label's first line is about 1.2 times its size tall; whichever of it and the track is
  // shorter moves down to center on the other, so the switch sits on the first line.
  const offset = () => (12 * theme().scale * 1.2 - space(TRACK_HEIGHT, theme().scale)) / 2;
  const parts = () => switchParts(theme, props.checked);
  // The switch stays near its label even in a wide settings column.
  return <Switch testID={props.id} ref={(node) => props.onRef?.(node)} accessibilityLabel={props.label} checked={props.checked} disabled={props.disabled} onChange={props.onChange}
    style={{ flexDirection: "row-reverse", alignItems: props.description ? "flex-start" : "center", justifyContent: "space-between", alignSelf: "flex-start", width: "100%", maxWidth: space(560, theme().scale), minHeight: space(44, theme().scale),
      paddingVertical: space(8, theme().scale), gap: space(16, theme().scale), borderRadius: space(8, theme().scale), opacity: props.disabled ? 0.45 : 1 }}
    focusStyle={focusRing(theme().palette, 2)}
    trackStyle={{ ...parts().trackStyle, marginTop: props.description ? Math.max(0, offset()) : 0 }} thumbStyle={parts().thumbStyle}>
    <View style={{ flex: 1, minWidth: 0, paddingTop: props.description ? Math.max(0, -offset()) : 0, gap: 3 }}><Text style={textStyle(theme)}>{props.label}</Text>{props.description && <Text style={{ ...textStyle(theme, 10, true), maxWidth: 560 }}>{props.description}</Text>}</View>
  </Switch>;
}

export interface Choice { id: string; label: string; description?: string; disabled?: boolean }
/** A selected chip is the only tab stop; arrows wrap, select and save. */
export function ChoiceRow(props: { id: string; label: string; description?: string; value: string; choices: Choice[]; onChange: (value: string) => void; disabled?: boolean; theme?: Theme }) {
  const theme = props.theme ?? defaultTheme, focused = signal<string | null>(null);
  let alive = true;
  const refs = new Map<string, YNode>();
  const tabStop = () => props.choices.find((choice) => !choice.disabled && choice.id === props.value)?.id ?? props.choices.find((choice) => !choice.disabled)?.id;
  function choose(value: string) {
    if (!alive) return;
    if (props.disabled || props.choices.find((choice) => choice.id === value)?.disabled) return;
    focused.set(value); props.onChange(value); refs.get(value)?.focus();
  }
  effect(() => {
    if (!focused() || props.disabled) return;
    const bindings: (() => void)[] = [];
    for (const [key, step] of [["left", -1], ["up", -1], ["right", 1], ["down", 1]] as const) bindings.push(keymap.bind(key, () => {
      const choices = props.choices.filter((choice) => !choice.disabled);
      if (!choices.length) return;
      const index = Math.max(0, choices.findIndex((choice) => choice.id === focused()));
      choose(choices[(index + step + choices.length) % choices.length].id);
    }));
    for (const key of ["enter", "space"]) bindings.push(keymap.bind(key, () => { const value = focused(); if (value) choose(value); }));
    return () => { for (const unbind of bindings) unbind(); };
  });
  cleanup(() => { alive = false; refs.clear(); });
  return <View testID={`setting-row-${props.id}`} style={{ gap: 6, paddingVertical: space(7, theme().scale) }}>
    <Text style={textStyle(theme)}>{props.label}</Text>
    {props.description && <Text style={{ ...textStyle(theme, 10, true), maxWidth: 560 }}>{props.description}</Text>}
    <View testID={props.id} role="radiogroup" accessibilityLabel={props.label} style={{ flexDirection: "row", flexWrap: "wrap", alignSelf: "flex-start", maxWidth: "100%", gap: space(2, theme().scale), padding: space(2, theme().scale), borderRadius: space(9, theme().scale), backgroundColor: theme().palette.panelActive }}>
      {props.choices.map((choice) => <View testID={choice.id} ref={(node) => refs.set(choice.id, node)} role="radio" accessibilityLabel={choice.label}
        disabled={!!props.disabled || !!choice.disabled}
        accessibilityState={{ checked: props.value === choice.id, selected: props.value === choice.id, disabled: !!props.disabled || !!choice.disabled }}
        focusable={!props.disabled && !choice.disabled && (tabStop() === choice.id || focused() === choice.id)}
        onFocus={() => { if (alive) focused.set(choice.id); }} onBlur={() => { if (alive) focused.set(null); }} onPress={() => choose(choice.id)}
        style={{ ...buttonStyle(theme), borderRadius: space(7, theme().scale), opacity: props.disabled || choice.disabled ? 0.45 : 1, borderColor: props.value === choice.id ? theme().palette.divider : "transparent", backgroundColor: props.value === choice.id ? theme().palette.bg : "transparent" }}
        hoverStyle={props.value === choice.id ? {} : { backgroundColor: mix(theme().palette.panelActive, theme().palette.text, 0.06) }} focusStyle={focusRing(theme().palette, 1)}>
        <Text style={{ ...textStyle(theme, 10), fontWeight: "500", color: props.value === choice.id ? theme().palette.text : theme().palette.muted }}>{choice.label}</Text>
        {choice.description && <Text style={textStyle(theme, 9, true)}>{choice.description}</Text>}
      </View>)}
    </View>
  </View>;
}

export function StepperRow(props: { id: string; label: string; description?: string; value: number; minimum: number; maximum: number; disabled?: boolean; decreaseId?: string; increaseId?: string; onChange: (value: number) => void; theme?: Theme; children?: JSX.Element }) {
  const theme = props.theme ?? defaultTheme, focused = signal(false);
  let alive = true;
  cleanup(() => { alive = false; });
  const step = (direction: number) => { if (alive && !props.disabled) props.onChange(Math.max(props.minimum, Math.min(props.maximum, props.value + direction))); };
  effect(() => {
    if (!focused() || props.disabled) return;
    const bindings: (() => void)[] = [];
    for (const [key, direction] of [["up", 1], ["right", 1], ["=", 1], ["shift+=", 1], ["down", -1], ["left", -1], ["-", -1]] as const) bindings.push(keymap.bind(key, () => step(direction)));
    return () => { for (const unbind of bindings) unbind(); };
  });
  return <View testID={props.id} focusable={true} role="adjustable" accessibilityLabel={props.label} onFocus={() => { if (alive) focused.set(true); }} onBlur={() => { if (alive) focused.set(false); }} style={{ gap: 5, paddingVertical: space(8, theme().scale) }}>
    <Text style={textStyle(theme)}>{props.label}</Text>{props.description && <Text style={textStyle(theme, 10, true)}>{props.description}</Text>}
    <View style={{ flexDirection: "row", gap: 8, alignItems: "center" }}>
      <Button testID={props.decreaseId ?? `${props.id}-decrease`} accessibilityLabel={`Decrease ${props.label}`} disabled={props.disabled || props.value <= props.minimum} onPress={() => step(-1)} style={buttonStyle(theme)} {...buttonStates(theme)}><Text style={textStyle(theme)}>−</Text></Button>
      <Text testID={`${props.id}-value`} style={textStyle(theme)}>{String(props.value)}</Text>
      <Button testID={props.increaseId ?? `${props.id}-increase`} accessibilityLabel={`Increase ${props.label}`} disabled={props.disabled || props.value >= props.maximum} onPress={() => step(1)} style={buttonStyle(theme)} {...buttonStates(theme)}><Text style={textStyle(theme)}>+</Text></Button>
      {props.children}
    </View>
  </View>;
}
