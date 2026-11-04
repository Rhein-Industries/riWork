import { Text, View, mount, signal } from "nsh";
import { Button } from "nsh/base";
import { info, project, tasks, type Project, type Task } from "./addin-api";

// An add-in is an NSH app in a tab of its own. It reaches Somtus through ./addin-api, which
// Somtus writes into every build; addin.json says what it may do.
function App() {
  const current = signal<Project | null>(null), open = signal<Task[]>([]), error = signal("");
  const load = () => Promise.all([project.get(), tasks.list()])
    .then(([next, list]) => { current.set(next); open.set(list.filter((task) => task.status !== "done")); })
    .catch((reason) => error.set(String(reason)));
  void load();
  return <View style={{ flex: 1, padding: 20, gap: 10 }}>
    <Text style={{ fontSize: 18, fontWeight: "600" }}>{info().name}</Text>
    <Text>{current() ? `${current()!.name} · ${current()!.worktrees.length} worktrees · ${open().length} open tasks` : "Loading…"}</Text>
    {open().slice(0, 8).map((task) => <Text style={{ opacity: 0.7 }}>{`· ${task.title}`}</Text>)}
    <Button testID="add-task" onPress={() => void tasks.add(`Follow up from ${info().name}`).then(load)}
      style={{ alignSelf: "flex-start", paddingHorizontal: 10, paddingVertical: 5, borderRadius: 6, backgroundColor: "#8884" }}>
      <Text>Add a task</Text>
    </Button>
    {error() !== "" && <Text style={{ color: "#c43" }}>{error()}</Text>}
  </View>;
}
mount(App);
