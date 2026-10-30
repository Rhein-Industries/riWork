/** In-memory flows fixture. No native flow store, chat host, provider, check command,
 * diagram worker, filesystem or network. */
import { mount, host, View } from "nsh";
import type { FlowDto, FlowRunDto, FlowsSnapshot } from "riwork/native";
import { DEFAULT_THEME } from "../appearance";
import { FlowsPanel, type FlowsApi } from "./FlowsPanel";

const PROJECT = "11111111-1111-4111-8111-111111111111";
const PNG = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
const REVIEW: FlowDto = { id: "code-review", path: "/fixture/.ontack/flows/code-review.json", name: "Code review", description: "Plan, build and review a change.", mermaid: "stateDiagram-v2\n  [*] --> s_plan" };
const BROKEN: FlowDto = { id: "broken", path: "/fixture/.ontack/flows/broken.json", name: "broken", description: "", error: "The state \"x\" goes to \"nowhere\", which is no state.", mermaid: "" };

function Fixture() {
  // Plain arrays: the panel calls the API inside its effects, which must not track them.
  const calls: string[] = [], opened: string[] = [], edited: string[] = [];
  let flows: FlowDto[] = [REVIEW, BROKEN], runs: FlowRunDto[] = [];
  const listeners = new Set<(snapshot: FlowsSnapshot) => void>();
  const snapshot = (): FlowsSnapshot => ({ folder: "/fixture/.ontack/flows", flows, runs });
  const publish = () => { for (const listener of listeners) listener(snapshot()); };
  const record = (call: string) => { calls.push(call); };
  const update = (id: string, patch: Partial<FlowRunDto>) => {
    runs = runs.map((run) => run.id === id ? { ...run, ...patch } : run); publish();
    return runs.find((run) => run.id === id)!;
  };
  const api: FlowsApi = {
    subscribe: (project, listener) => {
      record(`subscribe:${project}`); listeners.add(listener);
      void Promise.resolve().then(() => { if (listeners.has(listener)) listener(snapshot()); });
      return { close: () => { listeners.delete(listener); } } as ReturnType<FlowsApi["subscribe"]>;
    },
    diagram: async (source) => { record(`diagram:${source.length}`); return { dataUrl: PNG, width: 2, height: 2 }; },
    create: async (project, name) => {
      record(`create:${name}`);
      const made: FlowDto = { ...REVIEW, id: "deploy", name, path: "/fixture/.ontack/flows/deploy.json" };
      flows = [...flows, made]; publish();
      return { id: made.id, path: made.path };
    },
    start: async (project, flow, chat) => {
      record(`start:${flow}:${chat}`);
      const run: FlowRunDto = { id: "run-1", flowId: flow, flowName: "Code review", projectId: project, chatId: chat, chatTitle: "Fixture chat",
        state: "plan", location: "Plan", status: "active", events: [], history: [{ at: 1_900_000_000, kind: "start", from: "", event: "", to: "plan", evidence: "", detail: "", by: "person" }],
        mermaid: "stateDiagram-v2\n  [*] --> s_plan\n  class s_plan active", startedAt: 1_900_000_000, updatedAt: 1_900_000_000 };
      runs = [run]; publish(); return run;
    },
    approve: async (run, note) => { record(`approve:${run}:${note}`); return update(run, { status: "active", state: "build", location: "Build", pendingEvent: undefined, pendingLocation: undefined }); },
    deny: async (run, note) => { record(`deny:${run}:${note}`); return update(run, { status: "active", pendingEvent: undefined, pendingLocation: undefined }); },
    stop: async (run) => { record(`stop:${run}`); return update(run, { status: "stopped" }); },
  };
  // The agent runs `riwork flow next planned` and the transition needs approval.
  host.on("app.fixture.flows.ask", () => { update("run-1", { status: "waiting", pendingEvent: "planned", pendingLocation: "Build", pendingEvidence: "Plan in PLAN.md" }); });
  host.on("app.fixture.flows.inspect", () => host.post("app.fixture.flows.state", { calls, opened, edited, listeners: listeners.size }));
  return <View style={{ flex: 1 }}>
    <FlowsPanel api={api} projectId={() => PROJECT} theme={() => DEFAULT_THEME}
      chats={() => [{ id: "chat-a", title: "Fixture chat" }]}
      openChat={(id) => { opened.push(id); }} editFile={(path) => { edited.push(path); }} />
  </View>;
}
mount(Fixture);
