import { host, mount, signal, View } from "nsh";
import { WindowChrome } from "../WindowChrome";

function Fixture() {
  const state = signal({ mode: "windowed" });
  host.on("window.state", (value) => state.set(value as { mode: string }));
  return <View style={{ flex: 1 }}><WindowChrome state={{ state, fullscreen: () => state().mode === "fullscreen" }} panes={[{id:1,x:0,width:600}]} /></View>;
}
mount(Fixture);
