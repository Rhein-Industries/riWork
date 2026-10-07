# Draft image preview source receipt

Project `39832c2e-23a5-476d-aa8f-5ff34a02d314`, task
`cc2bf666-f875-4c7f-8117-70d1a3c5d57e`, assigned worktree
`9c106443-4122-4841-a2c7-4e525648e957`, branch
`feat/gpui-kit-transcript-selection`. Prior tracked reports and untracked
`GPUI_KIT_ROLLOUT_CONNECTION_AUDIT.md` preserved.
This focused correction starts from
`02ebe8ad618a0801a7148dcb182fbb0d3e785f8c`, verified tracked-clean with the
same untracked audit. Parent's requested queue placement and 56-pixel Ready
thumbnails remain intact. Only owned chat sources, the newly authorized pure
attachment utility, focused fixtures and this receipt changed.

## Authorized baseline sync

Verified cwd, branch and starting `20220e62e543f8a0ff559f7619c80884f8ddcb04`;
tracked tree was clean. Merged deployed
`78ebbbbecf2eb65308c3b7a3a1c54416e382fd72` with hooks and signing disabled,
no conflicts, producing `980721a61fa12f88ca58a90c996c337a1c7e5193`.
Before implementation, `git diff 78ebbbbecf2eb65308c3b7a3a1c54416e382fd72 --
src Cargo.toml Cargo.lock build.rs` was empty. No dependency change.

## Cause and resulting behavior

The draft queue was rendered after the composer and Attach row. Every chip
started collapsed, and the image element existed only inside its open disclosure,
so a staged pasted image had no automatic visual preview.

Draft attachments now precede the existing squircle composer, inside the same
palette/typography/scale-aware panel and a bounded scroll area. Ready image chips
automatically show a 56-pixel square, aspect-preserving thumbnail. The existing
filename Base button expands/collapses the larger preview; compact thumbnails
remain on collapse. Saved submission review cards retain their controls below
the composer. Empty attachment drafts retain the text-only footer layout.

Ready images use the existing Attachment.preview path. The initial 02ebe8a
implementation required Ready even for clipboard images, which left the user's
paste invisible while the retained older host refused staging. This correction
also produces a validated in-memory clipboard thumbnail in an independent
background-executor task, without calling Ensure, opening a socket, reading a
file/home or waiting for host staging. No UI-thread decode or raw-image render.

The small pure image_thumbnail helper in src/chat/attachments.rs shares static
PNG/JPEG/APNG admission with classify and the existing decoded routine: 4 MiB,
8192 per axis, 16 megapixels, 64 MiB allocation, no APNG. Both local and host
thumbnail generation use it, yielding bounded 256-pixel PNG bytes. Clipboard
declared formats outside PNG/JPEG are also refused for local previews. Picker,
drop and file paste retain the host-owned Ready path. No protocol/service or
dependency change; text-only paste and mixed precedence remain unchanged.

Pending/Failed clipboard chips show the same 56-pixel preview automatically and
can expand it, while status/error/retry/remove remain explicit. A local preview
never yields an Attachment, changes the editor generation or bypasses Send's
Pending/Failed guards. Ready clears the local Arc and takes precedence; late
local results cannot replace it. Removal drops chip-owned memory. Completion
callbacks accept only the current UUID, so late results cannot revive removed
chips or modify a newer attempt. Each local Image has a fresh private asset ID;
Ready/removal explicitly retire its GPUI asset cache after the old frame, so
identical images in other chips/windows cannot be evicted. Draft replacement
and view release retire remaining local caches too, using the existing owner
lifecycle (no cache service or protocol).

Image nodes, queue and status text have names/roles. Existing styled Base
preview/remove/retry buttons retain native activation; unavailable preview
actions are disabled, retries name the affected filename. Restored baseline
UUID-per-attempt retry semantics: target the correct chip, retain its original
Source and valid thumbnail, mint a fresh UUID to reject old completions. If the
prior local job has not finished, the new attempt starts its own independent
preview job. Host attachment IDs/bytes, admission limits and receipts are intact.

## Focused proposed checks — unexecuted

Two extended headless cases and one new pure case in
`chat_view::attachment_ui::tests` (all uncompiled/unexecuted here):

- `staged_png_jpeg_thumbnails_render_above_composer_and_keep_exact_draft`:
  real local staging into a private unique TMPDIR child, actual image paths and
  native rendered geometry/roles, automatic collapsed thumbnails, text exclusion,
  pointer/Space disclosure, stable editor/generation, exact targeted removal,
  recorded attachment submission, and Broken receipt retaining the snapshot
  and visible draft without automatic retry. Added actual Pending rendering at
  the local completion seam, Ready releasing local memory, host-path precedence
  and a late local result unable to replace Ready, including actual cache eviction.
- `mixed_image_paste_preview_survives_refusal_and_retry_guards_attempt_identity`:
  real test-platform Cmd+V for ordinary text and mixed PNG/JPEG/text, filename
  errors despite bounded, visible local previews with refusing Ensure, Pending
  and Failed Send guards, fresh retry UUID/original image bytes/untouched neighbor,
  old-attempt completion rejection, removal proven Pending with no resurrection,
  and corrupt/GIF/oversized/unsupported-declared clipboard images having no preview.
  Local asset removal must leave the untouched neighbor's cache intact.
  Its counted Ensure only refuses; no socket/host/provider is started.
- `pure_clipboard_thumbnail_keeps_shared_static_image_admission`: actual PNG/JPEG
  input yields bounded PNG; corrupt, unsupported, >4 MiB and >8192-axis inputs
  refuse. Static/APNG/pixel/allocation guards remain shared source, not a second
  decoder policy.

The existing editor harness is reused (one Base Root, recording Feed). Its
test-only mount_config is visible to the sibling fixture. Locally staged files
are left for reviewer inspection. No existing editor/selection test was weakened.
Reviewer should compile and run these three exact cases plus existing mixed-file paste,
attachment-only approval, model/question Copy priority and submission-generation
regressions in the audited private environment. Native clipboard/picker/drop,
real-font layout and GUI acceptance remain parent-owned.

Standalone rustfmt and git diff --check passed. Zero Cargo/check/build/test/list,
GUI/provider/host refresh/process/tmux/shell close, merge to main/push/install/
reload by this author. RIWORK_HOME unchanged. Only the previously authorized
baseline merge and owned source authoring/commits occurred; all new-source runtime
outcomes are pending. Production host PID78989 remains untouched; this change
does not depend on a host refresh or imply that refused attachments can be sent.

Project-search-bar correction belongs to the parent/forms owner; this attachment
change does not edit or claim to fix Workspace search. Parent alone integrates
and activates, using actual Cua.ai Driver MCP for desktop acceptance.

## Focused fixture observation correction

Starting source `e8392542e89bc413e42c3d871919043e6bc307ac`, tracked-clean with
the same untracked audit. Read all three exact preview logs under
`/private/tmp/rw3r-jul_grwi/logs/test-chat_view-attachment_ui-tests-*`.
Combined `1116a7de` compiled/listed successfully; its pure thumbnail admission
case passed. Parent also reports four existing editor/Copy regressions passing.
Both UI cases stopped at the missing observed `chat-composer-shell` lookup;
their later behavior assertions did not run.

The logs register the actual `chat-composer` beneath that decorative shell.
`panels::composer_editor` supplies this stable ID to `text_input::textarea`,
whose shared frame constructs Base InputBase. Pinned InputBase::new observes
its actual frame with test_support; the outer decorative shell does not.
Only the two placement lookups now use `chat-composer` bounds, asserting that
the entire preview queue ends above the user's actual input. No production
observer plumbing or source changes; every remaining bounding/status/state/
Send/removal/retry/stale-result/cache/editor/generation/disclosure assertion stays.

Formatted the owned test file and passed git diff --check only. No Cargo/check/
build/test/list or runtime action here. Reviewer must rerun both corrected exact
UI cases; their post-correction outcomes remain pending. Previous pure/regression
passes belong to the prior combined source and do not validate these later paths.
