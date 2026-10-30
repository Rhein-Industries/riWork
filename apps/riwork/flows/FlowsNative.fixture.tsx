/** The Flows panel on the real native module, in the runner's private home and a fixture
 * project folder. No chat is started. */
import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Files, Flows, Store } from "riwork/native";
import { FlowsPanel } from "./FlowsPanel";

function Fixture() {
  const key = (host.params as { key: string }).key;
  const project = signal<string | null>(null), error = signal("");
  const edited: string[] = [];
  let alive = true; cleanup(() => { alive = false; });
  void (async () => {
    // Files.fixtureDirectory refuses non-fixture use. The runner supplies a private home.
    const root = await Files.fixtureDirectory(key);
    const made = await Store.createProject({ root, name: "Flows project", initGit: false });
    if (alive) project.set(made.id);
  })().catch((reason) => { if (alive) error.set(String(reason)); });
  // Another writer (the CLI, an agent) adds a flow: the panel sees it through its watch.
  cleanup(host.on("app.fixture.flows.external", () => { void Flows.create(project()!, "Written elsewhere"); }));
  cleanup(host.on("app.fixture.flows.inspect", () => host.post("app.fixture.flows.state", { edited })));
  return <View style={{ flex: 1 }}>{project() ? <FlowsPanel projectId={() => project()!} chats={() => []}
    openChat={() => {}} editFile={(path) => { edited.push(path); }} /> : <Text>{error() || "Preparing an isolated project…"}</Text>}</View>;
}
mount(Fixture);
