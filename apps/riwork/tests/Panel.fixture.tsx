import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Button } from "nsh/base";
import { Store, type ProjectDto, type ShellSessionDto } from "riwork/native";
import { PanelContent, ShellIntent, createPanelController } from "../Panels";
import type { ListPanel } from "../panel_model";

function Body(props: { projects: ProjectDto[]; directory: string }) {
  const project = signal(props.projects[0]);
  const panel = signal<ListPanel>("projects");
  const intent = signal<string | null>(null);
  const notice = signal("");
  const controller = createPanelController({
    projectId: () => project().id, projectLabel: () => project().name,
    switchProject: async (next) => { project.set(next); },
    openPanel: (next) => panel.set(next as ListPanel),
    showShell: (id) => intent.set(id), selectTask: () => {}, selectWorktree: () => {},
    notice: (message) => notice.set(message), defaultProjectDirectory: props.directory,
  });
  host.on("app.fixture.inspect", () => host.post("app.fixture.state", {
    state: controller.state(), projectId: project().id,
    selectedTask: controller.selectedTask(), selectedWorktree: controller.selectedWorktree(),
    query: controller.query(), pending: controller.pending(),
  }));
  host.on("app.fixture.intent", () => {
    // Only inventory data is injected. No session process or tmux server starts.
    const shell: ShellSessionDto = {
      id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", projectId: project().id,
      worktreeId: null, kind: "project", cwd: project().root, command: "fixture command",
      alive: true, userOpened: true, parentId: null, editorPath: null, harness: null,
      unrestricted: false, codexAccountId: null, codexAccountLabel: null,
      codexAccountEmail: null, codexHome: null, orchestratorSkillLoaded: false,
      orchestratorSkillVersion: null, orchestratorDelegationVersion: null,
      orchestratorProjectRoot: null, createdAtUnix: 1,
    };
    controller.shells.set([shell, { ...shell, id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", alive: false }]);
  });
  return <View style={{ flex: 1 }}>
    <View style={{ flexDirection: "row", gap: 10 }}>
      {(["projects", "worktrees", "tasks", "shells"] as const).map((value) =>
        <Button testID={`show-${value}`} onPress={() => panel.set(value)}><Text>{value}</Text></Button>)}
    </View>
    <PanelContent controller={controller} panel={panel()} surface="fixture" />
    {intent() && <ShellIntent controller={controller} shellId={intent()!} />}
    <Text testID="fixture-notice">{notice()}</Text>
  </View>;
}

function Fixture() {
  const params = host.params as { fixtureKey: string };
  const directory = `/tmp/riwork-nsh-panel-ui-${params.fixtureKey}`;
  const projects = signal<ProjectDto[] | null>(null), error = signal("");
  let alive = true;
  cleanup(() => { alive = false; });
  void (async () => {
    const result: ProjectDto[] = [];
    for (const name of ["Alpha", "Beta"]) result.push(await Store.createProject({
      root: `${directory}/${name}`, name: `${name}-${params.fixtureKey}`, initGit: false,
    }));
    if (alive) projects.set(result);
  })().catch((reason: unknown) => { if (alive) error.set(String(reason)); });
  return <View style={{ flex: 1 }}>{projects() ? <Body projects={projects()!} directory={`${directory}/created`} /> : <Text>{error() || "Preparing isolated panel fixture…"}</Text>}</View>;
}
mount(Fixture);
