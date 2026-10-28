import { Text, View, cleanup, host, mount, signal } from "nsh";
import { Input } from "nsh/base";
import { Settings, type SettingsDto } from "riwork/native";
import { CuaSetup, type CuaSetupControl, type CuaState } from "../CuaSetup";
import { AppSettings, type AppSettingsControl } from "../settings/AppSettings";
import { RemoteSettings, createRemoteController, type RemoteApi } from "../remote/Remote";
import { EMPTY_REMOTE, EMPTY_WANTS } from "../remote/model";

// Retain the panel across show/hide notifications, as DockArea does. The
// controller reports fixture actions only: Enter must never invoke a driver.
function Fixture() {
  const params = host.params as { ready: boolean; pending?: boolean };
  const visible = signal(false), settings = signal<SettingsDto | null>(null);
  const cuaFocus = signal<CuaSetupControl | null>(null);
  // Closed in-memory adapter: even activating a Remote control cannot pair,
  // discover a host, read credentials, invoke a CLI or use the network.
  const remoteApi: RemoteApi = {
    subscribe: (_wants, listener) => {
      listener({ data: { ...EMPTY_REMOTE, hosts: [{ id: "focus-fixture", label: "Fixture Mac", link: "online", status: "Online" }] } });
      return { close() {} };
    },
    defaults: async () => ({ name: "Fixture Mac", relay: "", routes: "" }),
    addHost: async () => {}, removeHost: async () => {},
    pairDesktop: async () => ({ link: "riwork://pair?INERT_FOCUS_FIXTURE", masked: "fixture" }),
    createProject: async () => ({ key: "remote:focus-fixture:project", name: "Fixture" }),
    dismissFailure: async () => {},
  };
  const remote = createRemoteController({ api: remoteApi, wants: () => ({ ...EMPTY_WANTS, settings: true }), copyText: () => {} });
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
      remote={<RemoteSettings controller={remote} />}
      computerUseFocus={cuaFocus} computerUse={<CuaSetup visible={visible()} checkOnShow={false} onControl={(value) => { cuaFocus.set(value); return () => cuaFocus.set(null); }} controller={{ state: cua,
        run: async (action) => { host.post("app.fixture.cua-action", action); } }} />} />
    <Text testID="fixture-visible">{String(visible())}</Text>
  </View>;
}
mount(Fixture);
