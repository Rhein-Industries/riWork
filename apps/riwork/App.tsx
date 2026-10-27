import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Layouts, type LayoutSession } from "riwork/native";
import { Workspace } from "./Workspace";
import { windowTitle, type HostParams } from "./model";

function App() {
  const params = host.params as unknown as HostParams;
  const session = signal<LayoutSession | null>(null);
  const notice = signal(params.notice ?? "Loading workspace…");
  const name = params.path?.split("/").filter(Boolean).pop() ?? "Workspace";
  let alive = true;
  let restoreReady = false;
  function installed() {
    if (params.restore && !restoreReady) {
      restoreReady = true;
      host.post("app.restoreReady", {});
    }
  }
  cleanup(() => { alive = false; session()?.dispose(); });

  if (params.kind === "project" && params.projectId && params.windowId) {
    void Layouts.openSession({ windowId: params.windowId, projectId: params.projectId, restore: params.restore?.layout })
      .then((loaded) => { if (alive) session.set(loaded); else loaded.dispose(); })
      .catch((error: unknown) => notice.set(String(error)));
  } else {
    notice.set(params.kind === "global" ? "Global workspace" : "A project window is required.");
    if (params.kind === "global") installed();
  }
  return <View style={{ flex: 1 }}>
    {session() ? <Workspace session={session()!} title={windowTitle(name)} projectLabel={name} projectId={params.projectId ?? undefined} startupNotice={params.notice} onChange={installed} /> :
      <View style={{ flex: 1, padding: 20 }}><Text testID="startup-notice">{notice()}</Text></View>}
  </View>;
}

mount(App);
