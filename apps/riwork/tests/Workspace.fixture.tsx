import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Layouts, type LayoutSession } from "riwork/native";
import { Workspace } from "../Workspace";

function Fixture() {
  const params = host.params as { windowId: string; projectId: string; restore?: unknown; fixtureName?: string };
  const session = signal<LayoutSession | null>(null);
  const error = signal("");
  let alive = true;
  let latest: unknown;
  let manifest: unknown;
  let fixture: unknown;
  cleanup(() => { alive = false; session()?.dispose(); });
  host.on("app.fixture.inspect", () => host.post("app.fixture.state", latest));
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
    {session() ? <Workspace session={session()!} title="RiWork · Fixture" projectLabel="Fixture"
      onChange={(snapshot, dock) => { latest = { snapshot, dock, manifest, fixture }; host.post("app.fixture.state", latest); }} /> : <Text>{error() || "Loading fixture…"}</Text>}
  </View>;
}
mount(Fixture);
