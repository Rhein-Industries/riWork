import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Layouts, type LayoutSession } from "riwork/native";
import { Workspace } from "../Workspace";

// Native events are delivered by EngineControl in preview_focus.rs, never by
// a live view. Preview owners remain idle; no Run or real project is involved.
const previewId = "10000000-0000-4000-8000-000000000183";
const previewKey = `app_preview:${previewId}`;
function Fixture() {
  const session = signal<LayoutSession | null>(null), project = signal("");
  const error = signal("");
  let alive = true;
  cleanup(() => { alive = false; session()?.dispose(); });
  cleanup(host.on("app.fixture.settle", () => { setTimeout(() => host.post("app.fixture.settled", {}), 100); }));
  void Layouts.fixtureManifest().then(async (value) => {
    const entry = (value as { fixtures: { name: string; projectId: string }[] }).fixtures.find((entry) => entry.name === "L-MAIN")!;
    project.set(entry.projectId);
    const loaded = await Layouts.openSession({ windowId: "native-preview-focus", projectId: entry.projectId, restore: {
      layout: { split: { axis: "side_by_side", ratio: 0.5, first: { pane: 1 }, second: { pane: 2 } } },
      active_pane: 2, main_pane: 1, panels_initialized: true, locked_panes: [],
      panes: {
        "1": { tabs: [{ kind: "app_preview", preview_id: previewId,
          spec: { command: "", cwd: "/fixture/never-launched", mode: "cooperative" } },
          { kind: "panel", panel: "projects" }], active_tab_key: previewKey },
        "2": { tabs: [{ kind: "panel", panel: "tasks" }], active_tab_key: "panel:tasks" },
      },
    } });
    if (alive) session.set(loaded); else loaded.dispose();
  }).catch((reason) => error.set(String(reason)));
  return <View style={{ flex: 1 }}>{session() ? <Workspace session={session()!} projectId={project()}
    title="Native preview focus fixture" projectLabel="Fixture"
    onChange={(snapshot) => host.post("app.fixture.state", snapshot)} /> : <Text>{error() || "Loading…"}</Text>}</View>;
}
mount(Fixture);
