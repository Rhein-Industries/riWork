import { View, host, mount } from "nsh";
import { createNavStack, NavStack } from "nsh/base";
import { createMobileApp } from "../app_state";
import { HostsPage } from "../pages/Hosts";
import { createPadChrome, HostSwitcher } from "../shell/chrome";
import { kit, type Layout } from "../ui";

function Fixture() {
  const app = createMobileApp(), ui = kit(app);
  const layout = (host.params.layout ?? "phone") as Layout;
  const selected = () => { const route = app.top(); return route.page === "hosts" ? "gateway" : route.host; };
  const chrome = createPadChrome(app, selected, () => true);
  const stack = createNavStack(["hosts"]);
  return <View style={{ flex: 1 }}>
    {layout === "pad" && <View style={{ height: ui.bar() }}><HostSwitcher app={app} ui={ui} chrome={chrome} host={selected()} /></View>}
    {layout === "phone"
      ? <NavStack state={stack} style={{ flex: 1 }}>{() => <HostsPage app={app} ui={ui} layout="phone" onTop={() => true} />}</NavStack>
      : <HostsPage app={app} ui={ui} layout="pad" onTop={() => true} />}
  </View>;
}

mount(Fixture);
