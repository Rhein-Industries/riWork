# Pinned NSH base patch

RiWork W1 uses the published `nsh-base` **0.3.0** source from the existing Cargo cache. No dependency was installed or upgraded. The complete published `src/` and `tests/` trees are retained. The normalized published `Cargo.toml` is retained with the upstream test runner dependencies restored.

- Upstream repository: https://github.com/longbridge/nsh
- Upstream commit from `.cargo_vcs_info.json`: `76fa8a7fed47ea51aea01fa85bb40960be15d984`
- Upstream directory: `crates/nsh-base`
- Published archive: `nsh-base-0.3.0.crate`
- Archive SHA-256: `78d139f86c6d867029ab4cf605bb9e83d6fb08e159b0a747795905d8052c7705`
- Declared license: Apache-2.0. `LICENSE-APACHE` supplies the standard license text, which the published archive omits.

`RIWORK.patch` is the exact unified diff against that published source. The changes are additive:

1. `DockArea.dividerSize` forwards an optional real gap to `ResizablePanelGroup`; zero is the upstream-compatible default. RiWork opts into five points. Existing gap-aware resizing/handle placement is reused without changing its arithmetic. The visual indicator spans the requested gap (one point by default).
2. `DockArea.renderTabBarSuffix(groupId)` adds a fixed control slot after a horizontal scrolling native tab strip. The default remains the original strip. Native tab references, drag handling, panel render caching and IDs are retained. Selection, order and header-size changes measure and minimally reveal the selected tab; a revision guard discards stale asynchronous measurements.
3. The published `nsh-engine` and `nsh-testing` 0.3.0 development dependencies allow running the complete preserved upstream tests from this workspace.

The root manifest and `.cargo/config.toml` both patch crates.io. The latter also applies to NSH-generated nested runner workspaces. The installed NSH CLI embeds older base TypeScript declarations; RiWork's documented typed adapter widens only these two new properties, while the runtime continues using the patched `nsh/base` registration.

Regression evidence is recorded in `docs/nsh-phase1-w1-pane-results.md`. Keep this patch pinned until an upstream release supplies equivalent geometry and suffix behavior. Do not edit the global Cargo cache.
