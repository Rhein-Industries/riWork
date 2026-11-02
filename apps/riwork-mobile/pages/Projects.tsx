/** A host's projects: search, the person's order (`riwork.projectSort`), the Mac's folders when it gives them
 * (`native.invoke` `Store.snapshot`), what each project's agents are doing (`projects.list`, read again while
 * the page is on screen), and New project. A project opens its page; ＋ opens it with New terminal. On the
 * iPad it is the window's sidebar, drawn as the Mac's Projects panel (`apps/riwork/Panels.tsx`): a header
 * with Sort and New project, the search, the folders, and a row per project of its name and what its agents
 * are doing, the project on screen on the rounded highlight with ＋ and its ⋯. */
import { Scroll, Text, View, cleanup, computed, effect, keyboardFrame, keymap, signal, untracked, type YNode } from "nsh";
import { AlertDialog, Input, type AlertDialogAction } from "nsh/base";
import { Icon } from "../../riwork/Icon";
import { PortError } from "../../riwork/client/ports";
import type { ProjectSummary, StoreTree } from "../../riwork/client/types";
import type { MobileApp } from "../app_state";
import { PROJECT_SORTS, listedProjects, projectActivity, projectListRows, projectSort, sortProjects, type ListedProject, type ProjectSort } from "../projects_model";
import { Symbol } from "../Symbol";
import type { Kit, Layout } from "../ui";
import { NewProjectForm } from "./NewProject";

/** How often the list is read again while it is on screen: as the native page did; the iPad's sidebar, always up, less often. */
const PHONE_POLL_MS = 4_000, PAD_POLL_MS = 10_000;

export function ProjectsPage(props: {
  app: MobileApp; ui: Kit; layout: Layout; host: string; onTop: () => boolean;
  /** The iPad: the project on screen beside the sidebar, drawn selected. */
  current?: () => string | null;
  /** Its keyboard shortcuts are live (the iPad: not while a project's page beside it has the keys). */
  keys?: () => boolean;
  /** The iPad: a project was chosen (the sidebar, open over the main area, closes). */
  onOpen?: () => void;
}) {
  const { app, ui } = props;
  const { Action, Tool, Section, Divider, Note, Failure, palette, pt, target, text } = ui;
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
  /** The Mac lists its folders' read: when that changes (another connection), the folders are read again. */
  const folders = computed(() => (app.hosts().find((info) => info.id === hostId)?.features ?? []).includes("native:Store.snapshot"));
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
    folders();
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
    props.onOpen?.();
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
  function Form() {
    return <NewProjectForm app={app} ui={ui} host={hostId} open={creating()} onClose={() => creating.set(false)}
      onUncertain={() => void read(true)}
      onCreated={(project) => {
        // Just made: it counts as just active until the Mac has a figure for it, so Recent puts it first.
        touched.set({ ...touched(), [project.id]: project.createdAt ?? Math.floor(Date.now() / 1000) });
        list.set([...(list() ?? []).filter((entry) => entry.id !== project.id), project]);
        void read(true);
        app.openProject(hostId, project, { newTerminal: true });
      }} />;
  }
  if (props.layout === "pad") return <Sidebar />;

  // MARK: The iPad's sidebar, the Mac's Projects panel in touch density

  async function sortMenu() {
    const actions: AlertDialogAction[] = PROJECT_SORTS.map((choice) => ({ id: choice.value, label: choice.value === sort() ? `${choice.label} ✓` : choice.label }));
    actions.push({ id: "cancel", label: "Cancel", role: "cancel" });
    const choice = await AlertDialog.show({ title: "Sort projects", presentation: "actionSheet", actions }).catch(() => null);
    if (alive && choice && choice !== "cancel") chooseSort(projectSort(choice));
  }
  /** The ⋯ of the project on screen: what the Mac's row and its page offer that this device does. */
  async function projectMenu(project: ListedProject) {
    const features = app.hosts().find((info) => info.id === hostId)?.features ?? [];
    const actions: AlertDialogAction[] = [{ id: "new", label: "New terminal" }];
    if (features.includes("chat") || features.includes("orchestrator_create")) actions.push({ id: "chat", label: "New chat or orchestrator…" });
    if (features.includes("tabs")) actions.push({ id: "open", label: "Open a worker or shell…" });
    if (features.includes("project_memo")) actions.push({ id: "memo", label: "Memo…" });
    actions.push({ id: "cancel", label: "Cancel", role: "cancel" });
    const choice = await AlertDialog.show({ title: project.name, description: project.root, presentation: "actionSheet", actions }).catch(() => null);
    if (!alive || !choice || choice === "cancel") return;
    props.onOpen?.();
    if (choice === "new") open(project, true);
    else if (choice === "open") app.openProject(hostId, project, { picker: true });
    else if (choice === "chat") void app.present({ name: "newChat", host: hostId, projectId: project.id });
    else if (choice === "memo") void app.present({ name: "memo", host: hostId, projectId: project.id });
  }
  function SidebarProject(row: { project: ListedProject; depth: number }) {
    const current = () => props.current?.() === row.project.id;
    // The keyboard's place, once it was moved there (or a project was opened): a ring, apart from the project on screen.
    const cursor = () => keyboard().hardware && keysLive() && highlighted() != null && selected()?.id === row.project.id && !current();
    const summary = () => projectActivity(row.project)?.text ?? "";
    const disabled = () => app.blocked() || !connected();
    return <View ref={(node) => projectRefs.set(row.project.id, node)} style={{ flexDirection: "row", alignItems: "center", borderRadius: pt(10),
      borderWidth: 1, borderColor: cursor() ? palette().muted : "transparent", backgroundColor: current() ? palette().panelActive : "transparent" }}>
      <View testID={`project:${row.project.id}`} role="button" accessibilityLabel={`${row.project.name}. ${row.project.root}.${summary() ? ` ${summary()}.` : ""}`}
        accessibilityState={{ selected: current() }} disabled={disabled()} disabledStyle={{ opacity: 0.5 }}
        activeStyle={{ backgroundColor: palette().panelActive }} onPress={() => open(row.project)}
        style={{ flex: 1, minWidth: 0, minHeight: target(), justifyContent: "center", gap: pt(2), paddingVertical: pt(7),
          paddingLeft: pt(10 + row.depth * 20), paddingRight: pt(6), borderRadius: pt(10) }}>
        <Text numberOfLines={1} style={text(15)}>{row.project.name}</Text>
        {summary() && <Text numberOfLines={1} style={text(13, "muted")}>{summary()}</Text>}
      </View>
      {current() && <>
        <Tool testID={`project-new-terminal:${row.project.id}`} icon="add" label={`New terminal in ${row.project.name}`} disabled={disabled()} action={() => open(row.project, true)} />
        <Tool testID={`project-actions:${row.project.id}`} icon="more" label={`${row.project.name} actions`} disabled={disabled()} action={() => void projectMenu(row.project)} />
      </>}
    </View>;
  }
  function SidebarFolder(row: { id: string; name: string; count: number; depth: number }) {
    const folderId = row.id.slice("folder-".length);
    return <View testID={row.id} role="button" accessibilityLabel={`${row.name}, ${row.count} ${row.count === 1 ? "project" : "projects"}`}
      accessibilityState={{ expanded: !collapsed().has(folderId) }} onPress={() => toggle(folderId)} activeStyle={{ backgroundColor: palette().panelActive }}
      style={{ minHeight: target(), paddingLeft: pt(10 + row.depth * 20), paddingRight: pt(12), flexDirection: "row", alignItems: "center", gap: pt(8), borderRadius: pt(10) }}>
      <Icon name={collapsed().has(folderId) ? "right" : "down"} size={pt(12)} color={palette().muted} />
      <Text numberOfLines={1} style={{ ...text(13, "muted"), fontWeight: "600", flex: 1 }}>{row.name}</Text>
      <Text style={text(12, "muted")}>{String(row.count).padStart(2, "0")}</Text>
    </View>;
  }
  function Sidebar() {
    // With folders, an Unfiled that holds nothing is only noise (the Mac's panel leaves it out too).
    const shown = () => rows().filter((row) => !(row.kind === "folder" && row.id === "folder-unfiled" && row.count === 0) && row.id !== "empty-unfiled");
    const count = () => {
      const total = projects().length, hits = query().trim() ? ordered().length : total;
      return `${hits === total ? total : `${hits} of ${total}`} ${total === 1 ? "project" : "projects"}`;
    };
    // What the last call to Swift said is the main area's to show.
    const notes = () => !!(failure() || !connected());
    return <View testID={`page:projects:${hostId}`} style={{ flex: 1, minHeight: 0, backgroundColor: palette().panel }}>
      <View style={{ minHeight: pt(62), paddingLeft: pt(16), paddingRight: pt(6), flexDirection: "row", alignItems: "center" }}>
        <View style={{ flex: 1, minWidth: 0, gap: pt(1) }}>
          <Text role="header" numberOfLines={1} style={{ ...text(17), fontWeight: "600" }}>Projects</Text>
          <Text numberOfLines={1} style={text(13, "muted")}>{list() || tree() ? count() : connected() ? "Reading…" : app.link(hostId).label}</Text>
        </View>
        <Tool testID="sort-projects" icon="sort" label={`Sort projects: ${PROJECT_SORTS.find((choice) => choice.value === sort())!.label}`}
          disabled={app.blocked() || projects().length < 2} action={() => void sortMenu()} />
        <Tool testID="new-project" icon="add" label="New project" disabled={app.blocked() || !connected() || !services.projects.create} action={() => creating.set(true)} />
      </View>
      <View style={{ paddingHorizontal: pt(14), paddingBottom: pt(10) }}>
        <View onPress={() => searchInput?.focus()} style={{ height: target(), flexDirection: "row", alignItems: "center", gap: pt(8), paddingLeft: pt(14),
          paddingRight: query() ? 0 : pt(14), borderRadius: target() / 2, backgroundColor: palette().panelActive }}>
          <Icon name="search" size={pt(17)} color={palette().muted} />
          <Input ref={(node) => { searchInput = node; }} accessibilityLabel="Search projects" placeholder={keyboard().hardware ? "Search  ⌘F" : "Search"} value={query()}
            onChange={(value) => query.set(value)} onFocus={() => searchFocused.set(true)} onBlur={() => searchFocused.set(false)}
            autoCapitalize="none" autoCorrect={false} returnKeyType="search"
            style={{ ...text(15), flex: 1, minWidth: 0, padding: 0, backgroundColor: "transparent" }} />
          {query() !== "" && <Tool icon="close" label="Clear the search" action={() => { query.set(""); searchInput?.focus(); }} />}
        </View>
      </View>
      <Scroll ref={(node) => { scroll = node; }} onScroll={(event) => { scrollY = event.offset.y; }}
        style={{ flex: 1 }} contentStyle={{ paddingHorizontal: pt(8), paddingBottom: keyboardFrame().height + pt(12) }}
        refreshing={loading() && list() == null} onRefresh={refresh} keyboardDismissMode="interactive" keyboardShouldPersistTaps="handled">
        {notes() && <View style={{ paddingHorizontal: pt(4), paddingBottom: pt(10), gap: pt(8) }}>
          {failure() && <Note message={list() ? `Saved projects · ${failure()}` : failure()} error={!list()} />}
          {!connected() && <Note message={`${app.link(hostId).label}. ${list() ? "These projects are from the last read." : "Connect to this desktop to list its projects."}`} />}
        </View>}
        {shown().map((row) => row.kind === "project" ? <SidebarProject project={row.project} depth={row.depth} />
          : row.kind === "folder" ? <SidebarFolder id={row.id} name={row.name} count={row.count} depth={row.depth} />
          : <Text style={{ ...text(13, "muted"), paddingVertical: pt(10), paddingLeft: pt(30 + row.depth * 20) }}>Empty</Text>)}
        {empty() && connected() && list() != null && <View style={{ paddingHorizontal: pt(4) }}>
          <Note title={query().trim() ? "No matching projects" : "No projects yet"}
            message={query().trim() ? "Try another name or folder path." : "Create a project on this desktop to start a chat or terminal."}>
            {query().trim() ? <Action label="Clear search" action={() => query.set("")} />
              : services.projects.create && <Action label="New project" icon="add" disabled={app.blocked()} action={() => creating.set(true)} />}
          </Note>
        </View>}
      </Scroll>
      <Form />
    </View>;
  }

  return <ui.Page layout={props.layout} title="Projects" testID={`page:projects:${hostId}`} backTitle="Hosts" addButton={true}
    right={<Action label="New project" disabled={app.blocked() || !connected() || !services.projects.create} action={() => creating.set(true)} />}>
    <Scroll ref={(node) => { scroll = node; }} onScroll={(event) => { scrollY = event.offset.y; }}
      style={{ flex: 1 }} contentStyle={{ padding: pt(16), paddingBottom: Math.max(bottom(), keyboardFrame().height) + pt(24), gap: pt(16) }}
      refreshing={loading() && list() == null} onRefresh={refresh} keyboardDismissMode="interactive" keyboardShouldPersistTaps="handled">
      <Failure />
      {failure() && <Note message={list() ? `Saved projects · ${failure()}` : failure()} error={!list()} />}
      {!connected() && <Note message={`${app.link(hostId).label}. ${list() ? "These projects are from the last read." : "Connect to this desktop to list its projects."}`} />}
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
    <Form />
  </ui.Page>;
}
