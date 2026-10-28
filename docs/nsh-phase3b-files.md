# Phase 3b Files explorer, previews and editing

Assignment: `085153fd-aaf7-4b01-b0e2-407b847ea251`.

Branch: `nsh-p3b-files`, based on `nsh` / `7718e60`.

`verification_environment = isolated temporary fixture repositories and RIWORK_HOME/RIWORK_RUNTIME_DIR, native Rust tests and windowless NSH 0.3.0 QuickJS engines on macOS 26.2 (25C56), arm64, Rust/Cargo 1.99.1`

No live windows or RiWork state, installed application, desktop automation,
merge, push, installation or task-status mutation was used. Task status remains
the orchestrator's responsibility.

## Implemented

- Shared Files tree, natural directory ordering, loaded-path filtering, hidden
  entries, bounded directory listings, selection, keyboard navigation and Git
  decorations. A process-shared native filesystem watcher updates subscribed
  directories and Git index changes; hidden surfaces release subscriptions.
- Selected-worktree rooting with no fallback for an unavailable explicit
  selection. Root changes clear the tree, filter, selection and preview;
  relabelling preserves them. Active local shell changes follow their worktree.
- Text/code and rendered/source Markdown previews, bounded oriented image
  bitmaps, explicitly requested PDF page rendering/paging, binary and malformed
  media messages, exact text copying, line reveal and zoom. Markdown document
  images cannot start arbitrary reads.
- Selected-row Edit uses P2b `openEditor` and the existing Vim tab routing.
  New launches enforce the listing identity; a live editor remains reusable
  after its own saves. Open validates the selected target before calling the
  system host API. Preview placement uses core plans and retains clicked
  terminals and keyboard focus.
- Project memo editing with normalized UTF-8 byte limits, autosave, explicit
  save/retry and external updates. Native synchronous staging and a host flush
  barrier retain accepted edits after JavaScript teardown. Revision checks
  prevent a delayed older save from overwriting the close-time save.

`docs/nsh-contracts.md` records the introduced schema-3 Files payloads and API.
This slice adds the explicitly assigned watcher, Git decoration and rendered
Markdown behavior beyond the original GPUI parity rows; it does not assert
completion of unrelated host/share, chat-attachment or app-preview surfaces.

## Verification

Scratch evidence: `/tmp/riwork-nsh-p3b-085153fd.YKs6pJ`.
The existing cached Cargo target and pinned NSH CLI were reused; no installer ran.

| Check | Result | Evidence |
|---|---|---|
| `cargo test --locked --offline -p riwork-nsh-native -p riwork-nsh-workspace` | 333 primary tests passed, 1 existing ignored test; no failures | `cargo-verified.log` |
| `cargo test --locked --offline -p riwork-engine file_preview::tests --lib -- --test-threads=1` | 14 passed | `engine-preview-final.log` |
| NSH check with pinned 0.3.0 CLI and TypeScript compiler | Passed | `acceptance-final/nsh-check.log` |
| Complete NSH headless suite | 111 passed, 0 failed | `acceptance-final/nsh-test.log`, `acceptance-final/nsh.xml` |
| `cargo fmt --all -- --check`, `git diff --check`, polling guard | Passed | `fmt-verified.log`, shell output |

Containment tests include sibling-prefix and parent traversal, NUL paths,
symlink files/directories/roots/cycles, replaced ancestors, root replacement
races, stale identities, FIFO/socket refusal and invalid identity tokens.
Fixture repos contain nested, spaced and Unicode paths, hidden entries, binary
and large files, oversized directory listings, images and multipage PDFs.
Byte/type assertions verify exact CRLF text copying, clipped preview text,
magic-checked PNG transport, dimensions, EXIF orientation and distinct PDF page
pixels. Native bridge tests mutate a real temporary filesystem and verify
matching deliveries to two engines, quiet idle frames, cleanup and setup errors.
A third real-JavaScript bridge proof forwards a listed file's path, opaque
identity and line to an injected editor factory, then reuses one returned editor
tab in the existing main pane without launching Vim or tmux.

The complete headless run uses a fresh fixture home. The legacy workspace and
native session fixture readers use separate files, with the native file projected
to the current `ShellSessionDto`. The runner enables `test-support` only for
NSH's generated headless executable through its public `CARGO` override. Both
session inventories are fake; no tmux server or GUI is launched.
Existing UI checks now scope status labels to the status bar and restore the
pane-routing test's starting geometry after earlier shared-fixture splits.

Reproduce with an existing pinned NSH CLI and compiler:

```sh
python3 crates/riwork-desktop/verification/phase3b_files.py \
  --root /tmp/riwork-files-fresh-verification \
  --target-dir /absolute/path/to/disposable-cargo-target \
  --nsh /absolute/path/to/nsh-0.3.0/bin/nsh \
  --tsc /absolute/path/to/typescript/lib/tsc
```

The runner refuses an existing evidence root or a root outside `/tmp`, records
exact commands/logs/JUnit, and asserts that the isolated tmux directory remains
empty. Its fixtures are debug/test-support gated. No visual native-window
acceptance or actual Vim process launch is claimed by these headless checks;
the P2b editor creation path remains the existing implementation.
