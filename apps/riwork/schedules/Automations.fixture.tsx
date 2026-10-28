/** In-memory automation fixture. No native schedule calls, engine dispatch,
 * filesystem, provider, credentials, network, timers or application windows. */
import { Text, View, host, mount, signal } from "nsh";
import { Button } from "nsh/base";
import { AutomationsPanel, type AutomationsApi } from "./Automations";
import { defaults, localIso, projectScope, type CreateRequest, type ScheduleData, type ScheduleRow, type UpdateRequest } from "./model";

const PROJECT = "11111111-1111-4111-8111-111111111111", OTHER = "22222222-2222-4222-8222-222222222222";
const ID = "33333333-3333-4333-8333-333333333333", TARGET = "44444444-4444-4444-8444-444444444444";
const TREE = "55555555-5555-4555-8555-555555555555", NOW = 1_900_000_000;
function row(): ScheduleRow {
  return { key: { id: ID, revision: 4, targetId: TARGET, scope: { scope: "app", projectId: null, worktreeId: null } },
    title: "Pinned fixture", prompt: "Inert fixture prompt", destination: "shell", destinationLabel: "App · AI shell · pinned fixture", options: null,
    accountLabel: "Pinned with target", firstAt: NOW + 600, firstIso: localIso(NOW + 600), everyMinutes: null, paused: false, reviewRequired: false,
    nextRun: NOW + 600, nextDisplay: "2030-03-17 18:56:40 +01:00", lastRun: { dueAt: NOW - 60, observedAt: NOW - 30, dueDisplay: "fixture due", observedDisplay: "fixture observed", outcome: "submitted", message: "Inert fake result", createdChatId: "created-fixture-chat" } };
}
function Fixture() {
  const visible = signal(true), project = signal(PROJECT), calls = signal<string[]>([]), opened = signal<string[]>([]);
  let revision = 1, subscribes = 0, closes = 0, hold = false, release: (() => void) | null = null, conflict = false;
  let data: ScheduleData = { schema: 1, now: NOW, rows: [row()], targets: [
    { binding: { scope: { scope: "app", projectId: null, worktreeId: null }, kind: "shell", id: TARGET }, label: "Codex pinned fixture" },
    { binding: { scope: projectScope(PROJECT), kind: "shell", id: TARGET }, label: "Claude explicit project fixture" },
    { binding: { scope: { scope: "workspace", projectId: PROJECT, worktreeId: TREE }, kind: "shell", id: TARGET }, label: "Codex workspace fixture" },
  ], chats: [{ id: "earlier-fixture-chat", projectId: PROJECT, title: "Earlier automation result" }],
    worktrees: [{ id: TREE, projectId: PROJECT, label: "Disposable worktree" }], error: null, runtimeError: null };
  const listeners = new Set<(event: { rev: number; data: ScheduleData }) => void>(), requests: (CreateRequest | UpdateRequest)[] = [];
  const publish = () => { revision++; for (const listener of listeners) listener({ rev: revision, data }); };
  const wait = async () => { if (hold) await new Promise<void>((resolve) => { release = resolve; }); };
  const remember = async (call: string) => { calls.set([...calls(), call]); await wait(); };
  const create = async (request: CreateRequest) => {
    requests.push(request); await remember(request.options ? "createChat" : "create");
    const created: ScheduleRow = { ...row(), key: { id: "66666666-6666-4666-8666-666666666666", revision: 1, targetId: TARGET, scope: request.scope },
      title: request.title, prompt: request.prompt, destination: request.options ? "new_chat" : "shell", options: request.options,
      accountLabel: request.options?.accountId ?? "Pinned with target", everyMinutes: request.everyMinutes, lastRun: null };
    data = { ...data, rows: [...data.rows, created] }; publish(); return created;
  };
  const api: AutomationsApi = {
    subscribe: (listener) => { subscribes++; listeners.add(listener); void Promise.resolve().then(() => { if (listeners.has(listener)) listener({ rev: revision, data }); });
      return { close: () => { if (listeners.delete(listener)) closes++; } }; },
    chatOptions: async (provider) => ({ ...defaults(), provider, accountId: provider === "codex" ? "fake-account" : null }),
    create, createChat: create,
    update: async (request) => { requests.push(request); await remember(`update:${request.key.revision}`); if (conflict) throw new Error("revision_conflict: fixture changed");
      const edited = { ...row(), key: { ...request.key, revision: request.key.revision + 1 }, title: request.title, prompt: request.prompt, everyMinutes: request.everyMinutes };
      data = { ...data, rows: data.rows.map((item) => item.key.id === edited.key.id ? edited : item) }; publish(); return edited; },
    pause: async (key, paused) => { await remember(`pause:${key.revision}:${paused}`); const edited = { ...row(), key: { ...key, revision: key.revision + 1 }, paused };
      data = { ...data, rows: data.rows.map((item) => item.key.id === key.id ? edited : item) }; publish(); return edited; },
    delete: async (key) => { await remember(`delete:${key.revision}`); const previous = data.rows.find((item) => item.key.id === key.id)!;
      data = { ...data, rows: data.rows.filter((item) => item.key.id !== key.id) }; publish(); return previous; },
  };
  host.on("app.fixture.schedules.inspect", () => host.post("app.fixture.schedules.state", { calls: calls(), requests, opened: opened(), subscribes, closes, listeners: listeners.size, visible: visible(), project: project() }));
  host.on("app.fixture.schedules.hold", () => { hold = true; });
  host.on("app.fixture.schedules.release", () => { hold = false; release?.(); release = null; });
  host.on("app.fixture.schedules.conflict", () => { conflict = true; });
  return <View style={{ flex: 1 }}>
    <View style={{ flexDirection: "row", gap: 6 }}>
      <Button testID="fixture-toggle-visible" onPress={() => visible.set(!visible())}><Text>Toggle visible</Text></Button>
      <Button testID="fixture-switch-project" onPress={() => project.set(project() === PROJECT ? OTHER : PROJECT)}><Text>Switch project</Text></Button>
      <Button testID="fixture-runtime-error" onPress={() => { data = { ...data, runtimeError: "Disposable ticker error" }; publish(); }}><Text>Publish runtime error</Text></Button>
    </View>
    <AutomationsPanel api={api} projectId={project} visible={visible} now={() => NOW} openChat={(id) => opened.set([...opened(), id])} />
  </View>;
}
mount(Fixture);
