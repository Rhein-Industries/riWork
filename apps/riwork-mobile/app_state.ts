/** The iPhone and iPad app's state above both shells: the device (Swift), the hosts, the theme, the All
 * hosts overview Swift still reads, each host's ports and the navigation. `PhoneShell` and `PadShell` draw
 * the same routes, so resizing across 600 points keeps where the person is. */
import { cleanup, effect, host, keymap, signal, untracked, type Accessor, type Signal } from "nsh";
import { createNavStack, type NavStackState } from "nsh/base";
import { PortError, type DeviceTheme, type HostInfo, type LinkState, type PresentRequest, type Services } from "../riwork/client/ports";
import { createRemoteServices } from "../riwork/client/remote";
import { EMPTY_OVERVIEW, type NavOpen, type OverviewSnapshot } from "./model";
import { createSwiftDevice, swiftTransport, type SwiftDevice } from "./swift_transport";

export type Route =
  | { page: "hosts" }
  | { page: "dashboard"; host: string }
  | { page: "projects"; host: string }
  /** `tab`: the tab to open on (`shell:<id>`, `chat:<id>`); `newTerminal`: the New terminal form opens once the page is up. */
  | { page: "project"; host: string; projectId: string; name: string; tab?: string; newTerminal?: boolean };
export type ProjectRoute = Extract<Route, { page: "project" }>;

/** `mobile.action`, what Swift still does for the pages: read the overview again, open an overview row, and
 * learn which page is on top (the overview's connections, the activity it reads, the project its native
 * screens and the terminal follow). */
export type MobileAction =
  | { type: "refresh" }
  | { type: "openDashboardRow"; rowID: string }
  | { type: "navigate"; page: Route["page"]; hostID?: string; projectID?: string };

export interface MobileApp {
  device: SwiftDevice;
  hosts: Accessor<HostInfo[]>;
  theme: Accessor<DeviceTheme>;
  overview: Accessor<OverviewSnapshot>;
  /** The first overview arrived. */
  loaded: Accessor<boolean>;
  /** Why the last call to Swift failed, for the page on top. */
  failure: Signal<string>;
  nav: NavStackState<Route>;
  /** The route on top. */
  top: Accessor<Route>;
  services(hostId: string): Services;
  hostName(hostId: string | undefined): string;
  link(hostId: string): LinkState;
  /** A native Swift screen is over the pages: they take no input. */
  blocked(): boolean;
  /** Presents a native Swift screen; false (and `failure` says why) when it could not. */
  present(request: PresentRequest): Promise<boolean>;
  act(action: MobileAction): Promise<void>;
  /** Makes the host the interactive one and shows its page. */
  openHost(hostId: string): void;
  /** Connects to the host again, where the pages are. */
  connect(hostId: string): void;
  openDashboard(hostId: string): void;
  openProjects(hostId: string): void;
  /** Shows the project's page; the page already on screen (the iPad's sidebar) takes the tab, the New terminal
   * form or the picker of hidden tabs (`picker`, only there) without being made again. */
  openProject(hostId: string, project: { id: string; name: string }, options?: { tab?: string; newTerminal?: boolean; picker?: boolean }): void;
  /** Leaves the page on top. */
  back(): void;
  /** The page on top's refresh (⌘R, Swift's refresh command): set while it is on top. */
  refresher: { current: (() => void) | null };
  /** The page's app chords from Swift (`keyboard.command`, an `app_chords.ts` id: the root key host's Clicks
   * prefix and ⌘ chords, or the native chat's): set while a project's page (or the iPad's dashboard) takes keys. */
  chords: { current: ((id: string) => void) | null };
  /** Swift asked the project page on screen for a tab or the New terminal form (`nav.open`), or the iPad's sidebar did. */
  projectRequest: Accessor<ProjectRequest | null>;
  /** The iPad opened the only paired desktop at launch, or the person went somewhere first: it is not opened again. */
  launched: { current: boolean };
}
export interface ProjectRequest { projectId: string; tab?: string; newTerminal: boolean; picker?: boolean; seq: number }

export const HOSTS: Route = { page: "hosts" };

export function createMobileApp(): MobileApp {
  let alive = true;
  cleanup(() => { alive = false; });
  const device = createSwiftDevice();
  const theme = signal(device.theme.current());
  const hosts = signal<HostInfo[]>(device.hosts.current());
  const themeSubscription = device.theme.subscribe((next) => theme.set(next));
  const hostsSubscription = device.hosts.subscribe((next) => hosts.set(next));
  cleanup(() => { themeSubscription.close(); hostsSubscription.close(); });

  const overview = signal<OverviewSnapshot>(EMPTY_OVERVIEW);
  const loaded = signal(false), failure = signal("");
  function install(value: unknown) {
    const next = value as OverviewSnapshot | null;
    if (!next || next.schema !== 2 || !Number.isFinite(next.revision)) throw new Error("This mobile interface needs a compatible Somtus host.");
    if (!alive || next.revision < overview().revision) return;
    overview.set(next); loaded.set(true);
  }
  cleanup(host.on("mobile.snapshot", (value) => { try { install(value); } catch (error) { failure.set(String(error)); } }));
  void host.call("mobile.snapshot").then(install).catch((error) => { if (alive) failure.set(String(error)); });

  const refresher: { current: (() => void) | null } = { current: null };
  const chords: { current: ((id: string) => void) | null } = { current: null };
  const projectRequest = signal<ProjectRequest | null>(null);
  const nav = createNavStack<Route>([HOSTS]);
  const top = () => { const routes = nav.routes(); return routes[routes.length - 1] ?? HOSTS; };
  const services = new Map<string, Services>();
  const hostName = (id: string | undefined) => hosts().find((info) => info.id === id)?.name ?? "Desktop";

  async function act(action: MobileAction) {
    try { install(await host.call("mobile.action", action)); }
    catch (error) { if (alive) failure.set(PortError.from(error).message); }
  }
  /** The person went somewhere (a page past Hosts, under either shell, or a native screen): the iPad's launch no longer opens a desktop by itself. */
  const launched = { current: false };
  effect(() => {
    const routes = nav.routes();
    if (routes.length !== 1 || routes[0].page !== "hosts") launched.current = true;
  });
  async function present(request: PresentRequest): Promise<boolean> {
    launched.current = true;
    failure.set("");
    try { await device.nav.present(request); return true; }
    catch (error) { if (alive) failure.set(PortError.from(error).message); return false; }
  }

  // The routes of one host are kept while the stack has them: a page that stays keeps its state.
  function routesFor(hostId: string, page: "dashboard" | "projects"): Route {
    const found = untracked(() => nav.routes()).find((route) => route.page === page && route.host === hostId);
    return found ?? { page, host: hostId };
  }
  function connect(hostId: string) {
    failure.set("");
    // Swift connects, and refuses while a native screen is up or another host is still opening.
    void device.hosts.select(hostId).catch((error) => { if (alive) failure.set(PortError.from(error).message); });
  }
  function openHost(hostId: string) {
    nav.setRoutes([HOSTS, routesFor(hostId, "dashboard")]);
    connect(hostId);
  }
  function openDashboard(hostId: string) {
    nav.setRoutes([HOSTS, routesFor(hostId, "dashboard")]);
  }
  function openProjects(hostId: string) {
    nav.setRoutes([HOSTS, routesFor(hostId, "dashboard"), routesFor(hostId, "projects")]);
  }
  /** The project page on screen takes a tab, the form or the picker without being made again. */
  function request(projectId: string, options: { tab?: string; newTerminal?: boolean; picker?: boolean }) {
    projectRequest.set({ projectId, tab: options.tab, newTerminal: options.newTerminal === true, picker: options.picker === true, seq: (untracked(projectRequest)?.seq ?? 0) + 1 });
  }
  function openProject(hostId: string, project: { id: string; name: string }, options: { tab?: string; newTerminal?: boolean; picker?: boolean } = {}) {
    const current = untracked(top);
    // The project already on screen (the iPad's sidebar tapped again) stays as it is.
    if (current.page === "project" && current.host === hostId && current.projectId === project.id) {
      if (options.tab || options.newTerminal || options.picker) request(project.id, options);
      return;
    }
    const route: ProjectRoute = { page: "project", host: hostId, projectId: project.id, name: project.name, tab: options.tab, newTerminal: options.newTerminal };
    const routes = untracked(() => nav.routes()).filter((entry) => entry.page !== "project");
    const base = routes.some((entry) => entry.page !== "hosts" && entry.host === hostId) ? routes : [HOSTS, routesFor(hostId, "dashboard")];
    nav.setRoutes([...base, route]);
  }
  function back() {
    if (untracked(() => nav.routes()).length > 1) nav.pop();
  }

  // Swift sends the pages somewhere itself: a host just paired, an overview row, a terminal its own sheet made.
  cleanup(host.on("nav.open", (value) => {
    const open = (value ?? {}) as unknown as NavOpen;
    if (open.page === "hosts" || !open.host) { nav.setRoutes([HOSTS]); return; }
    if (open.page === "dashboard") openDashboard(open.host);
    else if (open.page === "projects") openProjects(open.host);
    else if (open.page === "project" && open.projectId) {
      const current = untracked(top);
      // Already on screen: the page takes the tab (or the form) without being made again.
      if (current.page === "project" && current.host === open.host && current.projectId === open.projectId) {
        request(open.projectId, { tab: open.tab, newTerminal: open.newTerminal });
      } else openProject(open.host, { id: open.projectId, name: open.name ?? "Project" }, { tab: open.tab, newTerminal: open.newTerminal === true });
    }
  }));
  // Swift follows the page on top: the overview reads while Hosts is up, a project's lists and native screens follow it.
  let said = "";
  effect(() => {
    const route = top();
    const action: MobileAction = route.page === "hosts" ? { type: "navigate", page: "hosts" }
      : route.page === "project" ? { type: "navigate", page: "project", hostID: route.host, projectID: route.projectId }
      : { type: "navigate", page: route.page, hostID: route.host };
    const key = JSON.stringify(action);
    if (key === said) return;
    said = key;
    void act(action);
  });
  const refresh = () => { if (refresher.current) refresher.current(); else void act({ type: "refresh" }); };
  // Swift's root key host sends these while it holds the keyboard (a Clicks keyboard); otherwise the keymap has them.
  cleanup(host.on("mobile.command", (value) => {
    const command = (value ?? {}) as { type?: string; name?: string };
    if (command.type === "refresh") refresh();
    else if (command.type === "present" && command.name === "settings") void present({ name: "settings" });
  }));
  // ⌘R and ⌘, on every page while no native screen is up.
  effect(() => {
    if (!loaded() || overview().nativePresented) return;
    return keymap.bind({ "mod+r": refresh, "mod+,": () => void present({ name: "settings" }) });
  });
  cleanup(host.on("keyboard.command", (value) => {
    const id = (value as { id?: unknown } | null)?.id;
    if (typeof id === "string") chords.current?.(id);
  }));

  return {
    device, hosts, theme, overview, loaded, failure, nav, top, hostName, act, present, openHost, connect, openDashboard, openProjects, openProject, back, refresher, chords, projectRequest,
    launched,
    services(hostId) {
      let found = services.get(hostId);
      if (!found) {
        found = createRemoteServices(swiftTransport(device, hostId), { id: hostId, name: untracked(() => hostName(hostId)) });
        services.set(hostId, found);
      }
      return found;
    },
    link: (hostId) => hosts().find((info) => info.id === hostId)?.link ?? device.link(hostId),
    blocked: () => overview().nativePresented,
  };
}
