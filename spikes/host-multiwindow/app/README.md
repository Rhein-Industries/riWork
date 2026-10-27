# Isolated mixed Rust + TSX fixture

This throwaway app runs the same bundle in two independently configured AppViews.
It imports `nsh/base` (`Button`, `TextView`) and the app's Rust UI set
`riwork/native` (`Probe`, `ReplayCounter`). All Rust NSH dependencies are pinned
to 0.3.0. `host` (imported from `nsh`) and its headless test support require the 0.3 CLI; the
machine's pre-existing 0.2 CLI is insufficient.

The custom host calls `set_ui_sets(vec![nsh_base::registry(),
riwork_nsh_probe::registry()])`. `host.params.windowId` distinguishes windows.
Other launch params are `historyRows` (default 200), `chunkBytes` (40), and
`intervalMs` (default 0 inside this app; host chooses pacing).

Host events `replay` and `stream/replay` accept `{ chunkBytes?, intervalMs? }`;
`probe` asks for the current state. The bundle posts `ready`, `progress` every
100 chunks, `done`, and `probe`. Payloads include window ID, native identity,
revision, byte/chunk count, and a native FNV-1a checksum. The manifest grants
only the four emitted host message names. `streamedMarkdownReads`,
`streamedLayoutCount`, and `streamedHeight` prove the streamed body actually
reaches TextView and layout. Edit `Revision.ts` for
the hot-reload fixture.

`fixture.ts` reproduces the repository's
`chat_view/display_tests.rs::long_answer_streaming_costs` generator: 60 Markdown
sections, 18,980 ASCII bytes, 475 chunks of at most 40 bytes, FNV-1a `4f4771e9`.
It is generated test data and never reads real conversations. A virtualized
`List` contains the history and the growing assistant message rendered by
`TextView` with `streaming: true` and `selectable: true`.

`done.updateMs`, `updateFirst100Ms`, and `updateLast100Ms` measure synchronous JS
signal/native-call duration. `tickGapMs` measures timer delivery intervals;
`elapsedMs` includes the replay and a final timer yield. These are not presented
frame times. The host records engine commits separately; GPU/UI presentation
requires independent evidence and must not be inferred from headless tests.

From the repository root, use the locally built 0.3 CLI (no install):

```sh
spikes/host-multiwindow/.tooling/target/release/nsh check spikes/host-multiwindow/app
spikes/host-multiwindow/.tooling/target/release/nsh test spikes/host-multiwindow/app
spikes/host-multiwindow/.tooling/target/release/nsh build spikes/host-multiwindow/app --out spikes/host-multiwindow/app/.nsh/out
```

The headless test opens two separate engines with A/B launch parameters,
replays all chunks through A's host event and native counter, validates the
complete checksum and visible final Markdown section/table after automatic bottom anchoring,
and proves B's counter remained zero. No `--target` is used
for tests, so no real application windows are touched.
