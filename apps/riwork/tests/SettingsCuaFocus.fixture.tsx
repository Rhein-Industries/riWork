import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Input } from "nsh/base";
import { Settings, type SettingsDto } from "riwork/native";
import { CuaSetup, type CuaSetupControl, type CuaState } from "../CuaSetup";
import { AppSettings, type AppSettingsControl } from "../settings/AppSettings";

// Retain the panel across show/hide notifications, as DockArea does. The
// controller reports fixture actions only: Enter must never invoke a driver.
function Fixture() {
  const params = host.params as { ready: boolean; pending?: boolean };
  const visible = signal(false), settings = signal<SettingsDto | null>(null);
  const cuaFocus = signal<CuaSetupControl | null>(null);
  const cua = signal<CuaState>({ pending: params.pending ? "check" : null, error: null, status: {
    installed: true, version: "fixture", running: true, ready: params.ready,
    accessibility: params.ready, screen_recording: params.ready, direct_capture_verified: params.ready,
    message: params.ready ? "Fixture ready" : "Fixture needs access",
    skills: { setup_needed: false, message: "Fixture only", targets: [] },
  } });
  let control: AppSettingsControl | undefined;
  const subscription = Settings.subscribe((event) => settings.set(event.data));
  cleanup(() => subscription.close());
  void Settings.set({ theme: "native", orchestratorMode: "chat" }).then((value) => settings.set(value));
  cleanup(host.on("app.fixture.show", (value) => visible.set(!!value)));
  cleanup(host.on("app.fixture.inspect", () => host.post("app.fixture.state", settings())));
  cleanup(host.on("app.fixture.reveal", () => control?.revealApiKey()));
  cleanup(host.on("app.fixture.cua-ready", (ready) => cua.set({ ...cua(), pending: null, status: { ...cua().status!, ready: !!ready } })));
  cleanup(host.on("app.fixture.settle", () => { setTimeout(() => host.post("app.fixture.settled", {}), 100); }));
  return <View style={{ flex: 1 }}>
    <Input testID="outside" placeholder="Outside Settings" />
    <AppSettings settings={settings} visible={visible} onControl={(value) => { control = value; }}
      computerUseFocus={cuaFocus} computerUse={<CuaSetup visible={visible()} checkOnShow={false} onControl={(value) => { cuaFocus.set(value); return () => cuaFocus.set(null); }} controller={{ state: cua,
        run: async (action) => { host.post("app.fixture.cua-action", action); } }} />} />
    <Text testID="fixture-visible">{String(visible())}</Text>
  </View>;
}
mount(Fixture);
