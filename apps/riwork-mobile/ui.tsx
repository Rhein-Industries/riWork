/** The touch pieces every iPhone and iPad page is drawn with, in the theme Swift resolved
 * (`platform.theme`): 44-point targets, system text sizes times the display scale, SF Symbols on the
 * iPhone and the Mac's icons (`Icon`) on the iPad. The iPad draws the Mac's sizes in touch density, as the
 * shared dashboard's `touch` mode does: its 13-point text at 17, 11 at 15, 10 at 13 and 9 at 12. */
import { Text, View, safeAreaInsets, type IOSNavPageHostProps, type JSX, type YNode } from "nsh";
import { NavPage as BaseNavPage, type NavPageProps } from "nsh/base";
import { Icon, type IconName } from "../riwork/Icon";
import type { MobileApp } from "./app_state";
import { Symbol } from "./Symbol";

/** NSH 0.3.0 reads a page's `ios` props (`IOSNavPageHostProps`: the header's system Add button) but leaves them out of `NavPageProps`. */
const NavPage = BaseNavPage as (props: NavPageProps & { ios?: IOSNavPageHostProps }) => YNode;

/** How a page is placed: a page of the phone's navigation stack, or a pane of the iPad's columns. */
export type Layout = "phone" | "pad";
export type Tone = "text" | "muted" | "accent" | "working" | "gold" | "error";

export type Kit = ReturnType<typeof kit>;

export function kit(app: MobileApp) {
  const palette = () => app.theme().theme.palette;
  const scale = () => { const value = app.theme().theme.scale; return Number.isFinite(value) && value > 0 ? value : 1; };
  const pt = (size: number) => size * scale();
  const target = () => Math.max(44, pt(44));
  const color = (tone: Tone | "error" = "text") => {
    const colors = palette();
    switch (tone) {
      case "muted": return colors.muted;
      case "accent": return colors.cyan;
      case "working": return colors.working;
      case "gold": return colors.gold;
      case "error": return colors.danger;
      default: return colors.text;
    }
  };
  const text = (size = 15, tone: Tone = "text") => ({ fontFamily: app.theme().theme.family, fontSize: pt(size), color: color(tone) });
  const dark = () => app.theme().dark;
  /** How wide the window's own controls are at its top leading corner (iPadOS 26): the top row's first element leaves it clear. */
  const windowControls = () => Math.max(0, app.overview().windowControls ?? 0);

  /** A text button; `symbol` an SF Symbol before it (the iPhone), `icon` one of the Mac's (the iPad). */
  function Action(props: { label: string; symbol?: string; icon?: IconName; action: () => void; disabled?: boolean; prominent?: boolean; compact?: boolean; destructive?: boolean; testID?: string }) {
    const tint = () => props.destructive ? color("error") : color("accent");
    return <View testID={props.testID} role="button" accessibilityLabel={props.label} disabled={props.disabled} onPress={props.action}
      activeStyle={{ backgroundColor: palette().panelActive }} disabledStyle={{ opacity: 0.45 }}
      style={{ minHeight: target(), minWidth: target(), flexDirection: "row", alignItems: "center", justifyContent: "center", gap: pt(8),
        paddingHorizontal: pt(props.compact ? 8 : 14), paddingVertical: pt(8), borderRadius: pt(10),
        backgroundColor: props.prominent ? palette().panelActive : "transparent" }}>
      {props.symbol && <Symbol name={props.symbol} color={tint()} size={pt(19)} />}
      {props.icon && <Icon name={props.icon} color={tint()} size={pt(17)} />}
      {(!props.compact || !(props.symbol || props.icon)) && <Text style={{ ...text(15), color: tint(), fontWeight: props.prominent ? "600" : "400" }}>{props.label}</Text>}
    </View>;
  }
  /** One of the Mac's toolbar buttons in touch size (the iPad): its icon, muted unless `selected`, which also
   * draws it on the rounded highlight; its label for VoiceOver. */
  function Tool(props: { label: string; icon: IconName; action: () => void; selected?: boolean; disabled?: boolean; testID?: string }) {
    return <View testID={props.testID} role="button" accessibilityLabel={props.label} accessibilityState={{ selected: !!props.selected }}
      disabled={props.disabled} onPress={props.action} activeStyle={{ backgroundColor: palette().panelActive }} disabledStyle={{ opacity: 0.4 }}
      style={{ width: target(), height: target(), flexShrink: 0, alignItems: "center", justifyContent: "center", borderRadius: pt(10),
        backgroundColor: props.selected ? palette().panelActive : "transparent" }}>
      <Icon name={props.icon} color={props.selected ? palette().text : palette().muted} size={pt(18)} />
    </View>;
  }
  /** A symbol-only button with its label for VoiceOver. */
  function IconButton(props: { label: string; symbol: string; action: () => void; disabled?: boolean; testID?: string; tone?: Tone }) {
    return <View testID={props.testID} role="button" accessibilityLabel={props.label} disabled={props.disabled} onPress={props.action}
      activeStyle={{ backgroundColor: palette().panelActive }} disabledStyle={{ opacity: 0.45 }}
      style={{ minWidth: target(), minHeight: target(), alignItems: "center", justifyContent: "center", borderRadius: pt(10) }}>
      <Symbol name={props.symbol} color={color(props.tone ?? "accent")} size={pt(20)} />
    </View>;
  }
  function Section(props: { title?: string; children?: JSX.Element }) {
    return <View style={{ gap: pt(8) }}>
      {props.title && <Text role="header" style={{ ...text(13, "muted"), fontWeight: "600", paddingHorizontal: pt(4) }}>{props.title}</Text>}
      <View style={{ backgroundColor: palette().panel, borderRadius: pt(12), overflow: "hidden" }}>{props.children}</View>
    </View>;
  }
  function Divider() { return <View pointerEvents="none" accessible={false} style={{ height: 0.5, marginLeft: pt(16), backgroundColor: palette().divider }} />; }
  function Note(props: { title?: string; message: string; error?: boolean; children?: JSX.Element }) {
    return <View style={{ padding: pt(16), gap: pt(8), backgroundColor: palette().panel, borderRadius: pt(12) }}>
      {props.title && <Text role="header" style={{ ...text(17), fontWeight: "600" }}>{props.title}</Text>}
      <Text style={text(14, props.error ? "error" : "muted")}>{props.message}</Text>
      {props.children}
    </View>;
  }
  /** What the last call to Swift said, with Dismiss. */
  function Failure() {
    return <>{app.failure() && <Note message={app.failure()} error={true}>
      <Action label="Dismiss" compact={true} action={() => app.failure.set("")} />
    </Note>}</>;
  }
  /** A tab's or a session's state, as the Mac's strip draws it: a dot in its color. */
  function StatusDot(props: { status: string }) {
    const tone = (): Tone | null => props.status === "working" ? "working" : props.status === "waiting" ? "gold" : props.status === "error" ? "error"
      : props.status === "done" ? "accent" : null;
    return <>{tone() && <View pointerEvents="none" accessible={false}
      style={{ width: pt(7), height: pt(7), borderRadius: pt(4), backgroundColor: color(tone()!) }} />}</>;
  }
  /** One page: on the phone a page of the navigation stack with the system header (and swipe back); on the
   * iPad the window under a top row of its own (the Hosts page; a project's page and the dashboard draw
   * the window's top row themselves, `shell/chrome.tsx`). */
  function Page(props: {
    layout: Layout; title: string; testID?: string; backTitle?: string; right?: JSX.Element; addButton?: boolean;
    onDismiss?: () => void; children?: JSX.Element;
  }) {
    if (props.layout === "phone") {
      return <NavPage testID={props.testID} title={props.title} headerBackTitle={props.backTitle} onDismiss={props.onDismiss}
        headerStyle={{ backgroundColor: palette().bg, color: palette().text, tintColor: palette().cyan }}
        ios={props.addButton ? { headerRightSystemItem: "add" } : undefined} headerRight={props.right}
        statusBar={{ foreground: dark() ? "light" : "dark" }} style={{ flex: 1, backgroundColor: palette().bg }}>
        {props.children}
      </NavPage>;
    }
    return <View testID={props.testID} style={{ flex: 1, minWidth: 0, backgroundColor: palette().bg }}>
      <View style={{ flexDirection: "row", alignItems: "center", height: target(), flexShrink: 0, paddingLeft: windowControls() + pt(16), paddingRight: pt(6), gap: pt(4),
        backgroundColor: palette().panel, borderBottomWidth: 1, borderColor: palette().divider }}>
        <Text role="header" numberOfLines={1} style={{ ...text(15), fontWeight: "600", flex: 1 }}>{props.title}</Text>
        {props.right}
      </View>
      {props.children}
    </View>;
  }
  /** A setting row with an on/off switch in the system's look (the whole row toggles it). */
  function Toggle(props: { label: string; detail?: string; checked: boolean; disabled?: boolean; onChange: (checked: boolean) => void; testID?: string }) {
    return <View testID={props.testID} role="switch" accessibilityLabel={props.label} accessibilityState={{ checked: props.checked, disabled: props.disabled }}
      disabled={props.disabled} disabledStyle={{ opacity: 0.45 }} onPress={() => props.onChange(!props.checked)}
      style={{ flexDirection: "row", alignItems: "center", gap: pt(12), minHeight: target() }}>
      <View style={{ flex: 1, gap: pt(3) }}>
        <Text style={text(15)}>{props.label}</Text>
        {props.detail && <Text style={text(12, "muted")}>{props.detail}</Text>}
      </View>
      <View pointerEvents="none" accessible={false} style={{ width: 51, height: 31, borderRadius: 16, padding: 2,
        backgroundColor: props.checked ? palette().working : palette().panelActive, alignItems: props.checked ? "flex-end" : "flex-start" }}>
        <View style={{ width: 27, height: 27, borderRadius: 14, backgroundColor: "#ffffff" }} />
      </View>
    </View>;
  }
  /** The bottom of a scrolling page: clear of the home indicator. */
  function bottomInset() { const insets = safeAreaInsets(); return () => insets().bottom; }

  return { palette, pt, target, color, text, dark, windowControls, Action, Tool, IconButton, Section, Divider, Note, Failure, StatusDot, Toggle, Page, bottomInset };
}
