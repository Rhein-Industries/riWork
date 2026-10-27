import { host, mount, signal, View } from "nsh";
import { WindowChrome } from "../WindowChrome";

function Fixture() {
  const params = host.params as { theme: string; controlsIsland: boolean };
  const state = signal({ mode: "windowed" });
  host.on("window.state", (value) => state.set(value as { mode: string }));
  const palette = () => ({ theme: params.theme, controlsIsland: params.controlsIsland, panelActive: "#14212a", divider: "#253c45", background: "#090d14", panel: "#101720", text: "#d3e1e6" });
  return <View style={{ flex: 1 }}><WindowChrome state={{ palette, state, fullscreen: () => state().mode === "fullscreen" }} panes={[{id:1,x:0,width:600}]} /></View>;
}
mount(Fixture);
