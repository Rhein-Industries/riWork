import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Files, Store } from "riwork/native";
import { ProjectSettingsPanel } from "../settings/ProjectSettings";

function Body(props: { projectId: string; groupId: string; childId: string }) {
  let controls: { revealMemo: () => void } | null = null;
  cleanup(host.on("app.fixture.inspect", () => {
    void Store.snapshot().then((state) => host.post("app.fixture.state", {
      project: state.projects.find((project) => project.id === props.projectId),
      folders: state.projectFolders, groupId: props.groupId, childId: props.childId,
      memo: state.projectMemos[props.projectId]?.text ?? "",
    }));
  }));
  cleanup(host.on("app.fixture.external", () => { void Store.renameProject(props.projectId, "Changed elsewhere"); }));
  cleanup(host.on("app.fixture.memo", () => controls?.revealMemo()));
  return <ProjectSettingsPanel projectId={() => props.projectId} onControl={(control) => { controls = control; }} />;
}

function Fixture() {
  const key = (host.params as { key: string }).key;
  const ready = signal<{ projectId: string; groupId: string; childId: string } | null>(null), error = signal("");
  let alive = true; cleanup(() => { alive = false; });
  void (async () => {
    // Files.fixtureDirectory refuses non-fixture use. The runner supplies a private home.
    const root = await Files.fixtureDirectory(key);
    const project = await Store.createProject({ root, name: "Original project", initGit: false });
    const group = await Store.createFolder(`Group ${key}`);
    const child = await Store.createFolder("日本語 child", group.id);
    if (alive) ready.set({ projectId: project.id, groupId: group.id, childId: child.id });
  })().catch((reason) => { if (alive) error.set(String(reason)); });
  return <View style={{ flex: 1 }}>{ready() ? <Body {...ready()!} /> : <Text>{error() || "Preparing isolated project settings…"}</Text>}</View>;
}
mount(Fixture);
