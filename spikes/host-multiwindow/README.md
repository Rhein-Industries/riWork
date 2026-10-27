# AppKit two-window NSH spike

Throwaway implementation for assignment `2a537a8b-d318-47a1-8478-a871dd429025`.
See the repository-root `WORKER_REPORT.md` for the decision, measured results,
verification environment, and limitations. Nothing here connects to RiWork's
user state or provider services.

- `host/`: custom Rust AppKit executable; two NSWindows, two NSH 0.3 AppViews.
- `app/`: shared TSX bundle and the `riwork/native` Rust UI set.
- `run_benchmarks.py`: three repetitions, two concurrent streams, zero or 2,400
  history rows, unpaced or 33 ms producer timers; fresh temporary data directory
  per process. Validates native identity, chunk checksum, and TextView activity.
- `verify_hot_reload.py`: starts the local CLI in server-only AppKit mode,
  verifies both windows before/after editing `Revision.ts`, and restores it.
- `evidence/`: raw measurements, command output, toolchain provenance and UI proof.
- `baseline-notes.md`: exact existing GPUI reference measurements and boundaries.

Build tools remain local under `.tooling/`; no CLI or application installation
is required. Commands are recorded in `WORKER_REPORT.md`. Tests use `nsh test`
with no target flag (headless). Native runs open only this fixture executable's
windows. Build output, caches, and disposable runtime data are ignored.

The public NSH 0.3 AppView API does not expose the built-in state-preserving dev
session. This host uses public `nsh_dev::DevClient` and reloads full source
bundles into both AppViews, retaining their host parameters and resetting their
app state. It does not patch the NSH crates or depend on private APIs.

Throughput is completed chunks per replay wall time. NSH's instrumented surface
frame cost is a different quantity from the repository's GPUI explicit drawn
frame timer. A JS timer yield, reported FPS, or final checksum alone is not proof
that every intermediate chunk was presented on screen.
