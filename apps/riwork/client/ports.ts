/** What a shared screen may ask of a host (`Services`) and of the device it runs
 * on (`Device`). Shared code: nothing here may import the Mac's native module,
 * not even its types. The Mac implements the ports over its native module
 * (`adapters/local.ts`); iOS over the encrypted relay (`client/remote.ts`).
 * Every call names its host; no screen holds a credential, a token or an URL. */
import type { InterfaceTheme } from "../theme";
import type { ChatDecision, DashboardDto, DecisionReviewDto, ProjectSummary } from "./types";

export type PortErrorCode = "offline" | "unsupported" | "not_found" | "conflict" | "limit" | "uncertain" | "failed";
export const PORT_ERROR_CODES: readonly PortErrorCode[] = ["offline", "unsupported", "not_found", "conflict", "limit", "uncertain", "failed"];

/** Why a port call did not do what was asked. `uncertain`: the request may have
 * reached the host, so nothing may send it again on its own. */
export class PortError extends Error {
  readonly code: PortErrorCode;
  constructor(code: PortErrorCode, message: string) {
    super(message);
    this.name = "PortError";
    this.code = code;
  }
  /** The message alone: screens put it in a sentence of their own. */
  override toString(): string { return this.message; }
  /** A rejection from a host call (`"<code>: <message>"`, the Swift host API's
   * form) or any other failure, as a PortError; what names no code is `failed`. */
  static from(cause: unknown): PortError {
    if (cause instanceof PortError) return cause;
    const text = cause instanceof Error ? cause.message : String(cause);
    const match = /^([a-z_]+): ([\s\S]*)$/.exec(text);
    if (match && (PORT_ERROR_CODES as readonly string[]).includes(match[1])) return new PortError(match[1] as PortErrorCode, match[2]);
    return new PortError("failed", text);
  }
}

/** One host's connection as a screen shows it. `since`: Unix seconds of the last change, when known. */
export interface LinkState {
  state: "connected" | "connecting" | "offline" | "failed" | "suspended";
  label: string;
  error?: string;
  since?: number;
}
export const LOCAL_LINK: LinkState = { state: "connected", label: "This Mac" };

/** A value read from a host: the last good one stays while newer reads fail.
 * `readAt` is Unix seconds; it is stale after `STALE_AFTER_SECS` without a read. */
export interface Fresh<T> {
  value: T | null;
  readAt: number | null;
  error: PortError | null;
}
export const STALE_AFTER_SECS = 30;
export function isStale<T>(fresh: Fresh<T>, now: number): boolean {
  return fresh.readAt == null || fresh.error != null || now - fresh.readAt > STALE_AFTER_SECS;
}

export interface Subscription { close(): void }

/** The two calls a decision review makes. The host keeps the request behind
 * `reviewId` and checks it again, fresh, before anything is sent; a response
 * whose delivery is not known comes back `uncertain` and is never sent again. */
export interface DecisionApi {
  reviewDecision(chatId: string, requestId: string, kind?: "approval" | "question"): Promise<DecisionReviewDto>;
  respondDecision(reviewId: string, decision?: ChatDecision | null, answers?: string[][] | null): Promise<DecisionReviewDto>;
}

export interface DashboardPort {
  /** Snapshots while anyone listens: the last one at once when there is one,
   * then one after every read (a failed read keeps the last good wire and says
   * why). Nothing is read while nobody listens. */
  subscribe(listener: (dto: DashboardDto) => void): Subscription;
  /** Read everything again now; the result arrives as the next snapshot. */
  refresh(): Promise<void>;
  decisions: DecisionApi;
}

export interface ProjectsPort {
  /** Reads the host's projects now. Rejects with a PortError. */
  list(): Promise<ProjectSummary[]>;
  /** The last good list, when it was read, and why a newer read failed. */
  last(): Fresh<ProjectSummary[]>;
}

export interface LinkPort {
  current(): LinkState;
  subscribe(listener: (link: LinkState) => void): Subscription;
}

export interface HostRef { id: string; name: string }

/** One host's ports. Members a host lacks stay absent; a screen hides what needs them. */
export interface Services {
  host: HostRef;
  link: LinkPort;
  dashboard: DashboardPort;
  projects: ProjectsPort;
}

/** A paired host as the device lists it. `features`: what its connection announced. */
export interface HostInfo {
  id: string;
  name: string;
  address?: string;
  selected: boolean;
  link: LinkState;
  features: string[];
}

export interface HostsPort {
  /** The last list the device sent (empty before the first). */
  current(): HostInfo[];
  list(): Promise<HostInfo[]>;
  subscribe(listener: (hosts: HostInfo[]) => void): Subscription;
  /** Makes `id` the interactive host. */
  select(id: string): Promise<void>;
  disconnect(id: string): Promise<void>;
}

/** Per-device preferences by name; the device maps a name to where it keeps it. */
export interface PrefsPort {
  get(name: string): Promise<unknown>;
  set(name: string, value: unknown): Promise<void>;
}

/** The theme the device resolved: its palette choice, the host's synced
 * palette, contrast fixes. `native`: the system skin; `mic`: dictation allowed. */
export interface DeviceTheme { theme: InterfaceTheme; native: boolean; mic: boolean; dark: boolean }
export interface ThemePort {
  current(): DeviceTheme;
  subscribe(listener: (theme: DeviceTheme) => void): Subscription;
}

/** A screen the device still draws natively (`nav.present`). */
export interface PresentRequest {
  name: string;
  host?: string;
  projectId?: string;
  shellId?: string;
  chatId?: string;
  /** `global` only: start the global orchestrator when there is none (an explicit Start). */
  start?: boolean;
}
export interface NavPort { present(request: PresentRequest): Promise<void> }

/** The device's own ports (iOS: the Swift host). */
export interface Device {
  hosts: HostsPort;
  prefs: PrefsPort;
  theme: ThemePort;
  nav: NavPort;
}
