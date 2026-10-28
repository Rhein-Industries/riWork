import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Files, Layouts, Store, type LayoutSession } from "riwork/native";
import { Workspace } from "../Workspace";

function Fixture() {
  const params = host.params as { key: string; unavailable?: boolean };
  const key = params.key;
  const ready = signal<{ session: LayoutSession; projectId: string } | null>(null);
  const error = signal("");
  let alive = true;
  cleanup(() => { alive = false; ready()?.session.dispose(); });
  void (async () => {
    const root = await Files.fixtureDirectory(key);
    const project = await Store.createProject({ root, name: `Workspace files ${key}`, initGit: false });
    const session = await Layouts.openSession({ windowId: `files-workspace-${key}`, projectId: project.id });
    if (params.unavailable) session.setSelectedWorktree("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    if (alive) ready.set({ session, projectId: project.id }); else session.dispose();
  })().catch((reason) => { if (alive) error.set(String(reason)); });
  return <View style={{ flex: 1 }}>{ready() ? <Workspace session={ready()!.session} projectId={ready()!.projectId}
    title="RiWork · Files fixture" projectLabel="Files fixture"
    onChange={(snapshot) => host.post("app.fixture.state", snapshot)} /> : <Text>{error() || "Preparing files workspace…"}</Text>}</View>;
}
mount(Fixture);
