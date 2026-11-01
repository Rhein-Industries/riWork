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
}
