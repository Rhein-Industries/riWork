import type { LayoutSession } from "riwork/native";
import type { WorkspaceSnapshot } from "../model";
import { remoteProjectKey, shellOpenRefusal, type RemoteShell, type RemoteSnapshot } from "./model";

/** Only existing engine operations. No transport, local shell API or retry lives here. */
export interface WorkspaceRemoteApi {
  createShell(request: { projectKey: string; worktreeId: string | null; kind: string; unrestricted: boolean }): Promise<RemoteShell>;
  clickWorktree(key: string, worktree: string, openTabs: string[]): Promise<{ action: string; shellId: string | null; shell: RemoteShell | null }>;
  orchestrators(host: string): Promise<RemoteShell[]>;
}
export function workspaceRemote(options: {
  api: WorkspaceRemoteApi;
  session: Pick<LayoutSession, "openRemoteShell" | "selectPane" | "selectPanel">;
  projectId: () => string;
  snapshot: () => WorkspaceSnapshot;
  data: () => RemoteSnapshot;
  selectedWorktree: () => string | null;
  alive: () => boolean;
  generation: () => number;
  install: (value: unknown) => void;
  focus: (key: string) => void;
  notice: (value: string) => void;
}) {
  const parts = () => remoteProjectKey(options.projectId());
  const lists = () => options.data().projects.find((project) => project.key === options.projectId());
  const current = (project: string, generation: number) => options.alive() && options.projectId() === project && options.generation() === generation;
  const creating = new Set<string>();
  function open(host: string, shell: RemoteShell) {
    const label = options.data().hosts.find((row) => row.id === host)?.label ?? host.slice(0, 8);
    const refusal = shellOpenRefusal(shell, label);
    if (refusal) { options.notice(refusal); return; }
    options.install(options.session.openRemoteShell(host, shell.id));
    options.focus(`remote:${host}:${shell.id}`);
  }
  // Record the destination pane before dispatch. If it closed, core chooses the
  // ordinary new-tab pane. The engine settles every result even after a switch.
  async function create(kind = "shell", worktreeId = options.selectedWorktree()) {
    const project = options.projectId(), generation = options.generation(), remote = parts(); if (!remote || creating.has(project)) return;
    const pane = options.snapshot().activePane;
    creating.add(project);
    try {
      const shell = await options.api.createShell({ projectKey: project, worktreeId, kind, unrestricted: false });
      if (!current(project, generation)) return;
      if (options.snapshot().panes.some((entry) => entry.id === pane)) options.install(options.session.selectPane(pane));
      open(remote.host, shell);
    } finally { creating.delete(project); }
  }
  async function worktree(id: string) {
    const project = options.projectId(), generation = options.generation(), remote = parts(); if (!remote) return;
    const prefix = `remote:${remote.host}:`;
    const tabs = options.snapshot().panes.flatMap((pane) => pane.tabs).filter((tab) => tab.id.startsWith(prefix)).map((tab) => tab.id.slice(prefix.length));
    const result = await options.api.clickWorktree(project, id, tabs);
    if (!current(project, generation)) return;
    if (result.action === "tab" && result.shellId) options.install(options.session.selectPanel(`${prefix}${result.shellId}`));
    else if (result.action === "open" && result.shell) open(remote.host, result.shell);
    else if (result.action === "start") await create("shell", id);
  }
  async function orchestrator() {
    const project = options.projectId(), generation = options.generation(), remote = parts(); if (!remote) return;
    const known = lists()?.shells.find((shell) => shell.orchestrator && shell.projectId === remote.project);
    if (known) { open(remote.host, known); return; }
    const rows = await options.api.orchestrators(remote.host);
    if (!current(project, generation)) return;
    const found = rows.find((shell) => shell.orchestrator && shell.projectId === remote.project);
    if (found) open(remote.host, found);
    else options.notice(`${lists()?.hostLabel ?? remote.host.slice(0, 8)} has no project orchestrator running`);
  }
  return { open, create, worktree, orchestrator };
}
