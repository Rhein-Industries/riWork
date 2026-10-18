# Haiku dashboard overview

The Global Dashboard and iPhone host dashboards can summarize observed activity
with the Anthropic Messages API. Enable **Settings → Agent sessions → Haiku
dashboard summaries**, and save an Anthropic API key in the existing Keychain
controls. The setting defaults off; enabling it does not change Claude chats'
subscription/API billing choice.

```sh
riwork settings set dashboard_haiku_summaries true
```

The helper uses `claude-haiku-5-5`, low effort, thinking off (it would count against
the output limit) and structured JSON output. It receives a bounded selection of
observed event descriptions, request titles, session labels, totals and coverage.
It receives no tools or action controls. Highlights and suggested followups must
cite event IDs present in that request;
unknown references or malformed output reject the result. AI summaries never
create approval buttons or answer provider requests.

The API call has an eight-second deadline, 1,500 output-token limit and 16 KiB
evidence limit. A summary is of the observed events and whether every chat log was
read: polling timestamps, live counts and the events' order do not change it, so
unchanged activity reuses its summary and the app and the phone share one. A private
cross-process cache permits at most one attempt per minute and 120 attempts per UTC
day, reserving the request budget before sending. Evidence that keeps failing is
retried after a doubling wait, up to about an hour, rather than each minute. A
missing or unreadable key is checked again each minute and spends no budget. The
saved key never enters arguments, logs or cache files.

The phone's `dashboard.get` never waits on Keychain or the API: it returns the saved
summary, and when a new one is due, starts a detached `riwork dashboard
--refresh-summary` process for a later read to show.

Event descriptions are the dashboard's own observations, so they can include
commands, file paths and agent messages from Claude, Codex and Muse chats. With the
setting on, that text is sent to Anthropic.

API usage is billed to the saved Anthropic key. No request is sent while the
setting is off. Missing credentials, exhausted helper budget and API failures
leave the observed dashboard and actual decisions usable; a previous summary
is marked saved/stale when its evidence changes or a refresh fails. Changing
the setting does not start agents or migrate conversations.

Each host produces its own summary and source references. The phone combines
host dashboards locally, keeping their identities, freshness and incomplete
figures visible. It uses the active host's existing connection and temporary
read-only connections for other established pairings. Single-use pending
pairing invites require the normal explicit first connection.

The direct API transport is tested with injected responses and an in-memory key
store. Tests never read the user's Keychain or make billable requests.

Anthropic references: [Haiku 5.5](https://platform.claude.com/docs/en/models/haiku-5-5/overview)
and [structured outputs](https://platform.claude.com/docs/en/build-with-claude/structured-outputs).
