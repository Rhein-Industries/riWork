# Transcript selection source milestone

Project `39832c2e-23a5-476d-aa8f-5ff34a02d314`; task
`bf157564-29a2-470b-abc3-ffe1dde412cc`; 2026-10-06.
Worktree `/Users/dominik/orca/projects/riWork-feat-gpui-kit-chat-input`, branch
`feat/gpui-kit-transcript-selection`, verified starting HEAD
`2f96821561105e3737cb019585841bc047cb38d4`. The previous rollout audit remains
untracked and unchanged. This is a source-only milestone, not acceptance.

## Why copying fails: evidence and limits

The deployed source's `select.rs` stores one `Selection { key, anchor, head,
text }`. Its move handler rejects a different run key. Paragraphs, code and
table cells have different keys, so it cannot extend across those boundaries
or messages. Pressing a link bypasses selection. Command/tool output and diff
lines were plain text children with no selection participant at all.

`ChatView::render` captures Base `input::Copy` before the focused editor.
`editors::copy_action` explicitly permits transcript fallback when the
composer has an empty selected range. That can copy retained transcript text
instead of letting the editor handle Copy. Its cached text also has no
streaming/projection lifecycle invalidation.

There is no production Base Root, TextSelectionLayer, `Root` key context or
explicit non-editor Cmd+C binding in the starting source. Base 0.7.1's Root
Copy key binding requires that context. Consequently the source does not
provide that library routing path while transcript focus is active. The
native application menus in `main.rs` also contain no Edit/Copy item.

These are source findings. No worker GUI observation, clipboard check or
production runtime reproduction occurred; attributing a particular reported
copy attempt to one of these paths remains an inference until parent checks.

## Implementation

Removed the handcrafted gesture, endpoint, word-boundary, range-merging and
clipboard-substring engine. Plain text uses Base `SelectableText::with_handle`.
Existing rich/link rendering uses the documented `TextSelectionHandle`,
`TextSelectionRegistration` and `TextSelectionRun` seam. Base owns all pointer
gestures, word/line multi-click, Shift extension, UTF-8 ranges, scroll gestures,
selected-text projection and copy ordering. Rich selection quads are only
renderer decoration of Base's projected ranges, preserving RiWork text style.

Each leaf retains its library handle and subscription. Reading order is
explicit `(visible row index << 32) | renderer leaf ordinal`; leaf ordinals
follow Vec/tree construction, independent of list paint order and HashMap
iteration. Separate handles prevent one leaf from overwriting another's
registration or copy projection. Prose, code, table cells, reasoning,
command/tool bodies, notices, plans and diff lines participate. Content inside
closed disclosures is not registered. Controls/icons and clipped-away output
are not synthesized into selected content.

Full Kit TextView was inspected. Replacing RiWork's markdown renderer would
also replace its code/table controls, image folding, paths/previews and style
handling; its public surface does not expose this transcript's explicit
cross-renderer document ordering. The documented participant seam preserves
those behaviors while moving selection itself to the library.

ChatView only activates its opaque selection scope on pointer interaction.
It does not install a second Root/layer or reproduce gesture logic. Focused
composer/model/request-answer editors retain their Base Copy handling; their
Focus events clear window transcript selection. The old capture Copy is gone.
A bubble Copy adapter queries Base's selected text without trimming, retaining
code indentation that stock Root's `.trim()` would remove. It consumes no
editor event before Base has handled it. Menu/action dispatch uses the same
Base `input::Copy` action.

Touched selected streaming rows invalidate selection before repaint. Tail
appends keep handle identities; non-prefix projection changes, display-mode
changes, deletion/release/window migration retire the old scope. Disclosure
and scale changes clear the relevant selection. Background chat retirement
only clears its window when its own participants hold selection. Link clicks
consult Base's snapshot/projection and decline activation for an extended
selection; collapsed clicks retain URL/file-preview behavior.

Diff rendering/count caches now compare exact source/kind, replacing their
length-only cache admission. Equal-byte-length updates can no longer display
or copy the previous diff.

## Foundation handoff dependencies

At the source reads for this milestone, foundation's shared contract and
`controls.rs` still expose the previous input/styling API; the new Base control
adapter contract is not yet published. No controls/main/theme/dependency
source was edited here. Button/toggle/disclosure migration and control AX
role/name/state/disabled-activation fixtures remain the next chat milestone
after that contract arrives.

The window owner must mount **one** Base Root around Workspace (or publish
an equivalent single TextSelectionLayer plus Root Copy/key-context contract).
Do not mount it separately per chat pane. Chat-owned test windows now mount
one Root and use recording Feed only. The active chat's pointer scope is
selected here; tab/pane composer focus clears the window selection.
Foundation/main owner should expose native Edit/Copy using Base `input::Copy`
so actual menu invocation follows the tested action path. This worker did not
change the native menus or window root.

Virtualized scrolling needs specific reviewer attention. Pinned Base
`WindowSelectionState::finish_frame` sweeps participants whose retained
rendered element leaves the window. Keeping a handle in the chat map alone
does not keep its registration alive. Thus a long drag that scrolls its anchor
row fully out of the list's rendered region may lose that anchor. This is a
source-inferred limitation, not an observed result or a solved claim. The
authored wheel fixture exercises a wrapped row that remains rendered; reviewer
must additionally probe dragging across virtualized-away messages. Any fix
must continue using Base's documented participant facilities, not restore a
custom range/gesture engine or upgrade dependencies.

## Authored checks — unexecuted

`src/chat_view/selection_tests.rs` uses actual pointer/key/action dispatch,
the test-platform clipboard and AX facts read from the actual element. It
never opens a chat subscription/provider. Source fixtures cover:

- `base_drag_crosses_unicode_paragraphs_and_messages_in_reading_order`:
  exact clipboard output, native menu-action equivalent, StaticText role/name.
- `base_word_line_and_shift_extension_use_real_pointer_events`:
  native click counts and Shift pointer extension.
- `rich_code_table_and_tool_output_participate_in_one_library_selection`:
  one mixed renderer selection and stable cell/output order without duplication.
- `focused_composer_model_and_request_answer_keep_copy_priority`:
  actual editor Copy, empty-selection priority and retained composer draft.
- `streaming_mode_and_other_window_cannot_copy_stale_or_foreign_selection`:
  pre-paint invalidation, mode reset and duplicate item IDs in distinct windows.
- `wrapped_unicode_drag_survives_a_wheel_repaint`:
  combining characters, a genuinely wrapping long row and actual wheel offset
  change, preserving the selected content while the row remains rendered.
- `selecting_a_path_does_not_open_it_but_a_collapsed_click_does`:
  drag/multi-click veto and recorded file-preview event, no real URL open.
- `library_copy_adapter_preserves_selected_code_indentation`.
- `same_length_diff_replacement_changes_visible_copy_and_clears_old_selection`.

Existing `editor_tests` share the single-Root mount and need regression runs.
Reviewer alone should compile and run exact selection/editor fixtures under
the already reviewed private environment. Then parent alone should perform
Cua.ai Driver MCP verification of selection visuals, scroll/virtualization,
links, native Copy menu, styles/scaling/AX and multi-pane lifecycle.

Only source formatting and `git diff --check` were run here. **No Cargo,
build/test binary, provider, service, GUI, process/session action or runtime
check ran.** No compile/test pass is claimed. Overall task remains in progress.
