import { Text, View, cleanup, mount, signal } from "nsh";
import { Button } from "nsh/base";
import { Appearance, Dashboard, Layouts, Sessions, Settings, Store, Tabs, type Subscription } from "riwork/native";

// Import every module owned by P1a and the P1c Layouts module from the exact
// registry used by the desktop. No JavaScript replacement module is registered.
const p1Modules = [Appearance, Dashboard, Layouts, Sessions, Settings, Store, Tabs];

function App() {
  const count = signal<number | null>(null);
  const error = signal("");
  let subscription: Subscription | undefined;
  let disposed = false;
  cleanup(() => { disposed = true; subscription?.close(); });
  void Store.snapshot().then(() => {
    if (!disposed) subscription = Store.subscribe(event => count.set(event.data.projectFolders.length));
  }).catch(reason => error.set(String(reason)));
  return <View style={{ padding: 12, gap: 8 }}>
    <Text>{`Modules: ${p1Modules.length}`}</Text>
    <Text>{count() === null ? "Loading" : `Folders: ${count()}`}</Text>
    <Button onPress={() => { void Store.createFolder("From the bridge").catch(reason => error.set(String(reason))); }}><Text>Add folder</Text></Button>
    <Text>{error()}</Text>
  </View>;
}
mount(App);
