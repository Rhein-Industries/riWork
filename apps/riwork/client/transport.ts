/** How the remote adapter reaches one host. Shared code: nothing here may import
 * the Mac's native module. On iOS it is the Swift host's `relay.call` and
 * `relay.link` (`apps/riwork-mobile/swift_transport.ts`); Swift owns the socket,
 * the keys, the request budget and the method allowlist, and JS never sends a
 * long poll (`wait_ms` above 0) or `pty.*`. */
import type { DecisionApi, LinkState, Subscription } from "./ports";

export interface RemoteTransport {
  /** The host this transport reaches. */
  readonly host: string;
  /** One relay RPC: its `result`, or a rejection with a `PortError`. A call is
   * never repeated here; a create whose answer was lost rejects `uncertain`. */
  call(method: string, params?: Record<string, unknown>, options?: { timeoutMs?: number }): Promise<unknown>;
  link(): LinkState;
  onLink(listener: (link: LinkState) => void): Subscription;
  /** What the host's connection announced (`ready.features`), by name. */
  features(): string[];
  /** The host's own tested decision responder, when the device has one (iOS:
   * `relay.decision.review` and `relay.decision.respond`). */
  decisions?: DecisionApi;
}
