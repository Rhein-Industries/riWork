# Codex subscription balancing

**Settings → Codex accounts → Balance Codex subscriptions** is off by default.
The same setting is available through the CLI:

```sh
riwork settings set codex_subscription_balancing true
```

When enabled, a **new automatic Codex session** chooses among the available saved
Codex accounts that RiWork discovered through Orca's public account list. This
applies to chat, terminal, orchestrator and `codex` launches in an unpinned plain
shell, including CLI and paired-device requests that use those common launch paths.
Refresh accounts in Settings after adding a saved account.

For each account, RiWork takes the smallest remaining percentage across its reported
quota windows. It chooses the account with the largest such percentage: 60% short
and 40% weekly beats 95% short and 10% weekly. A tie prefers the account selected in
Settings, then uses a stable account ID order. These are provider-reported
percentages, not estimates of how many tokens different subscription plans buy.

Only successful, nonempty Codex quota reports no more than two minutes old qualify.
Exhausted windows, invalid percentages, elapsed reset timestamps, future-dated
reports and failed latest reads exclude an account. An elapsed reset is never
assumed to restore quota. The system-default home is not a balancing candidate:
choose it explicitly or turn balancing off to use it automatically.

Automatic launches refresh quota through Codex's public app-server account methods.
No conversation or model request is started to measure quota. The launch shares
fresh per-account reports across app, CLI and chat host processes; successful
reports are refreshed after one minute, failed reports retried after fifteen
seconds. At most four readers run in parallel with a ten-second total refresh
budget, and no read starts with less than two seconds of it left. Accounts whose data
cannot be read within that budget remain unknown and are excluded; a read cut short by
the budget keeps the account's previous report, which still qualifies only while fresh.
The account is chosen before RiWork takes its session-registry and chat-creation locks,
so other sessions are never held up while quota is read. Initial account discovery, if
needed, has Orca's existing separate five-second bound. UI catalogs only inspect saved
reports; they never start quota readers. While balancing is on, New Chat still offers
Codex when the account selected in Settings is unusable: that account is only the
tie-break. Desktop quota refreshes feed the same cache.

If no saved subscription has fresh usable quota, the launch returns an explanation
naming each account and why it was passed over (not read in time, read failed, report
too old, a limit used up or reset since its report). It does not silently use a known
exhausted account. Retry, name an account explicitly with `--account`, pin the
project's account, or disable balancing. A used-up account is read again after a
minute; one with a limit that has reset since its report, after fifteen seconds.

In a plain shell, `codex resume <id>` and `codex fork <id>` run under the saved account
whose Codex home holds that thread, when the shell's latest account does not: each new
`codex` there may have been balanced onto another account. `--last` and the picker use
the latest account.

Identity precedence stays deliberate:

- An explicit `--account` or chat account parameter chooses that exact account.
- A project's **System default** or saved account choice stays pinned.
- A project that **inherits the app default**, or a session with no project,
  uses balancing while the setting is enabled.
- Existing sessions, resumed/forked conversations and account-bound child commands
  retain their saved account. Balancing does not migrate a running conversation or
  retry its work under another identity.
- Saved automations retain their pinned account, including a system-default account.
  A new-chat automation chooses and pins its account when the automation is created;
  subsequent occurrences keep that account until the automation is explicitly edited.

The configured app account is preserved when balancing is enabled or disabled.
The desktop account status says **SUBSCRIPTIONS (AUTO)** for an inheriting project;
focused sessions still report their own account and quota. This feature balances
Codex subscriptions only. Claude and Muse continue to use their existing sign-ins.
Quota samples contain account-home paths and numeric metrics in private files under
`RIWORK_HOME/codex-subscription-usage`; credentials, account labels and raw server
errors are not saved.
