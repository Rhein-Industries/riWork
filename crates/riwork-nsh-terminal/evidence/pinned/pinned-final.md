# Phase 2a pinned build and dependency proof

Recorded: 2026-10-27T21:53:58.703611+00:00

`verification_environment`: Read-only metadata/build-input verification; no application launch, GUI inspection/input, live RiWork state access, build, artifact replacement or system installation.

Workspace: `/Users/dominik/orca/projects/riWork-nsh-p2-terminals`. Production source remained frozen.

## Cargo dependency proof

```sh
cargo tree -p riwork-desktop --prefix none
```
Result: exit 0; **PASS**, zero package names containing `gpui` or `ghostty`; 412 unique package names across 1119 tree rows.
Raw tree: [cargo-tree-default.txt](/tmp/riwork-p2-terminal-verification/cargo-tree-default.txt); SHA-256 `5fc6cfd4970a5deabaf1f7be14fdb98c3d4c1795a952518a78267cd76c240f6e`.

```sh
cargo tree -p riwork-desktop --features terminal-fixture --prefix none
```
Result: exit 0; **PASS**, zero package names containing `gpui` or `ghostty`; 412 unique package names across 1119 tree rows.
Raw tree: [cargo-tree-terminal-fixture.txt](/tmp/riwork-p2-terminal-verification/cargo-tree-terminal-fixture.txt); SHA-256 `5fc6cfd4970a5deabaf1f7be14fdb98c3d4c1795a952518a78267cd76c240f6e`.

`riwork-nsh-terminal` is RiWork's adapter; its only Ghostty link is the native C static archive. `gpui-libghostty` is a source distribution carrier only and is absent from both Rust dependency graphs.

## Exact linked installation

`RIWORK_GHOSTTY_INSTALL_DIR` was unset. Build-script outputs select `/Users/dominik/orca/projects/riWork-nsh-p2-terminals/crates/riwork-nsh-terminal/.runtime/pinned/install/lib`, and `cargo:rustc-link-lib=static=ghostty-internal`. All captured terminal build-script link records select this same private installation, never either rebuild-check directory.

| Input | SHA-256 |
| --- | --- |
| `lib/libghostty-internal.a` | `b51367aa565527eddfa180d5ae6304899d903af6fc094b4531671ffc20fb064f` |
| `include/ghostty.h` | `f2c12e1bb02540d79feed6bd29ee37312c4e0d24391442a63e66f06e1f7f2e4a` |

**PASS: the selected archive is the exact accepted GO archive** `b51367aa565527eddfa180d5ae6304899d903af6fc094b4531671ffc20fb064f`. Its provenance action is `import_exact_go_artifacts`. The two fresh-build validation archives were not copied over it.

Exact verification command:
```sh
python3 crates/riwork-nsh-terminal/scripts/prepare_ghostty.py verify
```
Result: exit 0; source/revision/toolchain provenance, archive, header and all 602 theme files verified. Raw result: [pinned-install-verification.json](pinned-install-verification.json). Link directives: [terminal-link-directives.json](terminal-link-directives.json).

## Pins and private rebuild validation

- Ghostty revision: `9f0e1719dc918368367d368bfe300f59bb68b5a4` plus the declared vendor patches in `VENDOR.md`.
- Source distribution SHA-256: `2b12d0d271cf0ca99a9073bdc148ce43782713c6d01be33af2a105b00c62ef5d`.
- Complete 1,242-file source manifest: `78f97c2dae6f6717963a8de12a4164c6d20c57bd7f984536878d2b98bc9816de`.
- Zig version: `0.16.0`; binary SHA-256 `e6cd688d25664983833aae272f501d4bceeae304875b8f1741209d15fd13a4ec`.
- Theme distribution SHA-256: `ea9878471420ee5b12e7f2ff480099c954ea50e573a1bdf83f43e105c9be63f0`; 602-file manifest `72de45b75c66d97dac20bc58a843b6a2fbb4fb39f99261b977e28aeea30c29fe`.

Final standalone source-build check command:
```sh
python3 crates/riwork-nsh-terminal/scripts/prepare_ghostty.py build \
  --cache-dir crates/riwork-nsh-terminal/.runtime/final-build-check \
  --source-archive crates/riwork-nsh-terminal/.runtime/rebuild-check/ghostty-source.crate \
  --zig /Users/dominik/.local/share/riwork/toolchains/zig-0.16.0/zig
```
Result: exit 0. Rebuilt validation archive: `7a2573b7b934684366b753cb46164c44ae13a55db5ac36aae1a1a590ea62f6c9`; header matches the frozen pin. This differs from the accepted GO archive and is **build validation only**, not the linked/runtime acceptance artifact. Private build/source/cache/SDK paths and exact Zig arguments are preserved in [pinned-final.json](pinned-final.json). All writes were confined to private worktree caches; no system-wide install.

## Environment

```text
ProductName:		macOS
ProductVersion:		26.2
BuildVersion:		25C56
Darwin dgsPro 25.2.0 Darwin Kernel Version 25.2.0: Tue Nov 18 21:09:56 PST 2025; root:xnu-12377.61.12~1/RELEASE_ARM64_T6041 arm64
rustc 1.99.1 (58cf8f9c3 2026-10-13)
cargo 1.99.1 (5f94df478 2026-08-27)
Xcode 26.0.1
Build version 17A400
Zig 0.16.0
```

Current debug executable snapshot: `/Users/dominik/orca/projects/riWork-nsh-p2-terminals/target/debug/riwork`, 208603920 bytes, SHA-256 `f502c4d5d29a39494cfbc08fe398b543c34a9afa9fba417996d6c6abbeb6d45b`. This is a read-only file snapshot; runtime PID and fixture acceptance are recorded separately.
