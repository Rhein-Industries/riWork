import { expect, test } from "nsh/test";
import {
  BALANCED_LABEL, GROK_UNREAD, formatBytes, resourcesText, SYSTEM_DEFAULT_ID, SYSTEM_LABEL, accountItemLabel, codexAccountNumbers, defaultChoices,
  knownCodexProfiles, limits, projectDefaultAccountLabel, projectSelection, shellSelection, usageAria, usageFocus,
  usageHint, usageItemText, usageSegments, whole, type AccountLike, type AccountListLike, type ClaudeEntryLike,
  type CodexEntryLike, type ProviderUsageLike, type Selection, type ShellLike, type UsageFocus, type UsageInputs,
} from "./usage";

// Expectations copied from crates/riwork-desktop/src/account_status/tests.rs.
const NOW = 1_800_000_000;
const LONG_A = "a.rather.long.mailbox.name@example.test · A workspace with a long name";
const TEAM_B = "b@example.test · Team B";
const SYSTEM_REPORTED = "sys@example.test · plus";

function account(id: string, label: string, home: string): AccountLike {
  const email = label.split(" · ")[0];
  return { id, label, email: email.includes("@") ? email : null, homePath: home, available: true, unavailableReason: null, system: id === SYSTEM_DEFAULT_ID };
}

/** The system default, two saved accounts and a third whose home is gone. */
function accounts(root = "/fixture"): AccountListLike {
  const gone = { ...account("account-c", "c@example.test", `${root}/c`), available: false, unavailableReason: "This saved account's home is missing." };
  return { listed: true, accounts: [account(SYSTEM_DEFAULT_ID, SYSTEM_LABEL, `${root}/system`), account("account-a", LONG_A, `${root}/a`), account("account-b", TEAM_B, `${root}/b`), gone] };
}
const unlisted = (pending = false): AccountListLike => ({ listed: false, refreshing: pending, accounts: [account(SYSTEM_DEFAULT_ID, SYSTEM_LABEL, "/fixture/system")] });

function profile(list: AccountListLike, id: string): Selection {
  const found = list.accounts.find((entry) => entry.id === id)!;
  return { kind: "account", home: found.homePath, label: found.label };
}
const home = (selection: Selection) => selection.kind === "account" ? selection.home : "";

function quota(used: [number, number], updated: number): ProviderUsageLike {
  return { windows: [{ label: "5h", usedPercent: used[0] }, { label: "7d", usedPercent: used[1] }], updatedAtUnix: updated, accountLabel: null };
}
const read = (at: string, usage: ProviderUsageLike): CodexEntryLike => ({ home: at, usage, pending: false });
const failed = (at: string, error: string): CodexEntryLike => ({ home: at, error, pending: false });

class Reads {
  codex: CodexEntryLike[] = [];
  claude: ClaudeEntryLike[] = [];
  put(entry: CodexEntryLike) { this.codex = this.codex.filter((other) => other.home !== entry.home).concat(entry); }
  segments(mode: UsageInputs["mode"], focus: UsageFocus, projectDefault: Selection, profiles: UsageInputs["accounts"] = []) {
    return usageSegments({ mode, focus, projectDefault, accounts: profiles, codex: this.codex, claude: this.claude, now: NOW });
  }
  current(focus: UsageFocus, projectDefault: Selection) {
    const segments = this.segments("current_chat", focus, projectDefault);
    expect(segments.length).toBe(1);
    return segments[0];
  }
}
const text = (segment: ReturnType<Reads["current"]>) => usageAria([segment]).slice("Usage ".length);
const unknown: Selection = { kind: "unknown" };

test("P5-USAGE-01: current chat reads the focused Codex account, else the project default, and names it", () => {
  const list = accounts();
  const a = profile(list, "account-a");
  const b = profile(list, "account-b");
  const reads = new Reads();
  reads.put(read(home(a), quota([20, 40], NOW)));
  reads.put({ home: home(b), pending: true });

  const focused = reads.current({ kind: "codex", account: a }, b);
  expect([focused.source, focused.account, focused.freshness]).toEqual(["codex", LONG_A, "current"]);
  expect(text(focused)).toBe(`CODEX · ${LONG_A} · 5h 80% left / 7d 60% left`);
  expect(text(reads.current({ kind: "codex", account: a, number: 2 }, b))).toBe(`CODEX A2 · ${LONG_A} · 5h 80% left / 7d 60% left`);

  const fallback = reads.current({ kind: "other" }, b);
  expect(fallback.freshness).toBe("loading");
  expect(text(fallback)).toBe(`CODEX · ${TEAM_B} · LOADING`);
  expect(text(reads.current({ kind: "codex", account: unknown }, a))).toBe("CODEX · ACCOUNT UNKNOWN");
  expect(text(reads.current({ kind: "other" }, { kind: "unavailable" }))).toBe("CODEX · ACCOUNT UNAVAILABLE");

  reads.put(read(home(a), quota([20, 40], NOW - 16 * 60)));
  const old = reads.current({ kind: "codex", account: a }, unknown);
  expect(old.freshness).toBe("stale");
  expect(text(old).startsWith("CODEX* · ")).toBe(true);
  reads.put({ ...read(home(a), quota([20, 40], NOW)), error: "Codex is signed out" });
  const failedOver = reads.current({ kind: "codex", account: a }, unknown);
  expect(failedOver.freshness).toBe("stale");
  expect(failedOver.note).toBe("Last refresh failed: Codex is signed out");

  reads.claude.push({ shellId: "claude-1", usage: quota([50, 10], NOW) });
  expect(text(reads.current({ kind: "claude_shell", id: "claude-1" }, unknown))).toBe("CLAUDE · 5h 50% left / 7d 90% left");
  const waiting = reads.current({ kind: "claude_shell", id: "claude-2" }, unknown);
  expect(text(waiting)).toBe("CLAUDE · WAITING");
  expect(waiting.note).toBe("Claude reports usage after its first response");
  // GPUI read Grok and said "GROK · LOADING" before its first read; the native
  // service never reads Grok, so an unread live Grok shell says "GROK · —".
  expect(text(reads.current({ kind: "grok_shell", id: "grok-1", alive: true }, unknown))).toBe(GROK_UNREAD);
  expect(text(reads.current({ kind: "grok_shell", id: "grok-1", alive: false }, unknown))).toBe("GROK · EXITED");
});

test("P5-USAGE-02: all accounts lists every Codex account, keeps unread ones and adds only a known Claude", () => {
  const list = accounts();
  const profiles = knownCodexProfiles(list);
  expect(profiles.map((entry) => entry.label)).toEqual([SYSTEM_LABEL, LONG_A, TEAM_B]);
  const reads = new Reads();
  reads.put(read(home(profile(list, SYSTEM_DEFAULT_ID)), quota([10, 30], NOW)));
  reads.put(failed(home(profile(list, "account-a")), "Codex is signed out"));
  for (const focus of [{ kind: "grok_shell", id: "grok-1", alive: true }, { kind: "other" }, { kind: "codex", account: profile(list, "account-b") }] as UsageFocus[]) {
    const all = reads.segments("all_accounts", focus, profile(list, "account-a"), profiles);
    expect(all.map((segment) => [segment.source, segment.account, segment.figures, segment.freshness])).toEqual([
      ["codex", SYSTEM_LABEL, "5h 90% / 7d 70% left", "current"],
      ["codex", LONG_A, "UNAVAILABLE", "unavailable"],
      ["codex", TEAM_B, "LOADING", "loading"],
    ]);
    expect(all[1].note).toBe("Codex is signed out");
  }
  reads.claude.push({ shellId: "older", usage: quota([50, 10], NOW - 60) });
  reads.claude.push({ shellId: "newer", usage: { windows: [], updatedAtUnix: NOW } });
  const all = reads.segments("all_accounts", { kind: "other" }, unknown, profiles);
  expect(all.length).toBe(4);
  const claude = all[3];
  expect([claude.source, claude.account, claude.figures]).toEqual(["claude", undefined, "5h 50% / 7d 90% left"]);
  expect(all.every((segment) => segment.source === "codex" || segment.source === "claude")).toBe(true);
  expect(usageItemText("all_accounts", all)).toBe(`CODEX  System default 5h 90% / 7d 70% left  ${LONG_A} UNAVAILABLE  ${TEAM_B} LOADING  CLAUDE 5h 50% / 7d 90% left`);
  expect(knownCodexProfiles(unlisted()).map((entry) => entry.label)).toEqual([SYSTEM_LABEL]);
  // No system home at all: one unavailable readout.
  expect(usageItemText("all_accounts", reads.segments("all_accounts", { kind: "other" }, unknown, []).slice(0, 1))).toBe("CODEX  ACCOUNT UNAVAILABLE");
});

test("P5-USAGE-03: the default choices mark the saved one and say what each comes to", () => {
  const list = accounts();
  const choices = defaultChoices({ source: "inherit" }, "account-a", list, SYSTEM_REPORTED);
  expect(choices.status).toBe(undefined);
  expect(choices.rows.map((row) => [row.label, row.detail, row.selectable, row.current])).toEqual([
    ["Inherit app default", `Uses ${LONG_A}`, true, true],
    [SYSTEM_LABEL, SYSTEM_REPORTED, true, false],
    [LONG_A, undefined, true, false],
    [TEAM_B, undefined, true, false],
    ["c@example.test", "This saved account's home is missing.", false, false],
  ]);
  expect(choices.rows[2].choice).toEqual({ source: "saved", accountId: "account-a" });
  expect(defaultChoices({ source: "system_default" }, null, list, SYSTEM_REPORTED).rows[0].detail).toBe("Uses System default · sys@example.test · plus");
  const system = defaultChoices({ source: "system_default" }, null, list, undefined);
  expect(system.rows[0].detail).toBe("Uses System default");
  expect(system.rows[1].detail).toBe(undefined);
  expect(system.rows[1].current && !system.rows[0].current).toBe(true);
  expect(defaultChoices({ source: "inherit" }, "account-c", list, undefined).rows[0].detail).toBe("The app's account is unavailable");

  const gone = { source: "saved", accountId: "removed-account" };
  const kept = defaultChoices(gone, null, list, undefined);
  const last = kept.rows[kept.rows.length - 1];
  expect(last.label).toBe("Saved account unavailable · removed-");
  expect(last.current && !last.selectable).toBe(true);
  expect(kept.rows.filter((row) => row.current).length).toBe(1);
  const unavailable = defaultChoices({ source: "saved", accountId: "account-c" }, null, list, undefined);
  expect(unavailable.rows.some((row) => row.current && !row.selectable && row.label === "c@example.test")).toBe(true);

  for (const [pending, status] of [[true, "Reading saved accounts…"], [false, "Saved accounts have not been read"]] as const) {
    const unread = defaultChoices(gone, "account-a", unlisted(pending), undefined);
    expect(unread.status).toBe(status);
    expect(unread.rows.map((row) => [row.choice, row.selectable, row.current])).toEqual([
      [{ source: "inherit" }, true, false], [{ source: "system_default" }, true, false], [gone, false, true],
    ]);
    expect(unread.rows[2].label).toBe("Saved account · removed-");
    expect(unread.rows[0].detail).toBe("Uses the app's account · checking accounts…");
  }
  expect(defaultChoices({ source: "inherit" }, null, { ...list, error: "Orca's CLI is unavailable." }, undefined).status).toBe("Orca's CLI is unavailable.");
});

test("P5-USAGE-04: the project default resolves as a launch does and the item names it", () => {
  const list = accounts();
  const a = profile(list, "account-a");
  const system = profile(list, SYSTEM_DEFAULT_ID);
  expect(projectSelection({ source: "inherit" }, "account-a", list)).toEqual(a);
  expect(projectSelection({ source: "inherit" }, SYSTEM_DEFAULT_ID, list)).toEqual(system);
  expect(projectSelection({ source: "system_default" }, "account-a", list)).toEqual(system);
  expect(projectSelection({ source: "saved", accountId: "account-a" }, null, list)).toEqual(a);
  expect(projectSelection({ source: "saved", accountId: "account-c" }, null, list)).toEqual({ kind: "unavailable" });
  expect(projectSelection({ source: "saved", accountId: "account-a" }, null, unlisted())).toEqual(unknown);
  expect(projectDefaultAccountLabel({ source: "system_default" }, null, list, SYSTEM_REPORTED)).toBe("DEFAULT (SYSTEM) · System default · sys@example.test · plus");
  // main.rs project_default_account_label's remaining forms.
  expect(projectDefaultAccountLabel(undefined, null, unlisted(), undefined)).toBe("DEFAULT (APP) · System default · email unknown");
  expect(projectDefaultAccountLabel({ source: "inherit" }, "account-b", list, undefined)).toBe("DEFAULT (APP) · b@example.test");
  expect(projectDefaultAccountLabel({ source: "saved", accountId: "account-c" }, null, list, undefined)).toBe("DEFAULT (PROJECT) · Account unavailable");
  expect(projectDefaultAccountLabel({ source: "saved", accountId: "gone" }, null, list, undefined)).toBe("DEFAULT (PROJECT) · Account unavailable");
  expect(projectDefaultAccountLabel({ source: "saved", accountId: "account-a" }, null, unlisted(), undefined)).toBe("DEFAULT (PROJECT) · Email unknown");
  // PANEL-186: balancing applies while the project inherits, or with no project.
  expect(accountItemLabel(true, undefined, null, list, undefined)).toBe(BALANCED_LABEL);
  expect(accountItemLabel(true, { source: "inherit" }, "account-a", list, undefined)).toBe("SUBSCRIPTIONS (AUTO)");
  expect(accountItemLabel(true, { source: "saved", accountId: "account-b" }, null, list, undefined)).toBe("DEFAULT (PROJECT) · b@example.test");
});

test("P5-USAGE-05: a Codex shell is named by the account of its frozen home and numbered per project", () => {
  const list = accounts();
  const shell: ShellLike = { id: "codex-1", projectId: "alpha", harness: "codex", alive: true, codexHome: "/fixture/b", codexAccountId: "account-b" };
  expect(shellSelection(shell, list)).toEqual(profile(list, "account-b"));
  const removed = { ...shell, codexHome: "/fixture/removed", codexAccountLabel: "old@example.test · Old" };
  expect(shellSelection(removed, list)).toEqual({ kind: "account", home: "/fixture/removed", label: "old@example.test · Old" });
  expect(shellSelection({ ...shell, codexHome: null }, list)).toEqual(unknown);
  expect(shellSelection({ ...shell, codexHome: "/x", codexAccountId: null }, list)).toEqual({ kind: "account", home: "/x", label: SYSTEM_LABEL });
  expect(shellSelection({ ...shell, codexHome: "/x" }, list)).toEqual({ kind: "account", home: "/x", label: "Session account" });

  const shells: ShellLike[] = [
    shell, { ...shell, id: "codex-2", codexHome: "/fixture/a" }, { ...shell, id: "codex-3", codexHome: "/fixture/b" },
    { ...shell, id: "beta-1", projectId: "beta" }, { ...shell, id: "dead", codexHome: "/fixture/z", alive: false },
    { id: "claude-1", projectId: "alpha", harness: "claude", alive: true },
  ];
  expect([...codexAccountNumbers(shells)]).toEqual([["codex-1", 2], ["codex-2", 1], ["codex-3", 2]]);
  const focus = usageFocus("shell:codex-2", shells, list);
  expect(focus).toEqual({ kind: "codex", account: profile(list, "account-a"), number: 1 });
  expect(usageFocus("shell:claude-1", shells, list)).toEqual({ kind: "claude_shell", id: "claude-1" });
  expect(usageFocus("panel:files", shells, list)).toEqual({ kind: "other" });
  expect(usageFocus("chat:abc", shells, list)).toEqual({ kind: "other" });
});

test("P5-USAGE-06: figures, aria label and hint keep GPUI's forms", () => {
  expect(limits(quota([20, 40], NOW), false)).toBe("5h 80% left / 7d 60% left");
  expect(limits(quota([20, 40], NOW), true)).toBe("5h 80% / 7d 60% left");
  expect(limits({ windows: [], updatedAtUnix: NOW }, false)).toBe("—");
  expect(limits(quota([120, -5], NOW), false)).toBe("5h 0% left / 7d 100% left");
  expect(limits({ windows: [{ label: "5h", usedPercent: 1 }, { label: "7d", usedPercent: 2 }, { label: "30d", usedPercent: 3 }], updatedAtUnix: NOW }, true)).toBe("5h 99% / 7d 98% left");
  expect([whole(80.5), whole(81.5), whole(79.4), whole(0.2)]).toEqual(["80", "82", "79", "0"]);
  const reads = new Reads();
  reads.put({ ...read("/fixture/a", quota([20, 40], NOW)), error: "Codex is signed out" });
  const segments = reads.segments("current_chat", { kind: "other" }, { kind: "account", home: "/fixture/a", label: TEAM_B });
  expect(usageAria(segments)).toBe(`Usage CODEX* · ${TEAM_B} · 5h 80% left / 7d 60% left`);
  expect(usageItemText("current_chat", segments)).toBe(`CODEX* · ${TEAM_B} · 5h 80% left / 7d 60% left`);
  expect(usageHint(segments)).toBe(`CODEX* · ${TEAM_B} · 5h 80% left / 7d 60% left (Last refresh failed: Codex is signed out)\nClick for account usage`);
});

test("the resources item sums this project's live shells in main.rs's units", () => {
  expect([formatBytes(0), formatBytes(512 * 1024), formatBytes(3 * 1024 * 1024 / 2), formatBytes(2.5 * 1024 ** 3)])
    .toEqual(["0 KiB", "512 KiB", "1.5 MiB", "2.5 GiB"]);
  const shells = [{ id: "a", alive: true, projectId: "p" }, { id: "b", alive: true, projectId: "p" }, { id: "c", alive: false, projectId: "p" }, { id: "d", alive: true, projectId: "q" }];
  const metrics = [{ id: "a", cpuPercent: 1.25, ramBytes: 1024 * 1024 }, { id: "b", cpuPercent: 0.25, ramBytes: 1024 * 1024 }, { id: "c", cpuPercent: 9, ramBytes: 1 }, { id: "d", cpuPercent: 9, ramBytes: 1 }];
  expect(resourcesText(shells, metrics, "p")).toBe("CPU 1.5% RAM 2.0 MiB");
  // Before the first sample, and with no live shell: zeros, never a placeholder.
  expect(resourcesText(shells, [], "p")).toBe("CPU 0.0% RAM 0 KiB");
  expect(resourcesText([], metrics, "p")).toBe("CPU 0.0% RAM 0 KiB");
});
