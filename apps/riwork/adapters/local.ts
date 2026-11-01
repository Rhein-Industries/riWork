/** The shared ports over this Mac's own native module. Mac only: nothing the iOS
 * entry (`apps/riwork-mobile`) imports may reach this file. Assigning the native
 * values to the shared shapes (`client/types.ts`) is what keeps those shapes
 * equal to the native declarations: `nsh check apps/riwork` fails on a drift. */
import { Dashboard, Store, type DashboardDecisionReview, type DashboardEvent, type ProjectDto } from "riwork/native";
import { LOCAL_LINK, PortError, type DashboardPort, type DecisionApi, type Fresh, type HostRef, type ProjectsPort, type Services } from "../client/ports";
import type { DashboardDto, DecisionReviewDto, ProjectSummary } from "../client/types";

export const LOCAL_HOST: HostRef = { id: "local", name: "This Mac" };

export function createLocalServices(): Services {
  return {
    host: LOCAL_HOST,
    link: { current: () => LOCAL_LINK, subscribe: () => ({ close() {} }) },
    dashboard: localDashboard(),
    projects: localProjects(),
  };
}

/** The review calls go to the native module as they are: its errors reach the
 * review unchanged, and the review treats a failed send as uncertain. */
export const LOCAL_DECISIONS: DecisionApi = {
  reviewDecision: (chatId, requestId, kind): Promise<DecisionReviewDto> => review(Dashboard.reviewDecision(chatId, requestId, kind)),
  respondDecision: (reviewId, decision, answers): Promise<DecisionReviewDto> => review(Dashboard.respondDecision(reviewId, decision, answers)),
};
const review = (value: Promise<DashboardDecisionReview>): Promise<DecisionReviewDto> => value;

function localDashboard(): DashboardPort {
  return {
    subscribe(listener) {
      // The native read is shared by every window; a revision gap is a snapshot.
      let rev = -1;
      const subscription = Dashboard.subscribe((event: DashboardEvent) => {
        if (event.rev <= rev && !event.resync) return;
        rev = event.rev;
        const dto: DashboardDto = event.data;
        listener(dto);
      });
      return { close: () => subscription.close() };
    },
    refresh: () => Dashboard.refresh(),
    decisions: LOCAL_DECISIONS,
  };
}

/** A native error's prefix (`service.rs`, `files.rs`) as a port error code. */
export function localError(cause: unknown): PortError {
  const text = cause instanceof Error ? cause.message : String(cause);
  const match = /^(conflict|refused|unavailable|invalid|io): ?([\s\S]*)$/.exec(text);
  if (!match) return new PortError("failed", text);
  return new PortError(match[1] === "conflict" ? "conflict" : match[1] === "unavailable" ? "offline" : "failed", match[2] || text);
}

function localProjects(): ProjectsPort {
  let fresh: Fresh<ProjectSummary[]> = { value: null, readAt: null, error: null };
  const summary = (project: ProjectDto): ProjectSummary => ({ id: project.id, name: project.name, root: project.root, createdAt: project.createdAt });
  return {
    async list() {
      try {
        const value = (await Store.snapshot()).projects.map(summary);
        fresh = { value, readAt: Math.floor(Date.now() / 1000), error: null };
        return value;
      } catch (cause) {
        const failure = localError(cause);
        fresh = { ...fresh, error: failure };
        throw failure;
      }
    },
    last: () => fresh,
  };
}
