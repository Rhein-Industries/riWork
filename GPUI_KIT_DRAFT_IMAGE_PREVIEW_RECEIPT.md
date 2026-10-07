# Draft image preview source receipt

Project `39832c2e-23a5-476d-aa8f-5ff34a02d314`, task
`cc2bf666-f875-4c7f-8117-70d1a3c5d57e`, assigned worktree
`9c106443-4122-4841-a2c7-4e525648e957`, branch
`feat/gpui-kit-transcript-selection`. Prior tracked reports and untracked
`GPUI_KIT_ROLLOUT_CONNECTION_AUDIT.md` preserved.

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

Both image views load only the existing Attachment.preview path: host staging
already validates PNG/JPEG and writes a thumbnail bounded to 256 pixels. No
source-image/full-content decode, protocol/runtime change, new state or service.
Picker, image/file paste and external drop share the same Ready-chip renderer.
Text-only paste and mixed clipboard precedence remain unchanged.

Image nodes, queue and status text have names/roles. Existing styled Base
preview/remove/retry buttons retain native activation; unavailable preview
actions are disabled, retries name the affected filename. Explicit retry keeps
the chip ID and original Source. A prior Failed result is terminal for its
one-shot task; only Failed may start another attempt, Pending blocks overlapping
attempts, and a removed UUID cannot be resurrected by completion. Host attachment
IDs/bytes, admission limits, editor generation and submission receipts are intact.

## Focused proposed checks — unexecuted

Two new exact headless cases in `chat_view::attachment_ui::tests`:

- `staged_png_jpeg_thumbnails_render_above_composer_and_keep_exact_draft`:
  real local staging into a private unique TMPDIR child, actual image paths and
  native rendered geometry/roles, automatic collapsed thumbnails, text exclusion,
  pointer/Space disclosure, stable editor/generation, exact targeted removal,
  recorded attachment submission, and Broken receipt retaining the snapshot
  and visible draft without automatic retry.
- `mixed_image_paste_retry_remove_preserve_chip_source_identity`:
  real test-platform Cmd+V for ordinary text and mixed PNG/JPEG/text, filename
  errors, one explicit retry with stable chip/source/image bytes, untouched
  neighbor, and removal proven Pending before completion with no resurrection.
  Its counted ensure callback only refuses; no socket/host/provider is started.

The existing editor harness is reused (one Base Root, recording Feed). Its
test-only mount_config is visible to the sibling fixture. Locally staged files
are left for reviewer inspection. No existing editor/selection test was weakened.
Reviewer should compile and run both exact cases plus existing mixed-file paste,
attachment-only approval, model/question Copy priority and submission-generation
regressions in the audited private environment. Native clipboard/picker/drop,
real-font layout and GUI acceptance remain parent-owned.

Standalone rustfmt and git diff --check passed. Zero Cargo/check/build/test/list,
GUI/provider/host refresh/process/tmux/shell close, merge to main/push/install/
reload by this author. RIWORK_HOME unchanged. Only the authorized baseline merge
and owned source authoring/commit occurred; all runtime outcomes are pending.

Project-search-bar correction belongs to the parent/forms owner; this attachment
change does not edit or claim to fix Workspace search. Parent alone integrates
and activates, using actual Cua.ai Driver MCP for desktop acceptance.
