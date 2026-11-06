# Temporary universal polling mitigation — source-only handoff

Project `39832c2e-23a5-476d-aa8f-5ff34a02d314`, task
`6013a3ff-5fd4-4a9a-86b3-f566b636db07`. Separate change atop foundation
`ea05f94d566cdd952b778a2edafcb13f42a1a65f` in the assigned foundation worktree.

## Scope and source evidence

Read the independent review's mitigation and test recommendations at
`/Users/dominik/orca/projects/riWork-review-manu-20261005/GPUI_KIT_REVIEW_REPORT.md:193–213`.
The patch follows its universal polling recommendation, replacing the earlier
version-gate/graceful-teardown draft. That draft is preserved solely in the
untracked `GPUI_KIT_WATCH_GATE_ALTERNATIVE.patch`; it is not part of this commit.
The untracked attachment audit is also preserved separately.

- `src/sessions.rs:5024–5035`: `SessionManager::watch` unconditionally returns
  `None`, before constructing any command. Both output waiting and desktop
  `watch_shell` use this seam. There is no version probe, cache, runtime option,
  attach attempt, failure/retry path or installed-client version dependency.
- `src/sessions/watch.rs:133–139`: direct `PaneWatch::start` also returns `None`.
  Its old spawn/attach and notice-reader startup were removed. The type, legacy
  wait logic and existing Drop remain for compatibility, with no new teardown,
  signalling or cleanup. Private fields prevent production construction; the
  sole remaining struct literal is inside legacy `cfg(test)` tests.
- Caller search across `src`: the long-read `Waiter` (`sessions.rs:4994–5016`)
  and desktop worker (`terminal_links/ui.rs:1062`) are the production users.
  There is no remaining production direct-start caller or alternate watcher
  constructor. Other `"-C"` hits are Git directory flags or Codex argument
  parsing, not tmux control attachments.
- Long reads take the existing `Polling` branch and sleep
  `OUTPUT_POLL.min(remaining)` (80 ms maximum). `poll_output`, capture fallback,
  stable hash computation, styled/viewport/history metadata and the 10-second
  deadline cap are unchanged. Immediate changed output, unchanged-at-deadline
  and capture/error/ended-shell paths retain their existing logic.
- Desktop worker `terminal_links/ui.rs:1062–1091` takes its existing None branch:
  no Attached event, Changed every `HOVER_LIFETIME` (600 ms), receiver-close
  exit, no reattach. `attached=false` is initialized at line 670; `current` at
  lines 263–273 retains the 500 ms `VIEW_LIFETIME` cache expiry. Native terminal
  links, input and mouse source were not edited. These are source observations,
  not a GUI or headless UI execution receipt.

## Regression sources (not executed)

Six new tests in `src/sessions/polling_tests.rs`, behind `cfg(all(test, unix))`:

| Test | Assertion |
| --- | --- |
| `shared_desktop_and_direct_watch_entrypoints_start_no_commands` | Repeated shared/desktop starts, invalid ID, direct starts preconfigured with `-C attach-session` and `-CC attach`: zero commands, including probes or rejected attempts. |
| `unchanged_long_read_polls_until_timeout_without_control_attempts` | Actual `read_output` captures repeatedly, returns unchanged hash/screen at the 300 ms deadline, zero control attempts. |
| `immediate_changed_zero_and_short_waits_never_attempt_control` | Changed output takes one capture despite a long requested wait; zero/20 ms unchanged waits also never attach. |
| `changed_output_between_polls_ends_a_long_wait_without_control` | Same then changed fixed captures end a requested 10-second wait, with the new output/hash and exactly two waiting captures. |
| `capture_error_and_exited_shell_after_a_poll_never_attempt_control` | An error after an unchanged capture preserves capture-failure versus exited-shell errors; neither path attaches. |
| `concurrent_long_reads_and_cancelled_desktop_replacements_never_attach` | Two concurrent waiting reads plus repeated desktop seam creation/drop with a closed receiver: no control attempts. Tests ownership at the seam; does not execute the private UI event loop or claim full UI cancellation coverage. |

The fixed fixture uses only `/bin/sh` builtins, logs every argv, rejects control,
attach, destructive, input and resize commands, and never discovers/wraps real
tmux. Only capture and liveness answers are supplied. Its mode-0700 root,
registry and logs remain in the private test TMPDIR, without fixture Drop or
server/process cleanup. The tests also assert zero forbidden attempts, rather
than merely successful fallback after rejection.

Five existing real-tmux regression sources were adjusted coherently in
`src/sessions/tmux_tests.rs`: the three optimization/refusal tests now require
zero control clients/attempts and repeated captures; ended-pane and cleared-
history tests describe polling. Output, dimensions and existing timeout bounds
remain covered. **These fixtures still wrap real tmux and clean up their own
server; they are excluded from the first verification batch and were not run.**
The existing 10 `sessions::output_tests` cover stable hashes, text/styling and
all screen fields, clamped lines, immediate/zero-wait answers, early change,
deadline completion, capture errors and the 10-second cap without tmux.

## Verification status and exact proposed private commands

Source inspection only. `git diff --check` passed. No Cargo, test listing,
formatter, build, test, tmux, GUI, process probe, signal or cleanup was executed
for this patch. Six new tests and ten existing output tests are **planned**, not
passed. Earlier foundation and attachment receipts do not validate this patch.
No test/build child was started in this phase; all source/Git tool calls have
completed. No production state was investigated or changed.

After parent explicitly releases runtime checks and audits isolation, one build
owner may run the following from this worktree. This is a proposal only; none
of it has run. It creates a fresh private target rather than sharing a live
build directory. Offline compilation avoids network/credential use; missing
cached dependencies should stop verification, not trigger installation/fetch.
The child environment does not mutate inherited `RIWORK_HOME`.

```sh
umask 077
watch_verify_root=$(mktemp -d /tmp/rw-wp.XXXXXX)
mkdir -m 700 "$watch_verify_root/state" "$watch_verify_root/run" "$watch_verify_root/tmp" "$watch_verify_root/target"
watch_verify_cargo() {
  env -i HOME=/Users/dominik \
    CARGO_HOME=/Users/dominik/.cargo RUSTUP_HOME=/Users/dominik/.rustup \
    PATH=/Users/dominik/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin \
    LANG=en_US.UTF-8 \
    RIWORK_HOME="$watch_verify_root/state" \
    RIWORK_RUNTIME_DIR="$watch_verify_root/run" \
    TMPDIR="$watch_verify_root/tmp" \
    CARGO_TARGET_DIR="$watch_verify_root/target" \
    ZIG=/Users/dominik/.local/share/riwork/toolchains/zig-0.16.0/zig \
    /Users/dominik/.cargo/bin/cargo "$@"
}
watch_verify_cargo test --offline --locked --bin riwork sessions::polling_tests:: -- --test-threads=1 > "$watch_verify_root/polling-tests.log" 2>&1
watch_verify_cargo test --offline --locked --bin riwork sessions::output_tests:: -- --test-threads=1 > "$watch_verify_root/output-tests.log" 2>&1
```

Collect each exit status and log independently; expected selected counts are
6 and 10. Stop on failure and preserve receipts. No whole suite, legacy watcher
test filter, real-tmux fixture, cleanup or production socket access is proposed.
Full desktop event-loop/GUI proof remains parent-owned and separately held.

## Mitigation limits

Quiet long waits now spawn ordinary captures roughly every 80 ms rather than
using an idle control client; hover uses the existing 600 ms polling latency.
Only a built and rolled-out executable prevents new watcher creation. An old
installed/running app and its existing watchers remain exposed until parent-
owned rollout; this source change neither stops nor drops them.

The reported historical tmux 3.6a crash and the independent review support the
mitigation, but do not prove the cause or trigger of today's simultaneous shell
disappearance. Graceful client shutdown alone would still leave the identify/
disconnect race. Any future re-enable policy must verify the actual running
server contains both upstream fixes (primary `e5a2a25fafb8ee107c230d8acad694f6b635f8bb`,
followup `31c93c48`) and address server replacement between metadata and attach.
No version-based exception is enabled by this patch. Parent retains incident,
isolation, verification and rollout ownership; worker returns idle under hold.
