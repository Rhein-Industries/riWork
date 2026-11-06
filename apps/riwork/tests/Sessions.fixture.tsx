/** The Sessions panel with in-memory inventory: one project, three worktrees, chats in each state
 * and shells that are live, exited, a harness and an editor. Store, Sessions and Chat still answer
 * from the isolated RIWORK_HOME the test runs in; `app.fixture.seed` then replaces what they
 * delivered. No session process, tmux server or chat host starts. */
import { Text, View, host, mount, signal } from "nsh";
import type { ChatSummary, ShellSessionDto, StoreState } from "riwork/native";
import { PanelContent, ShellIntent, createPanelController } from "../Panels";
import { EMPTY_STORE } from "../panel_model";

// Not a UUID, so the controller opens no Tabs subscription for it.
const PROJECT = "fixture-riwork";
const ROOT = "/Users/dominik/orca/projects/riWork";
const PANEL = `${ROOT}/.claude/worktrees/sessions-panel`, SCROLL = `${ROOT}/.claude/worktrees/fix-chat-scroll`;

const STATE: StoreState = {
  ...EMPTY_STORE, activeProjectId: PROJECT,
  projects: [{ id: PROJECT, name: "riWork", root: ROOT, repositoryRoots: [ROOT], folderId: null, notifyOnAgentDone: true,
    wakeOrchestratorsOnAgentDone: false, codexAccount: { source: "default" }, orchestrator: null, delegationPolicy: null, createdAt: 1_760_000_000 }],
  worktrees: [
    { id: "wt-main", projectId: PROJECT, branch: "main", path: ROOT, isPrimary: true, repositoryRoot: ROOT, createdAt: 1_760_000_000, present: true },
    { id: "wt-panel", projectId: PROJECT, branch: "sessions-panel", path: PANEL, isPrimary: false, repositoryRoot: ROOT, createdAt: 1_761_000_000, present: true },
    { id: "wt-scroll", projectId: PROJECT, branch: "fix-chat-scroll", path: SCROLL, isPrimary: false, repositoryRoot: ROOT, createdAt: 1_761_500_000, present: true },
  ],
};

const chat = (id: string, provider: ChatSummary["provider"], title: string, state: string, worktreeId: string | undefined, cwd: string): ChatSummary =>
  ({ id, provider, title, state, projectId: PROJECT, worktreeId, cwd });
const CHATS: ChatSummary[] = [
  chat("3f6c1a52-8d0e-4b7a-9c21-5e4f0b7d9a13", "claude", "Polish the Sessions panel rows and filters", "running", "wt-panel", PANEL),
  chat("9a04e7d1-2b6f-4c3e-8f15-7d2a6c0e4b88", "codex", "Migrate the remote relay to protocol 2 and keep every paired phone working", "waiting", "wt-main", ROOT),
  chat("c2e8b6f0-5a71-4d9c-b3e2-1f0a9d7c6e54", "claude", "Investigate the flaky DockArea drag test", "failed", "wt-scroll", SCROLL),
  chat("71d5f3a9-0c4e-4e2b-a6d8-3b9f2e1c0a77", "muse", "Muse chat", "idle", undefined, ROOT),
  chat("e5b2c9d4-7f18-4a60-9e3c-2d4b8a1f6c09", "codex", "Write the 0.3 release notes", "stopped", "wt-main", ROOT),
];

const BASE: ShellSessionDto = {
  id: "", projectId: PROJECT, worktreeId: null, kind: "project", cwd: ROOT, command: null,
  alive: true, userOpened: true, parentId: null, editorPath: null, harness: null,
  unrestricted: false, codexAccountId: null, codexAccountLabel: null, codexAccountEmail: null, codexHome: null,
  orchestratorSkillLoaded: false, orchestratorSkillVersion: null, orchestratorDelegationVersion: null,
  orchestratorProjectRoot: null, createdAtUnix: 1_761_900_000,
};
const SHELLS: ShellSessionDto[] = [
  { ...BASE, id: "1b7e4c2a-9d3f-4a85-b0c6-8e2f5d1a7c40", command: "cargo watch -x 'test -p riwork-core'" },
  { ...BASE, id: "4d9a2f6e-1c8b-4e73-a5d0-9b3c7e2f1a86", worktreeId: "wt-panel", cwd: PANEL, harness: "claude", command: "claude --permission-mode acceptEdits" },
  { ...BASE, id: "8c3f1e7b-5a2d-4b96-9e04-6d1b8a3c5f22", cwd: `${ROOT}/apps/riwork`, editorPath: `${ROOT}/apps/riwork/Panels.tsx`, command: "vim Panels.tsx" },
  { ...BASE, id: "a6e0d4b9-3f7c-4d12-8b5a-0c9e6f2d4b31", worktreeId: "wt-scroll", cwd: SCROLL, alive: false },
  // Not a project shell: the panel leaves it out.
  { ...BASE, id: "f0c2a8e6-4b1d-4f59-a7e3-5c8d0b6a2e19", kind: "orchestrator", cwd: `${ROOT}/.orchestrator` },
];

function Fixture() {
  const intent = signal<string | null>(null);
  const activeProject = signal(PROJECT);
  const attempted: string[] = [], closed: string[] = [];
  let failure = false;
  const notice = signal("");
  const controller = createPanelController({
    projectId: activeProject, projectLabel: () => "riWork",
    switchProject: async () => {}, openPanel: () => {},
    showShell: (id) => intent.set(id), showChat: () => {}, newChat: () => {},
    selectTask: () => {}, selectWorktree: () => {}, notice: (message) => notice.set(message),
    sessionActions: {
      list: async () => ({ sessions: controller.shells() }),
      close: async (id) => {
        attempted.push(id);
        if (failure && id === SHELLS[2].id) throw new Error("Fixture exit refused");
        closed.push(id); controller.shells.set(controller.shells().filter((shell) => shell.id !== id));
      },
    },
  });
  host.on("app.fixture.inspect", () => host.post("app.fixture.state", { attempted, closed, shells: controller.shells(), chats: controller.chats(), notice: notice() }));
  host.on("app.fixture.failure", () => { failure = true; });
  host.on("app.fixture.project-switch", () => { activeProject.set("fixture-other"); controller.resetProject(); });
  host.on("app.fixture.add-shell", () => controller.shells.set([...controller.shells(), { ...BASE, id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa" }]));
  host.on("app.fixture.seed", () => {
    controller.state.set(STATE); controller.shells.set([...SHELLS, { ...BASE, id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", projectId: "fixture-other" }]); controller.chats.set(CHATS);
    controller.catalogNotice.set(""); controller.catalogLoading.set(false);
  });
  return <View style={{ flex: 1 }}>
    <PanelContent controller={controller} panel="shells" surface="fixture" />
    <Text testID="fixture-cleanup-notice">{notice()}</Text>
    {intent() && <ShellIntent controller={controller} shellId={intent()!} />}
  </View>;
}
mount(Fixture);
