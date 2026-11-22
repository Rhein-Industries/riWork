import { View, cleanup, host, mount, signal } from "nsh";
import { DEFAULT_THEME } from "../theme";
import { HostManagementPanel, type HostManagementApi } from "./Management";

function Fixture() {
  const visible = signal(true);
  const params = host.params as { stopped?: boolean; noPeers?: boolean; sshError?: boolean };
  const routes = [{ id: "build", label: "Build server", target: "bob@build-mac" }];
  const report = (name: string, value: unknown = null) => host.post("app.fixture.management", { name, value });
  const api: HostManagementApi = {
    sshRoutes: async () => routes.slice(),
    discoverTailscaleMacs: async () => {
      report("discover");
      if (params.stopped) throw new Error("Start Tailscale and refresh Hosts.");
      return params.noPeers ? [] : [
        { id: "old", label: "Build", target: "100.64.0.1", dnsName: "build-mac.example.ts.net.", online: true },
        { id: "studio", label: "Studio", target: "100.64.0.2", dnsName: "studio.example.ts.net", online: true },
        { id: "laptop", label: "Laptop", target: "100.64.0.3", dnsName: null, online: false },
      ];
    },
    addSsh: async request => {
      report("connect", request);
      if (params.sshError) throw new Error("SSH login was refused.");
      routes.push({ id: "new", label: request.label, target: request.target });
    },
  };
  cleanup(host.on("app.fixture.visibility", state => visible.set(state === true)));
  return <View style={{ flex: 1 }}><HostManagementPanel visible={visible} api={api} theme={() => DEFAULT_THEME} /></View>;
}
mount(Fixture);
