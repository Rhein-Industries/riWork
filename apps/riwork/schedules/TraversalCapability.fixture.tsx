/** Minimal reproducer for PANEL-113's pinned NSH API blocker. No native API,
 * store, provider, socket, clipboard, GUI, timer or application action. */
import { Text, View, host, mount, type YNode } from "nsh";
import { Button, Input, Radio } from "nsh/base";

function Fixture() {
  const controls = new Map<string, YNode>();
  host.on("app.fixture.schedules.focus", (name) => controls.get(String(name))?.focus());
  return <View style={{ flex: 1, padding: 20, gap: 10 }}>
    <Input testID="capability-input" placeholder="Focus starts here" />
    <Button testID="capability-button" ref={(node) => controls.set("button", node)}><Text>Action</Text></Button>
    <Radio testID="capability-radio" ref={(node) => controls.set("radio", node)} checked={false}><Text>Choice</Text></Radio>
    <View testID="capability-view" ref={(node) => controls.set("view", node)} focusable={true}><Text>Panel root</Text></View>
    <Input testID="capability-other-input" ref={(node) => controls.set("other-input", node)} placeholder="Input positive control" />
  </View>;
}
mount(Fixture);
