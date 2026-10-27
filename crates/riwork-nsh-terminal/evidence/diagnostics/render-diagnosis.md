# Integrated terminal rendering diagnosis

Status: final visible native acceptance passed. The earlier missed-visibility-transition diagnosis was incorrect: `NSWindowOcclusionStateVisible` is bit `2`, not `8192`. The background fixture was occluded, and Ghostty correctly paused rendering. No production lost-transition defect or fallback polling fix is established by that trial.

`verification_environment`: private RIWORK_HOME fixtures only, macOS 26.2 arm64, integrated `riwork` fixture build, pinned Ghostty C archive `b51367aa565527eddfa180d5ae6304899d903af6fc094b4531671ffc20fb064f`. This source-review worker performed no desktop interaction and changed no production source. Root performed read-only Cua.ai Driver verification with the exact fixture PID.

## Why early passes did not establish visual acceptance

`acceptance-stable.log` reports 20 successful close/reopen cycles in `/private/tmp/rwnsh-p2-y7lr4xjx`; root recorded 84 surface lifecycles. This established protocol, teardown, client/session survival, and related fixture assertions. It did not assert visible glyph pixels. Subsequent Cua screenshots showed blank terminal content while the fixture windows had been ordered behind other windows, so that protocol pass was retained as diagnostic evidence rather than treated as complete visual acceptance.

The actual production bug was the dock mount boundary. `acceptance-first.log` failed because a fixture terminal was no longer attached during an input probe. A changing layout object recreated the mounted dock/terminal subtree. The headless regression reproduced ten mounts instead of two across route selections and resize. `apps/riwork/Workspace.tsx` now uses a stable readiness signal for mounting and passes layout updates to the existing dock. The regression and full 16-test NSH suite passed afterward. `acceptance-diagnostic.log` also retains a failed resize trial; it was not counted as acceptance.

## Correct interpretation of the rendering diagnostics

- `/private/tmp/rwnsh-p2-i8pghkpu/diagnostic-force-draw.json`: Ghostty viewport text contained pressure output, the native view and IOSurface were 1220×335 points / 2440×670 pixels at scale 2, and sampling 182,336 pixels found zero pixels different from the background. Terminal parser state can continue advancing while rendering is paused for occlusion.
- Layer-hosting reset, forced draw/refresh, app/surface focus, inherited signal-mask clearing, and a vsync-disabled trial did not establish a production rendering defect or fix. Root sampled exact fixture PID 60444; `render-sample.txt` showed four renderer threads waiting in kqueue rather than blocked in font shaping.
- `/private/tmp/rwnsh-p2-786yvfe7/initial-native-snapshot.json` (fixture PID 7003) reported `lastShown=false`, `visibilityUpdates=3`, and `windowOcclusion=8192`. Because the visible bit is `2`, this window was occluded. The component's own visible property and its membership in a window do not imply that the window is unobscured. The native visibility state sent to Ghostty was correct.
- `RIWORK_NSH_BACKGROUND` had caused the diagnostic fixture to use `orderBack` beneath existing live windows. Cua reporting `on_screen=true` established WindowServer presence, not that the window was unobscured. No live window was targeted or manipulated.
- The same fixture's `host.log`, with Ghostty stderr logging enabled, confirmed the expected Menlo font faces, CoreText backend, Metal renderer, and pinned resources directory.
- Fixture requests `state/terminal-fixture/processed/100001.json` through `100004.json` explicitly forced Ghostty's visibility flag to true. Root then saw text in `/private/tmp/rwnsh-p2-786yvfe7/cua-show.png`. That diagnostic bypassed the correct occlusion pause; it did not prove a missed notification. Its result is not the final acceptance evidence.

## Source explanation and excluded hypotheses

Pinned `Surface.occlusionCallback` records visibility and queues it to the renderer. The renderer skips `updateFrame` while its visibility flag is false, so terminal text can arrive while the GPU cell buffer remains unchanged. `ghostty_surface_draw` draws cached cells without rebuilding terminal cells, explaining the background-only forced draw in the occluded fixture.

Zero `GHOSTTY_ACTION_RENDER` callbacks was not evidence of failure: this pinned Metal backend renders on its worker; the callback-driven main-thread path is for OpenGL. Config cloning deep-copies values and replay steps, and `ghostty_app_new` also clones configuration. There was no missing-finalize evidence. Link records and artifact verification selected the exact accepted GO archive; the separate fresh reproducibility builds were not linked.

A separate source review found that color-scheme changes emit soft `reload_config` actions. The native owner added handling that reapplies the owned config through the app/surface config-update APIs so conditional themes update. This was independent of the occluded fixture. Cached visibility remains an optimization; fallback polling for an alleged lost transition was not retained.

## Final visible acceptance

Root's final isolated native fixture `/private/tmp/rwnsh-p2-upf_p6zi/manifest.json` passed with its own two windows shown side by side without activating the app. Read-only Cua.ai Driver inspection targeted fixture PID `69604`, windows `6573` and `6574`, and confirmed visible terminal text. The final fixture did not rely on diagnostic `surface-show` overrides.

Root reported 84 surface lifetimes across the 20-cycle pressure run, maximum main-thread free time 5.840 ms, maximum disposal time 40.868 ms, maximum heartbeat gap 16.644 ms, zero global gaps greater than 100 ms, and all four sessions surviving application quit. The final native manifest and raw evidence, copied separately by root, are authoritative for acceptance and cleanup. This note corrects the earlier bitmask interpretation and distinguishes that fixture error from the proven dock remount defect.
