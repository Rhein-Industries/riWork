# RiWork patch to nsh-js 0.3.0

This is nsh-js 0.3.0 from crates.io with one change, in `src/reactive.rs` (`gc_mark`), used by the
desktop app and by the runners `nsh test` builds (both `Cargo.toml` and `.cargo/config.toml` patch
`nsh-js` to this folder).

**The bug.** A computed's (memo's) JS object reported its value to QuickJS's cycle collector in
`gc_mark`, but its finalizer (`finalize` → nsh-core `Graph::drop_handle`) gives that value up only
for a signal or a disposed memo. When a running memo's object ended up in a garbage cycle, the
collector freed the value while the reactive node still held it; on the memo's next run nsh-core
dup'ed, compared and freed it again. The freed block was then reused by the next native delivery
(for example an element of `Sessions.subscribe`'s list), which was freed while still in its array.
RiWork crashed this way on 2026-11-01 (SIGSEGV at 0x18 in `JS_CallInternal` under
`Array.prototype.find`, reading the status bar's sessions). Under ASan: heap-use-after-free in
`JS_DupValue` from `run_computation`, freed by `JS_RunGC`.

**The change.** A memo object reports its value and custom equals only when its finalizer will
release them (a signal, or a disposed memo). A running memo's value is owned by its node and stays
alive until the memo is disposed; a memo whose object goes while its node lives keeps its value
until the graph is torn down. That retention is the cost of the fix.

**Upstream.** Report to NSH: the fuller fix is for `finalize`/`drop_handle` to take and free a
running memo's value when its object goes. The iOS app uses the prebuilt NativeShell.xcframework,
which embeds nsh-js and cannot take this patch; it needs an NSH release with the fix.

The unit tests of this crate refer to sibling paths of the NSH monorepo and do not build here, so
the crate is excluded from the workspace.
