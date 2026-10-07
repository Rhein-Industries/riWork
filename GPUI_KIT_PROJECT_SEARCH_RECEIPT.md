# Projects search restoration — source receipt

Project `39832c2e-23a5-476d-aa8f-5ff34a02d314`; task `8aedc5d3-d281-4a7f-98c5-9d6f601260ef`; WT `e9e6f8d4-320d-4c63-99a7-dfbdb4d13969` at `/Users/dominik/orca/projects/riWork-feat-gpui-kit-form-inputs`, branch `feat/gpui-kit-app-controls`.

Baseline synchronization: started tracked-clean at `47080cbcebc575b75a3de9a382553ce11a32e6bf`. Merged exact deployed `78ebbbbecf2eb65308c3b7a3a1c54416e382fd72` with external hooks/signing disabled, producing `34800a57ddb8fe7cdd23049986be51585a910594`. No conflicts occurred. Before task edits, `git diff 78ebbbbecf2eb65308c3b7a3a1c54416e382fd72 -- src Cargo.toml Cargo.lock build.rs` was empty. No other baseline source was substituted.

## Concrete source cause and correction

The colorful Projects toolbar fixed its height and shared a shrink-to-zero search with two nonshrinking create controls, then clipped overflow. Narrow pane widths could leave no usable search viewport. Native put the shared form input (100% width, its own border/background and 6/3-point padding) next to a magnifier inside a 24-point capsule: it competed for the icon's width and its natural line-plus-padding height exceeded the compact chrome. Both outer wrappers dispatched Search on click through Workspace's active-pane fallback, rather than directly targeting the clicked surface's retained InputState. These are source/layout findings; no new desktop reproduction or execution is claimed.

`panels.rs` now shares panel_search between both chrome styles. It keeps the existing native capsule/palette/spacing and colorful strip/text-scale expressions; the colorful search has a readable minimum and toolbar wraps its existing create buttons instead of reducing the search to zero. Button order remains search, folder, project, then existing sort controls, with original 30-point colorful button height. The icon has a nonshrinking measured box. Input and icon expose actual mounted test identities; the real TextInput is named “Search projects” for accessibility. The existing Search ⌘F placeholder comes from the same retained state.

`form_input::search_frame` adapts the existing guarded Base input to its caller's search chrome: remaining flex width, full chrome height, no second form border/background/padding. Existing shared Enter/Escape/composition guards and paste policy remain. Ordinary form/text_input presentation is unchanged. Base continues to own selection, glyph hit-testing, caret, clipboard, undo and IME.

The outer icon/padding mouse-down path focuses that exact retained state and prevents ancestor default focus takeover. It neither resets text/cursor nor dispatches a second active-pane Search action. Text-child glyph hit-testing still runs first. No state is constructed, reset or resubscribed during render. Existing main.rs Ctrl/Cmd search shortcuts, Tab/ShiftTab/Return/Escape routing, per-tab membership checks, query source IDs, protected sibling synchronization, filtering and focused-input/terminal distinction are untouched (source read around ensure_search_inputs/set_search/focus_search_input and the PanelData caller). No competing EntityInputHandler or editor engine was introduced.

## Proposed focused regression fixtures

Four exact names/helper chains are in `GPUI_KIT_PROJECT_SEARCH_PROPOSED_TESTS.json`, all source-only and awaiting reviewer gate. They mount real Projects renderers, real Base states and one headless Root with in-memory globals; actual native face/scale uses ui_text_matches_terminal=false and no Ghostty/terminal globals.

Geometry fixtures check actual layout/paint at 160/240/480 pixels, input/icon visibility and nonoverlap, readable width, full-height text viewport, real empty-placeholder caret bounds, named TextInput, stable entity and icon pointer focus. Editing fixtures in native and colorful styles check real input/filter rows, select-all/copy, glyph cursor placement, and exact sibling-surface Change/Focus identity while preserving the first editor's draft/cursor. They require no Store, session, Workspace, real account, provider, filesystem fixture or service. Effects drain outside the borrowed Window before later assertions. The per-surface inert callback proves the renderer delivers editing to the correct supplied entity; production Workspace membership/synchronization is unchanged and is not launched by these fixtures.

Native OS font/SF-symbol pixels, real OS clipboard/AX/IME, actual pane shortcuts/lifecycle and GUI acceptance remain parent/reviewer limits. No tests were compiled, listed or executed here. Reviewer alone may perform the approved private isolated checks/exact invocations after source gate; no broad filter is proposed.

## Scope and checks

Only `src/panels.rs`, `src/form_input.rs` and the new task receipt/proposed-test JSON changed after baseline sync. No main/chat/text_input/backend/host protocol/dependency change. Image-paste previews remain a parent-coordinated chat consumer change using already validated/staged paths; this owned search patch does not change attachment staging or host runtime.

Approved Rust 1.95.0 rustfmt (`--edition 2024 --config skip_children=true`) and `git diff --check` passed. JSON names match actual source declarations. No Cargo/check/build/test/test-list, desktop input, native app/service/provider/process-control/tmux/host action, shell close/restart, cleanup, merge beyond the explicitly authorized source baseline sync, push/install/reload or delegation occurred. Inherited RIWORK_HOME/environment was not modified. Existing tracked receipts and untracked native audit remain; audit SHA-256 `5d5b74a4fe5f33c3d0491df533b0aa4e514d9febbd9897ebb9308e62fce4ef4e`. Parent alone integrates and activates; return idle after source commit.
