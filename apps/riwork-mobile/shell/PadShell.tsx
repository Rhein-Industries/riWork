/** 600 points and wider (iPad): the Mac's RiWork window in touch density. A host's window is [sidebar | main]
 * under one top row, with a status bar along the bottom (`chrome.tsx`): the sidebar is the Mac's Projects
 * panel (`ProjectsPage`) under the sidebar toggle, Dashboard and the host switcher; the main area is the
 * project chosen there, under its tab strip, or the host's dashboard. From `DOCKED_WIDTH` the sidebar stands
 * beside the main area, open by default; narrower it opens over it and closes once a project or the
 * dashboard is chosen. The Hosts page (All desktops) has the window to itself. The routes are the phone's
 * (`app_state.ts`), so a window resized across 600 points keeps where the person is. It needs none of the
 * desktop's nsh-base patches (the prebuilt framework has none). */
import { View, computed, effect, safeAreaInsets, untracked, windowSize } from "nsh";
import type { MobileApp, ProjectRoute } from "../app_state";
import { HostDashboardPage } from "../pages/HostDashboard";
import { HostsPage } from "../pages/Hosts";
import { ProjectPage } from "../pages/Project";
import { ProjectsPage } from "../pages/Projects";
import type { Kit } from "../ui";
import { DOCKED_WIDTH, HostSwitcher, SIDEBAR_WIDTH, StatusBar, TopRow, createPadChrome } from "./chrome";

export function PadShell(props: { app: MobileApp; ui: Kit }) {
  const { app, ui } = props;
  const insets = safeAreaInsets();
  const host = computed(() => { const top = app.top(); return top.page === "hosts" ? null : top.host; });
  const project = computed((): ProjectRoute | null => { const top = app.top(); return top.page === "project" ? top : null; });
  const wide = computed(() => windowSize().width - insets().left - insets().right >= DOCKED_WIDTH);
  const chrome = createPadChrome(app, host, wide);
  const shown = () => chrome.docked() || chrome.overlay();

  // At launch, the one paired desktop (or the one selected) opens at once; All desktops later is the person's choice.
  effect(() => {
    const hosts = app.hosts();
    if (app.launched.current || !hosts.length) return;
    app.launched.current = true;
    const selected = hosts.filter((info) => info.selected);
    const only = hosts.length === 1 ? hosts[0] : selected.length === 1 ? selected[0] : null;
    if (only) untracked(() => app.openHost(only.id));
  });

  function Sidebar(sidebar: { hostId: string }) {
    return <>
      <TopRow ui={ui} lead={true} testID="sidebar-top">
        <View style={{ flexDirection: "row", alignItems: "center", flex: 1, minWidth: 0, paddingHorizontal: ui.pt(4), gap: ui.pt(2) }}>
          <ui.Tool testID="sidebar-toggle" icon="layout" label="Hide the sidebar" action={chrome.toggle} />
          <ui.Tool testID="dashboard" icon="globe" label="Dashboard" selected={project() == null} disabled={app.blocked()}
            action={() => { chrome.close(); app.openDashboard(sidebar.hostId); }} />
          <HostSwitcher app={app} ui={ui} chrome={chrome} host={sidebar.hostId} />
        </View>
      </TopRow>
      <ProjectsPage app={app} ui={ui} layout="pad" host={sidebar.hostId} onTop={() => host() === sidebar.hostId && shown()}
        keys={() => project() == null || chrome.overlay()} current={() => project()?.projectId ?? null} onOpen={chrome.close} />
    </>;
  }

  // The sidebar comes after the main area, so that open over it, it draws above it (and its shade); the row
  // runs from its end, so that standing beside it, it is still at the leading edge. Hidden, it stays made
  // (`display: none`): its search, folders and place are kept.
  return <View testID="pad-shell" style={{ flex: 1, backgroundColor: ui.palette().bg, paddingTop: insets().top, paddingLeft: insets().left, paddingRight: insets().right }}>
    {host() == null ? <HostsPage app={app} ui={ui} layout="pad" onTop={() => app.top().page === "hosts"} />
      : [host()!].map((hostId) => <>
        <View style={{ flex: 1, minHeight: 0, flexDirection: "row-reverse" }}>
          <View style={{ flex: 1, minWidth: 0 }}>
            {project() ? [project()!].map((route) => <ProjectPage app={app} ui={ui} layout="pad" route={route} pad={chrome} onTop={() => project() === route} />)
              : <HostDashboardPage app={app} ui={ui} layout="pad" host={hostId} pad={chrome} onTop={() => host() === hostId && project() == null} />}
          </View>
          {chrome.overlay() && <View testID="sidebar-shade" role="button" accessibilityLabel="Close the sidebar" onPress={chrome.close}
            style={{ position: "absolute", left: 0, top: 0, right: 0, bottom: 0, zIndex: 2, backgroundColor: ui.dark() ? "rgba(0,0,0,0.5)" : "rgba(0,0,0,0.2)" }} />}
          <View testID="sidebar" style={{ display: shown() ? "flex" : "none", position: chrome.overlay() ? "absolute" : "relative", left: 0, top: 0, bottom: 0,
            zIndex: chrome.overlay() ? 3 : 0, width: SIDEBAR_WIDTH, flexShrink: 0, backgroundColor: ui.palette().panel, borderRightWidth: 1, borderColor: ui.palette().divider,
            shadowColor: "#000000", shadowOpacity: chrome.overlay() ? 0.3 : 0, shadowRadius: 16, shadowOffset: { width: 4, height: 0 } }}>
            <Sidebar hostId={hostId} />
          </View>
        </View>
        <StatusBar app={app} ui={ui} chrome={chrome} host={hostId} project={project} />
      </>)}
  </View>;
}
