# W3 / panels merge resolution

Task `c62ea465-bf1e-4ae9-ad41-0f7b86b2965f`, branch `nsh-p1-fix-native`.
Merged the requested `nsh` parent `09688dd54868e27456a8f81f922d14f15b32aef6`
into accepted W3 `62ec262f8db292038088f51f14d385df89fbc577`.
The `nsh` reference advanced during verification; this resolution retains the
requested merge parent. Task status remains with the orchestrator.

`verification_environment = macOS arm64; disposable HOME, RIWORK_HOME, runtime,
config, project directories and tmux fixtures; windowless NSH engines; no live
RiWork state, desktop input, installation or push.`

## Conflicts and choices

Git reported **13 conflicted files**: `apps/riwork/{App.tsx,Workspace.tsx,model.ts,
src/host_layouts.rs,src/lib.rs,tests/Workspace.fixture.tsx,tests/nsh.json}` and
`crates/riwork-nsh-native/{README.md,src/backend.rs,src/lib.rs,src/service.rs,
tests/native_headless.rs,tests/services.rs}`. All are resolved.

- Keep real Projects, Worktrees, Tasks and Sessions panels, search, CRUD editors,
  toolbar and project switching, together with W3 Settings, status items,
  notices, atomic appearance readiness and the stable DockArea subtree.
  Real panels/editors now consume the resolved palette, fonts and text scale.
  Status identity follows the selected project. An explicit callable context
  child preserves reactive panel selection in NSH 0.3.
- Keep exactly **one production `riwork/native` registry**, exporting Store,
  Sessions, Tabs, Settings, Appearance, Dashboard and Layouts. Consolidate Tabs
  backends, DTOs and topic hubs; retain strict request decoding, canonical UUID
  validation, session create/close adapters and both branches' bridge proofs.
- Settings and Appearance use the existing home watcher. Keep the incoming
  independent worktree-presence deadline and empty-inventory disarming rules;
  presence-only refreshes do not sample Tabs or sessions. No UI polling or new
  appearance timer was introduced. The five-topic 60-second quiet guard passes.
- Keep project switching with atomic project/writer snapshots and W3 atomic
  edit/close admission. Each retained snapshot generation transfers its owned
  writer Arc to background disposal, including readers surviving a switch;
  final destruction cannot synchronously flush on the engine. A regression
  verifies both switched and closed writer disposal threads.
- Refuse a switch into a destination containing unknown saved tabs before
  locked-region carry can remove its opaque pane. The regression covers both
  prepared and direct switch paths, unchanged destination JSON, the flushed
  outgoing edit and continued use of the outgoing writer after refusal.
- Keep W3 default layout/tab preservation and incoming panel placement in the
  active pane. Core/engine persistence serde formats are unchanged. Follow
  Ghostty keeps the RiWork palette and diagnostic, with no configuration parser
  or reachable appearance publisher; fixtures create no `appearance.json`.
- `Cargo.lock` combines the existing workspace dependencies, including the
  incoming workspace UUID dependency. Locked/offline execution required no
  lock regeneration or dependency version changes.

## Final verification

The authoritative final Rust run is archived under `final-rust`; the remaining
final checks are under `merged-ui-host`. Commands, complete environments, exit
codes and output hashes are in the [machine-readable report](nsh-phase1-w3-panels-merge.json).
Raw output is under [verification/nsh-w3-panels-merge](verification/nsh-w3-panels-merge/).
Earlier compile/UI attempts and the intermediate Rust run remain archived as
superseded evidence; they are excluded from the final counts below.

| Check | Final result |
|---|---|
| `cargo test -p riwork-nsh-native -p riwork-nsh-workspace --locked --offline` | Native 38 + workspace 48 = **86 passed**, zero failures/ignored; external settings writer child also passed |
| `nsh check apps/riwork --tsc …` | Passed Rust and TypeScript checks |
| `nsh test apps/riwork --junit …` | **43 passed**, zero failures, including panel workflows and all W3 UI regressions |
| `cargo test -p riwork-desktop --bin riwork --locked --offline` | **29 passed**, including switched-project host metadata |
| Native probe `nsh check` / `nsh test` | Passed; **1 bridge test** imports all seven modules and updates two engines |
| Native quiet guard | **60 actual seconds**, five subscribed topics, no quiet deliveries, one initial empty session sample, zero presence polls |
| NSH idle guard | 60,000 virtual milliseconds with unchanged host calls/posts |
| Formatting / whitespace / structural audit | Passed; one production registry, no UI polling/native storage/appearance publisher, no core/engine serde changes; 142 protected GPUI files unchanged |

The existing NSH warning about native exports lacking doc comments remains
informational. Fixture audits found no session/chat/appearance files or tmux
files/sockets. The original 20-row W3 report remains historical acceptance
evidence; this report records the requested merge verification.

Reproduce all checks from the repository with the pinned tools already present:

```sh
python3 crates/riwork-desktop/verification/phase1_w3_merge.py \
  --root /tmp/riwork-w3-panels-merge-rerun \
  --target-dir /tmp/riwork-w3-panels-merge-rerun/target
```

The runner supports `--stage rust`, `--stage nsh` and `--stage review`, and
`--nsh` / `--tsc` overrides. Every run creates fresh mutable fixture state and
records the exact command argv, cwd, environment, exit code and output hashes.
