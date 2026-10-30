import { Text, View, effect, keymap, signal, type YNode } from "nsh";
import type { InterfaceTheme } from "../appearance";
import { chatHairline } from "./design";

/** Whether the keyboard focus is in a part of the chat tab that folds away (a
 * side panel, an inline row): each of its focusables reports in with `track`,
 * under a key of its own. */
export function focusWithin() {
  const focused = signal<string | null>(null);
  return {
    inside: () => focused() !== null,
    track: (key: string) => ({
      onFocus: () => focused.set(key),
      onBlur: () => { if (focused() === key) focused.set(null); },
    }),
  };
}

export type FocusWithin = ReturnType<typeof focusWithin>;

/** ⎋ folds the part away while the focus is in it, and only then: elsewhere the
 * composer and the transcript keep their own ⎋. */
export function escapeWithin(focus: FocusWithin, active: () => boolean, fold: () => void) {
  effect(() => { if (active() && focus.inside()) return keymap.bind("escape", fold, { when: "always" }); });
}

/** A side panel's header: what it shows, a muted line under it, and Close. */
export function SideHeader(props: {
  theme: () => InterfaceTheme; title: () => string; detail?: () => string; focus: FocusWithin;
  onClose: () => void; disabled?: () => boolean; closeRef?: (node: YNode) => void;
}) {
  const theme = props.theme;
  const disabled = () => props.disabled?.() ?? false;
  const text = (size: number, color = theme().palette.text) => ({ fontFamily: theme().family, fontSize: size * theme().scale, color });
  return <View style={{ flexDirection: "row", alignItems: "center", gap: 8, paddingHorizontal: 12, paddingVertical: 7, borderBottomWidth: 1, borderColor: chatHairline(theme()) }}>
    <View style={{ flex: 1, minWidth: 0 }}>
      <Text role="header" numberOfLines={1} style={{ ...text(12), fontWeight: "600" }}>{props.title()}</Text>
      {!!props.detail && <Text numberOfLines={1} style={text(10, theme().palette.muted)}>{props.detail()}</Text>}
    </View>
    <View role="button" testID="chat-side-close" ref={(node) => props.closeRef?.(node)} focusable={!disabled()} accessibilityLabel="Close"
      accessibilityState={{ disabled: disabled() }} onPress={() => { if (!disabled()) props.onClose(); }} {...props.focus.track("close")}
      style={{ paddingHorizontal: 8, height: 27, flexShrink: 0, justifyContent: "center", borderRadius: 5, opacity: disabled() ? 0.35 : 1 }}
      hoverStyle={{ backgroundColor: theme().palette.panelActive }} focusStyle={{ backgroundColor: theme().palette.panelActive }}>
      <Text style={text(11, theme().palette.muted)}>Close</Text>
    </View>
  </View>;
}
