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
| `rate_limit:five_hour` | Claude | warning (close to) / error (reached) | yes | `rate_limit_event` whose status changed for that window | a later event says the window is `allowed` again, or `resets_at` has passed |
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
   dismissed, whatever turn they came in.
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
`$RIWORK_HOME/chats/notice-dismissals.json` file. Each entry is keyed by **provider +
exact kind + resets_at**, or **provider + exact kind + item id** when no reset is
known. Thus a usage-window dismissal covers all chats of that provider with the
same reset, including new chats and notices with different ids. A later reset is
a new occurrence and shows again; `auth_required` with a new id shows again.
Project changes, closing/reopening tabs, window reloads and host/app restarts do
not undo dismissal. Keys are provider-wide, including chats configured with different
accounts of the same provider, rather than scoped by chat or project.

Entries with `resets_at <= now` no longer match and are pruned on host startup,
dismissal commands and incoming item events. The host re-emits matching existing
items as `ItemCompleted` with the same id and `dismissed: true`, across all chats,
and marks matching incoming notices before logging/broadcasting them. Startup
reconciles stored notice bodies with the dismissal file, including notices outside
the driver's bounded resume tail. Consequently `chat.snapshot` contains the flag
and `chat.events` delivers live updates. Clients must hide flagged banners and
keep their history; older clients can ignore this additive field. Non-sticky
notices remain view-local on the Mac. The iOS rendering change is separate work.
