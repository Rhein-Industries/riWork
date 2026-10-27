# NSH Phase 1 consolidation

**Phase 1 is still not accepted.** The three fix branches are merged on
`nsh-p1-consolidate` and every rerun suite is green. The 101-item M5 matrix
now stands at **81 PASS, 7 PARTIAL, 12 FAIL and 1 report-only**. The
remaining failures are the unowned global window (§5.4) and two polling and
performance items (§5.12).

```yaml
assignment: c12f5e14-eaa3-4db4-afe6-50d9aa9cc22f
branch: nsh-p1-consolidate
base: 69365c6 (nsh, with Phase 2a terminals and Phase 3a panels)
merges:
  - aee6a4b  nsh-p1-fix-native     (W3 c69fa43)
  - 257fcc2  nsh-p1-fix-host       (W2 adea72c)
  - d416e2a  nsh-p1-fix-workspace  (W1 1066bac)
verification_environment: isolated /tmp homes, runtime and tmux; fixture PID only for read-only Cua
live_windows_or_state_touched: none
gpui_deletions: none (142 protected files byte-identical to ea0f2f5)
merge_push_install: none (local commits only)
task_status: unchanged; owned by orchestrator
```

Evidence is under [verification/nsh-p1-consolidation](verification/nsh-p1-consolidation/).
[m5-consolidated-items.json](verification/nsh-p1-consolidation/m5-consolidated-items.json)
lists all 101 items, with the M5, worker and consolidated dispositions and how
each was re-verified.

## Merges and conflict decisions

Each merge was built and its affected suites run before it was committed. The
commit messages list every conflict file.

### 1. W3 native (14 conflicts)

| Area | Decision |
|---|---|
| `Appearance` module | Phase 2a and W3 both added one. Exactly one module remains: W3's schema 2 `AppearanceDto`, extended with the terminal phase's `terminalLight`, `terminalDark` and `tabCloseBehavior`. The terminal fields come from core `terminal_override`; for every theme this matches the old Phase 2a projection. The schema 1 `snapshot()` and its separate hub were removed. |
| Golden | W3's appearance golden is byte-identical. The test checks the terminal fields against the same core override. The Phase 2a preset test moved into `appearance.rs`. |
| `settings.json` | One watcher flag (`SETTINGS`) drives `PreferenceService`, which publishes both Settings and Appearance. |
| `Sessions.create` | W3's strict decoding and canonical-UUID validation apply before any backend work. Phase 2a's optional `cwd` is kept: when omitted, the worktree or project root is used. |
| `Workspace.tsx` | Keeps W3 theming, StatusBar and the notice overlay alongside the terminals' LocalTerminal, detach/exit, route handlers and the `+ Shell` control (now a StatusBar slot). |

### 2. W2 host (11 conflicts)

| Area | Decision |
|---|---|
| Host lifecycle | W2's restart handoff, flush barriers, remembered geometry, focus flags and host text-size route are kept. Restore readiness also captures the panels' switched project identity. Terminals' link opening and W3's fonts and appearance pinning are unchanged. |
| `HostLayout` | Keeps the panels/W3 lock-free generations. W2's window size moved from a `Mutex` into an `AtomicU64`, so engine-thread publication never takes a cross-thread lock. A project switch keeps the captured size. |
| Chrome appearance | W2 had an interim `chrome::Appearance.get()`. It was removed so only one `Appearance` module remains. Chrome now reads W3's palette, whose golden already encodes the unpainted Native/Hermes islands. |
| Layout model | Panels' `open_panel` gained W2's rule that Files opens in the navigation pane. W2's `split_active`/`close_active` were added. W3's core default layout was kept, and W2's default-layout test passes against it. |
| UI | W2's controls island, drag regions and command dispatcher replace the separate title row. The panels toolbar moved below the dock. Title and Dock label follow panel project switches. One W2 test assertion was adapted, because W3's status bar has no pane count. All tabs and the interim close dialog became overlays, so Escape reaches the topmost one. |
| Features | W3's probe crate no longer requests native `test-support`. That feature now enables fixture seams, and feature unification would otherwise pull them into workspace-wide builds. |

### 3. W1 workspace (12 conflicts and the `Workspace.tsx` rewrite)

| Area | Decision |
|---|---|
| `LayoutSession` | W1's session (data, adoption, initial shell, focus, pending chats, shared-strip edits) sits on the panels' injectable layout repository and reservation/commit project switch. W1's post-switch steps run after the commit. |
| Persistence | W1's opaque-ID remap and manual test clock moved onto W3's async save worker. `LayoutBackend` gained `schedule_with_remap`. |
| Model | One `default_layout`: W1's, which relocates opaque tabs using the remapped saves. This supersedes W3's earlier refusal, which existed only because core could not migrate opaque tabs. One `open_panel`: the GPUI rule (main pane, otherwise active pane), plus W2's Files rule. W1's shell detach now also records the former pane, so the terminals' reopen rule still applies. |
| Native service | W1 had dropped the Settings and Tabs watcher flags; they are restored, and W1's layout wake hook is added. The process `Tabs` service now reads through W1's adapter. Before this, a debug fixture inventory and the real tmux inventory reconciled one TabStore against each other. |
| UI | W1's panes, shared strip, tab and pane menus, rename, drag, `CloseDialog` and focus presentation now run on the consolidated Workspace with Panels, LocalTerminal, W3 theme and status bar, and W2 chrome. All keys and menu commands go through W2's dispatcher, which accepts both W1 and W2 command IDs. W1's shared-strip numbering is wired into W2's `selectTab1-9`. The W3 layout menu gained W1's focus rows. |
| Fixtures | W2 keeps `p1_fixture_home`; W1's generator became `p1_workspace_fixture_home`, which W1's runner now calls. W1's metrics test became `workspace-metrics.test.ts`. |
| Tests | W1's `layout_session` fixture runtime is now multithreaded, because the save worker is a Tokio task, as in production. W3's fake-backend test uses a disposable home. One panels test waits for its own pending call. |

### Vendored `nsh-base` (W1)

Checked independently: `apps/riwork/vendor/nsh-base` is exactly the published
`nsh-base` 0.3.0 from the Cargo cache with `RIWORK.patch` applied (4 files,
+134/−15). The only extra files are `RIWORK.md`, `RIWORK.patch`,
`LICENSE-APACHE` and `.gitattributes`. The patch:

- adds `divider_size`, which defaults to 0 (the upstream behaviour);
- adds `render_tab_bar_suffix`, which is unused unless a renderer is supplied, plus classic-tab reveal;
- adds the upstream test dev-dependencies.

It is patched in both the root `[patch.crates-io]` and `.cargo/config.toml`;
the latter covers NSH's nested runner workspaces. Provenance and the rule
"keep pinned until upstream has equivalent geometry/suffix" are in
`vendor/nsh-base/RIWORK.md`.

## Preserved behaviours

| Invariant | Evidence |
|---|---|
| Panels CRUD and project switching | `panel_crud` 4/4; Panels/App headless tests in the 91 |
| Production terminals and teardown | `riwork-nsh-terminal` 17/17; the terminal detach/reopen and native-node identity headless tests; desktop terminal tests |
| M5 fixes | `preserve_order` golden, quiet empty inventory, stable DockArea, titlebar coordinates, deferred commands |
| CLI byte parity | 5/5 commands byte-equal (stdout, stderr, exit); CLI sources identical to `ea0f2f5`; settings insertion-order golden passes |
| Exactly one `riwork/native` registry | `crates/riwork-nsh-native/src/lib.rs`. Two other registrations exist: W3's test-only probe crate and an old spike. There is one `Appearance` module. |
| Quiet guard | Native 60-second quiet home (services 15/15); NSH Idle virtual 60-second test; polling guard: no matches |
| Write allow-list | The two-window fixture wrote only `layouts.json`, `project-tabs/*`, `nsh/desktop.handoff.lock` and `run/desktop.lock`. There is no `appearance.json` and there are no tmux files or sockets. |
| Deletion embargo | No file deleted relative to `69365c6`; W3 structural audit: 142 protected GPUI files byte-identical |

## Final verification (HEAD `d416e2a` code)

The environment is in [env.sh](verification/nsh-p1-consolidation/env.sh): Rust
1.99.1, `--locked --offline`, pinned NSH CLI 0.3.0 (`76fa8a7`, SHA-256
`90b76…2a60`), and the pinned Ghostty install copied into the worktree. State,
runtime, tmux and TMPDIR live under `/tmp/rw-c12f.*`. A short root is required
because macOS Unix-socket paths are limited to 104 bytes; a long scratch path
made the CLI chat-socket test fail. Test `HOME` is
`target/nsh-integration-home-c12f`.

| Check | Result |
|---|---|
| `cargo test --no-fail-fast` for core, engine, cli, desktop, native, workspace and terminal | **1981 passed, 0 failed, 189 ignored** (the ignores are pre-existing in cli/engine/core) |
| Same desktop/native/workspace crates with `riwork-nsh-native/test-support` | **292 passed, 0 failed** |
| `riwork-engine --features test-support runtime::tests::` | **32 passed, 10 ignored**; W2's four watchdog cases run with `--ignored`: **4/4** |
| `nsh check apps/riwork` | Passed (the informational doc-comment warning remains) |
| `nsh test apps/riwork` in W1's generated p1-layouts home with fake sessions | **91 passed, 0 failed**; no tmux files |
| Native probe `nsh check` / `nsh test` (`crates/riwork-nsh-native/tests/app`) | Passed; **1/1** |
| Integration build | Desktop and CLI built; NSH bundle built; `cargo tree` found no GPUI or Ghostty package |
| CLI parity | 5/5 byte-equal: `project list`, `settings list`, `instances`, `appearance`, unknown command |
| Polling guard, rustfmt, `git diff --check`, W3 structural audit | Passed; no UI polling, native storage, appearance publisher, or core/engine serde change |

Per-merge results, all in isolated homes:

| Merge | Cargo | NSH headless |
|---|---|---|
| W3 | 434 passed, 0 failed | 45/45 |
| W2 | 464 + 171 (test-support) + 32 (engine runtime), 0 failed | 63/63 |
| W1 | 598 + 291 (test-support) passed. The only failure, W3's fake-backend test, was fixed and rerun 1/1. An earlier run hung in `layout_session` until its runtime fix (rerun 18/18). | 91/91 |

### Two-window isolated fixture (read-only Cua)

Started with
`phase1_integration.py gui --workspace-fixture --theme native` (W1's p1-basic
Alpha/Beta home with fake sessions, debug build, clean environment,
`RIWORK_NSH_BACKGROUND=1`). The fixture ran as PID **7875**, with two runtime
windows registered under that one PID. Cua.ai Driver was used read-only and
only on that PID; no input calls were made ([cua.json](verification/nsh-p1-consolidation/fixture/cua.json)).

- **Windows:** `RiWork · Alpha` and `RiWork · Beta`, both 1220×780. Beta is offset (22,22) from Alpha.
- **Close control:** its AX offset is (9,7), within ±1 of (10,8).
- **Alpha:** the shared strip lists panels first, then named sessions including `Chat 2, waiting`. W1 pane controls, the panels, the toolbar and the W3 status bar render as expected ([screenshot](verification/nsh-p1-consolidation/fixture/cua-alpha.png)).
- **Beta:** keeps classic strips ([screenshot](verification/nsh-p1-consolidation/fixture/cua-beta.png)).
- **Shell pane:** the selected shell shows "unknown shell…", because fixture sessions have no tmux backing.
- **Cleanup:** SIGTERM (exit −15), which is fixture cleanup, not proof of a graceful quit. No fixture process remained.

## M5 section summary (actual consolidated dispositions)

How to read the table:

- "Worker" is the latest disposition from the owning fix branch, or the M5 result where no branch owned the item.
- "Consolidated" is the disposition on this branch after the reruns above.
- **Carried** means the automated successors were rerun here, but part of the evidence is a worker GUI route that was not repeated (see below).

| Section | M5 PASS | Worker PASS / PARTIAL / FAIL | Consolidated PASS / PARTIAL / FAIL | Carried |
|---|---:|---|---|---:|
| 5.1 Process and CLI | 4 | 5 / 2 / 0 | **5 / 2 / 0** | 1 |
| 5.2 Restart and restore | 2 | 3 / 1 / 0 | **3 / 1 / 0** | 1 |
| 5.3 Windows and chrome | 1 | 8 / 2 / 0 | **8 / 2 / 0** | 3 |
| 5.4 Global window | 0 | 0 / 0 / 10 | **0 / 0 / 10** | 0 |
| 5.5 Panes | 0 | 12 / 0 / 0 | **12 / 0 / 0** | 0 |
| 5.6 Tabs, strip, close dialog | 0 | 10 / 0 / 0 | **10 / 0 / 0** | 0 |
| 5.7 Drag and identity | 0 | 4 / 0 / 0 | **4 / 0 / 0** | 0 |
| 5.8 Layout persistence | 1 | 9 / 0 / 1 | **9 / 1 / 0** | 0 |
| 5.9 Menus and keys | 2 | 7 / 2 / 0 | **8 / 1 / 0** | 0 |
| 5.10 Themes and text | 1 | 10 / 0 / 0 | **10 / 0 / 0** | 1 |
| 5.11 Status bar | 0 | 5 / 0 / 0 | **5 / 0 / 0** | 0 |
| 5.12 Polling and performance | 2 | 2 / 0 / 2 (+1 report-only) | **2 / 0 / 2 (+1 report-only)** | 0 |
| 5.13 `riwork/native` | 0 | 5 / 0 / 0 | **5 / 0 / 0** | 0 |
| **Total** | **13** | **80 / 7 / 13** | **81 / 7 / 12** (+1) | **6** |

How these add up:

- **Expected:** 13 baseline, plus W1's 34, W2's 23 and W3's 20 PASS items, less the overlaps. W2's 23 include 9 baseline passes, W3's 20 include THM-06, and W1 claims 34 new. That gives 13 + 14 + 34 + 19 = **80**.
- **Consolidation:** two items moved.
  - **P1-MENU-03: PARTIAL → PASS.** W2's only residual was that ⌘T/`newTab` had no creation handler. ⌘T now reaches `LayoutSession.newShell` through the dispatcher, and the headless CloseDialog and Panes tests create shells with it. The shell is created through W1's session adapter, a known divergence (see open items).
  - **P1-LAY-07: FAIL → PARTIAL.** Host restart now flushes the host layouts before launch (W2), and the workspace barriers pass. The end-to-end replacement-process run (RLD-01) was not executed on the consolidated tree.
- **Re-verification:** 75 PASS items were fully re-verified here, by automated reruns and, for PROC-06, WIN-01, WIN-02, WIN-05, STRIP-02 and THM-06, the fixture above. Six PASS items still rely partly on worker GUI evidence that was not repeated:
  - PROC-03: W2's real invalid-home launch
  - RLD-02: W2's fail-flush fixture
  - WIN-03: W2's maximized restore
  - WIN-06: W2's seven-theme screenshots (only Native was observed here)
  - WIN-08: W2's real close/quit apps
  - THM-05: W3's light-mode Cua fixture

### Partial items

- **P1-PROC-04:** the missing-folder notice could not be found with the prescribed external AX query (W2).
- **P1-PROC-05:** D-PROC-1, the open output decision, remains open.
- **P1-RLD-01:** the restore works, but the mandated AX tab-label comparison returns no tabs (W2).
- **P1-WIN-07, P1-WIN-09:** tooltips do not hide on move, deactivation or project switch; no public runtime hook exists for it (W2).
- **P1-KEYS-02:**
  - Done: shared-strip numbering is now installed (W1's `selectNumbered`, exercised by `Strip.test` ⌘9).
  - Still open: the manual desktop Ctrl-Tab action was not performed, because desktop input was not authorized. The automated AppKit Ctrl-Tab test passes.
- **P1-LAY-07:** see above.

### Failing items (unchanged)

§5.4 GLB-01 to GLB-10: no branch owned the global-window content. §5.12
POLL-01 and PERF-02: streaming coalescing and gesture counters were not
claimed. PERF-01 remains report-only (D-PERF-1).

## Open items and follow-ups

- **Session-write divergence.** W1's `newShell`, `closePanel(exit)`,
  `renameTab` and `moveTab` write sessions and TabStore membership through the
  workspace-private `session_io`. Under the §1.10 Phase 2a contract, creation
  and exit go through `Sessions`. The semantics match: hide before exit, refuse
  on a failed hide, and record the detached identity. The UI uses one path per
  action. The adapter also samples sessions once per layout session on watcher
  wakes, rather than once per process. Recorded in `docs/nsh-contracts.md`.
  Routing these writes through `Sessions`/`Tabs` is a follow-up.
- **Two fake-session schemas share `RIWORK_NSH_FAKE_SESSIONS`.**
  - W2's `test-support` FakeSessions: only when `test-support` is enabled.
  - W1's `session_io`: debug or `test-support` builds, isolated `/tmp` homes only.
  - Nothing here enables both against one file, but they should be unified.
- **Two core default-layout implementations.** The model uses W1's
  `workspace::default_layout`. W3's `apply_default_layout` remains with its
  core tests and should be merged into one entry point.
- **Notice overlay over terminals.** Terminals freeze for menus, dialogs, the
  status menus, All tabs, editors and switches. They do not freeze for the
  persistent notice, because the default Follow Ghostty diagnostic would
  otherwise freeze them until it is dismissed. Whether NSH overlays draw above
  the native terminal view still needs a GUI parity check (contracts §3).
- **W2's `split_active`/`close_active`** remain exported and tested, but the UI
  uses W1's `newShell({split})` and close paths.
- **Unchanged decisions.** D-PROC-1, D-COPY-1 (the close dialog keeps the
  shipping copy) and D-THEME-1 (applied as W3's proposal) are not decided here.
  The deletion embargo holds.
