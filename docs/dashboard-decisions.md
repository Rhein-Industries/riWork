# Dashboard decisions and observed activity

`riwork dashboard --json` and the remote `dashboard.get` reply extend version 1
with optional `decisions`, `decisions_total`, `decision_sources`,
`recent_activity`, `recent_activity_total`, and `intelligence` fields. Existing
dashboard rows and older replies remain supported.

Decisions come from Claude, Codex, and Muse's native chat events. Each card has
the exact chat ID, provider request ID, session/project, and the original
`approval` or `question` payload. Approval choices and question option labels
retain their original values, including punctuation. Model summaries cannot
create a decision or choose an answer.

```json
{
  "id": "CHAT:approval:REQUEST",
  "chat_id": "CHAT",
  "request_id": "REQUEST",
  "kind": "approval",
  "session": {"target": {"type": "chat", "id": "CHAT"}, "agent": "Claude", "is_chat": true},
  "observed_at": 1790000000,
  "source": {"chat_id": "CHAT", "event_seq": 17, "event_id": "chat:CHAT:seq:17"},
  "approval": {"request_id": "REQUEST", "kind": "command", "title": "git push", "detail": "Publish changes", "choices": ["accept", "decline"]}
}
```

Question cards use `kind: "question"` and `question: {request_id, questions}`;
each question retains its header, text, options, descriptions, and
`multi_select`. Answer through the existing `chat.command` method on the same
desktop and exact chat/request IDs. Approvals use `approve`; questions use
`answer`, one array of original option labels or free text for each question.
Only chats currently reported as waiting expose answerable decisions.

The desktop Global Dashboard provides a review dialog. Every send rechecks the
running chat host, the chat's waiting state, and a fresh full snapshot. A
changed or resolved request cannot be answered from its old card. Sending uses
the existing provider driver and does not start a chat host or resume an ended
provider. The review stays in a sent state until closed; disappearance from the
inbox follows the provider's resolution event.

`decision_sources` describes coverage: `read`, `partial`, or `unavailable`, plus
`sampled_at`, `chats_read`, `chats_complete`, and `chats_total`. A bounded log
suffix can prove requests seen within it, but does not prove that older
requests are absent. Complete coverage requires the log's beginning or a turn
completion that clears preceding requests. Malformed records, sequence gaps,
unreadable logs, and oversized requests make coverage partial. Oversized option
labels are omitted as a whole request rather than truncated into a different
answer. Terminal agents retain their existing Needs you entries; these are not
invented chat decisions.

Reads inspect at most 128 chats, prioritizing waiting and failed sessions, at
most 2 MiB per log and 12 MiB of replay in total. Unchanged logs use a private
cache in `RIWORK_HOME/dashboard-observations` (directory 0700, files 0600),
revalidated against log identity, size, and modification time. The cache stores
only bounded observations and never answers a request. It can be deleted to
force another read. Empty logs do not create cache files, and a read with a
known chat list removes the entries of deleted chats. A log that cannot be read
(a malformed record, a sequence gap, more than 40 open requests) is remembered as
unreadable and not replayed again until it changes. Cards are listed in the order
the provider asked; a request asked again with a payload too large to keep loses
its older payload rather than staying answerable from it.

`recent_activity` describes observed messages, commands, tools, edits, plans,
notices, worker/subagent metadata, and turn outcomes. Each row has the same
source identity, up to 300 characters of concrete text, its reported status,
`observed_at`, and `log_updated_at`. The latter is explicitly a file
modification time, not a claimed event timestamp. Both new lists send at most
40 rows, with totals indicating the number observed before truncation. Decision
cards also stop at 64 KiB of JSON, and the whole reply before `intelligence` at
88 KiB, activity rows being cut first, so a reply without compression still fits
the connector's 128 KiB; the desktop's own inbox is not limited this way.

Optional Haiku intelligence summarizes these event sources separately. Its
highlights and followups refer to observed event IDs. Its state, generated
time, and stale flag describe the summary's freshness; it supplies no decision
controls and never alters provider state.
