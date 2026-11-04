import { Text, View, cleanup, host, mount, signal } from "nsh";
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
  workspaceCommands({} as LayoutSession, () => ({ panes: [], activePane: 1 } as unknown as WorkspaceSnapshot), (run) => run(), {
    keybindings: bindings, blocked: () => false, projectId: () => "fixture",
    newShell: () => record("new"), closeTab: () => record("close"), closePane: () => {},
    toggleFocus: () => {}, toggleCentered: () => {}, tabContextMenu: () => {}, view: () => {},
    selectNumbered: () => {}, cycle: () => record("cycle"), escape: () => {},
  });
  return <View><Text testID="key-result">{event()}</Text></View>;
}
mount(Fixture);
