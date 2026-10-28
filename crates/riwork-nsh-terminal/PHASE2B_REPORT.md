# Phase 2b terminal extras

Task: `9033f7c1-e271-41e1-8da6-e24c01e5a537`

Branch: `nsh-p2b-terminal-extras`

Base: `4e033b87eae5f8175fd2f456207917500f1b345e`

## Result

Local NSH terminals now support bracketed paste, file/image paste and drop,
nested shell tabs, terminal Find, and dedicated Vim file tabs. Hidden clients
follow the desktop release policy. Remote-Mac tabs remain Phase 6.

- Paste and attachments use the engine's per-shell input lock, copy-mode checks,
  shell escaping and bracketed tmux paste. Delivery does not append Return.
  Native pasteboard priority is files, nonempty text, then a lone image. Unsafe
  path batches fail atomically. Text preserves UTF-8; existing tmux paste
  handling converts LF to CR. macOS file URLs can normalize Unicode to NFD,
  with the resulting arguments resolving to the same file identity.
- PNG attachments use private 0600 files in 0700 process/UUID directories under
  `RIWORK_HOME/tmp/terminal-images`. They survive hidden surface release and
  reattach, and are removed on failed delivery or final tab disposal. Startup
  cleanup removes only recognized directories whose owner PID is proven gone.
- New Subshell validates the explicit parent/project, starts in the parent's
  current directory, records lineage, and owns a separate tmux session and
  scrollback. Exiting a nested shell preserves parent and sibling sessions.
- Find has a per-terminal bar, match count, next/previous, Cmd+F, Cmd+G,
  Shift+Cmd+G and Escape. It searches the current Ghostty buffer/scrollback;
  history that predates surface attachment has no additional search guarantee.
- Open File in Vim, the `openEditor` route, and terminal file Edit links open a
  validated project/worktree file in its own editor tab. Reopening reuses the
  live editor and supports line jumps. Existing engine Vim launch and save
  semantics are retained. Files/Preview selected-row actions belong to the
  deferred file browser.
- T2/T3 scheduling keeps three recent hidden surfaces warm for ten minutes,
  grants other hidden clients thirty seconds, releases oldest first with a
  two-per-pass cap, and honors 250 ms, pinned 15 s and frozen 5 s rechecks.
  T4 retains already-open ended local shells; initial restoration still prunes
  stale dead inventory. Explicit Hide and exited-editor pruning still work.

Two lifetime fixes were necessary for acceptance: removed panels release their
owning YNode/controller references while hidden/moved siblings remain retained;
queued focus and completed drops from a newly hidden/frozen terminal cannot
reselect it. Attachment teardown retains the P2a detach-before-child-exit-before-free barrier.

## verification_environment

All application and tmux state used disposable fixture homes. No live RiWork
window or state was inspected or changed. Cua.ai Driver MCP was used only for
read-only observations of the explicit fixture PIDs. Native input probes use
the feature-gated fixture transport and private NSPasteboards, with no global
keyboard/mouse input or system clipboard mutation.

| Item | Value |
| --- | --- |
| Host | macOS 26.2 (25C56), arm64; Xcode 26.0.1 |
| Rust / Cargo | 1.99.1 / 1.99.1 |
| NSH / TypeScript | 0.3.0 (`76fa8a7`) / 7.0.2 |
| tmux / shell | 3.6a / `/bin/zsh` |
| Extras fixture | `/private/tmp/rwnsh-p2-l5tprdfm`, PID 79642 |
| Disposal fixture | `/private/tmp/rwnsh-p2-gy5qtpq2`, PID 80886 |
| Cargo / NSH fixtures | `/private/tmp/riwork-p2b-checks` and private `target/nsh-integration-home-*` |
| Native build profile | standard Cargo `release`, optimized |
| Fixture binary SHA-256 | `49ca2142e4495d9e9b541646e0656a1035f8bffa729ed7ef8c715352a525ccbc` |
| Production bundle SHA-256 | `c16be73148018782e9f2754810534787dcfa85418abc86cad8653646ec32d5bc` |

Each native manifest records the exact binary/bundle paths, argv, HOME,
RIWORK_HOME, runtime/config/temp directories, tmux socket and command results.
The immutable release binary was copied to `/tmp/riwork-p2b-checks/bin`
immediately after the final feature build so ordinary Cargo test builds could
not overwrite it.
The pinned Ghostty C artifacts were hash-verified and imported only into this
worktree's ignored private runtime. No system installation was performed.

## Verification

| Check | Result | Retained evidence |
| --- | --- | --- |
| Broad Cargo suite | 1,965 passed, 0 failed, 189 ignored | `evidence/p2b/cargo-broad.log.gz` |
| Final affected Cargo packages | 295 passed, 0 failed | `evidence/p2b/cargo-integration.log.gz` |
| Final terminal fixture unit tests | 24 passed, 0 failed | `evidence/p2b/cargo-terminal.log.gz` |
| Final production Cargo check | passed | `evidence/p2b/production-check.log` |
| Final NSH check and tests | passed; 93/93 tests | `evidence/p2b/nsh-manifest.json`, `nsh.xml` |
| Ghostty preparation tests | 9 passed; pinned artifact verification passed | `evidence/p2b/pin-tests.log` |
| Production and fixture dependency trees | 449 packages each; zero GPUI or Ghostty Rust crates | `evidence/p2b/dependency-proof.json` |
| Native extras | passed | `evidence/p2b/extras/manifest.json` |
| Native disposal regression | 20 cycles; passed | `evidence/p2b/disposal/manifest.json`, `cycles.json` |

Test counts overlap and should not be added. The early broad runner's Cargo
command passed, but its later NSH command used incomplete seed data and failed.
The corrected final NSH fixture and 93/93 result supersede that NSH attempt;
the original broad manifest is preserved without altering its failed status.

The extras fixture proves raw bracketed bytes and `tmux capture-pane` hex,
spaces/Unicode/quotes/metacharacters as literal file arguments, native file and
caption/image paste priorities, PNG bytes/permissions, atomic unsafe-batch
rejection, six native Find matches and forward/backward navigation, nested
lineage and separate scrollback, natural exit isolation, and a Unicode Vim
edit/save/reopen-at-line round trip. It also proves hidden stale focus is ignored,
a late accepted drop cannot reselect a hidden tab, images survive
release/reattach and disappear on partial tab close, and sibling
attach PIDs remain unchanged. Policy boundaries are accelerated through the
feature-only clock probe; production intervals remain unchanged.

Extras shutdown covered 12 native views and 13 surface lifetimes, all with
confirmed detach before free, exit code 0 and zero leaked clients. Maximum
measured disposal was 60.473 ms, surface free 3.144 ms and shutdown call
0.114 ms. Both the fixture PID and its private tmux server were absent after
cleanup. Raw input, native lifetime events, compressed final snapshot and
PID-scoped Cua screenshots are retained alongside the manifest.

The disposal fixture keeps inert Files/Preview panel tabs beside its four
terminals, so closing a terminal does not collapse the pane being exercised.
Twenty close/reopen cycles retained the original shell/pane identities and four
clients, then used the normal host quit path under output pressure. All 84
surfaces and callback contexts were freed, pending count was zero, all exact-tty
detach confirmations preceded child exit/free, and no retry/park was accepted.
Maximum measured shutdown was 0.091 ms, native free 8.926 ms and disposal
49.843 ms. Across 2,844 independent main-loop heartbeats, the worst teardown gap
was 31.963 ms against the unchanged 100 ms limit; no global gap exceeded 100 ms.
The fixture PID, private tmux server and pressure children were absent afterward.

Native timing acceptance uses the standard Cargo release profile. The earlier
debug run completed all 20 cycles with correct 84-surface teardown and no leaks,
but failed its heartbeat limit at 496.026 ms. A sample attributed about 56% of
main-thread time to Objective-C signature verification enabled by debug
assertions, amid repeated NSH hit testing. A framework-only optimization
experiment still failed. No framework vendoring, profile override or threshold
change was retained. Standard release passed a two-cycle diagnostic, followed
by the full combined acceptance above. Failed manifests and sampling evidence
remain in `evidence/p2b/diagnostics`; debug native timing is not certified.

## Reproduction

See [README.md](README.md#reproduce-isolated-verification) for private Ghostty
preparation and fixture build instructions. The final candidate passed the
combined command below; the second command reproduces its baseline disposal
phase independently:

```sh
python3 crates/riwork-desktop/verification/phase2b_terminal_extras.py \
  --binary /tmp/riwork-p2b-checks/bin/riwork-terminal-release-fixture \
  --bundle /tmp/riwork-p2b-checks/bundle/bundle.js \
  --cycles 20 --linger-seconds 30

python3 crates/riwork-desktop/verification/phase2_terminals.py \
  --binary /tmp/riwork-p2b-checks/bin/riwork-terminal-release-fixture \
  --bundle /tmp/riwork-p2b-checks/bundle/bundle.js \
  --cycles 20 --linger-seconds 30
```

The combined extras entrypoint passed both phases on the final binary and
bundle. It requires at least 20 cycles. Both runner entrypoints retain strict
native ordering, context counts and latency assertions. Physical Finder drag
delivery and external agent image ingestion are outside this private-pasteboard
fixture's coverage.

No merge, push, installation or task-status change was performed. The
orchestrator owns task status and integration.
