/** What a project's page does with the host's tab list, shells and chats, decided apart from drawing:
 * which tabs the strip shows, what closing one may do, where a move puts it and what the picker of
 * hidden tabs lists. The host's shared list (`tabs.list`, `docs/shared-tabs.md`) is the truth: every
 * change is one `tabs.update` whose reply replaces the list. */
import type { HostChat, HostShell, TabEntry, TabUpdate, TabsReply } from "../riwork/client/types";

export interface WorkspaceTab {
  /** `shell:<id>` or `chat:<id>`. */
  key: string;
  kind: "shell" | "chat";
  /** The shell's or the chat's id. */
  id: string;
  title: string;
  /** working, waiting, error, done, idle, stopped or unknown. */
  status: string;
  worker: boolean;
  orchestrator: boolean;
  /** Its entry in the shared list; null on a host without one. */
  entry: TabEntry | null;
}

/** Every entry of the list, children after their parent. */
export function allEntries(reply: TabsReply | null): TabEntry[] {
  const result: TabEntry[] = [];
  const visit = (entries: TabEntry[]) => { for (const entry of entries) { result.push(entry); visit(entry.children); } };
  visit(reply?.entries ?? []);
  return result;
}

const shown = (entry: TabEntry) => !entry.hidden && !(entry.kind === "shell" && entry.status === "stopped");
const sessionId = (key: string) => key.slice(key.indexOf(":") + 1);

/** A newer reply replaces the list: another epoch, or the same with a revision at least as high. */
export function supersedes(next: TabsReply, current: TabsReply | null): boolean {
  return !current || next.epoch !== current.epoch || (next.revision ?? 0) >= (current.revision ?? 0);
}

/** The strip: with the shared list, its visible entries in shared order; without one (an older Mac), the
 * live project terminals, then the chats that are not stopped, oldest first. */
export function workspaceTabs(reply: TabsReply | null, shells: HostShell[], chats: HostChat[]): WorkspaceTab[] {
  const shellById = new Map(shells.map((shell) => [shell.id, shell]));
  if (reply) {
    return allEntries(reply).filter((entry) => shown(entry) && (entry.kind === "shell" || entry.kind === "chat"))
      .sort((a, b) => a.order - b.order)
      .map((entry) => {
        const id = sessionId(entry.key), shell = shellById.get(id);
        return {
          key: entry.key, kind: entry.kind as "shell" | "chat", id, title: entry.title || (entry.kind === "chat" ? "Chat" : "Terminal"),
          status: entry.status, worker: entry.worker || entry.parent != null, orchestrator: shell?.kind === "orchestrator", entry,
        };
      });
  }
  const terminals = shells.filter((shell) => shell.alive && shell.mode !== "chat").sort((a, b) => a.createdAt - b.createdAt)
    .map((shell): WorkspaceTab => ({
      key: `shell:${shell.id}`, kind: "shell", id: shell.id, title: shellTitle(shell), status: shellStatus(shell),
      worker: false, orchestrator: shell.kind === "orchestrator", entry: null,
    }));
  const talks = chats.filter((chat) => chat.state !== "stopped").sort((a, b) => a.createdAt - b.createdAt)
    .map((chat): WorkspaceTab => ({
      key: `chat:${chat.id}`, kind: "chat", id: chat.id, title: chat.title, status: chatStatus(chat), worker: false, orchestrator: false, entry: null,
    }));
  return [...terminals, ...talks];
}

/** A shell's name where the host gives none: its orchestrator, its agent, or a terminal. */
export function shellTitle(shell: HostShell): string {
  if (shell.kind === "orchestrator") return shell.projectId ? "Project orchestrator" : "Global orchestrator";
  if (shell.harness) return shell.harness.charAt(0).toUpperCase() + shell.harness.slice(1);
  return "Terminal";
}
function shellStatus(shell: HostShell): string {
  if (!shell.alive || shell.activity === "exited") return "stopped";
  return shell.activity === "working" || shell.activity === "waiting" || shell.activity === "done" ? shell.activity : "idle";
}
function chatStatus(chat: HostChat): string {
  switch (chat.state) {
    case "running": return "working";
    case "waiting": return "waiting";
    case "failed": return "error";
    case "stopped": return "stopped";
    case "idle": return "idle";
    default: return "unknown";
  }
}

/** The hidden tabs the picker offers: workers first, each group in shared order; a stopped shell is not one. */
export function openableEntries(reply: TabsReply | null): TabEntry[] {
  return allEntries(reply).filter((entry) => entry.hidden && (entry.kind === "chat" || (entry.kind === "shell" && entry.status !== "stopped")))
    .sort((a, b) => Number(b.worker || b.parent != null) - Number(a.worker || a.parent != null) || a.order - b.order);
}

/** What closing a tab may do. A worker and an orchestrator's terminal are only detached (hidden); a project
 * terminal may also be ended (`shell.close`); a chat is detached here and stopped from its own menu. Without
 * the shared list only a project terminal closes, and that ends it. */
export interface ClosePlan { detach: boolean; exit: boolean }
export function closePlan(tab: WorkspaceTab, shells: HostShell[]): ClosePlan {
  const shell = tab.kind === "shell" ? shells.find((item) => item.id === tab.id) : undefined;
  const endable = tab.kind === "shell" && !tab.worker && !tab.orchestrator && shell?.kind === "project";
  if (!tab.entry) return { detach: false, exit: endable };
  return { detach: true, exit: endable };
}

/** Move left and Move right, as the Mac's tab menu has them: one place among the visible tabs of the same
 * parent; null at the group's boundary. Right goes before the tab after the next one in shared order (hidden
 * members included), or to the group's end. */
export function moveLeft(entry: TabEntry, reply: TabsReply): TabUpdate | null {
  const visible = allEntries(reply).filter((other) => shown(other) && other.parent === entry.parent).sort((a, b) => a.order - b.order);
  const index = visible.findIndex((other) => other.key === entry.key);
  return index > 0 ? { action: "move", key: entry.key, before: visible[index - 1].key } : null;
}
export function moveRight(entry: TabEntry, reply: TabsReply): TabUpdate | null {
  const visible = allEntries(reply).filter((other) => shown(other) && other.parent === entry.parent).sort((a, b) => a.order - b.order);
  const index = visible.findIndex((other) => other.key === entry.key);
  if (index < 0 || index + 1 >= visible.length) return null;
  const group = allEntries(reply).filter((other) => other.parent === entry.parent).sort((a, b) => a.order - b.order);
  const at = group.findIndex((other) => other.key === visible[index + 1].key);
  if (at < 0) return null;
  const after = group.slice(at + 1).find((other) => other.key !== entry.key);
  return { action: "move", key: entry.key, before: after?.key ?? null };
}

/** The tab `step` places from `current` (wrapping); the first or last when `current` is not in the strip. */
export function cycled(tabs: WorkspaceTab[], current: string | null, step: number): WorkspaceTab | null {
  if (!tabs.length) return null;
  const index = tabs.findIndex((tab) => tab.key === current);
  const from = index < 0 ? (step > 0 ? tabs.length - 1 : 0) : index;
  return tabs[((from + step) % tabs.length + tabs.length) % tabs.length];
}

/** The terminal a page opens on: the one asked for, else the remembered one, else the first terminal. */
export function initialTerminal(tabs: WorkspaceTab[], ...wanted: (string | null | undefined)[]): string | null {
  for (const key of wanted) if (key && tabs.some((tab) => tab.key === key && tab.kind === "shell")) return key;
  return tabs.find((tab) => tab.kind === "shell")?.key ?? null;
}
