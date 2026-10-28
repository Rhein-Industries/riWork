import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Layouts, type LayoutSession } from "riwork/native";
import { Workspace } from "../Workspace";

function Fixture() {
  const session = signal<LayoutSession | null>(null), error = signal("");
  let alive = true;
  cleanup(() => { alive = false; session()?.dispose(); });
  void Layouts.openSession({ windowId: `remote-routing-${(host.params as { key: string }).key}`, projectId: "remote:fixture-mac:fixture-project" })
    .then((value) => { if (alive) session.set(value); else value.dispose(); }).catch((reason) => error.set(String(reason)));
  return <View style={{ flex: 1 }}>{session() ? <Workspace session={session()!} projectId="remote:fixture-mac:fixture-project" projectLabel="Remote fixture" title="RiWork · Remote fixture"
    onChange={(snapshot) => host.post("app.fixture.state", snapshot)} /> : <Text>{error() || "Loading…"}</Text>}</View>;
}
mount(Fixture);
