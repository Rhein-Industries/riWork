import { expect, open, test } from "nsh/test";
import { windowTitle, createTitleUpdater, createDockUpdater } from "../model";

test("P1-MENU-04 first Dock post waits for stored name and unchanged metadata never reposts", () => {
  const posts: {project:string;branch:string|null}[] = [];
  const update = createDockUpdater((info) => posts.push(info));
  update(false, "alpha");
  expect(posts).toEqual([]);
  update(true, "Alpha");
  update(true, "Alpha");
  expect(posts).toEqual([{project:"Alpha",branch:null}]);
  update(true, "Renamed Alpha");
  update(true, "Renamed Alpha");
  expect(posts).toEqual([{project:"Alpha",branch:null},{project:"Renamed Alpha",branch:null}]);
});

test("P1-WIN-05 stored project labels and remote metadata update a title only on change", () => {
  const calls:string[]=[]; const update=createTitleUpdater((title)=>calls.push(title));
  update(windowTitle("Alpha")); update(windowTitle("Alpha"));
  update(windowTitle("Alpha","Studio")); update(windowTitle(null,null,"global"));
  expect(calls).toEqual(["RiWork · Alpha","RiWork · Alpha (on Studio)","RiWork · Global"]);
});

test("P1-PROC-04 fallback notice is exposed as an accessible alert", async () => {
  const notice = "Project directory disappeared; opened the fallback project.";
  const app = await open("../App.tsx", {platform:"macos",width:1000,height:700,host:{params:{
    schema:1,windowId:"w2-notice",kind:"project",projectId:"w2-notice",path:"/fixture/Fallback",notice,
    restore:{layout:{layout:{pane:1},active_pane:1,panels_initialized:true,locked_panes:[],panes:{"1":{tabs:[]}}}},
  },handlers:{"app.restoreReady":null,"window.setDockInfo":null}}});
  try {
    await app.waitFor("id:workspace-notice-message");
    expect(app.shows(`label:${notice}`)).toBe(true);
    expect(app.shows("label:Dismiss")).toBe(true);
  } finally { app.dispose(); }
});

test("P1-WIN-09 occlusion is consumed without host work; fullscreen updates the native island", async () => {
  const app=await open("../App.tsx", {platform:"macos",width:1000,height:700,host:{params:{
    schema:1,windowId:"m5-window-state",kind:"project",projectId:"m5-window-state",path:"/fixture/State",
    restore:{layout:{layout:{pane:1},active_pane:1,main_pane:1,panels_initialized:true,locked_panes:[],panes:{"1":{tabs:[{kind:"shell",shell_id:"state"}],active_tab_key:"shell:state"}}}},
  },handlers:{"app.restoreReady":null,"window.setDockInfo":null}}});
  try {
    await app.waitFor("id:tab:shell:state");
    const calls=app.host.calls(); const posts=app.host.posts();
    await app.host.send("window.state",{key:false,visible:true,occluded:true,mode:"windowed",backingScale:2,frame:{x:0,y:0,width:1000,height:700},appActive:false});
    expect(app.host.calls()).toEqual(calls); expect(app.host.posts()).toEqual(posts);
    await app.host.send("window.state",{mode:"fullscreen"});
    expect(app.shows("id:window-controls-island")).toBe(false);
    await app.host.send("window.state",{mode:"windowed"});
    expect(app.shows("id:window-controls-island")).toBe(true);
  } finally { app.dispose(); }
});
