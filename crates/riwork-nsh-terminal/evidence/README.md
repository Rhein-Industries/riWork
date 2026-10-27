# Phase 2a verification evidence

This package retains build, dependency, integrity, Cargo, NSH, exact-tty and final native two-window acceptance evidence. Runtime checks used isolated fixtures only. No binary cache, application executable, user configuration or live RiWork state is included. The [worker report](../WORKER_REPORT.md) explains scope, commands, results and limits.

- `pinned/`: both Cargo trees contain zero gpui/ghostty Rust crates; link records select the accepted GO static C archive. Fresh-build provenance is reproducibility evidence only and was not the linked runtime. The `pinned-final` executable hash is the snapshot taken at its stated time, before later native diagnostic rebuilds.
- `headless/`: 1,728 Cargo tests passed, 0 failed, 188 ignored. Complete retained test output and exact command manifest are included. The earlier NSH suite is superseded by the final app suite below; counts are not added together.
- `app/`: final validation, mount regression before/after, all 16 NSH tests passing, typecheck and production bundle results. The full validation document describes earlier failures and final corrections.
- `tests/`: terminal Rust unit checks, fake exact-tty raw output and the earlier real-PTY worker record. Ignored tests are identified explicitly.
- `native/`: final visible 20-cycle acceptance, 84 complete native lifetimes, input/resize/theme/link readbacks, session survival on window close/quit, exact cleanup, two read-only Cua screenshots and metadata, plus final production-feature Cargo and real-PTY test logs.
- `diagnostics/`: corrected explanation of expected rendering suppression in fully occluded background fixture windows; early lifecycle/protocol passes were not complete visual acceptance. Final visible acceptance passed with no production visibility bypass or extra poller.

The headless manifest retains exact commands, fixture isolation, results and toolchain paths; unrelated inherited PATH directories are replaced with a SHA256. The original manifest digest and the transformation are recorded. Copied raw files are byte-identical to their sources except the final snapshot, which uses deterministic gzip (mtime 0). Generated summaries and corrections are labeled in the manifest. Worker-recorded results without a standalone raw transcript are labeled accordingly.

`manifest.json` inventories every payload file with its SHA256 and source provenance. `manifestSha256` is the SHA256 of the manifest bytes (the manifest and this sidecar are excluded from the payload inventory to avoid a circular digest). Evidence assembly itself only copies or summarizes the recorded results.
