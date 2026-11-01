/** A host's projects: search, the person's order (`riwork.projectSort`), the Mac's folders when it gives them
 * (`native.invoke` `Store.snapshot`), what each project's agents are doing (`projects.list`, read again while
 * the page is on screen), and New project. A project opens its page; ＋ opens it with New terminal. */
import { Scroll, Text, View, cleanup, computed, effect, keyboardFrame, keymap, signal, untracked, type YNode } from "nsh";
import { Input } from "nsh/base";
import { PortError } from "../../riwork/client/ports";
import type { ProjectSummary, StoreTree } from "../../riwork/client/types";
import type { MobileApp } from "../app_state";
import { PROJECT_SORTS, listedProjects, projectActivity, projectListRows, projectSort, sortProjects, type ListedProject, type ProjectSort } from "../projects_model";
import { Symbol } from "../Symbol";
import type { Kit, Layout } from "../ui";
import { NewProjectForm } from "./NewProject";

/** How often the list is read again while it is on screen: as the native page did; the iPad's column, always up, less often. */
const PHONE_POLL_MS = 4_000, PAD_POLL_MS = 10_000;

export function ProjectsPage(props: {
  app: MobileApp; ui: Kit; layout: Layout; host: string; onTop: () => boolean;
  /** The iPad: the project on screen beside the column, drawn selected. */
  current?: () => string | null;
  /** Its keyboard shortcuts are live (the iPad: not while a project's page beside it has the keys). */
  keys?: () => boolean;
  onBack?: () => void;
}) {
  const { app, ui } = props;
  const { Action, Section, Divider, Note, Failure, palette, pt, target, text } = ui;
  const hostId = props.host;
  const services = app.services(hostId);
  const list = signal<ProjectSummary[] | null>(services.projects.last().value);
  const tree = signal<StoreTree | null>(null);
  const failure = signal(""), loading = signal(false);
  const sort = signal<ProjectSort>("recent");
  const query = signal(""), collapsed = signal<ReadonlySet<string>>(new Set());
  const sortOpen = signal(false), creating = signal(false), searchFocused = signal(false);
  const highlighted = signal<string | null>(null);
  const touched = signal<Record<string, number>>({});
  const projectRefs = new Map<string, YNode>();
  let searchInput: YNode | undefined, scroll: YNode | undefined, scrollY = 0;
  let alive = true;
  cleanup(() => { alive = false; });
  const connected = () => app.link(hostId).state === "connected";
  const onScreen = computed(() => props.onTop() && !app.blocked());
  const keysLive = computed(() => onScreen() && (props.keys?.() ?? true));
  const keyboard = () => app.overview().keyboard;
  const bottom = ui.bottomInset();

  void app.device.prefs.get("projectSort").then((value) => { if (alive) sort.set(projectSort(value)); }).catch(() => {});
  function chooseSort(value: ProjectSort) {
    sort.set(value); sortOpen.set(false);
    void app.device.prefs.set("projectSort", value).catch((error) => { if (alive) failure.set(PortError.from(error).message); });
  }

  /** Reads the list; with `full`, the folders too (they change rarely: on entry, a refresh, a create). */
  async function read(full: boolean) {
    if (!connected()) return;
    loading.set(true);
    const treeRead = full && services.projects.tree ? services.projects.tree().catch(() => null) : null;
    try {
      const value = await services.projects.list();
      if (alive) { list.set(value); failure.set(""); }
    } catch (error) {
      if (alive) failure.set(PortError.from(error).message);
    }
    // A tree that could not be read keeps the last one: the folders do not flicker away.
    const value = treeRead ? await treeRead : null;
    if (alive && value) tree.set(value);
    if (alive) loading.set(false);
  }
  // The reads run untracked: what they set must not run this again.
  effect(() => {
    if (!onScreen() || !connected()) return;
    untracked(() => void read(true));
    const timer = setInterval(() => void read(false), props.layout === "pad" ? PAD_POLL_MS : PHONE_POLL_MS);
    return () => clearInterval(timer);
  });
  const refresh = () => void read(true);
  effect(() => {
    if (!onScreen() || props.layout === "pad") return;
    app.refresher.current = refresh;
    return () => { if (app.refresher.current === refresh) app.refresher.current = null; };
  });

  const projects = computed(() => sortProjects(listedProjects(list(), tree()), sort(), touched()));
  const rows = computed(() => projectListRows(projects(), tree()?.projectFolders ?? [], query(), collapsed()));
  const ordered = () => rows().flatMap((row) => row.kind === "project" ? [row.project] : []);
  const selected = () => ordered().find((project) => project.id === highlighted()) ?? ordered()[0];
  function open(project: ListedProject, newTerminal = false) {
    highlighted.set(project.id);
    app.openProject(hostId, project, { newTerminal });
  }
  function toggle(folderId: string) {
    const next = new Set(collapsed());
    if (next.has(folderId)) next.delete(folderId); else next.add(folderId);
    collapsed.set(next);
  }
  async function move(step: number) {
    const all = ordered();
    if (!all.length) return;
    const index = Math.max(0, all.findIndex((project) => project.id === selected()?.id));
    const project = all[(index + step + all.length) % all.length];
    highlighted.set(project.id);
    const node = projectRefs.get(project.id);
    if (!node || !scroll) return;
    const [row, viewport] = await Promise.all([node.measure(), scroll.measure()]);
    if (!onScreen() || selected()?.id !== project.id || !row || !viewport) return;
    const top = row.pagePosition.y - viewport.pagePosition.y;
    if (top < 0 || top + row.bounds.size.height > viewport.bounds.size.height) {
      scroll.scrollTo({ y: Math.max(0, scrollY + top - (viewport.bounds.size.height - row.bounds.size.height) / 2), animated: true });
    }
  }
  effect(() => {
    if (!keysLive() || creating()) return;
    return keymap.bind({
      "mod+f": () => searchInput?.focus(),
      "mod+o": () => { const index = PROJECT_SORTS.findIndex((choice) => choice.value === sort()); chooseSort(PROJECT_SORTS[(index + 1) % PROJECT_SORTS.length].value); },
      "mod+shift+n": () => { if (connected()) creating.set(true); },
      "mod+n": () => { const project = selected(); if (project && connected()) open(project, true); },
    });
  });
  effect(() => {
    if (!keysLive() || creating() || !keyboard().hardware || searchFocused() || sortOpen()) return;
    const actions: Record<string, () => void> = {
      up: () => void move(-1), down: () => void move(1), tab: () => void move(1), "shift+tab": () => void move(-1),
      enter: () => { const project = selected(); if (project && connected()) open(project); }, escape: () => query.set(""),
    };
    if (keyboard().clicks) { actions["mod+w"] = () => void move(-1); actions["mod+s"] = () => void move(1); }
    return keymap.bind(actions, { when: "notEditing" });
  });

  function ProjectRow(row: { project: ListedProject; depth: number }) {
    const marked = () => props.current?.() === row.project.id || (keyboard().hardware && selected()?.id === row.project.id);
    const activity = () => projectActivity(row.project);
    return <View ref={(node) => projectRefs.set(row.project.id, node)} style={{ flexDirection: "row", alignItems: "center",
      backgroundColor: marked() ? palette().panelActive : undefined }}>
      <View testID={`project:${row.project.id}`} role="button" accessibilityLabel={`${row.project.name}. ${row.project.root}. ${activity()?.text ?? ""}`}
        accessibilityState={{ selected: marked() }} disabled={app.blocked() || !connected()}
        disabledStyle={{ opacity: 0.5 }} activeStyle={{ backgroundColor: palette().panelActive }} onPress={() => open(row.project)}
        style={{ flex: 1, minHeight: target(), padding: pt(14), paddingLeft: pt(14 + row.depth * 14), flexDirection: "row", alignItems: "center", gap: pt(12) }}>
        <Symbol name="folder" color={palette().cyan} size={pt(22)} />
        <View style={{ flex: 1, gap: pt(4) }}>
          <Text numberOfLines={1} style={{ ...text(16), fontWeight: "500" }}>{row.project.name}</Text>
          <Text numberOfLines={1} style={text(12, "muted")}>{row.project.root}</Text>
          {activity() && <Text style={text(12, activity()!.tone)}>{activity()!.text}</Text>}
        </View>
        {props.layout === "phone" && <Symbol name="chevron.right" color={palette().muted} size={pt(13)} />}
      </View>
      <View role="button" accessibilityLabel={`New terminal in ${row.project.name}`} disabled={app.blocked() || !connected()}
        disabledStyle={{ opacity: 0.5 }} activeStyle={{ backgroundColor: palette().panelActive }} onPress={() => open(row.project, true)}
        style={{ minWidth: target(), minHeight: target(), alignItems: "center", justifyContent: "center", marginRight: pt(4) }}>
        <Symbol name="plus.circle" color={palette().cyan} size={pt(21)} />
      </View>
    </View>;
  }

  const empty = () => !rows().some((row) => row.kind === "project");
  return <ui.Page layout={props.layout} title={props.layout === "pad" ? app.hostName(hostId) : "Projects"} testID={`page:projects:${hostId}`}
    backTitle="Hosts" onBack={props.onBack} addButton={true}
    right={<Action label="New project" symbol={props.layout === "pad" ? "plus" : undefined} compact={props.layout === "pad"}
      disabled={app.blocked() || !connected() || !services.projects.create} action={() => creating.set(true)} />}>
    <Scroll ref={(node) => { scroll = node; }} onScroll={(event) => { scrollY = event.offset.y; }}
      style={{ flex: 1 }} contentStyle={{ padding: pt(16), paddingBottom: Math.max(bottom(), keyboardFrame().height) + pt(24), gap: pt(16) }}
      refreshing={loading() && list() == null} onRefresh={refresh} keyboardDismissMode="interactive" keyboardShouldPersistTaps="handled">
      <Failure />
      {failure() && <Note message={list() ? `Saved projects · ${failure()}` : failure()} error={!list()} />}
      {!connected() && <Note message={`${app.link(hostId).label}. ${list() ? "These projects are from the last read." : "Connect to this desktop to list its projects."}`} />}
      {props.layout === "pad" && <Section>
        <Action label="Dashboard" symbol="square.grid.2x2" disabled={app.blocked()} action={() => app.openDashboard(hostId)} />
      </Section>}
      <Input ref={(node) => { searchInput = node; }} accessibilityLabel="Search projects" placeholder="Search projects" value={query()}
        onChange={(value) => query.set(value)} onFocus={() => searchFocused.set(true)} onBlur={() => searchFocused.set(false)}
        autoCapitalize="none" autoCorrect={false} returnKeyType="search"
        style={{ ...text(16), minHeight: target(), paddingHorizontal: pt(14), paddingVertical: pt(10), backgroundColor: palette().panel, borderRadius: pt(11) }} />
      {projects().length > 1 && <View style={{ flexDirection: "row", flexWrap: "wrap" }}>
        <Action label={`Sort: ${PROJECT_SORTS.find((choice) => choice.value === sort())!.label}`} symbol="arrow.up.arrow.down"
          disabled={app.blocked()} action={() => sortOpen.set(!sortOpen())} />
      </View>}
      {sortOpen() && <Section title="Sort projects">
        {PROJECT_SORTS.map((choice) => <View role="radio" accessibilityLabel={`${choice.label}. ${choice.detail}`}
          accessibilityState={{ checked: sort() === choice.value }} onPress={() => chooseSort(choice.value)}
          activeStyle={{ backgroundColor: palette().panelActive }} style={{ minHeight: target(), padding: pt(14), flexDirection: "row", alignItems: "center", gap: pt(12) }}>
          <View style={{ flex: 1, gap: pt(3) }}><Text style={text(15)}>{choice.label}</Text><Text style={text(12, "muted")}>{choice.detail}</Text></View>
          {sort() === choice.value && <Symbol name="checkmark" color={palette().cyan} size={pt(16)} />}
        </View>)}
      </Section>}
      {!empty() ? <Section>
        {rows().map((row, index) => <>
          {index > 0 && <Divider />}
          {row.kind === "project" ? <ProjectRow project={row.project} depth={row.depth} />
            : row.kind === "folder" ? <View role="button" accessibilityLabel={`${row.name}, ${row.count} ${row.count === 1 ? "project" : "projects"}`}
              accessibilityState={{ expanded: !collapsed().has(row.id.slice("folder-".length)) }} onPress={() => toggle(row.id.slice("folder-".length))}
              activeStyle={{ backgroundColor: palette().panelActive }}
              style={{ minHeight: target(), paddingVertical: pt(10), paddingRight: pt(14), paddingLeft: pt(14 + row.depth * 14), flexDirection: "row", alignItems: "center", gap: pt(10) }}>
              <Symbol name={collapsed().has(row.id.slice("folder-".length)) ? "chevron.right" : "chevron.down"} color={palette().muted} size={pt(13)} />
              <Text style={{ ...text(14, "muted"), fontWeight: "600", flex: 1 }}>{row.name}</Text>
              <Text style={text(13, "muted")}>{String(row.count)}</Text>
            </View>
            : <Text style={{ ...text(13, "muted"), paddingVertical: pt(10), paddingLeft: pt(30 + row.depth * 14) }}>Empty</Text>}
        </>)}
      </Section> : connected() && list() != null && <Note title={query().trim() ? "No matching projects" : "No projects yet"}
        message={query().trim() ? "Try another name or folder path." : "Create a project on this desktop to start a chat or terminal."}>
        {query().trim() ? <Action label="Clear search" action={() => query.set("")} />
          : services.projects.create && <Action label="New project" symbol="plus" disabled={app.blocked()} action={() => creating.set(true)} />}
      </Note>}
    </Scroll>
    <NewProjectForm app={app} ui={ui} host={hostId} open={creating()} onClose={() => creating.set(false)}
      onUncertain={() => void read(true)}
      onCreated={(project) => {
        // Just made: it counts as just active until the Mac has a figure for it, so Recent puts it first.
        touched.set({ ...touched(), [project.id]: project.createdAt ?? Math.floor(Date.now() / 1000) });
        list.set([...(list() ?? []).filter((entry) => entry.id !== project.id), project]);
        void read(true);
        app.openProject(hostId, project, { newTerminal: true });
      }} />
  </ui.Page>;
}
