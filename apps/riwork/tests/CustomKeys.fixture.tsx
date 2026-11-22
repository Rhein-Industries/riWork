import { Text, View, cleanup, host, keymap, mount, signal } from "nsh";
import type { LayoutSession } from "riwork/native";
import { workspaceCommands } from "../commands";
import type { Keybindings } from "../keyboard";
import type { WorkspaceSnapshot } from "../model";

function Fixture() {
  const bindings = signal<Keybindings>({});
  const event = signal("none");
  let count = 0;
  const record = (name: string) => event.set(`${name}:${++count}`);
  cleanup(host.on("app.fixture.keys", (value) => bindings.set(value as Keybindings)));
  const transient = signal(false);
  cleanup(host.on("app.fixture.transient", (value) => transient.set(!!value)));
  // A lower-priority focused surface must receive these when the workspace
  // has no transient UI. This catches an inert but consuming window binding.
  for (const key of ["escape", "up", "down", "enter", "space"]) {
    cleanup(keymap.bind(key, () => record(`surface-${key}`), { when: "always" }));
  }
  const commands = workspaceCommands({} as LayoutSession, () => ({ panes: [], activePane: 1 } as unknown as WorkspaceSnapshot), (run) => run(), {
    keybindings: bindings, blocked: () => false, projectId: () => "fixture",
    newShell: () => record("new"), closeTab: () => record("close"), closePane: () => {},
    toggleFocus: () => {}, toggleCentered: () => {}, tabContextMenu: () => {}, view: () => {},
    selectNumbered: () => {}, cycle: () => record("cycle"), ownsEscape: transient, escape: () => { transient.set(false); record("dismiss"); },
  });
  cleanup(host.on("app.fixture.strip", () => commands.toggleStrip()));
  return <View><Text testID="key-result">{event()}</Text></View>;
}
mount(Fixture);
