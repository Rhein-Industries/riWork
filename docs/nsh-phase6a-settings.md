# NSH Phase 6a Settings and Project Settings

```yaml
assignment: 9cfd3947-ad83-4fde-9412-421383bdddd7
umbrella: 6ef6a636
branch: nsh-p3b-files
base: 4df3822dee9f4018be1670823ad49c2987cd6579
verification_environment: isolated_private_HOME_and_RIWORK_HOME_headless_fixtures_only
live_targets_touched: none_beyond_own_shell_and_worktree
effective_service_tier: default
fast: off
model: GPT-6.1-Sol
reasoning_effort: ultra
session_account: system-default
session_settings_changed: false
descendants: inherited_model_effort_account_and_fast_off_default_tier
task_status: left_to_orchestrator
```

The branch was first fast-forwarded from `4216a3a` to current `nsh` at `4df3822`.
That keeps the memo receipt fix, W2, quit ordering, draft identity, panel state and
Phase 5 work intact. This change adds the two forms and narrow native adapters;
core settings, account resolution, session launch and engine semantics are unchanged.

Global Settings now includes the account catalog and subscription balancing,
appearance and terminal-color preferences, tab controls and interface text size,
Files/Windows switches, agent and billing preferences, optional Haiku summaries,
orchestrator/provider/custom-model choices, delegation and status-bar editing.
The API-key row uses the existing engine validation and KeyStore, without putting
secrets in settings or snapshots. Orca import retains the original core preview
server-side behind an opaque id, so a client cannot submit its own import plan.
Computer use and Remote have integration slots; their assigned lanes own the content.

Project Settings separates explicit name/folder edits from immediate account,
orchestrator and delegation choices. Inline folders, locations and the existing
memo editor are included. Refresh and delayed receipts preserve newer edits,
including a folder A–B–A sequence. The form keeps its native input identity while
typing. Cmd+S saves metadata and the memo through one key owner; the original memo
autosave queue and close-flush behavior are retained.

Account inheritance is explicit: app selection uses null for system default and
an id for a saved account; projects persist `inherit`, `system_default`, or `saved`
with an id. Unavailable saved choices are disabled and diagnosed, and native writes
fail closed through the core account resolver. A missing saved pin remains visible.
The status-bar picker now uses the same project setter. Its changes never alter the
app selection or the account/home recorded on an already-running session.

New bridge surface (additive to existing schema 2 preferences):

- `Settings.get/set/subscribe` exposes the added form preferences and core-owned
  model/route choices. Nullable custom-model and delegation fields clear only the
  specified override. Unknown patches and invalid values are rejected before saving.
- `Settings.resetStatusBar()` and `setUsageMode(mode)` use core status defaults/rules.
- `ProjectSettings.get(projectId)` returns schema 1 form projections, inherited
  choices, sorted folder paths, unavailable-account and unreadable-value diagnostics.
- `Store.updateProjectMetadata`, `setProjectCodexAccount`, `setProjectOrchestrator`,
  `setProjectDelegationPolicy` publish the usual store snapshots after durable writes.
- `Accounts.list()` is cache-only. Existing refresh/selectDefault semantics are reused.
- `ApiKey.status/refresh/save/remove/subscribe` returns only saved/error state.
- `OrcaImport.preview()` returns counts/warnings/token; `run(token)` uses the exact
  retained core preview and its stale-source/destination checks.

| Parity rows | Coverage in this slice |
|---|---|
| PANEL-067–073, 075–077 | Project form, explicit/immediate save boundaries, native validation, nested folders, account scope, orchestrator/delegation, live refresh, locations, memo integration. |
| PANEL-074 | Roving enabled choice chips, ordinary input traversal, Enter and shared Cmd+S implemented; composition-aware Tab/Escape remains the pre-existing NSH input-hook gap. No live AX claim. |
| PANEL-131, 133–137 | Global frame/casing, width classes and section columns, immediate native writes, forward compatibility/strict rejection and event updates; core persistence semantics retained. |
| PANEL-132, 138 | Nine section positions and default-theme/API-key focus implemented; Cua/Remote content and Cua-primary focus belong to their lanes. |
| PANEL-143–149 | Global picker/availability/refresh/balancing preference and native account persistence. Launch-time balancing algorithm is unchanged. |
| PANEL-150–156, 158–164, 171–176 | Appearance, keyboard choices, text controls, Files/Windows, agent flags/billing, summaries, model validation, delegation and full status settings. |
| PANEL-157 | Match/step/reset behavior uses the existing core effective size. Reading the actual Ghostty font-size into the NSH host remains an inherited gap; this form does not invent a config parser or change font semantics. |
| PANEL-165–170 | Masked key editor, validation/failure handling, memory-store tests, status refresh and early API-key focus routing. Real Keychain and API-key-specific host menu checks were intentionally not run. |
| PANEL-179 | Preview/token/confirmation/error/completion UI and core import adapter; headless tests use a canned adapter and never run the installed Orca CLI. |
| PANEL-187–190 | Existing account/status projection retained; menu choices now write only project scope, preserve frozen sessions and still route Project Settings. |
| PANEL-139–142, 177–178 | Open for the Cua and Remote lanes; their surfaces were not implemented or operated here. |

Evidence root: `/tmp/riwork-nsh-p6a-9cfd3947.Gi2ljW`.
Builds used the worktree's private target directory (an APFS copy-on-write clone of
existing dependency artifacts, no shared target writes), `CARGO_BUILD_JOBS=2`,
`CARGO_NET_OFFLINE=true`, Rust/Cargo 1.99.1, preinstalled NSH 0.3.0 and TypeScript 7.0.2.
Heavy build commands were serialized. Cargo/Rustup read the existing tool caches.
No installation, reload, push, merge into `nsh`, network, Docker, operation of live windows,
real settings/account discovery or real credential access occurred.

The committed `phase6a_settings.py` runner creates a fresh `/tmp` fixture root,
sets a private OS HOME and RiWork home/runtime/tmp/tmux directories, strips injected
provider/home variables, seeds only invented account metadata and managed directories,
and enables native `test-support` only for its headless runner. That feature selects
MemoryStore for API keys and refuses actual Orca import. Ordinary debug builds
with a temporary process home use the same protection even without that feature;
unknown debug homes also fail closed. Import preview additionally rejects a home
that differs from the process-fixed home before calling the core constructor. No auth file is seeded.
The tmux directory must remain empty. Detailed argv are recorded in `commands.json`.

All commands ran from `/Users/dominik/orca/projects/riWork-nsh-p3b-files`.
The shared shell environment was:

```sh
export P6_VERIFY=/tmp/riwork-nsh-p6a-9cfd3947.Gi2ljW
export HOME=$P6_VERIFY/os-home
export RIWORK_HOME=$P6_VERIFY/state
export RIWORK_RUNTIME_DIR=$P6_VERIFY/runtime
export TMPDIR=$P6_VERIFY/tmp
export CARGO_HOME=/Users/dominik/.cargo
export RUSTUP_HOME=/Users/dominik/.rustup
export RUSTUP_TOOLCHAIN=1.99.1
export CARGO_TARGET_DIR=$P6_VERIFY/target
export CARGO_BUILD_JOBS=2
export CARGO_NET_OFFLINE=true
export NSH_CLI=/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/nsh-cli-0.3.0/bin/nsh
export NSH_TSC=/Users/dominik/Library/Caches/nsh/typescript-7.0.2-darwin-arm64/package/lib/tsc
unset CODEX_HOME RIWORK_CODEX_ACCOUNT_HOME ORCA_USER_DATA_PATH ANTHROPIC_API_KEY OPENAI_API_KEY RIWORK_CHAT_ID RIWORK_SHELL_ID RIWORK_RESTORE_TICKET
```

| Exact command | Final result | Evidence under root |
|---|---|---|
| `git merge nsh` | Exit 0, fast-forward `4216a3a..4df3822`. | Starting git history. |
| `cargo check --locked --offline -p riwork-nsh-native --features test-support` | Exit 0. | `cargo-check.log` |
| `cargo test --locked --offline -p riwork-nsh-native -p riwork-nsh-workspace` | Exit 0: 361 passed, 0 failed, 1 existing ignored (excluding nested child helper totals). | `cargo-tests-final.log` |
| `cargo test --locked --offline -p riwork-desktop` | Exit 0: 78 passed, 0 failed, 1 existing ignored (60 host unit, 11 AppKit, 6 CLI contract, 1 startup). | `desktop-review/desktop-suite.log`, `desktop-review/manifest.json` |
| `python3 crates/riwork-desktop/verification/phase6a_settings.py --root "$P6_VERIFY/focused-final" --target-dir "$CARGO_TARGET_DIR" --nsh "$NSH_CLI" --tsc "$NSH_TSC" --filter P6a` | Exit 0: check passed, 17 tests passed. | `focused-final-run.log`, `focused-final/nsh.xml`, `focused-final/nsh-test.log` |
| `python3 crates/riwork-desktop/verification/phase6a_settings.py --root "$P6_VERIFY/full-final" --target-dir "$CARGO_TARGET_DIR" --nsh "$NSH_CLI" --tsc "$NSH_TSC"` | Exit 0: check passed, 168 tests passed (151 existing + 17 Phase 6a). | `full-final-run.log`, `full-final/nsh.xml`, `full-final/nsh-test.log` |
| `cargo test --locked --offline -p riwork-engine file_preview::tests --lib -- --test-threads=1` | Exit 0: 14 passed. | `engine-preview.log` |
| `cargo test --locked --offline -p riwork-nsh-native --test settings_aux_isolation` | Exit 0: 1 new default-debug integration test passed, excluding its child helper. | `aux-isolation.log` |
| `python3 crates/riwork-desktop/verification/phase6a_settings.py --root "$P6_VERIFY/guard-final" --target-dir "$CARGO_TARGET_DIR" --nsh "$NSH_CLI" --tsc "$NSH_TSC" --filter P6a` | Exit 0: final native guard compiled, check passed, 17 tests passed. | `guard-final-run.log`, `guard-final/nsh.xml` |
| `cargo fmt --all -- --check` | Exit 0. | `fmt.log` |
| `sh apps/riwork/check-polling.sh` | Exit 0. | `polling.log` |
| `git diff --check` | Exit 0. | Final worktree check. |
| Python `ast.parse` of the new verification runner | Passed. | Local syntax-only check. |

NSH checks report public-export documentation warnings; there are no type errors.
During implementation, fixture runs exposed and corrected the unbound signal-setter
callback, input-remount typing bug, and a legacy assertion that assumed an empty saved
account catalog. Native test expectations were corrected to respect the core store's
existing materialization of absent default fields. The final results above are from
the corrected implementation. After the final fixture guard, the plain-debug
isolation test and engine suite were rerun; the final focused headless check passed as recorded in the table. These diagnostics and earlier failed runs remain in
`focused*`, `full`, and `cargo-tests.log` for auditability.


The plain-debug isolation regression starts an exact child test with an empty
environment, private HOME and RIWORK_HOME, and a deliberately nonexistent Orca
executable override. It verifies key save/validation/remove in memory, nonrecordable
fixture import previews and refused import confirmation without creating settings,
project-state or credential files. The engine dev dependency additionally refuses
Keychain calls, so a future regression cannot reach a real key during this test.

Review follow-up on `ed560af`: the desktop command above used the same `env.sh`
discipline with fresh directories under `$P6_VERIFY/desktop-review`: `HOME=os-home`,
`RIWORK_HOME=state`, `RIWORK_RUNTIME_DIR=runtime`, `TMPDIR=tmp`, `TMUX_TMPDIR=tmux`,
`XDG_CONFIG_HOME=xdg-config` and `XDG_CACHE_HOME=xdg-cache` (each relative to that
fixture root). The private Cargo target, offline mode and two-job limit stayed as
shown above. `RIWORK_GHOSTTY_INSTALL_DIR` was
`/Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install`,
the same pinned revision/artifacts used in prior host verification. All 605 files
retained identical SHA-256, byte counts and modification times, recorded in
`desktop-review/ghostty-before.json` and `ghostty-after.json`. The suite took
67.488 seconds; the tmux fixture stayed empty. AppKit checks used only their own
unshown fixture windows/objects, without ordering or activating windows or touching
a live application. Non-fatal linker-unwind and unused-helper warnings remain.
No implementation fix was needed. Effective service tier remains default, Fast off;
model, effort and account are unchanged. Live targets touched: none beyond the own
shell/worktree and isolated test fixtures. No merge, push, installation or reload.
