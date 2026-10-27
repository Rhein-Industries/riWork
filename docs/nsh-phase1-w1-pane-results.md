# W1 pane model and native gesture results

Task `71ac437e-38de-4861-807a-bf81e4e44e47`, 2026-10-27. Source: acceptance §§4.3–4.4, 5.5, 5.7 and P1-LAY-02/08. This is the Rust/model/native-component portion of the W1 report; the TSX, session-service and persistence reports supply the remaining end-to-end evidence. No GPUI source or test was deleted.

## Commands and results

Commands ran from `/Users/dominik/orca/projects/riWork-nsh-p1-fix-workspace`. The isolated target directory avoids the other W1 workers' build locks. Pure Headless fixtures use manual time and fake panel content, with no shell launches or user-home reads. The one core save/read test uses its own temporary directory.

| Command | Result |
|---|---|
| `CARGO_TARGET_DIR=/tmp/riwork-w1-pane-target cargo test -p riwork-nsh-workspace --lib --test pane_model --test layout_gestures --locked --offline -- --test-threads=1` | PASS: 29 library, 16 pane-model, 12 real Headless gesture tests; 0 failed. |
| `CARGO_TARGET_DIR=/tmp/riwork-w1-pane-target cargo test -p riwork-nsh-workspace --test layout_gestures --locked --offline -- --test-threads=1` | Final native reveal regression rerun: PASS 13; 0 failed. |
| `CARGO_TARGET_DIR=/tmp/riwork-w1-pane-target cargo test -p riwork-core --lib layouts::workspace::tests --locked --offline` | PASS: 14 successor tests; 0 failed. |
| `CARGO_TARGET_DIR=/tmp/riwork-w1-pane-target cargo test -p nsh-base --test base dock:: --locked --offline` | PASS: all 22 preserved upstream dock tests. |
| `CARGO_TARGET_DIR=/tmp/riwork-w1-pane-target cargo test -p nsh-base --test base resizable:: --locked --offline` | PASS: all 11 preserved upstream resizable tests. |
| `cargo tree --manifest-path apps/riwork/.nsh/rust/describe/Cargo.toml --offline -i nsh-base --target aarch64-apple-darwin` | PASS: exactly one `nsh-base v0.3.0`, resolved to `apps/riwork/vendor/nsh-base`, shared by workspace and native bridge. |
| `shasum -a 256 /Users/dominik/.cargo/registry/cache/index.crates.io-1949cf8c6b5b557f/nsh-base-0.3.0.crate` | `78d139f86c6d867029ab4cf605bb9e83d6fb08e159b0a747795905d8052c7705`. |

After adding the preserved vendor package as a workspace member, the same workspace test command was run once with `--offline` without `--locked` to record development dependency edges in the lockfile. No versions changed or dependencies were installed. The final command above passed with `--locked`.

## Evidence by behavior

- P1-PANE-02/05/06/07/08/09/10: `pane_model.rs` verifies transactional split input, second-half placement/selection, focus exit and unchanged saved geometry, existing-panel selection in place, exact lock refusals/internal bypass, main inheritance, last-pane survival, pending chat identity, gather/default notices, and selection-preserving adoption. The failed-create model case verifies rejected commit input; session-service and TSX tests separately exercise actual failed asynchronous creation.
- The 14 named `layouts::workspace` successors preserve the acceptance ledger's three routing, three close, two gather, five default-layout and one lock tests. The protected GPUI versions remain in place.
- P1-LAY-02: all eight specifically named `layout_gestures.rs` tests use actual native `DockArea.onLayoutChange` output from `nsh_testing::Headless`. The controlled component applies the production `LayoutModel` and installs only its returned projection. Counters assert one projection per applied gesture. Ordinary gestures produce no fallback diagnostic. The two-change case defers/coalesces real native selection and resize output and requires `layout.fallback`. The sentinel case drops a real panel into the empty target.
- P1-PANE-03/04: the native measured divider indicator is exactly **5 points** on both axes at 1280×800, 1920×1080 and 640×400. Measured panel body size plus its known 24-point test header matches `riwork_core::Layout::pane_extents` within **0.5 px**. Actual drag output changes the core width by 100 points and clamps ratios to 0.1–0.9. Existing locked-pane resize/projection tests pass. The production header remains governed by the TSX 28-point metric tests.
- P1-DRAG-01/02: native reorder, group move and all four edge drags preserve panel keys and report no removal. The selected source neighbour is retained, target selection is updated, and normal edits use the core operations. A separate actual-drag boundary test observes splitting at 34%/66% of target content and merging at 36%/64%, confirming the pinned NSH **35% edge zones** (source: `dock/drag.rs`). Horizontal zones win corners. Splits occupy approximately one half. The adapter defines no root/top window dock target and uses the accepted P1 upstream geometry GAP, not RiWork's deferred 23%/12-point zones.
- P1-PANE-12: native suffix test overflows nine tabs, presses the fixed 28-point pane-menu control, verifies its right edge remains at the pane's right edge, then performs a successful native panel drag. The suffix changes neither panel identity nor native drag ownership.
- P1-TAB-01/05, P1-LAY-06/08: pending chat resolution keeps the UI panel key, omits unsaved chats from disk, and returns the resolved SavedTab. Adoption preserves current selection and caps each delivery at 16; it uses unlocked main, active, then first unlocked. Hidden IDs normalize both raw and `shell:` forms. Unsafe pane IDs and opaque-tab mappings persist the safe aliases. Same-project refreshes retain opaque remaps and focus; Default Layout composes original and already-saved aliases when moving unknown tabs to main.

Rust native component lifecycle is not substituted for the JS keyed-render identity contract. DRAG-04's mount-once proof belongs to `Identity.test.tsx`; this suite asserts real panel IDs and absence of remove events. Both the production component and tests use `compact={false}`; pane locks never set `DockArea.locked`.

## Pinned native correction

Published NSH 0.3.0 reserves no divider gap and draws a one-point overlay, so merely styling its handle cannot reproduce core's five-point geometry. The additive patch reserves a configurable gap with upstream-compatible default zero and uses the existing gap-aware resizing arithmetic. A second optional prop provides the fixed pane-menu suffix beside a scrollable tab row. Provenance and the exact 220-line source diff are in `apps/riwork/vendor/nsh-base/RIWORK.md` and `RIWORK.patch`; all published source/tests are preserved.

D-COPY-1 remains open. This work retains the existing close-dialog wording; it does not resolve or approve copy changes.


## Final review corrections

The review found that centered-focus presentation measurements could become the locked-pane resize baseline. The model now retains its normal extent until the first normal measurement after leaving focus; projection/subscription updates cannot replace it. `centered_focus_enter_exit_preserves_the_normal_resize_baseline` covers 1280×800 → centered 1100×560 → exit → stale presentation projection → 1280×800, and a window initially restored in centered focus. Both retain the exact saved layout. The full locked/offline workspace command above then passed 29 library, 16 model and 12 gesture tests.

The classic header now reveals selected tabs on selection/order/row-size/viewport changes, using native measurements and minimum scroll distances. `selecting_a_clipped_native_tab_reveals_it_and_can_scroll_back` selects via the actual native API, observes ScrollTo428 for ten 80-point cells in a 372-point viewport, then ScrollTo0 on returning to the first tab; no panel removal occurs. Rust Headless selectors include clipped content, so this test asserts the actual native scroll commands and their measured reverse effect rather than treating selector presence as visible bounds. It uses the unchanged native controlled-layout return for stable header refs; the eight gesture-mapping tests separately cover production apply/reprojection counters.
