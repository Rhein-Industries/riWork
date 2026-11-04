export interface AgentSession {
  id: string; kind: "chat" | "terminal"; projectId: string | null; parentId: string | null;
  title: string; provider: string | null; state: string; source: string; since: number | null;
  subagents: number | null; requests: string[]; completion: string | null; unseen: boolean;
}
export interface AgentHost {
  id: string; connectionId: string | null; label: string; route: string;
  connection: string; freshness: string; receivedAt: number | null; error: string | null;
  snapshot: { version: number; hostId: string; observedAt: number; capabilities: string[];
    sessions: AgentSession[]; total: number; partial: boolean } | null;
}
export interface HostSnapshot { version: number; hosts: AgentHost[]; error: string | null }
export type Filter = "all" | "working" | "attention" | "unseen";
export function needsAttention(session: AgentSession) {
  return ["waiting", "blocked", "error"].includes(session.state) || session.requests.length > 0;
}
export function sessions(host: AgentHost, filter: Filter, query: string) {
  const words = query.toLocaleLowerCase().trim().split(/\s+/).filter(Boolean);
  return (host.snapshot?.sessions ?? []).filter(s =>
    (filter === "all" || filter === "working" && s.state === "working" || filter === "attention" && needsAttention(s) || filter === "unseen" && s.unseen) &&
    words.every(word => [host.label, s.title, s.provider ?? "", s.state, s.projectId ?? ""].join(" ").toLocaleLowerCase().includes(word))
  ).sort((a,b) => Number(needsAttention(b)) - Number(needsAttention(a)) || Number(b.unseen) - Number(a.unseen) || a.title.localeCompare(b.title));
}
export function connectionText(host: AgentHost) {
  const text = host.connection === "online" ? "Connected" : host.connection === "blocked" ? "Needs setup" : host.connection === "backoff" ? "Disconnected · retrying" : "Connecting";
  return host.freshness === "stale" ? `${text} · showing last known state` : text;
}
export function projectKey(host: AgentHost, session: AgentSession): string | null {
  return session.projectId ? host.connectionId ? `remote:${host.connectionId}:${session.projectId}` : session.projectId : null;
}
export function routeStep(host: AgentHost, session: AgentSession) {
  if (host.connectionId) return session.kind === "chat"
    ? { kind: "remoteChat", desktopId: host.connectionId, chatId: session.id }
    : { kind: "remoteShell", desktopId: host.connectionId, shellId: session.id };
  return session.kind === "chat" ? { kind: "chat", chatId: session.id } : { kind: "showShell", shellId: session.id };
}
