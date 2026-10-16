# Transcript grouping v2

This is the normative, language-independent contract for the pure Rust and Swift
projections. Source: proposal rev. 5, §§2.2–2.4 and §3. Rust implements it in
`crates/riwork-engine/src/chat/projection.rs`. The tests execute every JSON file
in this directory, including the shared shell classification table. A Swift
implementation must use these same inputs and exact expected JSON, rather than
reverse-engineering the Rust implementation. All times below are Unix milliseconds.

## Input schema and defaults

A projection receives `items`, `item_sequences`, `requests`, and `options`.

- An item has required `id` and `body`; optional `turn_id`, `started_at`,
  `ended_at`, and `presentation`; `status` defaults to `in_progress`. Its body is
  the existing tagged engine body (`type`). No body or image-source variant was
  added for this feature. Tool-call `input` is arbitrary JSON.
- Presentation has optional `phase` (`commentary` or `final`), `semantics` and
  `parent_id`, plus `images` and `attachments` arrays (empty by default).
- Semantics has `kind` (default/unknown word → `other`), optional `target`,
  `range:{start,end?}` (inclusive line numbers), `match_count`, `excerpt`,
  `worker:{role?,objective?,child_count?}`, and `produced:[{path,line?}]` (empty).
  Kinds are `shell`, `read`, `search`, `list`, `web_search`, `web_fetch`, `mcp`,
  `subagent`, `other`. Wrong-typed or `{"elided":true}` optional hints disappear;
  malformed elements of metadata arrays are discarded individually. Unknown
  metadata must not discard the item. Produced paths require recorded provenance;
  neither output prose nor filenames mentioned in assistant text create artifacts.
- `item_sequences` maps an item ID to the sequence that first introduced it,
  never the sequence of its latest update. It defaults to `{}`. Without an entry,
  ordering inside a projected sibling list is `sibling_index * 2 + 1`; the outer
  turn-block start similarly uses its index in the unique item array. For an
  absent next-turn sequence, the request-assignment upper bound is `u64::MAX`.
  Supply real orders whenever control-event ordering is known.
- A request record has `request_id`, `kind` (`approval` or `question`), `title`,
  `requested_seq`, and optional `turn_id`, `item_id`, `resolved_seq`, `resolution`,
  `answer_summary`. Records are ordered by `requested_seq`. Repeated requests keep
  their original sequence and update their title/association. A resolution removes
  the pending dock request but retains this record. A turn completion closes
  unresolved records from that turn (or unassociated records) as `cancel`.
- Options default to `mode:"normal"`, no `now_ms`/`running_turn_id`, empty
  `turn_timing`, `disclosure:{overrides:{},bulk:null}`, `provisional:false`, and
  `elided_items:[]`. Mode may be `verbose`. `turn_timing` maps a turn ID to
  `{started_at?,ended_at?}`. `elided_items` is the set of item IDs marked by
  response-only relay elision; the adapter extracts it from snapshot `elided` or
  live `item_elided` metadata. It is not persisted into item semantics.

Runtime wire decoders tolerate future keys. The **fixture input schema is strict**:
unknown case/action/page/options/timing/disclosure/item/body/presentation/semantic/
request keys are errors, including misspellings such as `running_turn` or `started`.
The arbitrary tool input object is the exception. A fixture may include top-level
item `elided` response metadata; `options.elided_items` explicitly supplies that
metadata to the pure projection. Optional malformed metadata examples remain valid.

## Enrichment before projection

Provider-supplied semantic kinds and known fields win. For old logs, classify the
incoming body before inheriting start-time facts. A populated completion replaces
an empty/stale start target. If classification yields nothing useful, inherit the
previous provider-only classification; if kinds match, fill only missing fields
from the start. The same merge applies whether replay already enriched the event
or the fold received raw events. Explicit `other` remains explicit; an inferred
unknown-tool `other` stays absent so it cannot masquerade as a provider decision.

Legacy rules, applied only when semantics are absent:

- Command → Shell; WebSearch → WebSearch. Empty/whitespace command or query yields
  no target (the category remains known).
- A tool with a server, or an `mcp__` prefix, is MCP. Otherwise normalize its name
  to lowercase and remove `_`/`-`, then use these aliases:

| Kind | Tool aliases |
|---|---|
| Read | read, readfile, fileread |
| Search | grep, search, searchfiles, filesearch, ripgrep |
| List | glob, ls, list, listfiles, listdirectory |
| WebSearch | websearch |
| WebFetch | webfetch, fetchurl |
| Shell | bash, shell, exec, execcommand, terminal, command |
| Subagent | task, agent, subagent, collabagenttoolcall |

Unknown names have no recoverable semantic distinction. Role-named legacy Muse
workers cannot be guessed. For each target, take the first nonempty string among:
Read `file_path/path/filename/file`; Search `pattern/query/search`; List
`pattern/path/directory`; WebSearch `query/search_term`; WebFetch `url/uri`;
Shell `command/cmd`; Subagent `description/objective/prompt`; MCP/Other
`file_path/path/query/url/command`. Read ranges use `offset` then `start_line`,
with `limit`; if either is supplied, start defaults to 1 and is clamped to at
least 1, and positive limit gives `end=start+limit-1` unless it overflows.
Worker role uses `subagent_type/role/agent_type`, objective uses
`description/objective/prompt`, and child count is supplied only when known.

Output evidence may fill missing facts without changing classification. Search
match count is the number of lines shaped `nonempty_path:unsigned_line:content`,
only when at least one such line exists. Read/fetch excerpt is the first nonempty
output line, at most 240 Unicode characters. Missing/elided/unparseable output
creates neither a zero match count nor an invented excerpt. Stored output and
sequence numbers never change on a read.

## Item identity, row kinds, and hierarchy

Deduplicate by item ID: keep first arrival position, latest item contents. Traverse
contiguous blocks with equal nonempty `turn_id`. An item without a turn ID is its
own legacy block, named `legacy:<item_id>`; do not merge unrelated legacy items.

Body kinds take precedence over hints: user → `user`; assistant commentary phase
→ `commentary`; other assistant text (including missing phase) → `final`;
reasoning → `reasoning`; **every command → `shell`, regardless of semantics.kind**;
edits → `edit`; plan/todo/compaction/notice → their matching row kinds. Other tool
bodies use semantic kind, defaulting to `other`. In mockup vocabulary,
`thought/comment/web/fetch/agent` mean
`reasoning/commentary/web_search/web_fetch/subagent` here.

A known `parent_id` nests an item under an existing Subagent in that block. A
missing parent, self-parent, non-subagent parent, or cycle leaves it as a root row.
Nested lists use the same projection rules and original item orders. A request
whose item is a nested child is inserted into that child's sibling list; otherwise
it is a root boundary. Beyond 64 recursive levels, descendants are retained as
flat rows rather than recursing indefinitely. A subagent is always its own row.

Relay-elided items carry `Row.elided=true`; clients show “shown on your Mac” and
no output preview. A relay's assistant-text placeholder with known tool semantics
is projected as that tool rather than a final answer. Known read/search/etc. kinds
may still contribute to a group and to activity counts. An elided shell whose
script is unavailable is its own row: unknown script is never read-only. Without
a known discriminator the placeholder is `other`. This requires no new ItemBody.

Leaf IDs equal item IDs. Header titles are User, Answer, Commentary, Thought, Ran,
Read, Searched, Listed, Searched the web, Fetched, Used MCP, Agent, Used tool,
Edited, Plan, Todo, Context compacted, Notice. Target comes from semantics, then
command/query/single edited path; **a Command's target always comes from its body**.
Bodies/output/diffs remain on the original item for lookup, never in a projected
header. A running short label uses reading/searching/listing/fetching/thinking/
agent/editing/running/planning/updating todo/using tool, followed by a known target;
edits may use their first path and generic tool calls their tool name as fallback.

## Verbose grouping

Eligible runs contain Read, Search, List, WebSearch, WebFetch, MCP, Other, Reasoning,
and read-only shell calls. Any Command body must pass `is_read_only_shell` and
have no approval record associated with its item, regardless of hints, exit code,
or running state. Shell-shaped generic tool calls use their known command target.
The exact accept/reject examples are executable in `shell.json`.

Shell tokenization respects single/double quotes and escapes. Substitution outside single quotes, redirection, parentheses, background jobs, unterminated quotes,
empty/trailing commands and unsupported wrappers are rejected. Every pipeline,
`&&`, `||`, semicolon or newline segment must pass the short allow-list:
`ls cat head tail grep rg find`, `git status|log|diff|show`, and `sed -n` with
only numeric/comma/`$` print addresses. Reject find execution/deletion/output flags,
rg preprocessor/hostname executables, git output/external-diff/textconv flags,
absolute executable paths, environment prefixes and git global-option wrappers.
This is a display classifier, not an authorization mechanism.

An edit, approval/question record, final/user text, subagent, plan/todo,
compaction, notice or turn boundary flushes a run. Failure alone never flushes an
eligible run. Commentary following a member travels with the run but is not a
call; show it adjacent to the header in Verbose. Commentary before a run remains
its own row. A single member (including a lone Thought) remains a leaf with its
comments in sequence. Two or more members become a group; comments are separate
from its children. All calls/groups start closed, including failures and streaming.

Group ID: `g:<turn_id>:<first_member_id>`. Count buckets in this order:

| Row kind | Singular header | Plural header |
|---|---|---|
| Read | Read 1 file | Read N files |
| Search | Searched 1 pattern | Searched N patterns |
| List | Listed 1 directory | Listed N directories |
| WebSearch | Searched the web 1 time | Searched the web N times |
| WebFetch | Fetched 1 page | Fetched N pages |
| MCP | Used MCP 1 time | Used MCP N times |
| Shell | Ran 1 command | Ran N commands |
| Reasoning | Thought 1 time | Thought N times |
| Other | Used 1 tool | Used N tools |

Join nonzero buckets with ` · `, at most three, followed by ` · +N more` for the
remaining **bucket** count. Group duration sums known member durations; absent
all timing means no duration. Running is true if any member runs; the **last
running member in arrival order** supplies `current_label` (also for activities).
Failures add a count badge, never disclosure or output. A failed item means status
`failed` or Command exit code != 0; declined/interrupted alone are not failures.

## Normal activity and timing

Normal keeps user/final rows visible and emits `activity:<turn_id>` between them
for every named turn, or for a legacy block with work. Running turns also get an
activity without calls. Its children are the grouped Verbose projection, with
commentary/notices hidden and requests/compaction/finals outside the disclosure.
Artifacts are lifted to one strip and removed from disclosed child rows; final
and user images stay on their message rows. Verbose artifacts stay on their owning
rows. Known descendant items are counted once by stable ID; a worker with known
children is metadata rather than another call.

Completed title is `Worked`, optional duration, then up to four nonzero buckets:
unique edited paths (`file edited/files edited`), shell items (`command/commands`),
Read/Search/List/WebSearch/WebFetch (`read/reads`), and MCP/Other/childless workers.
The last bucket is `MCP call/MCP calls` for MCP-only work, otherwise
`tool call/tool calls`. User/final/commentary/request records are never tool
counts. Reasoning/plan/todo remain work but have no count bucket.

Running title is `Working`, optional duration, then ` · <current_label>` when
known. It contains **no spinner or disclosure glyph**; clients draw from `running`
and `open`. No current running item means no invented label. Duration formatting
floors milliseconds to seconds: `Ns`, or `Mm Ns` from 60 seconds upward.

The host stamps `TurnStarted.started_at` and `TurnCompleted.ended_at`; the fold
retains `Transcript.turn_timing`. `project_transcript` fills timing options from
that map unless the caller supplied a turn override. Use known turn start/end for
completed wall time and start/now for running wall time, avoiding parallel-call
overcounting and including model time between calls. Without a complete turn
clock, sum available call durations (including known running elapsed times).
Item duration uses start/end, or start/now while in progress; negative or missing
intervals are unknown. Unknown durations never become zero. Actual item completion
is stamped by the host; synthetic turn closure can leave its item end unknown.

## Requests and ordering

Requests are boundaries at **requested**, not resolved, sequence. Associate by
turn, or by known item; a record without either association is placed inside the
block containing its sequence when possible. Request-only earlier turns remain
before later item blocks. Still-unmatched records are emitted by requested order,
including a controls-only page. Request row ID is `request:<request_id>`.

Titles are `<prefix>: <title>`, optionally ` · <answer_summary>` when nonempty:
accept/accept_for_session → Allowed; decline → Declined; cancel → Cancelled;
answered → Answered; another resolved word → Resolved; no resolution and a known
resolved sequence → Resolved; otherwise Question pending or Approval pending.
Elided approvals may have unknown decision but known resolution sequence: close
the history record with nullable resolution, never recast it as cancelled later.

## Disclosure, anchors and selection

`toggle(id)` flips the **projected** open state into the manual override map.
Overrides win over bulk. With no override, bulk applies to all calls/groups in
Verbose; in Normal it applies only to activity rows. User/final/commentary/
compaction/notice are not disclosures. Mode change clears overrides and bulk.
Interactive Expand/Collapse all uses `reset_bulk_for_rows`: it clears overrides
and bulk, then writes explicit states for the rows present at invocation. Later
streaming rows stay closed. The blanket `bulk` input and `reset_bulk` remain for
legacy callers. Normal Expand all opens activities with all individual rows
closed. Closing/reopening an activity preserves manual child overrides.
Streaming/failure never automatically opens anything.

Anchors store item IDs. A closed activity/group/subagent maps its descendants to
that visible container; an open one descends to the nearest visible row. Group
comments remain adjacent and visible with a closed group. `remap_disclosures`
remaps an old group override after backfill using its first known leaf as anchor.
Existing new IDs win; multiple old groups merging into one keep it open if any
old override was open. An opened standalone leaf that gains a new group opens
that group and retains its leaf override; an existing explicit group override
wins. Bulk is unchanged. Selection exports each visible row's
`title`, plus `" " + target` when a target is present; a closed container is one
leaf, while an open container adds visible child leaves. Adjacent comments are
selection leaves even when the group is closed. No spinner glyph enters selection.

## Page boundaries and cursors

The pure `turn_aligned_page(items,start,end,max_items?)` clamps the index range,
expands its ends to adjacent same-turn blocks, then applies an optional hard cap
from the oldest side. An empty range remains empty. A capped turn fragment is
`provisional`; absent turn IDs do not imply one giant legacy turn. Fixtures with a
page must project **only** `items[window.start..window.end]`, with the resulting
provisional flag, never the full source list.

Real snapshots additionally know turn-start **sequences**, not just adjacent
blocks. Extend a selected cut to its turn start, then keep extending while any
preceding row has `order >= page_start`; this retains no-turn or other-turn
interlopers. Set `before=min(page_start,first_kept_order)`. The invariant through
byte/transport trimming is: **no dropped row has order >= the returned before**.
If limits force a partial turn, use an exact safe item cursor and mark provisional.
Snapshots carry `turn_timing` (ms) and `turn_starts` (event sequences), filtered to
retained page turns, plus request records in the page's sequence interval. These
maps have different units. Legacy snapshots without sequence maps fall back to
known item/request boundaries while preserving the same losslessness invariant.
A page-global provisional option currently marks projected groups/activities;
clients remap item anchors and group overrides when older members arrive.

## Executable fixture format

Every file is `{ "spec_version":2, "cases":[...] }`, or contains
`"shell_cases":[{"command":"...","allowed":true|false}]`; either array can be
omitted but a file must contain at least one case. Each projection case has:

- Required `name`, `items`, `expected_rows`.
- Optional `item_sequences`, `requests`, `options`, `anchors` (item → visible row),
  `expected_selection` (ordered strings), `page`, and `actions`.
- Page: `start`, `end`, optional `max_items`, and `expected:{start,end,provisional}`.
- Action: `action` (`toggle/bulk/mode/backfill`), optional `id/open/mode/items`,
  required `expected_open_ids`, optional `anchors` and exact `expected_rows`.
  Backfill replaces items, clears provisional, projects the replacement, remaps
  manual group state, then projects again. Every action validates open IDs and
  anchors; supplied row expectations validate the whole result after that action.

Expected rows are compared as raw JSON against serialized output: **no tolerant
Row decoder supplies defaults or removes misspelled expected keys**. Required
serialized row keys are `id`, `kind`, `title`, `open`, `failed_count`, `running`,
`provisional`, `elided`. Optional `target/duration_ms/current_label/request_id`
are omitted when unknown. `children/comments/artifacts` are omitted when empty.
`ArtifactRef` has required `item_id` plus a `path:{path,line?}` or `image_index`.
All expected scalar defaults must be written explicitly. Input arrays may stay
minimal, but output expectations are exact. The shared shell table is normative
and must run in both implementations; Swift Core already decodes all shared item
and request inputs, while Swift projection execution belongs to phase 3.

Public helpers: `project_transcript`, `project`, `turn_aligned_page`,
`refresh_timing`, `visible_anchor`, `remap_disclosures`, `selection_leaves`,
`DisclosureState`, `ProjectionCache`. `refresh_timing` advances running row durations and activity
titles in place from transcript clocks without cloning item bodies or regrouping;
structural/status changes still require projection.
`ProjectionCache::project` shares the projection's global request assignment and
per-turn rendering, reusing unchanged turn rows without cloning historical item
bodies. Callers supply turns invalidated by item header/status changes; identity,
sequence, request, mode, elision and clock changes are also checked by the cache.
Codex child-thread capture, real Muse patch diffs, media handles/thumbnails/output
retrieval, and views remain later phases. No collapsed row exposes output/errors.
