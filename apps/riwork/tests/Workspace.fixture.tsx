import { Text, View, cleanup, host, mount, signal, type YNode } from "nsh";
import { Layouts, type LayoutSession } from "riwork/native";
import { Workspace } from "../Workspace";

function Fixture() {
  const params = host.params as { windowId: string; projectId: string };
  const session = signal<LayoutSession | null>(null);
  const error = signal("");
  let alive = true;
  let latest: unknown;
  const nodes = new Map<string, YNode>();
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
  void Layouts.openSession({ ...params }).then((value) => {
    if (alive) session.set(value); else value.dispose();
  }).catch((reason: unknown) => error.set(String(reason)));
  return <View style={{ flex: 1 }}>
    {session() ? <Workspace session={session()!} title="RiWork · Fixture" projectLabel="Fixture"
      onViewRef={(id, node) => nodes.set(id, node)}
      onChange={(snapshot, dock) => { latest = { snapshot, dock }; host.post("app.fixture.state", latest); }} /> : <Text>{error() || "Loading fixture…"}</Text>}
  </View>;
}
mount(Fixture);
