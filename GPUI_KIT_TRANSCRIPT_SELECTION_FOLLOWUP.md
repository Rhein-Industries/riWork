# Virtual transcript selection and shared chat controls — source receipt

2026-10-06; project `39832c2e-23a5-476d-aa8f-5ff34a02d314`, task
`bf157564-29a2-470b-abc3-ffe1dde412cc`, worktree
`/Users/dominik/orca/projects/riWork-feat-gpui-kit-chat-input`, branch
`feat/gpui-kit-transcript-selection`. Starting immutable source:
`84d677557bda946d9a970aed7313408f568f6df2`. This report accompanies the
new source commit; its immutable hash is in the author handoff. No runtime
acceptance or completion claim.

The previous report/receipt and untracked rollout audit are preserved.
This report supersedes its per-leaf registration design, virtualization
limitation, broad TextView assessment and deferred chat-control migration.
The prior contract lookup consulted baseline styling/input files; the actual
foundation behavior source and contract below were read for this follow-up.

## Selection and exact export

PRE-03 is a proven source-lifetime problem in the previous design:
Base `WindowSelectionState::finish_frame` drops registrations whose rendered
marker dies. Retaining a leaf handle cannot retain its unmounted element.
The change mounts one document participant around the persistent transcript
viewport, outside virtual row lifetimes. It registers the actual viewport,
measured ListState scroll offset, clipped glyph bounds and rendered marker.
Virtual leaves report `TextSelectionRun` geometry to that participant. There
is no retained registration per leaf and no hidden duplicate text renderer.

Base owns pointer down/move/up, click counts, word/line extension, Shift
extension, focus, scope and autoscroll. `resolve_content_key_with` uses native
`TextLayout::index_for_position` to map an endpoint to an opaque stable leaf
identity plus UTF-8 byte offset. Semantic source keys assign identities
independently of paint order and never reuse an identity for a different key.
The participant's `copy_with` reads the current source projection between
those endpoints, including selected rows which no longer render. It validates
participant identity and byte boundaries. No application pointer-history,
gesture or word-boundary engine was added. Stable source ranges also decorate
mounted glyphs, with their original content masks, after remeasurement.

`selection_document.rs` exports the current display projection in Vec/tree
order: messages, paragraphs/headings, code, lists/quotes, table cells, expanded
command/tool/reasoning/diff bodies, plans/todos/notices, compaction and turn
outcome text. Markdown parsing shares the renderer's exact-content cache.
Closed/hidden technical bodies, clipped-away earlier output, image/control
labels and toolbar/footer actions are excluded. Existing output tail/ANSI
cleaning, tab expansion and displayed diff placeholder policies stay the same.
One newline separates displayed text leaves; embedded code newlines, empty
lines, leading/trailing spaces and whitespace-only leaves are retained.
Whole-message/whole-code Copy buttons retain their existing separate raw
source export policy.

PRE-04 has two independent filters: Root trims the result, and Base
`resolve_copy_items` filters whitespace-only participant contributions. The
ChatView bubble Copy adapter now exports this single document directly from
Base's captured snapshot, bypassing both filters. The standard Base Copy
action is unchanged and focused composer/model/answer editors retain priority.
No capture Copy or text fallback was reintroduced. Native Edit/Copy installation
remains parent/foundation owned.

Selected touched rows clear before repaint, including unmounted middle rows.
Unrelated tail updates preserve endpoints. Scope retirement clears projection;
non-prefix projection/mode changes, release/deletion and scale/disclosure
changes retain their established invalidation policy. Window migration creates
a new document participant/subscription, preventing old-frame sweeping or an
old-window copy callback from affecting the new window. Scope activation and
retirement use foundation's modal-aware lifecycle API.

## Shared controls

Read the explicit foundation files:

- `/Users/dominik/orca/projects/riWork-feat-gpui-kit-foundation/src/behavior_controls.rs`
- `/Users/dominik/orca/projects/riWork-feat-gpui-kit-foundation/GPUI_KIT_BEHAVIOR_CONTRACT.md`

Chat widgets now return shared `behavior::Button` / `behavior::Toggle` using
the no-style `button_content` / `toggle_content` constructors. Ordinary,
disabled, capsule, symbol, round and Copy controls retain their RiWork style
chains, visible content, palette, scale, font and tooltip placement. Base's
neutral control line height is explicitly restored to the prior inherited
RiWork value. Native appearance helpers supply their focus ring once; caller
hover/focus refinements are not duplicated.

Converted composite actions include card/file disclosures, approval detail,
provider-thread copy, image viewer activation, attachment/saved-draft folds,
and every menu choice. Normal/Verbose, fast, dictation and question choices are
controlled Base toggles. No parent Enter/Space activation was added; ChatView
raw key routing still handles only its interruption policy. Existing editor,
dictation, approval, bounce guard, receipt/generation and draft policy remains
with its owner.

Picker names identify approval mode/model/effort and expose expanded state;
menu choices use MenuItemRadio + checked state, ordinary actions MenuItem.
Question choices use request + prompt index + exact prompt text identity (the
wire schema has no prompt ID), pressed state and an explicit option name.
Answered choices have real disabled behavior and no hover activation. Empty
Send is disabled in both appearances. Image, attachment/remove, Copy and More
icon actions have meaningful names; disclosures expose expanded state.

Foundation adapters/main/theme/dependencies were not copied or edited. Assemble
the exact foundation commit containing these constructors, Button role override,
modal-aware scope activation/retirement and Root/native Edit-menu changes before
reviewer compilation. Chat mounts no extra production Root/layer. Fixtures mount
one Root per synthetic window.

## Pinned TextView assessment and adoption

Kit/Base remains 0.7.1, GPUI-pre 0.3.8, Ghostty 0.3.1. TextView supports ordinary
Markdown, emphasis/headings/lists/code/tables, foreground/link/selection and
block styles, code/table action callbacks, image-source and link-click policy.
Those features alone do not justify a custom prose renderer.

Compatible ordinary **question instruction** paragraphs/emphasis/headings now
use real `TextView::markdown`, library keyed retained state and RiWork
`TextViewStyle` refinements. Source identity includes request/index/prompt text.
These formerly nonselectable instructions remain nonselectable. Rich prompts
fall back to their previous literal instruction rendering. An explicit inert
link callback prevents implicit URL navigation; no image/network policy is
introduced into question instructions.

The precise transcript boundary is shared document selection: pinned
`text/state.rs` keeps `selection_adapter` and `line_spans` private; the adapter's
handle/runs are not exposed. Public `TextViewState::rendered_text`,
`selected_source_range`, bounds and range highlights do not expose point-to-byte
glyph hit testing. `TextView` has no public external participant injection,
glyph-run reporting callback or document-order setter. Its element-keyed state
also leaves with an unmounted virtual row. Disabling its selection and adding a
shadow StyledText for glyph geometry would mis-map emphasis/headings/wrapping;
retaining private per-row adapters would repeat PRE-03. Consequently transcript
ordinary prose participates through the same documented public renderer seam
as the other rich leaves, rather than using another gesture engine or changing
the dependency.

Remaining renderer-specific needs are bounded, not claims that Kit cannot
render these formats: code keeps its raw Copy policy, compact header/label and
horizontal-only scrolling; tables keep their existing per-cell alignment,
scroll controls and cross-cell document export; images keep staged/local/URL
policy, fold/paging/previews and explicit errors; diffs keep source-kind-aware
coloring, gutters/statistics/truncation and expandable per-file cards (TextView
does not supply this diff-card renderer). Link/path preview routing remains
RiWork owned. All use the shared document participant where textual glyphs
exist; library markup rendering is adopted where it can be composed correctly.

## Authored fixtures and pending verification

All fixtures are **unexecuted**. New named cases in `selection_tests.rs`:

- `virtualized_partial_selection_exports_unmounted_endpoints_and_middle_both_directions`
  holds a real pointer drag while wheel scrolling, proves anchor and middle
  elements/geometry unmount during the drag, releases at a partial Unicode
  cursor, scrolls beyond both endpoints, proves all three elements absent and
  checks independently constructed exact Cmd+C/action Copy. It also verifies
  unrelated tail preservation and touched unmounted-middle invalidation.
- `source_projection_keeps_code_blank_lines_trailing_and_whitespace_only_bytes`
  checks a spaces-only selection and cross-code export containing empty/blank
  lines and trailing spaces, including the menu-equivalent Base Copy action.
- `base_pointer_transfers_editor_focus_before_transcript_copy_without_losing_draft`
  yields for Base's deferred focus after actual editor/drag input, checks focus,
  clipboard and retained composer draft.
- `chat_base_disclosure_pointer_enter_space_activate_once_and_disabled_is_inert`
  exercises the actual card button and disabled empty-output disclosure.
- `chat_base_menu_choice_has_radio_semantics_and_records_one_configuration`
  reads actual observed role/name/checked/expanded state and asserts exactly one
  recording Feed configuration, with no real provider.
- `chat_base_question_toggle_keyboard_and_answered_guard_preserve_draft`
  renders Kit emphasis in the prompt, tests pointer/Space/Enter toggling, pressed
  name/state, disabled answered activation and retained choice/no delivery.
- `chat_shared_widgets_actual_ax_nodes_keep_names_states_and_disabled_click_capability`
  reads the rendered shared primitive's actual AX node in both appearances,
  including foundation's disabled refinement and enabled Click capability.
  It does not claim native OS AX action delivery.

The nine earlier selection fixtures are retained, as are all editor regressions.
The strict selection mount now panics if host staging is reached and uses only
recording Feed. It starts in transcript focus for same-turn selection cases;
the explicit editor-to-transcript case yields through the actual deferred focus
path. Attachment staging-failure editor cases retain their separate fixture
configuration. A source fixture's nonexistent QuestionPrompt ID was corrected
without changing the wire model.

Only source reads/edits, standalone rustfmt, Git whitespace inspection and a
source commit are authorized here. No Cargo/check/build/test/test-list binary,
fixture launch, provider/service/socket, native GUI, process/session/tmux action,
production state, install/reload/merge/push or dependency change ran.

Reviewer must compile the immutable assembled candidate, run selection and
editor/control regressions in its reviewed private environment and scrutinize
virtualized pointer/focus effect timing. Headless Noop fonts/metadata cannot
establish native glyph appearance, AX descendants/action delivery or IME.
Parent alone must perform Cua.ai Driver MCP pointer/scroll/keyboard/native-menu,
style/font/scale/squircle, path preview, streaming/Normal/Verbose, modal/multi-chat
and active-editor acceptance. The source virtual-document fix is implemented;
neither its runtime success nor overall task completion is claimed.
