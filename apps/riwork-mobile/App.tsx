import { Scroll, Text, View, cleanup, colorScheme, effect, host, keyboardFrame, keymap, mount, safeAreaInsets, signal, untracked, type JSX, type YNode } from "nsh";
import { Input, NavPage, NavStack, createNavStack } from "nsh/base";
import { Symbol } from "./Symbol";
import { INITIAL_SNAPSHOT, SECTION_LABELS, SECTION_ORDER, type Dashboard, type DashboardFilter, type DashboardRow, type DashboardSection, type MobileAction, type MobileSnapshot, type Page, type Project, type Tone } from "./model";

type Route = { page: Page; hostID?: string };

function App() {
  const snapshot = signal<MobileSnapshot>(INITIAL_SNAPSHOT);
  const loaded = signal(false), failure = signal(""), pending = signal(false);
  const rootRoute: Route = { page: "hosts" };
  const navigation = createNavStack<Route>([rootRoute]);
  const hostRoutes = new Map<string, { dashboard: Route; projects: Route }>();
  let alive = true;
  cleanup(() => { alive = false; });

  function install(value: unknown) {
    const next = value as MobileSnapshot | null;
    if (!next || next.schema !== 1 || !Number.isFinite(next.revision)) throw new Error("This mobile interface needs a compatible RiWork host.");
    if (!alive || next.revision < snapshot().revision) return;
    snapshot.set(next); loaded.set(true);
  }
  cleanup(host.on("mobile.snapshot", (value) => {
    try { install(value); } catch (error) { failure.set(String(error)); }
  }));
  async function read() {
    failure.set("");
    try { install(await host.call("mobile.snapshot")); }
    catch (error) { if (alive) failure.set(String(error)); }
  }
  async function act(action: MobileAction) {
    // Native Back is always allowed, including while a host is connecting.
    const navigationOnly = action.type === "navigate";
    if (!navigationOnly && (pending() || snapshot().nativePresented)) return;
    if (!navigationOnly) pending.set(true);
    failure.set("");
    try { install(await host.call("mobile.action", action)); }
    catch (error) { if (alive) failure.set(String(error)); }
    finally { if (alive && !navigationOnly) pending.set(false); }
  }
  cleanup(host.on("mobile.command", (value) => {
    const command = value as { type?: string; name?: string };
    if (command.type === "refresh") void act({ type: "refresh" });
    else if (command.type === "present" && command.name === "settings") void act({ type: "present", name: "settings" });
  }));
  void read();

  // The host is authoritative about selected identity and explicit navigation.
  // Reusing route objects keeps page scroll/search state across pushed workspaces.
  effect(() => {
    const state = snapshot();
    let wanted: Route[] = [rootRoute];
    if (state.route !== "hosts" && state.selectedHostID) {
      let routes = hostRoutes.get(state.selectedHostID);
      if (!routes) {
        routes = { dashboard: { page: "dashboard", hostID: state.selectedHostID }, projects: { page: "projects", hostID: state.selectedHostID } };
        hostRoutes.set(state.selectedHostID, routes);
      }
      wanted.push(routes.dashboard);
      if (state.route === "projects") wanted.push(routes.projects);
    }
    // The native stack's interactive pop must not retrigger reconciliation
    // against the preceding snapshot before onDismiss tells the host.
    const current = untracked(() => navigation.routes());
    if (current.length !== wanted.length || wanted.some((route, index) => route !== current[index])) navigation.setRoutes(wanted);
  });
  function dismissed(route: Route) {
    // Programmatic route replacement already has the new snapshot. Only a
    // user's pop needs to tell the host which page now owns the connection.
    if (snapshot().route === route.page && snapshot().selectedHostID === route.hostID) {
      const routes = navigation.routes();
      void act({ type: "navigate", page: routes[routes.length - 1]?.page ?? "hosts" });
    }
  }
  const dark = () => loaded() ? snapshot().theme.dark : colorScheme() === "dark";
  const palette = () => loaded() ? snapshot().theme.palette : dark() ? {
    bg: "#000000", panel: "#1c1c1e", panelActive: "#2c2c2e", divider: "#38383a",
    text: "#f5f5f7", muted: "#98989d", cyan: "#0a84ff", gold: "#ff9f0a", working: "#ff9f0a", magenta: "#a8a8ad", error: "#ff453a",
  } : INITIAL_SNAPSHOT.theme.palette;
  const scale = () => Number.isFinite(snapshot().theme.scale) && snapshot().theme.scale > 0 ? snapshot().theme.scale : 1;
  const pt = (size: number) => size * scale();
  const target = () => Math.max(44, pt(44));
  const text = (size = 15, tone: "text" | "muted" | "cyan" | Tone = "text") => ({ fontFamily: "system-ui", fontSize: pt(size), color: palette()[tone] });
  const blocked = () => !loaded() || pending() || snapshot().nativePresented;
  const hostName = (id?: string) => snapshot().desktops.find((desktop) => desktop.id === id)?.name ?? "Desktop";
  const connected = () => snapshot().connection.state === "connected";
  effect(() => {
    if (!loaded() || snapshot().nativePresented) return;
    return keymap.bind({
      "mod+r": () => void act({ type: "refresh" }),
      "mod+,": () => void act({ type: "present", name: "settings" }),
    });
  });

  function Action(props: { label: string; symbol?: string; action: () => void; disabled?: boolean; prominent?: boolean; compact?: boolean }) {
    return <View role="button" accessibilityLabel={props.label} disabled={props.disabled} onPress={props.action}
      activeStyle={{ backgroundColor: palette().panelActive }} disabledStyle={{ opacity: 0.45 }}
      style={{ minHeight: target(), minWidth: target(), flexDirection: "row", alignItems: "center", justifyContent: "center", gap: pt(8),
        paddingHorizontal: pt(props.compact ? 8 : 14), paddingVertical: pt(8), borderRadius: pt(10),
        backgroundColor: props.prominent ? palette().panelActive : "transparent" }}>
      {props.symbol && <Symbol name={props.symbol} color={palette().cyan} size={pt(19)} />}
      <Text style={{ ...text(15, "cyan"), fontWeight: props.prominent ? "600" : "400" }}>{props.label}</Text>
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
  function Messages() {
    return <>
      {(failure() || snapshot().error) && <Note message={failure() || snapshot().error!} error={true} />}
      {!loaded() && <Note message={failure() ? "The mobile interface could not load." : "Loading your desktops…"}>
        {failure() && <Action label="Try again" symbol="arrow.clockwise" action={() => void read()} />}
      </Note>}
    </>;
  }
  function Connection() {
    const state = () => snapshot().connection;
    return <View style={{ padding: pt(14), backgroundColor: palette().panel, borderRadius: pt(12), gap: pt(8) }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: pt(9) }}>
        <Symbol name={connected() ? "checkmark.circle" : state().state === "connecting" ? "arrow.triangle.2.circlepath" : "network"}
          color={connected() ? palette().muted : palette().gold} size={pt(18)} />
        <Text style={{ ...text(14), flex: 1 }}>{state().label}</Text>
        {connected() && <Action compact={true} label="Disconnect" disabled={blocked()} action={() => void act({ type: "disconnect" })} />}
        {!connected() && state().state !== "connecting" && snapshot().selectedHostID && <Action compact={true} label="Connect" disabled={blocked()}
          action={() => void act({ type: "connect", hostID: snapshot().selectedHostID! })} />}
      </View>
      {state().error && <Text style={text(13, "error")}>{state().error}</Text>}
    </View>;
  }
  function Summary(props: { dashboard: Dashboard }) {
    return <>
      {props.dashboard.summary.length > 0 && <View style={{ flexDirection: "row", flexWrap: "wrap", gap: pt(10) }}>
        {props.dashboard.summary.map((metric) => <View style={{ flexGrow: 1, flexBasis: pt(120), padding: pt(14), gap: pt(5), borderRadius: pt(12), backgroundColor: palette().panel }}>
          <Text style={text(12, "muted")}>{metric.label}</Text>
          <Text style={{ ...text(25), fontWeight: "600" }}>{metric.value == null ? "—" : String(metric.value)}</Text>
          {metric.coverage && <Text style={text(11, "muted")}>{metric.coverage}</Text>}
        </View>)}
      </View>}
      {props.dashboard.freshness && <Text style={text(12, props.dashboard.stale ? "gold" : "muted")}>{props.dashboard.freshness}</Text>}
      {(props.dashboard.notes?.length ?? 0) > 0 && <View style={{ gap: pt(5) }}>
        {props.dashboard.notes!.map((note) => <Text style={text(12, "muted")}>{note}</Text>)}
      </View>}
    </>;
  }
  function Filters(props: { value: DashboardFilter; change: (value: DashboardFilter) => void }) {
    const choices: { value: DashboardFilter; label: string }[] = [
      { value: "all", label: "All" }, { value: "needs_you", label: "Needs you" },
      { value: "working", label: "Working" }, { value: "projects", label: "Projects" },
    ];
    return <Scroll horizontal={true} scrollbar={false} style={{ flexShrink: 0 }} contentStyle={{ flexDirection: "row", gap: pt(4) }}>
      <View role="tablist" accessibilityLabel="Dashboard filter" style={{ flexDirection: "row", padding: pt(3), gap: pt(3), borderRadius: pt(11), backgroundColor: palette().panel }}>
        {choices.map((choice) => <View role="tab" accessibilityLabel={choice.label} accessibilityState={{ selected: props.value === choice.value }}
          onPress={() => props.change(choice.value)} activeStyle={{ backgroundColor: palette().panelActive }}
          style={{ minHeight: target(), minWidth: pt(62), paddingHorizontal: pt(12), paddingVertical: pt(8), justifyContent: "center", alignItems: "center",
            borderRadius: pt(8), backgroundColor: props.value === choice.value ? palette().panelActive : "transparent" }}>
          <Text style={{ ...text(14, props.value === choice.value ? "text" : "muted"), fontWeight: props.value === choice.value ? "600" : "400" }}>{choice.label}</Text>
        </View>)}
      </View>
    </Scroll>;
  }
  function Intelligence(props: { dashboard: Dashboard }) {
    return <>{props.dashboard.intelligence?.map((report) => <Section title={report.hostName ? `${report.hostName} · Haiku overview` : "Haiku overview"}>
      <View style={{ padding: pt(16), gap: pt(10) }}>
        {report.stale && <Text style={text(12, "gold")}>Saved summary</Text>}
        {report.summary ? <Text selectable={true} style={text(15)}>{report.summary}</Text>
          : <Text style={text(14, report.error ? "gold" : "muted")}>{report.error ?? "Gathering observed work for the overview…"}</Text>}
        {report.highlights.map((line) => <View style={{ flexDirection: "row", gap: pt(8) }}>
          <Text accessible={false} style={text(14, "cyan")}>•</Text><Text selectable={true} style={{ ...text(14), flex: 1 }}>{line}</Text>
        </View>)}
        {report.followups.length > 0 && <Text role="header" style={{ ...text(12, "muted"), fontWeight: "600" }}>Suggested follow-ups</Text>}
        {report.followups.map((line) => <View style={{ flexDirection: "row", gap: pt(8) }}>
          <Symbol name="arrow.turn.down.right" color={palette().muted} size={pt(15)} /><Text selectable={true} style={{ ...text(14, "muted"), flex: 1 }}>{line}</Text>
        </View>)}
        {report.summary && report.error && <Text style={text(12, "gold")}>{report.error}</Text>}
        <Text style={text(11, "muted")}>{`AI summary of observed activity${report.generatedAt ? ` · ${new Date(report.generatedAt * 1000).toLocaleString()}` : ""}`}</Text>
      </View>
    </Section>)}</>;
  }
  function DashboardCell(props: { row: DashboardRow; showHost: boolean }) {
    return <View role={props.row.enabled ? "button" : "text"} accessibilityLabel={[props.row.hostName, props.row.stale ? "Saved data" : undefined, props.row.title, props.row.subtitle, props.row.status].filter(Boolean).join(". ")}
      disabled={props.row.enabled && blocked()} disabledStyle={{ opacity: 0.5 }} activeStyle={props.row.enabled ? { backgroundColor: palette().panelActive } : undefined}
      onPress={props.row.enabled ? () => void act({ type: "openDashboardRow", rowID: props.row.id }) : undefined}
      style={{ minHeight: target(), padding: pt(16), flexDirection: "row", alignItems: "center", gap: pt(12) }}>
      <View style={{ flex: 1, gap: pt(5) }}>
        {props.showHost && props.row.hostName && <Text style={text(12, props.row.stale ? "gold" : "cyan")}>{`${props.row.hostName}${props.row.stale ? " · Saved" : ""}`}</Text>}
        <Text style={{ ...text(15), fontWeight: "500" }}>{props.row.title}</Text>
        {props.row.subtitle && <Text style={text(13, "muted")}>{props.row.subtitle}</Text>}
        {props.row.status && <Text style={text(12, props.row.tone)}>{props.row.status}</Text>}
      </View>
      {props.row.enabled && <Symbol name="chevron.right" color={palette().muted} size={pt(13)} />}
    </View>;
  }
  function Activity(props: { dashboard: Dashboard; showHost?: boolean; filter: DashboardFilter }) {
    const expanded = signal<DashboardSection[]>([]);
    const includes = (section: DashboardSection) => props.filter === "all" || (props.filter === "needs_you" ? section === "decisions" || section === "attention" : section === props.filter);
    const localLimit = (section: DashboardSection) => expanded().includes(section) ? Number.MAX_SAFE_INTEGER : section === "projects" ? (props.filter === "projects" ? 150 : 8) : section === "decisions" ? 20 : 12;
    const anyShown = () => props.dashboard.rows.some((row) => includes(row.section));
    const empty = () => props.filter === "needs_you" ? "No pending decisions or attention rows in the latest read."
      : props.filter === "working" ? "No working agents in the latest read."
      : props.filter === "projects" ? "No projects in the latest dashboard."
      : "No activity in the latest dashboard.";
    return <>
      {!props.dashboard.available && props.dashboard.unavailableReason && <Note message={props.dashboard.unavailableReason} />}
      {SECTION_ORDER.filter(includes).map((section) => {
        const rows = () => props.dashboard.rows.filter((row) => row.section === section);
        return (rows().length > 0 || (props.dashboard.more?.[section] ?? 0) > 0) && <Section title={SECTION_LABELS[section]}>
          {rows().slice(0, localLimit(section)).map((row, index) => <>
            {index > 0 && <Divider />}
            <DashboardCell row={row} showHost={props.showHost ?? false} />
          </>)}
          {rows().length > localLimit(section) && <Action label={`Show ${rows().length - localLimit(section)} more`}
            action={() => expanded.set([...expanded(), section])} />}
          {(props.dashboard.more?.[section] ?? 0) > 0 && <Text style={{ ...text(12, "muted"), padding: pt(14) }}>{`${props.dashboard.more![section]} more on ${props.showHost ? "the desktops" : "the desktop"}`}</Text>}
        </Section>;
      })}
      {props.dashboard.available && !anyShown() && <Note message={empty()} />}
    </>;
  }
  function Hosts() {
    const insets = safeAreaInsets();
    const filter = signal<DashboardFilter>("all");
    return <NavPage title="RiWork" headerBackTitle="Hosts" headerStyle={{ backgroundColor: palette().bg, color: palette().text, tintColor: palette().cyan }}
      ios={{ headerRightSystemItem: "add" }} headerRight={<Action label="Add desktop" action={() => void act({ type: "present", name: "addHost" })} disabled={blocked() || snapshot().libraryUnavailable} />}
      statusBar={{ foreground: dark() ? "light" : "dark" }} style={{ flex: 1, backgroundColor: palette().bg }}>
      <Scroll style={{ flex: 1 }} contentStyle={{ padding: pt(16), paddingBottom: Math.max(pt(24), insets().bottom + pt(16)), gap: pt(20) }}
        refreshing={snapshot().loading} onRefresh={() => void act({ type: "refresh" })} keyboardDismissMode="interactive" keyboardShouldPersistTaps="handled">
        <Messages />
        {snapshot().libraryUnavailable ? <Note title="Saved desktops unavailable" message="The saved desktop library could not be read. Open recovery options to restore access.">
          <Action label="Recovery options" action={() => void act({ type: "present", name: "libraryRecovery" })} disabled={blocked()} />
        </Note> : <>
          {loaded() && snapshot().desktops.length === 0 && <Note title="Your desktop, wherever you are" message="Pair your Mac to see its projects, review work, and continue in your chats and terminals.">
            <Action label="Pair a desktop" symbol="plus" prominent={true} action={() => void act({ type: "present", name: "addHost" })} disabled={blocked()} />
          </Note>}
          {snapshot().desktops.length > 0 && <Section title="Desktops">
            {snapshot().desktops.map((desktop, index) => <>
              {index > 0 && <Divider />}
              <View style={{ flexDirection: "row", alignItems: "center" }}>
                <View role="button" accessibilityLabel={`${desktop.name}. ${desktop.connection}. ${desktop.detail ?? desktop.address}`}
                  disabled={blocked()} disabledStyle={{ opacity: 0.5 }} activeStyle={{ backgroundColor: palette().panelActive }}
                  onPress={() => void act({ type: "connect", hostID: desktop.id })}
                  style={{ flex: 1, minHeight: target(), padding: pt(16), flexDirection: "row", alignItems: "center", gap: pt(13) }}>
                  <Symbol name="desktopcomputer" color={palette().cyan} size={pt(24)} />
                  <View style={{ flex: 1, gap: pt(4) }}>
                    <Text style={{ ...text(16), fontWeight: "600" }}>{desktop.name}</Text>
                    <Text style={text(12, "muted")}>{desktop.address}</Text>
                    <Text style={text(12, "muted")}>{desktop.detail ?? desktop.connection}</Text>
                  </View>
                  <Symbol name="chevron.right" color={palette().muted} size={pt(13)} />
                </View>
                <View role="button" accessibilityLabel={`Manage ${desktop.name}`} disabled={blocked()} onPress={() => void act({ type: "present", name: "hostDetails", hostID: desktop.id })}
                  activeStyle={{ backgroundColor: palette().panelActive }} style={{ minWidth: target(), minHeight: target(), alignItems: "center", justifyContent: "center", marginRight: pt(4) }}>
                  <Symbol name="ellipsis.circle" color={palette().muted} size={pt(21)} />
                </View>
              </View>
            </>)}
          </Section>}
          {snapshot().homeDashboard && snapshot().desktops.length > 0 && <>
            <Text role="header" style={{ ...text(20), fontWeight: "600" }}>Across your desktops</Text>
            <Summary dashboard={snapshot().homeDashboard!} />
            <Filters value={filter()} change={(value) => filter.set(value)} />
            {filter() === "all" && <Intelligence dashboard={snapshot().homeDashboard!} />}
            <Activity dashboard={snapshot().homeDashboard!} showHost={true} filter={filter()} />
          </>}
        </>}
        <Section>
          <Action label="Display and keyboard settings" symbol="slider.horizontal.3" disabled={blocked()} action={() => void act({ type: "present", name: "settings" })} />
        </Section>
      </Scroll>
    </NavPage>;
  }
  function HostDashboard(props: { route: Route }) {
    const insets = safeAreaInsets();
    const filter = signal<DashboardFilter>("all");
    const dashboard = () => snapshot().selectedHostID === props.route.hostID ? snapshot().dashboard : INITIAL_SNAPSHOT.dashboard;
    return <NavPage title={hostName(props.route.hostID)} headerBackTitle="Dashboard" onDismiss={() => dismissed(props.route)}
      headerStyle={{ backgroundColor: palette().bg, color: palette().text, tintColor: palette().cyan }}
      headerRight={<Action label="Projects" symbol="folder" compact={true} disabled={blocked()} action={() => void act({ type: "showProjects" })} />}
      statusBar={{ foreground: dark() ? "light" : "dark" }} style={{ flex: 1, backgroundColor: palette().bg }}>
      <Scroll style={{ flex: 1 }} contentStyle={{ padding: pt(16), paddingBottom: Math.max(pt(24), insets().bottom + pt(16)), gap: pt(18) }}
        refreshing={snapshot().loading} onRefresh={() => void act({ type: "refresh" })}>
        <Messages /><Connection />
        <Summary dashboard={dashboard()} />
        {dashboard().global && <Section title="Global orchestrator">
          <View style={{ padding: pt(14), gap: pt(8) }}>
            <Text style={text(14, "muted")}>{dashboard().global!.summary}</Text>
            <Action label={dashboard().global!.actionTitle} symbol="globe" prominent={true}
              disabled={blocked() || !connected() || dashboard().global!.action === "unavailable"} action={() => void act({ type: "openGlobal" })} />
          </View>
        </Section>}
        <Filters value={filter()} change={(value) => filter.set(value)} />
        {filter() === "all" && <Intelligence dashboard={dashboard()} />}
        <Activity dashboard={dashboard()} filter={filter()} />
        <Action label="All projects" symbol="folder" prominent={true} disabled={blocked()} action={() => void act({ type: "showProjects" })} />
      </Scroll>
    </NavPage>;
  }
  function Projects(props: { route: Route }) {
    const routeHostID = props.route.hostID;
    const query = signal("");
    const searchInput = signal<YNode | null>(null);
    const searchFocused = signal(false), sortOpen = signal(false), folders = signal(true);
    const selected = signal<string | null>(null);
    const scroll = signal<YNode | null>(null);
    const projectRefs = new Map<string, YNode>();
    let scrollY = 0;
    const insets = safeAreaInsets();
    const projects = () => snapshot().selectedHostID === props.route.hostID ? snapshot().projects : [];
    const shown = () => {
      const words = query().trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
      return projects().filter((project) => words.every((word) => `${project.name} ${project.path} ${project.folder ?? ""}`.toLocaleLowerCase().includes(word)));
    };
    const groups = () => {
      const list = shown();
      if (!folders() || !list.some((project) => project.folder)) return [{ name: `${list.length} ${list.length === 1 ? "project" : "projects"}`, projects: list }];
      const result = new Map<string, Project[]>();
      for (const project of list) {
        const name = project.folder ?? "Other projects";
        const group = result.get(name) ?? []; group.push(project); result.set(name, group);
      }
      return [...result].map(([name, projects]) => ({ name, projects }));
    };
    const ordered = () => groups().flatMap((group) => group.projects);
    const highlighted = () => ordered().find((project) => project.id === selected()) ?? ordered()[0];
    const onTop = () => snapshot().route === "projects" && snapshot().selectedHostID === props.route.hostID && !snapshot().nativePresented;
    function openSelected(terminal = false) {
      const project = highlighted();
      if (!project || !connected()) return;
      void act({ type: terminal ? "newTerminal" : "openProject", projectID: project.id, hostID: props.route.hostID });
    }
    async function move(direction: number) {
      const list = ordered(); if (!list.length) return;
      const index = Math.max(0, list.findIndex((project) => project.id === highlighted()?.id));
      const project = list[(index + direction + list.length) % list.length];
      selected.set(project.id);
      const node = projectRefs.get(project.id), container = scroll();
      if (!node || !container) return;
      const [row, viewport] = await Promise.all([node.measure(), container.measure()]);
      if (!onTop() || highlighted()?.id !== project.id || !row || !viewport) return;
      const top = row.pagePosition.y - viewport.pagePosition.y;
      if (top < 0 || top + row.bounds.size.height > viewport.bounds.size.height) {
        container.scrollTo({ y: Math.max(0, scrollY + top - (viewport.bounds.size.height - row.bounds.size.height) / 2), animated: true });
      }
    }
    effect(() => {
      if (snapshot().route !== "projects" || snapshot().selectedHostID !== props.route.hostID || snapshot().nativePresented) return;
      return keymap.bind({
        "mod+f": () => searchInput()?.focus(),
        "mod+o": () => { if (projects().length > 1) void act({ type: "cycleProjectSort" }); },
        "mod+shift+n": () => { if (snapshot().canCreateProject) void act({ type: "present", name: "newProject" }); },
        "mod+n": () => openSelected(true),
      });
    });
    effect(() => {
      if (!onTop() || !snapshot().keyboard?.hardware || searchFocused() || sortOpen()) return;
      const actions: Record<string, () => void> = {
        up: () => void move(-1), down: () => void move(1), tab: () => void move(1), "shift+tab": () => void move(-1),
        enter: () => openSelected(), escape: () => query.set(""),
      };
      if (snapshot().keyboard?.clicks) { actions["mod+w"] = () => void move(-1); actions["mod+s"] = () => void move(1); }
      return keymap.bind(actions, { when: "notEditing" });
    });
    function ProjectCell(props: { project: Project }) {
      return <View ref={(node) => projectRefs.set(props.project.id, node)} style={{ flexDirection: "row", alignItems: "center",
        backgroundColor: snapshot().keyboard?.hardware && highlighted()?.id === props.project.id ? palette().panelActive : undefined }}>
        <View role="button" accessibilityLabel={`${props.project.name}. ${props.project.path}. ${props.project.activity ?? ""}`}
          accessibilityState={{ selected: snapshot().keyboard?.hardware && highlighted()?.id === props.project.id }} disabled={blocked() || !connected()}
          disabledStyle={{ opacity: 0.5 }} activeStyle={{ backgroundColor: palette().panelActive }}
          onPress={() => { selected.set(props.project.id); void act({ type: "openProject", projectID: props.project.id, hostID: routeHostID }); }}
          style={{ flex: 1, minHeight: target(), padding: pt(16), flexDirection: "row", alignItems: "center", gap: pt(12) }}>
        <Symbol name="folder" color={palette().cyan} size={pt(22)} />
        <View style={{ flex: 1, gap: pt(5) }}>
          <Text style={{ ...text(16), fontWeight: "500" }}>{props.project.name}</Text>
          <Text style={text(12, "muted")}>{props.project.path}</Text>
          {props.project.activity && <Text style={text(12, props.project.tone)}>{props.project.activity}</Text>}
        </View>
        <Symbol name="chevron.right" color={palette().muted} size={pt(13)} />
        </View>
        <View role="button" accessibilityLabel={`New terminal in ${props.project.name}`} disabled={blocked() || !connected()}
          disabledStyle={{ opacity: 0.5 }} activeStyle={{ backgroundColor: palette().panelActive }}
          onPress={() => void act({ type: "newTerminal", projectID: props.project.id, hostID: routeHostID })}
          style={{ minWidth: target(), minHeight: target(), alignItems: "center", justifyContent: "center", marginRight: pt(4) }}>
          <Symbol name="plus.circle" color={palette().cyan} size={pt(21)} />
        </View>
      </View>;
    }
    return <NavPage title="Projects" onDismiss={() => dismissed(props.route)}
      headerStyle={{ backgroundColor: palette().bg, color: palette().text, tintColor: palette().cyan }}
      ios={{ headerRightSystemItem: "add" }} headerRight={<Action label="New project" disabled={blocked() || !snapshot().canCreateProject}
        action={() => void act({ type: "present", name: "newProject" })} />}
      statusBar={{ foreground: dark() ? "light" : "dark" }} style={{ flex: 1, backgroundColor: palette().bg }}>
      <Scroll ref={(node) => scroll.set(node)} onScroll={(event) => { scrollY = event.offset.y; }}
        style={{ flex: 1 }} contentStyle={{ padding: pt(16), paddingBottom: Math.max(insets().bottom, keyboardFrame().height) + pt(24), gap: pt(16) }}
        refreshing={snapshot().loading} onRefresh={() => void act({ type: "refresh" })} keyboardDismissMode="interactive" keyboardShouldPersistTaps="handled">
        <Messages />
        {!connected() && <Connection />}
        {snapshot().projectsStale && projects().length > 0 && <Text style={text(12, "gold")}>Saved projects · reconnect to refresh</Text>}
        <Input ref={(node) => searchInput.set(node)} accessibilityLabel="Search projects" placeholder="Search projects" value={query()} onChange={(value) => query.set(value)}
          onFocus={() => searchFocused.set(true)} onBlur={() => searchFocused.set(false)}
          autoCapitalize="none" autoCorrect={false} returnKeyType="search"
          style={{ ...text(16), minHeight: target(), paddingHorizontal: pt(14), paddingVertical: pt(10), backgroundColor: palette().panel, borderRadius: pt(11) }} />
        <View style={{ flexDirection: "row", flexWrap: "wrap", justifyContent: "space-between" }}>
          {projects().length > 1 && snapshot().projectSort && <Action label={`Sort: ${snapshot().projectSort!.title}`} symbol="arrow.up.arrow.down"
            compact={true} disabled={blocked()} action={() => sortOpen.set(!sortOpen())} />}
          {projects().some((project) => project.folder) && <Action label={folders() ? "Grouped by folder" : "Flat list"} symbol={folders() ? "folder" : "list.bullet"}
            compact={true} action={() => folders.set(!folders())} />}
        </View>
        {sortOpen() && <Section title="Sort projects">
          {([{ value: "recent", label: "Recent", detail: "Latest terminal activity, then edits" }, { value: "name", label: "Name", detail: "A to Z" },
            { value: "dateAdded", label: "Date added", detail: "Newest first" }] as const).map((choice) => <View role="radio" accessibilityLabel={`${choice.label}. ${choice.detail}`}
              accessibilityState={{ checked: snapshot().projectSort?.value === choice.value }} disabled={blocked()}
              onPress={() => { sortOpen.set(false); void act({ type: "setProjectSort", value: choice.value }); }}
              activeStyle={{ backgroundColor: palette().panelActive }} style={{ minHeight: target(), padding: pt(14), flexDirection: "row", alignItems: "center", gap: pt(12) }}>
              <View style={{ flex: 1, gap: pt(3) }}><Text style={text(15)}>{choice.label}</Text><Text style={text(12, "muted")}>{choice.detail}</Text></View>
              {snapshot().projectSort?.value === choice.value && <Symbol name="checkmark" color={palette().cyan} size={pt(16)} />}
            </View>)}
        </Section>}
        {shown().length > 0 ? groups().map((group) => <Section title={group.name}>
          {group.projects.map((project, index) => <>{index > 0 && <Divider />}<ProjectCell project={project} /></>)}
        </Section>) : connected() && !snapshot().loading && <Note title={query().trim() ? "No matching projects" : "No projects yet"}
          message={query().trim() ? "Try another name or folder path." : "Create a project on this desktop to start a chat or terminal."}>
          {query().trim() ? <Action label="Clear search" action={() => query.set("")} /> : snapshot().canCreateProject && <Action label="New project" symbol="plus"
            disabled={blocked()} action={() => void act({ type: "present", name: "newProject" })} />}
        </Note>}
      </Scroll>
    </NavPage>;
  }

  return <View style={{ flex: 1, backgroundColor: palette().bg }}>
    <NavStack state={navigation} style={{ flex: 1 }}>
      {(route) => route.page === "hosts" ? <Hosts /> : route.page === "dashboard" ? <HostDashboard route={route} /> : <Projects route={route} />}
    </NavStack>
  </View>;
}

mount(App);
