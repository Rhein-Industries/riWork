/** A disposable in-memory paired-Mac fixture. No native Remote import, CLI,
 * network, test sockets, real clipboard, credentials or local filesystem calls. */
import { Text, View, host, mount, signal, type YNode } from "nsh";
import { Button, Input } from "nsh/base";
import { RemoteProjects, RemoteProjectPanel, RemoteSettings, RemoteStrip, RemoteUnavailable, createRemoteController, type RemoteApi } from "./Remote";
import { EMPTY_REMOTE, EMPTY_WANTS, shellOpenRefusal, type RemoteLists, type RemoteSnapshot } from "./model";

const KEY = "remote:disposable-mac:disposable-project";
const initialProject: RemoteLists = {
  key: KEY, host: "disposable-mac", project: "disposable-project", hostLabel: "Disposable Mac", projectName: "Remote fixture", link: "online", unpaired: false,
  worktrees: [{ id: "fixture-tree", branch: "main", path: "/FAKE/remote-only/project", primary: true }], worktreesLoading: false, worktreesError: null,
  tasks: [{ id: "fixture-task", title: "Remote task", details: "No mutation on this Mac", status: "todo", worktreeId: "fixture-tree" }], tasksLoading: false, tasksError: null,
  shells: [{ id: "fixture-shell", projectId: "disposable-project", worktreeId: "fixture-tree", orchestrator: false, chat: false, harness: "codex", alive: true, cwd: "/FAKE/remote-only/project", display: "Codex fixture-shell" },
    { id: "fixture-chat", projectId: "disposable-project", worktreeId: null, orchestrator: true, chat: true, harness: "claude", alive: true, cwd: "/FAKE", display: "Project orchestrator" }],
  shellsLoading: false, shellsError: null, creating: false, failure: null,
};
function Fixture() {
  let state: RemoteSnapshot = { ...EMPTY_REMOTE, selected: KEY,
    hosts: [{ id: "disposable-mac", label: "Disposable Mac", link: "online", status: "Online" }],
    folders: [{ host: "disposable-mac", label: "Disposable Mac", link: "online", count: 1, creating: false, failure: null, note: null,
      projects: [{ key: KEY, id: "disposable-project", name: "Remote fixture", dimmed: false, stats: "1 trees · 0/1 tasks · 1 live" }] }], projects: [initialProject] };
  const listeners = new Set<(event: { data: RemoteSnapshot }) => void>();
  const publish = () => { for (const listener of listeners) listener({ data: state }); };
  const notices = signal<string[]>([]), actions = signal<string[]>([]), copied = signal(0), calls = signal<string[]>([]);
  const query = signal(""), selected = signal<string | null>(KEY), collapsed = signal<ReadonlySet<string>>(new Set());
  const panel = signal<"worktrees" | "tasks" | "shells">("shells"), ended = signal(false);
  const unrelated = signal(0), elsewhere = signal("");
  let subscribed = 0, closed = 0, unfolded = false;
  // The rest of the window: an inline form must leave it in place and usable.
  let settingsBox: YNode | undefined, projectsBox: YNode | undefined;
  let hold = false, resume: (() => void) | null = null;
  const wait = async () => { if (hold) await new Promise<void>((resolve) => { resume = resolve; }); };
  const api: RemoteApi = {
    subscribe: (_wants, listener) => { subscribed++; listeners.add(listener); void Promise.resolve().then(() => { if (listeners.has(listener)) listener({ data: state }); }); return { close: () => { closed++; listeners.delete(listener); } }; },
    defaults: async () => ({ name: "Remembered name", relay: "wss://invalid.example/v1/ws", routes: "/FAKE/DISPOSABLE/relay-routes.json" }),
    addHost: async (_link, label) => { calls.set([...calls(), `add:${label}`]); await wait(); },
    removeHost: async (host) => { calls.set([...calls(), `remove:${host}`]); await wait(); state = { ...state, hosts: [], folders: [], projects: state.projects.map((p) => ({ ...p, unpaired: true })) }; publish(); },
    pairDesktop: async (_request) => { calls.set([...calls(), "pair"]); await wait(); return { link: "riwork://pair?DISPOSABLE_FAKE_NOT_A_CREDENTIAL", masked: "" }; },
    createProject: async (_host, name) => { calls.set([...calls(), `project:${name}`]); await wait(); return { key: "remote:disposable-mac:created-fake-project", name }; },
    dismissFailure: async () => { calls.set([...calls(), "dismiss"]); },
  };
  const c = createRemoteController({ api, wants: () => { unrelated(); return { ...EMPTY_WANTS, settings: true, folders: true, projectKey: KEY, shells: true }; },
    notice: (message) => notices.set([...notices(), message]), copyText: async () => { copied.set(copied() + 1); },
    beforePrompt: () => { unfolded = true; }, afterPrompt: () => { unfolded = false; } });
  host.on("app.fixture.remote.inspect", () => host.post("app.fixture.remote.state", {
    pending: c.pending(), confirmation: c.confirmation(), prompt: c.prompt()?.kind ?? null,
    calls: calls(), notices: notices(), actions: actions(), copied: copied(), selected: selected(),
    name: c.name(), relay: c.relay(), routes: c.routes(), subscribed, closed, unfolded, elsewhere: elsewhere(), // no secret link is exposed here
  }));
  host.on("app.fixture.remote.measure", () => void Promise.all([settingsBox?.measure(), projectsBox?.measure()]).then(([settings, projects]) => host.post("app.fixture.remote.layout", {
    settings: { y: settings?.pagePosition.y ?? -1, height: settings?.bounds.size.height ?? -1 }, projects: { y: projects?.pagePosition.y ?? -1 },
  })));
  // The same data again, as a new snapshot object: rows are redrawn, not remade.
  host.on("app.fixture.remote.publish", () => { state = { ...state, folders: state.folders.map((f) => ({ ...f })) }; publish(); });
  host.on("app.fixture.remote.hold", () => { hold = true; });
  host.on("app.fixture.remote.release", () => { hold = false; resume?.(); resume = null; });
  host.on("app.fixture.remote.unrelated", () => unrelated.set(unrelated() + 1));
  const offline = () => {
    state = { ...state, hosts: state.hosts.map((h) => ({ ...h, link: "offline", status: "Offline (or access revoked)" })),
      folders: state.folders.map((f) => ({ ...f, link: "offline", note: "Offline (or access revoked)", projects: f.projects.map((p) => ({ ...p, dimmed: true })) })),
      projects: state.projects.map((p) => ({ ...p, link: "offline" })) }; publish();
  };
  return <View style={{ flex: 1, gap: 8 }}>
    <View style={{ flexDirection: "row", gap: 6 }}>
      <Button testID="fixture-offline" onPress={offline}><Text>Offline fixture</Text></Button>
      <Button testID="fixture-worktrees" onPress={() => panel.set("worktrees")}><Text>Worktrees</Text></Button>
      <Button testID="fixture-tasks" onPress={() => panel.set("tasks")}><Text>Tasks</Text></Button>
      <Button testID="fixture-shells" onPress={() => panel.set("shells")}><Text>Shells</Text></Button>
      <Button testID="fixture-ended" onPress={() => ended.set(true)}><Text>End bridge fixture</Text></Button>
      <Input testID="fixture-elsewhere" accessibilityLabel="Elsewhere" value={elsewhere()} onChange={(value) => elsewhere.set(value)} style={{ width: 160 }} />
    </View>
    <View ref={(node) => { settingsBox = node; }}><RemoteSettings controller={c} /></View>
    <View ref={(node) => { projectsBox = node; }}><RemoteProjects controller={c} query={query} selected={selected} collapsed={collapsed} onToggleFolder={(key) => {
      const next = new Set(collapsed()); if (next.has(key)) next.delete(key); else next.add(key); collapsed.set(next);
    }} onSelectProject={(key) => { selected.set(key); actions.set([...actions(), `select:${key}`]); }} /></View>
    <RemoteProjectPanel controller={c} projectKey={() => KEY} panel={panel()} onWorktree={(id) => actions.set([...actions(), `worktree:${id}`])}
      onTask={(id) => actions.set([...actions(), `task:${id}`])} onShell={(host, shell) => {
        // As workspace.ts with no tab open on it: refuse a chat or ended shell.
        const refusal = shellOpenRefusal(shell, "Disposable Mac");
        if (refusal) notices.set([...notices(), refusal]); else actions.set([...actions(), `shell:${host}:${shell.id}`]);
      }} />
    <RemoteStrip label="Disposable Mac" link={c.data().hosts[0]?.link ?? null} ended={ended()} onReconnect={() => { actions.set([...actions(), "reattach-same-target"]); ended.set(false); }} />
    <RemoteUnavailable hostLabel="Disposable Mac" panel="files" />
  </View>;
}
mount(Fixture);
