import { Text, View, cleanup, host, mount, signal, type YNode } from "nsh";
import { Layouts, type LayoutSession } from "riwork/native";
import { Workspace } from "../Workspace";

function Fixture() {
  const params = host.params as { windowId: string; projectId: string; restore?: unknown; fixtureName?: string };
  const session = signal<LayoutSession | null>(null);
  const error = signal("");
  let alive = true;
  let latest: unknown;
  const nodes = new Map<string, YNode>();
  let manifest: unknown;
  let fixture: unknown;
  cleanup(() => { alive = false; session()?.dispose(); });
  host.on("app.fixture.inspect", () => host.post("app.fixture.state", latest));
  host.on("app.fixture.measure", async () => {
    const source = await nodes.get("tab:shell:first")?.measure();
    const destination = await nodes.get("panel:empty:3")?.measure();
    host.post("app.fixture.geometry", { source, destination });
  });
  host.on("app.fixture.flush", () => {
    void session()?.flush().then(() => host.post("app.fixture.flushed", {}));
  });
  async function load() {
    if (!params.fixtureName) return Layouts.openSession({ ...params });
    const value = await Layouts.fixtureManifest() as { fixtures: { name: string; projectId: string }[] };
    manifest = value;
    const entry = value.fixtures.find((entry) => entry.name === params.fixtureName);
    if (!entry) throw new Error(`Fixture ${params.fixtureName} is absent`);
    fixture = entry;
    return Layouts.openSession({ windowId: params.windowId, projectId: entry.projectId });
  }
  void load().then((value) => {
    if (alive) session.set(value); else value.dispose();
  }).catch((reason: unknown) => error.set(String(reason)));
  return <View style={{ flex: 1 }}>
    {session() ? <Workspace session={session()!} title="RiWork · Fixture" projectLabel="Fixture" diagnostics={true}
      onTerminalMount={(shellId) => host.post("app.fixture.terminalMount", { shellId })}
      onViewRef={(id, node) => nodes.set(id, node)}
      onChange={(snapshot, dock) => {
        // Geometry probes must not keep removed YNode subtrees alive either.
        const present = new Set(snapshot.panes.flatMap((pane) => [...pane.tabs.map((tab) => tab.id), `empty:${pane.id}`]));
        for (const key of nodes.keys()) if (!present.has(key.slice(key.indexOf(":") + 1))) nodes.delete(key);
        latest = { snapshot, dock, manifest, fixture }; host.post("app.fixture.state", latest);
      }} /> : <Text>{error() || "Loading fixture…"}</Text>}
  </View>;
}
mount(Fixture);
