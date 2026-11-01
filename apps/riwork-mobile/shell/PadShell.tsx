/** 600 points and wider (iPad): the hosts on their own, then a host's projects in a column beside its
 * workspace, which is the host's dashboard or the project chosen in the column. The routes are the phone's
 * (`app_state.ts`), so a window resized across 600 points keeps where the person is. It needs none of the
 * desktop's nsh-base patches (the prebuilt framework has none). */
import { View, computed, safeAreaInsets } from "nsh";
import type { MobileApp, ProjectRoute } from "../app_state";
import { HostDashboardPage } from "../pages/HostDashboard";
import { HostsPage } from "../pages/Hosts";
import { ProjectPage } from "../pages/Project";
import { ProjectsPage } from "../pages/Projects";
import type { Kit } from "../ui";

/** The projects column, as wide as a phone's page. */
const COLUMN = 340;

export function PadShell(props: { app: MobileApp; ui: Kit }) {
  const { app, ui } = props;
  const insets = safeAreaInsets();
  const host = computed(() => { const top = app.top(); return top.page === "hosts" ? null : top.host; });
  const project = computed((): ProjectRoute | null => { const top = app.top(); return top.page === "project" ? top : null; });
  return <View testID="pad-shell" style={{ flex: 1, backgroundColor: ui.palette().bg, paddingTop: insets().top, paddingLeft: insets().left, paddingRight: insets().right }}>
    {host() == null ? <HostsPage app={app} ui={ui} layout="pad" onTop={() => app.top().page === "hosts"} />
      : <View style={{ flex: 1, flexDirection: "row" }}>
        {[host()!].map((hostId) => <View style={{ width: COLUMN, borderRightWidth: 0.5, borderColor: ui.palette().divider }}>
          <ProjectsPage app={app} ui={ui} layout="pad" host={hostId} onTop={() => host() === hostId}
            keys={() => project() == null} current={() => project()?.projectId ?? null} onBack={() => app.nav.setRoutes([app.nav.routes()[0]])} />
        </View>)}
        {project() ? [project()!].map((route) => <ProjectPage app={app} ui={ui} layout="pad" route={route} onTop={() => project() === route} />)
          : [host()!].map((hostId) => <HostDashboardPage app={app} ui={ui} layout="pad" host={hostId} onTop={() => host() === hostId && project() == null} />)}
      </View>}
  </View>;
}
