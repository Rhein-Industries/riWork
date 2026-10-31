import { Text, View, host, mount, signal } from "nsh";
import type { StoreState, TaskDto, WorktreeDto } from "riwork/native";
import { PanelContent, createPanelController } from "../Panels";
import { EMPTY_STORE } from "../panel_model";

// The Worktrees panel with in-memory data, for scripts/pane-shot.sh. The panel reads a
// fixture-owned state signal, so no Store delivery can replace what it shows. Run it with
// an isolated RIWORK_HOME: the controller still opens its native subscriptions.
const PROJECT = "fixture-riwork", ROOT = "/Users/dana/code/riwork";
const tree = (id: string, branch: string, path: string, extra: Partial<WorktreeDto> = {}): WorktreeDto =>
  ({ id, projectId: PROJECT, branch, path, isPrimary: false, repositoryRoot: ROOT, createdAt: 1_761_800_000, present: true, ...extra });
const WORKTREES: WorktreeDto[] = [
  tree("0c4e8a12-5b7d-4f0e-9a31-6d2f8b1c7e90", "main", ROOT, { isPrimary: true }),
  tree("7a91d3f4-2c68-4b1e-8f05-3e9c6a4d2b17", "feature/worktree-panel", `${ROOT}/.claude/worktrees/agent-a9a39e1eb89d0cf4f`),
  tree("b25f7c03-9e14-4d8a-a6c2-81f0e3d5b469", "fix/chat-scroll-restore", "/Users/dana/code/riwork-worktrees/fix-chat-scroll-restore"),
  tree("e8d04b6a-7f32-4c95-b1e7-2a6c9d8f3051", "spike/remote-relay-reconnect-with-exponential-backoff", "/Users/dana/code/riwork-worktrees/spike-remote-relay-reconnect"),
  tree("3f6a2e81-d4c7-4b09-8e5a-c17b2f90d6e4", "release/ios-1.4", "/Users/dana/code/riwork-ios-worktrees/release-1.4", { repositoryRoot: "/Users/dana/code/riwork-ios" }),
];
const task = (id: string, title: string, status: string, worktreeId: string | null): TaskDto =>
  ({ id, projectId: PROJECT, title, details: "", status, worktreeId, createdAt: 1_761_800_000, updatedAt: 1_761_900_000 });
const TASKS: TaskDto[] = [
  task("t1", "Show branch and path on worktree rows", "in_progress", WORKTREES[1].id),
  task("t2", "Mark the selected worktree", "todo", WORKTREES[1].id),
  task("t3", "Count tasks per worktree", "done", WORKTREES[1].id),
  task("t4", "Restore chat scroll after reload", "done", WORKTREES[2].id),
  task("t5", "Keep scroll anchored while streaming", "done", WORKTREES[2].id),
  task("t6", "Release notes", "todo", WORKTREES[4].id),
];
// "missing": the primary checkout is gone. "stranded": the selected linked worktree is gone,
// which hides its row while the window keeps it selected. "empty": no worktrees at all.
function scenario(name: string): StoreState {
  const gone = (pick: (tree: WorktreeDto) => boolean) => WORKTREES.map((tree) => pick(tree) ? { ...tree, present: false } : tree);
  const worktrees = name === "empty" ? [] : name === "missing" ? gone((tree) => tree.isPrimary) : name === "stranded" ? gone((tree) => tree.id === WORKTREES[1].id) : WORKTREES;
  return { ...EMPTY_STORE, worktrees, tasks: name === "empty" ? [] : TASKS };
}

function Fixture() {
  const state = signal<StoreState>(scenario("default"));
  const notice = signal("");
  const controller = createPanelController({
    projectId: () => PROJECT, projectLabel: () => "riwork",
    switchProject: async () => {}, openPanel: () => {}, showShell: () => {}, selectTask: () => {}, selectWorktree: () => {},
    notice: (message) => notice.set(message), initialWorktree: WORKTREES[1].id,
  });
  // Steps switch what the panel shows: `await app.host.send("app.fixture.scenario", { name: "empty" })`.
  host.on("app.fixture.scenario", (event) => state.set(scenario((event as { name: string }).name)));
  return <View style={{ flex: 1 }}>
    <View style={{ flex: 1, minHeight: 0 }}><PanelContent controller={{ ...controller, state }} panel="worktrees" surface="fixture" /></View>
    {notice() !== "" && <Text testID="fixture-notice" numberOfLines={2} style={{ padding: 6, fontSize: 10 }}>{notice()}</Text>}
  </View>;
}
mount(Fixture);
