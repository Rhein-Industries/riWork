import { Text, View, cleanup, mount, signal } from "nsh";
import { Store, type ProjectDto } from "riwork/native";
import { privateFixtureDirectory } from "./private-home";
import { PanelContent, createPanelController } from "../Panels";

// The Projects panel with folders, a nested folder, an empty folder, a long name and
// tasks, for scripts/pane-shot.sh. Seeding goes through privateFixtureDirectory, which
// refuses unless RIWORK_NSH_FILES_FIXTURE=1 and RIWORK_HOME is an isolated /tmp home.
function Body(props: { active: ProjectDto; directory: string }) {
  const project = signal(props.active);
  const notice = signal("");
  const controller = createPanelController({
    projectId: () => project().id, projectLabel: () => project().name,
    switchProject: async (next) => { project.set(next); },
    openPanel: () => {}, showShell: () => {}, selectTask: () => {}, selectWorktree: () => {},
    notice: (message) => notice.set(message), defaultProjectDirectory: props.directory,
  });
  return <View style={{ flex: 1 }}>
    <PanelContent controller={controller} panel="projects" surface="fixture" />
    {notice() !== "" && <Text testID="fixture-notice">{notice()}</Text>}
  </View>;
}

function Fixture() {
  const active = signal<{ project: ProjectDto; directory: string } | null>(null), error = signal("");
  let alive = true;
  cleanup(() => { alive = false; });
  void (async () => {
    const base = await privateFixtureDirectory("projects-panel");
    const add = (name: string, initGit: boolean) => Store.createNewProject({ root: `${base}/${name}`, name, initGit });
    const clients = await Store.createFolder("Clients");
    const archive = await Store.createFolder("Archive", clients.id);
    const open = await Store.createFolder("Open source");
    await Store.createFolder("Experiments");
    const billing = await add("legacy-billing-api", true);
    const site = await add("acme-marketing-site-redesign-2026-q4", false);
    const portal = await add("acme-portal", true);
    const nsh = await add("nsh", true);
    const dotfiles = await add("dotfiles", false);
    const riwork = await add("riwork", true);
    await Store.moveProjectToFolder(billing.id, archive.id);
    await Store.moveProjectToFolder(site.id, clients.id);
    await Store.moveProjectToFolder(portal.id, clients.id);
    await Store.moveProjectToFolder(nsh.id, open.id);
    for (const [title, status] of [["Polish the projects panel", "in_progress"], ["Drag projects into folders", "todo"], ["Keep the edited order", "done"]] as const) {
      const task = await Store.addTask(riwork.id, title, "");
      if (status !== "todo") await Store.setTaskStatus(task.id, status);
    }
    await Store.addTask(portal.id, "Invoice export", "");
    for (const project of [billing, site, dotfiles, portal, nsh, riwork]) await Store.useProject(project.id);
    if (alive) active.set({ project: riwork, directory: base });
  })().catch((reason: unknown) => { if (alive) error.set(String(reason)); });
  return <View style={{ flex: 1 }}>{active() ? <Body active={active()!.project} directory={active()!.directory} /> : <Text>{error() || "Preparing the projects fixture…"}</Text>}</View>;
}
mount(Fixture);
