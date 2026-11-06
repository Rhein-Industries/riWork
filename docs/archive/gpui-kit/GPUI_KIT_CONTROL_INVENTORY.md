# GPUI Kit desktop control inventory

Project: `39832c2e-23a5-476d-aa8f-5ff34a02d314`  
Task: `a1b2ecc7-9f3f-486c-992b-9c78a20e992f`  
Worktree: `e9e6f8d4-320d-4c63-99a7-dfbdb4d13969`  
Branch: `feat/gpui-kit-app-controls`  
Inventory baseline: `2f96821561105e3737cb019585841bc047cb38d4`  
Date: 2026-10-06

## Status and ownership

Verified the exact working directory, branch and HEAD before editing. The tracked tree was clean; the existing untracked `GPUI_KIT_NATIVE_FIXTURE_AUDIT.md` is preserved. The initial inventory is now followed by the owned source migration and inert fixture sources. See [GPUI_KIT_CONTROLS_IMPLEMENTATION.md](GPUI_KIT_CONTROLS_IMPLEMENTATION.md) for implemented families, exact integration dependencies and the proposed test allowlist. Compilation, tests and native QA remain unperformed; chat copying belongs to the chat/foundation owners.

Forms worker owns the non-chat Settings, Project Settings, Automations, files/preview and task/panel controls inventoried below. Project creation, remote prompt and handoff dialog are included as related form surfaces; `status_bar.rs` contains Settings controls and belongs in this migration. Foundation owns `controls.rs`, theme/init/text-input and `main.rs` initially. Chat worker owns chat copying and `chat_view`; no chat edits here. Workspace sites below are read-only integration proposals for the parent/foundation owner.

Source-only boundary: no Cargo/build/test/listing, fixture execution, GUI, production settings, services, tmux, process actions, installation or cleanup. Reviewer owns compilation; parent owns actual Cua.ai Driver MCP desktop QA. Existing input adapters and legacy Project Settings input helpers remain intact.

## Replacement contract

The cached pinned Base 0.7.1 source contains `Button`, `Switch` (with `SwitchTrack`/`SwitchThumb`), `Toggle`, `Radio`/`RadioGroup`, `ToggleGroup`, `Link`, `Popover`/`PopoverState`, and `Dialog`/`DialogHandle`. These names were verified by reading source, without invoking dependencies. This task does not change pinned dependencies.

The initial scan found only styling helpers. During inventory, foundation published `GPUI_KIT_BEHAVIOR_CONTRACT.md` and an in-progress `behavior_controls.rs`. Consumers will use `crate::behavior_controls` (`button`, `symbol`, `switch`, `toggle`, `segment`, `link`, `popup`) rather than introduce a second adapter. The owner must integrate that module/commit before consumer compilation. The early contract provides controlled Toggle segments and measured Popup positioning; modal focus/open policy is still explicit owner work, not supplied by Popup. Radio/roving row behavior requires coordination rather than an invented adapter API.

Required shared adapter capabilities:

* Base owns enabled/disabled activation, Space/Enter behavior, focus and actual accessibility semantics; owner callbacks keep domain actions and immutable identifiers. Do not retain custom parent activation and add only role metadata.
* Accept caller styling/content and stable control IDs/focus handles. Preserve Native capsules/switches/segments/inset rows and colorful outlined controls, palette colors, scaled dimensions, hover/selected/focus appearance and tooltip anchors. Avoid default Kit layout altering spacing.
* Support selected action rows, exclusive choices, checked toggles and icon-only controls with meaningful accessible names. Generic rows need a reviewed Base-backed row contract; this inventory does not assume an unverified Base menu-item API.
* Preserve identity across render and item reorder; remove state for removed items. Do not recreate focus or popover/dialog state during render. Focus refresh must not steal focus from child inputs or terminals.
* Persistent popover/dialog state must provide dismissal, focus entry/trapping where appropriate, and focus restoration. Workspace-owned state additions require foundation/parent integration.
* Button/row callbacks must support propagation control for nested actions without suppressing the child's Base activation. File double-click handling must retain the `ClickEvent::Mouse` distinction.

## Settings controls

Line references below refer to the inventory baseline, before subsequent migration edits.

| Source sites | Current control family | Planned Base replacement | Required preservation |
| --- | --- | --- | --- |
| `src/settings.rs:1355,1363` | Codex account selection rows | Radio/group or reviewed selected-row adapter | Exact account ID, availability, pinning; account refresh must not move focus |
| `src/settings.rs:1398,1399` | Account refresh | Button | Pending/disabled state and refresh callback |
| `src/settings.rs:1620,1676,1689` | Orca import/preview/apply actions | Button | Preview/apply validation and disabled state; tests use inert callbacks |
| `src/settings.rs:1903,1943,1947` | Remote setup actions | Button; Link only for genuine navigation | Existing busy/availability handling; no remote/service execution during worker QA |
| `src/settings.rs:2115,2171,2182` | Cua setup, permission/status actions | Button or navigation Link according to action | Keep actual Cua integration and current permission/status callback; no GUI/controller substitution |
| `src/settings.rs:2285,2374,2380` | Theme choices | Radio/group with RiWork swatches | Existing theme identity, palette and Native/colorful visual treatment |
| `src/settings.rs:2411,2479,2493` | Boolean preferences | Switch with existing row styling | Stored values and disabled preferences remain unchanged |
| `src/settings.rs:2571,2606,2610` | Provider/orchestrator and other exclusive choices | Radio/group or exclusive segmented adapter | Exact available/disabled options and persisted values |
| `src/settings.rs:2701,2727,2731` | Font choices | Radio/group | Font selection and text scaling |
| `src/settings.rs:2792,2831,2835` | Text-size decrease/increase/reset | Button/icon Button | Existing bounds, shortcuts and scaling |
| `src/settings.rs:2924,2928` | Match UI text to terminal preference | Switch | Stored match setting and disabled behavior |
| `src/status_bar.rs:340` | Show status bar | Switch | Master visibility setting |
| `src/status_bar.rs:375` | Reset status defaults | Button | Existing defaults and persistence callback |
| `src/status_bar.rs:473` | Per-item visibility | Switch | Exact `StatusItemKind` |
| `src/status_bar.rs:514` | Per-item left/right placement | Exclusive segmented choice, or Toggle if the adapter preserves its two-state action | Exact kind/side binding and ordering |
| `src/status_bar.rs:580` | Move item up/down | Icon Button | Disabled bounds; exact item identity and order |

`Settings::key_down` (`settings.rs:1414`, root listener at 3063) currently dispatches Enter/Space for focused actions and manages Tab, theme arrows and text-size shortcuts. Convert activation to Base once each control is migrated; retain domain shortcuts with no duplicate dispatch. Text-size row background focus (`2988`) is a container interaction, not a second action button. Settings navigation callbacks that open URLs must use Link behavior where applicable; a setup/import action remains a Button even if its label resembles a link.

## Project and modal forms

| Source sites | Family | Planned Base replacement | Required preservation |
| --- | --- | --- | --- |
| `src/project_settings.rs:847,894` | Unfiled/folder chips | Radio/group or exclusive segmented choices | Exact folder ID, path label, dirty state; unfiled is explicit `None` |
| `src/project_settings.rs:984` | Project account choices | Radio/group | Exact account pin and unavailable/disabled options |
| `src/project_settings.rs:1097,1121,1185` | Create folder, refresh accounts, Save project | Button | Invalid/dirty/pending/error drafts and revisions; refresh does not focus an unrelated control |
| `src/project_settings.rs:1610` | FolderEditor parent-folder choices | Radio/group | Exact parent identity and existing hierarchy validation |
| `src/project_settings.rs:1641,1661` | FolderEditor cancel/save/create | Button + shared Dialog focus behavior | Pending/validation state, cancel draft semantics, invocation focus restoration |
| `src/project_creator.rs:548,578,669,689` | Close, browse, cancel, create | Icon Button/Button + shared Dialog focus behavior | Creating disables cancellation; browse and submission retain existing defaults/errors |
| `src/project_creator.rs:634` | Initialize Git preference | Switch/Checkbox-style Base adapter | Existing `can_init`, busy guards and Cmd+G semantics |
| `src/remote_prompt.rs:519` | Pair/add/cancel through shared helper | Button + shared Dialog focus behavior | Pairing state, busy/invalid guards, supplied defaults and cancellation |
| `src/handoff_dialog.rs:569` | Target/provider/account/permission option chips | Radio/group or exclusive segmented adapter | Immutable exact target/account/revision bindings, enabled options and busy state |
| `src/handoff_dialog.rs:670,696` | Handoff action buttons, Native/colorful branches | Button + shared Dialog focus behavior | Existing validation, busy/cancel/error state and transport semantics |

Project Settings currently shares a form focus handle and logical active field (`key_down` at 711); non-input controls need stable individual Base focus. Preserve Cmd+S and input submission from `InputEvent`; remove competing Enter/Space dispatch on migrated action controls. The same audit applies to creator/prompt/handoff custom keyboard handlers. InputState remains the caret/selection/clipboard/undo/IME owner.

Modal propagation barriers (`project_settings.rs:1563,1564`, `project_creator.rs:517,518`, `remote_prompt.rs:585,586`, `handoff_dialog.rs:884,885`) remain boundary behavior, subject to shared Dialog composition. They are not independently clickable actions. Remote prompt field background (`552`) forwards focus to its existing masked InputState; there is no reveal-secret control to invent. Handoff line background (`handoff_dialog.rs:604`) currently redirects focus after child clicks; revise it so it cannot steal focus from newly focused Base controls. Keep legacy Project Settings `Input`/helper/macros available for the parent audit.

## Automations

`src/schedule_panel.rs` builds controls through one shared local `button` helper (729; Native click at 758 and colorful click at 788), with an additional Native New action at 888. This family inventory covers each `Control` variant, not merely the number of click callbacks.

| Control variants | Planned Base replacement | Preservation |
| --- | --- | --- |
| `New`, `Edit(id)`, `Save`, `Cancel`, `Delete(id)`, `OpenChat(id)` | Button/icon Button | Exact IDs/revisions, dirty/invalid/pending/cancel state and delete-confirm flow |
| `Pause(id)`, `Fast` | Toggle or Switch according to the shared adapter | Existing state meanings and enabled rules |
| `Destination`, `Provider`, `Permission`, `Scope`, `Target`, `Workspace` | Radio/group or exclusive segmented choices | Reviewed typed Shell/LegacyChat target, pinned account/options/revision; never substitute display labels for identifiers |
| `Repeat`, `FirstIn`, `ExactTime` | Exclusive choices where mutually exclusive; Button for a quick action | Cadence values and manual time draft semantics unchanged |
| `Field(index)` | Existing Base InputState frame | All six fields stay persistent; input owns editing/IME; no per-render resets |

Field-frame click at 832 is focus forwarding to the existing editor, not a button conversion. `focus_control`/`key_down` (344/358) currently route non-fields through one panel focus and dispatch Enter/Space themselves. Replace that activation with Base and persistent focus keyed by exact control identity, retaining Tab/ShiftTab, composition-aware Escape and Cmd+S. `controls.clear()` during rendering must not destroy focus identity. Do not edit scheduler/provider/service code or execute the six existing schedule fixture tests.

## Files and preview

| Source sites | Family | Planned Base replacement | Preservation |
| --- | --- | --- | --- |
| `src/file_explorer.rs:2150,2161,2176,2243,2294` | Shared text/glyph action/toolbar buttons | Button/icon Button | Native/colorful sizes, tooltip names, exact enabled predicates and current action methods |
| `src/file_explorer.rs:2430` | File/directory/symlink/error/status rows | Reviewed Base selected/disclosure row | Selection identity, directory expansion, status/error availability and mouse double-click-to-edit |
| `src/file_explorer.rs:2498,2878` | Preview PDF action | Button | Current preview availability and selected file binding |
| `src/file_explorer.rs:2660,2682,3029,3049` | PDF previous/next page | Icon Button | Page bounds and rooted file identity validation |

Tree `key_down` (1977) implements arrow navigation/disclosure and activation. Coordinate roving focus with the row adapter; do not activate twice when Enter reaches both row and tree handlers. File double-click is mouse-only; a keyboard click must not masquerade as a double-click or initiate editing unexpectedly. Error/status rows must be classified by action availability rather than blindly made enabled.

Whole preview content focus clicks (`2794,3082`) and preview key handlers (`2706,3068`) are justified custom focus/read-only content interactions. Preserve text selection and existing file-copy behavior; wrapping the whole preview in a button would interfere. `src/file_preview.rs` supplies preview data/rendering and rooted-file validation, with no click callback to migrate. It is not a reason to alter file loading or services.

## Projects, tasks, worktrees, shells and remote panels

| `src/panels.rs` sites | Family | Planned Base replacement | Preservation |
| --- | --- | --- | --- |
| 1376 | Project sort menu trigger | Icon Button + Popover | Stable popup state, dismissal and focus return |
| 1415,1439 | Native/colorful project header actions | Button/icon Button | Existing action, tooltip and layout |
| 1501,1535 | Sort selector and sort direction | Radio/selected choice and Toggle/Button as semantics require | Current ordering, direction and popup state |
| 1580,1672 | Sort popover dismiss/option rows | Popover + Radio/selected menu-row adapter | Checked order and keyboard focus; no separate custom activation path |
| 1716,1717 | Nested project row actions | Icon Button | Exact project ID and propagation isolation from containing row |
| 1771,1772 | Project notifications | Toggle/Switch | Exact project UUID and stored enablement; no parent row activation |
| 1907 | Remote folder disclosure | Selected/disclosure Button | Existing expansion identity |
| 2007 | Failure dismissal | Button | Exact failure/action identity |
| 2520 | Folder disclosure/header row with nested controls and drag/drop | Base disclosure row/Button + retained pointer drag/drop | Exact folder ID, expansion, drop validation; nested button activation must not toggle folder |
| 2624 | Project selection row | Reviewed Base selected-row adapter | Immutable project action, selected state, nested controls and drag/drop |
| 2693 | Generic worktree/task/shell/remote action rows | Reviewed Base selected-row adapter | Exact cloned `PanelAction` identity; keyboard and AX discoverability |

Search-frame focus clicks (1084,1313) remain InputState focus forwarding. They are not action rows and must not steal input focus on render/refresh. `sidebar_row` is styling rather than behavioral exemption. Pointer drag/drop remains custom where required; it does not exempt standard selection/disclosure or nested actions from Base behavior. Foundation/Workspace state ownership is needed for persistent sort popup handles where panels are rendered as functions rather than entities.

## Read-only Workspace integration proposal for foundation/parent

No edits to `main.rs`. These are conventional controls even when adjacent to terminal/native layout surfaces:

| `src/main.rs` baseline sites/functions | Proposed shared Base integration |
| --- | --- |
| 4523 `remote_strip` | Review visible action/status navigation controls; keep service state untouched |
| 8180,8181,8186 layout tabs | Icon Button for close; selected Button/row for tab selection; preserve tab drag and nested close isolation |
| 8664,8709 pane menu | Button trigger + persistent Popover dismissal/focus restoration |
| 8994,9031,9087 status navigation | Button/Link according to actual project/worktree/activity navigation action |
| 9142 copy active shell ID | Button preserving native clipboard callback; this does not diagnose chat copying |
| 9158,9173 status theme/Settings | Appropriate Button/Toggle preserving existing options |
| 9226,9289,9359 layout selector/menu | Button + Popover + selected rows; disabled choices remain inactive |
| 9406,9468 account/usage status actions | Button navigation/trigger with exact account identity |
| 9589,9615 usage refresh | Button with pending/availability rules |
| 9778,9800 center focus/restore workspace | Toggle/Button with current layout semantics |
| 9889 `pane_button`, 9930 `main_marker`, 9975 `pane_menu_row` | Shared behavioral adapters; stable IDs/handles and selected/disabled states |

Proposed integration hunks: imports and control constructors; stable focus/popup state initialization on Workspace; action-site adapter calls; removal of duplicated parent Enter/Space handling only for migrated controls. Foundation should coordinate any constructor changes with chat ownership. Root search/key handling (10150) retains focused-input versus terminal distinctions. Global quit/text-scale actions (11269 onward) remain application shortcuts.

## Justified custom/decorative/native exceptions

* Native window drag/titlebar/control islands (`main.rs:8246,8267,9742,10568`) and layout resize/drag capture (`7711,10161,10165`) are OS/layout interactions, outside standard control conversion.
* Pane-background focus (`main.rs:8544`), read-only preview focus/selection, text input focus frames and modal propagation boundaries remain custom container behavior; preserve child focus and activation.
* Ghostty terminal canvas, native selection/clipboard/context shortcuts, workspace layout/dock geometry and terminal input handlers are native/terminal-specific. Do not replace them with generic Base widgets.
* Decorative SF symbols, checkmarks, swatches, switch knobs, tooltip anchors, dividers and noninteractive labels stay decorative **inside** behavioral Base controls where relevant. A decorative switch helper alone is insufficient for an interactive setting.
* Domain file-tree navigation and validated project/folder drag/drop remain owner logic composed with Base row activation, not alternative keyboard/button implementations.
* Persisted state, account/provider resolution, scheduler/services, revision checks, rooted preview validation and asynchronous error handling remain unchanged.

## Meaningful synthetic test plan (source only, not executed)

Author fixtures against the finalized shared contract, with inert callbacks and synthetic IDs/secret text. Do not construct StateStore/SessionManager, invoke setup/provider/service callbacks, or reuse broad domain fixtures. Tests must render and dispatch input rather than only call the callback or inspect added role metadata.

| Proposed fixture | Observable assertions |
| --- | --- |
| `settings_controls_keyboard_and_ax` | Tab/ShiftTab traverses stable Base focus; Space/Enter activates enabled action once; disabled action never fires; switch/radio AX name, role and checked state reflect synthetic model |
| `project_choices_preserve_identity_and_submit_once` | Exclusive folder/account choice carries exact synthetic ID; duplicate labels do not alias; invalid/pending Save cannot run; child input Enter submits once |
| `automation_controls_keep_typed_target_and_revision` | Inert callbacks capture exact Shell/LegacyChat target, account and revision; keyboard selection and disabled options respect binding; no schedule fixture creation |
| `panel_nested_controls_and_popup_focus` | Keyboard action has correct immutable row ID; nested icon/toggle does not also activate its row; popup Escape dismisses and returns focus; reorder does not create a new focused control |
| `file_rows_and_preview_buttons_keyboard_and_ax` | Keyboard selects/expands correct synthetic path; only mouse double-click requests edit; page-bound buttons disabled; read-only preview selection/copy is not captured by a button |
| `form_dialog_focus_and_masked_secret` | Focus stays in dialog and returns to invoker; pending cancel is disabled; existing synthetic secret input remains masked; composition Escape does not cancel the form |

GPUI effect timing: end the action's `cx.update_window`, drain effects outside a borrowed Window, then assert in a separate read/update. Do not assert subscribed model effects inside the update that emits them. Include AX state checks from the rendered tree and actual synthetic keyboard dispatch; defer native AX acceptance to parent Cua QA. Exact qualified names/helper chains must be reviewed before any execution.

## Integration milestone

Owned module source and fixture authoring are implemented using the parent-approved caller-styled Base API. The implementation receipt supplies the concrete Workspace sort-state/action handoff. Foundation must integrate its adapter/init/root changes and the owned `main.rs` hunks before reviewer compilation. No runtime release is inferred from this source commit.

## Historical early contract consumer findings — superseded by caller-styled API

At the inventory-only milestone, the source-only trial against status-bar Settings exposed two concrete contract gaps. The subsequent parent-approved content API resolves those gaps; the text below records the original findings, not the current implementation status. No trial module edit is retained: applying it now would either preserve an invalid style chain or change the required visuals. The inventory is the reviewable committed milestone; implementation and test authoring remain outstanding.

1. **Caller hover styles cannot currently coexist with constructor hover styles.** In the foundation's in-progress `src/behavior_controls.rs`, `button`, `symbol` and `toggle` already call `.hover(...)` (source seen at 132,146,157). All three status Settings action families, and many other existing module controls, install their own RiWork hover style. Pinned GPUI's `InteractiveElement::hover` (`gpui-pre-0.3.8/src/elements/div.rs:842`) asserts `hover_style.is_none()`. A straight constructor substitution followed by the existing `.hover` would panic in a debug render. Removing the caller style loses palette/hover details; removing foundation defaults is foundation-owned work. Request a shared caller-styled construction path that sets hover only once, with no constructor-owned padding/background/corners needing undo, or an explicit constructor style callback/default opt-out. This is a source finding, not an executed failure.

2. **Caller-supplied content/marks are needed.** `behavior::button`/`toggle` append their own visible label; `switch` appends a fixed Native switch or 17-pixel colorful checkbox mark. Existing status item preferences use a 14-pixel filled Native checkbox or a 12-pixel colorful checkbox (`src/status_bar.rs:595`), while the master switch uses its own Native trailing-knob layout. Rich panel/file rows contain icons, multiple lines, ellipsized labels and nested controls. Request shared content variants for Button/Toggle/Switch, keeping accessible name separate from visual content and allowing current marks/layout. Symbol is adequate for a real glyph action; an empty glyph used to evade label insertion would be an unsuitable general row API. Keep disabled AX refinement on these variants.

Suggested semantic API (names for foundation to confirm): `button_content(id, accessible_name, content, disabled)`, `toggle_content(id, accessible_name, content, pressed, disabled)`, `switch_content(id, accessible_name, content, checked, disabled)`, returning the same shared Control types without preinstalled hover/layout. Existing module style chains can then wrap the exact Base primitive and preserve visuals. Controlled toggle segments can enforce the current exclusive domain selection; Radio/roving row and dialog focus semantics still need an agreed shared contract.

3. **Key-down versus key-up duplication must be removed during adoption.** Pinned GPUI's focused `.on_click` records Enter/Space on key-down and calls its activation callback on key-up (`elements/div.rs:3014-3079`). Settings currently calls domain actions on parent key-down for the same supplied focus handles; form panels have analogous logical-active-control dispatch. Retaining the old branch can cause two actions from one key press, or a changed focus can suppress the Base action. Consumers must remove their matching parent activation alongside conversion, rather than assume Base propagation automatically makes the old handler harmless. Retain domain shortcuts and child input submission separately.

Synthetic fixture construction is available through existing `form_input::test_window`, which supplies inert Settings/Appearance globals and Base init with no StateStore or SessionManager. Render a single Base Root only in the fresh synthetic test window when testing Tab traversal; do not add a production child Root. Kit `TestWindowExt` dispatches real synthetic key-down/key-up and exposes observed roles/labels/checked/focus facts. The shared disabled AX wrapper refines outside Base's observed inner element, so `snapshot.disabled() == None` alone cannot establish a missing native disabled state; verify callback rejection plus the finalized wrapper's actual AX node. These are planned source fixtures, not run tests.

Historical parent action requested before implementation: forward these API/style findings to foundation, supply the adapter source/commit for integration, and confirm persistent popup/dialog focus ownership. No permission for runtime execution is being requested here.
