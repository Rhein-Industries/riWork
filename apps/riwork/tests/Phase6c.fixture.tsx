import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Cua, Store, Layouts, type LayoutSession } from "riwork/native";
import { CuaSetup, type CuaState } from "../CuaSetup";
import { Workspace } from "../Workspace";
import type { WorkspaceSnapshot } from "../model";

function Fixture() {
  const params = host.params as unknown as { surface?: string; cua?: CuaState; windowId: string; worktree?: boolean; restore?: unknown };
  const cua = signal<CuaState>(params.cua ?? { status: null, pending: null, error: null });
  if (params.surface === "cua") return <CuaSetup checkOnShow={false} controller={{ state: cua, run: async (action) => { host.post("app.fixture.action", { action }); } }} />;
  const session = signal<LayoutSession | null>(null);
  const project = signal("");
  const error = signal("");
  let alive = true;
  let last: WorkspaceSnapshot | undefined;
  cleanup(() => { alive = false; session()?.dispose(); });
  cleanup(host.on("app.fixture.inspect", () => host.post("app.fixture.state", last)));
  cleanup(host.on("app.fixture.flush", () => { void session()?.flush().then(() => host.post("app.fixture.flushed", {})); }));
  void Layouts.fixtureManifest().then(async (value) => {
    const checked = await Cua.run("check");
    host.post("app.fixture.cua", checked);
    const entry = (value as { fixtures: { name: string; projectId: string }[] }).fixtures.find((entry) => entry.name === "L-MAIN")!;
    project.set(entry.projectId);
    const loaded = await Layouts.openSession({ windowId: params.windowId, projectId: entry.projectId, restore: params.restore });
    if (params.worktree) {
      const state = await Store.snapshot();
      const worktree = state.worktrees.find((tree) => tree.projectId === entry.projectId && tree.present)!;
      loaded.setSelectedWorktree(worktree.id);
      host.post("app.fixture.cwd", { cwd: worktree.path });
    }
    if (alive) session.set(loaded); else loaded.dispose();
  }).catch((reason) => error.set(String(reason)));
  return <View style={{ flex: 1 }}>{session() ? <Workspace session={session()!} projectId={project()} title="Preview fixture" projectLabel="Fixture"
    onChange={(snapshot) => { last = snapshot; host.post("app.fixture.state", snapshot); }} /> : <Text>{error() || "Loading…"}</Text>}</View>;
}
mount(Fixture);
