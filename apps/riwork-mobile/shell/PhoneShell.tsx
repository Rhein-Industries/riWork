/** Narrower than 600 points (iPhone, iPad Slide Over): one page at a time on the native navigation stack,
 * with swipe back. Hosts → a host's dashboard → its projects → a project; forms are page sheets. */
import { View } from "nsh";
import { NavStack } from "nsh/base";
import type { MobileApp, Route } from "../app_state";
import { HostDashboardPage } from "../pages/HostDashboard";
import { HostsPage } from "../pages/Hosts";
import { ProjectPage } from "../pages/Project";
import { ProjectsPage } from "../pages/Projects";
import type { Kit } from "../ui";

export function PhoneShell(props: { app: MobileApp; ui: Kit }) {
  const { app, ui } = props;
  const onTop = (route: Route) => () => app.top() === route;
  return <View style={{ flex: 1, backgroundColor: ui.palette().bg }}>
    <NavStack testID="phone-shell" state={app.nav} style={{ flex: 1 }}>
      {(route) => route.page === "hosts" ? <HostsPage app={app} ui={ui} layout="phone" onTop={onTop(route)} />
        : route.page === "dashboard" ? <HostDashboardPage app={app} ui={ui} layout="phone" host={route.host} onTop={onTop(route)} />
        : route.page === "projects" ? <ProjectsPage app={app} ui={ui} layout="phone" host={route.host} onTop={onTop(route)} />
        : <ProjectPage app={app} ui={ui} layout="phone" route={route} onTop={onTop(route)} />}
    </NavStack>
  </View>;
}
