# Forms/search migration review receipt

Task `30c77170-9ae4-4dcb-9438-8ea9ba4d021d`; own RiWork-managed worktree only. Parent owns integration, runtime releases and completion. Foundation `ea05f94d566cdd952b778a2edafcb13f42a1a65f` is preserved as cherry-pick `3058780`. Dependency/adapter/init source stays foundation-owned; chat/dictation source stays chat-owned.

## Source coverage

All 21 assigned logical fields use retained actual Kit `InputState` entities with retained Window subscriptions and guarded foundation frames:

| Owner | Fields | State lifetime / domain boundary |
| --- | --- | --- |
| ProjectSettings | Project name, new folder name | Panel lifetime; external refresh checks live draft and marked composition; save/create reads live state |
| FolderEditor | New/rename name | Dialog lifetime; Enter event submits once; existing validation and Tab/Escape retained |
| ProjectCreator | Folder path, optional name | Dialog lifetime; derived path changes only until manual edit/browse; stale inspection and creation gates retained |
| SchedulePanel | Title, single-line prompt, exact date, repeat minutes, model, effort | All six created once per editor; hidden drafts retained; presets intentionally replace date; pure typed binding/options/revision/cadence logic retained |
| Workspace/panels | Shared navigation query | Separate persistent editor per navigation TabId; canonical query shared; focus checks actual child; terminal distinction retained |
| FileExplorer | Tree filter | Separate persistent editor per Files TabId through retained surface entities; query/tree shared, input geometry distinct; root/clear resets intentional |
| RemotePrompt | AddHost link/label; PairMac name/relay/routes; NewProject name | Conditional fields created once; secret state masked; supplied defaults and busy/errors retained |
| HandoffDialog | Optional model, single-line note | Dialog lifetime; suggestions intentional replacements; length limits/busy/error/cancel retained |

Migrated owners no longer implement an independent EntityInputHandler or invoke the legacy handler macro. Shared `project_settings::Input`, `input_content` and macro remain compatible for chat/dictation/legacy tests. Per-render frames never replace/recreate editing state; presentation-only pending/disabled changes are retained. Existing palettes/scaling/native shortcut routing remain application-owned. No scheduler/provider/service source changes.

## Verification status and limits

Source formatting and `git diff --check` completed. No tests executed, listed, or native/GUI acceptance claimed. The earlier foundation-only inherited-environment `cargo check --locked` completed before incident hold; it is historical evidence, not isolated validation of this migration.

The parent has now released ONLY offline locked check, binary build, and test-executable compilation under a fresh child whitelist. Result/log/environment attestation will be appended after those commands. Missing offline native/Cargo dependencies stop compilation; there is no fetch/install fallback. A private Ghostty system-package directory forces Zig's documented offline mode, because Cargo offline alone does not control build-script fetches.

Native focus, visual geometry, real IME and Ghostty/context shortcut acceptance remain pending parent-controlled Cua ownership. Headless tests remain pending an exact allowlist/helper review and separate execution release.

## Proposed execution allowlist, not authorization

New inert tests (qualified Rust names):

- `form_input::tests::original_form_and_workspace_paste_policies_remain_distinct`: pure paste normalization -> preserved single_line helper; no fixture or filesystem.
- `file_explorer::kit_filter_tests::kit_filter_persists_selection_and_owns_enter_escape_and_tab`
- `file_explorer::kit_filter_tests::kit_duplicate_filters_keep_distinct_persistent_geometry`
- `remote_prompt::kit_form_tests::kit_project_name_has_one_submit_and_preserves_composition`
- `remote_prompt::kit_form_tests::kit_pairing_secret_is_masked_and_busy_keeps_its_draft`
- `remote_prompt::kit_form_tests::kit_pair_fields_retain_supplied_defaults`

The GPUI tests call `form_input::test_window` -> TestAppContext stub window, Settings defaults, synthetic Appearance, `text_input::init`, then FileExplorer::new without root/filesystem or RemotePrompt::new with inert Err backend. Filter events touch only tree/query/focus. Remote NewProject emits to an in-memory subscription only; pairing tests use synthetic text/defaults and never submit pairing. No Workspace startup, SessionManager, native platform app, shell/server/provider/relay construction in these new helper chains.

Existing pure candidates: `project_settings::tests::choosing_an_account_never_claims_pending_edits_were_saved`; `project_settings::tests::a_multi_line_input_keeps_its_line_breaks_where_a_single_line_one_folds_them`; creator resolve-folder/shortcut cases. Six unchanged `schedule_panel::tests` domain regressions are preserved. LegacyEditorFixture constructs SessionManager and schedules/chat fixtures: those tests require further parent helper audit and are explicitly excluded from any initial execution allowlist. No broad cargo test filters.

After a separate parent release, proposed boundary is one compiled test executable invoked with one qualified name and `--exact --test-threads=1`, through the same fresh env whitelist/private paths used by compilation. No executable invocation, even `--list`, occurs under the current release.
