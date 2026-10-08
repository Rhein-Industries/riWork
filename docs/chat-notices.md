# Chat notices

What a provider or a chat driver wants the user to know that is not part of the
conversation: a usage limit, an API retry, a reconnect, a failed turn, a sign-in
that expired. Drivers emit them as `ItemBody::Notice` items in the chat log; every
client (the Mac chat tab, the phone) shows them in a **banner above the composer**,
not as transcript rows.

## Wire shape

A notice is an ordinary transcript item (`ItemStarted`/`ItemCompleted`, stored in the
chat log, returned by `chat.snapshot` and `chat.events`) whose body is:

```json
{
  "type": "notice",
  "level": "info" | "warning" | "error",
  "text": "This account is close to the weekly usage limit.",
  "kind": "rate_limit:seven_day",
  "resolved": true,
  "dismissed": true,
  "resets_at": 1767225600
}
```

| field | type | presence | meaning |
|---|---|---|---|
| `type` | `"notice"` | always | |
| `level` | string | always | `info`, `warning`, `error`. Unknown → `info`. |
| `text` | string | always | Plain text, may contain `\n`. Complete: never needs `kind` to make sense. |
| `kind` | string | optional | Stable dedupe key, see below. Absent in logs written before it existed and for one-off notices. |
| `resolved` | bool | optional, omitted when false | The cause went away (retry worked, reconnected, limit reset). |
| `resets_at` | integer | optional | When the limit the notice is about resets, Unix **seconds**. |
| `dismissed` | bool | optional, omitted when false | The host has dismissed this sticky notice occurrence; hide its banner, retain it in history. |

All optional fields use `#[serde(default)]` and are skipped when empty, so older
phones (whose decoder ignores unknown keys) keep working and show the text as they
always did; older logs decode with `kind: None`, `resolved: false`, `dismissed: false`.

Updates reuse the item id: a driver that updates a notice in place ("Reconnecting…
2/5" → "3/5") or resolves it re-emits `ItemCompleted` with **the same item id**. A
new occurrence of the same kind gets a new id.

## Kinds

A kind is `name` or `name:qualifier`. Clients must treat unknown kinds as opaque
dedupe keys (show the text, dedupe by the exact string). **New values are only ever
appended to the second table**; existing values are never renamed or repurposed.

### Agreed set (decoded by the phone)

| kind | provider | level | transient | emitted when | resolved when |
|---|---|---|---|---|---|
| `rate_limit:five_hour` | Claude | error (blocking) | yes | `rate_limit_event` with `status: rejected` (new status or reset) for that window | a later event says the window is `allowed` again, or `resets_at` has passed |
| `rate_limit:seven_day` | Claude | as above | yes | as above | as above |
| `rate_limit:seven_day_opus` | Claude | as above | yes | as above | as above |
| `rate_limit:seven_day_sonnet` | Claude | as above | yes | as above | as above |
| `rate_limit:overage` | Claude | as above | yes | as above | as above |
| `api_retry` | Claude | warning | yes | `system/api_retry` (one notice per turn, updated per attempt) | the next assistant output or the turn's result |
| `reconnecting` | Codex | warning | yes | `error` with `willRetry: true` ("Reconnecting… n/5"), updated in place | the next item or the turn completes |
| `effort_refused` | Codex | warning | no | the model does not take the chosen reasoning effort | — |
| `model_fallback` | Codex | warning | no | `model/rerouted`: the server answered with another model | — |
| `oversized_line` | both | warning | no | a provider line over the frame limit was skipped | — |
| `silence` | Claude | warning | yes | no output for the inactivity window during a turn | output arrives again |
| `turn_failed` | both | error | no | a turn failed (Claude failed `result`, Codex `turn/completed` failed) with a message not already shown | — |
| `auth_required` | both | error | no | signed out / key or token invalid: Claude `error: authentication_failed`; Codex `codexErrorInfo: unauthorized`, `account/updated` with neither auth mode nor plan, `account/login/completed` with `success: false`, a token-refresh request | the next successful turn |
| `config_warning` | Codex | warning | no | `configWarning` notification | — |
| `provider_error` | Codex | error | no | `error` notification that will not be retried and is not a sign-in or usage problem | — |
| `provider_warning` | Codex | warning | no | `warning` notification | — |

A `rate_limit:<window>` for a window name not listed (the provider adds one) is still
a usage limit: clients match the `rate_limit:` prefix.

### Appended values

| kind | provider | level | transient | emitted when | resolved when |
|---|---|---|---|---|---|
| `rate_limit:codex` | Codex | error | yes | `error` whose `codexErrorInfo` is `usageLimitExceeded` | the next turn completes |
| `fast_mode` | Claude | warning | yes | Fast mode is off although chosen | Fast mode is on again (re-emitted as info, `resolved`) |
| `setting_refused` | both | warning / error | no | the provider refused a settings change, a compact, or did not start a turn | — |
| `resumed_fresh` | both | info | no | the saved conversation/thread could not be resumed, a new one started | — |
| `undelivered` | Claude | warning | no | messages queued during a restart were lost | — |
| `deprecation` | Codex | info | no | `deprecationNotice` | — |
| `mcp_elicitation` | Codex | warning | no | an MCP server asked for input chats cannot take | — |

A resolved notice is re-emitted with the same id, `resolved: true`, level `info` and a
short text saying it recovered ("The API answered again.", "Reconnected.").

Notices without `kind` are one-offs: each is its own banner.

## Rendering rules (Mac and phone)

1. **Not in the transcript.** In the Normal display, notice items are not transcript
   rows. The Mac's Verbose display still lists every notice in place, as the full
   record of the chat.
2. **One banner per key.** The key is `kind`, or the item id when there is none. For
   each key only the newest notice (latest position in the transcript) counts.
3. **Which keys show.** A key's newest notice is a banner when it is not `resolved`,
   its `resets_at` (if any) is in the future, the user has not dismissed it, and it is
   live: it belongs to the current exchange (at or after the last user message), or
   its kind is sticky — `rate_limit:*` and `auth_required` stay until resolved or
   dismissed, whatever turn they came in. **Warning-level `rate_limit:*` items from
   old logs never show banners**; current drivers emit usage data for warnings.
4. **Stack.** Newest on top. At most two banners are visible; the rest collapse into a
   "n more" control that expands the stack in place.
5. **Dismissal.** Each banner has a close (×) button. Sticky notices use the host
   dismissal below; clients hide `dismissed: true` notices. The Mac hides immediately
   while sending the command. Non-sticky and tab-local notices use view-local
   item-id dismissal; an update stays hidden, a new item id shows again.
6. **Level colours.** info → the theme's muted/accent tone, warning → warning tone,
   error → error tone, as transcript notices used.
7. **Usage limits.** A `rate_limit:*` banner shows its reset time when `resets_at` is
   set ("resets Tue 14:00", in local time) and offers the app's usage view when it has
   one (the Mac opens its Usage panel).
8. **History.** Every notice, dismissed or resolved, stays reachable: on the Mac the
   chat's ⋯ menu lists "Notices (n)", which opens the list above the composer, newest
   first.

The Mac also routes the chat tab's own transient errors (could not reach the host,
could not delete, attachment errors) into the same stack under local keys, so a
repeated error updates its banner instead of stacking, and each clears itself when
its cause is resolved (the host is reachable again, the delete succeeded).

## Dismissal

Send the existing `chat.command` RPC with:

```json
{"chat_id":"UUID","command":{"command":"dismiss_notice","item_id":"notice-item-id"}}
```

On the host socket this is `ChatCommand::DismissNotice { item_id }`; the CLI accepts
`riwork chat command UUID --command-json '{"command":"dismiss_notice","item_id":"notice-item-id"}' --json`.
`item_id` is 1–512 bytes without control characters. The host resolves the item in
that chat's log and refuses unknown ids or non-sticky notices. Dismissal works on
stopped chats without starting a provider. Repeating it is idempotent.

The single host atomically replaces the owner-only
`$RIWORK_HOME/chats/notice-dismissals.json` file. Each entry has a string `key`,
`level` (the dismissed severity) and optional `resets_at`. A key is:

- With a reset: `<provider>:<account scope>|<kind>@<Unix seconds>`, for example
  `claude:sha256:0123456789abcdef0123456789abcdef|rate_limit:seven_day@1760000000` or
  `codex:sha256:fedcba9876543210fedcba9876543210|rate_limit:codex@1760000000`.
- Without a reset: `<provider>:<account scope>|<kind>#<item id>`.

All providers use a canonical `sha256:<32 hex characters>` account scope (the
first 128 bits of SHA-256, namespaced by provider). One resolver combines live
provider frames, the configured credential directory and previously verified
account metadata. Codex prefers an account id (`tokens.account_id` in `auth.json`,
or an id from `account/read`), otherwise email. An email-only server response does
not replace an id supplied by the current credentials. A matching email previously
associated with an id retains that id even if the latest source omits it.

Claude prefers the account id/email in system init/status frames, supplemented by
account fields in `.credentials.json`, `.claude.json` under `CLAUDE_CONFIG_DIR`,
or `~/.claude.json` for the default directory. Account UUID wins over email;
organization UUID is a last resort when neither exists. Access/refresh tokens are
**never canonical identities**. Rotating tokens with unchanged account fields
therefore preserves dismissal. With no account fields, persistent dismissal is
unavailable. No keychain access is required.

The host atomically saves only hashes (canonical scope, account id/email hashes,
and verified alias hashes) in each chat's owner-only `account-identity.json`.
The file contains an `active` identity and hashed `known` account associations;
logout clears `active` while preserving verified email-to-id mappings for later
restarts. History alone never grants an active scope: it must match current
account fields.
Driver initialization, account reads and login changes use the same resolver;
Codex re-reads identity after login changes and rejects stale read responses.
Host startup discovers all saved chats before reconciling dismissals. Stopped-chat
snapshots use the persisted canonical metadata and verified alias mapping, without
writing files. Raw identifiers, emails and tokens are never persisted here.

Migration is one-way and atomic in `notice-dismissals.json`: old email/other-source
fingerprints and the old Claude token fingerprint are recomputed from the current
login's credentials and mapped to its canonical scope. Verified email-to-id
upgrades migrate the existing keys and retain dismissal; ids never migrate back
to email. Conflicting account identities do not inherit each other's aliases.
Previously used managed Codex binding scopes migrate when that binding's canonical
account is identified. A `default` key migrates only when exactly one canonical
identity is known for its provider in this home. Ambiguous/unidentified defaults
are dropped on pruning; expired entries are also pruned. Discovery precedes this
pruning so a uniquely identifiable legacy login can migrate. Duplicate migrated
occurrences retain their highest dismissed severity. Repeating migration is a no-op.

Keys cover all chats of that provider and account with the same reset, including
notices with different ids. A changed reset is a new occurrence; `auth_required`
with a new id shows again. A dismissal suppresses only its recorded severity or
lower. A higher-level notice ends the lower-level dismissal and removes its key.
Project changes, tab close/reopen, window reloads and host/app restarts preserve it.

`chat.snapshot` additionally includes optional `dismissed_notices: [key, ...]` for
active dismissals in that chat's account scope (omitted when empty; default empty
when decoding old snapshots). The per-item `dismissed` flag remains authoritative
for banner rendering. Reading a snapshot never writes or prunes the dismissal file.

Entries with `resets_at <= now` no longer match and are pruned on host startup,
dismissal commands and incoming item events. The host re-emits matching existing
items as `ItemCompleted` with the same id and `dismissed: true`, across all chats,
and marks matching incoming notices before logging/broadcasting them. Startup
reconciles stored notice bodies with the dismissal file, including notices outside
the driver's bounded resume tail. Consequently `chat.snapshot` contains the flag
and `chat.events` delivers live updates. Clients must hide flagged banners and
keep their history; older clients can ignore this additive field. Non-sticky
notices remain view-local on the Mac. The iOS rendering change is separate work.

## Rate windows (usage data, not notice banners)

Drivers store and replay this control event to native host subscribers (including
the Mac). Remote clients must opt in independently on **both** `chat.snapshot` and
`chat.events` requests by adding `"features": ["rate_limits"]`. Without that feature,
snapshots omit the control. Ordinary remote event pages filter it while retaining
`next`/`more`, so cursors advance over omitted events. Legacy Swift validates
consecutive sequence numbers in complete/bounded replay: in those modes the relay
replaces unsupported quota events with the already-supported no-op
`{"event":"question_resolved","request_id":""}` at the same sequence number.
No actionable host question has an empty request id. Feature names are
case-sensitive; the optional array accepts at most 16 nonempty strings of at most
64 bytes. Other feature names are ignored. An initial snapshot includes the
control only when opted in and at least one window is known. Later empty live
controls are delivered to opted-in clients to clear their quota state.

The disk-reading CLI opt-in is `riwork chat snapshot UUID --rate-limits --json`;
the native host subscription and native CLI event reader retain all events. The
Mac reads that native subscription directly and needs no UI change. Old phones'
strict event enums therefore never see the new variant. Swift's keyed notice and
snapshot decoders ignore unknown fields: the additive `dismissed` and
`dismissed_notices` fields require no event capability or simultaneous phone
release (older phones do not yet honor dismissal).

Opted-in initial snapshot controls and live event pages use this shape:

```json
{
  "event": "rate_limits",
  "windows": [
    {"id":"five_hour","label":"5h","used_percent":30.0,"resets_at":1767225600,"warn_at":70.0},
    {"id":"seven_day","label":"weekly","used_percent":87.0,"resets_at":1767300000,"warn_at":70.0}
  ]
}
```

| RateWindow field | type | meaning |
|---|---|---|
| `id` | string | Claude window name (`five_hour`, `seven_day`, `seven_day_opus`, `seven_day_sonnet`, `overage`); Codex `primary` or `secondary`. |
| `label` | string | Claude: `5h`, `weekly`, `weekly Opus`, `weekly Sonnet`, `extra usage`. Codex: 300 minutes → `5h`, 10080 → `weekly`, otherwise whole days → `{d}d`, other durations → `{h}h`. |
| `used_percent` | number | 0–100, including windows below the warning threshold. Claude utilization × 100; Codex usedPercent. |
| `resets_at` | integer, optional | Unix seconds; milliseconds from either provider are normalized. Omitted when unknown. |
| `warn_at` | number | Claude 70; Codex 75, except 50 for a 300-minute window on Plus or Team. |

`ChatEvent::RateLimits { windows }` replaces `Transcript.rate_limits` wholesale,
including an empty array. No such event in an older log means no known windows.
Clients select a non-expired window at or above its own `warn_at` for a compact usage
chip, prefer the highest percentage, use warning tone and bold at 90% or above,
and list all windows in its tooltip. The chip is not dismissable. Do not turn these
windows or `allowed_warning` into notice banners, even at 30%: quota data is emitted
whenever it changes, independently of thresholds. Expired windows are retained as
provider data; clients hide them when `resets_at <= now`.

Claude merges `unifiedWindows` and the triggering window's utilization/reset into
its known windows. `rejected` (including overage/spend limits) alone creates the
error notice; `allowed` resolves it. Restored warning notices are still resolvable
for log compatibility, but must not banner.

Codex reads `account/rateLimits/read` once after session initialization and merges
non-null fields from `account/rateLimits/updated`, including nested window fields,
planType and rateLimitReachedType. A `rateLimitsByLimitId` response prefers the
previously selected bucket, then the account-wide `codex` bucket, legacy `rateLimits`,
or the first named bucket. The event contains primary/secondary once, with no legacy
mirror duplicates. `usageLimitExceeded` remains `rate_limit:codex`, at error level;
its notice uses the earliest reset among exhausted (100%) windows, or the earliest
known reset when none is exhausted. The provider read is background work: older
servers can refuse it without preventing chat startup.

Once selected, sparse updates for other named Codex buckets do not change that
bucket's windows. A background quota read is ignored if any quota notification
arrived after the read was issued. For an existing blocking notice, only exhausted
windows can move its reset; recovery resolves the same item while retaining the
original reset and dismissal. The soonest-known-reset fallback applies only when
creating a new `usageLimitExceeded` notice.
