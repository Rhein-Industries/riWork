/** In-memory Usage panel fixture. No native usage service, account discovery, session
 * inventory, Codex app-server, Claude status line or Grok harness. */
import { mount, host, View } from "nsh";
import type { AccountsDto, SessionsSnapshot, ShellSessionDto, UsageDto } from "riwork/native";
import { DEFAULT_THEME } from "./appearance";
import { UsagePanel, type UsagePanelApi } from "./UsagePanel";

const PROJECT = "11111111-1111-4111-8111-111111111111";
const OTHER = "22222222-2222-4222-8222-222222222222";
const SYSTEM_HOME = "/Users/fixture/.codex", WORK_HOME = "/Users/fixture/.riwork/codex/work", OLD_HOME = "/Users/fixture/.riwork/codex/old";
const now = Math.floor(Date.now() / 1000);
const minutes = (count: number) => count * 60, hours = (count: number) => count * 3600;

const shell = (id: string, harness: string, patch: Partial<ShellSessionDto> = {}): ShellSessionDto => ({
  userOpened: true, id, projectId: PROJECT, kind: "project", cwd: "/Users/fixture/src/alpha", harness, unrestricted: false,
  orchestratorSkillLoaded: false, createdAtUnix: now - hours(2), alive: true, ...patch,
});
const SHELLS: ShellSessionDto[] = [
  shell("c0de1111-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "codex", { codexHome: SYSTEM_HOME, codexAccountLabel: "System default" }),
  shell("c1a0de22-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "claude"),
  shell("c1a0de33-cccc-4ccc-8ccc-cccccccccccc", "claude"),
  shell("9f3c4444-dddd-4ddd-8ddd-dddddddddddd", "grok"),
  shell("c1a0de55-eeee-4eee-8eee-eeeeeeeeeeee", "claude", { projectId: OTHER }),
];

const ACCOUNTS: AccountsDto = {
  schema: 1, selectedId: null, selectedUnavailable: false, balancing: false, listed: true, fromCache: false, refreshing: false,
  accounts: [
    { id: "system-default", label: "System default", email: "dominik@example.invalid", home: "~/.codex", homePath: SYSTEM_HOME, system: true,
      available: true, saved: false, orcaActive: true, detail: "Current Codex profile · ~/.codex" },
    { id: "work", label: "Work", email: "dominik@work.example.invalid", home: "~/.riwork/codex/work", homePath: WORK_HOME, system: false,
      available: true, saved: true, orcaActive: false, detail: "Saved in Orca" },
    { id: "old", label: "Old team", email: "old@example.invalid", home: "~/.riwork/codex/old", homePath: OLD_HOME, system: false,
      available: false, unavailableReason: "Signed out. Run codex login for this account.", saved: true, orcaActive: false, detail: "Signed out" },
  ],
};

const USAGE: UsageDto = {
  schema: 1, now, refreshing: false,
  codex: [
    { home: SYSTEM_HOME, accountId: "system-default", label: "System default", pending: false, failures: 0, stale: false, lastAttempt: now - minutes(4),
      usage: { provider: "codex", updatedAtUnix: now - minutes(4), accountLabel: "dominik@example.invalid · pro",
        windows: [
          { label: "5h", usedPercent: 38, resetsAt: now + hours(2) + minutes(14), windowMinutes: 300 },
          { label: "7d", usedPercent: 71, resetsAt: now + hours(3 * 24 + 5), windowMinutes: 10080 },
        ] } },
    { home: WORK_HOME, accountId: "work", label: "Work", pending: false, failures: 2, stale: true, lastAttempt: now - minutes(3),
      error: "codex app-server did not answer within 10 s",
      usage: { provider: "codex", updatedAtUnix: now - minutes(48), accountLabel: "dominik@work.example.invalid · team",
        windows: [
          { label: "5h", usedPercent: 91, resetsAt: now + minutes(37), windowMinutes: 300 },
          { label: "7d", usedPercent: 44, resetsAt: now + hours(5 * 24 + 2), windowMinutes: 10080 },
        ] } },
  ],
  claude: [
    { shellId: "c1a0de22-bbbb-4bbb-8bbb-bbbbbbbbbbbb", projectId: PROJECT, waiting: false, stale: false,
      usage: { provider: "claude", updatedAtUnix: now - 50, contextUsedPercent: 41, sessionCostUsd: 1.84,
        windows: [
          { label: "5h", usedPercent: 12, resetsAt: now + hours(3) + minutes(2), windowMinutes: 300 },
          { label: "7d", usedPercent: 54, resetsAt: now + hours(2 * 24 + 7), windowMinutes: 10080 },
        ] } },
    { shellId: "c1a0de33-cccc-4ccc-8ccc-cccccccccccc", projectId: PROJECT, waiting: true, stale: false },
    { shellId: "c1a0de55-eeee-4eee-8eee-eeeeeeeeeeee", projectId: OTHER, waiting: true, stale: false },
  ],
  grok: [
    { shellId: "9f3c4444-dddd-4ddd-8ddd-dddddddddddd", projectId: PROJECT, label: "Grok · 9f3c4444", stale: false, pending: false, fetchedAtUnix: now - minutes(1),
      usage: { sessionId: "grok-session-1", updatedAtUnix: now - minutes(1), primaryModel: "grok-4", turns: 14, modelCalls: 31, costUsd: 0.4127,
        tokens: { input: 182_400, output: 21_950, cachedRead: 640_300, cacheCreation: 12_100, reasoning: 8_420, total: 865_170 },
        models: [
          { model: "grok-4", modelCalls: 27, costUsd: 0.3981, tokens: { input: 170_200, output: 20_100, cachedRead: 612_000, cacheCreation: 12_100, reasoning: 8_420, total: 822_820 } },
          { model: "grok-3-mini", modelCalls: 4, costUsd: 0.0146, tokens: { input: 12_200, output: 1_850, cachedRead: 28_300, cacheCreation: 0, reasoning: 0, total: 42_350 } },
        ] } },
  ],
};

function Fixture() {
  // Plain arrays: the panel calls the API inside its effects, which must not track them.
  const calls: string[] = [];
  let usage = USAGE, shells = SHELLS, rev = 1;
  const listeners = new Set<(data: UsageDto) => void>(), sessionListeners = new Set<(data: SessionsSnapshot) => void>();
  const publish = () => { rev++; for (const listener of listeners) listener(usage); };
  // Deliver after the current tick, as the native hub does.
  const later = (deliver: () => void) => { void Promise.resolve().then(deliver); };
  const closer = (close: () => void) => ({ close }) as ReturnType<UsagePanelApi["usage"]["subscribe"]>;
  const api: UsagePanelApi = {
    usage: {
      subscribe: (listener, project) => {
        calls.push(`usage:${project}`);
        const deliver = (data: UsageDto) => listener({ kind: "snapshot", rev, data });
        listeners.add(deliver); later(() => { if (listeners.has(deliver)) deliver(usage); });
        return closer(() => { listeners.delete(deliver); });
      },
      refresh: async (force) => {
        calls.push(`refresh:${force}`);
        usage = { ...usage, refreshing: true }; publish();
        await new Promise((resolve) => setTimeout(resolve, 300));
        usage = { ...usage, refreshing: false }; publish();
      },
    },
    accounts: {
      subscribe: (listener) => {
        calls.push("accounts"); let open = true;
        later(() => { if (open) listener({ kind: "snapshot", rev: 1, data: ACCOUNTS }); });
        return closer(() => { open = false; });
      },
    },
    sessions: {
      subscribe: (listener) => {
        calls.push("sessions");
        const deliver = (data: SessionsSnapshot) => listener({ kind: "snapshot", rev, data });
        sessionListeners.add(deliver); later(() => { if (sessionListeners.has(deliver)) deliver({ sessions: shells }); });
        return closer(() => { sessionListeners.delete(deliver); });
      },
    },
  };
  // Only Codex is in use: the Claude and Grok sections show their empty lines.
  host.on("app.fixture.usage.codex-only", () => {
    shells = SHELLS.filter((shell) => shell.harness === "codex");
    usage = { ...usage, claude: [], grok: [] }; publish();
    for (const listener of sessionListeners) listener({ sessions: shells });
  });
  host.on("app.fixture.usage.inspect", () => host.post("app.fixture.usage.state", { calls, listeners: listeners.size }));
  return <View style={{ flex: 1 }}>
    <UsagePanel api={api} projectId={() => PROJECT} theme={() => DEFAULT_THEME} visible={() => true} />
  </View>;
}
mount(Fixture);
