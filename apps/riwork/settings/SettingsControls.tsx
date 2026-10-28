import { Text, cleanup, effect, keymap, signal, type JSX, type YNode } from "nsh";
import { Button, Switch, View } from "../SettingsFocus";
import { Settings } from "riwork/native";
import { DEFAULT_THEME, space, type InterfaceTheme } from "../appearance";

export type Theme = () => InterfaceTheme;
export const defaultTheme: Theme = () => DEFAULT_THEME;
export const textStyle = (theme: Theme, size = 11, muted = false) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color: muted ? theme().palette.muted : theme().palette.text });
export const buttonStyle = (theme: Theme) => ({ paddingHorizontal: space(9, theme().scale), paddingVertical: space(6, theme().scale), borderRadius: 5, borderWidth: 1, borderColor: theme().palette.divider, backgroundColor: theme().palette.panel });

export function Section(props: { number: string; title: string; description?: string; theme?: Theme; padding?: number; children?: JSX.Element }) {
  const theme = props.theme ?? defaultTheme;
  return <View testID={`settings-section-${props.number}`} style={{ padding: props.padding ?? space(14, theme().scale), gap: space(10, theme().scale), borderWidth: theme().native ? 0 : 1, borderBottomWidth: 1, borderColor: theme().palette.divider, borderRadius: theme().native ? 0 : 8, backgroundColor: theme().palette.panel }}>
    <View style={{ gap: 4 }}>
      <Text role="header" style={{ ...textStyle(theme, 13), fontWeight: "600" }}>{`${theme().native ? "" : `${props.number}  `}${Settings.cased(props.title, theme().native)}`}</Text>
      {props.description && <Text style={{ ...textStyle(theme, 10, true), maxWidth: 560 }}>{props.description}</Text>}
    </View>
    {props.children}
  </View>;
}

export function ToggleRow(props: { id: string; label: string; description?: string; checked: boolean; disabled?: boolean; onChange: (value: boolean) => void; theme?: Theme }) {
  const theme = props.theme ?? defaultTheme;
  return <Switch testID={props.id} accessibilityLabel={props.label} checked={props.checked} disabled={props.disabled} onChange={props.onChange}
    style={{ paddingVertical: space(8, theme().scale), gap: space(12, theme().scale), alignItems: "flex-start" }}
    trackStyle={{ width: 28, height: 16, borderRadius: 8, backgroundColor: props.checked ? theme().palette.cyan : theme().palette.divider }} thumbStyle={{ width: 12, height: 12, margin: 2, borderRadius: 6, backgroundColor: theme().palette.bg, transform: [{ translateX: props.checked ? 12 : 0 }] }}>
    <View style={{ flex: 1, gap: 4 }}><Text style={textStyle(theme)}>{props.label}</Text>{props.description && <Text style={{ ...textStyle(theme, 10, true), maxWidth: 560 }}>{props.description}</Text>}</View>
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
    <View testID={props.id} role="radiogroup" accessibilityLabel={props.label} style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
      {props.choices.map((choice) => <View testID={choice.id} ref={(node) => refs.set(choice.id, node)} role="radio" accessibilityLabel={choice.label}
        disabled={!!props.disabled || !!choice.disabled}
        accessibilityState={{ checked: props.value === choice.id, selected: props.value === choice.id, disabled: !!props.disabled || !!choice.disabled }}
        focusable={!props.disabled && !choice.disabled && (tabStop() === choice.id || focused() === choice.id)}
        onFocus={() => { if (alive) focused.set(choice.id); }} onBlur={() => { if (alive) focused.set(null); }} onPress={() => choose(choice.id)}
        style={{ ...buttonStyle(theme), opacity: props.disabled || choice.disabled ? 0.45 : 1, borderColor: props.value === choice.id ? theme().palette.cyan : theme().palette.divider, backgroundColor: props.value === choice.id ? theme().palette.panelActive : theme().palette.panel }}
        focusStyle={{ borderColor: theme().palette.focus }}>
        <Text style={textStyle(theme, 10)}>{choice.label}</Text>
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
      <Button testID={props.decreaseId ?? `${props.id}-decrease`} accessibilityLabel={`Decrease ${props.label}`} disabled={props.disabled || props.value <= props.minimum} onPress={() => step(-1)} style={buttonStyle(theme)}><Text style={textStyle(theme)}>−</Text></Button>
      <Text testID={`${props.id}-value`} style={textStyle(theme)}>{String(props.value)}</Text>
      <Button testID={props.increaseId ?? `${props.id}-increase`} accessibilityLabel={`Increase ${props.label}`} disabled={props.disabled || props.value >= props.maximum} onPress={() => step(1)} style={buttonStyle(theme)}><Text style={textStyle(theme)}>+</Text></Button>
      {props.children}
    </View>
  </View>;
}
