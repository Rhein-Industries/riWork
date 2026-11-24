/** The shapes the side panels share (the "Paper & Clay" language), in every palette: buttons
 * that are 8-point rounded rectangles on the card color with a hairline, one filled primary
 * action per view, a segmented filter on a quiet track, a status pill, a search field and a
 * card. Colors come from the palette; only the shapes are fixed here. Shared code: it imports
 * nothing native, so the dashboard on iOS draws with it too. */
import { Text, View, type JSX } from "nsh";
import { Button } from "nsh/base";
import { Icon, type IconName } from "./Icon";
import { space, type InterfaceTheme } from "./theme";

export type KitTheme = () => InterfaceTheme;
type Palette = InterfaceTheme["palette"];

// ---- Colors ----------------------------------------------------------------------------------

function channels(color: string): [number, number, number] {
  const hex = color.replace("#", "");
  const value = hex.length === 3 ? hex.split("").map((digit) => digit + digit).join("") : hex.slice(0, 6);
  const number = parseInt(value, 16);
  return Number.isNaN(number) ? [0, 0, 0] : [(number >> 16) & 255, (number >> 8) & 255, number & 255];
}
const toHex = (rgb: number[]) => `#${rgb.map((value) => Math.round(Math.max(0, Math.min(255, value))).toString(16).padStart(2, "0")).join("")}`;

/** `first` moved `amount` (0 to 1) of the way to `second`, as core's `theme::mix`. */
export function mix(first: string, second: string, amount: number): string {
  const a = channels(first), b = channels(second);
  return toHex(a.map((value, index) => value + (b[index] - value) * amount));
}

function luminance(color: string): number {
  const linear = (value: number) => { const v = value / 255; return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; };
  const [r, g, b] = channels(color);
  return 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
}

/** A palette reads as dark when its card color does, as core's `theme::is_dark`. */
export const isDark = (palette: Palette) => luminance(palette.bg) < 0.5;

/** A light, quiet background of `color`'s own hue for a pill: about 91% light on a light
 * palette, a dark tint of the card color on a dark one. Grays fall back to the quiet fill. */
function tint(color: string, palette: Palette): string {
  if (isDark(palette)) return mix(palette.bg, color, 0.2);
  const [r, g, b] = channels(color).map((value) => value / 255);
  const max = Math.max(r, g, b), min = Math.min(r, g, b), lightness = (max + min) / 2;
  const chroma = max - min;
  if (chroma < 0.04) return palette.panelActive;
  const saturation = chroma / (1 - Math.abs(2 * lightness - 1));
  let hue = max === r ? ((g - b) / chroma) % 6 : max === g ? (b - r) / chroma + 2 : (r - g) / chroma + 4;
  hue = (hue * 60 + 360) % 360;
  const s = Math.min(saturation * 0.8, 0.7), l = 0.91;
  const c = (1 - Math.abs(2 * l - 1)) * s, x = c * (1 - Math.abs((hue / 60) % 2 - 1)), m = l - c / 2;
  const [r1, g1, b1] = hue < 60 ? [c, x, 0] : hue < 120 ? [x, c, 0] : hue < 180 ? [0, c, x] : hue < 240 ? [0, x, c] : hue < 300 ? [x, 0, c] : [c, 0, x];
  return toHex([(r1 + m) * 255, (g1 + m) * 255, (b1 + m) * 255]);
}

/** The hairline a little darker (or, on a dark palette, lighter): hover and focus on a field. */
export const strongLine = (palette: Palette) => mix(palette.divider, palette.text, 0.22);

/** The primary color pressed or hovered: a step darker, or lighter when it is near black. */
const primaryHover = (palette: Palette) => luminance(palette.primary) < 0.03 ? mix(palette.primary, palette.onPrimary, 0.18) : mix(palette.primary, "#000000", 0.1);

// ---- Status --------------------------------------------------------------------------------

/** What a state means to the person: something waits for them, works, failed, is live or
 * done, or none of these. Never the primary (clay) color. */
export type Tone = "needs" | "working" | "failed" | "done" | "neutral";

/** Done and live have no palette color of their own: a green that reads on the palette. */
const success = (palette: Palette) => isDark(palette) ? "#8ccb9b" : "#24603d";

export function toneColor(palette: Palette, tone: Tone): string {
  return tone === "needs" ? palette.gold : tone === "working" ? palette.working : tone === "failed" ? palette.danger : tone === "done" ? success(palette) : palette.muted;
}

export function toneColors(palette: Palette, tone: Tone): { fg: string; bg: string; dot: string } {
  if (tone === "neutral") return { fg: palette.muted, bg: palette.panelActive, dot: mix(palette.muted, palette.panelActive, 0.25) };
  const fg = toneColor(palette, tone);
  return { fg, bg: tint(fg, palette), dot: fg };
}

/** A state as a small pill: a dot and a word on a tint of the state's color. It keeps its own
 * width in a column; `inline` centers it on a row taller than itself. */
export function StatusPill(props: { theme: KitTheme; tone: Tone; label: string; id?: string; accessibilityLabel?: string; inline?: boolean }) {
  const scale = () => props.theme().scale, colors = () => toneColors(props.theme().palette, props.tone);
  return <View testID={props.id} accessibilityLabel={props.accessibilityLabel ?? props.label}
    style={{ flexDirection: "row", alignItems: "center", alignSelf: props.inline ? "center" : "flex-start", flexShrink: 0, gap: space(4, scale()), minHeight: space(19, scale()),
      paddingLeft: space(7, scale()), paddingRight: space(8, scale()), borderRadius: space(10, scale()), backgroundColor: colors().bg }}>
    {/* A glyph, not a rounded view: the host draws small rounded views as squircles. */}
    <Text accessible={false} style={{ fontSize: 7 * scale(), color: colors().dot }}>●</Text>
    <Text style={{ fontFamily: props.theme().family, fontSize: 10 * scale(), fontWeight: "500", color: colors().fg, numberOfLines: 1 }}>{props.label}</Text>
  </View>;
}

// ---- Controls ------------------------------------------------------------------------------

/** Keyboard focus: a 2-point ring in the focus color, set off by `offset` (negative inside). */
export const focusRing = (palette: Palette, offset = 2) => ({ outlineStyle: "solid" as const, outlineWidth: 2, outlineOffset: offset, outlineColor: palette.focus });

/** `default`: the card color with a hairline. `primary`: the view's one main action, filled with
 * the primary color; disabled, it falls back to the quiet fill with a hairline. `danger`: the
 * default shape with a red label. `quiet`: no edge until hovered. `link`: words that act. */
export type ButtonKind = "default" | "primary" | "danger" | "quiet" | "link";

export function buttonLook(theme: KitTheme, kind: ButtonKind = "default", disabled = false) {
  const p = theme().palette, s = theme().scale;
  const filled = kind === "primary" && !disabled;
  const base = kind === "link" ? { paddingHorizontal: space(2, s), paddingVertical: space(2, s), borderRadius: space(4, s) }
    : { minHeight: space(26, s), paddingHorizontal: space(10, s), borderRadius: space(8, s), borderWidth: 1, justifyContent: "center" as const };
  const fill = kind === "link" || kind === "quiet" ? "transparent" : filled ? p.primary : kind === "primary" ? p.panelActive : p.bg;
  const edge = kind === "link" || kind === "quiet" || filled ? "transparent" : p.divider;
  const label = disabled ? p.muted : filled ? p.onPrimary : kind === "danger" ? p.danger : kind === "quiet" ? p.muted : p.text;
  return {
    style: { ...base, flexDirection: "row" as const, alignItems: "center" as const, gap: space(5, s), backgroundColor: fill, ...(kind === "link" ? {} : { borderColor: edge }) },
    hoverStyle: kind === "link" ? { opacity: 0.75 } : filled ? { backgroundColor: primaryHover(p) } : kind === "quiet" ? { backgroundColor: p.panelActive } : { borderColor: strongLine(p) },
    focusStyle: focusRing(p, kind === "link" ? 1 : 2),
    disabledStyle: { opacity: kind === "primary" ? 1 : 0.5 },
    label: { fontFamily: theme().family, fontSize: 11 * s, fontWeight: "500" as const, color: label, ...(kind === "link" ? { textDecorationLine: "underline" as const } : {}) },
    icon: label,
  };
}

/** A text button in one of the kinds above. */
export function KitButton(props: { theme: KitTheme; label: string; kind?: ButtonKind; id?: string; accessibilityLabel?: string; disabled?: boolean; icon?: IconName; onPress: () => void; children?: JSX.Element }) {
  const look = () => buttonLook(props.theme, props.kind, props.disabled);
  return <Button testID={props.id} accessibilityLabel={props.accessibilityLabel ?? props.label} disabled={props.disabled} onPress={props.onPress}
    style={look().style} hoverStyle={look().hoverStyle} focusStyle={look().focusStyle} disabledStyle={look().disabledStyle}>
    {props.icon && <Icon name={props.icon} size={12 * props.theme().scale} color={look().icon} />}
    <Text style={{ ...look().label, numberOfLines: 1 }}>{props.children ?? props.label}</Text>
  </Button>;
}

export interface Segment {
  id?: string;
  /** What it shows; an icon segment shows `icon` and says `label` to screen readers. */
  label: string;
  icon?: IconName;
  count?: number | string;
  selected: boolean;
  disabled?: boolean;
  accessibilityLabel?: string;
  onPress: () => void;
}

/** A filter or a view mode: a quiet track with the chosen segment raised on the card color
 * with a hairline. Counts are muted tabular figures. A row too long for its panel wraps. */
export function Segmented(props: { theme: KitTheme; segments: Segment[]; id?: string; accessibilityLabel?: string; role?: "tab" | "radio"; touch?: boolean; stretch?: boolean; compact?: boolean }) {
  const p = () => props.theme().palette, s = () => props.theme().scale;
  return <View testID={props.id} role={props.role === "tab" ? "tablist" : props.role === "radio" ? "radiogroup" : undefined} accessibilityLabel={props.accessibilityLabel}
    style={{ flexDirection: "row", flexWrap: "wrap", alignSelf: props.stretch ? "stretch" : "flex-start", maxWidth: "100%", minWidth: 0, gap: space(2, s()), padding: space(2, s()), borderRadius: space(9, s()), backgroundColor: p().panelActive }}>
    {props.segments.map((segment) => <Button testID={segment.id} accessibilityLabel={segment.accessibilityLabel ?? segment.label} role={props.role ?? null}
      selected={segment.selected} disabled={segment.disabled} onPress={segment.onPress}
      style={{ flexDirection: "row", alignItems: "center", justifyContent: "center", gap: space(5, s()), minHeight: space(props.touch ? 40 : 24, s()), minWidth: space(segment.icon ? 30 : 0, s()),
        paddingHorizontal: space(segment.icon ? 6 : props.compact ? 7 : 9, s()), borderRadius: space(7, s()), borderWidth: 1,
        borderColor: segment.selected ? p().divider : "transparent", backgroundColor: segment.selected ? p().bg : "transparent", opacity: segment.disabled && !segment.selected ? 0.45 : 1 }}
      hoverStyle={segment.selected ? {} : { backgroundColor: mix(p().panelActive, p().text, 0.06) }} focusStyle={focusRing(p(), 1)}>
      {segment.icon ? <Icon name={segment.icon} size={14 * s()} color={segment.selected ? p().text : p().muted} />
        : <Text style={{ fontFamily: props.theme().family, fontSize: 10.5 * s(), fontWeight: "500", color: segment.selected ? p().text : p().muted, numberOfLines: 1 }}>{segment.label}</Text>}
      {segment.count != null && (!props.compact || segment.selected) && <Text style={{ fontFamily: props.theme().family, fontSize: 10 * s(), fontVariant: ["tabular-nums"], color: p().muted }}>{String(segment.count)}</Text>}
    </Button>)}
  </View>;
}

/** A search or filter field: the card color with a hairline that darkens while it is hovered
 * or focused. Its input goes in `children`; `focused` says whether that input has focus. */
export function searchBox(theme: KitTheme, focused: boolean) {
  const p = theme().palette, s = theme().scale;
  return {
    style: { flexDirection: "row" as const, alignItems: "center" as const, gap: space(6, s), minWidth: 0, height: space(30, s), paddingHorizontal: space(10, s),
      borderRadius: space(12, s), borderWidth: 1, borderColor: focused ? strongLine(p) : p.divider, backgroundColor: p.bg },
    hoverStyle: { borderColor: strongLine(p) },
  };
}

/** A self-contained object (an account, an automation, an add-in, a decision): the card color, a
 * hairline and a 14-point radius, no shadow. Lists sit on the panel itself instead. */
export function cardStyle(theme: KitTheme, padding = 12) {
  const p = theme().palette, s = theme().scale;
  return { padding: space(padding, s), borderRadius: space(14, s), borderWidth: 1, borderColor: p.divider, backgroundColor: p.bg };
}

/** An on-off switch's parts: 36 by 22, the hairline when off and ink when on. */
export function switchParts(theme: KitTheme, checked: boolean) {
  const p = theme().palette, s = theme().scale;
  return {
    trackStyle: { width: space(36, s), height: space(22, s), flexShrink: 0, borderRadius: space(11, s), justifyContent: "center" as const, backgroundColor: checked ? p.text : p.divider },
    thumbStyle: { width: space(16, s), height: space(16, s), margin: space(3, s), borderRadius: space(8, s), backgroundColor: checked || !isDark(p) ? p.bg : p.muted,
      transform: [{ translateX: checked ? space(14, s) : 0 }] },
  };
}
