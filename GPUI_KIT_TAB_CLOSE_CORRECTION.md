# Workspace tab-close correction — source only

2026-10-06 · Project `39832c2e-23a5-476d-aa8f-5ff34a02d314` · Task `d87296de-efe1-483a-98a0-d52a5da00e34`.

Foundation worktree/branch verified at `fe11f529f7ee1234d51b5ae705eef6a55c1e2557`. Parent's immutable assembled `9c16634` and private reviewer run were not modified or executed here.

## Exact production seam

Only Workspace's existing nested close-tab hunk changes in `main.rs`. The button keeps its Base action, accessible name, dimensions, Native round shape, existing hover/background/color treatment, tooltip, click propagation stop and exact `close_tab_by_user(pane_id, tab_id, window, cx)` callback. The tab row's selection/drop/drag handlers and terminal paste wiring are untouched.

The old Left mouse-down stop on the Base button ran before GPUI's default focus listener because bubble listeners run in reverse registration order (`gpui-pre-0.3.8/src/window.rs:5820`, `elements/div.rs:2774–2800`). `behavior_controls::tab_close_boundary` moves that stop to a non-focusable, unpadded `flex_none` ancestor. Base focuses its real child first; the boundary then prevents a close-target press from arming the parent tab's drag/selection engine. Simply removing the stop would change close-target drag behavior. The helper adds no focus handle, focus operation or activation engine.

Pinned GPUI's `Visibility::Hidden` exits before tab-stop registration (`elements/div.rs:2535`), so adding a focus style to `.invisible()` would leave the inactive Native close unreachable. `tab_close_reveal` uses opacity zero instead, preserving the control's layout/traversal, and reveals it through the existing tab group hover or its real focus style. Existing action focus-visible border styling remains; focus reveal also survives a later pointer move without depending on input modality. Active Native and other themes retain their prior visibility.

## Existing inert fixture strengthened

Affected exact name: `behavior_controls::tests::content_controls_keep_caller_hover_nested_isolation_and_radio_exclusivity`.

The fixture uses actual Base action presentation at Native close dimensions and the same two production helpers. It asserts quiet inactive presentation away from hover, Tab reachability/visibility, ShiftTab hiding, unchanged group-hover reveal, and actual pointer focus after the event boundary. Setup observes one intentional row activation before resetting the callback log; the subsequent strict `[close, close]` assertions and all original nested-disabled/radio/toggle/switch counts remain. Synthetic close-target dragging must produce zero row drags and no activation; ordinary row dragging must produce exactly one drag. The drag view is an inert entity, with no Workspace/store/session/provider or native terminal construction. Action and assertion turns drain effects separately.

## Verification and limits

`git diff --check` passed. The approved source formatter parsed the three files without rewriting unrelated formatting:

```sh
/Users/dominik/.rustup/toolchains/1.95.0-aarch64-apple-darwin/bin/rustfmt --edition 2024 --config skip_children=true --emit stdout src/behavior_controls.rs src/behavior_controls/tests.rs src/main.rs > /dev/null
```

No Cargo/build/test/list/runtime/GUI/provider/process-control/service/tmux command ran. The strengthened fixture is **uncompiled and unexecuted here**; no new passing receipt is claimed. Reviewer must verify the exact case after parent integration. Native geometry, keyboard traversal, AX and drag acceptance remain pending parent Cua control. Prior untracked audits are preserved; no other worker module, immutable assembly, branch, installed app or production state was changed.
