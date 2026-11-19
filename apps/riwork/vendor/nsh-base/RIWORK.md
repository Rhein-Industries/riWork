# Pinned NSH base patch

Somtus uses the published `nsh-base` **0.6.0** source with a local patch. The complete published `src/`, `tests/` and `fonts/` trees are retained. The normalized published `Cargo.toml` is retained with the upstream test runner dependencies restored.

- Upstream repository: https://github.com/longbridge/nsh
- Upstream commit from `.cargo_vcs_info.json`: `554163758e30d2f62f814096a6327937dcb4fe49` (tag `v0.6.0`)
- Upstream directory: `crates/nsh-base`
- Published archive: `nsh-base-0.6.0.crate`
- Archive SHA-256: `cf403f8cf3a973fef8f9234e2a769b6fad4cf01bcc14aff108e02af80399bdc7`
- Declared license: Apache-2.0. `LICENSE-APACHE` supplies the standard license text, which the published archive omits.

`RIWORK.patch` is the exact unified diff against that published source (`patch -p1` over the unpacked archive reproduces `Cargo.toml` and `src/`). Upstream 0.6.0 has none of these, so the patch is carried forward from the 0.5.1 one (first written for 0.3.0). The changes are additive; with none of the new props set, `DockArea`, `ResizablePanelGroup`, `Button` and `Switch` behave as upstream's:

1. `DockArea.dividerSize` forwards an optional real gap to `ResizablePanelGroup` (its own new `dividerSize`); zero is the upstream-compatible default. Somtus opts into five points. The group's existing gap-aware resizing is reused without changing its arithmetic. Each handle's indicator is a full-length one-point hairline that takes no pointer events, centered in a real gap (on the following panel's leading pixel without one). A terminal host keeps upstream's own one-Cell gap.
2. `DockArea.renderTabBarSuffix(groupId)` adds a fixed control slot after a horizontally scrolling tab strip, `renderTabBarPrefix(groupId)` one before it, and `fixedPanels` names the tabs kept before the scrolling segment. Without a suffix renderer the upstream tab bar is unchanged. Native tab references, drag handling, panel render caching and IDs are retained. Selection, order and size changes measure and minimally reveal the selected tab; a revision guard discards stale asynchronous measurements.
3. `DockArea.draggableTabBars` lists the groups whose unoccupied tab-bar area moves the native window (`windowDrag`); tabs opt out, so they keep their own presses and drags.
4. `DockArea.onDragChange({ dragging })` reports a tab drag's start and its end, a drop or a cancellation without a layout edit. As every NSH 0.4+ event its handler takes one object (`#[args(dragging)]` names the field; 0.3.0 passed the boolean itself). 0.6.0's area reports its events through `AreaCx::emit`, private to `dock_area.rs`; the patch makes it crate-visible so the tab drag in `tab_group.rs` can report this one.
5. The drop indicator covers the complete destination pane when splitting (its tab bar included, the divider gap reserved) and is a short insertion line between tabs when reordering, which snaps instead of animating.
6. Button and Switch expose their existing native root's Focus/Blur events (`onFocus`, `onBlur`). Settings uses these direct events to preserve focus anywhere in its form (including Cua actions) when shown. Activation, tab stops and native focus behavior are unchanged.
7. The published `nsh-engine` and `nsh-testing` 0.6.0 development dependencies allow running the complete preserved upstream tests from this workspace (`cargo test -p nsh-base`).

The root manifest and `.cargo/config.toml` both patch crates.io. The latter also applies to NSH-generated nested runner workspaces. The installed NSH CLI embeds the unpatched base TypeScript declarations; Somtus's Dock adapter (`apps/riwork/DockArea.ts`) widens the Dock properties and `SettingsFocus.tsx` adds the two opt-in focus events, while the runtime uses the patched `nsh/base` registration.

`apps/riwork/tests/base_patch.rs` checks what the TSX relies on against the patched registry on the headless engine: the declared TypeScript of the Dock props and events and of the focus events, a tab drag's `onDragChange` around its layout edit, and Button's and Switch's focus and blur under the keyboard. `apps/riwork/tests/layout_gestures.rs` drives the divider and the tab-bar slots. Regression evidence for the 0.3.0 patch is recorded in `docs/nsh-phase1-w1-pane-results.md`. Keep this patch pinned until an upstream release supplies equivalent geometry, tab-bar slots and focus events. Do not edit the global Cargo cache.
