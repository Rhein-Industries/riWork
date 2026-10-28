import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Accounts, Files, Layouts, Settings, Store, type LayoutSession } from "riwork/native";
import { Workspace } from "../Workspace";
import { useAppearance } from "../appearance";

function Body(props: { projectId: string; session: LayoutSession }) {
  const ui = useAppearance({ schema: 1, windowId: "settings-routing", kind: "project", projectId: props.projectId, path: "/fixture/Settings", appearance: "system" });
  cleanup(host.on("app.fixture.inspect", () => {
    void Promise.all([Store.snapshot(), Settings.get(), Accounts.list()]).then(([store, settings, accounts]) => {
      host.post("app.fixture.settingsState", { store, settings, accounts });
    });
  }));
  return <View style={{ flex: 1 }}>{ui.ready() && <Workspace session={props.session} projectId={props.projectId}
    projectLabel="Settings routing" title="RiWork · Settings fixture" settings={ui.settings} theme={ui.theme}
    onChange={(snapshot) => host.post("app.fixture.state", snapshot)} />}</View>;
}
function Fixture() {
  const key = (host.params as { key: string }).key;
  const ready = signal<{ projectId: string; session: LayoutSession } | null>(null), error = signal("");
  let alive = true;
  cleanup(() => { alive = false; ready()?.session.dispose(); });
  void (async () => {
    const root = await Files.fixtureDirectory(key);
    const project = await Store.createProject({ root, name: `Settings routing ${key}`, initGit: false });
    await Settings.set({ theme: "native", uiTextSize: 11, uiTextMatchesTerminal: false });
    await Settings.resetStatusBar(); await Accounts.selectDefault(null);
    const session = await Layouts.openSession({ windowId: `settings-routing-${key}`, projectId: project.id });
    if (alive) ready.set({ projectId: project.id, session }); else session.dispose();
  })().catch((reason) => { if (alive) error.set(String(reason)); });
  return <View style={{ flex: 1 }}>{ready() ? <Body {...ready()!} /> : <Text>{error() || "Preparing isolated Settings workspace…"}</Text>}</View>;
}
mount(Fixture);
