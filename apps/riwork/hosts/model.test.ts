import { test, expect } from "nsh/test";
import { connectionText, projectKey, routeStep, sessions, type AgentHost } from "./model";
const host: AgentHost = { id:"identity",connectionId:"route",label:"Build Mac",route:"relay",connection:"backoff",freshness:"stale",receivedAt:1,error:"offline",snapshot:{version:1,hostId:"identity",observedAt:1,capabilities:[],partial:false,total:1,sessions:[{id:"same",kind:"chat",projectId:"project",parentId:null,title:"Build",provider:"codex",state:"working",source:"provider",since:1,subagents:2,requests:[],completion:null,unseen:false}]}};
test("HOSTS-01 stale connection never changes agent execution state", () => {
  expect(connectionText(host)).toContain("last known state");
  expect(sessions(host,"working","").length).toBe(1);
  expect(sessions(host,"attention","").length).toBe(0);
  expect(host.snapshot!.sessions[0].state).toBe("working");
});
test("HOSTS-02 routes use the owning host without local fallback", () => {
  const session=host.snapshot!.sessions[0];
  expect(projectKey(host,session)).toBe("remote:route:project");
  expect(routeStep(host,session)).toEqual({kind:"remoteChat",desktopId:"route",chatId:"same"});
  expect(projectKey(host,{...session,projectId:null})).toBe(null);
  expect(sessions(host,"all","build codex").length).toBe(1);
});
