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

## Compile API correction before reviewer follow-up

Parent authorized copying audited Ghostty fingerprint/package cache sources. Copy attestation `/tmp/rwf-iynxctm6/cache-copy-attestation.json` records canonical nonoverlap, every file SHA-256/size and unchanged originals; internal relative dylib links were checked to remain within each subtree and preserved. Native archive SHA-256 `75443f9cd0b74a0b41f51b214bbc1cc00c10b94d10710302f5eca9971f157b66`; the installed target archive matched it. Private cache reuse skipped the Zig rebuild. The first copied-cache check found a placeholder accessor error; corrected to `state.presentation().placeholder()` and removed obsolete Files/Workspace imports.

`cargo check --offline --locked` succeeded at 12:32:25 UTC, and `cargo build --offline --locked --bin riwork` succeeded at 12:33:43 UTC, through the absolute approved Cargo and unchanged child whitelist. Initial failed attempt/logs preserved; new attempts have timestamped result/attestation files, while check.log appends. Four legacy helper/macro compatibility warnings remain. These passes precede the independent review corrections and are not final-candidate acceptance. No test compilation/list/execution had occurred at that point.

## Independent forms follow-up corrections

Read the entire Forms adapter follow-up section of parent-supplied `GPUI_KIT_REVIEW_REPORT.md` and the exact six-name supplement allowlist. Parent now authorizes compiled listing and only those six exact tests after effect-drain corrections; no other test/fixture family is released. Shared Escape policy is foundation-owned and its new commit is pending. No competing Escape action bridge was added here; no composition Escape acceptance is claimed.

- All form-owned deferred value and placeholder replacement bridges were removed. Window is threaded through creator inspection callbacks, settings save/refresh/folder creation, model suggestion clicks, Files root/reveal/clear boundaries and necessary Workspace/terminal-link callers. Writes are synchronous, so they cannot apply an old queued text value after newer input or a closed owner. Window-aware async callbacks use update_in against their live owner/window. The terminal_links/ui diff only forwards Window to these form-owned boundaries.
- External settings name refresh uses exact equality, a sticky user-touched bit (including whitespace, edit/undo ABA and marked edits), and a live marked-text guard. The bit resets only on explicit successful save. Model text is read from the actual editor after the refresh attempt; no domain snapshot advances in advance of a deferred editor write.
- Sibling search synchronization requires an acknowledged exact previous value, no actual focus and no marked text. Base replace_all preserves undo; selection/scroll are restored. One counted mirror Change is ignored by the owner so it cannot publish a second canonical update. Actual focus/Enter use the child live value. Explicit project/root/clear resets remain synchronous intentional domain resets.
- Workspace subscriptions verify TabId plus entity membership and reject stale Focus/Blur according to current child focus. Removed tabs prune mirror metadata and subscriptions. Files retains a separate initial fallback, verifies every event/source's membership, prunes stale metadata/subscriptions, and replaces an active removed-tab reference without stealing focus.
- All six released tests now finish event-producing Window updates, drain outside that borrow and assert in later updates. Remote one-submit also counts enclosing Enter actions/raw keys. Each remains root=None or backend=Err with the same inert helper boundary.

Additional **held, source-only** exact names:

- `form_input::tests::synchronous_replacements_and_refresh_preserve_newer_whitespace_and_aba_drafts`: inert DraftFixture -> test_window Settings/Appearance/Base init only; synchronous metadata A/B, save-then-type, exact whitespace and touched edit/undo checks. No Store/service/filesystem.
- `file_explorer::kit_filter_tests::kit_removed_filter_rejects_stale_events_and_reopens_without_subscriptions`: standalone explorer root=None, strong stale child handle, closure/membership/metadata count checks; no filesystem/host/process.
- `file_explorer::kit_filter_tests::kit_visible_filters_protect_composition_history_and_focus`: two rendered FileExplorerSurface entities over root=None; distinct headless bounds, shared query/history/undo, synthetic marked handler and focus checks. Does not mount Workspace or a native application.

These new names may be compiled/listed but **must not execute** until parent source/helper gate. Full Workspace project/layout/drag/resize and native IME coverage remains pending; standalone headless tests do not establish it. Correction to earlier domain audit prose: editor_defaults_and_options_edit_keep_the_pinned_account_and_destination is a private Store/filesystem fixture (including remove_dir_all), not pure computation; it remains excluded with all six schedule panel tests.

## Released final checks and six exact tests

Correction commits:

- `9c20d77b2ba79b9c549564036e45c544c57506c6`: compile API correction and initial copied-cache compile receipt.
- `a64df0d38bd6bf60e4a0eb1adb0966879f5f01aa`: independent review fixes, synchronous Window boundaries, protected mirrors/membership and effect drains; adds three held regressions.
- `1a83251c63a55a266c41337a69c522753da48fe0`: cfg(test)-only accessor correction for the newly held visible-filter test. Production source is unchanged from the checked/built a64df0d candidate.

Through the same absolute approved Cargo/runner whitelist:

| Command suffix | UTC start / finish | Result |
| --- | --- | --- |
| `check --offline --locked` | 12:43:35.999593 / 12:43:42.941083 | exit0 at a64df0d |
| `build --offline --locked --bin riwork` | 12:43:46.373851 / 12:44:00.766143 | exit0 at a64df0d |
| `test --offline --locked --bin riwork --no-run --message-format=json` | 12:45:11.551941 / 12:45:33.353977 | exit0 at 1a83251 |

No download/install occurred. Final test executable: `/private/tmp/riwork-gpui-kit-forms-30c77170-target/debug/deps/riwork-7d8133c829b1c91f`, SHA-256 `c6a1ff064a2f1a8e5b17083de89d3a0a20d506467562e039607418ac93fe843a`. This executable is the Rust test harness, not a native RiWork app launch. Parent-released `--list` succeeded and found exactly one compiled test for each released qualified name. Listing contains 1683 tests; no unselected test was executed. The three new names were only compiled/listed.

Parent released ONLY the six names in `/Users/dominik/orca/projects/riWork-review-manu-20261005/.review/forms-source-20261006T122449Z/supplements/20261006T123202Z/proposed-test-allowlist.json`. Runner `/tmp/rwf-iynxctm6/exact.py` uses the binary directly with one qualified name, `--exact --test-threads=1`, and the same explicit private env dictionary. It enforces one listing match and one selected/passed test, rejects zero/extra/ignored selections, records binary and allowlist hashes and verifies unchanged parent environment before/after each child.

| Exact qualified name | Passed / failed / ignored / filtered | Exit |
| --- | --- | --- |
| `form_input::tests::original_form_and_workspace_paste_policies_remain_distinct` | 1 / 0 / 0 / 1682 | 0 |
| `remote_prompt::kit_form_tests::kit_project_name_has_one_submit_and_preserves_composition` | 1 / 0 / 0 / 1682 | 0 |
| `remote_prompt::kit_form_tests::kit_pairing_secret_is_masked_and_busy_keeps_its_draft` | 1 / 0 / 0 / 1682 | 0 |
| `remote_prompt::kit_form_tests::kit_pair_fields_retain_supplied_defaults` | 1 / 0 / 0 / 1682 | 0 |
| `file_explorer::kit_filter_tests::kit_filter_persists_selection_and_owns_enter_escape_and_tab` | 1 / 0 / 0 / 1682 | 0 |
| `file_explorer::kit_filter_tests::kit_duplicate_filters_keep_distinct_persistent_geometry` | 1 / 0 / 0 / 1682 | 0 |

All six passed; parent environment equality held for every Cargo, listing and exact-test child. The helper boundaries remain headless: Files root=None/empty tree, RemotePrompt backend=Err with no Workspace subscriber and only synthetic secrets/defaults. No six schedule fixtures, providers, SessionManager, services, GUI, app, host, remote/iOS action or broad test filter ran. All started/tracked child commands exited naturally; no process or fixture cleanup followed.

Evidence preserved: `check.log`, `build.log`, `compile-tests.log`, `list.log`, `exact-0.log` through `exact-5.log`, timestamped per-command result/env files, `six-test-receipt.json`, `cache-copy-attestation.json`, `installed-native-attestation.json`, `run.py` and `exact.py` under `/tmp/rwf-iynxctm6`. Original failed check log/attestation/result and previous fixtures/logs remain intact.

Current source hashes (compiled at 1a83251):

- `Cargo.lock`: `6badaa76ede26da688bc5874bf2c9cf957b464f27849fe09396fc18998bfdc17`
- `src/form_input.rs`: `6727d94fcda78f30ec02cc539f279ff07d05a88a0c7ec0fda4d924cc3c04f429`
- `src/file_explorer.rs`: `2224869c23384de69c4ea59cc831453b84a8ca192716db243b5da0261b77fad4`
- `src/remote_prompt.rs`: `b837dedc8a42671d5e45a59f78beaa5c36bceac51c34a8c8c160a914ab4c942f`

Remaining exact newly added execution names, **held pending parent source/helper gate**:

1. `form_input::tests::synchronous_replacements_and_refresh_preserve_newer_whitespace_and_aba_drafts`
2. `file_explorer::kit_filter_tests::kit_removed_filter_rejects_stale_events_and_reopens_without_subscriptions`
3. `file_explorer::kit_filter_tests::kit_visible_filters_protect_composition_history_and_focus`

Shared composition Escape fix is still awaiting the foundation owner’s supplied commit; it was neither edited independently nor accepted by these six tests. No composition Escape acceptance or final combined-candidate/native acceptance is claimed. Parent owns cherry-pick coordination, integration and completion. Native Cua ownership remains held.

**Build slot released with this final receipt. Worker returns idle; no tracked running child remains.**
