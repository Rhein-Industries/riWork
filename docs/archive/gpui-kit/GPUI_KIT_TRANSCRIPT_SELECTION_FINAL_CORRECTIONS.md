# Focused endpoint and disclosure correction receipt

Project `39832c2e-23a5-476d-aa8f-5ff34a02d314`, task
`bf157564-29a2-470b-abc3-ffe1dde412cc`, owned worktree
`/Users/dominik/orca/projects/riWork-feat-gpui-kit-chat-input`, branch
`feat/gpui-kit-transcript-selection`. Verified starting HEAD
`30d82e181c47bfd7e8b19669d570c83a626f87ea`. Prior reports and the untracked
rollout audit remain untouched. Only owned chat source, fixtures and this receipt
changed; no foundation/forms/main/input/dependency changes.

## Evidence and narrow corrections

Read all three exact failure logs under `/private/tmp/rw2-2jhb4co_/logs/`:

- `test-chat_view-selection_tests-library_copy_adapter_preserves_selected_code_indentation.log`:
  actual `"  code 🦀\n    neste"`, expected `"  code 🦀\n    nested"`.
- `test-chat_view-selection_tests-source_projection_keeps_code_blank_lines_trailing_and_whitespace_only_bytes.log`:
  actual two spaces, expected three.
- `test-chat_view-selection_tests-chat_base_disclosure_pointer_enter_space_activate_once_and_disabled_is_inert.log`:
  first Enter left the clicked card open instead of closing it.

Pinned GPUI TextLayout delegates index_for_position to the containing-glyph
hit test, not the closest-caret test. A pointer at a text-sized Div's right edge
minus one lies inside its final glyph and resolves to that glyph's starting byte.
This matches both missing-final-character failures; the logs do not record
geometry, so the actual corrected positions still require reviewer execution.

The two code fixtures now take coordinates from the painted native TextLayout's
position_for_index: first glyph and one pixel past the final caret, at mid-line
height. A test-only helper asserts both endpoints remain inside the actual clip,
their native byte indexes are exactly zero/length, and the real content-key
resolver identifies the same source leaf/byte. Failure diagnostics include key,
pointer position, native bounds and clip. The tests then dispatch window.drag;
no selection snapshot/key injection or gesture replacement. Exact clipboard
strings, whitespace-only/trailing/blank-line checks and deferred menu Copy stay
unchanged. No exporter changes, padding or invented characters.

Pinned Base begin_impl calls the participant focus callback whenever the point
is inside its registered viewport, even outside text bounds. That callback is
deferred. Our full-viewport participant's unconditional callback focused ChatView,
overriding a child Base button's native pointer focus. Removed that redundant
callback and constructor argument. ChatView's existing native track_focus
handles text clicks; native GPUI child focus prevents the parent's default focus
transfer. No custom pointer focus/key activation handler added. Source tracing
establishes the conflict; corrected runtime behavior remains unexecuted.

The disclosure fixture drains deferred pointer effects, checks the card opened
once and actually holds native focus, then checks the same FocusHandle and
focused AX observation after every Enter/Space/rerender. Original toggle states,
disabled-card guard and recording Feed assertions remain. It never focuses the
button programmatically.

Manually included only the owned hunks of parent
`09cf9c8e91f763a1878deb34de1b7ba23b4a933c`: stable
`chat-choices-popover` ID before Menu role and borrowed `&Theme::global(cx)` for
ordinary-prose TextViewStyle. No cherry-pick or other-owner edits.

## Validation and required reviewer checks

Standalone rustfmt on the five changed Rust files and `git diff --check` passed.
No Cargo/check/build/test/test-list, fixture execution, provider/service/socket,
GUI/process/tmux/host/production action by this author.

Parent's 5e73c1c result (selection 13/16, total 41/50, eight editor regressions
passing) belongs to the earlier assembly, not this correction. Reviewer must
compile the new immutable assembly and rerun the three named failures, all
selection fixtures and editor regressions. In particular retain
`base_pointer_transfers_editor_focus_before_transcript_copy_without_losing_draft`,
`focused_composer_model_and_request_answer_keep_copy_priority`, and
`virtualized_partial_selection_exports_unmounted_endpoints_and_middle_both_directions`.
Their geometry, unmounted-middle/touched-source, partial bytes, window scopes,
Unicode and clipboard assertions were not weakened. All new-source outcomes are
pending. Parent remains sole Cua.ai Driver MCP desktop/integration/ship owner.
This is a source-only correction milestone; awaiting reviewer/parent release.
