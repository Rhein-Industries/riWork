import { expect, open, test } from "nsh/test";
import { options, settings } from "./appearance-options";
import { CONTROL_WIDTH, CONTROL_HEIGHT, CONTROL_INSET, DRAG_HANDLE_WIDTH, DRAG_ROOM, topPaneFrames, dragRegion } from "../WindowChrome";

test("P1-STAT-01: rendered status text scales and its right side scrolls at narrow widths", async () => {
  const app = await open("./Appearance.fixture.tsx", options("status-rendered-metrics"));
  try {
    await app.waitFor("text:0 LIVE");
    expect(app.spanStyle("text:0 LIVE").fontSize).toBe(9);
    await settings(app, { theme: "native" });
    await app.waitFor("text:0 live");
    expect(Math.round(app.spanStyle("text:0 live").fontSize * 100) / 100).toBe(10.64);
    await app.setViewport(420, 500);
    await app.scroll("id:status-right", 1000);
    await app.press("id:status-item-layout");
    expect(app.texts()).toContain("Default layout");
  } finally { app.dispose(); }
});

test("P1-WIN-06 chrome dimensions and top-pane drag regions match P-SHELL", () => {
  expect([CONTROL_WIDTH,CONTROL_HEIGHT,CONTROL_INSET,DRAG_HANDLE_WIDTH,DRAG_ROOM]).toEqual([78,28,92,12,18]);
  expect(topPaneFrames({type:"split",axis:"horizontal",sizes:[300,null],children:[
    {type:"split",axis:"vertical",sizes:[200,null],children:[{type:"tabs",id:1,panels:[]},{type:"tabs",id:2,panels:[]}]},
    {type:"tabs",id:3,panels:[]},
  ]},{width:900,height:600})).toEqual([{id:1,x:0,width:300},{id:3,x:305,width:595}]);
});


test("P1-WIN-06 empty strip extends after measured tabs without covering them", () => {
  expect(dragRegion({id:1,x:0,width:600,tabsEnd:250})).toEqual({left:250,width:350});
  expect(dragRegion({id:2,x:605,width:400,tabsEnd:390})).toEqual({left:975,width:30});
  expect(dragRegion({id:1,x:0,width:600})).toEqual({left:570,width:30});
});
