import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Layouts, type LayoutSession } from "riwork/native";
import { Workspace } from "../Workspace";

function Fixture() {
  const params = host.params as { windowId: string; projectId: string; restore?: unknown };
  const session = signal<LayoutSession | null>(null);
  const error = signal("");
  let alive = true;
  let latest: unknown;
  cleanup(() => { alive = false; session()?.dispose(); });
  host.on("app.fixture.inspect", () => host.post("app.fixture.state", latest));
  host.on("app.fixture.flush", () => {
    void session()?.flush().then(() => host.post("app.fixture.flushed", {}));
  });
  void Layouts.openSession({ ...params }).then((value) => {
    if (alive) session.set(value); else value.dispose();
  }).catch((reason: unknown) => error.set(String(reason)));
  return <View style={{ flex: 1 }}>
    {session() ? <Workspace session={session()!} title="RiWork · Fixture" projectLabel="Fixture"
      onTerminalMount={(shellId) => host.post("app.fixture.terminalMount", { shellId })}
      onChange={(snapshot, dock) => { latest = { snapshot, dock }; host.post("app.fixture.state", latest); }} /> : <Text>{error() || "Loading fixture…"}</Text>}
  </View>;
}
mount(Fixture);
