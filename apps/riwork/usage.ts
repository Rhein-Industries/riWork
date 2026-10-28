/**
 * The status bar's account readouts, ported 1:1 from
 * crates/riwork-desktop/src/account_status.rs and main.rs
 * (`project_default_account_label`, `codex_account_numbers`): PANEL-180..190.
 * Pure: everything is read already, by `Usage.subscribe` and `Accounts.subscribe`.
 */

export const SYSTEM_DEFAULT_ID = "system-default";
export const SYSTEM_LABEL = "System default";
/** Grok session figures are not read by the native Usage service. */
export const GROK_UNREAD = "GROK · —";
export const BALANCED_LABEL = "SUBSCRIPTIONS (AUTO)";
export const BALANCED_INHERIT = "Automatically chooses the saved subscription with the most usable quota. Explicit project choices below stay fixed.";
/** Older than this, figures may have moved (15 min). */
export const STALE_AFTER_SECS = 15 * 60;

export interface UsageWindowLike { label: string; usedPercent: number }
export interface ProviderUsageLike { windows: UsageWindowLike[]; updatedAtUnix: number; accountLabel?: string | null }
export interface AccountLike {
  id: string; label: string; email?: string | null; homePath: string;
  available: boolean; unavailableReason?: string | null; system: boolean;
}
/** `listed: false` is GPUI's missing snapshot: no saved list was read yet. */
export interface AccountListLike { listed: boolean; accounts: AccountLike[]; error?: string | null; refreshing?: boolean }
export interface CodexEntryLike { home: string; usage?: ProviderUsageLike | null; error?: string | null; pending: boolean }
export interface ClaudeEntryLike { shellId: string; usage?: ProviderUsageLike | null }
export interface ShellLike {
  id: string; projectId?: string | null; harness?: string | null; alive: boolean; codexHome?: string | null;
  codexAccountId?: string | null; codexAccountLabel?: string | null; codexAccountEmail?: string | null;
}
/** `ProjectCodexAccountDto`: `inherit`, `system_default` or `saved` with its id. */
export interface ProjectChoice { source: string; accountId?: string | null }

export interface CodexProfile { home: string; label: string }
export type Selection = { kind: "account"; home: string; label: string } | { kind: "unavailable" } | { kind: "unknown" };
export type UsageFocus =
  | { kind: "codex"; account: Selection; number?: number }
  | { kind: "claude_shell"; id: string }
  | { kind: "grok_shell"; id: string; alive: boolean }
  | { kind: "other" };
export type UsageSource = "codex" | "claude" | "grok_session";
export type Freshness = "current" | "stale" | "loading" | "unavailable";
export type UsageMode = "current_chat" | "all_accounts";

export interface UsageSegment {
  source: UsageSource;
  account?: string;
  number?: number;
  figures: string;
  freshness: Freshness;
  note?: string;
}

const account = (profile: CodexProfile): Selection => ({ kind: "account", home: profile.home, label: profile.label });
const isSystem = (id: string | null | undefined) => id == null || id === SYSTEM_DEFAULT_ID;

// ---- Which account ------------------------------------------------------------------------

/** What `selected` names (null, or the system default's id, for the system default). */
export function resolveSelection(selected: string | null | undefined, list: AccountListLike): Selection {
  const id = isSystem(selected) ? null : selected!;
  if (!list.listed) {
    if (id !== null) return { kind: "unknown" };
    const system = list.accounts.find((entry) => entry.system);
    return system ? { kind: "account", home: system.homePath, label: SYSTEM_LABEL } : { kind: "unavailable" };
  }
  const found = list.accounts.find((entry) => entry.available && (id === null ? entry.system : entry.id === id));
  return found ? { kind: "account", home: found.homePath, label: found.label } : { kind: "unavailable" };
}

/** The account new Codex sessions of a project get, the app's selection standing in for Inherit. */
export function projectSelection(choice: ProjectChoice, appSelected: string | null | undefined, list: AccountListLike): Selection {
  if (choice.source === "system_default") return resolveSelection(null, list);
  if (choice.source === "saved") return resolveSelection(choice.accountId ?? null, list);
  return resolveSelection(appSelected, list);
}

/** The accounts All accounts reads out: available ones by home, list order; unlisted, the system default. */
export function knownCodexProfiles(list: AccountListLike): CodexProfile[] {
  if (!list.listed) {
    const system = list.accounts.find((entry) => entry.system);
    return system ? [{ home: system.homePath, label: SYSTEM_LABEL }] : [];
  }
  const homes = new Set<string>();
  const profiles: CodexProfile[] = [];
  for (const entry of list.accounts) {
    if (!entry.available || homes.has(entry.homePath)) continue;
    homes.add(entry.homePath);
    profiles.push({ home: entry.homePath, label: entry.label });
  }
  return profiles;
}

function verifiedShellEmail(shell: ShellLike, list: AccountListLike): string | undefined {
  if (shell.codexAccountEmail) return shell.codexAccountEmail;
  if (!list.listed) return undefined;
  return list.accounts.find((entry) => shell.codexAccountId === entry.id && shell.codexHome === entry.homePath)?.email ?? undefined;
}

/** The account a Codex shell runs under, by the home it was started with. */
export function shellSelection(shell: ShellLike, list: AccountListLike): Selection {
  const home = shell.codexHome;
  if (!home) return { kind: "unknown" };
  const label = (list.listed ? list.accounts.find((entry) => entry.homePath === home)?.label : undefined)
    ?? verifiedShellEmail(shell, list)
    ?? shell.codexAccountLabel
    ?? (shell.codexAccountId == null ? SYSTEM_LABEL : "Session account");
  return { kind: "account", home, label };
}

/** What Codex itself said the account at `home` is, from its last usage read. */
export function reportedAccountLabel(codex: CodexEntryLike[], home: string | undefined): string | undefined {
  if (!home) return undefined;
  return codex.find((entry) => entry.home === home)?.usage?.accountLabel ?? undefined;
}

export function systemHome(selection: Selection): string | undefined {
  return selection.kind === "account" ? selection.home : undefined;
}

function comparePaths(a: string, b: string): number {
  const left = a.split("/").filter(Boolean);
  const right = b.split("/").filter(Boolean);
  for (let index = 0; index < Math.min(left.length, right.length); index++) {
    if (left[index] !== right[index]) return left[index] < right[index] ? -1 : 1;
  }
  return left.length - right.length;
}

/** "A{n}" numbers by frozen home across a project's live Codex shells, once it runs more than one. */
export function codexAccountNumbers(shells: ShellLike[]): Map<string, number> {
  const live = shells.filter((shell) => shell.alive && shell.harness === "codex" && shell.projectId && shell.codexHome);
  const homes = new Map<string, string[]>();
  for (const shell of live) {
    const list = homes.get(shell.projectId!) ?? [];
    if (!list.includes(shell.codexHome!)) list.push(shell.codexHome!);
    homes.set(shell.projectId!, list);
  }
  const numbers = new Map<string, number>();
  for (const shell of live) {
    const list = [...(homes.get(shell.projectId!) ?? [])].sort(comparePaths);
    if (list.length > 1) numbers.set(shell.id, list.indexOf(shell.codexHome!) + 1);
  }
  return numbers;
}

/** What the focused tab runs. Chat tabs carry no provider here and read as Other (the project default). */
export function usageFocus(tabId: string | undefined, shells: ShellLike[], list: AccountListLike): UsageFocus {
  const id = tabId?.startsWith("shell:") ? tabId.slice(6) : undefined;
  const shell = id ? shells.find((entry) => entry.id === id) : undefined;
  if (!shell) return { kind: "other" };
  if (shell.harness === "codex") return { kind: "codex", account: shellSelection(shell, list), number: codexAccountNumbers(shells).get(shell.id) };
  if (shell.harness === "claude") return { kind: "claude_shell", id: shell.id };
  if (shell.harness === "grok") return { kind: "grok_shell", id: shell.id, alive: shell.alive };
  return { kind: "other" };
}

// ---- The usage readouts -------------------------------------------------------------------

/** Older than a refresh takes. */
export function stale(usage: ProviderUsageLike, now: number): boolean {
  return Math.max(0, now - usage.updatedAtUnix) > STALE_AFTER_SECS;
}

/** Rust's `{:.0}`: an exact half rounds to even. */
export function whole(value: number): string {
  const floor = Math.floor(value);
  const rounded = value - floor === 0.5 ? (floor % 2 === 0 ? floor : floor + 1) : Math.round(value);
  return String(rounded === 0 ? 0 : rounded);
}

/** The first two windows: `5h 80% left / 7d 60% left`, or compact `5h 80% / 7d 60% left`. */
export function limits(usage: ProviderUsageLike, compact: boolean): string {
  const windows = usage.windows.slice(0, 2).map((window) => [window.label, Math.min(100, Math.max(0, 100 - window.usedPercent))] as const);
  if (windows.length === 0) return "—";
  const joined = windows.map(([label, left]) => compact ? `${label} ${whole(left)}%` : `${label} ${whole(left)}% left`).join(" / ");
  return compact ? `${joined} left` : joined;
}

export function mark(segment: UsageSegment): string {
  return segment.freshness === "stale" ? "*" : "";
}

/** `CODEX`, with the account's number when its tab has one, and the stale mark. */
export function codexTag(segment: UsageSegment): string {
  return segment.number !== undefined ? `CODEX A${segment.number}${mark(segment)}` : `CODEX${mark(segment)}`;
}

/** One line: Current chat's whole item, and each segment's hint. */
export function segmentText(segment: UsageSegment): string {
  if (segment.source === "codex") return segment.account !== undefined ? `${codexTag(segment)} · ${segment.account} · ${segment.figures}` : `CODEX · ${segment.figures}`;
  if (segment.source === "claude") return `CLAUDE${mark(segment)} · ${segment.figures}`;
  return segment.figures;
}

export interface UsageInputs {
  mode: UsageMode;
  focus: UsageFocus;
  projectDefault: Selection;
  /** `knownCodexProfiles`; read only in All accounts. */
  accounts: CodexProfile[];
  codex: CodexEntryLike[];
  /** Claude's reports, by shell (only shells that reported). */
  claude: ClaudeEntryLike[];
  /** Grok chip labels by shell; never read here, so absent shows `GROK · —`. */
  grok?: Record<string, string>;
  now: number;
}

function codexSegment(selection: Selection, inputs: UsageInputs, compact: boolean): UsageSegment {
  if (selection.kind !== "account") {
    return { source: "codex", figures: selection.kind === "unknown" ? "ACCOUNT UNKNOWN" : "ACCOUNT UNAVAILABLE", freshness: "unavailable" };
  }
  const entry = inputs.codex.find((candidate) => candidate.home === selection.home);
  const base = { source: "codex" as const, account: selection.label };
  if (entry?.usage) {
    return {
      ...base, figures: limits(entry.usage, compact),
      freshness: entry.error || stale(entry.usage, inputs.now) ? "stale" : "current",
      ...(entry.error ? { note: `Last refresh failed: ${entry.error}` } : {}),
    };
  }
  if (entry?.error && !entry.pending) return { ...base, figures: "UNAVAILABLE", freshness: "unavailable", note: entry.error };
  // Asked for and not answered, or not asked yet: the next refresh asks.
  return { ...base, figures: "LOADING", freshness: "loading" };
}

function claudeSegment(usage: ProviderUsageLike, compact: boolean, now: number): UsageSegment {
  return { source: "claude", figures: limits(usage, compact), freshness: stale(usage, now) ? "stale" : "current" };
}

/** Claude's subscription: the newest report with quota windows, else the newest (an API key has none). */
export function claudeSubscription(claude: ClaudeEntryLike[]): ProviderUsageLike | undefined {
  const reports = [...claude].sort((a, b) => a.shellId < b.shellId ? -1 : a.shellId > b.shellId ? 1 : 0)
    .map((entry) => entry.usage).filter((usage): usage is ProviderUsageLike => !!usage);
  const newest = (list: ProviderUsageLike[]) => list.reduce<ProviderUsageLike | undefined>((best, usage) => !best || usage.updatedAtUnix >= best.updatedAtUnix ? usage : best, undefined);
  return newest(reports.filter((usage) => usage.windows.length > 0)) ?? newest(reports);
}

/** Current chat: one readout. All accounts: one per known Codex account, then Claude once known. */
export function usageSegments(inputs: UsageInputs): UsageSegment[] {
  if (inputs.mode === "current_chat") {
    const focus = inputs.focus;
    if (focus.kind === "claude_shell") {
      const usage = inputs.claude.find((entry) => entry.shellId === focus.id)?.usage;
      return [usage ? claudeSegment(usage, false, inputs.now)
        : { source: "claude", figures: "WAITING", freshness: "loading", note: "Claude reports usage after its first response" }];
    }
    if (focus.kind === "grok_shell") {
      return [{ source: "grok_session", figures: focus.alive ? inputs.grok?.[focus.id] ?? GROK_UNREAD : "GROK · EXITED", freshness: "current" }];
    }
    if (focus.kind === "codex") {
      const segment = codexSegment(focus.account, inputs, false);
      return [focus.number !== undefined ? { ...segment, number: focus.number } : segment];
    }
    return [codexSegment(inputs.projectDefault, inputs, false)];
  }
  const segments = inputs.accounts.map((profile) => codexSegment(account(profile), inputs, true));
  // Not even the system default's home could be found.
  if (segments.length === 0) segments.push(codexSegment({ kind: "unavailable" }, inputs, true));
  const claude = claudeSubscription(inputs.claude);
  if (claude) segments.push(claudeSegment(claude, true, inputs.now));
  return segments;
}

/** One account's compact readout: its label (or CLAUDE), then its figures. */
export function compactReadout(segment: UsageSegment): string {
  const lead = segment.account ?? (segment.source === "claude" ? "CLAUDE" : undefined);
  const figures = `${mark(segment)}${segment.figures}`;
  return lead ? `${lead} ${figures}` : figures;
}

/** The item's visible text: Current chat's line, or the quiet CODEX prefix and every account. */
export function usageItemText(mode: UsageMode, segments: UsageSegment[]): string {
  if (mode === "current_chat") return segments[0] ? segmentText(segments[0]) : "";
  const parts = segments.map(compactReadout);
  return (segments.some((segment) => segment.source === "codex") ? ["CODEX", ...parts] : parts).join("  ");
}

/** The item's accessible name (PANEL-184). */
export function usageAria(segments: UsageSegment[]): string {
  return `Usage ${segments.map(segmentText).join("; ")}`;
}

/** The hint: one line per segment with its note, then the click. */
export function usageHint(segments: UsageSegment[]): string {
  return segments.map((segment) => segment.note ? `${segmentText(segment)} (${segment.note})` : segmentText(segment))
    .concat("Click for account usage").join("\n");
}

// ---- The Codex account item and menu ------------------------------------------------------

/** "DEFAULT ({PROJECT/SYSTEM/APP}) · {detail}" (main.rs `project_default_account_label`). */
export function projectDefaultAccountLabel(choice: ProjectChoice | undefined, appSelected: string | null | undefined, list: AccountListLike, systemLabel: string | undefined): string {
  const [source, selected]: [string, string | null] = choice?.source === "saved" ? ["PROJECT", choice.accountId ?? null]
    : choice?.source === "system_default" ? ["SYSTEM", null]
    : ["APP", isSystem(appSelected) ? null : appSelected!];
  const found = selected !== null && list.listed ? list.accounts.find((entry) => entry.id === selected) : undefined;
  const detail = selected === null ? `System default · ${systemLabel ?? "email unknown"}`
    : found && found.available ? found.email ?? "Email unknown"
    : found ? "Account unavailable"
    : !list.listed ? "Email unknown"
    : "Account unavailable";
  return `DEFAULT (${source}) · ${detail}`;
}

/** The Codex account item: balanced while the project inherits, else the project default. */
export function accountItemLabel(balancing: boolean, choice: ProjectChoice | undefined, appSelected: string | null | undefined, list: AccountListLike, systemLabel: string | undefined): string {
  if (balancing && (!choice || choice.source === "inherit")) return BALANCED_LABEL;
  return projectDefaultAccountLabel(choice, appSelected, list, systemLabel);
}

export interface DefaultChoiceRow { choice: ProjectChoice; label: string; detail?: string; selectable: boolean; current: boolean }
export interface DefaultChoices { status?: string; rows: DefaultChoiceRow[] }

export function sameChoice(a: ProjectChoice, b: ProjectChoice): boolean {
  return a.source === b.source && (a.source !== "saved" || (a.accountId ?? null) === (b.accountId ?? null));
}

/** The menu's rows: Inherit, System default, every saved account, and a kept missing pin. */
export function defaultChoices(stored: ProjectChoice, appSelected: string | null | undefined, list: AccountListLike, systemLabel: string | undefined): DefaultChoices {
  const system = (label: string) => systemLabel !== undefined ? `${label} · ${systemLabel}` : label;
  const appSystem = isSystem(appSelected);
  const resolved = resolveSelection(appSelected, list);
  const inherit = resolved.kind === "account" ? (appSystem ? `Uses ${system(SYSTEM_LABEL)}` : `Uses ${resolved.label}`)
    : resolved.kind === "unknown" ? "Uses the app's account · checking accounts…"
    : "The app's account is unavailable";
  const rows: DefaultChoiceRow[] = [
    { choice: { source: "inherit" }, label: "Inherit app default", detail: inherit, selectable: true, current: stored.source === "inherit" },
    { choice: { source: "system_default" }, label: SYSTEM_LABEL, ...(systemLabel !== undefined ? { detail: systemLabel } : {}), selectable: true, current: stored.source === "system_default" },
  ];
  const status = !list.listed ? (list.refreshing ? "Reading saved accounts…" : "Saved accounts have not been read") : list.error ?? undefined;
  if (list.listed) {
    for (const entry of list.accounts.filter((candidate) => !candidate.system)) {
      const choice = { source: "saved", accountId: entry.id };
      rows.push({
        choice, label: entry.label, selectable: entry.available, current: sameChoice(stored, choice),
        ...(!entry.available ? { detail: entry.unavailableReason ?? "Unavailable" } : {}),
      });
    }
  }
  if (stored.source === "saved" && !rows.some((row) => sameChoice(row.choice, stored))) {
    const short = [...(stored.accountId ?? "")].slice(0, 8).join("");
    rows.push({
      choice: stored, selectable: false, current: true,
      label: list.listed ? `Saved account unavailable · ${short}` : `Saved account · ${short}`,
      detail: list.listed ? "Choose another before starting Codex" : "Checking whether it is still saved…",
    });
  }
  return { ...(status ? { status } : {}), rows };
}
