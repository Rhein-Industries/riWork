/** The Tasks panel on a seeded project: open work in two statuses, finished work, a worktree
 * assignment and one task open in the detail. It uses the native Store, so run it with an
 * isolated /tmp RIWORK_HOME as the Panel tests do. No session or terminal starts. */
import { Text, View, cleanup, mount, signal } from "nsh";
import { Store } from "riwork/native";
import { PanelContent, createPanelController } from "../Panels";

const TASKS: { title: string; details: string; status: string; assign?: boolean }[] = [
  { title: "Write the 0.4 release notes", details: "", status: "todo" },
  { title: "Explain in the Assign menu why a remote project has no worktrees to choose from", details: "", status: "todo" },
  { title: "Draw the status icons at 14 pt", details: "", status: "done", assign: true },
  { title: "Fix the search field losing focus after ⌘F", details: "", status: "done" },
  { title: "Keep the task editor open when saving fails", details: "Today the form closes and the error shows as a notice.", status: "in_progress" },
  { title: "Show unread counts on the Sessions tab", status: "in_progress", assign: true,
    details: "Count chats that finished since they were last opened.\nClear the count when the chat is shown.\n\nThe badge sits after the tab title and uses the accent color." },
];

function Body(props: { projectId: string; selected: string }) {
  const notice = signal("");
  const controller = createPanelController({
    projectId: () => props.projectId, projectLabel: () => "riwork",
    switchProject: async () => {}, openPanel: () => {}, showShell: () => {},
    selectTask: () => {}, selectWorktree: () => {}, initialTask: props.selected,
    notice: (message) => notice.set(message),
  });
  return <View style={{ flex: 1 }}>
    <PanelContent controller={controller} panel="tasks" surface="fixture" />
    {notice() !== "" && <Text testID="fixture-notice">{notice()}</Text>}
  </View>;
}

function Fixture() {
  const ready = signal<{ projectId: string; selected: string } | null>(null), error = signal("");
  let alive = true;
  cleanup(() => { alive = false; });
  void (async () => {
    const project = await Store.createProject({ root: `/tmp/riwork-nsh-tasks-ui-${Date.now()}/riwork`, name: "riwork", initGit: true });
    const primary = (await Store.syncWorktrees(project.id)).find((tree) => tree.isPrimary);
    let selected = "";
    for (const [index, seed] of TASKS.entries()) {
      // The Store keeps whole seconds; one apart, the list order does not depend on random ids.
      if (index) await new Promise((resolve) => setTimeout(resolve, 1000));
      const task = await Store.addTask(project.id, seed.title, seed.details);
      if (seed.status !== "todo") await Store.setTaskStatus(task.id, seed.status);
      if (seed.assign && primary) await Store.assignTasks(primary.id, [task.id]);
      if (seed.assign && seed.status === "in_progress") selected = task.id;
    }
    if (alive) ready.set({ projectId: project.id, selected });
  })().catch((reason: unknown) => { if (alive) error.set(String(reason)); });
  return <View style={{ flex: 1 }}>{ready() ? <Body projectId={ready()!.projectId} selected={ready()!.selected} /> : <Text>{error() || "Preparing the tasks fixture…"}</Text>}</View>;
}
mount(Fixture);
