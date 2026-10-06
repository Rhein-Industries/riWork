# GPUI Kit chat migration contract

Task 96f8a1ee-c888-4ead-91bf-af5f09838712; worktree 9c106443-4122-4841-a2c7-4e525648e957.
Baseline: cd7a559d8dbb59dbd3c5593edd5558fbd55a1e7b. Status: in_progress.

## Ownership and sequencing

The foundation owner in `riWork-feat-gpui-kit-foundation` supplies a pinned GPUI Kit dependency, initialization, theme bridge and shared input adapter. This worktree owns `src/chat/`, `src/chat_view.rs` and `src/chat_view/`. Until that handoff, do not edit Cargo, shared input, init, project_settings or main.rs. Backend attachment work can land independently. The full UI migration remains required after integration; backend completion does not complete this task.

No applicable AGENTS.md was found in the checkout or its parent chain. Native host PID 78989, inherited RIWORK_HOME and production sessions remain untouched. All runtime verification uses fake provider processes/private fixture homes. Cargo uses `/tmp/riwork-chat-96f8a1ee-target` and installed Zig 0.16.0. No GUI verification until parent grants GUI ownership; desktop inspection and interaction must use RiWork Cua.ai Driver MCP, never a replacement provider.

## Baseline integration inventory (before migration)

- `chat_view.rs`: `ChatView::blank/open/create` owns a single FocusHandle, Field routing, legacy composer Input, model Input and Vec<Input> answers. Feed creation/subscription replays host history. `accept` batches events, failures and link changes; ChatModel tracks gapless sequence and completed turns. `ready_inputs` currently sizes answers by prompt count, not request identity: migration must key them by request id and prompt id/index to avoid copying an answer to a later request with the same count.
- `panels.rs::editor`: constructs StyledText, inserts a fake caret character, paints selection, attaches a canvas ElementInputHandler and only focuses on mouse-down. ChatView's `character_index_for_point` always returns end-of-text and `bounds_for_range` returns the entire box. These shortcuts explain missing click placement/drag selection and IME geometry. Replace the whole editor implementation with real input entities; do not retain a second entity input handler.
- Composer: multiline replacement; Enter sends, Shift/Alt-Enter inserts newline; IME-marked text owns Return. Unmodified arrows/Home/End manually move by logical lines. Cmd-V pastes only clipboard text. Cmd-C falls back to transcript selection when composer selection is empty. Cmd-. interrupts; Ctrl-Alt-D toggles dictation. `composer_scroll` estimates cursor position from byte proportion rather than measured wrapped lines. GPUI Kit must own caret, selection, editing, undo, clipboard text and wrapped-line scroll.
- `composer.rs`: pure Enter/Escape/approval policy and trimmed message logic. Retain these policies, replace manual movement. Approval keys apply only to an empty draft and offered decisions; held keys and the 700ms post-send guard cannot approve a command. A draft containing attachments is nonempty for approval purposes, even with no text.
- Sending: current `send_message` clears text before queueing Send; `command_failed` restores only if the box remains empty. `Feed::send` queues serialized commands. `feed::deliver` retries a broken connection based on error strings. This is unsafe for attachment submissions with ambiguous delivery: the attachment route must use CallError and one exchange only, then retain a reviewable draft on refusal/broken exchange. Add a command-submitted receipt for future UI integration. Submission is not turn completion.
- Questions: option picks live in request-keyed Draft; free text lives in a positional Vec<Input>. `answers_of` combines selected labels and trimmed typed answers, requires every prompt answered; single-choice one-prompt questions submit on click. Current submission clears text answers before acknowledgement. Migration should snapshot answers per request and retain on failure until resolution; choice and text editing cannot silently overwrite each other.
- Approvals: `approval.rs` maps offered decisions to keys and bounded detail lines. `approve` uses answered request ids to suppress repeated clicks; failures remove that flag. Keep provider decisions, default behavior and bounce guard.
- Models: catalog rows select model and compatible effort; empty catalog exposes a typed fallback. `toggle_menu` fills its legacy Input, model Enter configures. Configure controls also preserve fast/mode and handoff/stop/delete behavior. Use single-line GPUI Kit Input for fallback model entry and each question free-text field.
- Dictation: `chat_view/dictate.rs` runs Machine/Engine/Insertion, visible only with Settings.dictation_mic, off by default. One app-global active chat; partial speech replaces anchored selected text, user edits rebase the anchor; cancel removes partial insertion, final commits and does not send. Focus rules, vocabulary, silence timers, tab release and hiding the microphone are preserved. `dictation::Insertion` currently edits legacy Input directly: foundation adapter must expose text/selection snapshot and programmatic replace/restore so this integration can bridge it without editing shared dictation machinery initially. Unicode ranges must be explicit (legacy byte offsets vs Kit positions).
- `chat/{model,wire,client,host,driver,codex,claude}`: serialized vocabulary, strict legacy Capabilities `{identified_create}`, client Refused/Broken distinction, host per-chat lifecycle/events/logs, provider command seam. Automation identified creation probes strict Capabilities; adding fields would break an older probing client. Text-only Send must retain its exact serialized shape. Remote/iOS text callers remain compatible.
- `chat/media.rs`, `chat_view/media.rs` already retain provider image presentation and bounded rendering. Reuse their local-image display after attachment send; local attachments require owned files rather than user source paths. Preserve unavailable-image reasons, transcript selection, links and display-mode projection.
- `panels.rs::composer_path` paints continuous corners on an inset path, radius capped to bounds. Keep this outer shell and its tessellation test. Kit editor must have a transparent/background-free surface inside the existing squircle; native/theme fonts, scaling, disabled states, mic/send/interrupt alignment stay consistent.

## Foundation handoff requirements

1. Exact pinned commit/dependency revision and adapter API, including Textarea vs single-line Input construction, focus handles, event subscriptions, IME composition status, read/replace/set text, selection snapshot/restore, byte/UTF16 conversions and borderless styling.
2. Native GPUI revision compatibility, one initialization site and one theme bridge; no per-chat reinitialization.
3. Submit interception must allow approval-aware Enter and IME handling without swallowing normal editing keys. Textarea must support plain Return submit and Shift/Alt-Return newline. Entity focus must coexist with transcript copy and app/window actions.
4. Events/commands must identify the editor entity and request; renders must not recreate entities or reset text/selection. Model input persists while menu is open; answers are request-keyed.
5. Adapter route for existing dictation snapshot/range edits and programmatic text updates that participate correctly in Kit selection/layout. Do not retain legacy editor as an invisible parallel source of truth.

## Independent attachment backend contract

The initial supported set is UTF-8 text files (including source/config/Markdown) and static PNG/JPEG images. Directories, symlinks, special files, NUL-containing/binary text, empty or corrupt images, unsupported image formats and PDFs/archives/audio/video are refused visibly. General binary document ingestion is deferred rather than sent as a filename. File content is actually embedded in provider text blocks with a filename label; no claim of a provider-native arbitrary-file upload.

`StageAttachment` is a distinct host operation: it copies a bounded source into `chats/<chat>/attachments/<uuid>/`, creates owner-only metadata and preview, and returns an Attachment descriptor. Client source deletion or later edits cannot change the selected content. Descriptor includes id, display name, owned path, kind, byte count, change fingerprint and preview (bounded text excerpt or small PNG). Host verifies descriptor against its own metadata and confines it to that chat. Reads are bounded and reject symlinks/special files; store allocation is serialized and counts bytes and objects including previews/metadata. Staged files persist until chat deletion, including removed chips and failed drafts, so quota errors are explicit and no provider reference is silently evicted.

`ChatCommand::SendAttachments {text, attachments}` is a new enum variant. Old hosts reject the unknown command/operation and never see legacy Send. Do not append attachments to the legacy Send variant (an old serde host would ignore them). Do not alter strict legacy Capabilities. Staging refusal is enough to establish old-host incompatibility; no fallback is permitted. Text-only send remains `{command:send,text:...}`.

Host validates the whole set before starting/resuming a provider. Drivers validate/read again and assemble the complete content before writing. Invalid/stale/missing/cross-chat/over-limit attachment sets return a filename/reason without a partial text-only send. Attachment-only messages are supported. Provider submission never automatically falls back, queues into an unknown turn or retries after an ambiguous result.

Codex sends content arrays to `turn/start` or `turn/steer`. Images use a validated content data URL in the schema's `image.url` variant, eliminating provider-side path read races. Text files use separate text input items. Wait for the RPC result: success means accepted submission, not successful turn completion. Refused steering is returned without automatically starting another turn; a turn whose identity is not known yet is refused for explicit later submission.

Claude sends stream-json `user.message.content` arrays containing text and `{type:image,source:{type:base64,media_type,data}}`. An output writer receipt confirms the frame was written to the provider pipe; this proves local submission, not provider acceptance or completion. Restarting/not-ready drivers refuse attachments rather than burying them in the old text queue. Writer failure/timeouts are uncertain; the user must inspect transcript before explicitly resending.

Future UI uses a submission snapshot/generation: keep exact draft and attachments while pending; success clears only that snapshot, never newer edits; refusal preserves it; Broken preserves it and marks outcome uncertain. No automatic retry. Marking submitted must not display "completed". Only TurnCompleted outcome establishes completion. Staged entries and errors stay visible; failed staging never drops the source silently. Picker, image/file paste and file drop all share staging/validation; normal text paste stays with Kit. Empty text with attachments does not approve a waiting command.

## Provider evidence (2026-10-06)

- Installed `/opt/homebrew/Caskroom/codex/0.160.0/bin/codex`: `--version`; generated `TurnStartParams`/`TurnSteerParams` schema with `CODEX_HOME` overridden only for the child to an empty temp directory. UserInput has text, image.url and localImage.path; no arbitrary file input. Installed executable contains data-image URI handling. No provider prompt or real app-server runtime was started.
- Official [Codex app-server](https://learn.chatgpt.com/docs/app-server), Turns and steering: input arrays, image/localImage shapes and expectedTurnId. [OpenAI images](https://platform.openai.com/docs/guides/images-vision) documents base64 data URLs. These schema/input facts establish formatting, not live model support.
- Installed `/Users/dominik/.local/share/claude/versions/2.1.291` parser `eXo`: requires a source object, checks base64 media_type and decodes source.data; malformed images may otherwise degrade to a text note, so RiWork validates before sending. Only executable code was read; no login/settings files.
- Official [Claude streaming input](https://code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode) shows the stream user envelope and base64 image blocks. [Claude vision](https://platform.claude.com/docs/en/build-with-claude/vision) establishes media types and image content format. Runtime tests use fake drivers only; no inference/billing or entitlement validation is claimed.

## Integration and acceptance still required

After the pinned adapter arrives, replace all three chat editor families (composer Textarea, model Input, question Inputs), wire snapshot receipts/draft lifecycle and attachment picker/paste/drop/chips/previews. Preserve squircle, themes, scaling, dictation, approvals, questions, transcript copy, reconnects and handoff.

Automated acceptance: provider fake fixtures inspect exact attachment bytes/order/formats, attachment-only send, steering refusal, malformed/missing/stale/cross-chat files, size/count/quota bounds, private storage, legacy host refusal with zero legacy Send, text Send compatibility and identified-create/Automations regression. Draft/transport tests prove refusal and Broken preserve snapshots and do not retry, and success is distinct from TurnCompleted.

GUI acceptance (parent-coordinated, isolated fake home, RiWork Driver MCP): click caret at start/middle/end; drag, shift-click, double click and keyboard selection; wrapped multiline vertical movement; emoji/non-ASCII/IME composition; cut/copy/paste/undo; Enter/Shift/Alt-Enter; model fallback; request-keyed free-text answers; approval bounce guard and attachment-only draft; dictation final/cancel/user edits/tab switching; picker/paste/drop/remove, previews and unsupported/missing errors; rejected and broken sends with newer edits; themes/scaling and existing squircle. No task marked done until these pass and parent closes the task.

## Phase 1 recovery handoff (2026-10-06)

Independent attachment backend is implemented and verified; overall task remains in_progress. Preserved the prior backend, feed receipt and draft-helper edits. Recovery fixed cross-chat diagnostic ordering, validates encoded input bounds before starting a stopped provider, verifies Codex steering receipts against the expected turn, and isolates fake child HOME/provider homes/runtime with private permissions and removed API-key/token environment variables.

Committed contract: StageAttachment returns a retained owned descriptor; SendAttachments is additive and refuses unsupported hosts without legacy Send fallback. Capabilities remains exactly `{identified_create}` and text Send retains its serialized shape. Host ownership/metadata checks and driver byte validation precede any provider submission. Codex sends structured text/image data URLs and awaits start/steer RPC receipts; Claude sends base64 image blocks and awaits a pipe-write receipt. Unknown receipts/failed exchanges use `attachment_submission_unknown:` and surface as CallError::Broken. Attachment delivery performs one exchange and never automatically retries. Neither receipt establishes turn completion.

Recovery evidence (all fake/private runtime; no GUI, real provider, production host, real schedules or external state operations):

- `cargo test --offline --bin riwork attachment -- --test-threads=1`: 16 passed. Independent byte expectations include UTF-8 text, PNG and JPEG, attachment-only submission, deleted source files, refused/mismatched steering receipts, unknown starting turn, stopped Claude writer, lost replies, stale/missing/cross-chat descriptors, metadata/storage symlinks, count/send/encoded/storage limits, bounded previews, legacy-host refusal, and draft preservation.
- Focused regression executable filters: `chat::host::tests::` 39 passed; `chat::codex::tests::` 36 passed/1 live test ignored; `chat::claude::tests::` 51 passed/1 live test ignored; `chat::client::tests::` 5 passed; `chat_view::feed::tests::` 15 passed; `schedule_chat::fresh_tests::` 9 passed. Total 155 passed, 2 live-provider tests ignored.
- `cargo build --offline` passed. Five dead-code warnings identify the draft helper and receipt command field awaiting Phase 2 integration. `git diff --check` passed.
- Logs: `/tmp/riwork-chat-96f8a1ee-recovery-{attachments,regressions,build}.log`. Target: `/tmp/riwork-chat-96f8a1ee-target`; Zig PATH prefix: `/Users/dominik/.local/share/riwork/toolchains/zig-0.16.0`. Test-process home/runtime and profile overrides were child-scoped; fake providers have their own private homes.

Phase 2 awaits the foundation owner's compiled commit and adapter contract. The existing draft helper is an integration seam, not an active composer: wire receipts to submission/editor generations so a newer edit that returns to identical content is still preserved, correlate pending submissions, and retain reviewable failed snapshots. Replace composer/model/request-keyed answer editors, preserve dictation/squircle/approval behavior, add picker/paste/drop/chips/previews, and perform parent-authorized RiWork Driver GUI acceptance. Do not mark the overall task completed from this backend commit.

## Phase 2 source-only UI handoff (2026-10-06)

Runtime incident hold is active. These changes have **not been compiled or executed**.
Backend `1f0af2b5a9f29f70f3454981ee69505ef1997469` is preserved. Foundation
`ea05f94d566cdd952b778a2edafcb13f42a1a65f` was cherry-picked as
`d37d4d98b9c151f199b51ba5635990dd3296ddbc`; `GPUI_KIT_INPUT_CONTRACT.md`
is the adapter contract. No foundation verification receipt proves these consumer changes.

Composer uses one persistent Kit Textarea entity; typed model entry and request/prompt
answers use retained Input entities and subscriptions. The old ChatView input handler,
fake caret, manual editing/key routing and estimated composer scrolling are removed.
Render builds frames without replacing editor values. Native text editing, selection,
clipboard text, undo and composition stay with Kit. Only the four authorized ChatView
constructor calls changed in main; Cargo/shared input/forms/settings/init are unchanged.
Dictation bridges UTF-8 value/selection snapshots into undoable Kit replacements, retains
legacy helper compatibility and explicitly reanchors on user Change, including equal-text
undo/edit cycles. Continuous corners, palettes/fonts/scale, display/media/path previews,
transcript copy, tabs, focus, interruption and model/effort/fast controls remain in place.

Submission snapshots carry dispatch id, original editor identity and editor generation.
Receipts must also match the command. Success clears only that exact dispatched editor
generation; repeated/stale receipts cannot clear newer edits. Refusal and uncertainty keep
the exact saved snapshot and current draft, with review/copy/explicit resend/replace actions.
The UI delivery route performs one exchange for text, attachment and answer snapshots;
legacy Send serialization is unchanged. A receipt is submission acknowledgement, never
turn completion. Question editors include request id, prompt index and prompt text, retain
answers on refresh/failure, and discard entities when that identity leaves the transcript.
Choices reject stale buttons and do not transfer indexes into changed/reordered options.

Picker, native PNG/JPEG clipboard images, native file paste and external paths drop share
owned-byte staging. Chips retain source/error identity until staged, can be removed/retried
explicitly, and disclose bounded text/image previews. Pending/failed chips block partial
submission. Mixed clipboard policy: all file paths win over images/filename text; otherwise
all images win over accompanying text; ordinary text stays with Kit. This does not recover
formats the platform clipboard API itself omits. Unsupported image formats are visibly
refused. Any attachment chip makes the draft nonempty for approval policy. Kit's contract
reserves Shift/Alt+Enter for newline; session approval remains available through its button,
and the old Shift+Enter session-approval hint is removed from this UI.

Eight deferred ChatView tests mount the actual editors with a recording Feed (no socket,
provider or host startup) and cover composer dispatch/generation/stale receipts, uncertain
resend, model Enter/persistence, request-keyed answer Enter/refresh/refusal, IME and Alt
newline, mixed file paste, attachment-only approval guard and dictation partial/final/cancel
with intervening user edits. Snapshot helper tests also cover replacement editor identity.
These are authored source, **not passing test evidence**. Formatting and `git diff --check`
are source hygiene only.

After parent runtime/isolation release, the build owner must run locked offline check/build
and focused tests in audited private child HOME/runtime directories, without mutating the
inherited RIWORK_HOME. Reuse `/tmp/riwork-chat-96f8a1ee-target` and installed Zig prefix.
Required commands (not run under hold):

```text
cargo check --offline --locked
cargo build --offline --locked
cargo test --offline --locked --bin riwork chat_view::editor_tests:: -- --test-threads=1
cargo test --offline --locked --bin riwork attachment -- --test-threads=1
cargo test --offline --locked --bin riwork chat_view::feed::tests:: -- --test-threads=1
cargo test --offline --locked --bin riwork dictation::tests:: -- --test-threads=1
cargo test --offline --locked --bin riwork text_input::tests:: -- --test-threads=1
```

Native Cua.ai Driver acceptance still requires parent GUI ownership: caret/drag/wrapped
selection, physical IME commit, repeated-key approval guard, tabs/focus/dictation switching,
native image/file clipboard precedence, picker/drop/staging/removal, saved-snapshot recovery,
palette/scale/squircle and Normal/Verbose/media/path rendering. Overall task remains
in_progress until parent accepts compiled/runtime/GUI evidence. No production app/host,
tmux, shell, installed/main/iOS/relay/OTA state, fixture cleanup or delegate was touched.

## Independent attachment audit source follow-up (2026-10-06)

Read the full independent `GPUI_KIT_ATTACHMENT_AUDIT.md` from the foundation worktree;
no edits were made there. UI source commit is
`9c005da857e180c307b40a2db2ded6d6ffd01c06`. This backend follow-up is also
**source-only and uncompiled/unexecuted**, with the incident hold unchanged.

- A1: owned provider stdin is nonblocking. `send_line_until` bounds mutex acquisition,
  actual writes and pipe backpressure with a monotonic deadline and short poll waits.
  Codex's same 30-second deadline includes serialization/write plus receipt wait;
  Claude attachment deadlines start before enqueueing, so expired queued frames cannot
  later be silently written. Host Run retains a cancellation closure independently of
  the driver mutex; stop/reap cancels owned-child I/O before waiting for shutdown.
  Receipt waits observe cancellation. Zero bytes written are a known refusal; a failed
  prefix write is uncertain, closes/poisons that child's input, and never appends another
  JSON frame or retries. No detached writer was added; existing child process-group
  ownership, signal targets and shutdown escalation remain unchanged.
- A2: Claude's local user item stores the complete bounded Attachment descriptors in an
  additive, default-empty Presentation field. Durable history now links text and image
  messages to owned id/path/fingerprint/preview metadata without inlining file payloads.
  Equal-length same-name text versions remain distinguishable. Incoming ordinary text
  and image user blocks are retained too; a provider UUID gives replayed echoes a stable
  item identity. Local attempted user-item completion remains distinct from submission
  acknowledgement and provider turn completion.
- A3: incoming per-image and aggregate encoded retention limits derive from the accepted
  raw-byte attachment limits with base64 expansion and per-image padding allowance.
  The image renderer reads the same per-image limit. The bounds remain finite and below
  the provider frame cap; accepted PNG/JPEG echoes no longer become unavailable solely
  because encoding expanded their raw bytes.
- Start receipts require a nonblank turn id. Dispatch serials prevent late start receipts
  or refusals from mutating a newer lifecycle. A bounded completion cache also guards
  completion-before-receipt and reordered older completions; overflow is uncertain rather
  than reopening an already finished turn. Matching steer receipts acknowledge their
  expected turn without inferring its completion. No ambiguous receipt/write retries.

Deferred source fixtures now cover a live fake child that stops draining a large input,
pipe deadline/cancellation/mutex acquisition, distinct durable text versions, ordinary
user replay, a valid accepted PNG above the old encoded retention bound, aggregate raw
image echo retention, blank/missing ids and reordered completions. The fake backpressure
step records a consumed turn/start prefix before stopping reads, avoiding a timing-only
partial-write assumption. None of these fixtures was launched under hold.

After parent isolation release, run the UI handoff commands above plus focused
`chat::host::tests::`, `chat::codex::tests::`, `chat::claude::tests::`,
`chat::client::tests::`, and `schedule_chat::fresh_tests::` regressions with live provider
tests ignored. Verify end-to-end socket stop/shutdown during backpressure, the total
write/receipt deadline, cancellation before/after the first byte, pending answer replacement
and stale receipt ordering, native clipboard/IME and picker/drop behavior. The previous
Phase 1 receipts do not validate this follow-up. Parent owns build/runtime/GUI release and
task acceptance; this worktree returns idle without production or installed-state actions.
