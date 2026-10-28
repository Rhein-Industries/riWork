# NSH Phase 1 acceptance plan (P-SHELL subset)

```yaml
assignment: 635c2ee7-8c46-43cd-ab54-c6a9ea5c8f02
branch: nsh-p1-acceptance (off nsh at ea0f2f5)
inputs: docs/nsh-parity.md (P-SHELL, 329 rows), docs/nsh-contracts.md (frozen v1), docs/nsh-spec-terminal-layout.md
reviews: P1a 5c4cb942 (nsh-p1-native), P1b ddba58a0 (nsh-p1-host), P1c 40e6ac8b (nsh-p1-workspace)
verification_environment: code_and_documentation_reading_only
tests_executed: none
live_targets_touched: none
task_status: not_marked_done
```

This plan says which P-SHELL rows of [`docs/nsh-parity.md`](nsh-parity.md) Phase 1 must pass and how
each is proven. It also sets the order in which the three Phase-1 branches deliver them and lists
what a reviewer checks on each branch against [`docs/nsh-contracts.md`](nsh-contracts.md). Every
one of the 329 P-SHELL rows is either selected or deferred with a target ([§10](#10-row-disposition-index)).
Nothing here changes the contracts. Where a contract or a task leaves a choice open, the plan says
so in [§1](#1-decisions-needed-before-acceptance) and proposes an answer without deciding it.

When this was written, `nsh-p1-native`, `nsh-p1-host` and `nsh-p1-workspace` had no commits beyond
`ea0f2f5`. So the file, test and fixture names below are the names this plan asks the branches to
use. A branch may choose other names, but its report must map them to the IDs here.

---

## 1. Decisions needed before acceptance

| ID · status | Question | Why it matters | Proposal (as originally written) |
|---|---|---|---|
| **D-PERF-1** · **ADOPTED 2026-10-27** | What frame budget does NSH promise? The GPUI 25 ms large-turn guard already fails on GPUI. | P1 builds the first NSH perf harness and must not inherit a guard that fails on the baseline, or drop it silently. | [§7](#7-frame-budget-decision-d-perf-1). |
| **D-OWN-1** · **ADOPTED 2026-10-27** | Who builds which `riwork/native` module? `LayoutSession` is a `riwork/native` class (contracts §4.2), but P1a's task covers only the read side of Store and Sessions, and P1c owns the layout mapping. No task names `Tabs`, `Settings`, `Appearance` or the global-window state functions. | P1a and P1c both edit `crates/riwork-nsh-native`. P1b deletes GPUI code whose logic and tests these modules need. | **P1a:** crate skeleton, registry, subscription hubs, `Store`, `Sessions` (read side + `create`/`close` adapters), `Tabs`, `Settings`, `Appearance` (without Ghostty parsing), test support. **P1c:** `crates/riwork-nsh-native/src/layouts/` (`Layouts`, `LayoutSession`), the global-window state functions of `Dashboard`, and moving `gather` / default layout / tab routing out of `main.rs` into riwork-core. **P1b:** host-internal modules, plus moving `dock_menu.rs` logic into the host. |
| D-THEME-1 · P1 applied by W3 assignment 2026-10-27 | What does "Follow Ghostty" (the default theme) do in P1? Parsing the user's Ghostty config goes through `gpui_libghostty` (`crates/riwork-desktop/src/theme.rs:6`; the surface-backed init in `mod native`, theme.rs:916-1045), which P1 does not link. | It is the default, so most homes resolve through it. | Use GPUI's own hard-failure path (SHELL-280): RiWork palette, terminal colors unknown, the diagnostic kept. Publish no `appearance.json`. Real parsing comes with the terminal phase that links libghostty (D-GHOSTTY). |
| D-PROC-1 · open | What does `riwork open` print under single-instance forwarding (contracts §0)? Today it prints `Opened <root> (PID n)` / JSON `pid` of the process it spawned (SHELL-006). Under forwarding, that process exits after forwarding. | The CLI must stay unchanged (§0). Scripts may read `pid`. | Either keep the output, with `pid` naming the short-lived forwarder, or have the forwarder return the running host's pid over `desktop.sock` and print that. Decide before P1b freezes the forward message. |
| D-COPY-1 · open | Tab-close dialog body still says "reopen it from ＋ › Open a worker or shell" (parity disagreement #7). | P1 ports the dialog (P1-TAB-04) and its tests pin the string. | Owner picks the wording before P1c's test freezes it. |

### 1.1 Decision record

The proposal cells above are kept exactly as first written. Adopted decisions are recorded,
dated, in [`docs/nsh-contracts.md` §8](nsh-contracts.md#8-decisions-addendum).

| Date | Decision | Status | Recorded in |
|---|---|---|---|
| 2026-10-27 | D-OWN-1, as proposed above (relayed to P1a, P1b and P1c by the orchestrator) | **ADOPTED** | contracts §8.1 |
| 2026-10-27 | D-PERF-1, as proposed in [§7](#7-frame-budget-decision-d-perf-1); alternatives (A) and (B) not taken; Phase 1 perf stays report-only until the chat phase | **ADOPTED** | contracts §8.2 |
| 2026-10-27 | Phase 2 entry gate: the pinned-runtime teardown proof (task e940bd0a) must pass before Phase 2 opens. The cached-runtime 110–156 ms main-loop gaps are not accepted. §3.7 is not amended. This is an owner decision, not a proposal of this plan. | **ADOPTED** | contracts §8.3 |
| 2026-10-27 | W3 task c62ea465 explicitly applies the D-THEME-1 proposal for P1: RiWork fallback palette, diagnostic retained, no Ghostty parsing or appearance publisher. | P1 assignment | contracts §8.4 |
| 2026-10-28 | P1-GLB-05 / R-DEFER 5: a dashboard or global-window read may create an empty `schedules.lock`. This is the engine's schedule-ledger lock, taken exactly as `riwork dashboard` takes it (`crates/riwork-cli/tests/dashboard_cli.rs`). The allow-list was written for the Phase 1 placeholder global window. In a global-only run `state.json` stays byte-identical, as that run's own before/after inventory shows; the generic audit scripts still allow `state.json` writes. | **ACCEPTED** (orchestrator, Phase 5 task 9a9d3423) | contracts §8.5 |
| — | D-PROC-1, D-COPY-1 | open | — |

---

## 2. Scope

### 2.1 Selection rule

Phase 1 covers the umbrella task's scope (5f5073a7):

- windowing: the process, CLI entry, project and global windows, chrome, restart/reload;
- panes, tabs, splits and drag;
- layout persistence and restore, including reopening layouts the GPUI build wrote;
- menus and keys;
- themes, appearance plumbing and interface text size;
- the status bar.

A row is **Phase 1** when it is fully inside that scope. A row is **Partial** when its window or
layout part is inside and the rest depends on a later surface (terminal, chat, panels, remote,
background services). A row is **Deferred** otherwise. Partial rows list both halves in
[§10](#10-row-disposition-index). The deferred half is in [§8](#8-deferred-items).

| Section (parity) | Rows | Phase 1 | Partial | Deferred |
|---|---:|---:|---:|---:|
| Startup, CLI entry & instances (001–009) | 9 | 7 | 1 | 1 |
| Restart / reload / restore (010–016) | 7 | 6 | 0 | 1 |
| Window lifecycle & title bar (017–033) | 17 | 11 | 6 | 0 |
| Global window (034–068) | 35 | 12 | 3 | 20 |
| Global navigation (069–080) | 12 | 6 | 4 | 2 |
| Workspace pane tree (081–097) | 17 | 16 | 1 | 0 |
| Pane layout workaround (098) | 1 | 1 | 0 | 0 |
| Tabs (099–113) | 15 | 7 | 3 | 5 |
| Tab strip (114–134) | 21 | 16 | 5 | 0 |
| All tabs menu (135–138) | 4 | 3 | 1 | 0 |
| Tab context menu (139–143) | 5 | 4 | 1 | 0 |
| Tab close dialog (144–148) | 5 | 4 | 1 | 0 |
| Terminal hosting glue (149–153) | 5 | 0 | 1 | 4 |
| Layout save / restore (154–159) | 6 | 5 | 1 | 0 |
| Project switching (160–167) | 8 | 1 | 0 | 7 |
| Session discovery & adoption (168–178) | 11 | 3 | 0 | 8 |
| Shared tabs (179–188) | 10 | 8 | 0 | 2 |
| Remote projects & shells (189–200) | 12 | 0 | 0 | 12 |
| Handoff (201–203) | 3 | 0 | 0 | 3 |
| Agent terminals (204–207) | 4 | 0 | 0 | 4 |
| Chat tabs in panes (208–216) | 9 | 0 | 1 | 8 |
| Orchestrator tabs (217–220) | 4 | 0 | 0 | 4 |
| Usage & metrics chips (221–224) | 4 | 0 | 0 | 4 |
| Chat tab ownership (225–227) | 3 | 0 | 0 | 3 |
| Orchestrators & wakes (228–232) | 5 | 0 | 0 | 5 |
| Orchestrator choice (233–237) | 5 | 0 | 0 | 5 |
| Schedules (238–239) | 2 | 1 | 0 | 1 |
| Keyboard shortcuts (240–249) | 10 | 5 | 2 | 3 |
| Application menu (250–252) | 3 | 3 | 0 | 0 |
| Dock menu (253–258) | 6 | 6 | 0 | 0 |
| Status bar items, layout menu, notices (259–261) | 3 | 2 | 1 | 0 |
| Status bar (262–268) | 7 | 5 | 0 | 2 |
| Notifications (269–273) | 5 | 0 | 0 | 5 |
| Theme & palette (274–288) | 15 | 11 | 0 | 4 |
| Appearance sync (289–296) | 8 | 5 | 1 | 2 |
| Interface text size (297–307) | 11 | 8 | 1 | 2 |
| Symbols (308–310) | 3 | 0 | 0 | 3 |
| Metal layer (311–312) | 2 | 0 | 0 | 2 |
| Settings application (313) | 1 | 0 | 1 | 0 |
| Background timers (314–321) | 8 | 2 | 4 | 2 |
| Integration points (322–329) | 8 | 1 | 1 | 6 |
| **Total** | **329** | **159** | **40** | **130** |

### 2.2 Rules for deferred features inside a Phase-1 build (R-DEFER)

A P1 build is a fixture-only build. It must still keep every seam the later phases plug into:

1. **Tabs of every kind keep their identity.** `shell:`, `remote:`, `chat:`, `app_preview:` and
   `panel:` tabs load, render, move, save and restore. A tab whose surface is deferred renders a
   placeholder panel showing its title and `mount <n>` (P1-DRAG-04). It attaches nothing, starts
   nothing and sends nothing.
2. **Menus and pane menus keep GPUI's full structure and order.** An item for a deferred feature is
   present and **disabled**. P1 adds no notice strings.
3. **The keymap declares every GPUI chord** (SHELL-240–243 plus the app-level ones). A chord of a
   deferred feature is **inert**. Every inert command id sits in one set, `DEFERRED_COMMANDS`,
   which must equal the list in [§8](#8-deferred-items) (P1-KEYS-01).
4. **Status items without P1 data** use the GPUI no-data text (`Agents · —`, `CPU — RAM —`). Data
   items are disabled.
5. **Write allow-list.** A P1 build writes only these RIWORK_HOME files: `layouts.json` (+
   `layouts.lock`, `layouts.corrupt-*.json`), `global-window.json`, `settings.json`, `state.json`
   (active project and folders via `Store`), `project-tabs/*` (via `Tabs.update`), `nsh/**` and
   `run/desktop.lock`. It never writes `appearance.json`, `sessions.json` or `chats/`, and it starts
   no tmux server (checked in [§3.3](#33-fixture-app-runs-and-cua-read-only-checks)).
   *Amended 2026-10-28 (Phase 5, accepted, §1.1):* this list was written for the Phase 1
   placeholder global window. Once the dashboard reads (global window, dashboard paths), an
   **empty** `schedules.lock` is also allowed: it is the engine's schedule-ledger lock, created as
   `riwork dashboard` creates it. A non-empty one, or `schedules.json`, is still a disallowed
   write. In a global-only run (P1-GLB-05), `state.json` must also stay byte-identical. That is
   checked by the run's own before/after inventory. The generic fixture scripts in
   `crates/riwork-desktop/verification/phase1_{w3,m5,integration}.py` allow only an empty
   `schedules.lock` beyond this list. They still allow `state.json` writes, which broader
   workflows make, so they do not check that byte-identity.

### 2.3 Findings from NSH 0.3 that bind P1c

Two facts from reading the NSH 0.3 sources are not stated in the contracts or the parity rows:

- **DockArea goes compact below 600 pt of its own width.** In compact mode it shows one group with
  every panel's tab (`nsh-base-0.3.0/src/dock/dock_area.rs:105-106`, `src/dock/compact.rs:1-3`;
  test `at_compact_width_the_center_shows_one_group_with_every_tab`). RiWork's minimum window is
  640 pt and its pane tree must never collapse, so P1c passes `compact={false}` explicitly
  (C-C-07).
- **`DockArea.locked` is one `bool` for the whole area** (`dock_area.rs:260`; test
  `a_locked_area_moves_nothing`). It is not per group, as SHELL-085's note assumed. RiWork pane
  locks are `ProjectLayout.locked_panes` inside `LayoutSession` (contracts §4). P1c must never map
  them to DockArea's `locked` (C-C-08).

Also, `nsh/test` 0.3 has no JS bounds query: the protocol's `Bounds` step takes a node id, not a
selector (`nsh-dev-0.3.0/src/protocol.rs:595-599`). Pixel geometry is therefore checked in Rust
(projection sizes, metrics helpers), in Cua frame reads, or as screenshot evidence, never by a
TSX bounds assertion.

---

## 3. Verification routes and fixtures

### 3.1 Route kinds

| Route | What runs | Command shape | Isolation |
|---|---|---|---|
| `core` | Existing riwork-core/engine/cli tests, unchanged. Moved tests keep their names. | `cargo test -p riwork-core --lib <name>`; `-p riwork-engine`; `-p riwork-cli` | the tests' own TempDirs |
| `bridge` | Rust tests of `riwork-nsh-native` adapters and `LayoutSession`. DockArea gestures are driven headless with `nsh_testing::Headless`, as `nsh-base-0.3.0/tests/base/dock.rs` does. | `cargo test -p riwork-nsh-native --features test-support --test <file> -- <name>` | the home is a `TempDir` passed to backend constructors; `ManualClock`; never `std::env::set_var("RIWORK_HOME")` (parallel test threads) |
| `nsh-test` | Headless TSX tests of the app bundle. | §3.2 recipe | copied fixture home; `FakeSessions` (no tmux, no chat host) |
| `host-unit` | Pure host logic: startup resolution, instance lock, placement, restore state machine, window registry, menu model, Dock labels. | `cargo test -p riwork-desktop --bin riwork <module>::` | TempDir |
| `appkit` | Host code that needs real AppKit objects but no visible window: NSMenu trees, window style and size settings, Dock menu. | `cargo test -p riwork-desktop --test appkit_shell -- --test-threads=1`. `[[test]] harness = false`, so it runs on the main thread. `NSApplicationActivationPolicyProhibited`. Windows are never ordered front. | TempDir |
| `fixture+cua` | The built `riwork` binary on a fixture home, inspected read-only with Cua, on the fixture PID only. | [§3.3](#33-fixture-app-runs-and-cua-read-only-checks) | fixture env |
| `cli-diff` | CLI output of the NSH build compared with the reference build of `ea0f2f5` on identical home copies. | [§3.4](#34-cli-diff) | fresh home per command |
| `review` | Code reading or grep, recorded with its command in the branch report. | — | — |
| `manual` | A person checks screenshot evidence written by a read-only Cua call. Used only for colors and visual polish. | `get_window_state({... "include_accessibility_tree": false, "screenshot_out_file": "$FIX/evidence/<item>.png"})` | fixture PID only |

Conventions every branch uses:

- `$APP` is the P1c app (mixed template). The proposed path is `crates/riwork-nsh-app`. Keeping it
  out of `crates/riwork-desktop` avoids conflicts with P1b, which rewrites that crate.
- `$NSH` is the NSH 0.3.0 CLI from [`docs/nsh-toolchain.md`](nsh-toolchain.md)
  (`$NSH_PROBE/nsh-cli-0.3.0/bin/nsh`, `nsh 0.3.0 (76fa8a7)`), or the host spike's local build.
  Never `~/.local/bin/nsh`, which is 0.2.0.
- Stable selectors, which are also AX identifiers:
  - `pane-<PaneId>`, `pane-menu-<PaneId>`, `tab-<panelId>`, `tab-close-<panelId>`;
  - `strip-all-tabs`, `status-bar`, `status-item-<key>`, `layout-menu`, `notice-dismiss`;
  - `window-controls-island`.
  
  GPUI's existing ids are kept: `global-view-dashboard`, `global-view-chat`,
  `global-notice-dismiss`, `top-orchestrator`, `status-bar-visible`, `status-item-<key>-*`. The
  divider label is `Pane divider` (DockArea `handle_accessibility_label`).
- `riwork-nsh-native` gets a `test-support` cargo feature with `testing::{ManualClock, FakeSessions}`
  and the fixture generator. Only test builds enable it. A release `nsh build` must not contain it
  (C-A-09).

### 3.2 Fixture homes

Fixture homes are generated, not hand-edited. The generator writes every file through the same
riwork-core/engine writers the GPUI build uses: `Store`, `LayoutStore::save`, `TabStore::update`,
the settings save, the global-window save, and the runtime snapshot API. The one exception is
deliberately legacy or damaged files, which are copied verbatim from riwork-core test literals.

```sh
cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home -- \
  --scenario <name> --out "$H/home"      # also creates "$H/repos/<project>" roots, "$H/run" for p1-restore
```

| Scenario | Contents | Used by |
|---|---|---|
| `p1-basic` | Local projects **Alpha**, **Beta**, **Gamma**, **Busy**. Alpha has layout L-NAV and a shared tab list (3 shells, 1 chat, 1 hidden worker). Beta has L-NESTED and an unreadable `project-tabs` file, so it uses the classic bar. Gamma has no layout. Busy has 24 shell tabs. Settings are defaults. `fake-sessions.json` lists: Alpha shells live; one dead; one CLI-made shell not in the layout; one global-orchestrator shell; one other project's orchestrator; one entry with `appear_after_ms: 300`; Beta `create` set to fail with a fixed message. | most nsh-test, fixture+cua, cli-diff |
| `p1-layouts` | One project per layout fixture below, plus `fixtures.json` (name → project id → golden projection text). D-* fixtures each get their own home. | LAY |
| `p1-restore` | `p1-basic` plus a runtime snapshot in `$H/run`: Alpha windowed at 100,100 1000×700; Beta maximized; global window (view chat, filter needs_you); Beta in focus mode. If the engine's runtime API cannot write a ticket without a live requester, the generator writes only the two project windows, and P1-GLB-04 is covered by host-unit + nsh-test. | RLD, WIN-03 |
| `p1-theme` | `p1-basic` with one settings variant per theme (7), `use_riwork_colors`, `native_terminal_colors=false`, `ui_text_size` 12.5, each `interface_font`; an empty `$HOME/.config/ghostty`. | THM, TXT |

Layout fixtures. Line numbers are `crates/riwork-core/src/layouts.rs` tests unless noted.

| Name | Shape | Source of truth |
|---|---|---|
| L-NAV | Default navigation pane (`locked_panes: null`) at 0.27; main pane with 2 shells, 1 chat, 1 remote shell | 2533; default layout code 874-896 |
| L-NESTED | Depth 3, mixed axes, including a same-axis chain (exercises flattening); ratios 0.30 / 0.62 / 0.50; active pane not the first; main pane set | 2162, 2415 |
| L-EMPTY | Empty main pane, plus an empty pane kept beside a locked one | 2712 |
| L-UNKNOWN | An unknown tab kind between known tabs; an unknown top-level field | 2836 |
| L-PREVIEW-REMOTE | `app_preview` tab with launch settings; `remote:<desk>:<shell>` tab | 2917, 2989 |
| L-LEGACY | Only legacy `shell_ids` / `active_shell_id`; bottom `tab_edge` | 2314, 2354 |
| L-GLOBAL-TABS | Dashboard panel tab, global chat tab, live global-orchestrator shell tab | `crates/riwork-desktop/src/global_nav/tests.rs:440,481` |
| L-UNLOCK | `locked_panes: []` (explicit no lock), no main pane | 2533 |
| L-BIGID | A pane id of 2^53 + 5 | contracts §4.6 |
| L-RATIO | Ratios 0.02 and 0.98; narrow locked panes | 2460, 3547-3742 |
| L-CARRY | Two projects with colliding pane ids and locked regions | 2563, 2617, 3320 |
| L-MAIN | Main pane kept per project | 3194 |
| D-NEWER | `layouts.json` with a newer `schema_version` | 3408 |
| D-CORRUPT | Truncated JSON | 3376 |
| D-ENTRY | One unparsable project entry next to a good one | contracts §4.1 (`layouts.rs:1596-1599,1877-1879`) |

nsh-test recipe:

```sh
H="$(mktemp -d "${TMPDIR:-/tmp}/riwork-p1-nsh.XXXXXX")"
cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home -- --scenario p1-basic --out "$H/home"
mkdir -p "$H/user/.config" "$H/tmux"
"$NSH" check "$APP"
env RIWORK_HOME="$H/home" RIWORK_NSH_FAKE_SESSIONS="$H/home/fake-sessions.json" \
    HOME="$H/user" XDG_CONFIG_HOME="$H/user/.config" TMUX_TMPDIR="$H/tmux" \
    "$NSH" test "$APP" --junit "$H/p1-nsh.xml"
test -z "$(ls -A "$H/tmux")"           # no tmux server was started
```

Each `*.test.tsx` file uses its own project ids, so files do not share mutable state. Tests open
windows with `open("./App.tsx", { platform: "macos", width: 1220, height: 780, host: { params,
handlers } })`. Here `params` is a schema-1 `host.params` (contracts §2.2), and `handlers` mirror
the §2.3 result shapes. The handlers live in one `$APP/tests/hostFake.ts` (C-C-12). Assertions use
`app.texts()`, `app.shows()`, `app.count()`, `app.spanStyle()`, `app.menu()`, `app.host.calls()`,
`app.host.send()`, `app.keyboard.press()`, `app.drag()`, `app.rightClick()`, `app.setViewport()`,
`app.setColorScheme()` and `app.clock.runFor()` (with `{ clock: {} }`). All of these appear in the
NSH 0.3 testing sources (`nsh-dev-0.3.0/src/protocol.rs`, `nsh-testing-0.3.0/tests/testing/`).
Persistence across windows is checked by opening a second app on the same project after the
250 ms trailing save, and reading what it renders.

### 3.3 Fixture app runs and Cua read-only checks

These runs open **fixture windows** on the desktop. The app may take focus. They never touch the
user's RiWork windows, sessions or home.

```sh
FIX="$(mktemp -d "${TMPDIR:-/tmp}/riwork-p1-fix.XXXXXX")"
cargo build -p riwork-desktop                       # the NSH host, binary `riwork`
cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home -- --scenario p1-basic --out "$FIX/home"
mkdir -p "$FIX/user/.config" "$FIX/run" "$FIX/tmp" "$FIX/tmux" "$FIX/evidence"
touch "$FIX/start.stamp"
fixture_env() { env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin HOME="$FIX/user" TMPDIR="$FIX/tmp" \
  TMUX_TMPDIR="$FIX/tmux" XDG_CONFIG_HOME="$FIX/user/.config" \
  RIWORK_HOME="$FIX/home" RIWORK_RUNTIME_DIR="$FIX/run" "$@"; }
fixture_env target/debug/riwork "$FIX/repos/alpha" >"$FIX/gui.log" 2>&1 &
echo $! >"$FIX/gui.pid"
# … Cua reads below …
kill -TERM "$(cat "$FIX/gui.pid")"
# write allow-list (R-DEFER 5):
(cd "$FIX/home" && find . -type f -newer "$FIX/start.stamp" | sort) >"$FIX/written.txt"
test -z "$(ls -A "$FIX/tmux")" && test ! -e "$FIX/home/appearance.json"
```

The tmux socket is `riwork-<hash of RIWORK_HOME>` (`crates/riwork-engine/src/sessions.rs:220-223`).
A fixture home therefore cannot reach the user's tmux server. `TMUX_TMPDIR` makes any accidental
server visible inside `$FIX`.

Cua calls (cua-driver MCP). Every call names the fixture `pid` and the session label `p1-acceptance`:

1. `list_windows({"pid": PID, "session": "p1-acceptance"})`. Record `window_id`, `title`, `bounds`,
   `is_on_screen`.
2. `get_window_state({"pid": PID, "window_id": WID, "query": "<label text>", "tree_format":
   "elements", "include_screenshot": false, "session": "p1-acceptance"})`. Read roles, labels and
   frames.
3. `verify_state({"pid": PID, "window_id": WID, "expect": [{"window": {"exists": true, "bounds":
   {"x": X, "y": Y, "width": W, "height": H, "tolerance_px": 1}}}, {"element": {"selector":
   {"label_contains": "<text>"}, "exists": true}}], "timeout_ms": 5000, "stable_samples": 2,
   "session": "p1-acceptance"})`. `unknown` counts as a failure.
4. Evidence only: `get_window_state({... "include_accessibility_tree": false,
   "screenshot_out_file": "$FIX/evidence/<item>.png"})`.

**Forbidden in Phase-1 acceptance:** every Cua tool that sends input or changes state. That
includes `click`, `double_click`, `right_click`, `type_text`, `press_key`, `hotkey`, `drag`,
`scroll`, `set_value`, `set_window_frame`, `invoke_menu`, `bring_to_front`, `launch_app`,
`kill_app`, `clipboard_write`, `run_steps`, `run_script` and `browser_*`. An unfiltered
`list_windows` is also forbidden. Input-driven behavior is proven headless (nsh-test, appkit,
host-unit). The fixture app is started and stopped from the shell. Menu-bar contents are not
reachable through these read tools, so menus are checked by the `appkit` route.

### 3.4 CLI diff

```sh
REF="$(mktemp -d "${TMPDIR:-/tmp}/riwork-p1-ref.XXXXXX")"; mkdir -p "$REF/src"
git archive ea0f2f5 | tar -x -C "$REF/src"                      # reference GPUI build source; no repo mutation
cargo build --manifest-path "$REF/src/Cargo.toml" --target-dir "$REF/target" -p riwork-desktop
for cmd in "project list --json" "settings list --json" "instances --json" "appearance --json" "nosuchcommand"; do
  for side in ref new; do
    rm -rf "$REF/cli" && mkdir -p "$REF/cli"
    cargo run -q -p riwork-nsh-native --features test-support --example p1_fixture_home -- --scenario p1-basic --out "$REF/cli/home"
    bin=target/debug/riwork; [ "$side" = ref ] && bin="$REF/target/debug/riwork"
    env -i PATH=/usr/bin:/bin HOME="$REF/cli/user" RIWORK_HOME="$REF/cli/home" RIWORK_RUNTIME_DIR="$REF/cli/run" \
      "$bin" $cmd >"$REF/$side.out" 2>"$REF/$side.err"; echo $? >"$REF/$side.code"
  done
  diff "$REF/ref.out" "$REF/new.out" && diff "$REF/ref.err" "$REF/new.err" && diff "$REF/ref.code" "$REF/new.code"
done
```

Both sides use the same home path, so paths in the output compare equal. None of these commands
may start a GUI (SHELL-001).

---

## 4. Ordering against P1a, P1b and P1c

### 4.1 What each branch must expose to the others

| From → to | Interface | Needed by |
|---|---|---|
| P1a → P1c, P1b | `riwork_nsh_native::registry()`; subscription hub API; `testing::{ManualClock, FakeSessions}`; `Store`/`Sessions`/`Tabs`/`Settings`/`Appearance` DTOs | P1c's `#[class] LayoutSession` and TSX; P1b's `set_ui_sets` |
| P1c → P1b | Built bundle of `$APP`. A Rust handle to each window's `LayoutSession` by `WindowId`, with `flush_blocking()` and `project_layout() -> ProjectLayout` (contracts §2.5, §4.5). | host flush on close/quit/restart; `RuntimeWindow` publishing |
| P1b → P1c | `host.params` schema 1, §2.3 calls, §2.4 events, as served by the real host | M4 integration; until then P1c uses `hostFake.ts` |

### 4.2 Milestones

| Milestone | Branch | Delivers | Acceptance items it closes |
|---|---|---|---|
| M0 | all | Agree on paths (`$APP`, fixture generator), D-OWN-1, and the deletion embargo (§4.3) | — |
| M1 | P1a | Crate skeleton, registry, hubs, `Store`/`Sessions`/`Tabs`/`Settings`/`Appearance`, test support, fixture generator, probe `nsh test` | P1-NAT-01–05, P1-POLL-01, P1-POLL-02 (bridge), P1-THM-01–03, -06, -07, P1-TXT-01, P1-STAT-01 (model), P1-STRIP-05 (bridge), P1-PROC-07 (hubs) |
| M2a | P1c (parallel with M1) | Pure layout code: projection/apply, oracle, gestures via Rust `Headless` DockArea; core moves (gather, default layout, routing) with their tests | P1-LAY-01, -02, -04, -08, -10 (bridge), P1-PANE-04 (bridge), P1-PERF-02 |
| M2b | P1c (rebased on M1) | `#[class] LayoutSession`, TSX workspace, global window TSX, status bar, keymap on `hostFake.ts` | all PANE, TAB, STRIP, DRAG, KEYS (TSX), GLB (TSX), STAT, THM-04/-05 (TSX), TXT-02/-03 (TSX), LAY-03/-05/-06/-07/-09, RLD-04 (TSX), POLL-03 |
| M3 | P1b (parallel; the spike bundle stands in until M2b) | Host binary: CLI first, instance lock, windows, chrome, menus, Dock, window registry, restore state machine, `appkit_shell` | PROC-01–06, WIN-01–03, -07–-09, MENU-01–04 (appkit, host-unit), GLB-01, -08 (host), -09, RLD-02, -03 |
| M4 | on `nsh`, after merging P1a → P1c → P1b | The host loads `$APP` with `riwork/native`; end-to-end runs | fixture+cua parts of PROC, WIN, RLD-01, GLB-02/-04; cli-diff; write allow-list; LAY-07 end-to-end; PERF-01 report |
| M5 | coordinator | Phase-1 exit: every Phase-1 item passes; partial rows' P1 halves pass; §8 acknowledged; D-PERF-1 decided or explicitly carried | — |

**Merge order: P1a → P1c → P1b.** P1b goes last because it deletes GPUI code. Its deletions may
merge only when every Phase-1 test listed in §4.3 already has its successor on the target branch,
or the successor lands in the same merge.

### 4.3 Test carry-over ledger (deletion embargo)

These tests in `crates/riwork-desktop` are what pin Phase-1 rows today. Before P1b deletes their
files, each needs a successor:

- **move**: same name, pure logic moved to riwork-core, the host crate or `riwork-nsh-native`;
- **port**: a new `nsh-test` or `bridge` test that checks the same behavior.

Tests that pin only deferred rows can go with the GPUI code. Their source stays readable at
`ea0f2f5`.

| GPUI test (file) | Rows | Successor | Owner |
|---|---|---|---|
| `a_gather_moves_every_unlocked_tab_into_the_main_pane_and_leaves_the_locked_pane_alone`, `a_gather_never_touches_any_locked_pane_among_several` (main.rs) | 091 | move: `gather` (main.rs:617-661) → riwork-core | P1c |
| `a_messy_window_becomes_a_locked_navigation_pane_beside_the_main_pane`, `the_default_layout_closes_nothing_and_a_locked_panes_tabs_are_moved_not_dropped`, `a_window_that_is_laid_out_already_is_left_exactly_as_it_is`, `panes_keep_their_ids_and_a_new_one_is_made_only_when_there_is_none_to_keep`, `the_default_layout_is_saved_as_the_projects_layout_and_reads_back_as_it_was` (main.rs) | 092 | move: default layout (main.rs:694-924) → riwork-core | P1c |
| `only_a_main_pane_that_exists_outside_focus_mode_takes_what_is_opened`, `a_new_tab_opens_in_the_main_pane_whichever_pane_is_selected`, `an_open_chat_orchestrator_tab_comes_to_the_front_by_the_main_pane_rules` (main.rs) | 087, 088, 100 | move: `routing_target`, `new_tab_target`, `bring_up_target` (main.rs:1070-1133) → riwork-core | P1c |
| `closing_a_pane_focuses_the_pane_that_takes_its_space`, `a_closed_pane_hands_its_tabs_to_the_main_pane_after_its_own`, `closing_the_main_pane_or_a_locked_one_or_having_no_main_pane_closes_the_tabs_as_before` (main.rs) | 089, 090 | move the pure parts (main.rs:587-615) + port to nsh-test | P1c |
| `lock_state_follows_the_users_choice_and_defaults_to_the_navigation_pane` (main.rs) | 085, 086 | move (main.rs:12860-12871) | P1c |
| `a_tab_moves_between_panes_and_the_pane_it_left_keeps_what_it_showed` (main.rs) | 106 | port: nsh-test `Drag.test.tsx` + bridge gesture test | P1c |
| `a_chat_still_being_made_is_not_saved_and_one_with_an_id_is` (main.rs) | 099, 209 | port: bridge `chat_pending_is_never_saved_and_maps_to_chat_id` | P1c |
| `shared_adoption_rejects_dead_orchestrator_global_and_foreign_shells`, `shared_membership_orders_main_pane_keeps_other_panes_and_hides_without_stopping` (main.rs) | 147, 177, 178, 180, 182, 187, 209 | port: bridge (adoption, membership) + nsh-test | P1c |
| `shared_drag_mapping_ignores_panels_foreign_and_global` (main.rs) | 108, 132, 185 | move (main.rs:13134-13157) | P1c |
| `shared_menu_moves_are_adjacent_and_stay_in_parent_groups` (main.rs) | 142 | move (main.rs:12971-12993) → `Tabs` helper | P1a |
| `the_orchestrator_closes_like_any_user_chat_and_workers_only_detach` (main.rs) | 105, 144 | move: close choice (main.rs:12958-12969) → `Sessions.closeChoice` | P1a |
| `the_strip_draws_panels_then_shared_order_as_named_tabs`, `the_selected_strip_tab_comes_back_into_view_after_a_shared_move_or_rename`, `the_selected_strip_tab_stays_in_view_when_it_or_a_tab_before_it_is_renamed`, `arrows_stay_inside_an_open_strip_menu_so_escape_closes_it_and_thaws` (main.rs) | 114, 115, 118, 121, 122, 127, 130, 135–137, 182, 246 | port: nsh-test `Strip.test.tsx` | P1c |
| `no_key_is_bound_twice_and_the_chat_shortcuts_are_among_them` (main.rs) | 079, 240–242 | port: `keymap.test.ts` (TSX keymap + host key equivalents) | P1c, P1b |
| `a_reload_is_confirmed_once_every_window_it_handed_over_is_back` (main.rs) | 014, 019 | move: confirmation logic → host `restore` module | P1b |
| `a_vanished_project_root_is_an_error_for_the_caller_not_a_panic` (main.rs) | 004 | move: `WorkspaceStartup` (main.rs:1877-1948) → host `startup` | P1b |
| `selected_theme_controls_terminals_and_keeps_legacy_preferences` (main.rs) | 279, 281, 313 | move (override-choice part) → riwork-core | P1a |
| `measure_a_busy_workspace_frame`, `a_pane_laid_out_alone_draws_clicks_types_and_scrolls_in_place` (main.rs) | 098 | port: P1-PERF-01 harness (report-only); the GPUI copy stays runnable at `ea0f2f5` | P1c |
| `only_an_agent_in_a_terminal_offers_to_hand_off` (main.rs) | 097 | port: nsh-test pane menu (row present, disabled) | P1c |
| `panels_lead_then_sessions_in_shared_order`, `command_numbers_count_session_tabs_and_nine_is_the_last`, `a_strip_drag_moves_anywhere_across_parents`, `carried_foreign_and_global_views_are_never_move_operands`, `stopped_and_unknown_activity_map_to_the_phones_states`, `all_tabs_appears_only_when_the_scrolling_tabs_do_not_fit` (tab_strip.rs) | 115, 116, 120, 129, 133 | move: pure strip functions → `Tabs` helpers or a TSX pure module with the same tests | P1c |
| `keyboard_menu_moves_focus_and_rename_uses_persistent_input` (tab_menu.rs) | 139–141 | port: nsh-test | P1c |
| `the_order_does_not_depend_on_the_input_order`, `labels_stay_unique_even_when_a_project_is_named_like_a_number_suffix` (dock_menu.rs) | 253, 254 | move → host `dock_menu` | P1b |
| `every_entry_reuses_the_one_global_window_and_a_reload_records_it_as_itself`, `a_reload_reopens_the_window_as_it_handed_it_over_else_as_it_was_last_shown`, `the_dashboard_opens_in_its_own_window_and_starts_or_selects_nothing`, `the_dashboards_search_filter_and_order_are_the_windows_own`, `only_an_explicit_start_asks_for_a_new_orchestrator` (global_window/tests.rs) | 034, 036–042, 067 | port: host-unit (singleton, reload kind) + nsh-test (state, view switch, ⌘W); the orchestrator part is deferred | P1b, P1c |
| `the_global_item_opens_a_menu_whose_rows_bring_up_the_global_window`, `a_project_window_hands_the_dashboard_and_the_global_orchestrator_to_the_global_window`, `an_older_layout_s_global_tabs_are_the_global_window_s`, `an_older_layout_loads_without_its_global_tabs_and_saves_without_them`, `dashboard_rows_open_their_project_window_and_the_global_window_stays_global` (global_nav/tests.rs) | 039, 062, 069–078 | port: nsh-test (global item, redirect) + bridge (legacy global tabs) | P1c |
| `hermes_settings_round_trip_preserves_every_existing_theme_name`, `only_native_goes_stale_when_macos_switches_light_and_dark`, `published_terminal_follows_what_the_desktop_shows_while_following_ghostty` (bg fallback only), `terminal_override_is_what_terminals_are_forced_to`, `native_terminals_match_the_theme_only_while_the_option_is_on` (theme.rs) | 274–277, 281, 291, 292 | move: palettes, `mix`/`readable`/`contrast`, ANSI tables, override choice → riwork-core or `Appearance` | P1a |
| `a_change_of_theme_option_or_ghostty_color_is_published_once` (appearance_sync.rs) | 278, 284 | move: the derivation part (Gruvbox bg) only; publishing is deferred (D-GHOSTTY) | P1a |
| `saved_sizes_round_to_tenths_clamp_and_write_plain_numbers`, `a_step_starts_from_the_stored_size_not_the_cached_one` (ui_text.rs) | 297, 301 | none: the same names already exist in `crates/riwork-core/src/ui_text.rs:297,315` | — |
| `ui_text_size_defaults_to_today_clamps_and_keeps_odd_values_until_changed` (settings.rs) | 297 | move → riwork-core settings tests | P1a |
| `partial_preferences_and_unknown_items_preserve_known_choices`, `a_bar_saved_before_the_layout_menu_gets_it_last_on_the_right_and_keeps_every_choice` (status_bar.rs) | 264 | none: core copies exist (`crates/riwork-core/src/status_bar.rs:320,438`) | — |
| `routing_and_reordering_preserve_other_side_and_round_trip` (status_bar.rs) | 265 | move → riwork-core `status_bar` | P1a |
| `native_viewer_routes_zoom_paging_and_escape_without_touching_the_composer` (chat_view/viewer_tests.rs) | 298 | deferred with the file preview; the P1 half (no file preview) needs no successor | — |

---

## 5. Acceptance items

Each item names its rows (SHELL-), the contract section, the owning branch and the exact check.
Bridge test files are in `crates/riwork-nsh-native/tests/`. nsh-test files are in `$APP/tests/`.
Host-unit modules are in `crates/riwork-desktop/src/`.

### 5.1 Process and CLI entry (P1b)

| ID | Accept when | Rows · contract · owner | Route and check |
|---|---|---|---|
| P1-PROC-01 | `riwork <cmd>` runs the CLI and exits with no AppKit start. The GUI starts only with no argument or one existing directory; `-psn_*` is ignored. Failures print `riwork: <err>` and exit 2. | 001 · §0 · P1b | core `cargo test -p riwork-cli only_gui_startup_forms_open_the_workspace`; cli-diff (§3.4): all 5 commands identical; review: `main` calls `cli::run_cli` before any AppKit or NSH symbol. |
| P1-PROC-02 | After the CLI passes, the GUI clears `RIWORK_CHAT_ID` and `RIWORK_SHELL_ID` before any child spawn or reload launch. | 002 · P1b | host-unit `startup::tests::gui_start_scrubs_child_ids`. |
| P1-PROC-03 | Startup failures print `riwork: <error>` and exit 2, never a panic. | 003 · P1b | fixture: `printf x >"$FIX/notadir"; fixture_env RIWORK_HOME="$FIX/notadir" target/debug/riwork "$FIX/repos/alpha"` → exit 2, stderr starts `riwork: `, `pgrep -f "$FIX"` empty. |
| P1-PROC-04 | First window's project: the canonicalised folder argument (an existing project is reused, else registered), else the active project, else the cwd / `$HOME` / `.` fallback. A bad folder falls back to the active project and shows the error as the notice. | 004, 005 · §2.2 `notice` · P1b (+P1c) | host-unit: moved `a_vanished_project_root_is_an_error_for_the_caller_not_a_panic`, plus `startup::tests::{folder_arg_reuses_existing_project, no_active_project_registers_the_cwd_fallback}`; fixture+cua: launch with `$FIX/repos/beta` → `list_windows` title `RiWork · Beta`; launch with `$FIX/missing` → title `RiWork · Alpha` and `verify_state` element `label_contains: "missing"`. |
| P1-PROC-05 | One process per RIWORK_HOME. A second launch or `riwork open PATH` forwards over `run/desktop.sock` and exits 0; the running host targets or opens the window (`project.open`). Only a reload replacement may overlap. This replaces SHELL-006 deliberately (§0); output per D-PROC-1. | 006 · §0 · P1b | host-unit `instance::tests::{second_launch_forwards_and_exits, a_stale_lock_is_reclaimed, a_reload_replacement_may_overlap_until_confirmed}`; fixture: `fixture_env target/debug/riwork open "$FIX/repos/beta" --json` exit 0; `fixture_env target/debug/riwork instances --json` → exactly one pid; `list_windows({pid})` gains `RiWork · Beta`. |
| P1-PROC-06 | `riwork instances` lists the NSH GUI and its window count. | 007 · P1b | core `live_gui_pids_lists_running_guis_and_changes_nothing`; fixture: after PROC-05, `instances --json` shows 2 windows. |
| P1-PROC-07 | UI sets and native components are registered once per process, before the first AppView. Process services exist once, whatever the window count. Services outside P1 are not started. | 008 (part) · §0 · P1a, P1b | bridge `hubs.rs::two_engines_share_one_home_watcher_and_one_hub_per_topic`; review of host `main` (one `set_ui_sets`, before the first `AppView`). |

### 5.2 Restart, reload and restore (P1b with P1c)

| ID | Accept when | Rows · contract · owner | Route and check |
|---|---|---|---|
| P1-RLD-01 | End to end: a GUI showing Alpha (windowed), Beta (maximized, focus mode) and Global reloads with `riwork restart`. The replacement reopens all of them with the same frames, modes, titles, layouts and focus state. The CLI reports restored, the old process exits, the new one is registered. | 010–013 · §2.2 `restore`, §2.5, §4.5 · P1b, P1c | fixture+cua on `p1-restore`. (1) `fixture_env RIWORK_RESTORE_TICKET=<generated ticket> target/debug/riwork` → `list_windows` shows 3 windows whose titles and bounds equal the snapshot ±1. (2) Record tab labels per window: `get_window_state({query: "", tree_format: "elements", include_screenshot: false})`, filtered to role tab. (3) `fixture_env target/debug/riwork restart --json` → exit 0, restored = all, failed = 0. (4) `instances --json` shows a new pid; the old pid is gone; `list_windows(new)` equals (1); labels equal (2). |
| P1-RLD-02 | Confirmation waits for every handed-over window (project windows after their layout session opened, global immediately). A global window opened during restore is ignored unless it was in the snapshot. 20 s cap; 40 s watchdog exits 75. A failed launch calls `fail_reload` and the old app stays. A failed draft flush cancels the reload (the hook exists; drafts are deferred). | 011, 014 · §2.5 · P1b | core `queuing_and_wait_timeout_leave_old_gui_alive_and_do_not_launch_anything`, `readiness_requires_correct_child_and_matching_registered_windows`, `failed_or_hung_replacement_keeps_old_alive_and_allows_retry`, `a_watchdog_that_fired_refuses_to_let_the_restore_confirm`; host-unit `restore::tests::{a_reload_is_confirmed_once_every_window_it_handed_over_is_back, a_global_window_opened_while_restoring_is_not_counted, confirmation_gives_up_after_20_s}` (fake clock). |
| P1-RLD-03 | The registry heartbeat publishes only on change, at most every 2 s, and never waits on the registry lock. | 015 · §2.5 · P1b | core `publishing_writes_only_changes_and_never_waits_for_the_registry_lock`; host-unit `runtime_tick::tests::a_busy_registry_lock_stays_due`. |
| P1-RLD-04 | Each `RuntimeWindow` is built in Rust from the window's `LayoutSession` and NSWindow frame/mode, with no JS round trip. `host.params.restore.layout` goes to `Layouts.openSession` and wins over `layouts.json`, then is saved as after any successful load (SHELL-155). `app.restoreReady` is posted exactly once after the layout is installed, and only when `restore` is present. | 159 · §2.2, §2.3, §2.5, §4.5 · P1b, P1c | core `identifiers_geometry_and_private_file_read_are_validated`; host-unit `runtime_window::tests::built_from_layout_session_and_frame`; bridge `layout_session.rs::restore_payload_wins_over_the_store`; nsh-test `Restore.test.tsx`: open Beta with `restore.layout` = a variant of L-NESTED → rendered groups match the payload; `app.host.calls()` holds one `app.restoreReady` (posts included); without `restore` → none. |

### 5.3 Windows and chrome

| ID | Accept when | Rows · contract · owner | Route and check |
|---|---|---|---|
| P1-WIN-01 | Transparent title bar, full-size content view, traffic lights at (10, 8), minimum size 640×400. | 017 · §2.5 · P1b | appkit `appkit_shell::window_chrome` (style mask has full-size content, `titlebarAppearsTransparent`, `minSize` 640×400); fixture+cua: `get_window_state({query: "close", tree_format: "elements", include_screenshot: false})` → close-button frame origin minus window origin equals (10, 8) ±1 pt. If the two builds disagree, record the reference build's reading on the same fixture. |
| P1-WIN-02 | Opening frame: remembered size when `remember_window_size` is on, else 1220×780. Fitted to the visible frame (display − 40, minimum 640×400) and centred. A 22 px cascade in 5 steps, counted over open non-tooltip windows. | 018 · §2.3 `window.placement` · P1b | core `opening_windows_cascade_inside_the_display_and_clamp_after_cascading`; host-unit `placement::tests::the_cascade_counts_open_non_tooltip_windows`; fixture+cua: Alpha open, then `riwork open beta` → second origin = first + (22, 22) ±1, size 1220×780. |
| P1-WIN-03 | A restored window uses its saved frame exactly, as Windowed / Maximized / Fullscreen. A zoomed window counts as Maximized and keeps its normal frame. | 019, 020 · §2.3 `window.placement` · P1b | host-unit `placement::tests::{zoomed_counts_as_maximized_and_keeps_its_normal_frame, fullscreen_counts_as_fullscreen}`; fixture+cua inside RLD-01 (Beta maximized). |
| P1-WIN-04 | Remembered size: only ordinary frames count; moves and returns to the settled size are ignored; 250 ms debounce; valid 320–16384 × 240–16384; gated by `remember_window_size`; turning it on captures the current size; the size follows the window across a project switch. | 021, 022 (part), 313 (g), 320 (part) · §4.5 · P1c (session), P1b (frames from Rust) | bridge `window_size.rs::{an_ordinary_resize_is_saved_after_250_ms, zoomed_and_fullscreen_frames_are_ignored, a_move_or_a_return_to_the_settled_size_writes_nothing, out_of_range_sizes_are_rejected, remember_off_writes_nothing, turning_remember_on_captures_the_current_size, the_size_follows_the_window_across_a_project_switch}`; core `legacy_and_invalid_window_sizes_do_not_break_layout_restoration`. |
| P1-WIN-05 | The title is `RiWork · <project>`, and is set only when it changes. | 023 · P1c | nsh-test `Window.test.tsx`: pure `windowTitle()` cases; review: `Window.setTitle` called from one memoised effect; fixture+cua: `list_windows` titles. |
| P1-WIN-06 | Window-controls island 78×28: drawn with panel_active fill, radius 8 and divider borders in colourful themes; Native draws nothing but keeps the drag region; hidden in fullscreen. Drag regions: the island, empty strip space (≥ 18 pt) and the 12 pt handle (`desktop={{ windowDrag: true }}`); only panes touching the top get them. A double-click follows the system zoom/minimise preference. | 024, 025, 131, 288 · §2.5 · P1c, P1b | nsh-test `Chrome.test.tsx`: `id:window-controls-island` shown in all 7 themes, absent with `window.state {mode: "fullscreen"}`; `metrics.test.ts` (island 78×28, inset 92 = 78 + 14); appkit `titlebar_double_click_reads_the_system_preference`; manual evidence `win-06-<theme>.png`. |
| P1-WIN-07 | Closing the last window quits (tooltips excluded). Other closes drop the window's Dock item. Tooltips are not windows for quit or cascade, and hide on move, deactivation and project switch. | 027, 329 · §2.5 · P1b | host-unit `window_registry::tests::{the_last_close_quits, tooltips_do_not_count}`; appkit `tooltips_create_no_nswindow`. |
| P1-WIN-08 | A closing or dropped window saves its layout from Rust (claims are deferred). Quit saves every project window's layout and the global state, then quits. | 028 (part), 029 · §2.3 `window.close`, `app.quit`, §2.5 · P1b | host-unit `quit::tests::quit_flushes_every_layout_session_and_the_global_state_from_rust`, `close_flushes_before_the_window_goes`; RLD-01 end to end. |
| P1-WIN-09 | `window.state` is sent on change only, with `key`, `visible`, `occluded`, `mode`, `backingScale`, `frame` and `appActive`. Activation marks the Dock frontmost; deactivation hides tooltips. | 030 (part), 031 (part) · §2.4 · P1b | host-unit `window_state::tests::events_only_on_change`; nsh-test: `host.send("window.state", {...occluded: true})` → no error and no host call. |
| P1-WIN-10 | ⌘⇧N opens another window on the current project. | 032 (part), 033 (part) · §2.3 `window.open` · P1c, P1b | nsh-test `cmd+shift+n` → `app.host.calls()` has `window.open {kind: "project", projectId: <Alpha>}`; host-unit cascade (WIN-02). |

### 5.4 Global window

| ID | Accept when | Rows · contract · owner | Route and check |
|---|---|---|---|
| P1-GLB-01 | At most one global window. Every way in focuses it, sends `global.show` and returns `created: false`. It works with no window open. | 034 · §2.3 `window.open`, §2.4 `global.show` · P1b | host-unit `window_registry::tests::the_global_window_is_a_singleton_and_opens_with_no_window`; nsh-test `Global.test.tsx`: `host.send("global.show", {view: "chat", start: false})` → `id:global-view-chat` selected. |
| P1-GLB-02 | Title `RiWork · Global`; 1100×760 or the last ordinary size; an impossible size is dropped; size capture is unconditional (G3); the Dock lists it as `Global` with no branch. | 035, 052 (part) · P1b, P1c | core global_window tests (size validation at `crates/riwork-core/src/global_window.rs:215-222`); fixture+cua title in RLD-01; nsh-test `window.setDockInfo {project: "Global", branch: ""}` posted. |
| P1-GLB-03 | `global-window.json` (view, `show_quiet`, filter, sort, size) is saved on view/filter/sort change and on release; atomic; > 64 KiB rejected; equal state not rewritten; never in a layout; a save error becomes the notice; search is not saved. | 036, 037, 067 (part) · §1.10 `Dashboard` · P1c | core global_window tests; nsh-test: press `id:global-view-chat`, wait 300 ms, open a second global app → chat selected. |
| P1-GLB-04 | Reload records it as kind `global`, with no project id or layout and path `<home>/global-window` (never created). Restore reopens it as global. Handed-over state wins over the file. | 038 · §2.2 · P1b, P1c | core `the_global_window_restores_as_itself_and_older_snapshots_hold_project_windows`; host-unit `runtime_window::tests::the_global_window_is_recorded_as_global`; nsh-test: `params.restore.global.view = "chat"`, file says dashboard → chat selected. |
| P1-GLB-05 | It belongs to no project: it never registers, selects or switches one, and never writes layouts, shared tabs or chat requests. Opening it starts nothing. | 039 · P1c | nsh-test with `kind: "global", projectId: null`; fixture write allow-list for a global-only run: only `global-window.json` and an empty `schedules.lock` (amended 2026-10-28, [§1.1](#11-decision-record), R-DEFER 5); `state.json` sha256 unchanged. |
| P1-GLB-06 | Bar: dashboard glyph and `Global` in magenta; segmented Dashboard / Chat (`global-view-dashboard`, `global-view-chat`); a drag region; muted `This Mac · every project`; 34 pt; inset 92 / 10 pt. Switching view saves state. Both bodies are placeholders. | 040, 041 (part) · P1c | nsh-test texts and `app.press("id:global-view-chat")`; `metrics.test.ts`. |
| P1-GLB-07 | The root has focus from the start; ⌘W closes the global window. | 042 · §2.3 `window.close` · P1c | nsh-test `cmd+w` right after open → `app.host.calls()` has `window.close` (C-C-11 picks this path). |
| P1-GLB-08 | From the global window: ⌘⇧N and Dock New Window open the frontmost local project window's project, else the active project, else show `Open one of this Mac's projects first.`. ⌘, and ⌘⇧S route the panel to that project's window. | 044, 045 · §2.3 `window.list`, `window.route` · P1c, P1b | nsh-test with a `window.list` handler → `window.open {kind: "project", projectId}`; empty list and no active project → notice; `cmd+,` → `window.route {step: {panel: "settings"}}`; host-unit for Dock New Window. |
| P1-GLB-09 | `window.route` searches frontmost first, then minimised and other-Space windows. The first window showing the project is raised and gets `route`; with none, a window opens by id. The global window keeps its view; the active project is unchanged. | 062 · §2.3 `window.route`, §2.4 `route` · P1b, P1c | host-unit `window_registry::tests::route_prefers_frontmost_then_minimised_then_opens`; nsh-test: `host.send("route", {step: {panel: "settings"}})` → Settings tab selected. |
| P1-GLB-10 | Bottom notice bar: gold text and Dismiss (`global-notice-dismiss`). | 066 · §2.2 `notice` · P1c | nsh-test: `params.notice` shown; press `id:global-notice-dismiss` → gone. |

### 5.5 Panes, splits, locks, main pane and focus (P1c)

| ID | Accept when | Rows · contract | Route and check |
|---|---|---|---|
| P1-PANE-01 | A project with no layout (or `panels_initialized = false` with the sidebar visible) opens as riwork-core's default: locked navigation pane [Projects, Files, Worktrees, Tasks, Shells] at 0.27 beside the main pane. With no live shells, the main pane holds the one shell `Sessions.create` returned (SHELL-157). | 081 · §4.5 step 3; L10 | bridge `layout_session.rs::no_layout_opens_riwork_cores_default` (projection equals `project()` of riwork-core's default); nsh-test `Panes.test.tsx` Gamma: 2 groups; the 5 panel titles in order. |
| P1-PANE-02 | ⌘D / ⌘⇧D leave focus mode and split the active pane 50/50, new pane second, holding a new shell, selected and active. A failed create undoes the split and shows the error. | 082 · §4.4 (session method); L3 | bridge `split_right_adds_the_new_pane_second_at_one_half`, `a_failed_create_undoes_the_split`; nsh-test Alpha `cmd+d` → `app.count("role:tablist")` + 1 and the new tab selected; Beta (create fails) → count unchanged, notice shows the fake's message. |
| P1-PANE-03 | Dividers are 5 pt. A drag sets the ratio live, clamped to 0.1–0.9, and the layout saves on release (≤ 250 ms trailing). | 083 · §4.3 rule 5, §4.4 sizes | core `split_ratios_round_trip_and_survive_insertions_and_collapses`, `invalid_ratios_are_repaired_before_persistence`; bridge `gesture_sizes_recomputes_binary_ratios`, `a_dragged_ratio_is_clamped_to_0_1_0_9` (Rust `Headless` DockArea drag); nsh-test `app.drag("label:Pane divider", 200, 0)` → a second app after 300 ms shows the same groups (no fallback rebuild). |
| P1-PANE-04 | On window resize, locked panes keep their pixels and the others absorb the change (minimum 100). Changes in the first 2 s after first draw are ignored. The save is debounced 250 ms. Focus mode leaves ratios alone. | 084, 320 (part) · §4.3 rule 5, §4.4 `setExtent`; L11 | core `preserve_locked` tests (3547–3742); bridge `set_extent_matches_preserve_locked_at_three_extents` (1280×800 → 1920×1080 → 640×400 and back; `pane_extents` within 0.5 px), `the_projection_gives_null_to_the_single_absorber`, `the_first_2_s_after_first_draw_are_ignored`, `focus_mode_leaves_ratios_alone`. |
| P1-PANE-05 | Lock state: the explicit set, else auto (first pane holding Projects, Worktrees or Files). The lock button shows at headers ≥ 108 pt, else the menu item. A locked pane refuses user closes with `Unlock the pane to close its tabs` / `Unlock the pane to close it`; its X and Close pane are not drawn. Internal removals are never refused. Not DockArea's `locked` (C-C-08). | 085, 086 | moved `lock_state_follows_the_users_choice_and_defaults_to_the_navigation_pane`; core `stale_lock_ids_are_removed_without_reenabling_auto_lock`; nsh-test L-NAV: select a Projects tab, `cmd+w` → first notice; `cmd+shift+w` → second; `app.shows("id:tab-close-panel:projects")` false; at a 640-wide viewport the pane menu offers `Unlock pane`. |
| P1-PANE-06 | At most one main pane, toggled from the pane menu (checkbox). Its marker shows at 136 / 152 pt. New tabs, clicked shells, chats and previews open in the main pane, except in focus mode. A missing main pane counts for nothing. Bring-up rule (SHELL-088). | 087, 088; L7 | moved routing tests (§4.3); core `a_main_pane_that_is_not_in_the_layout_is_dropped_and_there_is_never_more_than_one`; nsh-test L-NESTED: select pane 3, `cmd+t` → the new tab is in `pane-2` (main); `cmd+shift+f`, `cmd+t` → lands in the active pane. |
| P1-PANE-07 | Close pane: a non-main unlocked pane's tabs go to the end of the main pane in order, keeping membership; otherwise its tabs close. Focus goes to the inheriting pane. The last pane is never removed. An emptied non-main pane disappears; an empty main pane stays and shows `Drop a tab here or use the pane menu`. | 089, 090 · §4.3 rule 3, §4.4 group disappeared; L8 | moved close-pane tests (§4.3); bridge `an_empty_main_keeps_its_sentinel_through_every_gesture`; nsh-test L-EMPTY shows the empty text; L-NESTED `cmd+shift+w` on pane 3 → its tabs follow the main pane's. |
| P1-PANE-08 | Gather (⌘⇧M, layout menu, pane menu) behaves as SHELL-091, including its notices and the locked-neighbour rule. | 091; L9 | moved gather tests; nsh-test L-NESTED `cmd+shift+m` → `Gathered N tab(s) into the main pane`; L-UNLOCK → `No main pane. Choose Main pane in a pane's … menu first`. |
| P1-PANE-09 | Default layout (⌘⌥L, layout menu, View menu) behaves as SHELL-092: nothing closed, ids reused, its notices, and `The default layout is already in place` the second time. | 092; L10 | moved default-layout tests (5); nsh-test L-NESTED `cmd+alt+l` → 2 groups and a notice starting `Applied the default layout`; again → `The default layout is already in place`; `host.send("command", {id: "defaultLayout"})` does the same. |
| P1-PANE-10 | Focus mode (⌘⇧F, Focus button ≥ 180 pt, menu) shows only the active pane. Centred mode caps at 1100 with a 30 % bottom margin. `↙ RESTORE  ⌘⇧F` exits. Split, gather, default layout and project load exit it. The split tree and the window frame never change. The state is in the reload snapshot. DockArea stays mounted. | 093 · §4.3 rule 6; P9 | bridge `focus_mode_never_changes_the_saved_layout`; nsh-test `cmd+shift+f` → one tablist and the RESTORE text; `cmd+d` exits; mount counters unchanged (DRAG-04 c); snapshot field in RLD-01. |
| P1-PANE-11 | A click in a pane body selects the pane and focuses its content (with the search field focused, it only marks the pane active). | 094, 095 (part) | nsh-test L-UNLOCK (no main pane): `app.press("id:pane-3")`, `cmd+t` → the new tab is in pane 3. |
| P1-PANE-12 | Pane header 28 pt (grows with text); Native buttons 22 / 2 / 3, menu button 28. The top-left pane insets its tabs by 92 pt. The pane "…" menu has GPUI's rows, sections, shortcut labels and conditional rows; rows of deferred features are present and disabled. | 026, 096, 097 · R-DEFER 2 | nsh-test `PaneMenu.test.tsx`: `app.press("id:pane-menu-<id>")` → items in order via `app.menu()` (with `Menu.popup`) or `app.texts()` (with a Popover); `app.enabled("label:Codex")` false; `Close pane` absent on a locked pane; `metrics.test.ts`; manual evidence. |

### 5.6 Tabs, strip, menus and the close dialog (P1c, with P1a `Tabs`/`Sessions`)

| ID | Accept when | Rows · contract | Route and check |
|---|---|---|---|
| P1-TAB-01 | Tab kinds and saved keys (`shell:`, `panel:`, `remote:`, `chat:`, `app_preview:`); DockArea panel id = `SavedTab::key()`; a chat without an id is never saved. | 099 · §4.6 | core `a_remote_shell_tab_has_a_fixed_json_shape_and_key`, `a_chat_tab_has_a_fixed_json_shape_and_key`; bridge `chat_pending_is_never_saved_and_maps_to_chat_id`. |
| P1-TAB-02 | ⌘T makes a shell (fake) in the main or active pane, with cwd = selected worktree else project root, selected and saved. Opening a panel selects it where it exists, else opens it in the pane. Dashboard never becomes a tab: it calls `window.open` for the global window. Panel titles follow theme casing and tooltips. | 077 (part), 100 (part), 110 (part), 111 | bridge `new_shell_uses_the_selected_worktree_else_the_root`; nsh-test `Tabs.test.tsx`: `cmd+shift+e` twice → one Files tab; pane menu Dashboard → `window.open {kind: "global", view: "dashboard", start: false}` and no `tab-panel:dashboard`; Native → `Projects`, RiWork theme → `PROJECTS`; `app.hover("id:tab-panel:projects")` → `Projects · ⌘B`. |
| P1-TAB-03 | Clicking a tab selects it, shows only its surface, scrolls it into view, focuses it and saves. | 103 (part) · §4.4 activeIndex | bridge `gesture_active_index_sets_the_active_tab_and_pane`; nsh-test: press a tab → its placeholder is shown and the previous one is hidden. |
| P1-TAB-04 | Close flow: ⌘W or X on a local session key goes through the close choice (Ask / Detach / Exit; workers always detach; `tab_close_behavior`). Remote, panel and preview tabs close at once. The dialog text is exact (D-COPY-1): Detach has initial focus, backdrop clicks do nothing, Esc cancels, width 380. Detach hides the shared entry; Exit also stops the session (fake). Remote-project tabs close at once. A locked pane refuses. | 105, 144–148, 326 (part) · §1.7 `Tabs.update`, `Sessions.close` | moved `the_orchestrator_closes_like_any_user_chat_and_workers_only_detach`; nsh-test `CloseDialog.test.tsx`: `cmd+w` on a shell tab → `Close <title>?` and the body; `escape` → tab stays; `cmd+w`, press `label:Detach` → gone in a second app after 300 ms, and the fake session is still live; Exit → the fake records `close`. |
| P1-TAB-05 | Adoption: sessions made outside the window are appended to the adoption pane (main if unlocked, else active, else first unlocked; none if all locked) behind the current tab. Focus, selection and active pane don't change. At most 16 per apply. Only live Project-kind shells of this project. | 177, 178 · §1.7 `Sessions.subscribe` | moved adoption tests; bridge `adoption_caps_at_16_per_apply`, `all_locked_adopts_nothing`; nsh-test: the fake's `appear_after_ms: 300` shell → `await app.waitFor("id:tab-shell:<id>")`, and the previously selected placeholder is still shown. |
| P1-STRIP-01 | A local project with a shared list draws the single strip; a remote project or unavailable list keeps the classic bar. Strip order: panels first (segment `Panels`), then sessions in shared order, then unlisted carried views in their relative order. | 114, 115; L5 | moved `panels_lead_then_sessions_in_shared_order`; nsh-test `Strip.test.tsx` (port of `the_strip_draws_panels_then_shared_order_as_named_tabs`): Alpha order via `app.texts()`; Beta (unreadable list) → one tab list per pane. |
| P1-STRIP-02 | Session cells: metrics, status dot colours and status source mapping, title source, a11y name `<title>, <status>` (role Tab; strip role TabList `Tabs`), close mark (always on selected, on hover otherwise, tooltip `Close tab · ⌘W`), 32 pt panel cells, flat styling. Chat-summary hints and agent activity are deferred. | 118–124 (parts), 127, 128 | moved `stopped_and_unknown_activity_map_to_the_phones_states`; nsh-test `app.shows("label:Chat 2, waiting")`, `app.count("role:tablist")`; fixture+cua `verify_state` element `label_contains: ", waiting"`; `metrics.test.ts`; manual evidence per theme. |
| P1-STRIP-03 | Tab context menu (right-click, Shift+F10, Menu key on a tab with a shared entry): Rename… / Move left / Move right / Close / Cancel with separators and disabled moves. Rename opens a popover prefilled with placeholder `Tab title`; Enter or Save title applies it. Moves are adjacent within the parent group. Every outcome refocuses the active pane. Middle-click is deferred (upstream GAP). | 125 (part), 126, 139–143 | port of `keyboard_menu_moves_focus_and_rename_uses_persistent_input`; moved `shared_menu_moves_are_adjacent_and_stay_in_parent_groups`; nsh-test: `app.rightClick("id:tab-shell:S1")` → `app.menu()` labels in order, `Move left` disabled; Rename → `app.type`, `app.submit` → a second app shows the new title. |
| P1-STRIP-04 | `All tabs` appears only on overflow and stays while its menu is open. The selected tab auto-reveals after width, count, selection, position or rename changes. The menu lists every session tab in strip order with a dot and a check; 260 wide, max 420 with scrolling; Esc, Up/Down wrap, outside click; it closes on focus mode, strip loss, pane close, or a shortcut changing the selection. | 129, 130, 135–138 | moved `all_tabs_appears_only_when_the_scrolling_tabs_do_not_fit`; ports of the two auto-reveal tests and `arrows_stay_inside_an_open_strip_menu_so_escape_closes_it_and_thaws`; nsh-test Busy: `setViewport(800, 780)` → `id:strip-all-tabs` shown; at 1920 → hidden; `cmd+9` → last tab shown. |
| P1-STRIP-05 | Shared membership: a snapshot applies only on an epoch change or revision ≥ known. Hidden or Stopped entries leave every pane (the session runs on; selection kept by id). Store-title priority. The main pane re-sorts to shared order. Not applied during a drag, menu or dialog, but applied afterwards. Updates re-snapshot and keep live status. Close and hide go to the session's own project store, with the exact error notices. | 179–183, 186, 187 · §1.7 `Tabs` | core `stale_inventory_keeps_hide_and_rename`, `one_refresh_transaction_ends_where_separate_transactions_did`, `titles_come_from_a_rename_then_the_provider_then_the_first_message_then_the_kind`, `a_title_is_set_once_and_only_a_higher_source_or_a_new_rename_replaces_it`, `new_sessions_append_and_any_tab_hides`; bridge `tabs.rs::stale_revisions_are_dropped`; nsh-test with **two windows**: `open()` Alpha twice; Detach in window 1 → `await` window 2 `waitFor` the tab gone (shared store + process watcher). |

### 5.7 Drag and panel identity (P1c)

| ID | Accept when | Rows · contract | Route and check |
|---|---|---|---|
| P1-DRAG-01 | Tab drag: a ghost (title, 12×7 padding, cyan border, 11 pt). Starting closes the pane menu. A drop on a tab inserts at its index (adjusted when moving right); the strip end or empty space appends. The moved tab is selected and its pane becomes active. An emptied non-main pane is removed. A strip reorder publishes shared `Move {key, before}` (leftward lands before, rightward after; carried foreign/global views are never operands; a self-drop does nothing). | 106, 132 (part), 133, 134, 185 · §4.4 reorder / move / group disappeared; L4, L6 | moved strip-move and drag-mapping tests; bridge `layout_gestures.rs::gesture_reorder_in_group`, `gesture_move_to_group`; nsh-test `Drag.test.tsx` L-NESTED: `app.drag("id:tab-shell:S3", dx, dy)` onto pane 2's tab bar (dx, dy from the golden projection sizes) → S3 in pane 2 and selected; the source shows its neighbour; a second app shows the same. |
| P1-DRAG-02 | Pane-edge drops split that side (left/top puts the new pane first) and centre drops append, through DockArea. RiWork's 23 % thresholds, 12 pt window-edge strips and top root dock are an upstream GAP (contracts §6): P1 accepts DockArea's geometry and records it. | 107 · §4.4 move to edge, §6 | bridge `gesture_move_to_edge_splits_with_split_with_ratio`; nsh-test: drag a tab to pane 1's right edge → a new group to its right. The branch report records the measured edge-zone fraction and the absence of root/top targets. |
| P1-DRAG-03 | Drops work only within the window and project of origin. No cross-window move: `tab.transfer` / `tab.receive` are not served. | 108 · §2.3 reserved | host-unit `host_names::tests::tab_transfer_is_refused`; review: no handler registered. |
| P1-DRAG-04 | **Panel instance identity.** A panel's content component mounts once and survives: (a) a move between groups, (b) a move to an edge split, (c) focus mode on/off, (d) window resize, (e) a project switch for tabs carried in a locked region. Hidden tabs get `visible = false`, shown ones `true`. This de-risks the port gate "a moved tab keeps its `riwork.terminal` instance"; NSView re-parenting itself is checked in the terminal phase. | 149 (part) · §3.5 inv. 3, §3.2 `visible`; T2, T6 | nsh-test `Identity.test.tsx`: the placeholder renders `mount <n>` from a module counter keyed by panel id and `visible: <bool>`; after each of (a)–(e) the shown placeholder still reads `mount 1`. |

### 5.8 Layout persistence, restore and reopening GPUI layouts (P1c)

| ID | Accept when | Rows · contract | Route and check |
|---|---|---|---|
| P1-LAY-01 | Round-trip oracle: `project` then `applyDock(unchanged)` gives `normalize(input)` for every fixture listed in §4.7 and every L-* fixture. Ratios match within 1e-4; flattened shapes give `pane_extents` within 0.5 px at 1280×800, 1920×1080 and 640×400. | 154 · §4.7 | bridge `layout_oracle.rs`: one test per listed riwork-core test (`oracle_<core test name>`) plus `oracle_<fixture>`. |
| P1-LAY-02 | One test per §4.4 gesture row, including the fallback (with a `layout.fallback` diagnostic) and the sentinel drop. After apply: normalize, re-project, one projection per gesture. TSX installs only the returned projection. | §4.4 | bridge `layout_gestures.rs::{gesture_active_index, gesture_sizes, gesture_reorder_in_group, gesture_move_to_group, gesture_move_to_edge, gesture_group_disappeared_keeps_main_and_locked_neighbours, gesture_sentinel_next_to_real_panels_is_dropped, gesture_two_changes_fall_back_and_log}`. Each takes DockArea's real `onLayoutChange` output from a Rust `nsh_testing::Headless` drag, not a hand-built layout. |
| P1-LAY-03 | **Reopening GPUI layouts.** Each L-* fixture renders its golden groups, tabs and selection. Open plus close with no edit writes nothing (sha256 and mtime unchanged). After one gesture, the saved entry equals riwork-core's own save of the equivalent `ProjectLayout` edit (JSON value). Other project entries are JSON-value identical. Unknown fields and unparsable entries are byte-identical as raw text. | 154 · §4.2; P3, P4 | bridge `layout_reopen.rs::{reopen_<fixture>_renders_golden_and_writes_nothing, edit_<fixture>_saves_what_riwork_core_saves}`; nsh-test `Reopen.test.tsx` on `p1-layouts`: for each manifest entry, `open` → expected titles and selection. |
| P1-LAY-04 | An unreadable layout (corrupt, newer schema, unreadable entry) opens the default layout with the notice `<error>; using a default layout`. Stored data is untouched; a newer schema is never overwritten; a corrupt file is set aside on the next save. | 156 · §4.5 step 3; P4 | core `corrupt_file_is_set_aside_on_save_instead_of_crashing_or_being_deleted`, `newer_schema_is_neither_loaded_nor_overwritten`; bridge `open_session_on_{d_newer, d_corrupt, d_entry}`; nsh-test: notice text ends `; using a default layout`. |
| P1-LAY-05 | Legacy layouts: `shell_ids` / bottom strips migrate with order and selection. Older Dashboard, global-chat and live global-orchestrator tabs are left out on load and dropped on the next save; `MOVED_NOTICE` is shown and the global window is not opened. Remote-shell and preview tabs never leave. | 078 · G4, P2 | core `legacy_shell_layout_migrates_without_changing_tab_order_or_selection`, `mixed_tabs_preserve_order_and_selection_while_migrating_bottom_strips`; bridge ports of the two `global_nav` older-layout tests; nsh-test L-GLOBAL-TABS: the exact MOVED_NOTICE text; no `window.open` call. |
| P1-LAY-06 | Restore reconciliation: rebuilt pane by pane; live shells only (dead and detached skipped; the claim check is deferred); selection from `active_tab_key`; CLI-made live project shells join main or the first unlocked pane; no layout and no live shells → one new shell. A failed restore shows the notice, keeps saved positions, and retries on the next `Sessions` delivery (no 2 s timer). | 157 (part), 158 · §4.5 step 4; P6 | bridge `restore_skips_dead_and_detached_shells`, `restore_adopts_cli_shells_into_main`, `a_failed_restore_retries_on_the_next_sessions_delivery_and_keeps_positions`. |
| P1-LAY-07 | Every SHELL-155 trigger saves once, trailing 250 ms on the blocking pool, through `save_tab_layout` (shared store) or `save_chat_state`. Synchronous Rust flush on `window.close`, `app.quit`, `app.restart`, project switch and session drop. No periodic save. | 155 · §4.5 persistence | bridge `layout_session.rs::{each_trigger_saves_once_after_250_ms (table over the trigger list, ManualClock), flush_points_write_synchronously, nothing_is_saved_without_an_edit}`; host-unit (WIN-08); RLD-01 end to end. |
| P1-LAY-08 | Ids: PaneId = NodeId, adopted by NSH's counter. JS-unsafe ids are remapped to the smallest free ids (active, main, locks and skipped tabs follow) and persisted on the next save. Depth 32, 256 panes. Panel ids never change: `chat-pending:<uuid>` maps to `chat:<id>` internally with no remount; `LayoutSession.panel(id)` returns the `SavedTab`. | 209 (part) · §4.6 | bridge `big_pane_ids_are_remapped_and_persisted` (L-BIGID), `chat_pending_maps_without_changing_the_panel_id`; nsh-test L-BIGID renders. |
| P1-LAY-09 | Project switch (`project.open`): save the outgoing layout; carry locked regions (contents, selection, ratios); map colliding ids; recover displaced locked tabs; reset per-project panels. On error show `<err>; locked regions were not carried over`. Re-selecting the current project does nothing. The orchestrator carry (SHELL-229–231) is deferred and must not break the switch. | 160 · §2.4 `project.open`, §4.5 `switchProject`; P5 | core `carry_preserves_locked_tabs_ratios_and_maps_colliding_destination_ids`, `destination_locked_pane_sessions_are_recovered_into_unlocked_region`, `a_locked_main_pane_stays_the_main_pane_when_the_project_changes`; nsh-test L-CARRY: `app.host.send("project.open", {projectId: <dest>})` → the carried navigation tabs plus the destination's tabs; carried placeholders still `mount 1`. |
| P1-LAY-10 | Both directions: a home saved by the NSH build loads to the same `ProjectLayout` through riwork-core, as the GPUI build reads it. Only `LayoutStore` writes `layouts.json`. | 154 · §4.2, §5 compatibility | bridge `nsh_saved_layouts_load_identically_through_layout_store`; review: `rg -n 'layouts\.json' crates/riwork-nsh-native crates/riwork-desktop "$APP"` shows no writer outside riwork-core; optional: the reference build launched on an NSH-saved fixture home, `list_windows` title plus manual evidence. |

### 5.9 Menus and keys

| ID | Accept when | Rows · contract · owner | Route and check |
|---|---|---|---|
| P1-MENU-01 | The `RiWork` app menu: Settings… ⌘,, Project Settings… ⌘⌥A, Automations ⌘⇧S, a separator, Quit RiWork ⌘Q. No About, Hide or Services unless decided (parity SHELL-250 note). | 250 · §2.4 · P1b | appkit `appkit_shell::app_menu_matches_gpui` walks the built NSMenu (titles, order, separators, `keyEquivalent` + modifier mask, command id); host-unit `menu::tests::command_ids_are_the_v1_set`. |
| P1-MENU-02 | Edit menu: Cut, Copy, Paste, a separator, Select All, as the standard responder selectors. A native text field handles them; otherwise the AppView forwards them as `command {id: "copy"}` etc. | 251 · §2.4 Edit · P1b, P1c | appkit `edit_menu_uses_standard_selectors` (`cut:`, `copy:`, `paste:`, `selectAll:`); nsh-test `host.send("command", {id: "copy"})` with nothing selected → `app.clipboard()` unchanged. |
| P1-MENU-03 | View menu items exactly as SHELL-252. Window-level commands go to the key window. `openGlobalDashboard`, `openGlobalChat`, `biggerText`, `smallerText`, `actualSize` and `quit` work with no window open. ⌘W closes a tab, never a window. No Window or Help menu. | 079, 252, 299 · §2.4 · P1b | appkit `view_menu_matches_gpui`; host-unit `menu::tests::{window_commands_go_to_the_key_window, app_commands_work_with_no_window}`; nsh-test: each window-level id through `host.send("command", {id})` has its effect; deferred ids are inert. |
| P1-MENU-04 | Dock menu: one item per window; label/sort/uniqueness rules; the last key window checked; rebuilt only on change; `New Window` last after a separator. Choosing an item raises its window. New Window uses the frontmost project window's project, else the global-window rule. Each window posts `window.setDockInfo` on open, project switch and change only. | 031 (part), 052 (part), 253–258 · §2.3 `window.setDockInfo`, §2.5 · P1b (+P1c posts) | moved `dock_menu` tests (host-unit); appkit `dock_menu_from_a_fake_registry` (`applicationDockMenu:`); nsh-test: one `window.setDockInfo {project: "Alpha"}` post on open, none after an unchanged update. |
| P1-KEYS-01 | The full GPUI chord table is declared once. "No key bound twice" is ported as a scan of the TSX keymap plus the host key equivalents. Deferred chords are inert and listed in `DEFERRED_COMMANDS` = §8. | 240 · R-DEFER 3 · P1c, P1b | `$APP/tests/keymap.test.ts` "no key is bound twice and every GPUI chord is present" (port of main.rs:17650, including chat chords); review: `DEFERRED_COMMANDS` matches §8. |
| P1-KEYS-02 | ⌘1–⌘8 select the Nth session tab (panels skipped); ⌘9 the last; ⌘0 or a missing index does nothing; nothing while a modal is open; without a strip, pane order including panels. Ctrl-Tab / ⌘} / ⌘⇧] and their reverses cycle in drawn order, wrapping. | 104, 116, 117; L5 · P1c | moved `command_numbers_count_session_tabs_and_nine_is_the_last`; nsh-test `cmd+2`, `cmd+9`, `ctrl+tab`, `ctrl+shift+tab`. Ctrl-Tab under AppKit's key-view loop is a GAP (SHELL-104): **manual** check in the fixture app; the result is recorded. |
| P1-KEYS-03 | ⌘B, ⌘⇧E (into the pane holding a navigation panel), ⌘⇧P, ⌘⌥S, ⌘⇧S, ⌘, and ⌘⌥A open or select their (placeholder) tabs. ⌘⇧N opens a new window. ⌘⌥G, ⌘⇧O and ⌘Q are host menu key equivalents and are **not** bound in TSX. Deferred: ⌘F, agent keys, ⌘⌥⇧C, ⌘⌥O, ⌘N. | 239, 241 (part), 242 (part) · §2.4 · P1c, P1b | nsh-test per chord; `keymap.test.ts` asserts ⌘⌥G / ⌘⇧O / ⌘Q are absent from the TSX keymap; appkit: those key equivalents are on the View / RiWork menu items. |
| P1-KEYS-04 | While any modal is open, window shortcuts do nothing. Backdrop 60 % black, 16 pt padding. Esc order per SHELL-246. Any ⌘ or Ctrl shortcut closes an open strip menu first. | 245–247 · P1c | port of `arrows_stay_inside_an_open_strip_menu_so_escape_closes_it_and_thaws`; nsh-test: with the close dialog open, `cmd+d` → no split; with the All tabs menu open, `cmd+2` → menu closes, then the selection changes. |
| P1-KEYS-05 | Window actions sit on the window root; app-level actions go through the host menu and work with no project window. ⌘=, ⌘- and ⌘0 with a terminal first responder belong to the terminal (deferred). With no terminal they resize text. | 249 · §2.4 key-equivalent order · P1b | host-unit `menu::tests::routing`; appkit `text_size_key_equivalents_dispatch_without_a_window`. |

### 5.10 Themes, appearance and interface text

| ID | Accept when | Rows · contract · owner | Route and check |
|---|---|---|---|
| P1-THM-01 | 7 theme choices in snake_case; `ghostty` (`Follow Ghostty`) is the default; unknown saved keys are kept; labels come from core. | 274 · §1.10 `Settings` · P1a | moved `hermes_settings_round_trip_preserves_every_existing_theme_name`; review: no theme label hard-coded in TSX. |
| P1-THM-02 | Palettes exact: RiWork, Native light/dark and Hermes hex values; derived palettes for Catppuccin, Tokyo Night and Gruvbox (mix 0.035 / 0.075 / 0.20 / 0.58; ANSI 6 / 3 / 5; WCAG 4.5:1 nudge on panel_active); 16-colour tables exact; `dark` iff luminance < 0.5; diff colours from ANSI 2 / 1. | 275–279, 284, 285 · §1.10 `Appearance` · P1a | moved theme tests (§4.3); bridge `appearance.rs::every_preset_token_is_exact` (golden table of every token for every preset, hex from the parity rows). |
| P1-THM-03 | Terminal override choice (preset forces; Follow Ghostty only with `use_riwork_colors`; Native only with `native_terminal_colors`, default true; else the user's Ghostty config) is an `Appearance` field for the terminal phase. | 281 · P1a | moved `terminal_override_is_what_terminals_are_forced_to`, `native_terminals_match_the_theme_only_while_the_option_is_on`, the override part of `selected_theme_controls_terminals_and_keeps_legacy_preferences`. |
| P1-THM-04 | First paint uses the resolved palette (no RiWork flash). Before `Appearance` exists, the context default is RiWork. Resolved at startup from `settings.theme` and system dark; a settings load failure means defaults plus a log line. | 287, 290 · §2.2 `appearance` · P1a, P1b, P1c | nsh-test `Theme.test.tsx`: Native theme with `params.appearance: "light"` → the first `app.spanStyle("text:Projects")` colour is #1d1d1f; with no `Appearance` yet → #d3e1e6. |
| P1-THM-05 | `RIWORK_APPEARANCE` pins NSApp. A macOS light/dark switch updates Native at once in every window. Every window re-applies on a Settings or Appearance change. Another process's `settings.json` write reaches every window through the process watcher (no per-window poll). A theme ≠ resolved is resolved first (313 b); errors become the notice (313 e). | 289, 291 (part), 292, 293, 296, 313 (part) · §1.3, §2.4 `appearance` · P1a, P1b, P1c | host-unit `startup::tests::forced_appearance_from_env`; bridge `settings.rs::a_write_by_another_process_reaches_every_subscriber_once`; nsh-test two windows: `app.setColorScheme("dark")` on each → Native colours flip in that window; manual evidence `thm-05-light.png` with `RIWORK_APPEARANCE=light`. |
| P1-THM-06 | P1 never writes `appearance.json` (the publisher is deferred with Ghostty parsing). | 294, 295 deferred · P1a, P1b | fixture write allow-list (§3.3); for a home that has one, sha256 unchanged; review: no call to the `appearance_file` writer. |
| P1-THM-07 | Follow Ghostty resolves as decided in D-THEME-1. | 280 deferred · P1a | bridge `follow_ghostty_without_a_parser_uses_the_riwork_palette_and_keeps_the_error`. |
| P1-TXT-01 | Text size: default 11, range 9–24, stored in tenths, written as a plain JSON number. A step moves one whole point with snapping, from settings read under the store lock. Reset sets 11. Never computed in TSX. | 297, 301 · P1a | core `saved_sizes_round_to_tenths_clamp_and_write_plain_numbers`, `a_step_starts_from_the_stored_size_not_the_cached_one`, moved `ui_text_size_defaults_to_today_clamps_and_keeps_odd_values_until_changed`; bridge `change_text_size_is_a_read_modify_write_under_the_lock` (two Settings handles race). |
| P1-TXT-02 | ⌘= / ⌘+ / ⌘⇧= bigger, ⌘- smaller, ⌘0 reset, through View ▸ Bigger Text / Smaller Text / Actual Size (⌘= is the menu hint). Not applied while a terminal or the file preview has focus (both deferred). | 298 (part), 299 · P1b, P1c | appkit `view_menu_text_size_items`; nsh-test `host.send("command", {id: "biggerText"})` → `app.spanStyle(...)` size 12; `actualSize` → 11. |
| P1-TXT-03 | Face: Menlo in colourful themes; system font in Native; the default "System + monospace accents" uses SF Mono for technical text (Menlo if `system.fonts` lacks it). Native and Hermes +2 pt unless matching the terminal. `scale = (points + offset) / 11`; `space(base) = round(base × max(scale, 1))`. Tooltips hide and windows redraw on change. Casing via `cased` / `sentence_case` with the 13 proper nouns and 14 acronyms. | 303–307 · §2.3 `system.fonts` · P1a, P1c | new core test `sentence_case_keeps_proper_nouns_and_acronyms` lands **before** the port; nsh-test `TextSize.test.tsx`: `spanStyle` family and size per theme; a `system.fonts` handler without SF Mono → Menlo; pure `space()` test. |

### 5.11 Status bar (P1c, with P1a `Settings`)

| ID | Accept when | Rows · contract | Route and check |
|---|---|---|---|
| P1-STAT-01 | Bar: 22 pt, 9 pt muted text, padding 12 / 8, gap 16 / 10, one line, left ≤ 40 % (100 % when the right is empty), each side scrolls. 11 kinds with their defaults and sides; tolerant parsing; `move_item` / `set_side` / `set_visible`; the master switch keeps choices. Every change goes through the Settings owner to all windows. | 262–265, 267 · §1.10 `Settings` | core status_bar tests (`crates/riwork-core/src/status_bar.rs:320,385,438`), moved `routing_and_reordering_preserve_other_side_and_round_trip`; nsh-test `StatusBar.test.tsx`: default order; `status_bar.enabled = false` → `id:status-bar` hidden; `metrics.test.ts`. |
| P1-STAT-02 | Items with P1 data: Project and Worktree (they open their placeholder panels), Live sessions count, Layout. Session ID is disabled without a session. Items without data show their no-data form (R-DEFER 4). | 259 (part) | nsh-test: texts `Project Alpha`; `app.press("id:status-item-project")` → the Projects tab is selected. |
| P1-STAT-03 | Layout menu: 296 pt, above the item, 6 pt from window edges; rows `Default layout ⌘⌥L`, `Gather into main pane ⌘⇧M` (with its no-main detail), `Main pane: <summary>`. The item toggles it; it closes when the item is hidden. | 260 | nsh-test: press `id:status-item-layout` → the three rows; press again → gone; Default layout → the PANE-09 effect. |
| P1-STAT-04 | Notice toast: one at a time, top right (3 pt below the header, 8 pt from the edge), ≤ 520 pt, gold text. × (`Dismiss`) or Esc on × closes it and focus returns. No auto-dismiss; a newer notice replaces the older. Drawing above terminals is the terminal phase's. | 261 | nsh-test with `{ clock: {} }`: two different notices → only the second shows; `app.clock.runFor(60_000)` → still shown; press `label:Dismiss` → gone. |
| P1-STAT-05 | Global item `top-orchestrator` (aria `Global: dashboard and chat`), shown per settings. A click toggles its menu (not while a modal is open) and closes other menus. Esc, an outside click or hiding the item closes it. 300 pt wide (≤ window − 12). Rows Dashboard ⌘⌥G / Chat ⌘⇧O, with a check on the shown view. A row click calls `window.open` for the global window (Dashboard `start: false`, Chat `start: true`) and focuses the active tab. Row details are deferred. | 069–075 (parts) · §2.3 `window.open` | port of `the_global_item_opens_a_menu_whose_rows_bring_up_the_global_window`; nsh-test: press `id:top-orchestrator` → `Global`, `Dashboard`, `⌘⌥G`; press `label:Dashboard` → `window.open {kind: "global", view: "dashboard", start: false}`. |

### 5.12 Polling, idle work and performance

| ID | Accept when | Rows · contract · owner | Route and check |
|---|---|---|---|
| P1-POLL-01 | No per-window or per-frame polling: the 2 s workspace refresh is replaced by subscriptions (`Store`, `Sessions` through one `SessionSampler` per process, `Tabs`, `Settings`, `Appearance`). The first delivery is a snapshot; `rev` is monotonic; after a gap or `resync` the snapshot is authoritative; stale results never win (171). | 171, 314, 315 (part) · §1.3 · P1a | bridge `subscriptions.rs::{the_first_delivery_is_a_snapshot, rev_is_monotonic_per_topic_per_process, overflow_clears_the_queue_and_sends_one_resync_snapshot, one_delivery_per_topic_per_engine_turn, streaming_topics_coalesce_to_16_ms_trailing, closing_or_dropping_the_owner_ends_the_task, a_project_switch_resubscribes_and_drops_late_deliveries}`; `hubs.rs::one_watcher_and_one_sampler_per_home_per_process` (two engines). |
| P1-POLL-02 | Process-level timers only, in Rust: the host's runtime tick; the sampler's 2 s tmux liveness fallback, only while shells exist. No idle redraw timer in P1. A quiet home produces zero deliveries. | 317 (part), 318 (part), 319 · §1.3 table · P1a, P1b | bridge `a_quiet_home_produces_no_deliveries_for_60_s` (ManualClock); nsh-test `Idle.test.tsx`: `open(..., { clock: {} })`, settle, `app.clock.runFor(60_000)` → `app.host.calls()` length unchanged. |
| P1-POLL-03 | TSX has no `setInterval`, `requestAnimationFrame` or repeated `refetch` for data. The check runs in the branch's test script. | §1.3 lint · P1c | `rg -n 'setInterval\(\|requestAnimationFrame\(\|\.refetch\(' "$APP" -g '!**/*.test.ts*'` → no hits outside code annotated as animation. |
| P1-PERF-01 | The busy-workspace frame and idle CPU are **measured and reported, not asserted**, against a matched GPUI run (D-PERF-1). | 098 · §5 perf · P1c, P1b | GPUI reference, from the `ea0f2f5` archive (§3.4): `RIWORK_FRAME_TABS=24 cargo test -p riwork-desktop --release measure_a_busy_workspace_frame -- --ignored --nocapture`. NSH: the same shape (Busy, 24 tabs, 120 projects, 1220×780, Native) in a release fixture run; median and p90 of NSH's settled surface-frame monitor over ≥ 41 frames (the monitor and boundaries of `spikes/host-multiwindow/WORKER_REPORT.md`). Idle: two idle windows for 20 s, `ps -o time= -p PID` delta as % of a core, against GPUI's 0.65 %. Both go in a table in the branch report. |
| P1-PERF-02 | Structural guards (hard): zero deliveries while idle (POLL-02); exactly one projection per `onLayoutChange`; no fallback for any §4.4 gesture row. | §4.4 · P1c | bridge counters in `layout_gestures.rs`. |

### 5.13 `riwork/native` contract items (P1a)

| ID | Accept when | Contract | Route and check |
|---|---|---|---|
| P1-NAT-01 | Crate `riwork-nsh-native`, `#[native_module("riwork/native")]`, types generated at `.nsh/types/riwork/native.d.ts`; PascalCase modules, camelCase functions and fields; DTO wrappers live in the crate, and riwork-core/engine serde types are unchanged. | §1.1 | `"$NSH" check crates/riwork-nsh-native/tests/app` (a probe importing every P1 module); `git diff ea0f2f5...nsh-p1-native -- crates/riwork-core crates/riwork-engine` has no serde attribute or format change. |
| P1-NAT-02 | Sync functions are pure in-memory work under 1 ms. Async bodies only hop to `spawn_blocking`. Errors are `"<code>: <message>"` with the 7 frozen codes only. | §1.2 | review: a table of every `#[module]` function (kind, touches disk / socket / lock?); bridge `errors_use_only_the_frozen_codes`. |
| P1-NAT-03 | Each module adapts a backend trait with a production and a fake implementation. `AppOptions::data_dir` = `RIWORK_HOME/nsh`. No `nsh/storage`. | §1.4 | review; `rg -n 'nsh/storage' "$APP" crates/riwork-nsh-native` → none. |
| P1-NAT-04 | A headless `nsh test` proves delivery across the bridge: a snapshot first, then a change after a write made through the bridge, seen by two engines. | §1.3, §1.4 | `crates/riwork-nsh-native/tests/app/App.test.tsx`: two `open()`; `Store.subscribe` shows N folders; press `Add folder` (`Store.createFolder`) in one → both show N + 1. |
| P1-NAT-05 | Ids from JS are validated with the CLI's validators (`project_tabs::valid_key`, UUID parsing) and rejected as `invalid:`. | §5 security | bridge `tabs_update_rejects_invalid_keys`, `store_rejects_non_uuid_ids`. |

---

## 6. Exit criteria for Phase 1

1. Every Phase-1 item in §5 passes by its route, with commands, exit codes and the JUnit file
   recorded in the merging branch's report. The partial rows' P1 halves pass.
2. The §4.3 ledger is complete on `nsh`, and nothing listed there was deleted without a successor.
3. The write allow-list and the "no tmux server" checks pass for every fixture run.
4. The deviations accepted in P1 are recorded with measurements: DockArea drop zones (P1-DRAG-02),
   the Ctrl-Tab manual result (P1-KEYS-02), and Follow Ghostty per D-THEME-1.
5. ~~D-PERF-1 is decided, or explicitly carried to the chat phase with P1-PERF-01 published as
   report-only.~~ **Met 2026-10-27:** D-PERF-1 was adopted as proposed
   ([§1.1](#11-decision-record), contracts §8.2). P1-PERF-01 is published report-only, and Phase 1
   perf stays report-only until the chat phase.
6. §9 checklists are signed off for P1a, P1b and P1c.

Status note (2026-10-27): Phase 1 exit does not by itself open Phase 2. Phase 2's entry gate is the
pinned-runtime teardown proof (task e940bd0a), which must pass first. The cached-runtime
110–156 ms main-loop gaps are not accepted, and §3.7 is not amended (contracts §8.3).

---

## 7. Frame budget decision (D-PERF-1)

**Status: ADOPTED 2026-10-27, as proposed** (task 8fa16659; recorded in
[`docs/nsh-contracts.md` §8.2](nsh-contracts.md#82-d-perf-1-nsh-frame-budgets-adopted-2026-10-27)).
The facts, the proposed wording and the alternatives below are kept exactly as first written. The
original status line was:

~~**Status: open, and the owner's decision. This plan adopts no value.**~~

The facts:

- The GPUI guard is `large_disclosed_turn_virtualizes_children_and_streams_within_budget`
  (`crates/riwork-desktop/src/chat_view/display_tests.rs:346`; assertion at :421-424). It asserts
  that the median of three drawn frames is **< 25 ms** in an unoptimized test build. It also
  asserts < 60 ms medians for Expand all and row toggle, fewer than 100 materialized rows,
  < 1.5 ms per event, and zero token reprojection.
- It **fails on GPUI itself**:
  - 34.0 ms (baseline) and 40.5 ms (current) on 2026-10-22, with the host under load (1-minute
    load average 29.9) (`PERFORMANCE_RECOVERY_REPORT.txt:110-121`);
  - 33.6 ms on the unpatched `4ec85e8`;
  - 26.7 / 27.6 / 29.5 ms pre-anchor, and current failing once at 36.0 ms (:245-252, :310-319);
  - 35.2 ms on recheck (:414-419).
- Parity records this as disagreement #10, and contracts §5 says the NSH target is "set
  explicitly".
- No Phase-1 surface renders a transcript, so the 25 ms guard is **not a Phase-1 gate**. Phase 1
  still affects it in two ways:
  - P1 writes the first NSH perf harness. It must neither copy the 25 ms constant into a new
    wall-clock assertion nor invent a different one.
  - P1b's removal of `crates/riwork-desktop` deletes the only executable copy of the guard. It
    stays runnable from the `ea0f2f5` archive (§3.4) until the chat phase replaces it.
- Phase 1's own references are both report-only today:
  - the busy-workspace frame (`measure_a_busy_workspace_frame`, `#[ignore]`, no assertion);
  - idle CPU (0.65 % of a core, a single contended sample).
  
  Phase 1 keeps them report-only (P1-PERF-01) until this decision exists.

**Proposed NSH wording, for decision:**

> **NSH frame budgets.**
>
> 1. *Structural guards are hard and machine-independent.* They are asserted in every test build:
>    fewer than 100 rows or selection bodies materialized per frame for the 2,200-call disclosed
>    turn; no reprojection for token deltas; under 1.5 ms per streamed event at the bridge;
>    exactly one projection per layout gesture; zero deliveries while idle.
> 2. *Wall-clock budgets are asserted only on release builds* of the Rust host, the UI set and the
>    JS bundle. They are measured with NSH's settled surface-frame monitor, as the median of at
>    least 5 runs, on a quiet host: no cargo, rustc or nsh process at start, and a 1-minute load
>    average below half the core count, recorded with the run.
> 3. *Targets.* For the 2,200-call disclosed turn at 1220×780, Native theme: median ≤ 16.7 ms and
>    p95 ≤ 25 ms. Each must also be no worse than the matched GPUI release measurement of the same
>    fixture, viewport and theme on the same host. For the Phase-1 busy workspace (24 tabs,
>    120 projects): median ≤ 16.7 ms and p90 ≤ 25 ms, with the same matched comparison.
> 4. *Debug timings are reported, never asserted.*
> 5. *The GPUI 25 ms Debug guard is retired from the NSH acceptance set.* It stays a GPUI
>    regression test for as long as the GPUI desktop builds.

Alternatives, if the owner prefers:

- **(A)** Keep "median < 25 ms" unchanged, but release-only. This is the smallest change, but it
  is looser than one 60 Hz frame.
- **(B)** Relative only: "≤ the matched GPUI release median + 10 %". This is robust to hardware,
  but it lets NSH inherit GPUI's slowness.

The recommendation is the wording above (an absolute release target plus the matched comparison),
because the host spike already saw no retained NSH surface frame above 16.67 ms under streaming
(`spikes/host-multiwindow/WORKER_REPORT.md`).

**Decision (2026-10-27):** the owner adopted the proposed wording as written, rules 1–5. The
alternatives (A) and (B) were not taken. Phase 1 perf stays report-only until the chat phase:

- P1-PERF-01 records the busy-workspace frame and idle CPU against the rule 3 targets, and fails
  no Phase 1 check.
- The rule 1 structural guards that exist in Phase 1 (P1-PERF-02, P1-POLL-02) are hard.

---

## 8. Deferred items

Each target is a later port surface. The P1 behavior follows R-DEFER. "Hook" names what P1 must
leave in place.

| Target | Rows | P1 behavior | Hook P1 must leave |
|---|---|---|---|
| **D-TERM**: `riwork.terminal`, gated by the teardown protocol (§3.7, `nsh-p2-teardown`) | 016, 051, 055, 109, 112, 150–153, 188, 244, 248, 300, 311, 312; terminal halves of 022, 028, 041, 052 (Metal scale), 067, 071, 095, 100, 103, 132, 136, 143, 146, 149, 157 (claims), 298 (terminal focus), 313 (c, d, f), 320 (attach retry) | Shell and remote tabs are placeholders. No tmux client, no claims: two windows can list the same shell (a recorded deviation). No freezing. | `visible` flow and instance identity (P1-DRAG-04). An overlay strategy chosen for menus (prefer `Menu.popup`, which draws above NSViews). Terminal key-equivalent order in the host (§2.4). |
| **D-CHAT**: chat views, orchestrators, chat ownership | 053, 054, 056–060, 080, 102, 184, 208, 210–220, 225–231, 243, 322; chat halves of 028 (chat claims), 041, 067, 069, 074, 077, 120, 121, 123, 209, 242, 259 | Chat tabs are placeholders. Chat chords are inert. | `chat-pending:` mapping (P1-LAY-08); `switchProject` stays open for `return_own_orchestrators` (229). |
| **D-SESS**: sessions, harnesses, workers, titles | 101, 164, 165, 168–170, 173–176, 204–207, 321; halves of 030, 110, 120, 121, 242, 259, 315 | Real `Sessions` read side; fakes in tests. Shell titles are the shared entry's title or the key. | Before any non-fixture use, windows must report shown keys (SHELL-170) or the orphan reaper may end tabbed sessions. |
| **D-PANEL**: panels, settings UI, modals, files, worktrees | 009, 162, 163, 166, 167, 233–237, 266, 268, 316, 323, 324; halves of 030, 033, 095, 241, 242, 298, 313 (a), 315, 317, 318, 326 | Panel tabs are placeholders; ⌘F is inert. | Panel tab identity and routing (P1-TAB-02). |
| **D-DASH**: dashboard contents and data | 043, 046–050, 061, 063–065, 068, 076, 172; halves of 071, 074, 320 | The global window's Dashboard body is a placeholder. | `Dashboard` global-window state (P1-GLB-03). |
| **D-REMOTE** | 161, 189–200, 325; halves of 032, 100 | Remote keys and `remote:` tabs round-trip unchanged (P1-LAY-01/-03). | — |
| **D-HANDOFF** | 201–203 | The menu row is present and disabled. | — |
| **D-USAGE** | 221–224; halves of 110, 259, 315 | No-data status items. | — |
| **D-SVC**: schedules, wakes, notifications, worker watch, drafts | 232, 238, 269–273, 327; halves of 008, 031 | Not started. | Draft flush hook in the reload choreography (P1-RLD-02). |
| **D-PREVIEW** | 113, 328 | `app_preview:` tabs round-trip and are placeholders. | — |
| **D-GHOSTTY**: Ghostty config parsing and the `appearance.json` publisher (needs libghostty) | 280, 282, 283, 286, 294, 295, 302; half of 291 | As D-THEME-1; no `appearance.json` writes (P1-THM-06). | `Appearance` carries the override choice (P1-THM-03). |
| **D-UPSTREAM**: NSH API gaps | 125 (middle-click), 308–310 (SF Symbols); accepted with recorded deviation in P1: 107 (drop zones), 104 (Ctrl-Tab, manual check), 084 (fixed children during live resize beyond one absorber) | Vector glyph fallback instead of SF Symbols. | Gap register entries in `docs/nsh-parity.md`. |

`DEFERRED_COMMANDS` must equal the command ids of the deferred chords and menu items:

- agent terminals (⌘⇧C, ⌘⇧L, ⌘⇧G, ⌘⇧U);
- new chat (⌘⌥⇧C);
- project orchestrator (⌘⌥O);
- create project (⌘N);
- search (⌘F);
- chat-scoped keys (⌘., ⌃⌥D, ⌘⇧V, ⌘⌥→, ⌘⌥←, chat ⌘F / ⌘G / ⌘⇧G);
- the View items `toggleTranscriptMode`, `expandTranscript`, `collapseTranscript`, `newChat`,
  `appPreview`, `sessions`;
- the pane menu's Existing sessions / Existing Claude chats / Existing Muse chats, New tab ▸
  {Codex, Claude, Grok, Muse, Global orchestrator, Project orchestrator, New chat…,
  App preview…}, and Hand off….

`Shell` (⌘T) is **not** deferred: it creates through `Sessions` (a fake in tests).

---

## 9. Contract-conformance review checklist

Apply each list to its branch at review time. Record every check as pass, fail or n/a, with the
command or file:line that shows it.

### 9.1 All three branches

| ID | Check | How |
|---|---|---|
| C-0-01 | Based on `nsh` at or after `ea0f2f5`. `docs/nsh-contracts.md` is unchanged, except a §7 change-log entry with a `schema` bump and sign-off from both sides. New `riwork/native` function tables (§1.10) are added there before first use. | `git diff ea0f2f5...<branch> -- docs/nsh-contracts.md` |
| C-0-02 | No RIWORK_HOME format change. serde types of `layouts.rs`, `settings.rs`, `global_window.rs`, `project_tabs.rs`, `status_bar.rs` and `runtime.rs` are untouched; logic moves keep names and tests. | `git diff ea0f2f5...<branch> -- crates/riwork-core crates/riwork-engine \| rg '^[+-].*(#\[serde\|pub struct\|pub enum\|SCHEMA)'` |
| C-0-03 | NSH pins exactly `=0.3.0` (core, base, engine, platform-macos). No GPUI crate in the new crates' trees. | `cargo tree -p riwork-nsh-native -e normal \| rg -i gpui` → empty; same for the host binary |
| C-0-04 | Tests are isolated: no test resolves the real home, and none mutates `RIWORK_HOME` in-process. Every fixture run uses §3.3's env. | `rg -n 'riwork_home\(\)\|open_default\(\)\|set_var\("RIWORK_HOME"\|home_dir\(' <new test files>` |
| C-0-05 | Verification report: exact commands, exit codes, JUnit path, environment. No live windows. Cua only read-only and only on the fixture pid (§3.3). | read the report |
| C-0-06 | No wall-clock threshold asserted (D-PERF-1 pending). The structural guards exist. | `rg -n 'assert!\(.*(ms\|elapsed)' <new tests>`; review |
| C-0-07 | §4.3 ledger: no listed test deleted without its successor. | `git diff --name-status ea0f2f5...<branch> -- crates/riwork-desktop`, cross-checked against §4.3 |
| C-0-08 | R-DEFER holds: deferred features are present and disabled or inert, the write allow-list holds, no tmux server starts. | §3.3 run output |

### 9.2 P1a: `riwork/native` (nsh-p1-native)

| ID | Check | Contract |
|---|---|---|
| C-A-01 | Crate name, `#[native_module("riwork/native")]`, generated `native.d.ts`, naming rules; DTOs in the crate; ids are strings except `PaneId` (number). | §1.1 |
| C-A-02 | Every sync `#[module]` function is pure in-memory work. Every async one only hops to `spawn_blocking`. None blocks the engine thread or calls riwork-core on the main thread. | §1.2, §5 threads |
| C-A-03 | Errors are exactly `"<code>: <message>"` with `refused`, `broken`, `not_found`, `invalid`, `conflict`, `unavailable`, `io`. | §1.2 |
| C-A-04 | Subscriptions: a `#[class] Subscription` with `close()`; owner drop ends the task; snapshot first; per-topic `rev`; bounded queue of 64 with a single `resync` snapshot on overflow; ≤ 1 delivery per topic per engine turn; streaming topics ≤ 1 per 16 ms (trailing); UTF-16 offsets on the bridge; injectable clock. | §1.3 |
| C-A-05 | One hub per home per topic per process. One FSEvents watcher per home per process (reusing `subshells.rs:240-300`'s approach). Polling only inside process services, at intervals no shorter than today's, emitting only on change. | §1.3 table |
| C-A-06 | Backend traits (`StoreBackend`, `SessionsBackend`, …) with production and fake implementations. Bridge tests take a TempDir home. `data_dir = RIWORK_HOME/nsh`. No `nsh/storage`. | §1.4 |
| C-A-07 | `Store` functions match §1.5 names and wrapped calls. A mutation resolves after the write and reaches every subscriber. | §1.5 |
| C-A-08 | `Sessions` / `Tabs` match §1.7. `Tabs.subscribe` delivers `{epoch, revision, entries}`. JS ids are validated with `project_tabs::valid_key` / UUID parsing. | §1.7, §5 security |
| C-A-09 | The `test-support` feature (fakes, `ManualClock`, fixture generator, `RIWORK_NSH_FAKE_SESSIONS`) is off by default and absent from a release `nsh build` of `$APP`. | §1.4; R-DEFER |
| C-A-10 | `Settings` / `Appearance` function tables were added to contracts §1.10 under a schema bump (C-0-01). `Appearance` has no libghostty dependency (D-THEME-1). | §1.10 |
| C-A-11 | No `appearance.json` writer is reachable. | P1-THM-06 |

### 9.3 P1b: AppKit host (nsh-p1-host)

| ID | Check | Contract |
|---|---|---|
| C-B-01 | One `riwork` binary; `cli::run_cli` first; the GUI starts only for the SHELL-001 forms; cli-diff passes. | §0, SHELL-001 |
| C-B-02 | `RIWORK_HOME/run/desktop.lock` plus forwarding over `run/desktop.sock`. Only a host-started reload replacement may overlap, until it confirms. Forward output per D-PROC-1. | §0 |
| C-B-03 | `RIWORK_HOME` is resolved once per process (`riwork_core::paths::riwork_home()`) and is never taken from JS. | §0 |
| C-B-04 | `set_ui_sets([nsh_base::registry(), riwork_native::registry()])` runs once, before the first AppView; one shared component registry. If P1b registers a `riwork.terminal` stand-in, it uses exactly the §3.2–§3.4 names. | §0, §3.1 |
| C-B-05 | One `HostChannel` per window. `WindowId` is UUID v4, stable for the window's life including project switches. TSX imports `host` from `"nsh"`. | §2.1 |
| C-B-06 | `host.params` is exactly the schema-1 shape (field names, `kind`, `projectId: null` and path `<home>/global-window` for the global window, `restore` only when restoring, `cascade`, `appearance`, `build`). | §2.2 |
| C-B-07 | `nsh.json` grants exactly `"host": ["window.*", "app.*", "system.*"]`, with no fs, process or net capability. Every served name is from §2.3. Unserved v1 names reject at once with `unavailable: …` (no 30 s timeout). `tab.transfer` / `tab.receive` are not served. Handlers return at once and answer later. | §2.1, §2.3, §5 |
| C-B-08 | The host owns the app menu (RiWork / Edit / View only). TSX never calls `Menu.setApplicationMenu`. ⌘W never closes a window. Edit uses responder selectors with `command` forwarding. A first-responder terminal gets ⌘= / ⌘- / ⌘0 first. | §2.4 |
| C-B-09 | `window.state` fields and change-only emission; `window.willClose` reasons; `project.open`, `route`, `global.show`, `appearance` delivered to the right window. | §2.4 |
| C-B-10 | Runtime registration, heartbeat and reload choreography are host-internal. `RuntimeWindow` is built in Rust from `LayoutSession` and the frame/mode. Every `LayoutSession` (and the draft hook) is flushed from Rust before the replacement launch and on quit. No JS round trip. | §2.5, §4.5 |
| C-B-11 | Window chrome at creation: transparent title bar, full-size content, traffic lights (10, 8), minimum 640×400, title-bar double-click per the system preference. Quit when the last window closes. | §2.5 |
| C-B-12 | GPUI modules deleted only on this branch, and only per §4.3 (C-0-07). | task ddba58a0 |

### 9.4 P1c: workspace TSX and layout mapping (nsh-p1-workspace)

| ID | Check | Contract |
|---|---|---|
| C-C-01 | `ProjectLayout` is the source of truth. TSX passes the session's projection as the controlled `layout` and hands **every** `onLayoutChange` to `LayoutSession.applyDock`. It never installs a reported layout, never edits the layout itself, and never uses DockArea ref methods for app edits (those are session methods running riwork-core's placement code). | §4.2, §4.4 |
| C-C-02 | Projection rules 1–7: center only; PaneId = group NodeId; `empty:<paneId>` sentinels (also for panes holding skipped tabs); flattened same-axis splits without ids; sizes from `pane_extents` with 5 pt dividers and `null` for the single absorber; panel options `{closable: false, zoomable: false}` plus `visible: true` for sentinels; JS-unsafe ids remapped. | §4.3 |
| C-C-03 | Gesture classification per the §4.4 table, with the fallback logging `layout.fallback`. `normalize` and re-projection after each apply. Window resize through `setExtent` → `preserve_locked`. | §4.4 |
| C-C-04 | `openSession` order: `restore` → `LayoutStore::load` → the GPUI default (an unreadable entry is never overwritten; its error becomes the notice) → reconcile. `switchProject` uses `carry_locked_regions_from`. | §4.5 |
| C-C-05 | Saves trail by 250 ms on the blocking pool through `save_chat_state` / `save_tab_layout` (as GPUI chooses). Synchronous Rust flush at the five flush points. Window size captured per P10 from host-reported frames. | §4.5 |
| C-C-06 | Panel ids never change during a session (`chat-pending:<uuid>` mapping). `LayoutSession.panel()` exists. Depth and pane limits hold. | §4.6 |
| C-C-07 | DockArea gets `compact={false}` (§2.3 finding). | NSH 0.3 `dock_area.rs:105-106,264` |
| C-C-08 | DockArea's area-wide `locked` is never used for RiWork pane locks (§2.3 finding). | §4.2; NSH 0.3 `dock_area.rs:260` |
| C-C-09 | Focus mode keeps DockArea mounted and changes presentation only. DockArea's own zoom is not used (`zoomable: false`). | §4.3 rule 6; P1-DRAG-04 |
| C-C-10 | §4.7 oracle tests exist for every listed fixture, at the stated tolerances. One test exists per §4.4 gesture row. | §4.7 |
| C-C-11 | The app closes its own window through `host.call("window.close")`, so the Rust flush is ordered before the close, rather than through `Window.close`. If it uses `Window.close`, the host's close handler must flush (C-B-10), and the reviewer checks that path instead. | §2.3 `window.close`, §2.3 note on built-ins |
| C-C-12 | `$APP/tests/hostFake.ts` result shapes equal the §2.3 tables. No TSX polling (P1-POLL-03). No durable state outside `riwork/native`. | §2.3, §1.3, §1.4 |
| C-C-13 | Only `LayoutStore` writes `layouts.json` (P1-LAY-10). | §4.2 |

---

## 10. Row disposition index

Every P-SHELL row, in runs of consecutive rows with the same disposition. "Partial" lists the
Phase-1 item, then, after `/`, the deferred targets of [§8](#8-deferred-items). The index was
checked mechanically to cover SHELL-001 to SHELL-329 exactly once (159 Phase 1, 40 Partial,
130 Deferred).

Items with no P-SHELL row are not listed here. They come from the contracts or are the P1 side of a deferred row: P1-NAT-01–05, P1-LAY-02, P1-PERF-02, P1-POLL-03, P1-THM-06 (for 294/295) and P1-THM-07 (for 280).

| Rows | Disposition | Acceptance item / deferred target |
|---|---|---|
| SHELL-001 | Phase 1 | P1-PROC-01 |
| SHELL-002 | Phase 1 | P1-PROC-02 |
| SHELL-003 | Phase 1 | P1-PROC-03 |
| SHELL-004–005 | Phase 1 | P1-PROC-04 |
| SHELL-006 | Phase 1 | P1-PROC-05 |
| SHELL-007 | Phase 1 | P1-PROC-06 |
| SHELL-008 | Partial | P1-PROC-07 / D-SVC |
| SHELL-009 | Deferred | D-PANEL |
| SHELL-010–013 | Phase 1 | P1-RLD-01 |
| SHELL-014 | Phase 1 | P1-RLD-02 |
| SHELL-015 | Phase 1 | P1-RLD-03 |
| SHELL-016 | Deferred | D-TERM |
| SHELL-017 | Phase 1 | P1-WIN-01 |
| SHELL-018 | Phase 1 | P1-WIN-02 |
| SHELL-019–020 | Phase 1 | P1-WIN-03 |
| SHELL-021 | Phase 1 | P1-WIN-04 |
| SHELL-022 | Partial | P1-WIN-04 / D-TERM |
| SHELL-023 | Phase 1 | P1-WIN-05 |
| SHELL-024–025 | Phase 1 | P1-WIN-06 |
| SHELL-026 | Phase 1 | P1-PANE-12 |
| SHELL-027 | Phase 1 | P1-WIN-07 |
| SHELL-028 | Partial | P1-WIN-08 / D-TERM, D-CHAT |
| SHELL-029 | Phase 1 | P1-WIN-08 |
| SHELL-030 | Partial | P1-WIN-09 / D-SESS, D-PANEL |
| SHELL-031 | Partial | P1-WIN-09, P1-MENU-04 / D-SVC |
| SHELL-032 | Partial | P1-WIN-10 / D-REMOTE |
| SHELL-033 | Partial | P1-WIN-10 / D-PANEL |
| SHELL-034 | Phase 1 | P1-GLB-01 |
| SHELL-035 | Phase 1 | P1-GLB-02 |
| SHELL-036–037 | Phase 1 | P1-GLB-03 |
| SHELL-038 | Phase 1 | P1-GLB-04 |
| SHELL-039 | Phase 1 | P1-GLB-05 |
| SHELL-040 | Phase 1 | P1-GLB-06 |
| SHELL-041 | Partial | P1-GLB-06 / D-CHAT, D-TERM |
| SHELL-042 | Phase 1 | P1-GLB-07 |
| SHELL-043 | Deferred | D-DASH |
| SHELL-044–045 | Phase 1 | P1-GLB-08 |
| SHELL-046–050 | Deferred | D-DASH |
| SHELL-051 | Deferred | D-TERM |
| SHELL-052 | Partial | P1-GLB-02, P1-MENU-04 / D-TERM |
| SHELL-053–054 | Deferred | D-CHAT |
| SHELL-055 | Deferred | D-TERM |
| SHELL-056–060 | Deferred | D-CHAT |
| SHELL-061 | Deferred | D-DASH |
| SHELL-062 | Phase 1 | P1-GLB-09 |
| SHELL-063–065 | Deferred | D-DASH |
| SHELL-066 | Phase 1 | P1-GLB-10 |
| SHELL-067 | Partial | P1-GLB-03 / D-CHAT, D-TERM |
| SHELL-068 | Deferred | D-DASH |
| SHELL-069 | Partial | P1-STAT-05 / D-CHAT |
| SHELL-070 | Phase 1 | P1-STAT-05 |
| SHELL-071 | Partial | P1-STAT-05 / D-TERM, D-DASH |
| SHELL-072–073 | Phase 1 | P1-STAT-05 |
| SHELL-074 | Partial | P1-STAT-05 / D-DASH, D-CHAT |
| SHELL-075 | Phase 1 | P1-STAT-05 |
| SHELL-076 | Deferred | D-DASH |
| SHELL-077 | Partial | P1-TAB-02 / D-CHAT |
| SHELL-078 | Phase 1 | P1-LAY-05 |
| SHELL-079 | Phase 1 | P1-MENU-03 |
| SHELL-080 | Deferred | D-CHAT |
| SHELL-081 | Phase 1 | P1-PANE-01 |
| SHELL-082 | Phase 1 | P1-PANE-02 |
| SHELL-083 | Phase 1 | P1-PANE-03 |
| SHELL-084 | Phase 1 | P1-PANE-04 |
| SHELL-085–086 | Phase 1 | P1-PANE-05 |
| SHELL-087–088 | Phase 1 | P1-PANE-06 |
| SHELL-089–090 | Phase 1 | P1-PANE-07 |
| SHELL-091 | Phase 1 | P1-PANE-08 |
| SHELL-092 | Phase 1 | P1-PANE-09 |
| SHELL-093 | Phase 1 | P1-PANE-10 |
| SHELL-094 | Phase 1 | P1-PANE-11 |
| SHELL-095 | Partial | P1-PANE-11 / D-TERM, D-PANEL |
| SHELL-096–097 | Phase 1 | P1-PANE-12 |
| SHELL-098 | Phase 1 | P1-PERF-01 |
| SHELL-099 | Phase 1 | P1-TAB-01 |
| SHELL-100 | Partial | P1-TAB-02 / D-REMOTE, D-TERM |
| SHELL-101 | Deferred | D-SESS |
| SHELL-102 | Deferred | D-CHAT |
| SHELL-103 | Partial | P1-TAB-03 / D-TERM |
| SHELL-104 | Phase 1 | P1-KEYS-02 |
| SHELL-105 | Phase 1 | P1-TAB-04 |
| SHELL-106 | Phase 1 | P1-DRAG-01 |
| SHELL-107 | Phase 1 | P1-DRAG-02 |
| SHELL-108 | Phase 1 | P1-DRAG-03 |
| SHELL-109 | Deferred | D-TERM |
| SHELL-110 | Partial | P1-TAB-02 / D-SESS, D-USAGE |
| SHELL-111 | Phase 1 | P1-TAB-02 |
| SHELL-112 | Deferred | D-TERM |
| SHELL-113 | Deferred | D-PREVIEW |
| SHELL-114–115 | Phase 1 | P1-STRIP-01 |
| SHELL-116–117 | Phase 1 | P1-KEYS-02 |
| SHELL-118–119 | Phase 1 | P1-STRIP-02 |
| SHELL-120–121 | Partial | P1-STRIP-02 / D-CHAT, D-SESS |
| SHELL-122 | Phase 1 | P1-STRIP-02 |
| SHELL-123 | Partial | P1-STRIP-02 / D-CHAT |
| SHELL-124 | Phase 1 | P1-STRIP-02 |
| SHELL-125 | Partial | P1-STRIP-03 / D-UPSTREAM |
| SHELL-126 | Phase 1 | P1-STRIP-03 |
| SHELL-127–128 | Phase 1 | P1-STRIP-02 |
| SHELL-129–130 | Phase 1 | P1-STRIP-04 |
| SHELL-131 | Phase 1 | P1-WIN-06 |
| SHELL-132 | Partial | P1-DRAG-01 / D-TERM |
| SHELL-133–134 | Phase 1 | P1-DRAG-01 |
| SHELL-135 | Phase 1 | P1-STRIP-04 |
| SHELL-136 | Partial | P1-STRIP-04 / D-TERM |
| SHELL-137–138 | Phase 1 | P1-STRIP-04 |
| SHELL-139–142 | Phase 1 | P1-STRIP-03 |
| SHELL-143 | Partial | P1-STRIP-03 / D-TERM |
| SHELL-144–145 | Phase 1 | P1-TAB-04 |
| SHELL-146 | Partial | P1-TAB-04 / D-TERM |
| SHELL-147–148 | Phase 1 | P1-TAB-04 |
| SHELL-149 | Partial | P1-DRAG-04 / D-TERM |
| SHELL-150–153 | Deferred | D-TERM |
| SHELL-154 | Phase 1 | P1-LAY-01, P1-LAY-03, P1-LAY-10 |
| SHELL-155 | Phase 1 | P1-LAY-07 |
| SHELL-156 | Phase 1 | P1-LAY-04 |
| SHELL-157 | Partial | P1-LAY-06 / D-TERM |
| SHELL-158 | Phase 1 | P1-LAY-06 |
| SHELL-159 | Phase 1 | P1-RLD-04 |
| SHELL-160 | Phase 1 | P1-LAY-09 |
| SHELL-161 | Deferred | D-REMOTE |
| SHELL-162–163 | Deferred | D-PANEL |
| SHELL-164–165 | Deferred | D-SESS |
| SHELL-166–167 | Deferred | D-PANEL |
| SHELL-168–170 | Deferred | D-SESS |
| SHELL-171 | Phase 1 | P1-POLL-01 |
| SHELL-172 | Deferred | D-DASH |
| SHELL-173–176 | Deferred | D-SESS |
| SHELL-177–178 | Phase 1 | P1-TAB-05 |
| SHELL-179–183 | Phase 1 | P1-STRIP-05 |
| SHELL-184 | Deferred | D-CHAT |
| SHELL-185 | Phase 1 | P1-DRAG-01 |
| SHELL-186–187 | Phase 1 | P1-STRIP-05 |
| SHELL-188 | Deferred | D-TERM |
| SHELL-189–200 | Deferred | D-REMOTE |
| SHELL-201–203 | Deferred | D-HANDOFF |
| SHELL-204–207 | Deferred | D-SESS |
| SHELL-208 | Deferred | D-CHAT |
| SHELL-209 | Partial | P1-LAY-08 / D-CHAT |
| SHELL-210–220 | Deferred | D-CHAT |
| SHELL-221–224 | Deferred | D-USAGE |
| SHELL-225–231 | Deferred | D-CHAT |
| SHELL-232 | Deferred | D-SVC |
| SHELL-233–237 | Deferred | D-PANEL |
| SHELL-238 | Deferred | D-SVC |
| SHELL-239 | Phase 1 | P1-KEYS-03 |
| SHELL-240 | Phase 1 | P1-KEYS-01 |
| SHELL-241 | Partial | P1-KEYS-03 / D-PANEL |
| SHELL-242 | Partial | P1-KEYS-03 / D-PANEL, D-SESS, D-CHAT |
| SHELL-243 | Deferred | D-CHAT |
| SHELL-244 | Deferred | D-TERM |
| SHELL-245–247 | Phase 1 | P1-KEYS-04 |
| SHELL-248 | Deferred | D-TERM |
| SHELL-249 | Phase 1 | P1-KEYS-05 |
| SHELL-250 | Phase 1 | P1-MENU-01 |
| SHELL-251 | Phase 1 | P1-MENU-02 |
| SHELL-252 | Phase 1 | P1-MENU-03 |
| SHELL-253–258 | Phase 1 | P1-MENU-04 |
| SHELL-259 | Partial | P1-STAT-02 / D-SESS, D-USAGE, D-CHAT |
| SHELL-260 | Phase 1 | P1-STAT-03 |
| SHELL-261 | Phase 1 | P1-STAT-04 |
| SHELL-262–265 | Phase 1 | P1-STAT-01 |
| SHELL-266 | Deferred | D-PANEL |
| SHELL-267 | Phase 1 | P1-STAT-01 |
| SHELL-268 | Deferred | D-PANEL |
| SHELL-269–273 | Deferred | D-SVC |
| SHELL-274 | Phase 1 | P1-THM-01 |
| SHELL-275–279 | Phase 1 | P1-THM-02 |
| SHELL-280 | Deferred | D-GHOSTTY |
| SHELL-281 | Phase 1 | P1-THM-03 |
| SHELL-282–283 | Deferred | D-GHOSTTY |
| SHELL-284–285 | Phase 1 | P1-THM-02 |
| SHELL-286 | Deferred | D-GHOSTTY |
| SHELL-287 | Phase 1 | P1-THM-04 |
| SHELL-288 | Phase 1 | P1-WIN-06 |
| SHELL-289 | Phase 1 | P1-THM-05 |
| SHELL-290 | Phase 1 | P1-THM-04 |
| SHELL-291 | Partial | P1-THM-05 / D-GHOSTTY |
| SHELL-292–293 | Phase 1 | P1-THM-05 |
| SHELL-294–295 | Deferred | D-GHOSTTY |
| SHELL-296 | Phase 1 | P1-THM-05 |
| SHELL-297 | Phase 1 | P1-TXT-01 |
| SHELL-298 | Partial | P1-TXT-02 / D-TERM, D-PANEL |
| SHELL-299 | Phase 1 | P1-TXT-02 |
| SHELL-300 | Deferred | D-TERM |
| SHELL-301 | Phase 1 | P1-TXT-01 |
| SHELL-302 | Deferred | D-GHOSTTY |
| SHELL-303–307 | Phase 1 | P1-TXT-03 |
| SHELL-308–310 | Deferred | D-UPSTREAM |
| SHELL-311–312 | Deferred | D-TERM |
| SHELL-313 | Partial | P1-THM-05, P1-WIN-04 / D-TERM, D-PANEL |
| SHELL-314 | Phase 1 | P1-POLL-01 |
| SHELL-315 | Partial | P1-POLL-01 / D-SESS, D-PANEL, D-USAGE |
| SHELL-316 | Deferred | D-PANEL |
| SHELL-317–318 | Partial | P1-POLL-02 / D-PANEL |
| SHELL-319 | Phase 1 | P1-POLL-02 |
| SHELL-320 | Partial | P1-PANE-04, P1-WIN-04 / D-TERM, D-DASH |
| SHELL-321 | Deferred | D-SESS |
| SHELL-322 | Deferred | D-CHAT |
| SHELL-323–324 | Deferred | D-PANEL |
| SHELL-325 | Deferred | D-REMOTE |
| SHELL-326 | Partial | P1-TAB-04 / D-PANEL |
| SHELL-327 | Deferred | D-SVC |
| SHELL-328 | Deferred | D-PREVIEW |
| SHELL-329 | Phase 1 | P1-WIN-07 |

---

## 11. Verification record

- Inputs read on this branch at `ea0f2f5`:
  - `docs/nsh-parity.md`: P-SHELL rows 001–329, port gates, perf baselines, doc/code
    disagreements, gap register;
  - `docs/nsh-contracts.md` §0–§7;
  - `docs/nsh-spec-terminal-layout.md` rows L1–L11, P1–P10, G1–G4;
  - `docs/nsh-toolchain.md`;
  - `spikes/host-multiwindow/` (README, WORKER_REPORT, `App.test.tsx`);
  - `PERFORMANCE_RECOVERY_REPORT.txt:110-122,245-253,310-319,414-419`;
  - `crates/riwork-desktop/src/chat_view/display_tests.rs:405-430`.
- Sibling tasks read with `riwork task show --json` (5c4cb942, ddba58a0, 40e6ac8b, 5f5073a7). The
  branches `nsh-p1-native`, `nsh-p1-host` and `nsh-p1-workspace` had no commits beyond `ea0f2f5`.
- NSH 0.3.0 sources read in the local cargo registry:
  - `nsh-base-0.3.0/src/dock/dock_area.rs` (compact width 600 pt at :105-106; props at :255-349;
    `locked: Prop<bool>` at :260);
  - `nsh-base-0.3.0/tests/base/dock.rs`;
  - `nsh-dev-0.3.0/src/protocol.rs` (test step names; `Bounds` takes a node id);
  - `nsh-testing-0.3.0/src/driver.rs` and its `tests/testing/{host,embedding}`.
- RiWork code checked for claims made here:
  - the tmux socket is named from the home (`crates/riwork-engine/src/sessions.rs:212-229`);
  - the Ghostty parsing dependency (`crates/riwork-desktop/src/theme.rs:5-6`, `mod native` at :916-1045);
  - env names `RIWORK_HOME`, `RIWORK_RUNTIME_DIR`, `RIWORK_RESTORE_TICKET`, `RIWORK_APPEARANCE`;
  - the cited GPUI test names exist (`main.rs:14434,14532,15688,17650`; `layouts.rs:2836,3376,3408,3458`);
  - `measure_a_busy_workspace_frame` is `#[ignore]` and has no assertion.
- Row coverage: a script over the disposition data behind §10 confirmed every SHELL-001…329
  appears exactly once (159 / 40 / 130). §2.1's counts and §4.3's ledger come from the same data.
  The ledger lists the `crates/riwork-desktop` tests cited in the Tests column of non-deferred
  rows.
- Cua tool argument names come from the cua-driver tool schemas (`list_windows`,
  `get_window_state`, `verify_state`). No Cua call was made.
- Nothing was built, run or tested. No window was opened. No merge or push. Task status is left to
  the orchestrator.
