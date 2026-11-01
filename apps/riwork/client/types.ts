/** The data shapes the shared screens draw. Shared code: nothing here may import
 * the Mac's native module, not even its types. The shapes are copies of the
 * native declarations (`apps/riwork/.nsh/types/riwork/native.d.ts`); the Mac
 * adapter (`adapters/local.ts`) assigns the native values to them, so
 * `nsh check apps/riwork` fails when the two drift apart. */

/** One Backends row (`overview::BackendLineDto`). */
export interface DashboardBackendLine { key: string; title: string; detail: string; tone: "current" | "stale" | "unknown" | "idle" }

/** What the dashboard shows (`overview::DashboardDto`): the last good read and
 * why a newer one failed. `now` and `readAt` are the host's Unix seconds;
 * `wire` is `dashboard.get` (wire v1, `dashboard/model.ts` `Wire`). */
export interface DashboardDto {
  schema: number;
  now: number;
  readAt: number | null;
  error: string | null;
  wire: unknown | null;
  backendLines: DashboardBackendLine[] | null;
}
/** The schema the Mac's dashboard snapshots carry. */
export const DASHBOARD_SCHEMA = 3;

export type ChatDecision = "accept" | "accept_for_session" | "decline" | "cancel";
export interface ChatApproval {
  requestId: string;
  kind: "command" | "file_change" | "permissions" | "tool";
  heading: string;
  title: string;
  detail: string;
  choices: { decision: ChatDecision; label: string; key?: string }[];
}
export interface ChatQuestion {
  requestId: string;
  prompts: { header?: string; question: string; options: { label: string; description: string }[]; multiSelect: boolean }[];
}

/** A provider request under review (`DashboardDecisionReview`). `reviewId` is an
 * opaque token the host keeps: it holds the exact request, so a response names
 * only the token and the answer, never the request again. */
export interface DecisionReviewDto {
  reviewId: string;
  chatId: string;
  requestId: string;
  title: string;
  kind: "approval" | "question";
  approval?: ChatApproval;
  question?: ChatQuestion;
  status: "pending" | "sending" | "sent" | "uncertain" | "unavailable";
  error?: string;
}

/** A host's project, as a list shows it. `createdAt` and `lastActivity` are Unix seconds. */
export interface ProjectSummary {
  id: string;
  name: string;
  root: string;
  createdAt?: number;
  lastActivity?: number;
  /** When the Mac last saw one of its files change (`projects.list` `last_edited_unix`). */
  lastEdited?: number;
  /** Its agent shells by state (`projects.list` `agents`). */
  agents?: { working: number; waiting: number; done?: number };
}

/** A folder of the host's project tree (`Store.snapshot` `projectFolders`, `FolderDto`). */
export interface StoreFolder { id: string; name: string; parentId?: string | null; createdAt: number }
/** A project of the host's store (`Store.snapshot` `projects`): the fields the shared screens read of `ProjectDto`. */
export interface StoreProject { id: string; name: string; root: string; folderId?: string | null; createdAt: number }
/** The parts of the host's `Store.snapshot` (`StoreState`) the shared Projects page reads: the
 * projects, their folders and the last observed file edits (`projectRecency`, Unix seconds). */
export interface StoreTree {
  projects: StoreProject[];
  projectFolders: StoreFolder[];
  projectRecency: Record<string, number>;
}

/** One entry of a project's shared tab list (`tabs.list`, `docs/shared-tabs.md`): the Mac owns
 * membership, order, title and visibility; selection is the device's own. `key` is
 * `shell:<uuid>` or `chat:<uuid>`; `status` is working, waiting, error, done, idle or stopped. */
export interface TabEntry {
  key: string;
  kind: string;
  title: string;
  status: string;
  hidden: boolean;
  worker: boolean;
  order: number;
  parent: string | null;
  children: TabEntry[];
  childCount: number;
}
/** `tabs.list`, `tabs.open` and `tabs.update` all answer the whole list. A reply supersedes another of
 * the same `epoch` only with a revision at least as high; a new epoch always does. */
export interface TabsReply { epoch: string | null; revision: number | null; entries: TabEntry[] }
export type TabUpdate =
  | { action: "hide" | "unhide"; key: string }
  | { action: "move"; key: string; before: string | null }
  | { action: "rename"; key: string; title: string };

/** A project's shell or orchestrator (`shells.list` Session). `mode` "chat": an orchestrator that runs
 * as a chat, opened by its `chatId`. `activity`: working, waiting, done, unknown or exited. */
export interface HostShell {
  id: string;
  projectId: string | null;
  worktreeId: string | null;
  kind: string;
  cwd: string;
  harness: string | null;
  alive: boolean;
  createdAt: number;
  activity?: string;
  lastActivity?: number;
  mode?: string;
  chatId?: string;
  provider?: string;
}
/** A chat of a project (`chats.list` ChatInfo), as a tab shows it. `state` is starting, idle, running,
 * waiting, stopped or failed (with `message`). */
export interface HostChat {
  id: string;
  provider: string;
  title: string;
  projectId: string | null;
  worktreeId: string | null;
  state: string;
  message?: string;
  createdAt: number;
}
/** A worktree of a project (`worktrees.list`). */
export interface HostWorktree { id: string; projectId: string; branch: string; path: string; isPrimary: boolean }

/** What `shell.create` starts: a plain shell or an agent. A Mac without Grok or Muse refuses them. */
export type ShellKind = "shell" | "codex" | "claude" | "grok" | "muse";
export interface ShellCreate {
  kind: ShellKind;
  /** The project's main worktree, or (`worktreeId`) one of its worktrees: exactly one of them. */
  projectId?: string;
  worktreeId?: string;
  /** An agent without approval prompts (never with `asSettings`). */
  unrestricted?: boolean;
  /** An agent started as the Mac's "Agent terminals run unrestricted" setting says (`shell_create_as_settings`). */
  asSettings?: boolean;
}
