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

## Isolated compilation receipt — stopped on missing dependency

Source implementation commit: `2082aea`. Only the following released Cargo command ran, through `/usr/bin/python3 /tmp/rwf-iynxctm6/run.py check` and an explicitly constructed subprocess environment:

```text
/Users/dominik/.rustup/toolchains/1.95.0-aarch64-apple-darwin/bin/cargo check --offline --locked
```

Started `2026-10-06T12:28:42.337876+00:00`; completed `2026-10-06T12:28:57.477598+00:00`; exit **101**. Compilation stopped in gpui-libghostty's build script before this migration was type-checked. Zig `--system` reported missing package `uucode-0.2.0-ZZjBPlK5VADj7fdoq7G8LIHzD5o6FSkcBXXrRWr4jnrA` in `/tmp/rwf-iynxctm6/zigsys`. No fallback, fetch/install, cache cleanup or additional compilation command was attempted. Binary build and test-executable compilation remain **not run**. No tests/listings/native application/GUI/services/production interactions occurred under this release. Runner and Cargo child exited naturally; no started child remains tracked as running. Build slot remains this worker's pending parent final receipt.

Runner root `/tmp/rwf-iynxctm6` and its private directories are mode0700. The explicit child environment inherited zero keys; before/after equality checks confirmed the runner's parent environment remained byte-for-byte unchanged. Approved Cargo/rustc/rustdoc and Zig paths are absolute. CARGO_HOME is the permitted shared cached-read/lock directory; CARGO_TARGET_DIR is the assigned owned existing target. No existing fixtures/logs were removed. This is environment isolation, not an OS sandbox.

Preserved evidence:

- `/tmp/rwf-iynxctm6/run.py`: exact runner and approved-command mapping (only check actually invoked).
- `/tmp/rwf-iynxctm6/check.log`: exact argv, environment, timestamps and native failure.
- `/tmp/rwf-iynxctm6/check-result.json`: exit/status/attestation.
- `/tmp/rwf-iynxctm6/env-attestation.json`: start attestation.
- Historical earlier foundation-only log `/tmp/riwork-forms-foundation-check.log` preserved.

Exact whitelist for the failed check:

```json
{
  "HOME": "/tmp/rwf-iynxctm6/home",
  "RIWORK_HOME": "/tmp/rwf-iynxctm6/rw",
  "RIWORK_RUNTIME_DIR": "/tmp/rwf-iynxctm6/rt",
  "TMPDIR": "/tmp/rwf-iynxctm6/tmp",
  "CODEX_HOME": "/tmp/rwf-iynxctm6/codex",
  "CLAUDE_CONFIG_DIR": "/tmp/rwf-iynxctm6/claude",
  "XDG_CONFIG_HOME": "/tmp/rwf-iynxctm6/config",
  "XDG_CACHE_HOME": "/tmp/rwf-iynxctm6/cache",
  "XDG_DATA_HOME": "/tmp/rwf-iynxctm6/data",
  "XDG_STATE_HOME": "/tmp/rwf-iynxctm6/state",
  "XDG_RUNTIME_DIR": "/tmp/rwf-iynxctm6/xdgrun",
  "GHOSTTY_NATIVE_CACHE_DIR": "/tmp/rwf-iynxctm6/native",
  "GHOSTTY_ZIG_PACKAGE_CACHE_DIR": "/tmp/rwf-iynxctm6/zigpkg",
  "GHOSTTY_ZIG_GLOBAL_CACHE_DIR": "/tmp/rwf-iynxctm6/zigglobal",
  "GHOSTTY_ZIG_SYSTEM_PACKAGE_DIR": "/tmp/rwf-iynxctm6/zigsys",
  "ZIG_GLOBAL_CACHE_DIR": "/tmp/rwf-iynxctm6/zigglobal",
  "ZIG_LOCAL_CACHE_DIR": "/tmp/rwf-iynxctm6/ziglocal",
  "CLANG_MODULE_CACHE_PATH": "/tmp/rwf-iynxctm6/clang",
  "SWIFT_MODULE_CACHE_PATH": "/tmp/rwf-iynxctm6/swift",
  "PATH": "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin",
  "CARGO_HOME": "/Users/dominik/.cargo",
  "RUSTC": "/Users/dominik/.rustup/toolchains/1.95.0-aarch64-apple-darwin/bin/rustc",
  "RUSTDOC": "/Users/dominik/.rustup/toolchains/1.95.0-aarch64-apple-darwin/bin/rustdoc",
  "ZIG": "/Users/dominik/.local/share/riwork/toolchains/zig-0.16.0/zig",
  "CARGO_TARGET_DIR": "/tmp/riwork-gpui-kit-forms-30c77170-target",
  "CARGO_BUILD_JOBS": "2",
  "MACOSX_DEPLOYMENT_TARGET": "13.0",
  "CARGO_NET_OFFLINE": "true",
  "LC_ALL": "C"
}
```

Needed parent-reviewed offline cache provision: the Ghostty package closure copied into a private `--system` directory, or a verified native archive cached under the build script's fingerprint `b1b8e8c04734504b2bf3bf7553ee247f` in the private native cache. The existing target archive has not been manipulated to bypass this failure. Any provision/retry waits for parent direction. After the dependency is available, needed commands remain the released check, `cargo build --offline --locked --bin riwork`, and optionally `cargo test --offline --locked --bin riwork --no-run --message-format=json`, using the absolute approved Cargo and the recorded whitelist. No executable invocation follows compilation without a separate exact-test/helper release.

The six preserved panel domain names are:

- `schedule_panel::tests::legacy_editor_same_chat_reselection_saves_a_chat_binding`
- `schedule_panel::tests::legacy_editor_scope_changes_rebind_only_the_matching_app_or_project_chat`
- `schedule_panel::tests::legacy_editor_ordinary_shell_selection_stays_explicit_and_never_falls_back`
- `schedule_panel::tests::editor_defaults_and_options_edit_keep_the_pinned_account_and_destination`
- `schedule_panel::tests::quick_first_run_choices_select_future_instants_without_editing_other_fields`
- `schedule_panel::tests::custom_zero_minutes_cannot_become_a_one_time_schedule`

The first three use LegacyEditorFixture -> TestHost, SessionManager::at, Store, ScheduleStore, chat-host fixture creation; its Drop calls `sessions.kill_server()`, and the ordinary-shell case additionally calls `sessions.create`. They remain excluded pending parent audit, regardless of private environment. The last three are pure editor/domain computations; parent still owns exact execution release.
