/** The iOS device and each host's transport, over the Swift host API
 * (`ios/RiWorkRemote/NSHHostAPI.swift`, `docs/nsh-ios-migration.md`). Swift
 * keeps the pairings, keys and sockets; JS sees host ids and labels only. */
import { host } from "nsh";
import {
  PortError, type DecisionApi, type Device, type DeviceTheme, type HostInfo, type LinkState, type PresentRequest, type Subscription,
} from "../riwork/client/ports";
import type { RemoteTransport } from "../riwork/client/transport";
import type { ChatDecision, DecisionReviewDto } from "../riwork/client/types";
import { DEFAULT_THEME, NATIVE_PALETTE, type InterfaceTheme } from "../riwork/theme";

const OFFLINE: LinkState = { state: "offline", label: "Not connected" };
/** Before Swift's first host list: not known yet, so nothing is read and nothing is called offline. */
const UNKNOWN: LinkState = { state: "connecting", label: "Connecting…" };
/** `relay.call`'s own wait when the caller names none; Swift ends it sooner. */
const CALL_TIMEOUT_MS = 30_000;

/** A host call; its rejection (`"<code>: <message>"`) as a PortError. `timeout`
 * 0 waits as long as Swift takes, which itself bounds every request. */
async function call<T>(name: string, data?: unknown, timeout?: number): Promise<T> {
  try {
    return await host.call(name, data, timeout == null ? undefined : { timeout }) as T;
  } catch (cause) {
    throw PortError.from(cause);
  }
}

function deviceTheme(value: unknown): DeviceTheme {
  const pushed = (value ?? {}) as Partial<DeviceTheme> & { theme?: Partial<InterfaceTheme> };
  const theme: Partial<InterfaceTheme> = pushed.theme ?? {};
  return {
    theme: {
      palette: { ...NATIVE_PALETTE, ...(theme.palette ?? {}) },
      scale: typeof theme.scale === "number" && theme.scale > 0 ? theme.scale : DEFAULT_THEME.scale,
      family: theme.family ?? DEFAULT_THEME.family,
      monoFamily: theme.monoFamily ?? DEFAULT_THEME.monoFamily,
    },
    native: pushed.native !== false,
    mic: pushed.mic === true,
    dark: pushed.dark === true,
  };
}

export interface SwiftDevice extends Device {
  /** A host's link as the last `relay.hosts` or `relay.link` said. */
  link(hostId: string): LinkState;
  onLink(hostId: string, listener: (link: LinkState) => void): Subscription;
}

/** The device's ports. Made while a component renders, its host listeners stop with it. */
export function createSwiftDevice(): SwiftDevice {
  let hosts: HostInfo[] = [];
  let received = false;
  let theme = deviceTheme(host.params.theme);
  const hostListeners = new Set<(hosts: HostInfo[]) => void>();
  const linkListeners = new Map<string, Set<(link: LinkState) => void>>();
  const themeListeners = new Set<(theme: DeviceTheme) => void>();

  function notifyLink(id: string, link: LinkState) {
    for (const listener of [...(linkListeners.get(id) ?? [])]) listener(link);
  }
  function install(next: HostInfo[]) {
    const previous = new Map(hosts.map((info) => [info.id, info.link]));
    hosts = next;
    received = true;
    for (const info of next) {
      const before = previous.get(info.id);
      if (!before || before.state !== info.link.state || before.label !== info.link.label || before.error !== info.link.error) notifyLink(info.id, info.link);
    }
    for (const listener of [...hostListeners]) listener(hosts);
  }
  host.on("relay.hosts", (data) => install(((data ?? {}) as { hosts?: HostInfo[] }).hosts ?? []));
  host.on("relay.link", (data) => {
    const event = (data ?? {}) as { host?: string; link?: LinkState };
    if (!event.host || !event.link) return;
    hosts = hosts.map((info) => info.id === event.host ? { ...info, link: event.link! } : info);
    notifyLink(event.host, event.link);
  });
  host.on("platform.theme", (data) => {
    theme = deviceTheme(data);
    for (const listener of [...themeListeners]) listener(theme);
  });
  const list = async () => {
    const result = await call<{ hosts?: HostInfo[] }>("relay.hosts");
    install(result?.hosts ?? []);
    return hosts;
  };
  void list().catch(() => { /* The next `relay.hosts` event brings the list. */ });

  return {
    hosts: {
      current: () => hosts,
      list,
      subscribe(listener) {
        hostListeners.add(listener);
        return { close: () => { hostListeners.delete(listener); } };
      },
      select: async (id) => { await call("relay.select", { host: id }, 0); },
      disconnect: async (id) => { await call("relay.disconnect", { host: id }); },
    },
    prefs: {
      get: async (name) => (await call<{ value?: unknown }>("platform.prefs.get", { name }))?.value ?? null,
      set: async (name, value) => { await call("platform.prefs.set", { name, value }); },
    },
    theme: {
      current: () => theme,
      subscribe(listener) {
        themeListeners.add(listener);
        return { close: () => { themeListeners.delete(listener); } };
      },
    },
    nav: { present: async (request: PresentRequest) => { await call("nav.present", request, 0); } },
    link: (id) => hosts.find((info) => info.id === id)?.link ?? (received ? OFFLINE : UNKNOWN),
    onLink(id, listener) {
      const listeners = linkListeners.get(id) ?? new Set();
      listeners.add(listener);
      linkListeners.set(id, listeners);
      return { close: () => { listeners.delete(listener); } };
    },
  };
}

/** One host's relay through Swift: `relay.call` under Swift's allowlist and
 * request budget, and the decisions through Swift's own responder (a fresh
 * check of the request before sending, one send, never repeated). */
export function swiftTransport(device: SwiftDevice, hostId: string): RemoteTransport {
  const decisions: DecisionApi = {
    reviewDecision: (chatId: string, requestId: string, kind?: "approval" | "question") =>
      call<DecisionReviewDto>("relay.decision.review", { host: hostId, chatId, requestId, kind: kind ?? null }),
    // Swift bounds the send; a wait cut short here would only turn a known outcome into an uncertain one.
    respondDecision: (reviewId: string, decision?: ChatDecision | null, answers?: string[][] | null) =>
      call<DecisionReviewDto>("relay.decision.respond", { host: hostId, reviewId, decision: decision ?? null, answers: answers ?? null }, 0),
  };
  return {
    host: hostId,
    call(method, params, options) {
      const timeoutMs = options?.timeoutMs;
      const data: Record<string, unknown> = { host: hostId, method, params: params ?? {} };
      if (timeoutMs != null) data.timeoutMs = timeoutMs;
      // Swift answers by `timeoutMs` (or its own request timeout); this wait only covers a host that never answers.
      return call("relay.call", data, (timeoutMs ?? CALL_TIMEOUT_MS) + 5_000);
    },
    link: () => device.link(hostId),
    onLink: (listener) => device.onLink(hostId, listener),
    features: () => device.hosts.current().find((info) => info.id === hostId)?.features ?? [],
    decisions,
  };
}
