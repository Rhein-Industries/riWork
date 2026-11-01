/** RiWork on iPhone and iPad: the shared screens over the Swift host (`ios/RiWorkRemote/NSHHostAPI.swift`).
 * The window's width picks the shell, never the device: narrower than 600 points the phone's navigation
 * stack (`PhoneShell`), wider the iPad's columns (`PadShell`); both draw the same routes. */
import { View, computed, mount, windowSize } from "nsh";
import { createMobileApp } from "./app_state";
import { PadShell } from "./shell/PadShell";
import { PhoneShell } from "./shell/PhoneShell";
import { kit } from "./ui";

/** Where the iPad's columns start (`docs/nsh-ios-migration.md`). */
export const PAD_WIDTH = 600;

function App() {
  const app = createMobileApp();
  const ui = kit(app);
  const wide = computed(() => windowSize().width >= PAD_WIDTH);
  return <View style={{ flex: 1, backgroundColor: ui.palette().bg }}>
    {wide() ? <PadShell app={app} ui={ui} /> : <PhoneShell app={app} ui={ui} />}
  </View>;
}

mount(App);
