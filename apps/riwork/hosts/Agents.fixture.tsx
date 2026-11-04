import { View, host, mount, setColorScheme, signal } from "nsh";
import { DEFAULT_THEME } from "../appearance";
import { Agents } from "./Agents";
import type { HostSnapshot } from "./model";
function Fixture() {
  setColorScheme("light");
  const visible = signal(true);
  const initial: HostSnapshot = {version:1,error:null,hosts:[{id:"host-a",connectionId:"route-a",label:"Build machine",route:"relay",connection:"online",freshness:"current",receivedAt:1,error:null,snapshot:{version:1,hostId:"host-a",observedAt:1,capabilities:["chats"],total:2,partial:false,sessions:[
    {id:"same",kind:"chat",projectId:"project",parentId:null,title:"Compile project",provider:"codex",state:"working",source:"provider",since:1,subagents:2,requests:[],completion:null,unseen:false},
    {id:"question",kind:"chat",projectId:"project",parentId:null,title:"Review changes",provider:"claude",state:"waiting",source:"provider",since:1,subagents:null,requests:["permission"],completion:null,unseen:false},
  ]}}]};
  return <View style={{flex:1,paddingTop:34,backgroundColor:DEFAULT_THEME.palette.bg}}><Agents theme={() => DEFAULT_THEME} visible={visible} onOpen={(hostInfo,session) => host.post("app.fixture.open",{host:hostInfo.id,connection:hostInfo.connectionId,session:session.id})}
    onNotice={message => host.post("app.fixture.notice",{message})} subscribe={listener => {
      listener({rev:1,data:initial});
      const stop=host.on("app.fixture.snapshot",event => listener(event as unknown as {rev:number;data:HostSnapshot}));
      return {close:stop};
    }} /></View>;
}
mount(Fixture);
