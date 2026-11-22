import { expect, open, test, type App } from "nsh/test";
const options = { platform: "macos" as const, width: 1000, height: 1200, host: { handlers: { "app.fixture.management": null } } };
function calls(app: App, name: string) {
  return app.host.posts().filter(p => p.name === "app.fixture.management")
    .map(p => p.data as { name: string; value: unknown }).filter(p => p.name === name).map(p => p.value);
}
test("TAILMACS shows owned Mac availability and pre-fills an explicitly confirmed SSH connection", async () => {
  const app = await open("./Management.fixture.tsx", options);
  try {
    await app.waitFor("id:tailscale-setup-studio");
    expect(app.shows("id:tailscale-peer-old")).toBe(false);
    expect(app.shows("id:managed-host-build")).toBe(true);
    expect(app.texts()).toContain("Online on Tailscale");
    expect(app.texts()).toContain("Offline on Tailscale");
    await app.press("id:tailscale-setup-studio");
    expect(app.value("id:host-target")).toBe("100.64.0.2"); expect(app.value("id:host-label")).toBe("Studio");
    expect(calls(app, "connect")).toHaveLength(0);
    await app.clear("id:host-target"); await app.type("id:host-target", "alice@100.64.0.2");
    await app.press("id:host-connect"); await app.waitFor("id:managed-host-new");
    expect(calls(app, "connect")).toEqual([{ target: "alice@100.64.0.2", label: "Studio",
      remoteBin: "/Applications/Somtus.app/Contents/MacOS/somtus-remote", somtus: "/Applications/Somtus.app/Contents/MacOS/somtus" }]);
    expect(app.shows("id:tailscale-peer-studio")).toBe(false);
  } finally { app.dispose(); }
});
test("TAILMACS refreshes on opening the tab and on manual refresh", async () => {
  const app = await open("./Management.fixture.tsx", options);
  try {
    await app.waitFor("id:tailscale-setup-studio");
    await app.press("id:hosts-refresh");
    await app.host.send("app.fixture.visibility", false); await app.host.send("app.fixture.visibility", true);
    expect(calls(app, "discover")).toHaveLength(3);
    expect(calls(app, "connect")).toHaveLength(0);
  } finally { app.dispose(); }
});
test("TAILMACS empty and stopped tailnets leave manual SSH setup available", async () => {
  for (const [params, message] of [
    [{ noPeers: true }, "No other Macs to add on this Tailscale account."],
    [{ stopped: true }, "Start Tailscale and refresh Hosts."],
  ] as const) {
    const app = await open("./Management.fixture.tsx", { ...options, host: { ...options.host, params } });
    try {
      await app.waitFor(`text:${message}`); expect(app.shows("id:managed-host-build")).toBe(true);
      await app.press("id:hosts-add"); expect(app.value("id:host-target")).toBe("");
      expect(app.enabled("id:host-connect")).toBe(false);
    } finally { app.dispose(); }
  }
});
test("TAILMACS rejected SSH login leaves the form open for correction", async () => {
  const app = await open("./Management.fixture.tsx", { ...options, host: { ...options.host, params: { sshError: true } } });
  try {
    await app.waitFor("id:tailscale-setup-studio"); await app.press("id:tailscale-setup-studio");
    await app.press("id:host-connect"); await app.waitFor("text:SSH login was refused.");
    expect(app.value("id:host-target")).toBe("100.64.0.2"); expect(app.shows("id:managed-host-new")).toBe(false);
  } finally { app.dispose(); }
});
