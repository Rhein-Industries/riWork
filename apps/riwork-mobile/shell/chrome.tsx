/** The iPad window's own pieces around its pages (`PadShell`), drawn as the Mac's RiWork window draws them
 * (`apps/riwork/Workspace.tsx`, `StatusBar.tsx`) in touch density: the top row across the window, the
 * sidebar toggle and Dashboard that start a page's part of it while the sidebar does not, a tab's state dot
 * as the Mac's strip draws it, the link's dot, the host menu and the status bar along the bottom. */
import { Text, View, safeAreaInsets, signal, type Accessor, type JSX, type Signal } from "nsh";
import { AlertDialog, type AlertDialogAction } from "nsh/base";
import { Icon } from "../../riwork/Icon";
import { PortError, type LinkState } from "../../riwork/client/ports";
import type { MobileApp, ProjectRoute } from "../app_state";
import type { Kit } from "../ui";

/** The sidebar's width, the Mac's Projects panel's in touch density. */
export const SIDEBAR_WIDTH = 300;
/** From this window width the sidebar stands beside the main area, open by default; narrower it opens over it. */
export const DOCKED_WIDTH = 900;

/** A project's tabs by state, for the status bar. */
export interface TabCounts { working: number; waiting: number; done: number; error: number }
/** What the project page on screen tells the status bar: its tabs by state and its live terminals (null before the first read). */
export interface PadStatus { projectId: string; counts: TabCounts; live: number | null }

/** What the iPad's window gives the pages it holds. */
export interface PadChrome {
  /** The sidebar stands open beside the main area: it starts the top row. */
  docked: Accessor<boolean>;
  /** The sidebar is open over the main area (a window narrower than `DOCKED_WIDTH`). */
  overlay: Accessor<boolean>;
  /** Shows or hides the sidebar (its toggle, ⌘B). */
  toggle(): void;
  /** Shows the sidebar (Clicks Tab P). */
  reveal(): void;
  /** Closes the sidebar where it is open over the main area: a project or the dashboard was chosen. */
  close(): void;
  /** A menu of the window (the host menu) is open: the page takes no keys meanwhile. */
  menuOpen: Accessor<boolean>;
  /** The host menu: the other desktops, All desktops, Add desktop, the desktop's details, Connect or Disconnect, the settings. */
  hostMenu(): void;
  /** What the project page on screen says for the status bar. */
  status: Signal<PadStatus | null>;
}

export function createPadChrome(app: MobileApp, host: Accessor<string | null>, wide: Accessor<boolean>): PadChrome {
  const dockedOpen = signal(true), overlayOpen = signal(false), menuOpen = signal(false);
  const status = signal<PadStatus | null>(null);
  async function hostMenu() {
    const hostId = host();
    if (!hostId || menuOpen()) return;
    const link = app.link(hostId);
    const actions: AlertDialogAction[] = app.hosts().filter((info) => info.id !== hostId).map((info) => ({ id: `host:${info.id}`, label: info.name }));
    actions.push({ id: "all", label: "All desktops" }, { id: "add", label: "Add desktop…" }, { id: "details", label: "Desktop details…" });
    if (link.state === "connected") actions.push({ id: "disconnect", label: "Disconnect", role: "destructive" });
    else if (link.state !== "connecting") actions.push({ id: "connect", label: "Connect" });
    actions.push({ id: "settings", label: "Display and keyboard settings" }, { id: "cancel", label: "Cancel", role: "cancel" });
    menuOpen.set(true);
    const choice = await AlertDialog.show({ title: app.hostName(hostId), description: linkText(link), presentation: "actionSheet", actions }).catch(() => null);
    menuOpen.set(false);
    if (choice?.startsWith("host:")) app.openHost(choice.slice("host:".length));
    else if (choice === "all") app.nav.setRoutes([{ page: "hosts" }]);
    else if (choice === "add") void app.present({ name: "addHost" });
    else if (choice === "details") void app.present({ name: "hostDetails", host: hostId });
    else if (choice === "connect") app.connect(hostId);
    else if (choice === "disconnect") void app.device.hosts.disconnect(hostId).catch((error) => app.failure.set(PortError.from(error).message));
    else if (choice === "settings") void app.present({ name: "settings" });
  }
  return {
    docked: () => wide() && dockedOpen(),
    overlay: () => !wide() && overlayOpen(),
    toggle: () => { if (wide()) dockedOpen.set(!dockedOpen()); else overlayOpen.set(!overlayOpen()); },
    reveal: () => { if (wide()) dockedOpen.set(true); else overlayOpen.set(true); },
    close: () => overlayOpen.set(false),
    menuOpen, hostMenu: () => void hostMenu(), status,
  };
}

/** A link as the host menu and the status bar say it. */
export function linkText(link: LinkState): string {
  return link.error ? `${link.label} · ${link.error}` : link.label;
}

/** The window's top row (or a column's part of it), as tall as a touch target, on the panel color with the
 * Mac's divider below. `lead`: it starts the window's row, so it leaves the window's own controls clear. */
export function TopRow(props: { ui: Kit; lead?: boolean; testID?: string; children?: JSX.Element }) {
  const { ui } = props;
  return <View testID={props.testID} style={{ height: ui.target(), flexShrink: 0, flexDirection: "row", alignItems: "center", paddingLeft: props.lead ? ui.windowControls() : 0,
    backgroundColor: ui.palette().panel, borderBottomWidth: 1, borderColor: ui.palette().divider }}>
    {props.children}
  </View>;
}

/** The sidebar toggle and Dashboard: where the sidebar is, it holds them; while it is hidden (or open over
 * the main area) they start the page's top row, clear of the window's controls. */
export function Leading(props: { app: MobileApp; ui: Kit; chrome: PadChrome; host: string }) {
  const { app, ui, chrome } = props;
  return <>{!chrome.docked() && <View style={{ flexDirection: "row", alignItems: "center", flexShrink: 0, height: "100%", paddingLeft: ui.windowControls() + ui.pt(4),
    paddingRight: ui.pt(4), borderRightWidth: 1, borderColor: ui.palette().divider }}>
    <ui.Tool testID="sidebar-toggle" icon="layout" label="Show the sidebar" action={chrome.toggle} />
    <ui.Tool testID="dashboard" icon="globe" label="Dashboard" selected={app.top().page !== "project"} disabled={app.blocked()}
      action={() => app.openDashboard(props.host)} />
  </View>}</>;
}

/** A tab's state as the Mac's strip draws it: working in the working color, waiting and failed in gold,
 * done muted, idle a small dot, stopped a ring; nothing while it is not known. */
export function TabDot(props: { ui: Kit; status: string }) {
  const { ui } = props;
  const color = () => props.status === "working" ? ui.palette().working : props.status === "waiting" || props.status === "error" ? ui.palette().gold : ui.palette().muted;
  const side = () => props.status === "idle" ? ui.pt(4) : ui.pt(8);
  return <>{["working", "waiting", "error", "done", "idle", "stopped"].includes(props.status) && <View pointerEvents="none" accessible={false}
    style={{ width: ui.pt(8), height: ui.pt(8), flexShrink: 0, alignItems: "center", justifyContent: "center" }}>
    <View style={{ width: side(), height: side(), borderRadius: side() / 2, borderWidth: props.status === "stopped" ? 1 : 0, borderColor: color(),
      backgroundColor: props.status === "stopped" ? "transparent" : color() }} />
  </View>}</>;
}

/** The link's state as a dot: connected in the working color, connecting in gold, failed in the error color, else muted. */
export function LinkDot(props: { ui: Kit; link: LinkState }) {
  const { ui } = props;
  const color = () => props.link.state === "connected" ? ui.palette().working : props.link.state === "connecting" ? ui.palette().gold
    : props.link.state === "failed" ? ui.palette().danger : ui.palette().muted;
  return <View pointerEvents="none" accessible={false} style={{ width: ui.pt(8), height: ui.pt(8), borderRadius: ui.pt(4), flexShrink: 0, backgroundColor: color() }} />;
}

/** The host switcher at the end of the sidebar's part of the top row: the desktop's name with its link's dot;
 * it opens the host menu. */
export function HostSwitcher(props: { app: MobileApp; ui: Kit; chrome: PadChrome; host: string }) {
  const { app, ui } = props;
  const link = () => app.link(props.host);
  return <View testID="host-switcher" role="button" accessibilityLabel={`${app.hostName(props.host)}, ${linkText(link())}. Desktops`}
    disabled={app.blocked()} onPress={props.chrome.hostMenu} activeStyle={{ backgroundColor: ui.palette().panelActive }}
    style={{ flex: 1, minWidth: 0, height: ui.target(), flexDirection: "row", alignItems: "center", gap: ui.pt(8), paddingHorizontal: ui.pt(10), borderRadius: ui.pt(10) }}>
    <LinkDot ui={ui} link={link()} />
    <Text numberOfLines={1} style={{ ...ui.text(15), fontWeight: "600", flexShrink: 1 }}>{app.hostName(props.host)}</Text>
    <Icon name="down" size={ui.pt(12)} color={ui.palette().muted} />
  </View>;
}

/** The Mac's status bar in touch density: thin, small muted text, its items apart. On a project: its name,
 * then its tabs by state ("● 1 working · ✓ 2 done · ◌ 1 waiting"); at the end its live terminals and the
 * link (the desktop's name with its dot, or the link's label while not connected), which opens the host
 * menu. On the dashboard: Dashboard and the link. Clear of the home indicator; the link's target reaches
 * under it, so it is a full touch target while the bar stays thin. */
export function StatusBar(props: { app: MobileApp; ui: Kit; chrome: PadChrome; host: string; project: Accessor<ProjectRoute | null> }) {
  const { app, ui, chrome } = props;
  const insets = safeAreaInsets();
  const bar = () => Math.max(ui.pt(30), ui.target() - insets().bottom);
  const status = () => { const route = props.project(), value = chrome.status(); return route && value?.projectId === route.projectId ? value : null; };
  const counts = () => {
    const value = status()?.counts;
    if (!value) return "";
    return [value.working ? `● ${value.working} working` : "", value.done ? `✓ ${value.done} done` : "", value.waiting ? `◌ ${value.waiting} waiting` : "",
      value.error ? `${value.error} failed` : ""].filter(Boolean).join(" · ");
  };
  const link = () => app.link(props.host);
  const small = (tone: "muted" | "accent" | "working" = "muted") => ui.text(12, tone);
  return <View testID="status-bar" style={{ flexShrink: 0, flexDirection: "row", alignItems: "flex-start", height: bar() + insets().bottom,
    paddingLeft: ui.pt(16), paddingRight: ui.pt(6), gap: ui.pt(20), backgroundColor: ui.palette().panel, borderTopWidth: 1, borderColor: ui.palette().divider }}>
    <View style={{ height: bar(), flex: 1, minWidth: 0, flexDirection: "row", alignItems: "center", gap: ui.pt(20) }}>
      <Text testID="status-project" numberOfLines={1} style={{ ...small("accent"), flexShrink: 0, maxWidth: ui.pt(260) }}>{props.project()?.name ?? "Dashboard"}</Text>
      {counts() && <Text testID="status-tabs" numberOfLines={1} style={{ ...small((status()?.counts.working ?? 0) > 0 ? "working" : "muted"), flexShrink: 1 }}>{counts()}</Text>}
      <View style={{ flex: 1 }} />
      {status()?.live != null && <Text testID="status-live" numberOfLines={1} style={{ ...small(), fontFamily: app.theme().theme.monoFamily, flexShrink: 0 }}>{`${status()!.live} live`}</Text>}
    </View>
    <View testID="status-link" role="button" accessibilityLabel={`${app.hostName(props.host)}, ${linkText(link())}. Desktops`} disabled={app.blocked()}
      onPress={chrome.hostMenu} activeStyle={{ backgroundColor: ui.palette().panelActive }}
      style={{ height: bar() + insets().bottom, paddingBottom: insets().bottom, minWidth: ui.target(), maxWidth: ui.pt(300), flexDirection: "row", alignItems: "center",
        justifyContent: "center", gap: ui.pt(7), paddingHorizontal: ui.pt(10), borderRadius: ui.pt(8) }}>
      <LinkDot ui={ui} link={link()} />
      <Text numberOfLines={1} style={{ ...small(), flexShrink: 1 }}>{link().state === "connected" ? app.hostName(props.host) : link().label}</Text>
    </View>
  </View>;
}
