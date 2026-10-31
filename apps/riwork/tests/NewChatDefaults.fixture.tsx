/** A New chat tab on the real Settings module in the runner's private home: it saves its
 * choices as defaults, and a reopened one starts from what Settings then holds. */
import { View, host, mount, signal } from "nsh";
import { Settings, type NewChatDefaultsDto } from "riwork/native";
import { DEFAULT_THEME } from "../appearance";
import { NewChat } from "../NewChat";

function Fixture() {
  // undefined: reading Settings; the tab is made again from each reading.
  const defaults = signal<NewChatDefaultsDto | null | undefined>(undefined);
  const read = () => void Settings.get().then((settings) => defaults.set(settings.newChatDefaults ?? null));
  read();
  host.on("app.fixture.reopen", () => { defaults.set(undefined); read(); });
  return <View style={{ flex: 1 }}>{defaults() !== undefined && <NewChat theme={() => DEFAULT_THEME} projectId="fixture" cwd="/tmp"
    locations={[{ label: "Project root", cwd: "/tmp" }]} defaults={defaults()} onCreated={() => {}} onCancel={() => {}} onError={() => {}}
    modelCatalog={async () => ({ supported: [], configured: [] })} />}</View>;
}
mount(Fixture);
