import { expect, open, test, type App } from "nsh/test";
import type { HostInfo } from "../../riwork/client/ports";

const peer = "gateway:build-box";
const hosts: HostInfo[] = [
  { id: "gateway", name: "Gateway Mac", address: "gateway-mac", selected: false,
    link: { state: "connected", label: "Connected" }, features: [] },
  { id: peer, name: "Build box", gateway: { id: "gateway", name: "Gateway Mac" }, selected: true,
    link: { state: "connected", label: "Connected" }, features: [] },
  { id: "gateway:studio", name: "Studio", gateway: { id: "gateway", name: "Gateway Mac" }, selected: false,
    link: { state: "offline", label: "Not connected" }, features: [] },
];
const snapshot = { schema: 2, revision: 1, nativePresented: false, libraryUnavailable: false, loading: false,
  details: {}, keyboard: { hardware: false, clicks: false } };

async function fixture(layout: "phone" | "pad") {
  const app = await open("./App.tsx", { platform: "ios", width: layout === "phone" ? 390 : 1024, height: 900,
    host: { params: { layout }, handlers: {
      "mobile.snapshot": snapshot, "mobile.action": snapshot, "relay.hosts": { hosts: layout === "phone" ? hosts.slice(0, 2) : hosts },
      "relay.select": {}, "relay.disconnect": {}, "nav.present": {},
    } } });
  await app.waitFor(`id:host:${peer}`);
  return app;
}

function calls(app: App, name: string): unknown[] {
  return app.host.calls().filter((call) => call.name === name).map((call) => call.data);
}

test("phone gateway rows show their route and manage the gateway pairing while opening the peer", async () => {
  const app = await fixture("phone");
  try {
    expect(app.texts()).toContain("via Gateway Mac");
    expect(app.texts()).toContain("gateway-mac");
    await app.press(`id:host:${peer}`);
    expect(calls(app, "relay.select")).toEqual([{ host: peer }]);
    expect(calls(app, "mobile.action").slice(-1)[0]).toEqual({ type: "navigate", page: "dashboard", hostID: peer });
    await app.press(`id:page:hosts >> label:Manage Gateway Mac connection`);
    expect(calls(app, "nav.present")).toEqual([{ name: "hostDetails", host: "gateway" }]);
    await app.press("label:Manage Gateway Mac");
    expect(calls(app, "nav.present")).toEqual([
      { name: "hostDetails", host: "gateway" }, { name: "hostDetails", host: "gateway" },
    ]);
  } finally { app.dispose(); }
});

test("iPad gateway menu manages the pairing but connects and disconnects only the selected peer", async () => {
  const app = await fixture("pad");
  try {
    await app.press(`id:host:${peer}`);
    app.onDialog(() => "Gateway details…");
    await app.press("id:host-switcher");
    const menu = app.dialogs()[0];
    expect(menu.message).toBe("via Gateway Mac · Connected");
    expect(menu.buttons).toContain("Studio · via Gateway Mac");
    expect(menu.buttons).toContain("Gateway details…");
    expect(menu.buttons).not.toContain("Desktop details…");
    expect(calls(app, "nav.present")).toEqual([{ name: "hostDetails", host: "gateway" }]);
    app.onDialog(() => "Disconnect");
    await app.press("id:host-switcher");
    expect(calls(app, "relay.disconnect")).toEqual([{ host: peer }]);
    await app.host.send("relay.hosts", { hosts: hosts.map((info) => info.id === peer
      ? { ...info, link: { state: "offline", label: "Not connected" } } : info) });
    app.onDialog(() => "Connect");
    await app.press("id:host-switcher");
    expect(calls(app, "relay.select")).toEqual([{ host: peer }, { host: peer }]);
  } finally { app.dispose(); }
});
