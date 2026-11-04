import { Scroll, Text, View, cleanup, mount, signal } from "nsh";
import { Button } from "nsh/base";
import { info, onPaletteChanged, workspace, type WorkspaceCommand, type WorkspaceContext } from "./addin-api";

function Navigator() {
  const palette = signal(info().palette ?? {});
  cleanup(onPaletteChanged((next) => palette.set(next)));
  const ink = () => palette().text ?? "#222", muted = () => palette().muted ?? "#777";
  const context = signal<WorkspaceContext | null>(null), error = signal("");
  let alive = true, unsubscribe: (() => void) | undefined;
  cleanup(() => { alive = false; unsubscribe?.(); });
  void workspace.subscribe((next) => context.set(next)).then((stop) => {
    if (alive) unsubscribe = stop; else stop();
  }).catch((reason) => { if (alive) error.set(String(reason)); });
  function act(action: () => Promise<void>) {
    error.set("");
    void action().catch((reason) => { if (alive) error.set(String(reason)); });
  }
  const button = (title: string, action: () => Promise<void>) => <Button onPress={() => act(action)}
    style={{ paddingHorizontal: 10, paddingVertical: 7, borderRadius: 5, backgroundColor: "#8882" }}>
    <Text style={{ color: ink() }}>{title}</Text>
  </Button>;
  const commands: [string, WorkspaceCommand][] = [["Projects", "openProjects"], ["Files", "openFiles"], ["New shell", "newTab"],
    ["New chat", "newChat"], ["Split right", "splitRight"], ["Focus tab", "toggleFocusMode"], ["Customize", "openCustomization"]];
  return <View style={{ flex: 1, minHeight: 0, padding: 14, gap: 12, backgroundColor: palette().bg ?? "transparent" }}>
    <Text style={{ color: ink(), fontSize: 17, fontWeight: "600" }}>Workspace navigator</Text>
    <Text style={{ color: muted(), fontSize: 11 }}>Panes and tabs in this window</Text>
    <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>{commands.map(([title, id]) => button(title, () => workspace.command(id)))}</View>
    {!!error() && <Text role="alert" style={{ color: palette().gold ?? "#b60" }}>{error()}</Text>}
    <Scroll style={{ flex: 1, minHeight: 0 }} contentStyle={{ gap: 12 }}>
      {(context()?.panes ?? []).map((pane) => <View style={{ padding: 10, gap: 6, borderRadius: 6, backgroundColor: "#8881" }}>
        {button(`Pane ${pane.id}${pane.active ? " · active" : ""}`, () => workspace.command("selectPane", { id: pane.id }))}
        {pane.tabs.map((tab) => button(`${tab.id === pane.activeTab ? "● " : ""}${tab.title || tab.kind}${tab.status ? ` · ${tab.status}` : ""}`, () => workspace.command("selectTab", { id: tab.id })))}
      </View>)}
    </Scroll>
  </View>;
}

mount(Navigator);
