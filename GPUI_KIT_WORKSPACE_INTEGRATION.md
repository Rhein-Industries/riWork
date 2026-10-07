# Workspace / forms controls integration — source handoff

2026-10-06 · Project `39832c2e-23a5-476d-aa8f-5ff34a02d314` · Task `d87296de-efe1-483a-98a0-d52a5da00e34`.

Base: `f55ad76ed80c4fa564ebf13ac31c68d3262e4583`, branch `feat/gpui-kit-behavior-controls`, foundation worktree `51ffe747-ac8f-48ab-9fbf-0781904db12f`. Source implementation touches only `src/main.rs`; this report is the other tracked change.

## Assembly dependency

Read the entire Foundation handoff and actual forms source at **`a57e4db3ca3816030e5d6b0b337c0c3dd4fbd2c3`** in the read-only forms worktree. This main change requires that forms commit's `panels::ProjectSortUi`, `PanelData::project_sort_ui`, `PanelAction::SetProjectSortMenuOpen(bool)`, mutable-Window `render_panel`, and four persistent Base Dialog owners. Those modules are intentionally not copied into this branch. Parent must assemble both before reviewer compilation; no standalone compile or native acceptance is claimed.

## Ownership and preserved behavior

* Workspace retains a `ProjectSortUi` by the globally unique tab ID used for that surface's search editor. Membership uses the existing shown-tab predicate, including focus mode; simultaneous Projects panes get separate owners. Ordinary rerenders retain the same entities and never focus them. Hidden owners close through pinned Base's non-focusing `set_open(false)` before removal; a project layout rebuild retires all owners before numeric IDs can be reused.
* The converted trigger has already opened/focused its Base state before sending controlled `SetProjectSortMenuOpen(true)`. Workspace identifies that focused open scope, dismisses peers without restoring their focus, and keeps the existing pane-menu/drag policy. An unowned open request fails closed. The old Toggle compatibility path only opens a focused retained trigger; it does not invent an owner or focus Workspace.
* Close/order callbacks and external settings, drag, project-load, pane/layout-menu and dialog-open paths reach live state. Popup-owned close/order no longer calls `focus_active`; Base restores its live invoker once. Menu invoker capture happens after sort dismissal. Root Escape yields to a focused sort scope; the popup's handler owns dismissal. The existing search composition guard stays in place. Membership retirement deliberately does not restore a removed trigger; existing intentional navigation still owns domain focus.
* `render_layout`, `render_pane`, and `render_focus` now pass `&mut Window` through the sole `PanelData`/`render_panel` call. Settings persistence retains the exact `settings_store.update(|settings| settings.project_order = order)` path and existing success/error handling. Project/folder DnD and specialized terminal/window behavior are unchanged.
* ProjectCreator, FolderEditor, RemotePrompt and HandoffDialog backdrops now use `selection_scope` only. Actual forms rendering supplies `.handle(self.dialog.clone()).focus_handle(self.focus.clone())`; pinned Base Dialog installs its focus trap. Actual cancellation/Hide source calls `project_settings::close_modal` before its event, conditionally restoring the invoker only while the dialog owns focus. Workspace removes its redundant modal handles/FocusReturn and does not focus again on Cancel, Closed or Handoff Detached. Existing success/domain navigation and failure handling remain. The transition-based modal selection tracker, one window Root/selection layer, and pane/layout menu focus scopes/restoration remain intact.

## Verification boundary and reviewer follow-up

Source review checked forms `panels.rs` owner, trigger transitions, popup Escape/outside dismissal and render signature; `project_settings::close_modal`; and all four Dialog constructors/render chains plus Escape/Cancel/Hide call sites. Pinned Base 0.7.1 `popover.rs` distinguishes non-focusing retirement from live focus restoration; `dialog.rs` installs the actual trap. No dependency upgrade or other worker source edit.

`git diff --check` passed. No Cargo/build/test/list/runtime/GUI/provider/tmux/service command ran. Existing f55 test receipts remain pending; this change adds no passing receipt. Prior untracked audits and branches are preserved.

Only the reviewer may compile/run after release. Relevant existing proposed exact fixtures:

* `panels::kit_control_tests::sort_popover_keyboard_reselect_dismissal_and_refresh_preserve_focus`
* `panels::kit_control_tests::panel_rows_keyboard_ax_and_nested_actions_keep_exact_project_identity`
* `remote_prompt::kit_control_tests::remote_dialog_tab_traps_focus_and_busy_buttons_preserve_draft`
* `remote_prompt::kit_control_tests::remote_dialog_cancel_restores_invoker_after_subscribed_close`

Those component fixtures alone do not prove Workspace lifecycle. Integration review should also exercise two simultaneously visible Projects tabs, peer opening, retained identity/focus after refresh, hidden-tab/focus-mode retirement, project-ID reuse, external menu transitions and dialog Cancel/Hide under the single Root. Use synthetic/private fixtures only, drain dispatched effects before assertions, and verify exactly one restoration/trap owner. Native keyboard/AX/clipboard acceptance remains parent-owned and pending.
